"""Compare cold SSH selected-stream, nonanalog-first and early-frame reads.
Discover stream and channel ranges inside each timed run; reuse overlapping bytes.
Keep remote contents in memory and verify staged payloads against a full-file MD5.
This standalone experiment measures transport and reconstruction, not rendering.
Configuration is JSON-compatible YAML; the remote file is opened read-only.
"""

import argparse
import hashlib
import importlib.util
import inspect
import json
from pathlib import Path
import struct
import time


spec = importlib.util.spec_from_file_location("ssh_read", Path(__file__).with_name("02_ssh_read_experiment.py"))
base = importlib.util.module_from_spec(spec)
spec.loader.exec_module(base)
CHANNELS_HEADER = bytes.fromhex("f30a394c0200")
DIMENSIONS = {"FloatChannel": 1, "Float16Channel": 16, "Vector3fChannel": 3,
              "QuaternionChannel": 4,
              "class MoCapCore::cChannel<struct MoCapCore::sArrayChannelValues<float,16> >": 16}
MODES = ("selected_all", "nonanalog_then_analog", "early_frames_then_nonanalog_then_analog",
         "server_index_nonanalog_then_analog", "server_index_early_frames_then_nonanalog_then_analog")


class IntervalReader:
    def __init__(self, session, size, gap=0):
        self.session, self.size, self.gap = session, size, gap
        self.cache = []

    def many(self, ranges):
        missing = []
        for offset, length in ranges:
            if offset < 0 or length < 0 or offset + length > self.size:
                raise ValueError("Range outside file")
            end, cursor = offset + length, offset
            for start, block in self.cache:
                if cursor < start:
                    missing.append((cursor, min(start, end)))
                    cursor = min(start, end)
                if start <= cursor:
                    cursor = max(cursor, start + len(block))
                if cursor >= end:
                    break
            if cursor < end:
                missing.append((cursor, end))
        merged = []
        for start, end in sorted(missing):
            if start >= end:
                continue
            if (merged and start <= merged[-1][1] + self.gap
                    and not any(a < start and a + len(block) > merged[-1][1] for a, block in self.cache)):
                merged[-1][1] = max(end, merged[-1][1])
            else:
                merged.append([start, end])
        if merged:
            chunks = self.session.fetch([(a, b - a) for a, b in merged])
            self.cache = sorted(self.cache + [(a, block) for (a, _), block in zip(merged, chunks)])
        output = []
        for offset, length in ranges:
            end, cursor = offset + length, offset
            for start, block in self.cache:
                if start <= cursor < start + len(block):
                    stop = min(end, start + len(block))
                    output.append(block[cursor - start:stop - start])
                    cursor = stop
                if cursor == end:
                    break
            if cursor != end:
                raise ValueError("Incomplete cache coverage")
        return b"".join(output)

    def read(self, offset, length):
        return self.many([(offset, length)])


class Stream:
    def __init__(self, reader, pieces, length):
        self.reader, self.mapping, self.length = reader, pieces, length
        if sum(n for _, n in pieces) < length:
            raise ValueError("Stream chain shorter than declared size")

    def pieces(self, offset, length):
        if offset < 0 or length < 0 or offset + length > self.length:
            raise ValueError("Range outside stream")
        result, logical = [], 0
        for physical, count in self.mapping:
            a, b = max(offset, logical), min(offset + length, logical + count)
            if a < b:
                result.append((physical + a - logical, b - a))
            logical += count
        return result

    def many(self, ranges):
        return self.reader.many([piece for a, n in ranges for piece in self.pieces(a, n)])

    def read(self, offset, length):
        return self.many([(offset, length)])


class Compound:
    def __init__(self, reader):
        head = reader.read(0, 512)
        if head[:8] != bytes.fromhex("d0cf11e0a1b11ae1"):
            raise ValueError("Only uncompressed compound TAK is supported")
        u32 = lambda data, pos: struct.unpack_from("<I", data, pos)[0]
        shift, mini_shift = struct.unpack_from("<HH", head, 30)
        sector, mini_sector = 1 << shift, 1 << mini_shift
        count = u32(head, 44)
        if sector not in (512, 4096, 32768) or mini_shift > shift or count > 109 or u32(head, 72):
            raise ValueError("Unsupported sector size or extended DIFAT")
        fat_raw = reader.many([((u32(head, 76 + i * 4) + 1) * sector, sector) for i in range(count)])
        fat = struct.unpack("<" + "I" * (len(fat_raw) // 4), fat_raw)

        def chain(first, table):
            result, seen = [], set()
            while first != 0xFFFFFFFE:
                if first in seen or first >= len(table):
                    raise ValueError("Invalid sector chain")
                seen.add(first)
                result.append(first)
                first = table[first]
            return result

        def large(first, length=None):
            ids = chain(first, fat)
            if length is None:
                length = len(ids) * sector
            if len(ids) != (length + sector - 1) // sector:
                raise ValueError("Stream size and chain disagree")
            return Stream(reader, [((i + 1) * sector, sector) for i in ids], length)

        directory = large(u32(head, 48))
        raw = directory.read(0, directory.length)
        entries = []
        for pos in range(0, len(raw), 128):
            entry = raw[pos:pos + 128]
            name_len = struct.unpack_from("<H", entry, 64)[0]
            if entry[66] and (not 2 <= name_len <= 64 or name_len % 2):
                raise ValueError("Invalid directory name")
            entries.append((entry[:max(0, name_len - 2)].decode("utf-16le"), entry[66],
                            u32(entry, 116), struct.unpack_from("<Q", entry, 120)[0],
                            u32(entry, 68), u32(entry, 72), u32(entry, 76)))
        roots = [e for e in entries if e[1] == 5]
        if len(roots) != 1:
            raise ValueError("Expected one compound root")
        mini_fat_stream = large(u32(head, 60), u32(head, 64) * sector)
        mini_raw = mini_fat_stream.read(0, mini_fat_stream.length)
        mini_fat = struct.unpack("<" + "I" * (len(mini_raw) // 4), mini_raw)
        mini = large(roots[0][2], roots[0][3])
        pending, visited, top = [roots[0][6]], set(), []
        while pending:
            i = pending.pop()
            if i == 0xFFFFFFFF:
                continue
            if i in visited or i >= len(entries):
                raise ValueError("Invalid directory tree")
            visited.add(i)
            top.append(entries[i])
            pending.extend(entries[i][4:6])
        self.streams = {}
        for name in ("Nodes.dat", "MetaData.dat", "Channels.dat"):
            matches = [e for e in top if e[0] == name and e[1] == 2]
            if len(matches) != 1:
                raise ValueError("Expected one stream: " + name)
            _, _, first, length = matches[0][:4]
            if length < u32(head, 56):
                ids = chain(first, mini_fat)
                if len(ids) != (length + mini_sector - 1) // mini_sector:
                    raise ValueError("Mini stream size and chain disagree")
                pieces = [p for k, i in enumerate(ids) for p in mini.pieces(i * mini_sector, min(mini_sector, length - k * mini_sector))]
                self.streams[name] = Stream(reader, pieces, length)
            else:
                self.streams[name] = large(first, length)


def inventory(stream, probe_bytes):
    head = stream.read(0, min(10, stream.length))
    if len(head) != 10 or head[:6] != CHANNELS_HEADER:
        raise ValueError("Unknown or truncated Channels header")
    count = struct.unpack_from("<I", head, 6)[0]
    if count > (stream.length - 10) // 25:
        raise ValueError("Impossible channel record count")
    records, pos = [], 10
    for _ in range(count):
        def read_head(need):
            if pos + need > stream.length:
                raise ValueError("Truncated channel header")
            return stream.read(pos, min(max(need, probe_bytes), stream.length - pos))
        data = read_head(2)
        kind_len = struct.unpack_from("<H", data)[0]
        cur = 2 + kind_len
        if len(data) < cur + 3:
            data = read_head(cur + 3)
        kind = data[2:cur].decode("ascii")
        if kind not in DIMENSIONS or data[cur] != 4:
            raise ValueError("Unsupported channel kind or version")
        chars = struct.unpack_from("<H", data, cur + 1)[0]
        uid = cur + 3 + chars * 2
        header = uid + 16 + 4 * DIMENSIONS[kind] + 4
        if len(data) < header:
            data = read_head(header)
        name = data[cur + 3:uid].decode("utf-16le")
        n = struct.unpack_from("<I", data, header - 4)[0]
        stride = 4 + 4 * DIMENSIONS[kind]
        end = pos + header + n * stride
        if end > stream.length:
            raise ValueError("Truncated channel samples")
        records.append(dict(start=pos, header=header, data=pos + header, count=n,
                            stride=stride, analog=name.startswith("AnalogChannel"), end=end))
        pos = end
    if pos != stream.length:
        raise ValueError("Unexpected trailing channel bytes")
    return records


class RemoteReader:
    def __init__(self, source, size):
        self.source, self.size, self.bytes, self.ranges = source, size, 0, 0

    def read(self, offset, length):
        if offset < 0 or length < 0 or offset + length > self.size:
            raise ValueError("Index range outside file")
        self.source.seek(offset)
        data = self.source.read(length)
        if len(data) != length:
            raise ValueError("Truncated index source")
        self.bytes += length
        self.ranges += 1
        return data

    def many(self, ranges):
        return b"".join(self.read(a, n) for a, n in ranges)


def index_reply(source, size, request):
    reader = RemoteReader(source, size)
    if request["probe_bytes"] < 2:
        raise ValueError("Invalid index probe size")
    stream = Stream(reader, request["pieces"], request["length"])
    records = inventory(stream, request["probe_bytes"])
    return dict(records=records, header_read_bytes=reader.bytes, header_read_ranges=reader.ranges)


def remote_helper():
    definitions = "import struct\nCHANNELS_HEADER = " + repr(CHANNELS_HEADER) + "\nDIMENSIONS = " + repr(DIMENSIONS) + "\n"
    definitions += "\n".join(inspect.getsource(item) for item in (Stream, inventory, RemoteReader, index_reply))
    branch = '''        elif isinstance(request, dict) and request.get("op") == "channel_index":
            reply = index_reply(source, initial[2], request)
            if signature() != initial:
                raise RuntimeError("Source changed during index discovery")
            print(json.dumps(reply), flush=True)
'''
    return definitions + "\n" + base.REMOTE.replace("        else:\n            for offset, length in request:", branch + "        else:\n            for offset, length in request:")


def remote_inventory(session, stream, probe_bytes):
    session.send(dict(op="channel_index", pieces=stream.mapping, length=stream.length, probe_bytes=probe_bytes))
    raw = session.process.stdout.readline()
    reply = json.loads(raw)
    session.bytes += len(raw)
    session.requests += 1
    return reply, len(raw)


def payload(stream, records, windows=None):
    spans = []
    for r in records:
        first, last = (0, r["count"]) if windows is None else windows[r["start"]]
        if not 0 <= first <= last <= r["count"]:
            raise ValueError("Invalid sample window")
        spans.extend([(r["start"], r["header"]), (r["data"] + first * r["stride"], (last - first) * r["stride"])])
    raw, pos, result = stream.many(spans), 0, [CHANNELS_HEADER, struct.pack("<I", len(records))]
    for r in records:
        first, last = (0, r["count"]) if windows is None else windows[r["start"]]
        header = bytearray(raw[pos:pos + r["header"]])
        struct.pack_into("<I", header, len(header) - 4, last - first)
        pos += r["header"]
        n = (last - first) * r["stride"]
        result.extend([bytes(header), raw[pos:pos + n]])
        pos += n
    return b"".join(result)


def preview_windows(stream, records, frame_limit):
    spans = [(r["data"], min(r["count"], frame_limit) * r["stride"]) for r in records]
    data, cursor, windows = stream.many(spans), 0, {}
    for r, (_, n) in zip(records, spans):
        frames = [struct.unpack_from("<I", data, p)[0] for p in range(cursor, cursor + n, r["stride"])]
        keep = sum(frame < frame_limit for frame in frames)
        if any(frame >= frame_limit for frame in frames[:keep]):
            raise ValueError("Preview requires sorted frame IDs")
        windows[r["start"]] = (0, keep)
        cursor += n
    return windows


def run_mode(session, size, config, mode):
    reader = IntervalReader(session, size)
    start, before = time.perf_counter(), (session.bytes, session.requests, session.ranges)
    def snapshot():
        return dict(ms=(time.perf_counter() - start) * 1000, bytes=session.bytes - before[0],
                    requests=session.requests - before[1], ranges=session.ranges - before[2])
    stages, outputs, records = [], [], []
    row = dict(mode=mode, bootstrap=None, index=None)
    if mode == "selected_all":
        streams = base.selected_streams(reader)
        stages.append(dict(stage="complete", **snapshot()))
        outputs.append(dict(stage="complete", streams=streams))
    else:
        mapped = Compound(reader).streams
        row["bootstrap"] = snapshot()
        channels = mapped["Channels.dat"]
        if mode.startswith("server_index_"):
            reply, reply_bytes = remote_inventory(session, channels, config["probe_bytes"])
            records = reply["records"]
            row.update(index_reply_bytes=reply_bytes, index_local_read_bytes=reply["header_read_bytes"],
                       index_local_read_ranges=reply["header_read_ranges"])
        else:
            records = inventory(channels, config["probe_bytes"])
        row["index"] = snapshot()
        nonanalog = [r for r in records if not r["analog"]]
        common = {name: stream.read(0, stream.length) for name, stream in mapped.items() if name != "Channels.dat"}
        if "early_frames" in mode:
            windows = preview_windows(channels, nonanalog, config["preview_frame_limit"])
            streams = dict(common, **{"Channels.dat": payload(channels, nonanalog, windows)})
            stages.append(dict(stage="early_frames", **snapshot()))
            outputs.append(dict(stage="early_frames", streams=streams))
        streams = dict(common, **{"Channels.dat": payload(channels, nonanalog)})
        stages.append(dict(stage="nonanalog", **snapshot()))
        outputs.append(dict(stage="nonanalog", streams=streams))
        streams = dict(common, **{"Channels.dat": payload(channels, records)})
        stages.append(dict(stage="complete", **snapshot()))
        outputs.append(dict(stage="complete", streams=streams))
    row.update(stages=stages, first_stage_ms=stages[0]["ms"], complete_ms=stages[-1]["ms"],
               cache_payload_bytes=sum(len(block) for _, block in reader.cache))
    return row, outputs, reader, records


def verify(outputs, reader, reference, reference_streams, records, frame_limit):
    source = reference_streams["Channels.dat"]
    nonanalog = [r for r in records if not r["analog"]]
    expected = {"complete": source, "nonanalog": CHANNELS_HEADER + struct.pack("<I", len(nonanalog)) +
                b"".join(source[r["start"]:r["end"]] for r in nonanalog)}
    early = [CHANNELS_HEADER, struct.pack("<I", len(nonanalog))]
    for r in nonanalog:
        samples = [source[p:p + r["stride"]] for p in range(r["data"], r["end"], r["stride"])]
        frames = [struct.unpack_from("<I", sample)[0] for sample in samples]
        if any(a >= b for a, b in zip(frames, frames[1:])):
            raise ValueError("Preview assumption violated: frame IDs are not strictly increasing")
        kept = [sample for frame, sample in zip(frames, samples) if frame < frame_limit]
        header = bytearray(source[r["start"]:r["data"]])
        struct.pack_into("<I", header, len(header) - 4, len(kept))
        early.extend([bytes(header), b"".join(kept)])
    expected["early_frames"] = b"".join(early)
    digests = {}
    for output in outputs:
        target = dict(reference_streams, **{"Channels.dat": expected[output["stage"]]})
        digests[output["stage"]] = {}
        for name, actual in output["streams"].items():
            digest = hashlib.md5(actual).hexdigest()
            if digest != hashlib.md5(target[name]).hexdigest():
                raise ValueError("Stage MD5 mismatch: " + output["stage"] + "/" + name)
            digests[output["stage"]][name] = digest
    for offset, block in reader.cache:
        if hashlib.md5(block).digest() != hashlib.md5(reference[offset:offset + len(block)]).digest():
            raise ValueError("Cached range MD5 mismatch")
    return digests


def run(args, config):
    if config["rounds"] < 1 or config["probe_bytes"] < 2 or config["preview_frame_limit"] < 1:
        raise ValueError("Invalid experiment configuration")
    started = time.perf_counter()
    session = base.Session(args.host, args.file, config, helper_source=remote_helper())
    try:
        metadata = json.loads(session.process.stdout.readline())
        setup_ms = (time.perf_counter() - started) * 1000
        size = metadata["size"]
        if not 512 <= size <= config["maximum_file_bytes"]:
            raise ValueError("File outside experiment size limit")
        measurements, pending = [], []
        for round_number in range(config["rounds"]):
            shift = round_number % len(MODES)
            order = MODES[shift:] + MODES[:shift]
            for mode in order:
                row, outputs, reader, records = run_mode(session, size, config, mode)
                row["round"] = round_number + 1
                measurements.append(row)
                pending.append((row, outputs, reader))
        # Full reference and channel inventory are deliberately obtained after all timings.
        reference = session.fetch([(0, size)])[0]
        streams = base.selected_streams(base.MemoryReader(reference))
        channel_stream = Stream(base.MemoryReader(streams["Channels.dat"]), [(0, len(streams["Channels.dat"]))], len(streams["Channels.dat"]))
        records = inventory(channel_stream, config["probe_bytes"])
        for row, outputs, reader in pending:
            row["verified_stage_md5"] = verify(outputs, reader, reference, streams, records, config["preview_frame_limit"])
        session.send("md5")
        digest = session.process.stdout.readline().decode().strip()
        if digest != hashlib.md5(reference).hexdigest():
            raise ValueError("Remote and full-reference MD5 differ")
        return dict(file_bytes=size, connection_setup_ms=setup_ms, config=config, measurements=measurements,
                    reference_md5=digest, stage_md5_verified=True, range_md5_verified=True,
                    stream_bytes={k: len(v) for k, v in streams.items()},
                    channel_records={"nonanalog": sum(not r["analog"] for r in records),
                                     "analog": sum(r["analog"] for r in records)},
                    limitations=["Transport and payload reconstruction only; no viewer decode or render",
                                 "SSH read-only Python helper; not SFTP",
                                 "Received application bytes count source ranges and index JSON replies; outgoing JSON, range acknowledgments and SSH framing excluded",
                                 "Server index probes channel headers locally and transfers descriptors only; payload cache starts cold",
                                 "Each mode has a cold logical client cache; server filesystem cache may be warm",
                                 "Connection setup is separate; cumulative stage timings include bootstrap and index discovery",
                                 "Preview is frame IDs [0, limit), assumes strictly increasing unsigned IDs verified against reference",
                                 "Selected-all baseline retains existing compound extraction behavior",
                                 "Cache payload excludes reconstructed outputs and is not peak process memory"])
    finally:
        session.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--file", required=True)
    parser.add_argument("--config", type=Path, default=Path(__file__).resolve().parents[1] / "configs/ssh_progressive_experiment.yaml")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = run(args, json.loads(args.config.read_text(encoding="utf-8-sig")))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8-sig")
    print(json.dumps(report, indent=2))
