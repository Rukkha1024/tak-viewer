"""Validate progressive channel prefixes and byte-range caching without SSH.
Build synthetic TAK channel records and compare with a separate linear oracle.
Check exact sample bits, sparse frame IDs, and reuse of overlapping ranges.
"""

import hashlib
import importlib.util
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import threading
import unittest


SCRIPT = Path(__file__).with_name("03_ssh_progressive_experiment.py")
SPEC = importlib.util.spec_from_file_location("ssh_progressive_experiment", SCRIPT)
EXPERIMENT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = EXPERIMENT
SPEC.loader.exec_module(EXPERIMENT)
HEADER = bytes.fromhex("f30a394c0200")
DIMENSIONS = {"FloatChannel": 1, "Vector3fChannel": 3, "QuaternionChannel": 4}


def channel(kind, name, frames, uid=1):
    kind_bytes = kind.encode("ascii")
    name_bytes = name.encode("utf-16le")
    dim = DIMENSIONS[kind]
    header = (struct.pack("<H", len(kind_bytes)) + kind_bytes + b"\x04"
              + struct.pack("<H", len(name_bytes) // 2) + name_bytes
              + bytes([uid]) * 16 + bytes(dim * 4)
              + struct.pack("<I", len(frames)))
    values = [0x7FC01234, 0x80000000, 0x3FC00000, 0x3F800000][:dim]
    return header + b"".join(struct.pack("<I", frame)
                              + struct.pack("<" + "I" * dim, *values)
                              for frame in frames)


def channels(records):
    return HEADER + struct.pack("<I", len(records)) + b"".join(records)


def linear_oracle(data, limit):
    """Parse every record sequentially and preserve selected raw sample bytes."""
    if data[:6] != HEADER:
        raise ValueError("Bad synthetic channel stream")
    count = struct.unpack_from("<I", data, 6)[0]
    position = 10
    result, records = [], []
    for _ in range(count):
        start = position
        kind_size = struct.unpack_from("<H", data, position)[0]
        position += 2
        kind = data[position:position + kind_size].decode("ascii")
        position += kind_size
        if data[position] != 4:
            raise ValueError("Bad record version")
        name_size = struct.unpack_from("<H", data, position + 1)[0] * 2
        position += 3
        name = data[position:position + name_size].decode("utf-16le")
        position += name_size + 16 + DIMENSIONS[kind] * 4
        sample_count = struct.unpack_from("<I", data, position)[0]
        sample_header = data[start:position]
        position += 4
        data_start = position
        stride = 4 + 4 * DIMENSIONS[kind]
        samples = []
        previous = -1
        for _ in range(sample_count):
            sample = data[position:position + stride]
            frame = struct.unpack_from("<I", sample)[0]
            if frame <= previous:
                raise ValueError("Non-increasing frames in oracle")
            previous = frame
            if frame < limit:
                samples.append(sample)
            position += stride
        records.append(dict(start=start, header=data_start - start, data=data_start,
                            count=sample_count, stride=stride,
                            analog=name.startswith("AnalogChannel"), end=position))
        if not name.startswith("AnalogChannel"):
            result.append(sample_header + struct.pack("<I", len(samples))
                          + b"".join(samples))
    if position != len(data):
        raise ValueError("Trailing bytes in oracle")
    return channels(result), records


def linear_prefix(data, limit):
    return linear_oracle(data, limit)[0]


class FakeSession:
    def __init__(self, data):
        self.data = data
        self.calls = []

    def fetch(self, ranges):
        self.calls.append(list(ranges))
        return [self.data[offset:offset + length] for offset, length in ranges]


class IntervalReaderTests(unittest.TestCase):
    def setUp(self):
        self.source = bytes(range(128))
        self.session = FakeSession(self.source)
        self.reader = EXPERIMENT.IntervalReader(self.session, len(self.source), 0)

    def test_overlapping_reads_fetch_only_missing_bytes(self):
        for offset, length in [(10, 20), (20, 20), (0, 50)]:
            self.assertEqual(self.reader.read(offset, length),
                             self.source[offset:offset + length])
        self.assertEqual(self.session.calls,
                         [[(10, 20)], [(30, 10)], [(0, 10), (40, 10)]])
        before = len(self.session.calls)
        requested = [(8, 6), (35, 12), (0, 50), (50, 0)]
        self.assertEqual(self.reader.many(requested),
                         b"".join(self.source[a:a + n] for a, n in requested))
        self.assertEqual(len(self.session.calls), before)
        for start, block in self.reader.cache:
            self.assertEqual(hashlib.md5(block).digest(),
                             hashlib.md5(self.source[start:start + len(block)]).digest())

    def test_overlapping_batch_is_fetched_once_in_requested_order(self):
        ranges = [(10, 20), (20, 20), (5, 10), (10, 20)]
        self.assertEqual(self.reader.many(ranges),
                         b"".join(self.source[a:a + n] for a, n in ranges))
        self.assertEqual(self.session.calls, [[(5, 35)]])

    def test_zero_length_ranges_do_not_read_remote_bytes(self):
        self.assertEqual(self.reader.many([(0, 0), (len(self.source), 0)]), b"")
        self.assertEqual(self.session.calls, [])

    def test_invalid_ranges_do_not_read_remote_bytes(self):
        for ranges in [[(-1, 1)], [(0, -1)], [(127, 2)], [(0, 5), (129, 0)]]:
            with self.subTest(ranges=ranges), self.assertRaises(ValueError):
                self.reader.many(ranges)
        self.assertEqual(self.session.calls, [])


class PrefixTests(unittest.TestCase):
    def build(self, raw):
        session = FakeSession(raw)
        reader = EXPERIMENT.IntervalReader(session, len(raw))
        stream = EXPERIMENT.Stream(reader, [(0, len(raw))], len(raw))
        return stream, reader, session

    def test_frame_ids_define_prefix_and_preserve_sample_bits(self):
        cases = {
            "sparse": [0, 119, 500],
            "late": [120, 4800],
            "boundary": [0, 119, 120],
            "empty": [],
            "unsigned": [0, 119, 0x80000000, 0xFFFFFFFF],
            "more_than_120_samples": list(range(119)) + [500] + list(range(600, 720)),
        }
        for name, frames in cases.items():
            with self.subTest(case=name):
                raw = channels([channel("FloatChannel", "AnalogChannel0", [0, 119, 500], 2),
                                channel("Vector3fChannel", "Translation", frames)])
                stream, _, _ = self.build(raw)
                records = EXPERIMENT.inventory(stream, 128)
                nonanalog = [r for r in records if not r["analog"]]
                windows = EXPERIMENT.preview_windows(stream, nonanalog, 120)
                self.assertEqual(windows[nonanalog[0]["start"]],
                                 (0, sum(frame < 120 for frame in frames)))
                actual = EXPERIMENT.payload(stream, nonanalog, windows)
                expected = linear_prefix(raw, 120)
                self.assertEqual(actual, expected)
                self.assertEqual(hashlib.md5(actual).digest(), hashlib.md5(expected).digest())
                # Raw NaN payload and negative zero survive without float conversions.
                if frames and frames[0] < 120:
                    self.assertIn(struct.pack("<III", 0x7FC01234, 0x80000000, 0x3FC00000), actual)

    def test_empty_stream_and_analog_only_prefix(self):
        for raw in [channels([]), channels([channel("FloatChannel", "AnalogChannel0", [0, 1])])]:
            with self.subTest(raw=raw):
                stream, _, _ = self.build(raw)
                records = [r for r in EXPERIMENT.inventory(stream, 128) if not r["analog"]]
                windows = EXPERIMENT.preview_windows(stream, records, 120)
                self.assertEqual(EXPERIMENT.payload(stream, records, windows), channels([]))

    def test_full_payload_and_progressive_reads_reuse_cached_bytes(self):
        raw = channels([channel("Vector3fChannel", "Translation", list(range(500))),
                        channel("FloatChannel", "AnalogChannel0", list(range(700)), 2),
                        channel("Vector3fChannel", "Translation", [0, 119, 500], 3)])
        stream, reader, session = self.build(raw)
        records = EXPERIMENT.inventory(stream, 128)
        nonanalog = [r for r in records if not r["analog"]]
        windows = EXPERIMENT.preview_windows(stream, nonanalog, 120)
        self.assertEqual(EXPERIMENT.payload(stream, nonanalog, windows), linear_prefix(raw, 120))
        self.assertEqual(EXPERIMENT.payload(stream, records), raw)
        before = len(session.calls)
        self.assertEqual(EXPERIMENT.preview_windows(stream, nonanalog, 120), windows)
        self.assertEqual(EXPERIMENT.payload(stream, records), raw)
        self.assertEqual(len(session.calls), before)
        transferred = [span for request in session.calls for span in request]
        transferred.sort()
        self.assertTrue(all(a + n <= b for (a, n), (b, _) in zip(transferred, transferred[1:])),
                        "Progressive stages fetched an already cached byte")
        self.assertEqual(sum(len(block) for _, block in reader.cache), len(raw))

    def test_reference_verification_rejects_non_increasing_ids(self):
        for frames in [[0, 0], [119, 0], list(range(120)) + [119]]:
            with self.subTest(frames=frames):
                raw = channels([channel("Vector3fChannel", "Translation", frames)])
                stream, reader, _ = self.build(raw)
                records = EXPERIMENT.inventory(stream, 128)
                outputs = [{"stage": "complete", "streams": {"Channels.dat": raw}}]
                with self.assertRaisesRegex(ValueError, "strictly increasing"):
                    EXPERIMENT.verify(outputs, reader, raw, {"Channels.dat": raw}, records, 120)


class RemoteIndexProtocolTests(unittest.TestCase):
    def test_fragmented_index_then_range_read_and_source_change(self):
        raw = channels([channel("Vector3fChannel", "Translation", [0, 119, 500]),
                        channel("FloatChannel", "AnalogChannel0", list(range(200)), 2),
                        channel("QuaternionChannel", "Rotation", [], 3)])
        chunks = [raw[:33], raw[33:104], raw[104:]]
        offsets = [512, 64, 1024]
        source = bytearray(1024 + len(chunks[-1]))
        for offset, block in zip(offsets, chunks):
            source[offset:offset + len(block)] = block
        request = dict(op="channel_index", pieces=list(zip(offsets, map(len, chunks))),
                       length=len(raw), probe_bytes=128)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "fragmented.tak"
            path.write_bytes(source)
            process = subprocess.Popen([sys.executable, "-u", "-c", EXPERIMENT.remote_helper(), str(path)],
                                       stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
            timer = threading.Timer(10, process.kill)
            timer.daemon = True
            timer.start()
            try:
                self.assertEqual(json.loads(process.stdout.readline())["size"], len(source))
                process.stdin.write(json.dumps(request).encode("utf-8") + b"\n")
                process.stdin.flush()
                reply = json.loads(process.stdout.readline())
                self.assertEqual(reply["records"], linear_oracle(raw, 120)[1])
                self.assertGreater(reply["header_read_bytes"], 0)
                self.assertGreater(reply["header_read_ranges"], 0)
                ranges = [(512, 33), (64, 7), (1024, 11)]
                process.stdin.write(json.dumps(ranges).encode("utf-8") + b"\n")
                process.stdin.flush()
                expected = b"".join(source[a:a + n] for a, n in ranges)
                self.assertEqual(process.stdout.read(len(expected)), expected)
                self.assertEqual(process.stdout.readline(), b"OK\n")
                path.write_bytes(source + b"changed")
                process.stdin.write(json.dumps(request).encode("utf-8") + b"\n")
                process.stdin.flush()
                remaining, error = process.communicate(timeout=5)
                self.assertNotEqual(process.returncode, 0)
                self.assertEqual(remaining, b"")
                self.assertIn(b"Source changed during experiment", error)
            finally:
                timer.cancel()
                if process.poll() is None:
                    process.kill()
                process.communicate()


if __name__ == "__main__":
    unittest.main()
