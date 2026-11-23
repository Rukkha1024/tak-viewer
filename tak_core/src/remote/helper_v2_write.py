"""TAKM v2 encoding and logical recovery for the guarded Python write routes.

The SSH bootstrap executes this file in the helper's namespace. The packed
read path uses only the standard library and never reaches these codecs.
"""

import tarfile


V2_SPATIAL = ("first", "preview", "exact")
V2_DEVICES = ("plates", "analog")


def v2_zstd():
    try:
        import zstandard
    except ImportError as error:
        raise Failure("codec_unavailable", "TAKM v2 server writes need Python zstandard") from error
    return zstandard


def v2_numeric():
    try:
        import numpy
        import pcodec
    except ImportError as error:
        raise Failure("codec_unavailable", "Hybrid TAKM v2 server writes need Python numpy and pcodec") from error
    return numpy, pcodec


def v2_zip(*sequences):
    if any(len(seq) != len(sequences[0]) for seq in sequences[1:]):
        raise ValueError("TAKM v2 recipe sequences have different lengths")
    return zip(*sequences)


def v2_pack(*blobs):
    return b"".join(piece for blob in blobs for piece in (struct.pack("<Q", len(blob)), blob))


def v2_unpack(raw, count):
    data = memoryview(raw)
    blobs, pos = [], 0
    if count > len(data) // 8:
        raise ValueError("Truncated TAKM v2 blob table")
    for _ in range(count):
        length, = struct.unpack_from("<Q", data, pos)
        pos += 8
        end = pos + length
        if end > len(data):
            raise ValueError("Truncated TAKM v2 blob")
        blobs.append(data[pos:end])
        pos = end
    if pos != len(data):
        raise ValueError("Unexpected bytes after TAKM v2 blobs")
    return blobs


def v2_expand(archive, kind, maximum, expected=None):
    size = archive.parts[kind][2]
    budget([size], maximum)
    if expected is not None and size != expected:
        raise ValueError("TAKM v2 section differs from expected expanded size")
    packed = archive.read_part(kind, maximum)
    zstd = v2_zstd()
    try:
        frame = zstd.get_frame_parameters(packed)
        if (frame.content_size not in (size, zstd.CONTENTSIZE_UNKNOWN)
                or frame.window_size > 8 << 20):
            raise ValueError("Invalid TAKM v2 Zstandard content size or window")
        decoder = zstd.ZstdDecompressor()
        if frame.content_size == 0:
            # One-shot decode can return early on a declared empty frame.
            stream = decoder.decompressobj()
            raw = stream.decompress(packed)
            if not stream.eof or stream.unused_data:
                raise ValueError("Invalid empty TAKM v2 Zstandard frame")
        else:
            raw = decoder.decompress(packed, max_output_size=max(1, size), allow_extra_data=False)
    except zstd.ZstdError as error:
        raise ValueError("Invalid TAKM v2 Zstandard frame") from error
    if len(raw) != size:
        raise ValueError("TAKM v2 expanded size differs from catalog")
    return raw


def v2_v1_header(header, maximum):
    if len(header) != 64 or header[:12] != b"TAKM-2\r\n\x01\0\0\0":
        raise ValueError("Invalid TAKM v2 logical header")
    offset, table_size, fixed = struct.unpack_from("<QQQ", header, 16)
    if offset < 64 or table_size > 16 << 20 or offset + table_size != fixed:
        raise ValueError("Invalid TAKM v2 logical table bounds")
    budget([fixed, 32], maximum)
    return offset, table_size, fixed


def v2_v1_table(raw_table, offset, table_size):
    if len(raw_table) != table_size:
        raise ValueError("Wrong TAKM v2 logical table size")
    table = json.loads(bytes(raw_table))
    if not isinstance(table, dict) or not isinstance(table.get("streams"), list):
        raise ValueError("Invalid TAKM v2 logical stream table")
    rows = table["streams"]
    dropped = table.get("dropped", [])
    if not isinstance(dropped, list) or len(rows) + len(dropped) > 1000000:
        raise ValueError("Invalid TAKM v2 logical stream counts")
    ordinals = set()
    for row in (*rows, *dropped):
        if not isinstance(row, dict) or not isinstance(row.get("name"), str):
            raise ValueError("Invalid TAKM v2 logical stream")
        index, size = row.get("index"), row.get("size")
        if (type(index) is not int or not 0 <= index < 1 << 64 or index in ordinals
                or type(size) is not int or not 0 <= size < 1 << 64):
            raise ValueError("Invalid or duplicate TAKM v2 source stream ordinal or size")
        ordinals.add(index)
    for row in rows:
        start = row.get("offset")
        if type(start) is not int or not 64 <= start <= offset - row["size"]:
            raise ValueError("TAKM v2 stream outside logical body")
    order = sorted(range(len(rows)), key=lambda i: rows[i]["offset"])
    cursor = 64
    for index in order:
        row = rows[index]
        if row["offset"] < cursor:
            raise ValueError("Overlapping TAKM v2 logical streams")
        cursor = row["offset"] + row["size"]
    return table, rows, order


def v2_legacy(raw, maximum):
    data = memoryview(raw)
    offset, table_size, fixed = v2_v1_header(data[:64], maximum)
    if len(data) < fixed + 32 or hashlib.sha256(data[:fixed]).digest() != data[fixed:fixed + 32]:
        raise ValueError("TAKM v2 logical v1 seal does not match")
    table, rows, order = v2_v1_table(data[offset:fixed], offset, table_size)
    return data, offset, fixed, table, rows, order


def v2_selected(rows):
    # Table order retains original stream ordinals. A full read selects the last
    # occurrence of a name, while every other occurrence is retained in recovery.
    names = {row["name"]: i for i, row in enumerate(rows)}
    return [names[name] for name in ("Nodes.dat", "MetaData.dat", "Channels.dat")]


def v2_manifest(archive, maximum):
    info, nodes, meta, channel_head, channel_rows = v2_unpack(v2_expand(archive, "manifest", maximum), 5)
    return json.loads(bytes(info)), nodes, meta, channel_head, channel_rows


def metadata_v2(archive, maximum):
    if archive.profile == "opaque-v1":
        raw = v2_expand(archive, "recovery", maximum)
        data, _, fixed, _, rows, _ = v2_legacy(raw, maximum)
        if len(data) != fixed + 32:
            raise ValueError("Opaque TAKM v2 recovery contains trailing bytes")
        row = rows[v2_selected(rows)[1]]
        return bytes(data[row["offset"]:row["offset"] + row["size"]])
    return bytes(v2_manifest(archive, maximum)[2])


def v2_pco_lane(raw, count, dtype, np, pco):
    if bool(raw) != bool(count):
        raise ValueError("Missing or unexpected TAKM v2 PCo lane")
    lane = np.empty(count, dtype)
    if not count:
        return lane
    try:
        progress = pco.standalone.simple_decompress_into(bytes(raw), lane)
    except (RuntimeError, TypeError, ValueError) as error:
        raise ValueError("Invalid TAKM v2 PCo stream") from error
    if not progress.finished or progress.n_processed != count:
        raise ValueError("TAKM v2 PCo lane count differs from manifest")
    return lane


def v2_stage_values(archive, stage, specs, maximum, np, pco):
    include_frames = stage in V2_DEVICES
    count = sum(n * (width - 1) for n, width in specs)
    frame_count = sum(n for n, _ in specs) if include_frames else 0
    budget([4 * count, 4 * frame_count], maximum)
    if stage == "plates":
        delta = v2_expand(archive, stage, maximum, 4 * (count + frame_count))
        words = np.frombuffer(delta, "u1").reshape(4, -1).T.copy().view("<u4").ravel()
        result, pos = [], 0
        for n, width in specs:
            end = pos + n * width
            result.append(np.bitwise_xor.accumulate(words[pos:end].reshape(width, n), axis=1).T)
            pos = end
        return result
    raw = archive.read_part(stage, maximum)
    blobs = v2_unpack(raw, 2 if include_frames else 1)
    values = v2_pco_lane(blobs[-1], count, "<f4", np, pco).view("<u4")
    frames = v2_pco_lane(blobs[0], frame_count, "<u4", np, pco) if include_frames else None
    result, pos, f = [], 0, 0
    for n, width in specs:
        end = pos + n * (width - 1)
        array = values[pos:end].reshape(width - 1, n).T
        result.append(np.column_stack((frames[f:f + n], array)) if include_frames else array)
        pos, f = end, f + n
    return result


def v2_masks(frames, first, start):
    first_mask = frames == first
    preview_mask = (frames % 8 == start % 8) & ~first_mask
    return first_mask, preview_mask, ~(first_mask | preview_mask)


class V2MemoryStream:
    def __init__(self, data):
        self.data = data
        self.length = len(data)

    def read(self, offset, length):
        return bytes(self.data[offset:offset + length])


def v2_restore_channels(archive, info, meta, channel_head, raw_heads, total, maximum):
    if (not isinstance(info, dict) or not isinstance(info.get("records"), list)
            or len(channel_head) != 10 or channel_head[:6] != CHANNELS_HEADER):
        raise ValueError("Invalid TAKM v2 channel manifest")
    records = info["records"]
    if struct.unpack_from("<I", channel_head, 6)[0] != len(records):
        raise ValueError("TAKM v2 manifest channel count differs from stream")
    start, stop, first = (info.get(key) for key in ("start", "stop", "first"))
    if (any(type(n) is not int for n in (start, stop, first))
            or not 0 <= start <= first <= stop <= 0xffffffff or info.get("preview_stride") != 8
            or stop - start + 1 > maximum // 4 or (start, stop) != v2_frame_bounds(meta)):
        raise ValueError("Invalid TAKM v2 manifest frame bounds")
    heads = v2_unpack(raw_heads, len(records))
    header_bytes = bytes(channel_head) + b"".join(bytes(h[:-4]) + bytes(4) for h in heads)
    parsed = inventory(V2MemoryStream(header_bytes), 0)
    expanded = len(header_bytes)
    selected = {kind: [] for kind in (*V2_SPATIAL, *V2_DEVICES)}
    for i, (row, head, parsed_row) in enumerate(v2_zip(records, heads, parsed)):
        if not isinstance(row, list) or len(row) != 4:
            raise ValueError("Invalid TAKM v2 channel descriptor")
        width, count, role, marker = row
        if (type(width) is not int or width * 4 != parsed_row["stride"] or type(count) is not int
                or count != struct.unpack_from("<I", head, len(head) - 4)[0]
                or role not in ("spatial", *V2_DEVICES) or type(marker) is not bool
                or (role != "spatial" and marker)):
            raise ValueError("TAKM v2 channel descriptor differs from header")
        expanded += count * width * 4
        if role != "spatial":
            selected[role].append((i, count, width))
    if expanded != total:
        raise ValueError("TAKM v2 reconstructed channel length differs from stream table")
    budget([total], maximum)
    np, pco = v2_numeric()
    spatial = [i for i, row in enumerate(records) if row[2] == "spatial"]
    presence = v2_expand(archive, "presence", maximum)
    runs = v2_unpack(presence, len(spatial))
    channel_arrays = [None] * len(records)
    masks, marker_first = {}, None
    for i, packed in v2_zip(spatial, runs):
        width, count, _, marker = records[i]
        if len(packed) % 8:
            raise ValueError("Invalid TAKM v2 presence run bytes")
        array = np.empty((count, width), dtype="<u4")
        pos, prev = 0, -1
        for begin, length in struct.iter_unpack("<II", packed):
            end = begin + length
            if (length == 0 or begin <= prev or end > 1 << 32 or pos + length > count
                    or (marker and (begin < start or end - 1 > stop))):
                raise ValueError("Invalid TAKM v2 presence run")
            if marker:
                marker_first = begin if marker_first is None else min(begin, marker_first)
            array[pos:pos + length, 0] = np.arange(begin, end, dtype="<u4")
            pos, prev = pos + length, end - 1
        if pos != count:
            raise ValueError("TAKM v2 presence count differs from channel")
        channel_arrays[i] = array
        masks[i] = v2_masks(array[:, 0], first, start)
        for kind, mask in zip(V2_SPATIAL, masks[i]):
            selected[kind].append((i, int(mask.sum()), width))
    if (start if marker_first is None else marker_first) != first:
        raise ValueError("TAKM v2 first frame differs from marker presence")
    for stage, specs in selected.items():
        arrays = v2_stage_values(archive, stage, [(n, width) for _, n, width in specs], maximum, np, pco)
        for (i, _, _), array in v2_zip(specs, arrays):
            if stage in V2_SPATIAL:
                channel_arrays[i][masks[i][V2_SPATIAL.index(stage)], 1:] = array
            else:
                channel_arrays[i] = array
    return b"".join([channel_head, *(piece for h, a in v2_zip(heads, channel_arrays)
                                    for piece in (h, a.tobytes()))])


def restore_v2(archive, maximum):
    archive.verify_fixed()
    recovery = v2_expand(archive, "recovery", maximum)
    if archive.profile == "opaque-v1":
        _, _, fixed, _, _, _ = v2_legacy(recovery, maximum)
        if len(recovery) != fixed + 32:
            raise ValueError("Opaque TAKM v2 recovery contains trailing bytes")
        return recovery
    info, nodes, meta, channel_head, heads = v2_manifest(archive, maximum)
    if not isinstance(info, dict) or type(info.get("streams")) is not int or info["streams"] < 3:
        raise ValueError("Invalid TAKM v2 logical stream count")
    header, table_raw, seal, gap_raw, *opaque = v2_unpack(recovery, info["streams"] + 1)
    offset, table_size, fixed = v2_v1_header(header, maximum)
    _, rows, order = v2_v1_table(table_raw, offset, table_size)
    if (len(rows) != info["streams"] or info.get("stream_ids") != v2_selected(rows)
            or len(seal) != 32):
        raise ValueError("TAKM v2 manifest stream ordinals differ from logical table")
    node_id, meta_id, ch_id = info["stream_ids"]
    channels = v2_restore_channels(archive, info, meta, channel_head, heads, rows[ch_id]["size"], maximum)
    gaps = v2_unpack(gap_raw, len(rows) + 1)
    selected = {node_id: nodes, meta_id: meta, ch_id: channels}
    others = iter(opaque)
    streams = [selected[i] if i in selected else next(others) for i in range(len(rows))]
    pieces, cursor = [header], 64
    for i, gap in v2_zip(order, gaps[:-1]):
        row = rows[i]
        if len(gap) != row["offset"] - cursor or len(streams[i]) != row["size"]:
            raise ValueError("TAKM v2 recovery stream or gap size differs from logical table")
        pieces.extend((gap, streams[i]))
        cursor = row["offset"] + row["size"]
    if len(gaps[-1]) != offset - cursor:
        raise ValueError("Wrong final TAKM v2 logical gap size")
    raw = b"".join([*pieces, gaps[-1], table_raw, seal])
    if len(raw) != fixed + 32 or hashlib.sha256(memoryview(raw)[:fixed]).digest() != seal:
        raise ValueError("TAKM v2 recovered v1 seal does not match")
    return raw


def v2_marker_uids(nodes):
    uids = set()
    for prefix in (b"\x06\0Marker", b"\x06\0" + "Marker".encode("utf-16le")):
        cursor = 0
        while True:
            at = nodes.find(prefix, cursor)
            if at < 0:
                break
            cursor = at + len(prefix)
            try:
                chars, = struct.unpack_from("<H", nodes, cursor)
                end = cursor + 2 + 2 * chars
                obj = json.loads(nodes[cursor + 2:end].decode("utf-16le"))
                if end + 35 <= len(nodes) and isinstance(obj.get("properties"), list):
                    uids.add(nodes[end + 19:end + 35])
            except (ValueError, AttributeError, struct.error):
                continue
    return uids


def v2_frame_bounds(meta):
    xml = ET.fromstring(properties_xml(bytes(meta))[0])
    props = {}
    for p in xml.findall("property"):
        props.setdefault(p.findtext("name", p.get("name")), p.findtext("value", p.get("value")))
    start, stop = int(props["StartFrame"]), int(props["EndFrame"])
    if not 0 <= start <= stop <= 0xffffffff:
        raise ValueError("Unsupported TAKM v2 motion frame bounds")
    return start, stop


def v2_split_source(nodes, meta, channel_data, maximum):
    records = inventory(V2MemoryStream(channel_data), 0)
    registry = device_registry(nodes)
    for row in records:
        row["analog"] = row["analog"] or row["uid"] in registry[0]
    plates, _, reason = classify_devices(nodes, records, registry)
    if reason:
        raise ValueError(reason)
    start, stop = v2_frame_bounds(meta)
    if stop - start + 1 > maximum // 4:
        raise ValueError("TAKM v2 take has too many frames")
    markers = v2_marker_uids(nodes)
    descriptions = []
    first = None
    channel = memoryview(channel_data)
    for row in records:
        role = "spatial" if not row["analog"] else "plates" if row["device"] in plates else "analog"
        marker = role == "spatial" and row["name"] == "Translation" and row["uid"] in markers
        descriptions.append((row["stride"] // 4, row["count"], role, marker))
        if row["count"] and role == "spatial":
            previous = -1
            for pos in range(row["data"], row["end"], row["stride"]):
                frame, = struct.unpack_from("<I", channel, pos)
                if frame <= previous or (marker and not start <= frame <= stop):
                    raise ValueError("Unsupported TAKM v2 spatial frame sequence")
                previous = frame
            if marker:
                at, = struct.unpack_from("<I", channel, row["data"])
                first = at if first is None else min(at, first)
    return records, descriptions, start, stop, start if first is None else first


def v2_pco_encode(arrays, include_frames, np, pco):
    config = pco.ChunkConfig(compression_level=8)
    values = [array[:, 1:].T.ravel() for array in arrays if len(array)]
    frames = [array[:, 0] for array in arrays if len(array)] if include_frames else []
    compressed_values = pco.standalone.simple_compress(np.concatenate(values).view("<f4"), config) if values else b""
    if include_frames:
        compressed_frames = pco.standalone.simple_compress(np.concatenate(frames), config) if frames else b""
        return v2_pack(compressed_frames, compressed_values)
    return v2_pack(compressed_values)


def v2_pack_hybrid(raw, offset, fixed, rows, order, selected, prepared):
    nodes_id, meta_id, ch_id = selected
    streams = [raw[row["offset"]:row["offset"] + row["size"]] for row in rows]
    records, descriptions, start, stop, first = prepared
    np, pco = v2_numeric()
    sections = {stage: [] for stage in (*V2_SPATIAL, *V2_DEVICES)}
    runs = []
    channel = streams[ch_id]
    for row, (width, count, role, _) in v2_zip(records, descriptions):
        array = np.frombuffer(channel[row["data"]:row["end"]], dtype="<u4").reshape(count, width)
        if role == "spatial":
            frames = array[:, 0]
            if count:
                starts = np.r_[0, np.flatnonzero(np.diff(frames) != 1) + 1]
                runs.append(np.column_stack((frames[starts], np.diff(np.r_[starts, count]))).astype("<u4").tobytes())
            else:
                runs.append(b"")
            for stage, mask in zip(V2_SPATIAL, v2_masks(frames, first, start)):
                sections[stage].append(array[mask])
        else:
            sections[role].append(array)
    plate_words = []
    for array in sections["plates"]:
        lanes = array.T
        delta = np.array(lanes, dtype="<u4", order="C", copy=True)
        delta[:, 1:] = lanes[:, 1:] ^ lanes[:, :-1]
        plate_words.append(delta.ravel())
    words = np.concatenate(plate_words) if plate_words else np.empty(0, dtype="<u4")
    info = dict(start=start, stop=stop, first=first, preview_stride=8, records=descriptions,
                stream_ids=selected, streams=len(rows))
    gaps, cursor = [], 64
    for i in order:
        row = rows[i]
        gaps.append(raw[cursor:row["offset"]])
        cursor = row["offset"] + row["size"]
    gaps.append(raw[cursor:offset])
    manifest = v2_pack(encoded(info).rstrip(b"\n"), streams[nodes_id], streams[meta_id], channel[:10],
                       v2_pack(*(row["head"] for row in records)))
    recovery = v2_pack(raw[:64], raw[offset:fixed], raw[fixed:fixed + 32], v2_pack(*gaps),
                       *(stream for i, stream in enumerate(streams) if i not in selected))
    return dict(manifest=manifest, presence=v2_pack(*runs),
                first=v2_pco_encode(sections["first"], False, np, pco),
                preview=v2_pco_encode(sections["preview"], False, np, pco),
                exact=v2_pco_encode(sections["exact"], False, np, pco),
                plates=words.view("u1").reshape(-1, 4).T.tobytes(),
                analog=v2_pco_encode(sections["analog"], True, np, pco), recovery=recovery)


def v2_tar_header(name, size):
    entry = tarfile.TarInfo(name)
    entry.size = size
    return entry.tobuf(tarfile.USTAR_FORMAT)


def v2_write_member(output, name, data, digest=None):
    for piece in (v2_tar_header(name, len(data)), data, bytes(-len(data) % 512)):
        output.write(piece)
        if digest is not None:
            digest.update(piece)


def write_v2(legacy, output, maximum):
    raw, offset, fixed, _, rows, order = v2_legacy(legacy, maximum)
    tail = raw[fixed + 32:]
    budget([len(tail)], min(maximum, INDEX_LIMIT))
    selected = v2_selected(rows)
    def stream(index):
        row = rows[index]
        return raw[row["offset"]:row["offset"] + row["size"]]
    try:
        prepared = v2_split_source(bytes(stream(selected[0])), bytes(stream(selected[1])), stream(selected[2]), maximum)
    except (ValueError, TypeError, KeyError, AttributeError, struct.error, ET.ParseError):
        prepared = None
    profile = "opaque-v1" if prepared is None else None
    sections = {"recovery": raw[:fixed + 32]} if profile else v2_pack_hybrid(raw, offset, fixed, rows, order, selected, prepared)
    zstd = v2_zstd().ZstdCompressor(level=3, write_checksum=True)
    descriptors = {}
    packed_members = {}
    for kind, data in sections.items():
        name, codec = V2_PARTS[kind]
        budget([len(data)], maximum)
        packed = data if codec == "pco" else zstd.compress(data)
        budget([len(packed)], maximum)
        descriptors[kind] = [name, codec, 0 if codec == "pco" else len(data), hashlib.sha256(packed).hexdigest()]
        packed_members[name] = packed
    catalog = dict(takm=2, parts=descriptors)
    if profile:
        catalog["profile"] = profile
    digest = hashlib.sha256()
    v2_write_member(output, "takm.json", encoded(catalog).rstrip(b"\n"), digest)
    for name, data in packed_members.items():
        v2_write_member(output, name, data, digest)
    fixed_len = output.tell()
    v2_write_member(output, "seal.sha256", digest.digest())
    v2_write_member(output, "comments.csv", tail)
    output.write(bytes(1024))
    output.flush()
    os.fsync(output.fileno())
    archive = V2Archive(Reader(output, output.tell()))
    recovered = restore_v2(archive, maximum)
    journal, _, _ = archive.journal(min(maximum, INDEX_LIMIT))
    if archive.fixed_len != fixed_len or recovered != raw[:fixed + 32] or journal != tail:
        raise Failure("verification_failed", "Written TAKM v2 logical body or Comment journal differs")
    return fixed_len


def v2_patch_source(raw, rows):
    # Keep the existing validation and analog-preservation policy for marker edits.
    logical = Source.__new__(Source)
    logical.nodes = None
    logical.reader = Reader(io.BytesIO(raw), len(raw))
    logical.all_streams = [(row["name"], Stream(logical.reader, [(row["offset"], row["size"])], row["size"]))
                           for row in rows]
    logical.streams = dict(logical.all_streams)
    logical.records = inventory(logical.streams["Channels.dat"], 0)
    return logical


def v2_replace_streams(raw, offset, fixed, table, rows, order, replacements, tail, maximum):
    # Preserve layout 1 reserved bytes, the complete gaps and every table field.
    # Shift physical ranges independently of their original directory order.
    head = bytearray(raw[:64])
    parts, cursor, shift, changed_table = [head], 64, 0, False
    hashes = [None] * len(rows)
    for i in order:
        row = rows[i]
        start, size = row["offset"], row["size"]
        previous = raw[start:start + size]
        data = replacements.get(i, previous)
        digest = hashlib.sha256(data).hexdigest()
        if i in replacements and "sha256" in row and row["sha256"] != digest:
            row["sha256"] = digest
            changed_table = True
        hashes[i] = dict(name=row["name"], index=row["index"], size=len(data), sha256=digest)
        if shift or len(data) != size:
            row["offset"] = start + shift
            row["size"] = len(data)
            changed_table = True
        shift += len(data) - size
        parts.extend((raw[cursor:start], data))
        cursor = start + size
    table_bytes = encoded(table).rstrip(b"\n") if changed_table else raw[offset:fixed]
    table_at = offset + shift
    fixed = table_at + len(table_bytes)
    budget([fixed, 32], maximum)
    struct.pack_into("<QQQ", head, 16, table_at, len(table_bytes), fixed)
    body = b"".join([*parts, raw[cursor:offset], table_bytes])
    return body + hashlib.sha256(body).digest() + tail, hashes


def rewrite_v2(source, output, maximum, nodes=None, channels=None, events=b"", progress=None):
    logical = restore_v2(source.archive, maximum)
    tail, _, _ = source.archive.journal(min(maximum, INDEX_LIMIT))
    raw, offset, fixed, table, rows, order = v2_legacy(logical, maximum)
    nodes_id, meta_id, ch_id = v2_selected(rows)
    replacements = {}
    if progress is not None:
        row = rows[meta_id]
        size = row["size"]
        if size > 256 << 10:
            raise ValueError("Metadata exceeds progress budget")
        start = row["offset"]
        replacements[meta_id] = patch_progress(bytes(raw[start:start + size]), *progress)
    elif nodes is not None:
        replacements[nodes_id] = nodes
        patch_source = v2_patch_source(logical, rows)
        parts = patch_channels(patch_source, channels)
        budget([sum(len(p) if isinstance(p, bytes) else p[2] for p in parts)], maximum)
        replacements[ch_id] = b"".join(p if isinstance(p, bytes) else p[0].read(*p[1:]) for p in parts)
    if events:
        tail = safe_append_tail(tail, events)
    changed, hashes = v2_replace_streams(raw, offset, fixed, table, rows, order, replacements, tail, maximum)
    return write_v2(changed, output, maximum), hashes


def append_v2(source, expected, events, maximum):
    archive = source.archive
    archive.verify_fixed(verify_packed=True)
    limit = min(maximum, INDEX_LIMIT)
    tail, offset, ordinal = archive.journal(limit)
    combined = safe_append_tail(tail, events)
    if not tail:
        combined = combined[len(COMMENT_HEADER):]
    budget([len(combined)], limit)
    suffix = combined[len(tail):]
    source.check(expected)
    with open(source.path, "r+b") as output:
        if identity(output, source.path) != expected:
            raise Failure("source_changed", "Source changed before Comment append")
        if not suffix and source.reader.size >= offset + 1024:
            if all(not any(source.reader.read(at, min(COPY_CHUNK, source.reader.size - at)))
                   for at in range(offset, source.reader.size, COPY_CHUNK)):
                return archive.fixed_len
        output.seek(offset)
        if ordinal > 9999999999999999:
            raise Failure("invalid_file", "TAKM v2 Comment sequence is exhausted")
        if suffix:
            name = f"comments/{ordinal:016d}-{hashlib.sha256(suffix).hexdigest()}.csv"
            v2_write_member(output, name, suffix)
        output.write(bytes(1024))
        output.truncate()
        output.flush()
        os.fsync(output.fileno())
        written = V2Archive(Reader(output, output.tell()))
        if written.fixed_len != archive.fixed_len or written.journal(limit)[0] != combined:
            raise Failure("verification_failed", "Appended TAKM v2 Comment journal differs")
    return archive.fixed_len
