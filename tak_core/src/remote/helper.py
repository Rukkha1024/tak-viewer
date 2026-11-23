"""Serve bounded TAK reads and server-side TAKM edits over stdin/stdout for SSH.
Send markers before analog; retain opaque analog data on the server during saves.
Check source identity and copied streams; never modify original TAK files.
"""

import bisect
import csv
import hashlib
import io
import tempfile
import datetime
import json
import os
import re
import stat
import struct
import sys
import time
import zlib
import xml.etree.ElementTree as ET
from xml.parsers import expat

CHANNELS_HEADER = bytes.fromhex("f30a394c0200")
DIMENSIONS = {"FloatChannel": 1, "Float16Channel": 16, "Vector3fChannel": 3,
              "QuaternionChannel": 4,
              "class MoCapCore::cChannel<struct MoCapCore::sArrayChannelValues<float,16> >": 16}
INDEX_LIMIT = 64 << 20
REQUEST_LIMIT = 1 << 20


class Failure(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code


class Stream:
    def __init__(self, reader, pieces, length):
        self.reader, self.mapping, self.length = reader, [], length
        for offset, count in pieces:
            if self.mapping and self.mapping[-1][0] + self.mapping[-1][1] == offset:
                old, size = self.mapping[-1]
                self.mapping[-1] = (old, size + count)
            else:
                self.mapping.append((offset, count))
        self.ends, total = [], 0
        for _, count in self.mapping:
            total += count
            self.ends.append(total)
        if total < length:
            raise ValueError("Stream chain shorter than declared size")

    def pieces(self, offset, length):
        if offset < 0 or length < 0 or offset + length > self.length:
            raise ValueError("Range outside stream")
        result = []
        index = bisect.bisect_right(self.ends, offset)
        logical = self.ends[index - 1] if index else 0
        while index < len(self.mapping) and logical < offset + length:
            physical, count = self.mapping[index]
            a, b = max(offset, logical), min(offset + length, logical + count)
            if a < b:
                result.append((physical + a - logical, b - a))
            logical += count
            index += 1
        return result

    def many(self, ranges):
        return self.reader.many([piece for a, n in ranges for piece in self.pieces(a, n)])

    def read(self, offset, length):
        return self.many([(offset, length)])


class Compound:
    def __init__(self, reader, metadata_only=False):
        head = reader.read(0, 512)
        if head[:8] != bytes.fromhex("d0cf11e0a1b11ae1"):
            raise Failure("unsupported_format", "Only uncompressed compound TAK is supported")
        u32 = lambda data, pos: struct.unpack_from("<I", data, pos)[0]
        shift, mini_shift = struct.unpack_from("<HH", head, 30)
        sector, mini_sector = 1 << shift, 1 << mini_shift
        count = u32(head, 44)
        if sector not in (512, 4096, 32768) or mini_shift > shift or count > 109 or u32(head, 72):
            raise Failure("unsupported_format", "Unsupported sector size or extended DIFAT")
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
        if directory.length > INDEX_LIMIT:
            raise ValueError("Compound directory exceeds index limit")
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
        if mini_fat_stream.length > INDEX_LIMIT:
            raise ValueError("Mini FAT exceeds index limit")
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
        self.streams, self.all_streams = {}, []
        visited = set()
        def walk(i, parents, depth=0):
            if i == 0xFFFFFFFF:
                return
            if i in visited or i >= len(entries) or depth > 100:
                raise ValueError("Invalid directory tree")
            visited.add(i)
            entry = entries[i]
            walk(entry[4], parents, depth + 1)
            name = "/".join(parents + [entry[0]])
            if entry[1] == 1:
                walk(entry[6], parents + [entry[0]], depth + 1)
            elif entry[1] == 2:
                if metadata_only and name != "MetaData.dat":
                    walk(entry[5], parents, depth + 1)
                    return
                first, length = entry[2:4]
                if length == 0:
                    stream = Stream(reader, [], 0)
                elif length < u32(head, 56):
                    ids = chain(first, mini_fat)
                    if len(ids) != (length + mini_sector - 1) // mini_sector:
                        raise ValueError("Mini stream size and chain disagree")
                    pieces = [p for k, sector_id in enumerate(ids) for p in mini.pieces(sector_id * mini_sector, min(mini_sector, length - k * mini_sector))]
                    stream = Stream(reader, pieces, length)
                else:
                    stream = large(first, length)
                self.all_streams.append((name, stream))
                if name in ("Nodes.dat", "MetaData.dat", "Channels.dat"):
                    if name in self.streams:
                        raise ValueError("Duplicate selected stream")
                    self.streams[name] = stream
            else:
                raise ValueError("Unexpected directory entry")
            walk(entry[5], parents, depth + 1)
        walk(roots[0][6], [])
        if not metadata_only and len(self.streams) != 3:
            raise ValueError("Missing selected stream")


def inventory(stream, probe_bytes):
    head = stream.read(0, min(10, stream.length))
    if len(head) != 10 or head[:6] != CHANNELS_HEADER:
        raise ValueError("Unknown or truncated Channels header")
    count = struct.unpack_from("<I", head, 6)[0]
    if count > 1000000 or count > (stream.length - 10) // 25:
        raise ValueError("Impossible channel record count")
    records, pos = [], 10
    for _ in range(count):
        def read_head(need):
            if pos + need > stream.length:
                raise ValueError("Truncated channel header")
            # A speculative probe can cross into EMG samples. Only headers may
            # be read while building the inventory, even with a large probe.
            return stream.read(pos, need)
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
                            stride=stride, head=data, analog=name.startswith("AnalogChannel"),
                            name=name, uid=data[uid:uid + 16], end=end))
        pos = end
    if pos != stream.length:
        raise ValueError("Unexpected trailing channel bytes")
    return records


class Reader:
    def __init__(self, file, size):
        self.file, self.size = file, size

    def read(self, offset, length):
        if offset < 0 or length < 0 or offset + length > self.size:
            raise ValueError("Range outside file or truncated source")
        self.file.seek(offset)
        block = self.file.read(length)
        if len(block) != length:
            raise ValueError("Truncated source")
        return block

    def many(self, ranges):
        return b"".join(self.read(offset, length) for offset, length in ranges)


def identity(file, path):
    def fields(value):
        return [value.st_dev, value.st_ino, value.st_size, value.st_mtime_ns, value.st_ctime_ns]
    try:
        opened = os.fstat(file.fileno())
        named = os.stat(path)
    except OSError as error:
        raise Failure("source_changed", "Source disappeared: " + str(error))
    # Windows fstat/stat expose different ctime values; compare each to itself later.
    if not stat.S_ISREG(opened.st_mode) or fields(opened)[:4] != fields(named)[:4]:
        raise Failure("source_changed", "Opened source and path identity differ")
    return fields(opened) + fields(named)


def takm_index(reader):
    head = reader.read(0, min(64, reader.size))
    if len(head) < 64:
        raise ValueError("Truncated TAKM header")
    if struct.unpack_from("<I", head, 8)[0] != 1:
        raise Failure("unsupported_format", "Unsupported TAKM layout")
    offset, length, fixed = struct.unpack_from("<QQQ", head, 16)
    if offset < 64 or length > 16 << 20 or offset + length != fixed or fixed + 32 > reader.size:
        raise ValueError("Invalid or truncated TAKM table")
    table = json.loads(reader.read(offset, length))
    if not isinstance(table, dict) or not isinstance(table.get("streams"), list):
        raise ValueError("Invalid TAKM stream table")
    mapped = {}
    for row in table["streams"]:
        if not isinstance(row, dict) or not isinstance(row.get("name"), str):
            raise ValueError("Invalid TAKM stream row")
        start, size = row.get("offset"), row.get("size")
        if (type(start) is not int or type(size) is not int or start < 64 or size < 0
                or start + size > offset or type(row.get("index")) is not int or row["index"] < 0):
            raise ValueError("TAKM stream outside body")
        if row["name"] in ("Nodes.dat", "MetaData.dat", "Channels.dat"):
            if row["name"] in mapped:
                raise ValueError("Duplicate TAKM selected stream")
            mapped[row["name"]] = Stream(reader, [(start, size)], size)
    if len(mapped) != 3:
        raise ValueError("Missing TAKM selected stream")
    return mapped, fixed + 32


class Source:
    def __init__(self, path, probe, progress_only=False):
        self.path = path
        self.file = open(path, "rb")
        try:
            self.identity = identity(self.file, path)
            self.reader = Reader(self.file, self.identity[2])
            magic = self.reader.read(0, min(8, self.reader.size))
            if magic == b"TAKM-2\r\n":
                self.format = "takm"
                self.streams, self.tail = takm_index(self.reader)
                offset, length, _ = struct.unpack_from("<QQQ", self.reader.read(16, 24))
                self.table = json.loads(self.reader.read(offset, length))
                self.all_streams = [(row["name"], Stream(self.reader, [(row["offset"], row["size"])], row["size"])) for row in self.table["streams"]]
            elif magic == bytes.fromhex("d0cf11e0a1b11ae1"):
                self.format = "tak"
                compound = Compound(self.reader)
                self.streams, self.all_streams, self.tail = compound.streams, compound.all_streams, self.reader.size
            else:
                raise Failure("unsupported_format", "Unsupported TAK/TAKM container; no full-file fallback")
            self.records = [] if progress_only else inventory(self.streams["Channels.dat"], probe)
            self.nodes = None
            self.check()
        except Exception:
            self.file.close()
            raise

    def check(self, expected=None):
        if identity(self.file, self.path) != (self.identity if expected is None else expected):
            raise Failure("source_changed", "Source changed since the initial read")

    def classify(self, maximum):
        if self.nodes is None:
            budget([self.streams["Nodes.dat"].length], maximum)
            self.nodes = self.streams["Nodes.dat"].read(0, self.streams["Nodes.dat"].length)
            try:
                registry = device_registry(self.nodes)
            except (ValueError, KeyError, TypeError, AttributeError, struct.error) as error:
                self.full_fallback = True
                self.plate_devices, self.device_types, self.fallback_reason = None, None, str(error)
                return
            self.full_fallback = False
            # A known device UID is device data even when its property has an
            # unsupported name. Do not send those samples with marker payloads.
            uids = registry[0]
            for record in self.records:
                record["analog"] = record["analog"] or record["uid"] in uids
            self.plate_devices, self.device_types, self.fallback_reason = classify_devices(self.nodes, self.records, registry)

    def channels(self, analog, devices=None, headers_only=False):
        stream = self.streams["Channels.dat"]
        selected = [record for record in self.records if record["analog"] == analog]
        result = [CHANNELS_HEADER, struct.pack("<I", len(selected))]
        for record in selected:
            if headers_only or (devices is not None and record["device"] not in devices):
                head = record["head"]
                result.append(head[:-4] + bytes(4))
            else:
                result.append(stream.read(record["start"], record["end"] - record["start"]))
        return b"".join(result)

    def channel_length(self, analog, devices=None, headers_only=False):
        return 10 + sum(record["header"] if headers_only or
                        (devices is not None and record["device"] not in devices)
                        else record["end"] - record["start"] for record in self.records
                        if record["analog"] == analog)


def device_node_offsets(nodes):
    generic = b"class MoCapCore::cDeviceNode<float,16>"
    signatures = [b"\x0a\x00ForcePlate", b"\x0a\x00" + "ForcePlate".encode("utf-16le"),
                  struct.pack("<H", len(generic)) + generic]
    matches = []
    for signature in signatures:
        cursor = 0
        while True:
            at = nodes.find(signature, cursor)
            if at < 0:
                break
            matches.append((at, at + len(signature)))
            cursor = at + 1
    return sorted(matches)


def device_registry(nodes):
    devices, plates = {}, []
    offsets = device_node_offsets(nodes)
    regions = []
    for _, at in offsets:
        chars = struct.unpack_from("<H", nodes, at)[0]
        regions.append((at + 2, at + 2 + chars * 2))
    # Unknown/malformed envelopes cannot be assigned marker/device ranges safely.
    for key in ("DeviceNode_DeviceType", "DeviceNode_Corner"):
        token = key.encode("utf-16le")
        if nodes.count(token) != sum(nodes[start:end].count(token) for start, end in regions):
            raise ValueError("unrecognized structural device node record")
    known_json = {start for start, _ in regions}
    cursor = 0
    while True:
        at = nodes.find(b"{\0", cursor)
        if at < 0:
            break
        cursor = at + 2
        if at < 2 or at in known_json:
            continue
        chars = struct.unpack_from("<H", nodes, at - 2)[0]
        end = at + chars * 2
        if not chars or end > len(nodes):
            continue
        try:
            obj = json.loads(nodes[at:end].decode("utf-16le"))
        except (ValueError, UnicodeError):
            continue
        if isinstance(obj, dict) and isinstance(obj.get("properties"), list):
            if any(isinstance(row, dict) and row.get("name") == "DeviceNode_DeviceType"
                   for row in obj["properties"]):
                raise ValueError("unrecognized structural device node record")
    for _, at in offsets:
        chars = struct.unpack_from("<H", nodes, at)[0]
        tail = at + 2 + chars * 2
        obj = json.loads(nodes[at + 2:tail].decode("utf-16le"))
        properties = {row["name"]: row.get("value") for row in obj.get("properties", [])}
        uid = nodes[tail + 19:tail + 35]
        kind = properties.get("DeviceNode_DeviceType")
        if str(kind) not in ("1", "2") or len(uid) != 16 or not any(uid) or uid in devices:
            raise ValueError("Device node UID/type is unknown or duplicated")
        if (int(kind) == 2) != any(key.startswith("DeviceNode_Corner") for key in properties):
            raise ValueError("Device type and forceplate geometry disagree")
        devices[uid] = int(kind)
        if int(kind) == 2:
            plates.append(uid)
    return devices, plates


def classify_devices(nodes, records, registry):
    """Join device node UIDs to channel header UIDs; never infer from names/order."""
    groups, current, next_channel = [], None, 0
    reason = None
    for record in records:
        if not record["analog"]:
            continue
        if not record["name"].startswith("AnalogChannel"):
            return None, None, "unsupported structural device channel property"
        try:
            index = int(record["name"][len("AnalogChannel"):])
        except ValueError:
            index = -1
        if index == 0:
            current = len(groups)
            groups.append(record["uid"])
            next_channel = 0
        record["device"] = current
        if current is None or index != next_channel or record["uid"] != groups[current]:
            reason = "Analog channel device UID/group is ambiguous"
        next_channel += 1
    if not groups:
        if reason:
            return None, None, reason
        if "DeviceNode_Corner".encode("utf-16le") in nodes:
            return None, None, "Forceplate geometry has no matching analog device"
        return [], [], None
    try:
        devices, plates = registry
        if reason:
            raise ValueError(reason)
        if len(set(groups)) != len(groups) or any(not any(uid) or uid not in devices for uid in groups):
            raise ValueError("Analog device UID is missing, duplicated or unmatched")
        if any(uid not in groups for uid in plates):
            raise ValueError("Forceplate node has no matching analog device")
        return [groups.index(uid) for uid in plates], [devices[uid] for uid in groups], None
    except (ValueError, KeyError, TypeError, AttributeError, struct.error) as error:
        return None, None, str(error)


def integer(request, key, minimum, maximum):
    value = request.get(key)
    if type(value) is not int or not minimum <= value <= maximum:
        raise Failure("invalid_request", "Invalid " + key)
    return value


def path_arg(request, allow_top=False):
    path = request.get("path")
    if not isinstance(path, str) or (not path and not allow_top) or "\0" in path or len(path) > 32768:
        raise Failure("invalid_request", "Invalid path")
    if not path:
        return "" if os.name == "nt" else "/"
    return os.path.abspath(os.path.expanduser(path))


def windows_drives():
    try:
        return os.listdrives(), "os.listdrives"
    except (AttributeError, OSError):
        pass
    # Old Python / restricted hosts: slow disconnected roots cannot block the
    # whole alphabet or helper shutdown. Only this discovery has a short wait.
    import queue
    import threading
    import time
    results = queue.Queue()

    def probe(root):
        try:
            exists = os.path.exists(root)
        except OSError:
            exists = False
        results.put(root if exists else None)

    for letter in "ABCDEFGHIJKLMNOPQRSTUVWXYZ":
        threading.Thread(target=probe, args=(letter + ":\\",), daemon=True).start()
    roots, deadline = [], time.monotonic() + 0.3
    for _ in range(26):
        try:
            root = results.get(timeout=max(0, deadline - time.monotonic()))
        except queue.Empty:
            break
        if root:
            roots.append(root)
    return sorted(roots), "exists (300 ms)"


def budget(lengths, maximum):
    if sum(lengths) > maximum:
        raise Failure("payload_too_large", "Selected payload exceeds max_bytes")


def encoded(header):
    return json.dumps(header, ensure_ascii=True, separators=(",", ":")).encode("ascii") + b"\n"


def shuffle_channels(channels, records, xor=True):
    """XOR consecutive words from a channel, then group by sample byte offset."""
    result = bytearray(channels)
    cursor = 10
    for row in records:
        start = cursor + row["header"]
        count = struct.unpack_from("<I", channels, start - 4)[0]
        stride = row["stride"]
        end = start + count * stride
        samples = channels[start:end]
        for lane in range(stride):
            column = samples[lane::stride]
            if xor:
                bits = int.from_bytes(column, "little")
                column = (bits ^ (int.from_bytes(column[:-1], "little") << 8)).to_bytes(count, "little")
            offset = start + lane * count
            result[offset:offset + count] = column
        cursor = end
    if cursor != len(channels):
        raise ValueError("Channel transform did not consume the stream")
    return result


def split_preview(channels, records, first, last, requested, maximum):
    preview = bytearray(channels[:10])
    tail = bytearray(channels[:10])
    presence = bytearray()
    cursor = 10
    for row in records:
        start = cursor + row["header"]
        stride, count = row["stride"], row["count"]
        end = start + stride * count
        head = channels[cursor:start]
        cursor = end
        if row["name"] != "Translation" or stride != 16:
            preview.extend(head[:-4] + bytes(4))
            tail.extend(channels[start - len(head):end])
            presence.extend(bytes(4))
            continue

        early, late = bytearray(), bytearray()
        runs = []
        prior = first - 2
        for i, (frame, _) in enumerate(struct.iter_unpack("<I12s", memoryview(channels)[start:end])):
            if frame <= prior or not first <= frame <= last:
                return None
            if frame == prior + 1:
                runs[-1] = (runs[-1][0], frame)
            else:
                if len(presence) + 4 + (len(runs) + 1) * 8 > maximum:
                    return None
                runs.append((frame, frame))
            prior = frame
            destination = early if (frame - first) % 8 == 0 or frame == requested else late
            pos = start + i * stride
            destination.extend(channels[pos:pos + stride])
        for output, samples in ((preview, early), (tail, late)):
            output.extend(head[:-4])
            output.extend(struct.pack("<I", len(samples) // stride))
            output.extend(samples)
        presence.extend(struct.pack("<I", len(runs)))
        for a, b in runs:
            presence.extend(struct.pack("<II", a, b))
    return preview, tail, presence


def preview_range(meta, requested, maximum):
    try:
        xml, _, _, _ = properties_xml(meta)
        props = {p.findtext("name", p.get("name")): p.findtext("value", p.get("value"))
                 for p in ET.fromstring(xml).findall("property")}
        first, last = int(props["StartFrame"]), int(props["EndFrame"])
    except (ValueError, TypeError, KeyError, ET.ParseError):
        return None
    # The viewer keeps an exact per-frame occlusion strip.
    if not (0 <= first <= last <= 0xFFFFFFFF and last - first + 1 <= maximum // 8):
        return None
    if type(requested) is not int or not first <= requested <= last:
        requested = first

    return first, last, requested


def sample_at(stream, row, frame):
    if row["name"] != "Translation" or row["stride"] != 16 or not row["count"]:
        return b""
    start, stride = row["data"], row["stride"]
    sample = stream.read(start, stride)
    first = struct.unpack_from("<I", sample)[0]
    if frame <= first:
        return sample if frame == first else b""
    lo, hi = 1, row["count"]
    while lo < hi:
        mid = (lo + hi) // 2
        sample = stream.read(start + mid * stride, stride)
        at = struct.unpack_from("<I", sample)[0]
        if at == frame:
            return sample
        if at < frame:
            lo = mid + 1
        else:
            hi = mid
    return b""


def send_first_pose(source, meta, records, frame, maximum, send):
    expanded_cap = len(source.nodes) + len(meta) + 10 + sum(row["header"] + row["stride"] for row in records)
    if expanded_cap > maximum:
        return
    channels = [CHANNELS_HEADER, struct.pack("<I", len(records))]
    stream = source.streams["Channels.dat"]
    for row in records:
        sample = sample_at(stream, row, frame)
        channels.extend((row["head"][:-4], struct.pack("<I", int(bool(sample))), sample))
    raw = [source.nodes, meta, b"".join(channels)]
    packed = [zlib.compress(block, 1) for block in raw]
    lengths = [len(block) for block in packed]
    if sum(lengths) <= maximum:
        source.check()
        send(dict(ok=True, first_pose=1, frame=frame, identity=source.identity,
                  file_size=source.reader.size, lengths=lengths,
                  expanded_lengths=[len(block) for block in raw]), packed)


def compress_part(part):
    return zlib.compress(part, 6)


def pco_columns(raw, records, pool):
    import numpy as np
    from pcodec import ChunkConfig, standalone

    side = bytearray(raw[:10])
    columns = []
    cursor = 10
    # Keep headers and grouped frame-index bytes in the zlib side block.
    # Each track contributes distinct contiguous X, Y and Z series.
    for row in records:
        stride = row["stride"]
        start = cursor + row["header"]
        count = struct.unpack_from("<I", raw, start - 4)[0]
        end = start + stride * count
        side.extend(raw[cursor:start])
        if row["name"] == "Translation" and stride == 16:
            words = np.frombuffer(raw, dtype="<u4", count=count * 4, offset=start).reshape(-1, 4)
            frames = words[:, 0].copy()
            frames[1:] = frames[1:] ^ frames[:-1]
            side.extend(frames.view("u1").reshape(-1, 4).T.tobytes())
            if count:
                columns.append(words[:, 1:].T.ravel())
        else:
            side.extend(raw[start:end])
        cursor = end
    if cursor != len(raw):
        raise ValueError("Invalid PCo channel block")
    values = np.concatenate(columns).view("<f4") if columns else np.empty(0, dtype="<f4")
    parts = np.array_split(values, min(4, max(1, len(values) // 32768)))

    def compress(part):
        return standalone.simple_compress(part, ChunkConfig(compression_level=8))

    packed = list(pool.map(compress, parts))
    side_bytes = zlib.compress(side, 1)
    header = struct.pack("<III", len(side), len(side_bytes), len(parts))
    header += b"".join(struct.pack("<II", len(part), len(encoded)) for part, encoded in zip(parts, packed))
    return header + side_bytes + b"".join(packed)


def try_pco(raw, records, pool, enabled):
    # On small blocks, retain the cheap codec and avoid cold optional imports.
    if not enabled or len(raw) < (256 << 10):
        return None
    try:
        payload = pco_columns(raw, records, pool)
        return payload if len(payload) < len(raw) else None
    except Exception:
        # Optional package/import/encode failure. The same source is still readable.
        return None


def progressive_open(source, header, blocks, records, frames, maximum, send_first, pool, pco):
    first, last, requested = frames
    split = split_preview(blocks[2], records, first, last, requested, maximum)
    if split is None:
        return None
    preview, tail, presence = split
    del split
    early = [blocks[0], blocks[1], preview, blocks[3], blocks[4], presence]
    expanded = [len(block) for block in early]
    if sum(expanded) > maximum:
        return None
    axes = try_pco(preview, records, pool, pco)
    early[2] = b"" if axes is not None else shuffle_channels(preview, records)
    packed = list(pool.map(lambda block: zlib.compress(block, 1), early)) if pool else [zlib.compress(block, 1) for block in early]
    if axes is not None:
        packed[2] = axes
    lengths = [len(block) for block in packed]
    if sum(lengths) > maximum:
        return None
    digest = hashlib.sha256(blocks[2]).hexdigest()
    stage = dict(header, progressive=1, lengths=lengths, expanded_lengths=expanded,
                 frame=requested, exact_bytes=len(blocks[2]), exact_sha256=digest,
                 remainder_bytes=len(tail), remainder_parts=4 if pool else 1)
    if axes is not None:
        stage["marker_encoding"] = "pco-axes-1"
    if len(encoded(stage)) > REQUEST_LIMIT:
        return None
    source.check()
    send_first(stage, packed)
    axes = try_pco(tail, records, pool, pco)
    if axes is not None and len(axes) <= maximum:
        source.check()
        return dict(ok=True, progressive=1, marker_encoding="pco-axes-1",
                    lengths=[len(axes)], expanded_lengths=[len(tail)], identity=source.identity), [axes]
    shuffled = memoryview(shuffle_channels(tail, records))
    parts = [shuffled[i * len(tail) // 4:(i + 1) * len(tail) // 4] for i in range(4)] if pool else [shuffled]
    late = list(pool.map(compress_part, parts)) if pool else [compress_part(shuffled)]
    lengths = [len(block) for block in late]
    budget(lengths, maximum)
    source.check()
    return dict(ok=True, progressive=1, lengths=lengths, expanded_lengths=[len(part) for part in parts],
                identity=source.identity), late


class Server:
    def __init__(self):
        self.source = None
        self.patch = None

    def close(self):
        if self.source:
            self.source.file.close()
            self.source = None

    def dispatch(self, request, send_first=None):
        if not isinstance(request, dict):
            raise Failure("invalid_request", "Request must be an object")
        op = request.get("op")
        if op == "hello":
            return dict(ok=True, lengths=[], protocol="tak-ssh-1", progressive_open=1, first_pose=1, motion_codecs=1, python_major=sys.version_info.major), []
        if op == "close":
            self.close()
            return dict(ok=True, lengths=[]), []
        if op == "progress_batch":
            return progress_batch(request)
        if op == "takm_progress":
            try:
                return write_dispatch(self, request)
            except Failure as error:
                if error.code == "source_changed":
                    raise Failure("progress_changed", str(error)) from error
                raise
        if op in ("stat", "comment_ids", "takm_convert", "takm_append", "takm_save_patch", "patch_begin", "patch_chunk"):
            return write_dispatch(self, request)
        if op not in ("list", "open", "analog"):
            raise Failure("invalid_request", "Unknown operation")
        path = path_arg(request, allow_top=op == "list")
        maximum = integer(request, "max_bytes", 1, (1 << 63) - 1)
        if op == "list":
            limit = integer(request, "limit", 1, 10000)
            entries, truncated = [], False
            drive_source = None
            if path == "":
                drives, drive_source = windows_drives()
                truncated = len(drives) > limit
                entries = [dict(directory=True, name=root, path=root, size=0) for root in drives[:limit]]
            else:
                with os.scandir(path) as listing:
                    for entry in listing:
                        directory = entry.is_dir()
                        if not directory and not (entry.is_file() and entry.name.lower().endswith((".tak", ".takm"))):
                            continue
                        if len(entries) == limit:
                            truncated = True
                            break
                        entries.append(dict(directory=directory, name=entry.name, path=os.path.join(path, entry.name),
                                            size=entry.stat().st_size if not directory else 0))
            entries.sort(key=lambda row: (not row["directory"], row["name"].lower(), row["name"]))
            header = dict(ok=True, lengths=[], path=path, entries=entries, truncated=truncated)
            if drive_source:
                header["drive_source"] = drive_source
            header_length = len(encoded(header))
            if header_length > REQUEST_LIMIT:
                raise Failure("payload_too_large", "Response header exceeds the 1 MiB transport limit")
            budget([header_length], maximum)
            return header, []
        probe = integer(request, "probe_bytes", 2, 1 << 20)
        expected = request.get("expected_identity")
        if op == "analog" and (not isinstance(expected, list) or len(expected) != 10
                               or any(type(value) is not int or value < 0 for value in expected)):
            raise Failure("invalid_request", "Invalid expected_identity")
        if op == "open" or self.source is None or self.source.path != path:
            self.close()
            # Compare before indexing so a replaced or truncated source reports source_changed.
            if op == "analog":
                try:
                    with open(path, "rb") as file:
                        if identity(file, path) != expected:
                            raise Failure("source_changed", "Source differs from the initial open")
                except OSError as error:
                    raise Failure("source_changed", "Source cannot be reopened: " + str(error))
            self.source = Source(path, probe)
        source = self.source
        source.check(expected if op == "analog" else None)
        maximum = min(maximum, source.reader.size + (64 << 10))
        source.classify(maximum)
        frames = None
        streaming = send_first is not None and request.get("progressive") in (1, 2)
        first_pose = request.get("progressive") == 2
        if op == "open" and source.full_fallback:
            lengths = [len(source.nodes), source.streams["MetaData.dat"].length,
                       source.streams["Channels.dat"].length, source.reader.size - source.tail]
            budget(lengths, maximum)
            # Establish and expose the reason before any unclassified sample read.
            print("TAK SSH full-read fallback: " + source.fallback_reason, file=sys.stderr, flush=True)
            blocks = [source.nodes, source.streams["MetaData.dat"].read(0, lengths[1]),
                      source.streams["Channels.dat"].read(0, lengths[2]), source.reader.read(source.tail, lengths[3])]
            header = dict(ok=True, lengths=lengths, identity=source.identity, file_size=source.reader.size,
                          has_analog=False, format=source.format, full_fallback=True,
                          channel_order=[False for _ in source.records], analog_fallback=source.fallback_reason)
        elif op == "open":
            lengths = [source.streams["Nodes.dat"].length, source.streams["MetaData.dat"].length,
                       source.channel_length(False), source.reader.size - source.tail,
                       source.channel_length(True, headers_only=True)]
            budget(lengths, maximum)
            meta = source.streams["MetaData.dat"].read(0, lengths[1])
            if streaming:
                frames = preview_range(meta, request.get("frame"), maximum)
                if frames is not None:
                    records = [row for row in source.records if not row["analog"]]
                    if first_pose:
                        send_first_pose(source, meta, records, frames[2], maximum, send_first)
            blocks = [source.nodes, meta]
            blocks.extend([source.channels(False), source.reader.read(source.tail, lengths[3]),
                           source.channels(True, headers_only=True)])
            header = dict(ok=True, lengths=lengths, identity=source.identity, file_size=source.reader.size,
                          has_analog=any(record["analog"] for record in source.records), format=source.format,
                          channel_order=[record["analog"] for record in source.records],
                          plate_devices=source.plate_devices, device_types=source.device_types,
                          analog_fallback=source.fallback_reason)
        else:
            selection = request.get("selection", "all")
            if selection not in ("all", "forceplates", "non_forceplates"):
                raise Failure("invalid_request", "Invalid analog selection")
            if source.device_types is None or selection == "all":
                selected = None
            else:
                kind = 2 if selection == "forceplates" else 1
                selected = {i for i, value in enumerate(source.device_types) if value == kind}
            lengths = [source.channel_length(True, selected)]
            budget(lengths, maximum)
            blocks = [source.channels(True, selected)]
            header = dict(ok=True, lengths=lengths, identity=source.identity)
        source.check(expected if op == "analog" else None)
        if op == "open":
            header["fixed_len"] = source.tail - 32 if source.format == "takm" else source.reader.size
            header["next_comment_id"] = 0 if source.format == "tak" else 1
        if len(encoded(header)) > REQUEST_LIMIT:
            raise Failure("payload_too_large", "Response header exceeds transport limit")
        if frames is not None:
            if first_pose:
                from concurrent.futures import ThreadPoolExecutor
                with ThreadPoolExecutor(max_workers=4) as pool:
                    result = progressive_open(source, header, blocks, records, frames, maximum, send_first, pool,
                                              request.get("marker_encoding") == "pco-axes-1")
            else:
                result = progressive_open(source, header, blocks, records, frames, maximum, send_first, None, False)
            if result is not None:
                return result
        if op == "analog" and request.get("codec") == 1:
            records = [row for row in source.records if row["analog"]]
            offered = request.get("analog_encodings")
            grouped = request.get("selection") == "forceplates" and isinstance(offered, list) and "zlib-field-1" in offered
            encoding = None
            if grouped:
                raw = shuffle_channels(blocks[0], records, xor=False)
                packed = None
                if "zstd-field-1" in offered and len(raw) >= (256 << 10):
                    try:
                        import zstandard
                        candidate = zstandard.ZstdCompressor(level=3).compress(raw)
                        if len(candidate) < len(raw):
                            packed, encoding = candidate, "zstd-field-1"
                    except Exception:
                        pass
                if packed is None:
                    packed, encoding = zlib.compress(raw, 1), "zlib-field-1"
            else:
                packed = zlib.compress(shuffle_channels(blocks[0], records), 6)
            if len(packed) <= maximum:
                source.check(expected)
                packed_header = dict(header, lengths=[len(packed)], expanded_lengths=lengths, codec=1,
                                     sha256=hashlib.sha256(blocks[0]).hexdigest())
                if encoding is not None:
                    packed_header["analog_encoding"] = encoding
                return packed_header, [packed]
        return header, blocks


COMMENT_HEADER = b"time,kind,comment_id,markers,frame_start,frame_end,comment_type,comment_text,shown_a,shown_b,file_a,file_b\r\n"
CAMERA_STREAM = "DeviceData/Track 0.trk"
COPY_CHUNK = 65536


class ProgressReader(Reader):
    def __init__(self, file, size):
        super().__init__(file, size)
        self.remaining = 1 << 20
        self.calls = 128

    def read(self, offset, length):
        self.remaining -= length
        self.calls -= 1
        if self.remaining < 0 or self.calls < 0:
            raise ValueError("Progress read exceeds budget")
        return super().read(offset, length)


def properties_xml(raw):
    for encoding in ("utf-16le", "utf-8"):
        opening = "<properties>".encode(encoding)
        closing = "</properties>".encode(encoding)
        start = raw.find(opening)
        end = raw.find(closing, start + len(opening)) if start >= 0 else -1
        if end >= 0:
            end += len(closing)
            return raw[start:end].decode(encoding).encode("utf-8"), start, end, encoding
    raise ValueError("Metadata XML missing")


def progress_properties(xml):
    return [element for element in ET.fromstring(xml).findall("property")
            if element.findtext("name", element.get("name")) == "Progress"]


def progress_number(element):
    value = int(element.findtext("value", element.get("value")))
    if not -(1 << 63) <= value < (1 << 63):
        raise ValueError("Progress outside integer range")
    return value


def progress_batch(request):
    paths = request.get("paths")
    if not isinstance(paths, list) or not 1 <= len(paths) <= 16:
        raise Failure("invalid_request", "Expected a bounded batch of paths")
    rows = []
    for requested in paths:
        path = path_arg({"path": requested})
        result = None
        try:
            if not path.lower().endswith((".tak", ".takm")) or not os.path.isfile(path):
                raise ValueError("Progress requires a take")
            with open(path, "rb") as file:
                version = identity(file, path)
                reader = ProgressReader(file, version[2])
                streams = (takm_index(reader)[0] if reader.read(0, 8) == b"TAKM-2\r\n"
                           else Compound(reader, metadata_only=True).streams)
                stream = streams["MetaData.dat"]
                if stream.length > 256 << 10:
                    raise ValueError("Metadata exceeds progress budget")
                props = progress_properties(properties_xml(stream.read(0, stream.length))[0])
                value = progress_number(props[-1]) if props else None
                if version == identity(file, path):
                    result = dict(value=value, identity=version)
        except (OSError, ValueError, KeyError, TypeError, Failure, ET.ParseError, struct.error):
            pass
        rows.append(result)
    return dict(ok=True, lengths=[], rows=rows), []


_XML_ATTR = re.compile(rb"""([^\s=]+)\s*=\s*(?:"([^"]*)"|'([^']*)')""")


def patch_progress(raw, before, after):
    if any(n is not None and (type(n) is not int or not -(1 << 63) <= n < (1 << 63))
           for n in (before, after)):
        raise ValueError("Invalid progress value")
    xml, start, end, encoding = properties_xml(raw)
    props = progress_properties(xml)
    if len(props) > 1 or (progress_number(props[0]) if props else None) != before:
        raise Failure("source_changed", "Progress changed or is ambiguous")
    if not props:
        if after is None:
            return raw
        pos = len(xml) - len(b"</properties>")
        replacement = f'<property name="Progress" value="{after}"/>'.encode()
        patched = xml[:pos] + replacement + xml[pos:]
    else:
        # ElementTree identifies the property. Expat provides exact source byte positions
        # so the rest of the XML (including comments and attribute formatting) survives.
        root = ET.fromstring(xml)
        property_index = next(i for i, child in enumerate(root)
                              if child.tag == "property" and child.findtext("name", child.get("name")) == "Progress")
        parser = expat.ParserCreate()
        depth, ordinal, selected = 0, -1, False
        spans = {}
        def opening(name, attrs):
            nonlocal depth, ordinal, selected
            if depth == 1:
                ordinal += 1
                selected = ordinal == property_index
                if selected:
                    spans["property"] = parser.CurrentByteIndex
            elif selected and depth == 2 and name == "value" and "value" not in spans:
                spans["value"] = parser.CurrentByteIndex
            depth += 1
        def closing(name):
            nonlocal depth, selected
            depth -= 1
            if selected and depth == 1:
                spans["property_end"] = parser.CurrentByteIndex
                selected = False
            elif selected and depth == 2 and name == "value" and "value_end" not in spans:
                spans["value_end"] = parser.CurrentByteIndex
        parser.StartElementHandler, parser.EndElementHandler = opening, closing
        parser.Parse(xml, True)
        if after is None:
            a, b = spans["property"], spans["property_end"]
            tag = re.match(rb"""(?:[^>"']|"[^"]*"|'[^']*')*>""", xml[a:])
            if tag is None:
                raise ValueError("Invalid Progress tag")
            if not tag[0].endswith(b"/>"):
                b = xml.index(b">", b) + 1
            replacement = b""
        elif "value" in spans:
            value = props[0].find("value")
            a = xml.index(b">", spans["value"]) + 1
            b = spans["value_end"]
            if len(value) or b < a or b"<" in xml[a:b]:
                raise ValueError("Unsupported Progress value")
            replacement = str(after).encode()
        else:
            pos = spans["property"] + len(b"<property")
            for match in _XML_ATTR.finditer(xml, pos):
                if xml[pos:match.start()].strip() or match[1].startswith((b">", b"/>", b"<")):
                    break
                if match[1] == b"value":
                    a, b = match.span(2 if match[2] is not None else 3)
                    break
                pos = match.end()
            else:
                raise ValueError("Missing Progress value")
            if "a" not in locals():
                raise ValueError("Missing Progress value")
            replacement = str(after).encode()
        if after is not None:
            value = xml[a:b].decode("utf-8")
            a += len(value[:len(value) - len(value.lstrip())].encode("utf-8"))
            b -= len(value[len(value.rstrip()):].encode("utf-8"))
        patched = xml[:a] + replacement + xml[b:]
    return raw[:start] + patched.decode("utf-8").encode(encoding) + raw[end:]


def write_progress(source, output, before, after):
    if source.format != "takm":
        raise Failure("invalid_request", "Progress needs an existing TAKM")
    source_seal(source)
    reader = source.reader
    header = bytearray(reader.read(0, 64))
    table_at, table_size, old_fixed = struct.unpack_from("<QQQ", header, 16)
    meta = source.streams["MetaData.dat"]
    if meta.length > 256 << 10:
        raise ValueError("Metadata exceeds progress budget")
    start = meta.mapping[0][0]
    end = start + meta.length
    patched = patch_progress(meta.read(0, meta.length), before, after)
    shift = len(patched) - meta.length
    previous, changed_table = 64, bool(shift)
    for row in source.table["streams"]:
        a = row["offset"]
        if a < previous:
            raise ValueError("Overlapping or unordered TAKM streams")
        previous = a + row["size"]
        if row["name"] == "MetaData.dat":
            row["size"] = len(patched)
            if "sha256" in row:
                row["sha256"] = hashlib.sha256(patched).hexdigest()
                changed_table = True
        elif a >= end:
            row["offset"] += shift
    table = (json.dumps(source.table, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
             if changed_table else reader.read(table_at, table_size))
    fixed = table_at + shift + len(table)
    struct.pack_into("<QQQ", header, 16, table_at + shift, len(table), fixed)
    digest = hashlib.sha256()
    def put(block):
        output.write(block)
        digest.update(block)
    def copy(a, b, write):
        for offset in range(a, b, COPY_CHUNK):
            write(reader.read(offset, min(COPY_CHUNK, b - offset)))
    put(header)
    copy(64, start, put)
    put(patched)
    copy(end, table_at, put)
    put(table)
    output.write(digest.digest())
    copy(old_fixed + 32, reader.size, output.write)
    output.flush()
    os.fsync(output.fileno())
    written = Reader(output, output.tell())
    takm_index(written)
    if hash_range(written, 0, fixed) != written.read(fixed, 32):
        raise Failure("verification_failed", "Written TAKM seal does not match")
    if hash_range(written, fixed + 32, reader.size - source.tail) != hash_range(reader, source.tail, reader.size - source.tail):
        raise Failure("verification_failed", "Written Comment tail differs")
    return fixed


def expected_identity(request):
    value = request.get("expected_identity")
    if not isinstance(value, list) or len(value) != 10 or any(type(n) is not int or n < 0 for n in value):
        raise Failure("invalid_request", "Invalid expected_identity")
    return value


def hash_range(reader, start, length):
    digest = hashlib.sha256()
    for offset in range(start, start + length, COPY_CHUNK):
        digest.update(reader.read(offset, min(COPY_CHUNK, start + length - offset)))
    return digest.digest()


def stream_chunks(stream):
    for offset in range(0, stream.length, COPY_CHUNK):
        yield stream.read(offset, min(COPY_CHUNK, stream.length - offset))


def source_seal(source):
    if source.format == "takm":
        fixed = source.tail - 32
        if hash_range(source.reader, 0, fixed) != source.reader.read(fixed, 32):
            raise Failure("invalid_file", "TAKM seal does not match")


def write_paths(request, path):
    if not path.lower().endswith((".tak", ".takm")) or os.path.islink(path):
        raise Failure("invalid_request", "Write source must be a regular TAK/TAKM, not a symbolic link")
    folder = os.path.realpath(os.path.dirname(path))
    destination = path if path.lower().endswith(".takm") else os.path.splitext(path)[0] + ".takm"
    explicit = request.get("destination", destination)
    if not isinstance(explicit, str) or "\0" in explicit or os.path.normcase(os.path.abspath(explicit)) != os.path.normcase(destination):
        raise Failure("invalid_request", "Destination must be the requested file or its TAKM sibling")
    if os.path.normcase(os.path.realpath(os.path.dirname(destination))) != os.path.normcase(folder) or os.path.islink(destination):
        raise Failure("invalid_request", "Destination is outside source folder or is a symbolic link")
    if path.lower().endswith(".takm") and os.stat(path).st_nlink != 1:
        raise Failure("invalid_request", "TAKM with hard-link aliases cannot be written")
    return destination


def complete_csv(raw):
    """Keep complete legacy CSV records; a torn quoted final record is ignored."""
    if len(raw) > INDEX_LIMIT:
        raise Failure("payload_too_large", "Legacy Comment CSV exceeds limit")
    if raw.startswith(b"\xef\xbb\xbf"):
        raw = raw[3:]
    records, start, quoted, field_start, position = [], 0, False, True, 0
    while position < len(raw):
        byte = raw[position]
        if quoted:
            if byte == 34:
                if position + 1 < len(raw) and raw[position + 1] == 34:
                    position += 1
                else:
                    quoted = False
        elif byte == 34 and field_start:
            quoted = True
        elif byte == 10:
            records.append(raw[start:position + 1])
            start, field_start = position + 1, True
        elif byte in (44, 13):
            field_start = True
        else:
            field_start = False
        position += 1
    output, count = bytearray(COMMENT_HEADER), 0
    for record in records[1:]:
        try:
            rows = list(csv.reader(io.StringIO(record.decode("utf-8")), strict=True))
            if len(rows) != 1:
                continue
            row = rows[0]
            if len(row) < 12 or row[1] not in ("add", "edit", "delete"):
                continue
            int(row[2]); int(row[4]); int(row[5])
            if row[0].startswith("@event:"):
                token = row[0][7:].split(":", 1)[0]
                if len(row) < 13 or not row[12].startswith("operation=") or row[12][10:].encode("utf-8").hex() != token:
                    continue
            output.extend(record)
            count += 1
        except (ValueError, UnicodeError, csv.Error):
            continue
    return bytes(output), count


def next_comment_id(source, tail):
    # A remote TAK permits staged Comments before conversion. Reserve IDs beyond the
    # legacy CSV rows that conversion will import, without displaying those rows yet.
    if source.format == "tak":
        sidecar = os.path.splitext(source.path)[0] + ".comments.csv"
        if os.path.isfile(sidecar):
            with open(sidecar, "rb") as file:
                tail, _ = complete_csv(file.read(INDEX_LIMIT + 1))
    highest = 0
    if tail:
        try:
            for row in csv.reader(io.StringIO(tail.decode("utf-8", errors="replace"))):
                if len(row) >= 12 and row[1] in ("add", "edit", "delete") and row[-1] != "__takm_truncated_v1__":
                    try:
                        highest = max(highest, int(row[2]))
                    except ValueError:
                        pass
        except csv.Error:
            pass
    if highest >= (1 << 64) - 1:
        raise Failure("invalid_file", "Comment number space is exhausted")
    return highest + 1


def event_tail(raw):
    if not raw:
        return b""
    if len(raw) > INDEX_LIMIT or not raw.endswith(b"\n"):
        raise Failure("invalid_request", "Comment events are incomplete or too large")
    try:
        rows = list(csv.reader(io.StringIO(raw.decode("utf-8")), strict=True))
    except (ValueError, UnicodeError, csv.Error):
        raise Failure("invalid_request", "Invalid Comment CSV")
    if any(len(row) < 12 or row[1] not in ("add", "edit", "delete") for row in rows):
        raise Failure("invalid_request", "Invalid Comment event")
    return raw


def safe_append_tail(tail, events):
    if not tail:
        tail = COMMENT_HEADER
    if events and not tail.endswith(b"\n"):
        tail += b'"' if tail.count(b'"') % 2 else b''
        tail += b',,,,,,,,,,,,__takm_truncated_v1__\r\n'
    # The same operation can be resubmitted after a refreshed open without duplicating it.
    try:
        rows = list(csv.reader(io.StringIO(tail.decode("utf-8", errors="replace"))))
        incoming = list(csv.reader(io.StringIO(events.decode("utf-8")), strict=True))
    except csv.Error:
        raise Failure("invalid_file", "Cannot parse Comment tail")
    completed = {row[12][10:]: row for row in rows if len(row) >= 13 and row[12].startswith("operation=")}
    live = {}
    for row in rows:
        if len(row) < 12 or row[-1] == "__takm_truncated_v1__":
            continue
        if row[1] == "delete":
            live.pop(row[2], None)
        elif row[1] in ("add", "edit"):
            live[row[2]] = row
    output = io.StringIO(newline="")
    writer = csv.writer(output, lineterminator="\r\n")
    for row in incoming:
        operation = row[12][10:] if len(row) >= 13 and row[12].startswith("operation=") else None
        if row[0].startswith("@event:"):
            token = row[0][7:].split(":", 1)[0]
            if not operation or operation.encode("utf-8").hex() != token:
                raise Failure("invalid_request", "Comment operation token differs")
        if operation in completed:
            if row != completed[operation]:
                raise Failure("comment_conflict", "Comment operation already has different content")
            continue
        if row[1] == "add" and row[2] in live:
            raise Failure("comment_conflict", "Comment number is already in use")
        if row[1] in ("edit", "delete") and row[2] not in live:
            raise Failure("comment_conflict", "Comment no longer exists")
        if row[1] == "delete":
            live.pop(row[2], None)
        else:
            live[row[2]] = row
        writer.writerow(row)
        if operation:
            completed[operation] = row
    return tail + output.getvalue().encode("utf-8")


def patch_channels(source, replacement):
    """Replace only client marker records; copy all device records on the server."""
    source.classify(source.reader.size + (64 << 10))
    replacement_reader = Reader(io.BytesIO(replacement), len(replacement))
    replacement_stream = Stream(replacement_reader, [(0, len(replacement))], len(replacement))
    records = inventory(replacement_stream, 2)
    if any(r["analog"] for r in records):
        raise Failure("invalid_request", "Marker patch contains analog records")
    available = {(r["uid"], r["name"]): r for r in records}
    if len(available) != len(records):
        raise Failure("invalid_request", "Duplicate patch channel")
    output = [CHANNELS_HEADER, bytes(4)]
    count = 0
    original = source.streams["Channels.dat"]
    for row in source.records:
        if row["analog"]:
            output.append((original, row["start"], row["end"] - row["start"]))
        else:
            key = (row["uid"], row["name"])
            patch = available.pop(key, None)
            if patch is None:
                # A removed generated marker has no replacement; non-marker channels remain.
                if row["name"] == "Translation":
                    continue
                raise Failure("invalid_request", "Patch removed an opaque channel")
            raw = replacement[patch["start"]:patch["end"]]
            if row["name"] != "Translation" and raw != original.read(row["start"], row["end"] - row["start"]):
                raise Failure("invalid_request", "Patch changed an opaque channel")
            output.append(raw)
        count += 1
    for row in records:
        if (row["uid"], row["name"]) in available:
            if row["name"] != "Translation":
                raise Failure("invalid_request", "Patch added an opaque channel")
            output.append(replacement[row["start"]:row["end"]])
            count += 1
    output[1] = struct.pack("<I", count)
    return output


def stream_slice_chunks(stream, start, length):
    # Generator keeps opaque analog pass-through bounded during the physical save copy.
    for offset in range(start, start + length, COPY_CHUNK):
        yield stream.read(offset, min(COPY_CHUNK, start + length - offset))


def write_container(source, file, nodes, channels, events):
    source_seal(source)
    imported = 0
    if source.format == "takm":
        table = dict(source.table)
        tail = source.reader.read(source.tail, source.reader.size - source.tail)
    else:
        table = dict(format="takm", layout=1, made_by="tak_viewer SSH", made_at=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                     source=dict(name=os.path.basename(source.path), size=source.reader.size, sha256=hash_range(source.reader, 0, source.reader.size).hex()), dropped=[])
        tail = COMMENT_HEADER
        sidecar = os.path.splitext(source.path)[0] + ".comments.csv"
        if os.path.isfile(sidecar):
            with open(sidecar, "rb") as previous:
                tail, imported = complete_csv(previous.read(INDEX_LIMIT + 1))
    tail = safe_append_tail(tail, events)
    # Nodes and packed marker channels are the only client payloads. Large untouched
    # analog records are represented by descriptors and streamed directly from source.
    channel_parts = patch_channels(source, channels) if channels is not None else None
    parts = []
    rows = []
    dropped = list(table.get("dropped", []))
    offset = 64
    for ordinal, (name, stream) in enumerate(source.all_streams):
        index = source.table["streams"][ordinal]["index"] if source.format == "takm" else ordinal
        if source.format == "tak" and name == CAMERA_STREAM:
            digest = hashlib.sha256()
            for block in stream_chunks(stream):
                digest.update(block)
            dropped.append(dict(name=name, index=index, size=stream.length, sha256=digest.hexdigest()))
            continue
        if name == "Nodes.dat" and nodes is not None:
            content = [nodes]
        elif name == "Channels.dat" and channel_parts is not None:
            content = channel_parts
        else:
            content = stream
        size = sum(len(p) if isinstance(p, bytes) else p[2] for p in content) if isinstance(content, list) else stream.length
        rows.append(dict(name=name, index=index, offset=offset, size=size))
        parts.append(content)
        offset += size
    table["streams"], table["dropped"] = rows, dropped
    table_bytes = json.dumps(table, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    fixed = offset + len(table_bytes)
    header = b"TAKM-2\r\n" + struct.pack("<IIQQQ", 1, 0, offset, len(table_bytes), fixed) + bytes(24)
    digest = hashlib.sha256()
    def put(block):
        file.write(block)
        digest.update(block)
    put(header)
    hashes = []
    for content in parts:
        stream_hash = hashlib.sha256()
        if isinstance(content, list):
            blocks = (block for part in content for block in ([part] if isinstance(part, bytes) else stream_slice_chunks(*part)))
        else:
            blocks = stream_chunks(content)
        for block in blocks:
            put(block)
            stream_hash.update(block)
        hashes.append(stream_hash.digest())
    put(table_bytes)
    file.write(digest.digest())
    file.write(tail)
    file.flush()
    os.fsync(file.fileno())
    reader = Reader(file, file.tell())
    if hash_range(reader, 0, fixed) != reader.read(fixed, 32):
        raise Failure("verification_failed", "Written TAKM seal does not match")
    for row, expected in zip(rows, hashes):
        if hash_range(reader, row["offset"], row["size"]) != expected:
            raise Failure("verification_failed", "Copied stream SHA-256 does not match")
    if reader.read(fixed + 32, len(tail)) != tail:
        raise Failure("verification_failed", "Written Comment tail differs")
    return fixed, imported, [dict(name=row["name"], index=row["index"], size=row["size"], sha256=value.hex()) for row, value in zip(rows, hashes)]


def open_write_lock(path):
    """Persistent empty lock inode; OS releases ownership if SSH/helper is killed."""
    if os.path.islink(path):
        raise Failure("invalid_request", "Write lock cannot be a symbolic link")
    flags = os.O_CREAT | os.O_RDWR | getattr(os, "O_NOFOLLOW", 0)
    descriptor = os.open(path, flags, 0o600)
    try:
        opened, named = os.fstat(descriptor), os.stat(path)
        if (not stat.S_ISREG(opened.st_mode) or opened.st_nlink != 1 or opened.st_size != 0
                or (opened.st_dev, opened.st_ino) != (named.st_dev, named.st_ino)):
            raise Failure("invalid_request", "Write lock is not an empty dedicated lock file")
        if os.name == "nt":
            import msvcrt
            msvcrt.locking(descriptor, msvcrt.LK_NBLCK, 1)
        else:
            import fcntl
            fcntl.flock(descriptor, fcntl.LOCK_EX | fcntl.LOCK_NB)
        return descriptor
    except BaseException:
        os.close(descriptor)
        raise


def write_dispatch(server, request):
    op = request["op"]
    path = path_arg(request)
    if op == "stat":
        with open(path, "rb") as file:
            version = identity(file, path)
        return dict(ok=True, lengths=[], identity=version, file_size=version[2]), []
    expected = expected_identity(request)
    maximum = integer(request, "max_bytes", 1, 2 << 30)
    if op == "comment_ids":
        source = Source(path, 2)
        try:
            source.check(expected)
            tail = source.reader.read(source.tail, source.reader.size - source.tail) if source.format == "takm" else b""
            next_id = next_comment_id(source, tail)
            source.check(expected)
            return dict(ok=True, lengths=[], identity=source.identity, next_comment_id=next_id), []
        finally:
            source.file.close()
    if op == "takm_progress" and not path.lower().endswith(".takm"):
        raise Failure("invalid_request", "Progress needs an existing TAKM")
    destination = write_paths(request, path)
    if op == "patch_begin":
        lengths = request.get("part_lengths")
        if not isinstance(lengths, list) or len(lengths) != 3 or any(type(n) is not int or n < 0 for n in lengths) or sum(lengths) > maximum:
            raise Failure("invalid_request", "Invalid patch lengths")
        with open(path, "rb") as file:
            if identity(file, path) != expected:
                raise Failure("source_changed", "Source changed since open")
        server.patch = dict(path=path, identity=expected, lengths=lengths, parts=[bytearray(), bytearray(), bytearray()])
        return dict(ok=True, lengths=[]), []
    if op == "patch_chunk":
        patch = server.patch
        part = integer(request, "part", 0, 2)
        offset = integer(request, "offset", 0, maximum)
        raw = request.get("hex")
        if not patch or patch["path"] != path or patch["identity"] != expected or not isinstance(raw, str) or len(raw) > 524288:
            raise Failure("invalid_request", "No matching bounded patch")
        block = bytes.fromhex(raw)
        if len(patch["parts"][part]) != offset or offset + len(block) > patch["lengths"][part]:
            raise Failure("invalid_request", "Patch chunk exceeds declared range")
        patch["parts"][part].extend(block)
        return dict(ok=True, lengths=[]), []
    nodes = channels = None
    events = b""
    if op in ("takm_save_patch", "takm_append"):
        patch, server.patch = server.patch, None
        if not patch or patch["path"] != path or patch["identity"] != expected or any(len(p) != n for p, n in zip(patch["parts"], patch["lengths"])):
            raise Failure("invalid_request", "Missing or incomplete patch")
        nodes, channels, events = map(bytes, patch["parts"])
        nodes, channels = nodes or None, channels or None
        events = event_tail(events)
        if bool(nodes) != bool(channels) or (op == "takm_append" and (nodes or channels or not path.lower().endswith(".takm"))):
            raise Failure("invalid_request", "Invalid marker patch or append target")
    lock_path = destination + ".tak-viewer-lock"
    lock_fd = open_write_lock(lock_path)
    temporary = None
    try:
        with open(path, "rb") as file:
            if identity(file, path) != expected:
                raise Failure("source_changed", "Source changed since open")
        if destination != path and os.path.lexists(destination):
            with open(destination, "rb") as file:
                version = identity(file, destination)
                source = Source(destination, 2)
                try:
                    if source.format != "takm":
                        raise Failure("invalid_file", "Existing sibling is not TAKM")
                    source_seal(source)
                    fixed = source.tail - 32
                finally:
                    source.file.close()
            return dict(ok=True, lengths=[], path=destination, identity=version, source_bytes=expected[2], output_bytes=version[2], fixed_len=fixed, existing=True, verified=True), []
        source = Source(path, 2, progress_only=op == "takm_progress")
        try:
            source.check(expected)
            if op == "takm_append":
                source_seal(source)
                tail = source.reader.read(source.tail, source.reader.size - source.tail)
                combined = safe_append_tail(tail, events)
                source.check(expected)
                with open(path, "r+b") as file:
                    if identity(file, path) != expected:
                        raise Failure("source_changed", "Source changed before Comment append")
                    file.seek(0, os.SEEK_END)
                    file.write(combined[len(tail):])
                    file.flush()
                    os.fsync(file.fileno())
                    file.seek(source.tail)
                    if file.read() != combined:
                        raise Failure("verification_failed", "Appended Comment tail differs")
                fixed, imported, hashes = source.tail - 32, 0, []
            else:
                fd, temporary = tempfile.mkstemp(prefix="." + os.path.basename(destination) + ".", suffix=".part", dir=os.path.dirname(destination))
                with os.fdopen(fd, "w+b") as output:
                    if op == "takm_progress":
                        mode = stat.S_IMODE(os.fstat(source.file.fileno()).st_mode)
                        if not mode & 0o222:
                            raise Failure("io_error", "Progress target is read-only")
                        fixed = write_progress(source, output, request["before"], request["after"])
                        os.chmod(temporary, mode)
                        imported, hashes = 0, []
                    else:
                        fixed, imported, hashes = write_container(source, output, nodes, channels, events)
                source.check(expected)
                source.file.close()
                server.close()  # Windows replacement needs the cached reader released too.
                if destination == path:
                    os.replace(temporary, destination)
                    temporary = None
                else:
                    # A hard-link publication is atomic and fails if a sibling appeared.
                    os.link(temporary, destination)
                    os.unlink(temporary)
                    temporary = None
                if os.name != "nt":
                    directory = os.open(os.path.dirname(destination), os.O_RDONLY)
                    try:
                        os.fsync(directory)
                    finally:
                        os.close(directory)
        finally:
            source.file.close()
        server.close()
        with open(destination, "rb") as file:
            version = identity(file, destination)
        return dict(ok=True, lengths=[], path=destination, identity=version, source_bytes=expected[2], output_bytes=version[2], fixed_len=fixed, existing=False, verified=True, imported_comments=imported, streams=hashes), []
    finally:
        if temporary is not None:
            os.unlink(temporary)
        os.close(lock_fd)

def main():
    server = Server()
    input_stream, output = sys.stdin.buffer, sys.stdout.buffer
    clock = None

    def send_frame(header, blocks):
        nonlocal clock
        if clock is not None and header.get("ok"):
            header = dict(header, server_wall_ms=(time.perf_counter() - clock[0]) * 1000,
                          server_cpu_ms=(time.process_time() - clock[1]) * 1000)
        output.write(encoded(header))
        for block in blocks:
            output.write(block)
        output.flush()
        if clock is not None:
            clock = time.perf_counter(), time.process_time()

    try:
        while True:
            clock = None
            line = input_stream.readline(REQUEST_LIMIT + 1)
            if not line:
                break
            try:
                if len(line) > REQUEST_LIMIT or not line.endswith(b"\n"):
                    while line and not line.endswith(b"\n"):
                        line = input_stream.readline(REQUEST_LIMIT + 1)
                    raise Failure("invalid_request", "Request line too long or missing newline")
                try:
                    request = json.loads(line)
                except (ValueError, UnicodeError):
                    raise Failure("invalid_request", "Invalid JSON request")
                clock = (time.perf_counter(), time.process_time()) if isinstance(request, dict) and request.get("telemetry") is True else None
                header, blocks = server.dispatch(request, send_frame)
            except Failure as error:
                header, blocks = dict(ok=False, error=error.code, message=str(error)), []
            except OSError as error:
                header, blocks = dict(ok=False, error="io_error", message=str(error)), []
            except (ValueError, KeyError, IndexError, TypeError, struct.error, OverflowError, RecursionError, ET.ParseError, expat.ExpatError) as error:
                header, blocks = dict(ok=False, error="invalid_file", message=str(error)), []
            send_frame(header, blocks)
    finally:
        server.close()


if __name__ == "__main__":
    main()
