"""Validate experimental Windows SSH startup and the unchanged Linux default.
Execute the bootstrap locally to check large helper delivery and byte framing.
Use command capture for Windows shell isolation without requiring an SSH host.
"""

import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shlex
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch


SCRIPT = Path(__file__).with_name("03_ssh_progressive_experiment.py")
SPEC = importlib.util.spec_from_file_location("windows_transport_progressive", SCRIPT)
EXPERIMENT = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = EXPERIMENT
SPEC.loader.exec_module(EXPERIMENT)
BASE = EXPERIMENT.base
CONFIG = {"connect_timeout_seconds": 5, "session_timeout_seconds": 10}


class SessionStartupTests(unittest.TestCase):
    def capture(self, path, config, helper):
        process = Mock()
        process.stdin = io.BytesIO()
        process.stdout = io.BytesIO()
        with patch.object(BASE.subprocess, "Popen", return_value=process) as popen, \
                patch.object(BASE.threading, "Timer"):
            session = BASE.Session("test-host", path, config, helper)
        return session, popen.call_args.args[0], process.stdin.getvalue()

    def test_linux_default_command_is_unchanged(self):
        path, helper = "/tmp/a file's.tak", "print('helper')"
        _, arguments, startup = self.capture(path, CONFIG, helper)
        self.assertEqual(arguments, [
            "ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
            "-o", "Compression=no", "-o", "ConnectTimeout=5", "--", "test-host",
            "python3 -u -c " + shlex.quote(helper) + " " + shlex.quote(path),
        ])
        self.assertEqual(startup, b"")

    def test_windows_command_keeps_large_helper_and_unusual_path_on_stdin(self):
        path = r"C:\Users\테스트 user\Downloads\a & ^ % ! (trial).tak"
        helper = EXPERIMENT.remote_helper() + "\n# Large helper delivery\n" * 400
        self.assertGreater(len(helper), 8191)
        python = r"C:\Program Files\Python313\python.exe"
        config = dict(CONFIG, remote_platform="windows", remote_python=python)
        _, arguments, startup = self.capture(path, config, helper)
        command = arguments[-1]
        self.assertLess(len(command), 8191)
        self.assertEqual(command, subprocess.list2cmdline([
            python, "-u", "-c", BASE.WINDOWS_BOOTSTRAP,
        ]))
        self.assertNotIn(path, command)
        self.assertNotIn(helper, command)
        self.assertEqual(json.loads(startup), {"helper_source": helper, "path": path})
        self.assertTrue(startup.isascii())
        self.assertEqual(startup.count(b"\n"), 1)

    def test_windows_unsafe_executable_and_unknown_platform_fail_before_ssh(self):
        for config in [dict(CONFIG, remote_platform="other"),
                       dict(CONFIG, remote_platform="windows", remote_python="python & whoami"),
                       dict(CONFIG, remote_platform="windows", remote_python="python%PATH%")]:
            with self.subTest(config=config), patch.object(BASE.subprocess, "Popen") as popen:
                with self.assertRaises(ValueError):
                    BASE.Session("test-host", "sample.tak", config)
                popen.assert_not_called()


class BootstrapProtocolTests(unittest.TestCase):
    def test_large_helper_preserves_following_json_and_raw_range_bytes(self):
        helper = EXPERIMENT.remote_helper() + "\n# Large helper delivery\n" * 400
        self.assertGreater(len(helper), 8191)
        header = bytes.fromhex("f30a394c0200") + struct.pack("<I", 0)
        raw = header + b"\r\n\x1a\x00\xff\n\r\n" + bytes(range(256))
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "한글 path & ^ % ! (trial).tak"
            path.write_bytes(raw)
            startup = dict(helper_source=helper, path=str(path))
            index = dict(op="channel_index", pieces=[[0, len(header)]],
                         length=len(header), probe_bytes=128)
            requests = [startup, index, [[0, len(raw)]], "md5"]
            wire = b"".join(json.dumps(request).encode("ascii") + b"\n"
                            for request in requests)
            result = subprocess.run([sys.executable, "-u", "-c", BASE.WINDOWS_BOOTSTRAP],
                                    input=wire, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr.decode("utf-8", errors="replace"))
            metadata, reply, remaining = result.stdout.split(b"\n", 2)
            self.assertNotIn(b"\r", metadata + reply)
            self.assertEqual(json.loads(metadata)["size"], len(raw))
            self.assertEqual(json.loads(reply)["records"], [])
            self.assertEqual(remaining, raw + b"OK\n" + hashlib.md5(raw).hexdigest().encode("ascii") + b"\n")


if __name__ == "__main__":
    unittest.main()
