"""Serve bounded, read-only TAK stream requests over stdin/stdout for SSH clients.
Discover compound or TAKM-2 streams and send markers before analog samples.
Keep source identities stable across staged reads without storing remote files.
"""

import bisect
import json
import os
import stat
import struct
import sys

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
    def __init__(self, reader):
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
    if count > 1000000 or count > (stream.length - 10) // 25:
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
    def __init__(self, path, probe):
        self.path = path
        self.file = open(path, "rb")
        try:
            self.identity = identity(self.file, path)
            self.reader = Reader(self.file, self.identity[2])
            magic = self.reader.read(0, min(8, self.reader.size))
            if magic == b"TAKM-2\r\n":
                self.format = "takm"
                self.streams, self.tail = takm_index(self.reader)
            elif magic == bytes.fromhex("d0cf11e0a1b11ae1"):
                self.format = "tak"
                self.streams, self.tail = Compound(self.reader).streams, self.reader.size
            else:
                raise Failure("unsupported_format", "Unsupported TAK/TAKM container; no full-file fallback")
            self.records = inventory(self.streams["Channels.dat"], probe)
            self.check()
        except Exception:
            self.file.close()
            raise

    def check(self, expected=None):
        if identity(self.file, self.path) != (self.identity if expected is None else expected):
            raise Failure("source_changed", "Source changed since the initial read")

    def channels(self, analog):
        stream = self.streams["Channels.dat"]
        selected = [record for record in self.records if record["analog"] == analog]
        return (CHANNELS_HEADER + struct.pack("<I", len(selected))
                + stream.many([(record["start"], record["end"] - record["start"])
                               for record in selected]))

    def channel_length(self, analog):
        return 10 + sum(record["end"] - record["start"] for record in self.records
                        if record["analog"] == analog)


def integer(request, key, minimum, maximum):
    value = request.get(key)
    if type(value) is not int or not minimum <= value <= maximum:
        raise Failure("invalid_request", "Invalid " + key)
    return value


def path_arg(request):
    path = request.get("path")
    if not isinstance(path, str) or not path or "\0" in path or len(path) > 32768:
        raise Failure("invalid_request", "Invalid path")
    return os.path.abspath(os.path.expanduser(path))


def budget(lengths, maximum):
    if sum(lengths) > maximum:
        raise Failure("payload_too_large", "Selected payload exceeds max_bytes")


def encoded(header):
    return json.dumps(header, ensure_ascii=True, separators=(",", ":")).encode("ascii") + b"\n"


class Server:
    def __init__(self):
        self.source = None

    def close(self):
        if self.source:
            self.source.file.close()
            self.source = None

    def dispatch(self, request):
        if not isinstance(request, dict):
            raise Failure("invalid_request", "Request must be an object")
        op = request.get("op")
        if op == "close":
            self.close()
            return dict(ok=True, lengths=[]), []
        if op not in ("list", "open", "analog"):
            raise Failure("invalid_request", "Unknown operation")
        path = path_arg(request)
        maximum = integer(request, "max_bytes", 1, (1 << 63) - 1)
        if op == "list":
            limit = integer(request, "limit", 1, 10000)
            entries, truncated = [], False
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
        if op == "open":
            lengths = [source.streams["Nodes.dat"].length, source.streams["MetaData.dat"].length,
                       source.channel_length(False), source.reader.size - source.tail]
            budget(lengths, maximum)
            blocks = [source.streams[name].read(0, source.streams[name].length)
                      for name in ("Nodes.dat", "MetaData.dat")]
            blocks.extend([source.channels(False), source.reader.read(source.tail, lengths[3])])
            header = dict(ok=True, lengths=lengths, identity=source.identity, file_size=source.reader.size,
                          has_analog=any(record["analog"] for record in source.records), format=source.format,
                          channel_order=[record["analog"] for record in source.records])
        else:
            lengths = [source.channel_length(True)]
            budget(lengths, maximum)
            blocks = [source.channels(True)]
            header = dict(ok=True, lengths=lengths, identity=source.identity)
        source.check(expected if op == "analog" else None)
        if len(encoded(header)) > REQUEST_LIMIT:
            raise Failure("payload_too_large", "Response header exceeds transport limit")
        return header, blocks


def main():
    server = Server()
    input_stream, output = sys.stdin.buffer, sys.stdout.buffer
    try:
        while True:
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
                header, blocks = server.dispatch(request)
            except Failure as error:
                header, blocks = dict(ok=False, error=error.code, message=str(error)), []
            except OSError as error:
                header, blocks = dict(ok=False, error="io_error", message=str(error)), []
            except (ValueError, KeyError, IndexError, TypeError, struct.error, OverflowError, RecursionError) as error:
                header, blocks = dict(ok=False, error="invalid_file", message=str(error)), []
            output.write(encoded(header))
            for block in blocks:
                output.write(block)
            output.flush()
    finally:
        server.close()


if __name__ == "__main__":
    main()
