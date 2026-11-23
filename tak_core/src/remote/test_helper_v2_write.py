"""Guarded v2 write coverage using local, synthetic TAK and TAKM fixtures."""

import hashlib
import json
import os
from pathlib import Path
import struct
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
import helper
from test_helper import channel, channels, compound, device_node


MARKER = bytes(range(16))
PLATE = bytes(range(16, 32))
OTHER = bytes(range(32, 48))
COMMENT = b"t,add,1,M,0,1,,first,1,,file,,operation=event-one\r\n"
MAXIMUM = 1 << 20


def metadata(progress):
    return (f'<properties><property name="StartFrame" value="2"/><property name="EndFrame" value="28"/>'
            f'<property name="Progress" value="{progress}"/></properties>').encode()


def marker_node():
    properties = dict(properties=[dict(name="NodeName", value="M")])
    raw = json.dumps(properties).encode("utf-16le")
    return b"\x06\x00Marker" + struct.pack("<H", len(raw) // 2) + raw + bytes(19) + MARKER + bytes(33)


def translation(change=0):
    name = "Translation".encode("utf-16le")
    head = (b"\x0f\0Vector3fChannel\x04" + struct.pack("<H", len(name) // 2)
            + name + MARKER + bytes(12) + struct.pack("<I", 5))
    return head + b"".join(struct.pack("<IIII", f, value ^ change, 0x7F812345, 0x80000000)
                           for f, value in zip((3, 4, 10, 18, 28), (1, 0x7FC12345, 0, 0x00800000, 0xff800000)))


def fixture():
    return {"Nodes.dat": marker_node() + device_node(PLATE, 2) + device_node(OTHER, 1),
            "MetaData.dat": metadata(4),
            "Channels.dat": channels([channel("AnalogChannel0", [2, 3], PLATE), translation(),
                                     channel("AnalogChannel0", [7, 99], OTHER)])}


def logical_with_gaps(streams, tails):
    raw = bytearray(64)
    rows = []
    for name, data in streams:
        raw.extend(b"\x81gap\x00")
        rows.append(dict(name=name, index=len(rows), offset=len(raw), size=len(data)))
        raw.extend(data)
    # Directory order and physical order differ, including duplicate names.
    kept = list(reversed(rows[1:]))
    kept.insert(1, rows[0])
    raw.extend(b"trailing-gap")
    offset = len(raw)
    table = json.dumps(dict(streams=kept, dropped=[], source=dict(size=5, sha256="0" * 64, name="x.tak"),
                            made_at="now", made_by="test")).encode()
    raw.extend(table)
    struct.pack_into("<8sIIQQQ", raw, 0, b"TAKM-2\r\n", 1, 0, offset, len(table), len(raw))
    raw[41] = 91
    return bytes(raw) + hashlib.sha256(raw).digest() + tails


class WriteV2Tests(unittest.TestCase):
    def setUp(self):
        directory = Path(os.environ["TAK_SSH_TEST_DIR"])
        self.temp = tempfile.TemporaryDirectory(prefix="tak_ssh_v2_", dir=directory)
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.server = helper.Server()
        self.addCleanup(self.server.close)
        self.source = self.directory / "테스트.tak"
        self.streams = fixture()
        self.source.write_bytes(compound(self.streams))
        self.output = self.source.with_suffix(".takm")

    def request(self, operation, path, **fields):
        return self.server.dispatch(dict(op=operation, path=str(path), max_bytes=MAXIMUM, **fields))[0]

    def version(self, path):
        return self.request("stat", path)["identity"]

    def convert(self):
        return self.request("takm_convert", self.source, takm_layout=2,
                            expected_identity=self.version(self.source))

    def upload(self, path, expected, parts):
        self.request("patch_begin", path, expected_identity=expected, part_lengths=list(map(len, parts)))
        for i, block in enumerate(parts):
            if block:
                self.request("patch_chunk", path, expected_identity=expected, part=i, offset=0, hex=block.hex())

    def append(self, event):
        identity = self.version(self.output)
        self.upload(self.output, identity, (b"", b"", event))
        return self.request("takm_append", self.output, expected_identity=identity)

    def read(self):
        with self.output.open("rb") as stream:
            archive = helper.V2Archive(helper.Reader(stream, self.output.stat().st_size))
            raw = helper.restore_v2(archive, MAXIMUM)
            tail, offset, ordinal = archive.journal(MAXIMUM)
            return raw, tail, offset, ordinal, archive.fixed_len, archive.profile

    def test_convert_and_patch_preserve_original_and_analog(self):
        before = self.source.stat()
        original = self.source.read_bytes()
        result = self.convert()
        raw, tail, _, _, fixed, profile = self.read()
        self.assertEqual(profile, "hybrid")
        self.assertEqual(fixed, result["fixed_len"])
        self.assertEqual(tail, helper.COMMENT_HEADER)
        _, _, _, _, rows, _ = helper.v2_legacy(raw, MAXIMUM)
        self.assertEqual({row["name"]: raw[row["offset"]:row["offset"] + row["size"]] for row in rows}, self.streams)
        with tarfile.open(self.output, "r:") as tar:
            self.assertEqual(tar.getnames(), ["takm.json", *(name for name, _ in helper.V2_PARTS.values()),
                                             "seal.sha256", "comments.csv"])
        identity = self.version(self.output)
        self.upload(self.output, identity, (self.streams["Nodes.dat"], channels([translation(1)]), COMMENT))
        self.request("takm_save_patch", self.output, expected_identity=identity)
        after, tail, _, _, _, _ = self.read()
        _, _, _, _, rows, _ = helper.v2_legacy(after, MAXIMUM)
        found = {r["name"]: after[r["offset"]:r["offset"] + r["size"]] for r in rows}
        self.assertEqual(found["Nodes.dat"], self.streams["Nodes.dat"])
        self.assertEqual(found["MetaData.dat"], metadata(4))
        self.assertEqual(found["Channels.dat"], channels([channel("AnalogChannel0", [2, 3], PLATE),
                                                         translation(1), channel("AnalogChannel0", [7, 99], OTHER)]))
        self.assertEqual(tail, helper.COMMENT_HEADER + COMMENT)
        self.assertEqual(self.source.read_bytes(), original)
        self.assertEqual((self.source.stat().st_size, self.source.stat().st_mtime_ns),
                         (before.st_size, before.st_mtime_ns))

    def test_v2_journal_repair_and_retry_keep_fixed_body(self):
        self.convert()
        self.append(COMMENT)
        _, tail, at, ordinal, fixed, _ = self.read()
        self.assertEqual(ordinal, 2)
        previous = self.output.read_bytes()
        second = COMMENT.replace(b"add,1", b"add,2").replace(b"event-one", b"event-two")
        name = "comments/0000000000000002-" + hashlib.sha256(second).hexdigest() + ".csv"
        self.output.write_bytes(previous[:at] + helper.v2_tar_header(name, len(second)) + second[:20])
        self.assertEqual(self.read()[1], tail)
        self.assertEqual(self.append(second)["fixed_len"], fixed)
        self.assertEqual(self.read()[1], tail + second)
        repaired = self.output.read_bytes()
        self.assertEqual(repaired[:at], previous[:at])
        self.append(second)
        self.assertEqual(self.output.read_bytes(), repaired)

    def test_append_refuses_fixed_or_earlier_journal_damage(self):
        self.convert()
        _, _, first, _, fixed, _ = self.read()
        self.append(COMMENT)
        second = COMMENT.replace(b"add,1", b"add,2").replace(b"event-one", b"event-two")
        self.append(second)
        raw = self.output.read_bytes()
        for at in (first + 512, fixed - 1):
            with self.subTest(offset=at):
                damaged = raw[:at] + bytes([raw[at] ^ 1]) + raw[at + 1:]
                self.output.write_bytes(damaged)
                with self.assertRaises((ValueError, helper.Failure)):
                    self.append(second)
                self.assertEqual(self.output.read_bytes(), damaged)

    def test_append_checks_packed_sha_before_opening_for_write(self):
        self.convert()
        healthy = self.output.read_bytes()
        _, _, append_at, _, _, _ = self.read()
        with tarfile.open(self.output, "r:") as tar:
            preview = tar.getmember("preview.pco.parts")
            seal = tar.getmember("seal.sha256")
        damaged = bytearray(healthy)
        damaged[preview.offset_data + 5] ^= 0x40
        damaged[seal.offset_data:seal.offset_data + 32] = hashlib.sha256(damaged[:seal.offset]).digest()
        self.output.write_bytes(damaged)
        with patch("builtins.open", wraps=open) as access:
            with self.assertRaisesRegex(helper.Failure, r"packed SHA-256 mismatch \(preview\.pco\.parts\)") as failure:
                self.append(COMMENT)
        self.assertEqual(failure.exception.code, "invalid_file")
        self.assertTrue(all(call.args[1] == "rb" for call in access.call_args_list))
        self.assertEqual(self.output.read_bytes(), damaged)

        self.output.write_bytes(healthy)
        self.append(COMMENT)
        appended = self.output.read_bytes()
        self.assertEqual(appended[:append_at], healthy[:append_at])
        self.assertTrue(appended.endswith(bytes(1024)))
        self.assertEqual(self.read()[1], helper.COMMENT_HEADER + COMMENT)
        self.append(COMMENT)
        self.assertEqual(self.output.read_bytes(), appended)

    def test_progress_preserves_duplicate_streams_gaps_and_comments(self):
        streams = [("Nodes.dat", b"an older duplicate"), *self.streams.items(), ("Opaque", b"\xff\xc3")]
        legacy = logical_with_gaps(streams, helper.COMMENT_HEADER + COMMENT)
        with self.output.open("w+b") as output:
            helper.write_v2(legacy, output, MAXIMUM)
        raw, tail, _, _, _, profile = self.read()
        self.assertEqual(profile, "hybrid")
        self.assertEqual(raw + tail, legacy)
        identity = self.version(self.output)
        result, _ = self.server.dispatch(dict(op="progress_batch", paths=[str(self.output)]))
        self.assertEqual(result["rows"][0]["value"], 4)
        with patch.object(helper, "write_v2", side_effect=OSError("write failure fixture")):
            before = self.output.read_bytes()
            with self.assertRaisesRegex(OSError, "write failure fixture"):
                self.request("takm_progress", self.output, expected_identity=identity, before=4, after=18)
            self.assertEqual(self.output.read_bytes(), before)
        self.request("takm_progress", self.output, expected_identity=identity, before=4, after=18)
        changed, after_tail, _, _, _, _ = self.read()
        _, _, _, _, rows, order = helper.v2_legacy(changed, MAXIMUM)
        self.assertEqual(after_tail, tail)
        self.assertEqual(changed[:16], raw[:16])
        self.assertEqual(changed[40:64], raw[40:64])
        got = [(rows[i]["name"], changed[rows[i]["offset"]:rows[i]["offset"] + rows[i]["size"]]) for i in order]
        want = [(name, metadata(18) if name == "MetaData.dat" else content) for name, content in streams]
        self.assertEqual(got, want)
        self.assertEqual(changed.count(b"\x81gap\x00"), len(streams))
        ids = self.request("comment_ids", self.output, expected_identity=self.version(self.output))
        self.assertEqual(ids["next_comment_id"], 2)

    def test_opaque_roundtrip_survives_unsupported_channel_bytes(self):
        self.streams["Channels.dat"] = b"unknown-channel-version"
        self.source.write_bytes(compound(self.streams))
        self.convert()
        raw, _, _, _, _, profile = self.read()
        self.assertEqual(profile, "opaque-v1")
        _, _, _, _, rows, _ = helper.v2_legacy(raw, MAXIMUM)
        self.assertEqual({row["name"]: raw[row["offset"]:row["offset"] + row["size"]] for row in rows}, self.streams)

    def test_codecs_missing_never_publish_a_partial_v2(self):
        with patch.object(helper, "v2_numeric", side_effect=helper.Failure("codec_unavailable", "test dependency")):
            with self.assertRaisesRegex(helper.Failure, "test dependency"):
                self.convert()
        self.assertFalse(self.output.exists())
        self.assertEqual(list(self.directory.glob("*.part")), [])


if __name__ == "__main__":
    unittest.main()
