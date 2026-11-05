import importlib.util
import io
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
import zipfile

SCRIPT = Path(__file__).resolve().parents[1] / "verify_release_marker.py"
spec = importlib.util.spec_from_file_location("release_marker", SCRIPT)
marker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(marker)


def record(version="1.80.0"):
    return marker.HEADER + version.encode() + marker.SUFFIX


class ReleaseMarkerTests(unittest.TestCase):
    def test_valid_marker_at_chunk_boundaries(self):
        for offset in [0, 65440, 65500, 65535, 65536, 131070]:
            with self.subTest(offset=offset):
                marker.verify_marker(io.BytesIO(b"x" * offset + record()), "1.80.0")

    def test_rejects_malformed_duplicate_and_conflicting_occurrences(self):
        bad = [b"old binary", marker.MAGIC, record("1.79.3"), record("01.80.0"),
               record("1.80.0-rc.1"), record("18446744073709551616.1.0"),
               record("1" * 63), record()[:-1], marker.HEADER + b"1.80.0",
               record().replace(b":V1:", b":V2:"), record()[1:],
               marker.HEADER + b"1.80.0\0bad", marker.HEADER + b"\xff" + marker.SUFFIX]
        for data in bad:
            for offset in [0, 65510, 65535, 65536, 131070]:
                candidates = [data] if data == b"old binary" else [data, data + record(), record() + data]
                for candidate in candidates:
                    with self.subTest(data=data, offset=offset):
                        with self.assertRaises(ValueError):
                            marker.verify_marker(io.BytesIO(b"x" * offset + candidate), "1.80.0")
        with self.assertRaises(ValueError):
            marker.verify_marker(io.BytesIO(record() * 2), "1.80.0")

    def test_archive_and_cli_tag_gate(self):
        with tempfile.TemporaryDirectory() as directory:
            for extension in ["tar.gz", "zip"]:
                path = Path(directory) / f"asset.{extension}"
                if extension == "zip":
                    with zipfile.ZipFile(path, "w") as archive:
                        archive.writestr("duduclaw.exe", record())
                else:
                    with tarfile.open(path, "w:gz") as archive:
                        entry = tarfile.TarInfo("duduclaw")
                        entry.size = len(record())
                        archive.addfile(entry, io.BytesIO(record()))
                marker.verify_archive(path, "1.80.0")
                for tag, status in [("v1.80.0", 0), ("v1.79.3", 1), ("v01.80.0", 2)]:
                    result = subprocess.run([sys.executable, str(SCRIPT), "--tag", tag,
                                             "--archive", str(path)], capture_output=True)
                    self.assertEqual(result.returncode, status, result.stderr)

    def test_archive_rejects_duplicate_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "asset.tar.gz"
            with tarfile.open(path, "w:gz") as archive:
                for _ in range(2):
                    entry = tarfile.TarInfo("duduclaw")
                    entry.size = len(record())
                    archive.addfile(entry, io.BytesIO(record()))
            with self.assertRaises(ValueError):
                marker.verify_archive(path, "1.80.0")

    def test_pro_packaging_refuses_signing_or_upload_on_mismatch(self):
        root = SCRIPT.parent.parent
        release = (SCRIPT.parent / "release.sh").read_text()
        begin = release.index("package_and_upload_pro_bin() {")
        function = release[begin:release.index("\n}\n", begin) + 3]
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            staged = work / "staged"
            binary_dir = staged / "linux-x64"
            binary_dir.mkdir(parents=True)
            tools = work / "tools"
            tools.mkdir()
            calls = work / "calls"
            for tool in ["minisign", "gcloud"]:
                executable = tools / tool
                executable.write_text(f'#!/bin/sh\necho {tool} >> "$CALLS"\n')
                executable.chmod(0o755)
            env = dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                       CALLS=str(calls), STAGED=str(staged))
            command = ('set -euo pipefail\nPRO_BIN_BUCKET=test\nPRO_BIN_KEY=unused\n' + function
                       + '\npackage_and_upload_pro_bin linux-x64 "$STAGED" 1.80.0\n')
            for version, valid in [("1.79.3", False), ("1.80.0", True)]:
                (binary_dir / "duduclaw-pro").write_bytes(record(version))
                result = subprocess.run(["bash", "-c", command], cwd=root, env=env,
                                        capture_output=True, text=True)
                self.assertEqual(result.returncode == 0, valid, result.stdout + result.stderr)
                if not valid:
                    self.assertFalse(calls.exists(), "mismatched asset was signed/uploaded")
            self.assertEqual(calls.read_text().splitlines(), ["minisign", "gcloud"])

    def test_read_errors_fail_closed(self):
        class Broken:
            def read(self, _):
                raise OSError("cannot read")
        with self.assertRaises(OSError):
            marker.verify_marker(Broken(), "1.80.0")


if __name__ == "__main__":
    unittest.main()
