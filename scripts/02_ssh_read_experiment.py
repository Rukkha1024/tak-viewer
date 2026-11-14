"""Measure whole-file, selected-stream and memory-only reads over one SSH session.
The remote helper only reads; source data stays in memory on the Mac.
Compare selected TAK streams against a full-file reference and remote MD5.
This is a transport experiment, not an integrated viewer or SFTP implementation.
Configuration uses the JSON subset of YAML; no extra packages are required.
"""

import argparse
import hashlib
import json
from pathlib import Path
import shlex
import struct
import subprocess
import threading
import time


WINDOWS_BOOTSTRAP = (
    "import json,os,sys; "
    "sys.stdout.reconfigure(newline='\\n'); "
    "msvcrt=__import__('msvcrt') if os.name=='nt' else None; "
    "msvcrt.setmode(sys.stdin.fileno(),os.O_BINARY) if msvcrt else None; "
    "msvcrt.setmode(sys.stdout.fileno(),os.O_BINARY) if msvcrt else None; "
    "payload=json.loads(sys.stdin.buffer.readline()); "
    "sys.argv=['ssh_read',payload['path']]; exec(payload['helper_source'])"
)


REMOTE = r'''
import hashlib, json, os, sys
with open(sys.argv[1], "rb", buffering=0) as source:
    def signature():
        s = os.fstat(source.fileno())
        p = os.stat(sys.argv[1])
        return [s.st_dev, s.st_ino, s.st_size, s.st_mtime_ns,
                p.st_dev, p.st_ino, p.st_size, p.st_mtime_ns]
    initial = signature()
    print(json.dumps({"size": initial[2], "signature": initial}), flush=True)
    for line in sys.stdin.buffer:
        request = json.loads(line)
        if signature() != initial:
            raise RuntimeError("Source changed during experiment")
        if request == "md5":
            source.seek(0)
            digest = hashlib.md5()
            while True:
                block = source.read(1 << 20)
                if not block:
                    break
                digest.update(block)
            if signature() != initial:
                raise RuntimeError("Source changed during checksum")
            print(digest.hexdigest(), flush=True)
        else:
            for offset, length in request:
                if offset < 0 or length < 0 or offset + length > initial[2]:
                    raise ValueError("Invalid range")
                source.seek(offset)
                remaining = length
                while remaining:
                    data = source.read(min(remaining, 1 << 20))
                    if not data:
                        raise EOFError("Truncated source")
                    sys.stdout.buffer.write(data)
                    remaining -= len(data)
            sys.stdout.buffer.flush()
            if signature() != initial:
                raise RuntimeError("Source changed during read")
            sys.stdout.buffer.write(b"OK\n")
            sys.stdout.buffer.flush()
'''


class Session:
    def __init__(self, host, path, config, helper_source=REMOTE):
        platform = config.get("remote_platform", "linux")
        if platform not in ("linux", "windows"):
            raise ValueError("remote_platform must be linux or windows")
        executable = config.get("remote_python", "python" if platform == "windows" else "python3")
        if not isinstance(executable, str) or not executable or any(c in executable for c in "\r\n\0"):
            raise ValueError("Invalid remote_python executable")
        if platform == "windows":
            if any(c in executable for c in '&|<>^%!"()'):
                raise ValueError("Unsafe Windows remote_python executable")
            command = subprocess.list2cmdline([executable, "-u", "-c", WINDOWS_BOOTSTRAP])
        else:
            command = shlex.quote(executable) + " -u -c " + shlex.quote(helper_source) + " " + shlex.quote(path)
        self.process = subprocess.Popen(
            ["ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
             "-o", "Compression=no", "-o",
             "ConnectTimeout=" + str(config["connect_timeout_seconds"]), "--", host, command],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
        )
        self.timer = threading.Timer(config["session_timeout_seconds"], self.process.kill)
        self.timer.daemon = True
        self.timer.start()
        self.bytes = self.requests = self.ranges = 0
        if platform == "windows":
            self.send(dict(helper_source=helper_source, path=path))

    def send(self, request):
        self.process.stdin.write(json.dumps(request).encode() + b"\n")
        self.process.stdin.flush()

    def read(self, length):
        data = bytearray()
        while len(data) < length:
            block = self.process.stdout.read(length - len(data))
            if not block:
                raise EOFError("SSH closed or experiment timeout expired")
            data.extend(block)
        return bytes(data)

    def fetch(self, ranges):
        self.send(ranges)
        result = [self.read(n) for _, n in ranges]
        if self.process.stdout.readline() != b"OK\n":
            raise RuntimeError("Remote range validation failed")
        self.bytes += sum(n for _, n in ranges)
        self.requests += 1
        self.ranges += len(ranges)
        return result

    def close(self):
        self.timer.cancel()
        self.process.stdin.close()
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.process.stdout.close()


class Reader:
    def __init__(self, session, size, merge_gap):
        self.session, self.size, self.gap = session, size, merge_gap
        self.cache = {}

    def many(self, ranges):
        missing = sorted(set(ranges) - self.cache.keys())
        merged = []
        for offset, length in missing:
            if offset < 0 or length < 0 or offset + length > self.size:
                raise ValueError("TAK range outside file")
            end = offset + length
            if merged and offset <= merged[-1][1] + self.gap:
                merged[-1][1] = max(merged[-1][1], end)
            else:
                merged.append([offset, end])
        if merged:
            chunks = self.session.fetch([(a, b - a) for a, b in merged])
            index = 0
            for offset, length in missing:
                while merged[index][1] < offset + length:
                    index += 1
                start = offset - merged[index][0]
                self.cache[offset, length] = chunks[index][start:start + length]
        return b"".join(self.cache[r] for r in ranges)

    def read(self, offset, length):
        return self.many([(offset, length)])


class MemoryReader:
    def __init__(self, data):
        self.data = data
        self.size = len(data)

    def many(self, ranges):
        return b"".join(self.read(offset, length) for offset, length in ranges)

    def read(self, offset, length):
        if offset < 0 or length < 0 or offset + length > self.size:
            raise ValueError("Memory range outside file")
        return self.data[offset:offset + length]


def selected_streams(reader):
    head = reader.read(0, 512)
    if head[:8] != bytes.fromhex("d0cf11e0a1b11ae1"):
        raise ValueError("Experiment supports compound TAK files only")
    u32 = lambda data, pos: struct.unpack_from("<I", data, pos)[0]
    sector = 1 << struct.unpack_from("<H", head, 30)[0]
    mini_shift = struct.unpack_from("<H", head, 32)[0]
    if mini_shift > struct.unpack_from("<H", head, 30)[0]:
        raise ValueError("Unsupported mini sector size")
    mini_sector = 1 << mini_shift
    count = u32(head, 44)
    if sector not in (512, 4096, 32768) or count > 109 or u32(head, 72):
        raise ValueError("Unsupported sector size or extended DIFAT")
    fat_raw = reader.many([((u32(head, 76 + i * 4) + 1) * sector, sector)
                           for i in range(count)])
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
        if length is not None:
            if len(ids) != (length + sector - 1) // sector:
                raise ValueError("Stream size and chain disagree")
        ranges = [((i + 1) * sector, min(sector, reader.size - (i + 1) * sector)) for i in ids]
        data = reader.many(ranges)
        if length is not None and len(data) < length:
            raise ValueError("Truncated stream")
        return data if length is None else data[:length]

    directory = large(u32(head, 48))
    entries = []
    for pos in range(0, len(directory), 128):
        entry = directory[pos:pos + 128]
        name_len = struct.unpack_from("<H", entry, 64)[0]
        if entry[66] and not 2 <= name_len <= 64:
            raise ValueError("Invalid directory name")
        name = entry[:max(0, name_len - 2)].decode("utf-16le")
        entries.append((name, entry[66], u32(entry, 116),
                        struct.unpack_from("<Q", entry, 120)[0],
                        u32(entry, 68), u32(entry, 72), u32(entry, 76)))
    mini_raw = large(u32(head, 60), u32(head, 64) * sector)
    mini_fat = struct.unpack("<" + "I" * (len(mini_raw) // 4), mini_raw)
    roots = [entry for entry in entries if entry[1] == 5]
    if len(roots) != 1:
        raise ValueError("Expected one root")
    mini = large(roots[0][2], roots[0][3])
    top_level = []
    pending = [roots[0][6]]
    visited = set()
    while pending:
        index = pending.pop()
        if index == 0xFFFFFFFF:
            continue
        if index in visited or index >= len(entries):
            raise ValueError("Invalid directory tree")
        visited.add(index)
        entry = entries[index]
        top_level.append(entry)
        pending.extend(entry[4:6])
    result = {}
    for name in ("Nodes.dat", "MetaData.dat", "Channels.dat"):
        matches = [entry for entry in top_level if entry[0] == name and entry[1] == 2]
        if len(matches) != 1:
            raise ValueError("Expected one stream: " + name)
        _, _, first, length = matches[0][:4]
        if length < u32(head, 56):
            result[name] = b"".join(mini[i * mini_sector:(i + 1) * mini_sector]
                                    for i in chain(first, mini_fat))[:length]
        else:
            result[name] = large(first, length)
        if len(result[name]) != length:
            raise ValueError("Truncated stream: " + name)
    return result


def run(args, config):
    started = time.perf_counter()
    session = Session(args.host, args.file, config)
    try:
        metadata = json.loads(session.process.stdout.readline())
        setup_ms = (time.perf_counter() - started) * 1000
        size = metadata["size"]
        if not 512 <= size <= config["maximum_file_bytes"]:
            raise ValueError("File size outside experiment limit")
        rows = []
        reference = None
        for round_number in range(config["rounds"]):
            order = ["full", "selected"] if round_number % 2 == 0 else ["selected", "full"]
            reader = Reader(session, size, config["maximum_merge_gap_bytes"])
            for mode in order:
                before = session.bytes, session.requests, session.ranges
                started = time.perf_counter()
                if mode == "full":
                    full = session.fetch([(0, size)])[0]
                    full_streams = selected_streams(MemoryReader(full))
                else:
                    streams = selected_streams(reader)
                elapsed = (time.perf_counter() - started) * 1000
                rows.append(dict(round=round_number + 1, mode=mode, ms=elapsed,
                                 bytes=session.bytes - before[0], requests=session.requests - before[1],
                                 ranges=session.ranges - before[2]))
                if mode == "full":
                    digest = hashlib.md5(full).hexdigest()
                    if reference is not None and hashlib.md5(reference).hexdigest() != digest:
                        raise ValueError("Reference changed")
                    reference = full
            if streams != full_streams:
                raise ValueError("Selected streams differ from full-file extraction")
            for (offset, length), block in reader.cache.items():
                if hashlib.md5(block).digest() != hashlib.md5(reference[offset:offset + length]).digest():
                    raise ValueError("Range MD5 differs from reference")
            before = session.bytes, session.requests
            started = time.perf_counter()
            cached = selected_streams(reader)
            elapsed = (time.perf_counter() - started) * 1000
            if cached != streams or (session.bytes, session.requests) != before:
                raise ValueError("Cache reused remote reads or changed data")
            rows.append(dict(round=round_number + 1, mode="memory", ms=elapsed,
                             bytes=0, requests=0, ranges=0))
        session.send("md5")
        remote_md5 = session.process.stdout.readline().decode().strip()
        if remote_md5 != hashlib.md5(reference).hexdigest():
            raise ValueError("Full file MD5 differs from remote reference")
        return dict(file_bytes=size, connection_setup_ms=setup_ms, config=config,
                    measurements=rows, stream_bytes={k: len(v) for k, v in streams.items()},
                    cache_payload_bytes=sum(map(len, reader.cache.values())),
                    stream_md5={k: hashlib.md5(v).hexdigest() for k, v in streams.items()},
                    range_md5_verified=True, full_md5_verified=True,
                    reference_md5=remote_md5,
                    limitations=["Transfer + container extraction, not viewer render time",
                                 "Selected Channels.dat includes analog data",
                                 "Repeated reads use warm server filesystem caches",
                                 "SSH Python helper, not production SFTP",
                                 "Cache lifetime is one unchanged open remote file",
                                 "Cache payload is not peak process memory",
                                 "Memory timings reparse and copy cached ranges"])
    finally:
        session.close()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--host", required=True)
    parser.add_argument("--file", required=True)
    parser.add_argument("--config", type=Path,
                        default=Path(__file__).resolve().parents[1] / "configs/ssh_read_experiment.yaml")
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    configuration = json.loads(arguments.config.read_text(encoding="utf-8-sig"))
    report = run(arguments, configuration)
    arguments.output.parent.mkdir(parents=True, exist_ok=True)
    arguments.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8-sig")
    print(json.dumps(report, indent=2))
