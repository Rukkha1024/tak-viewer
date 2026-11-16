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
                            stride=stride, analog=name.startswith("AnalogChannel"),
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
                head = stream.read(record["start"], record["header"])
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
        if op == "hello":
            return dict(ok=True, lengths=[], protocol="tak-ssh-1", python_major=sys.version_info.major), []
        if op == "close":
            self.close()
            return dict(ok=True, lengths=[]), []
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
            blocks = [source.nodes, source.streams["MetaData.dat"].read(0, lengths[1])]
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
