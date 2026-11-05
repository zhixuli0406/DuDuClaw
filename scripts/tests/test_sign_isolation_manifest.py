"""Offline release-tool tests. All private keys live only in temporary folders."""
import importlib.util
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "sign-isolation-manifest.py"
SPEC = importlib.util.spec_from_file_location("sign_isolation", SCRIPT)
SIGN = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIGN)


class ManifestSigningTests(unittest.TestCase):
    def test_canonical_json_and_ambiguous_input(self):
        self.assertEqual(SIGN.canonical_bytes({"z": {"b": 2, "a": "員工"}, "a": 1}),
                         '{"a":1,"z":{"a":"員工","b":2}}'.encode())
        for raw in ['{"a":1,"a":2}', '{"a":1.0}', '{"a":NaN}']:
            with self.assertRaises(ValueError):
                json.loads(raw, object_pairs_hook=SIGN.unique_object,
                           parse_float=SIGN.reject_number, parse_constant=SIGN.reject_number)

    def test_artifact_digest_size_and_path_rejection(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            data = b"test artifact"
            (directory / "gateway").write_bytes(data)
            document = {"artifacts": [{"id": "gateway", "mode": 0o755,
                                       "install_target": {"kind": "gateway"}}]}
            SIGN.bind_artifacts(document, directory)
            self.assertEqual(document["artifacts"][0]["sha256"], list(hashlib.sha256(data).digest()))
            self.assertEqual(document["artifacts"][0]["size_bytes"], len(data))
            for artifact_id in ("../gateway", ".", "..", "/gateway", "x" * 129):
                document["artifacts"][0]["id"] = artifact_id
                with self.assertRaises(ValueError):
                    SIGN.bind_artifacts(document, directory)

    @unittest.skipUnless(hasattr(__import__("os"), "O_NOFOLLOW"), "requires nofollow open")
    def test_symlink_artifact_refused(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            (directory / "source").write_bytes(b"data")
            (directory / "gateway").symlink_to("source")
            document = {"artifacts": [{"id": "gateway", "mode": 0o755,
                                       "install_target": {"kind": "gateway"}}]}
            with self.assertRaises(OSError):
                SIGN.bind_artifacts(document, directory)

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires system OpenSSH")
    def test_real_ssh_keygen_round_trip_and_no_overwrite(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            key = directory / "key"
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
            template = directory / "template.json"
            template.write_text('{"z":2,"a":1}')
            output = directory / "root.json"
            command = [sys.executable, str(SCRIPT), "--kind", "root", "--template", str(template),
                       "--output", str(output), "--key", str(key)]
            subprocess.run(command, check=True, capture_output=True)
            payload = output.read_bytes()
            self.assertEqual(payload, b'{"a":1,"z":2}')
            signatures = json.loads(Path(str(output) + ".signatures.json").read_text())
            detached = directory / "signature"
            detached.write_text(signatures[0])
            allowed = directory / "allowed"
            allowed.write_text("root " + Path(str(key) + ".pub").read_text())
            verification = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed),
                "-I", "root", "-n", SIGN.NAMESPACES["root"], "-s", str(detached)],
                input=payload, capture_output=True)
            self.assertEqual(verification.returncode, 0, verification.stderr)
            self.assertNotEqual(subprocess.run(command, capture_output=True).returncode, 0)
            self.assertEqual(output.read_bytes(), payload)


if __name__ == "__main__":
    unittest.main()
