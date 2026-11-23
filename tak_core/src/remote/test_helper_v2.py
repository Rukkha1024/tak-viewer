"""Verify the helper relays only the requested USTAR payloads."""

import hashlib
import io
import json
import os
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch

import helper


class RelayV2Tests(unittest.TestCase):
    def setUp(self):
        scratch = self.enterContext(tempfile.TemporaryDirectory(dir=os.environ.get("TAK_SSH_TEST_DIR")))
        self.path = Path(scratch) / "sample.takm"
        self.server = helper.Server()
        self.addCleanup(self.server.close)
        # Deliberately use arbitrary packed bytes. Only the client decodes motion.
        self.parts = {kind: (kind.encode() * 530) for kind in helper.V2_PARTS}
        catalog = {"takm": 2, "parts": {
            kind: [name, codec, 128000 if codec != "pco" else 0, hashlib.sha256(self.parts[kind]).hexdigest()]
            for kind, (name, codec) in helper.V2_PARTS.items()
        }}
        self.catalog = json.dumps(catalog).encode()
        self.tail = helper.COMMENT_HEADER
        head = io.BytesIO()
        with tarfile.open(fileobj=head, mode="w", format=tarfile.USTAR_FORMAT) as tar:
            self.add(tar, "takm.json", self.catalog)
            for kind, (name, _) in helper.V2_PARTS.items():
                self.add(tar, name, self.parts[kind])
            self.fixed = head.tell()
            self.add(tar, "seal.sha256", hashlib.sha256(head.getvalue()).digest())
            self.add(tar, "comments.csv", self.tail)
            self.journal = head.tell()
        self.path.write_bytes(head.getvalue())

    @staticmethod
    def add(tar, name, raw):
        info = tarfile.TarInfo(name)
        info.size = len(raw)
        tar.addfile(info, io.BytesIO(raw))

    def request(self, op, **fields):
        return dict({"op": op, "path": str(self.path), "archive_ranges": 2,
                     "max_bytes": 1 << 20, "probe_bytes": 256}, **fields)

    def test_stage_payloads_and_analog_are_on_disk_ranges(self):
        reads, staged = [], []
        original = helper.Reader.read
        def trace(reader, offset, length):
            reads.append((offset, length))
            return original(reader, offset, length)
        def send(header, blocks):
            staged.append((header, blocks, reads.copy()))
        with patch.object(helper.Reader, "read", trace), patch.object(helper.zlib, "compress", side_effect=AssertionError):
            header, exact = self.server.dispatch(self.request("open", progressive=2), send)
            archive = self.server.source.archive
            self.assertEqual(header["fixed_len"], self.fixed)
            self.assertEqual([first[0]["archive_stage"] for first in staged], ["first", "preview"])
            self.assertEqual(header["archive_stage"], "exact")
            self.assertEqual(staged[0][1], [self.catalog, self.parts["manifest"], self.parts["presence"],
                                             self.tail, self.parts["first"]])
            self.assertEqual(staged[1][1], [self.parts["preview"]])
            self.assertEqual(exact, [self.parts["exact"]])
            self.assertEqual(header["identity"], staged[0][0]["identity"])
            for name in ("preview", "exact", "plates", "analog", "recovery"):
                self.assertNotIn(archive.ranges[name], staged[0][2])
            for name in ("plates", "analog", "recovery"):
                self.assertNotIn(archive.ranges[name], reads)
            for selection, part in [("forceplates", "plates"), ("non_forceplates", "analog")]:
                reads.clear()
                _, blocks = self.server.dispatch(self.request("analog", expected_identity=header["identity"],
                                                              selection=selection))
                self.assertEqual(blocks, [self.parts[part]])
                self.assertEqual(reads, [archive.ranges[part]])

    def test_packed_budget_and_source_identity_after_first_packet(self):
        with self.assertRaisesRegex(helper.Failure, "exceeds max_bytes"):
            self.server.dispatch(self.request("open", max_bytes=512), lambda *_: None)
        def change_source(*_):
            self.path.touch()
            current = self.path.stat()
            os.utime(self.path, ns=(current.st_atime_ns, current.st_mtime_ns + 1_000_000_000))
        with self.assertRaisesRegex(helper.Failure, "Source changed"):
            self.server.dispatch(self.request("open", progressive=2), change_source)


if __name__ == "__main__":
    unittest.main()
