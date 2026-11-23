"""Index and relay bounded USTAR TAKM v2 members without decoding motion."""

import hashlib
import json
import re

V2_CATALOG_LIMIT = 16 << 10
V2_LOGICAL_LIMIT = 1 << 30
V2_PARTS = {
    "manifest": ("manifest.bin.zst", "zstd"),
    "presence": ("presence.bin.zst", "zstd"),
    "first": ("first.pco.parts", "pco"),
    "preview": ("preview.pco.parts", "pco"),
    "exact": ("exact.pco.parts", "pco"),
    "plates": ("plates.xor.zst", "xor-zstd"),
    "analog": ("analog.pco.parts", "pco"),
    "recovery": ("recovery.bin.zst", "zstd"),
}
V2_JOURNAL_NAME = re.compile(rb"comments/([0-9]{16})-([0-9a-f]{64})\.csv\Z")
V2_BLOCK = 512


def has_v2(reader):
    return reader.size >= V2_BLOCK and reader.read(0, 10) == b"takm.json\0"


def v2_header(raw):
    if (len(raw) != V2_BLOCK or raw[257:265] != b"ustar\x0000"
            or raw[156] not in (0, 48) or any(raw[345:500])):
        raise ValueError("Invalid TAKM v2 USTAR header")
    fields = [raw[a:b].strip(b" \0") for a, b in ((124, 136), (148, 156))]
    if not all(re.fullmatch(rb"[0-7]+", field) for field in fields):
        raise ValueError("Invalid TAKM v2 USTAR size or checksum")
    size, checksum = [int(field, 8) for field in fields]
    if checksum != sum(raw[:148]) + 8 * 32 + sum(raw[156:]):
        raise ValueError("Invalid TAKM v2 USTAR checksum")
    name, _, rest = raw[:100].partition(b"\0")
    if not name or any(rest):
        raise ValueError("Invalid TAKM v2 USTAR name")
    return name, size


def v2_aligned(size):
    return (size + V2_BLOCK - 1) // V2_BLOCK * V2_BLOCK


class V2Archive:
    def __init__(self, reader):
        self.reader = reader
        name, size, data, cursor = self.member_at(0)
        if name != b"takm.json" or size > V2_CATALOG_LIMIT:
            raise ValueError("Invalid TAKM v2 catalog member")
        self.catalog = reader.read(data, size)
        info = json.loads(self.catalog)
        if type(info) is not dict or type(info.get("takm")) is not int or info["takm"] != 2:
            raise ValueError("Invalid TAKM v2 catalog")
        self.profile = info.get("profile", "hybrid")
        if self.profile not in ("hybrid", "opaque-v1"):
            raise ValueError("Unknown TAKM v2 profile")
        parts = info.get("parts")
        keys = ("recovery",) if self.profile == "opaque-v1" else tuple(V2_PARTS)
        if type(parts) is not dict or parts.keys() != set(keys):
            raise ValueError("Invalid TAKM v2 part list")
        self.parts, self.ranges = parts, {}
        for kind in keys:
            desc = parts[kind]
            if (type(desc) is not list or len(desc) != 4 or tuple(desc[:2]) != V2_PARTS[kind]
                    or type(desc[2]) is not int or not 0 <= desc[2] <= V2_LOGICAL_LIMIT
                    or (desc[1] == "pco" and desc[2] != 0)
                    or not isinstance(desc[3], str) or not re.fullmatch("[0-9a-f]{64}", desc[3])):
                raise ValueError("Invalid TAKM v2 part descriptor")
            name, size, data, cursor = self.member_at(cursor)
            if name != desc[0].encode("ascii") or cursor > V2_LOGICAL_LIMIT:
                raise ValueError("Unexpected TAKM v2 fixed member")
            self.ranges[kind] = data, size
        self.fixed_len = cursor
        name, size, self.seal_at, self.comments_at = self.member_at(cursor)
        if name != b"seal.sha256" or size != 32:
            raise ValueError("Invalid TAKM v2 seal member")
        offset = info.get("tar_comment_header")
        if offset is not None and (not isinstance(offset, str) or not offset.isascii()
                                    or not offset.isdecimal() or int(offset) != self.comments_at):
            raise ValueError("TAKM v2 catalog Comment offset differs")
        self.base_size = info.get("tar_comment_size")
        if self.base_size is not None and (type(self.base_size) is not int or self.base_size < 0):
            raise ValueError("Invalid TAKM v2 catalog Comment size")

    def member_at(self, offset):
        name, size = v2_header(self.reader.read(offset, V2_BLOCK))
        data = offset + V2_BLOCK
        end = data + v2_aligned(size)
        if end > self.reader.size:
            raise ValueError("Truncated TAKM v2 fixed member")
        return name, size, data, end

    def parts_bytes(self, kinds, maximum):
        budget([self.ranges[kind][1] for kind in kinds], maximum)
        return [self.reader.read(*self.ranges[kind]) for kind in kinds]

    def read_part(self, kind, maximum=None):
        offset, size = self.ranges[kind]
        if maximum is not None:
            budget([size], maximum)
        packed = self.reader.read(offset, size)
        if hashlib.sha256(packed).hexdigest() != self.parts[kind][3]:
            raise ValueError("TAKM v2 packed part digest differs")
        return packed

    def verify_fixed(self, *, verify_packed=False):
        cursor = 0
        while cursor <= self.fixed_len:
            _, size, data, cursor = self.member_at(cursor)
            if any(self.reader.read(data + size, cursor - data - size)):
                raise Failure("invalid_file", "TAKM v2 fixed padding is not zero")
        mismatch = None
        if verify_packed:
            outer = hashlib.sha256()
            cursor = 0
            for kind in sorted(self.ranges, key=self.ranges.get):
                offset, size = self.ranges[kind]
                for block in stream_slice_chunks(self.reader, cursor, offset - cursor):
                    outer.update(block)
                packed = hashlib.sha256()
                for block in stream_slice_chunks(self.reader, offset, size):
                    outer.update(block)
                    packed.update(block)
                if packed.hexdigest() != self.parts[kind][3] and mismatch is None:
                    mismatch = self.parts[kind][0]
                cursor = offset + size
            for block in stream_slice_chunks(self.reader, cursor, self.fixed_len - cursor):
                outer.update(block)
            digest = outer.digest()
        else:
            digest = hash_range(self.reader, 0, self.fixed_len)
        if digest != self.reader.read(self.seal_at, 32):
            raise Failure("invalid_file", "TAKM v2 seal does not match")
        if mismatch is not None:
            raise Failure("invalid_file", f"TAKM v2 packed SHA-256 mismatch ({mismatch})")

    def journal(self, maximum):
        """Return complete CSV bytes, current end header, and the next suffix ordinal."""
        size = self.reader.size - self.comments_at
        budget([size], min(INDEX_LIMIT, maximum))
        raw = self.reader.read(self.comments_at, size)
        name, base = v2_header(raw[:V2_BLOCK])
        offset = V2_BLOCK + v2_aligned(base)
        if (name != b"comments.csv" or offset > size or any(raw[V2_BLOCK + base:offset])
                or self.base_size not in (None, base)):
            raise ValueError("Invalid TAKM v2 Comment base")
        blocks = [raw[V2_BLOCK:V2_BLOCK + base]]
        ordinal = 1
        while offset < size:
            head = raw[offset:offset + V2_BLOCK]
            if not any(head) and not any(raw[offset:]):
                break
            try:
                number, payload, end = v2_event_at(raw, offset)
            except ValueError:
                # A committed later suffix establishes interior damage.
                for later in range(offset + V2_BLOCK, size - V2_BLOCK + 1, V2_BLOCK):
                    if not raw.startswith(b"comments/", later):
                        continue
                    try:
                        v2_event_at(raw, later)
                    except ValueError:
                        continue
                    raise ValueError("Damaged earlier TAKM v2 Comment member")
                break
            if number != ordinal:
                raise ValueError("Out-of-sequence TAKM v2 Comment member")
            blocks.append(payload)
            ordinal += 1
            offset = end
        return b"".join(blocks), self.comments_at + offset, ordinal


def v2_event_at(raw, offset):
    name, n = v2_header(raw[offset:offset + V2_BLOCK])
    match = V2_JOURNAL_NAME.fullmatch(name)
    start = offset + V2_BLOCK
    end = start + v2_aligned(n)
    if match is None or end > len(raw):
        raise ValueError("Invalid TAKM v2 Comment suffix range")
    payload = raw[start:start + n]
    if hashlib.sha256(payload).hexdigest().encode("ascii") != match[2]:
        raise ValueError("Invalid TAKM v2 Comment suffix digest")
    if any(raw[start + n:end]):
        raise ValueError("Invalid TAKM v2 Comment suffix padding")
    return int(match[1]), payload, end


def v2_packet(source, stage, blocks, maximum, expected=None):
    budget([len(block) for block in blocks], maximum)
    source.check(expected)
    return (dict(ok=True, lengths=[len(block) for block in blocks],
                 identity=source.identity, file_size=source.reader.size,
                 fixed_len=source.archive.fixed_len, archive_stage=stage), blocks)


def open_v2(source, op, request, maximum, send, expected):
    archive = source.archive
    maximum = min(maximum, source.reader.size)
    if request.get("archive_ranges") != 2:
        raise Failure("unsupported_format", "Client must support TAKM v2 archive ranges")
    if op == "analog":
        selection = request.get("selection")
        kinds = {"forceplates": ("plates",), "non_forceplates": ("analog",),
                 "all": ("plates", "analog")}.get(selection)
        if kinds is None or archive.profile != "hybrid":
            raise Failure("invalid_request", "Invalid TAKM v2 analog selection")
        return v2_packet(source, selection, archive.parts_bytes(kinds, maximum), maximum, expected)

    if archive.profile == "opaque-v1":
        tail = archive.journal(INDEX_LIMIT)[0]
        remaining = maximum - len(archive.catalog) - len(tail)
        budget([archive.ranges["recovery"][1]], remaining)
        archive.verify_fixed()
        blocks = [archive.catalog, *archive.parts_bytes(("recovery",), remaining), tail]
        return v2_packet(source, "opaque", blocks, maximum)
    staged = send is not None and request.get("progressive") in (1, 2)
    tail = archive.journal(INDEX_LIMIT)[0]
    kinds = ("manifest", "presence", "first")
    if not staged:
        kinds += ("preview", "exact")
    parts = archive.parts_bytes(kinds, maximum - len(archive.catalog) - len(tail))
    base = [archive.catalog, *parts[:2], tail, *parts[2:]]
    if staged:
        send(*v2_packet(source, "first", base, maximum))
        preview = archive.parts_bytes(("preview",), maximum)
        send(*v2_packet(source, "preview", preview, maximum))
        return v2_packet(source, "exact", archive.parts_bytes(("exact",), maximum), maximum)
    return v2_packet(source, "markers", base, maximum)
