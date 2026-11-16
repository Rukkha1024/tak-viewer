"""Exercise the production SSH helper protocol using synthetic TAK and TAKM files.
Compare progressive binary responses with independent fixture byte sequences.
Verify read-only identity checks, limits, malformed input and fragmented streams.
"""

import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HELPER = Path(__file__).with_name("helper.py")
HEADER = bytes.fromhex("f30a394c0200")
END = 0xFFFFFFFE
FREE = 0xFFFFFFFF


def channel(name, frames, uid=bytes(16)):
    kind = b"FloatChannel"
    name = name.encode("utf-16le")
    return (struct.pack("<H", len(kind)) + kind + b"\x04"
            + struct.pack("<H", len(name) // 2) + name + uid + bytes(4)
            + struct.pack("<I", len(frames))
            + b"".join(struct.pack("<II", frame, 0x7FC01234) for frame in frames))


def channels(records):
    return HEADER + struct.pack("<I", len(records)) + b"".join(records)


def device_node(uid, kind):
    properties = [{"name": "NodeName", "value": "same misleading name"},
                  {"name": "DeviceNode_DeviceType", "value": kind}]
    if kind == 2:
        properties.extend({"name": "DeviceNode_Corner " + str(i), "value": "0,0,1"} for i in range(4))
    text = json.dumps(dict(properties=properties), indent=4).encode("utf-16le")
    signature = b"class MoCapCore::cDeviceNode<float,16>"
    return (struct.pack("<H", len(signature)) + signature + struct.pack("<H", len(text) // 2)
            + text + bytes(19) + uid)


def compound(streams):
    """Construct a small compound container with independent regular-sector streams."""
    names = ["Nodes.dat", "MetaData.dat", "Channels.dat"]
    sectors = [bytearray(512), bytearray(512)]
    fat = [0xFFFFFFFD, END]
    starts = []
    for name in names:
        raw = streams[name]
        count = (len(raw) + 511) // 512
        ids = list(range(len(sectors), len(sectors) + count))
        starts.append(ids[0] if ids else END)
        for j, sector_id in enumerate(ids):
            sectors.append(bytearray(raw[j * 512:(j + 1) * 512].ljust(512, b"\0")))
            fat.append(ids[j + 1] if j + 1 < count else END)
    def entry(name, kind, start, size, sibling=FREE, child=FREE):
        data = bytearray(128)
        encoded = (name + "\0").encode("utf-16le")
        data[:len(encoded)] = encoded
        struct.pack_into("<H", data, 64, len(encoded))
        data[66:68] = bytes([kind, 1])
        struct.pack_into("<III", data, 68, FREE, sibling, child)
        struct.pack_into("<IQ", data, 116, start, size)
        return data
    sectors[1][:128] = entry("Root Entry", 5, END, 0, child=1)
    for i, name in enumerate(names):
        sectors[1][128 * (i + 1):128 * (i + 2)] = entry(
            name, 2, starts[i], len(streams[name]), i + 2 if i < 2 else FREE)
    sectors[0][:] = struct.pack("<128I", *(fat + [FREE] * (128 - len(fat))))
    head = bytearray(512)
    head[:8] = bytes.fromhex("d0cf11e0a1b11ae1")
    struct.pack_into("<HHHH", head, 24, 0x3E, 3, 0xFFFE, 9)
    struct.pack_into("<H", head, 32, 6)
    for offset, value in [(44, 1), (48, 1), (56, 0), (60, END), (64, 0), (68, END), (72, 0)]:
        struct.pack_into("<I", head, offset, value)
    struct.pack_into("<109I", head, 76, 0, *([FREE] * 108))
    return bytes(head) + b"".join(sectors)


def mini_compound(streams, mini_size=64):
    raw = bytearray(compound({name: b"" for name in streams}))
    struct.pack_into("<H", raw, 32, mini_size.bit_length() - 1)
    mini_body, mini_fat = bytearray(), []
    for i, block in enumerate(streams.values()):
        first = len(mini_fat)
        count = (len(block) + mini_size - 1) // mini_size
        struct.pack_into("<I", raw, 1024 + (i + 1) * 128 + 116, first)
        struct.pack_into("<Q", raw, 1024 + (i + 1) * 128 + 120, len(block))
        mini_body.extend(block.ljust(count * mini_size, b"\0"))
        mini_fat.extend([first + j + 1 if j + 1 < count else END for j in range(count)])
    sectors = (len(mini_body) + 511) // 512
    struct.pack_into("<III", raw, 56, 4096, 2, 1)
    struct.pack_into("<IQ", raw, 1024 + 116, 3, len(mini_body))
    fat = [0xFFFFFFFD, END, END] + [i + 4 if i + 1 < sectors else END for i in range(sectors)]
    raw[512:1024] = struct.pack("<128I", *(fat + [FREE] * (128 - len(fat))))
    raw.extend(struct.pack("<128I", *(mini_fat + [FREE] * (128 - len(mini_fat)))))
    raw.extend(mini_body.ljust(sectors * 512, b"\0"))
    return bytes(raw)


def fragmented_compound(streams):
    raw = compound(streams)
    sectors = [raw[i:i + 512] for i in range(512, len(raw), 512)]
    mapping = {0: 0, 1: 1}
    mapping.update({i: len(sectors) + 1 - i for i in range(2, len(sectors))})
    result = [bytearray(512) for _ in sectors]
    fat = list(struct.unpack("<128I", sectors[0]))
    remapped = [FREE] * 128
    for old, new in mapping.items():
        result[new][:] = sectors[old]
        remapped[new] = mapping.get(fat[old], fat[old])
    result[0][:] = struct.pack("<128I", *remapped)
    for index in range(1, 4):
        position = index * 128 + 116
        first = struct.unpack_from("<I", result[1], position)[0]
        struct.pack_into("<I", result[1], position, mapping.get(first, first))
    return raw[:512] + b"".join(result)


def takm(streams, tail=b"Time,Comment\r\n"):
    body = bytearray(64)
    table = []
    for i, (name, raw) in enumerate(streams.items()):
        table.append(dict(name=name, index=i, offset=len(body), size=len(raw)))
        body.extend(raw)
    offset = len(body)
    raw_table = json.dumps(dict(streams=table, dropped=[], source=dict(name="x.tak", size=1,
                           sha256="0" * 64), made_by="test", made_at="now")).encode()
    body.extend(raw_table)
    body[:8] = b"TAKM-2\r\n"
    struct.pack_into("<I", body, 8, 1)
    struct.pack_into("<QQQ", body, 16, offset, len(raw_table), len(body))
    return bytes(body) + hashlib.sha256(body).digest() + tail


class IdentityRegressionTests(unittest.TestCase):
    def test_windows_descriptor_and_path_ctime_can_differ(self):
        import importlib.util
        from types import SimpleNamespace
        from unittest.mock import patch
        spec = importlib.util.spec_from_file_location("production_ssh_helper", HELPER)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        attrs = dict(st_dev=5, st_ino=7, st_size=9, st_mtime_ns=11, st_mode=0o100644)
        descriptor = SimpleNamespace(**attrs, st_ctime_ns=13)
        named = SimpleNamespace(**attrs, st_ctime_ns=17)
        with patch.object(helper.os, "fstat", return_value=descriptor), patch.object(helper.os, "stat", return_value=named):
            result = helper.identity(SimpleNamespace(fileno=lambda: 4), "example.tak")
        self.assertEqual(result, [5, 7, 9, 11, 13, 5, 7, 9, 11, 17])


class HelperTests(unittest.TestCase):
    def setUp(self):
        directory = os.environ.get("TAK_SSH_TEST_DIR")
        if directory:
            Path(directory).mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="tak_ssh_py_", dir=directory)
        self.addCleanup(self.temp.cleanup)
        self.process = None
        self.addCleanup(self.stop)
        self.directory = Path(self.temp.name)
        self.path = self.directory / "한글 경로.tak"
        self.marker = channel("Marker", [0, 9, 120])
        self.analog = channel("AnalogChannel0", [0, 2, 8])
        self.streams = {"Nodes.dat": b"nodes", "MetaData.dat": b"metadata",
                        "Channels.dat": channels([self.analog, self.marker])}
        self.path.write_bytes(compound(self.streams))
    def stop(self):
        if self.process:
            try:
                self.process.stdin.close()
                self.process.wait(timeout=5)
                self.assertEqual(self.process.stderr.read(), b"")
            finally:
                if self.process.poll() is None:
                    self.process.kill()
                    self.process.wait()
                self.process.stdout.close()
                self.process.stderr.close()

    def start(self):
        if not self.process:
            self.process = subprocess.Popen([sys.executable, "-u", "-c", HELPER.read_text(encoding="utf-8-sig")],
                                             stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)

    def request(self, request):
        self.start()
        raw = request if isinstance(request, bytes) else json.dumps(request).encode()
        self.process.stdin.write(raw + b"\n")
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        self.assertTrue(line, "Helper exited without a response")
        header = json.loads(line)
        blocks = [self.process.stdout.read(n) for n in header.get("lengths", [])]
        return header, blocks

    def open(self, **updates):
        return self.request(dict(op="open", path=str(self.path), max_bytes=100000,
                                 probe_bytes=128, **updates))

    def test_open_then_analog_preserves_exact_bytes_and_md5(self):
        header, blocks = self.open()
        self.assertTrue(header["ok"], header)
        self.assertEqual(header["format"], "tak")
        self.assertTrue(header["has_analog"])
        self.assertEqual(len(header["identity"]), 10)
        expected = [b"nodes", b"metadata", channels([self.marker]), b"", channels([channel("AnalogChannel0", [])])]
        self.assertEqual(blocks, expected)
        self.assertEqual([hashlib.md5(b).hexdigest() for b in blocks],
                         [hashlib.md5(b).hexdigest() for b in expected])
        result, analog = self.request(dict(op="analog", path=str(self.path),
                         expected_identity=header["identity"], max_bytes=100000, probe_bytes=128))
        self.assertTrue(result["ok"], result)
        self.assertEqual(analog, [channels([self.analog])])

    def test_selective_reads_never_touch_emg_samples_until_explicit_access(self):
        import importlib.util
        from unittest.mock import patch
        spec = importlib.util.spec_from_file_location("selective_ssh_helper", HELPER)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        emg_uid, plate_uid = bytes([21]) * 16, bytes([22]) * 16
        emg = channel("AnalogChannel0", [0, 2, 8], emg_uid)
        plate = channel("AnalogChannel0", [0, 2, 8], plate_uid)
        manifest = channels([channel("AnalogChannel0", [], emg_uid), channel("AnalogChannel0", [], plate_uid)])
        streams = {"Nodes.dat": device_node(plate_uid, 2) + device_node(emg_uid, 1),
                   "MetaData.dat": b"metadata", "Channels.dat": channels([emg, self.marker, plate])}
        for make in [compound, mini_compound, fragmented_compound, takm]:
            with self.subTest(container=make.__name__):
                raw = make(streams)
                self.path.write_bytes(raw)
                # Obtain the physical sample ranges independently before tracing.
                source = helper.Source(str(self.path), 65536)
                record = next(r for r in source.records if r["uid"] == emg_uid)
                protected = source.streams["Channels.dat"].pieces(record["data"], record["count"] * record["stride"])
                source.file.close()
                seen = []
                original = helper.Reader.read
                def traced(reader, offset, length):
                    if any(offset < start + size and start < offset + length for start, size in protected):
                        seen.append((offset, length))
                    return original(reader, offset, length)
                server = helper.Server()
                request = dict(path=str(self.path), max_bytes=100000, probe_bytes=65536)
                try:
                    with patch.object(helper.Reader, "read", traced):
                        header, blocks = server.dispatch(dict(request, op="open"))
                        self.assertEqual(header["device_types"], [1, 2])
                        self.assertEqual(header["plate_devices"], [1])
                        self.assertIsNone(header["analog_fallback"])
                        self.assertEqual(blocks[4], manifest)
                        self.assertEqual(seen, [], "marker open read EMG samples")
                        request["expected_identity"] = header["identity"]
                        _, plate_blocks = server.dispatch(dict(request, op="analog", selection="forceplates"))
                        self.assertEqual(plate_blocks, [channels([channel("AnalogChannel0", [], emg_uid), plate])])
                        self.assertEqual(seen, [], "forceplate stage read EMG samples")
                        _, emg_blocks = server.dispatch(dict(request, op="analog", selection="non_forceplates"))
                        self.assertTrue(seen, "on-access stage never read EMG samples")
                        self.assertEqual(emg_blocks, [channels([emg, channel("AnalogChannel0", [], plate_uid)])])
                        _, full_blocks = server.dispatch(dict(request, op="analog", selection="all"))
                        self.assertEqual(full_blocks, [channels([emg, plate])])
                finally:
                    server.close()

    def test_unknown_device_structure_reports_full_read_fallback(self):
        header, _ = self.open()
        self.assertIsNone(header["device_types"])
        self.assertTrue(header["analog_fallback"])
        _, blocks = self.request(dict(op="analog", path=str(self.path), selection="forceplates",
                                      expected_identity=header["identity"], max_bytes=100000, probe_bytes=128))
        self.assertEqual(blocks, [channels([self.analog])])
        self.assertEqual(self.request(dict(op="close"))[0], dict(ok=True, lengths=[]))
        result, analog = self.request(dict(op="analog", path=str(self.path),
                         expected_identity=header["identity"], max_bytes=100000, probe_bytes=128))
        self.assertTrue(result["ok"], result)
        self.assertEqual(analog, [channels([self.analog])])

    def test_nonstandard_property_with_device_uid_is_skipped_then_falls_back(self):
        import importlib.util
        from unittest.mock import patch
        spec = importlib.util.spec_from_file_location("nonstandard_ssh_helper", HELPER)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        uid = bytes([31]) * 16
        unknown = channel("UnrecognizedEMGProperty", [0, 3, 7], uid)
        streams = {"Nodes.dat": device_node(uid, 1), "MetaData.dat": b"metadata",
                   "Channels.dat": channels([unknown, self.marker])}
        self.path.write_bytes(compound(streams))
        source = helper.Source(str(self.path), 65536)
        record = next(r for r in source.records if r["uid"] == uid)
        protected = source.streams["Channels.dat"].pieces(record["data"], record["count"] * record["stride"])
        source.file.close()
        seen = []
        original = helper.Reader.read
        def traced(reader, offset, length):
            if any(offset < start + size and start < offset + length for start, size in protected):
                seen.append((offset, length))
            return original(reader, offset, length)
        server = helper.Server()
        try:
            with patch.object(helper.Reader, "read", traced):
                request = dict(path=str(self.path), max_bytes=100000, probe_bytes=65536)
                header, blocks = server.dispatch(dict(request, op="open"))
                self.assertIsNone(header["device_types"])
                self.assertEqual(header["analog_fallback"], "unsupported structural device channel property")
                self.assertEqual(header["channel_order"], [True, False])
                self.assertEqual(blocks[2], channels([self.marker]))
                self.assertEqual(blocks[4], channels([channel("UnrecognizedEMGProperty", [], uid)]))
                self.assertFalse(seen, "unknown device property samples leaked into marker open")
                _, analog = server.dispatch(dict(request, op="analog", selection="forceplates",
                                                  expected_identity=header["identity"]))
                self.assertTrue(seen)
                self.assertEqual(analog, [channels([unknown])])
        finally:
            server.close()

    def test_invalid_or_unknown_registry_decides_full_fallback_before_sample_read(self):
        import importlib.util
        import io
        from unittest.mock import patch
        spec = importlib.util.spec_from_file_location("registry_ssh_helper", HELPER)
        helper = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(helper)
        uid = bytes([41]) * 16
        valid = device_node(uid, 1)
        unknown = valid.replace(b"cDeviceNode", b"cUnknownNode")
        escaped_text = json.dumps(dict(properties=[{"name":"DeviceNode_DeviceType", "value":1}]))
        escaped_text = escaped_text.replace("DeviceNode_DeviceType", r"\u0044eviceNode_DeviceType").encode("utf-16le")
        escaped = b"unknown class" + struct.pack("<H", len(escaped_text) // 2) + escaped_text + bytes(19) + uid
        cases = [device_node(uid, 9), valid + valid, unknown, escaped,
                 valid.replace('"properties"'.encode("utf-16le"), '"propertie!"'.encode("utf-16le"))]
        for nodes in cases:
            with self.subTest(nodes=nodes[:32]):
                signal = channel("UnknownDeviceProperty", [0, 3, 7], uid)
                streams = {"Nodes.dat":nodes, "MetaData.dat":b"metadata",
                           "Channels.dat":channels([signal, self.marker])}
                self.path.write_bytes(compound(streams))
                source = helper.Source(str(self.path), 65536)
                record = next(r for r in source.records if r["uid"] == uid)
                protected = source.streams["Channels.dat"].pieces(record["data"], record["count"] * record["stride"])
                source.file.close()
                seen, diagnostic = [], io.StringIO()
                original = helper.Reader.read
                def traced(reader, offset, length):
                    if any(offset < start + size and start < offset + length for start, size in protected):
                        self.assertIn("full-read fallback", diagnostic.getvalue())
                        seen.append((offset, length))
                    return original(reader, offset, length)
                server = helper.Server()
                try:
                    with patch.object(helper.Reader, "read", traced), patch.object(helper.sys, "stderr", diagnostic):
                        header, blocks = server.dispatch(dict(op="open", path=str(self.path), max_bytes=100000, probe_bytes=65536))
                        self.assertTrue(header["full_fallback"])
                        self.assertTrue(header["analog_fallback"])
                        self.assertFalse(header["has_analog"])
                        self.assertEqual(blocks[2], streams["Channels.dat"])
                        self.assertTrue(seen)
                finally:
                    server.close()

    def test_mini_stream_and_fragmented_regular_ranges(self):
        for make in [mini_compound, lambda streams: mini_compound(streams, 512), fragmented_compound]:
            with self.subTest(container=make.__name__):
                marker = channel("Marker", list(range(1000))) if make == fragmented_compound else self.marker
                self.streams["Channels.dat"] = channels([self.analog, marker])
                self.path.write_bytes(make(self.streams))
                header, blocks = self.open()
                self.assertTrue(header["ok"], header)
                self.assertEqual(blocks, [b"nodes", b"metadata", channels([marker]), b"", channels([channel("AnalogChannel0", [])])])

    def test_oversized_request_recovers_and_analog_budget_is_enforced(self):
        header, _ = self.request(b" " * ((1 << 20) + 1))
        self.assertEqual(header["error"], "invalid_request")
        header, _ = self.open()
        reply, _ = self.request(dict(op="analog", path=str(self.path),
                         expected_identity=header["identity"], max_bytes=10, probe_bytes=128))
        self.assertEqual(reply["error"], "payload_too_large")

    def test_changed_and_replaced_sources_are_rejected(self):
        for replacement in [False, True]:
            with self.subTest(replacement=replacement):
                header, _ = self.open()
                if replacement:
                    # Windows cannot replace an open rb handle; a new session must
                    # still reject the original identity after that handle closes.
                    self.request(dict(op="close"))
                    other = self.directory / "replacement"
                    other.write_bytes(self.path.read_bytes())
                    os.replace(other, self.path)
                else:
                    with self.path.open("ab") as file:
                        file.write(b"changed")
                reply, blocks = self.request(dict(op="analog", path=str(self.path),
                         expected_identity=header["identity"], max_bytes=100000, probe_bytes=128))
                self.assertEqual(reply["error"], "source_changed")
                self.assertNotIn("lengths", reply)
                self.assertEqual(blocks, [])

    def test_missing_source_on_fresh_analog_session_reports_change(self):
        header, _ = self.open()
        self.request(dict(op="close"))
        self.path.unlink()
        reply, _ = self.request(dict(op="analog", path=str(self.path),
                         expected_identity=header["identity"], max_bytes=100000, probe_bytes=128))
        self.assertEqual(reply["error"], "source_changed")

    def test_takm_comments_and_unknown_layout(self):
        self.path = self.directory / "take.takm"
        tail = "Time,Comment\r\n1,한글\r\n".encode()
        self.path.write_bytes(takm(self.streams, tail))
        header, blocks = self.open()
        self.assertTrue(header["ok"], header)
        self.assertEqual(header["format"], "takm")
        self.assertEqual(blocks[3], tail)
        raw = bytearray(self.path.read_bytes())
        struct.pack_into("<I", raw, 8, 8)
        self.path.write_bytes(raw)
        header, _ = self.open()
        self.assertEqual(header["error"], "unsupported_format")

    def test_directory_listing_limits_and_unicode(self):
        (self.directory / "folder").mkdir()
        (self.directory / "other.txt").write_text("ignored")
        header, blocks = self.request(dict(op="list", path=str(self.directory), limit=5, max_bytes=100000))
        self.assertTrue(header["ok"], header)
        self.assertEqual(blocks, [])
        self.assertIn("truncated", header)
        self.assertFalse(header["truncated"])
        self.assertEqual({entry["name"] for entry in header["entries"]}, {"folder", self.path.name})
        self.assertTrue(next(e for e in header["entries"] if e["name"] == "folder")["directory"])
        header, _ = self.request(dict(op="list", path=str(self.directory), limit=1, max_bytes=100000))
        self.assertEqual(len(header["entries"]), 1)
        self.assertTrue(header["truncated"])

    def test_directory_header_respects_transport_limit(self):
        for index in range(2300):
            (self.directory / (str(index) + "x" * 220 + ".tak")).touch()
        header, blocks = self.request(dict(op="list", path=str(self.directory), limit=10000,
                                           max_bytes=10 << 20))
        self.assertFalse(header["ok"])
        self.assertEqual(header["error"], "payload_too_large")
        self.assertNotIn("lengths", header)
        self.assertEqual(blocks, [])

    def test_invalid_requests_are_framed_and_session_survives(self):
        for request in [b"not json", [], dict(op="nope"), dict(op="open", path=str(self.path)),
                        dict(op="list", path=str(self.directory), limit=-1, max_bytes=1000),
                        dict(op="open", path=str(self.path), max_bytes=True, probe_bytes=128)]:
            with self.subTest(request=request):
                header, _ = self.request(request)
                self.assertFalse(header["ok"])
                self.assertEqual(header["error"], "invalid_request")
                self.assertNotIn("lengths", header)
        self.assertTrue(self.open()[0]["ok"])

    def test_truncation_and_payload_budget(self):
        self.path.write_bytes(self.path.read_bytes()[:-400])
        header, _ = self.open()
        self.assertFalse(header["ok"])
        self.path.write_bytes(compound(self.streams))
        header, _ = self.request(dict(op="open", path=str(self.path), max_bytes=10, probe_bytes=128))
        self.assertEqual(header["error"], "payload_too_large")


if __name__ == "__main__":
    unittest.main()
