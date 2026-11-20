"""scripts/isolation-root-metadata.py: canonical root.json from public keys only (plan R2 §4.2)."""
import base64
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/isolation-root-metadata.py"
SPEC = importlib.util.spec_from_file_location("isolation_root_metadata", SCRIPT)
META = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(META)
SK = "sk-ssh-ed25519@openssh.com"
ED = "ssh-ed25519"
NOW = 1_800_000_000
EXPIRES = 2_000_000_000


def ssh_string(value):
    return struct.pack(">I", len(value)) + value


def public_line(algorithm, raw, application=None, comment="duduclaw-TEST-ONLY"):
    blob = ssh_string(algorithm.encode()) + ssh_string(raw)
    if application is not None:
        blob += ssh_string(application.encode())
    return f"{algorithm} {base64.b64encode(blob).decode()} {comment}\n", blob


class RootMetadataTests(unittest.TestCase):
    def key_set(self):
        return (public_line(SK, bytes([1]) * 32, META.ROOT_APPLICATION), public_line(ED, bytes([2]) * 32),
                public_line(ED, bytes([3]) * 32), public_line(SK, bytes([4]) * 32, META.TARGET_APPLICATION))

    def roots(self, *lines):
        return [META.parse_public_key(line, META.ROOT_APPLICATION) for line in lines]

    def test_canonical_root_matches_the_rust_schema(self):
        (r1, r1_blob), (r2, _), (r3, _), (target, target_blob) = self.key_set()
        document = META.build_root(self.roots(r1, r2, r3), META.parse_public_key(target, META.TARGET_APPLICATION),
                                   1, EXPIRES, now=NOW)
        payload = META.load_signer().canonical_bytes(document)
        decoded = json.loads(payload)
        self.assertEqual(list(decoded), sorted(decoded))
        self.assertEqual(set(decoded), {"expires_at_unix", "frozen", "revoked_key_ids", "root_keys",
                                        "root_threshold", "schema_version", "target_key", "version"})
        self.assertEqual((decoded["schema_version"], decoded["version"], decoded["root_threshold"],
                          decoded["frozen"], decoded["revoked_key_ids"], decoded["expires_at_unix"]),
                         (1, 1, 2, False, [], EXPIRES))
        self.assertEqual(decoded["root_keys"][0], {"algorithm": "sk_ed25519", "application": "ssh:duduclaw-root",
                                                   "key_id": list(hashlib.sha256(r1_blob).digest()),
                                                   "public_blob": list(r1_blob)})
        for key in decoded["root_keys"][1:]:
            self.assertEqual((key["algorithm"], key["application"]), ("ed25519", None))
        self.assertEqual(decoded["target_key"]["key_id"], list(hashlib.sha256(target_blob).digest()))
        self.assertEqual(decoded["target_key"]["application"], "ssh:duduclaw-target")
        # Same role applications and key blob bound as the Rust verifier.
        source = (ROOT / "crates/duduclaw-isolation/src/update.rs").read_text()
        self.assertIn(f'pub const ROOT_SK_APPLICATION: &str = "{META.ROOT_APPLICATION}";', source)
        self.assertIn(f'pub const TARGET_SK_APPLICATION: &str = "{META.TARGET_APPLICATION}";', source)
        self.assertIn(f"pub const MAX_KEY_BLOB_BYTES: usize = {META.MAX_KEY_BLOB_BYTES};", source)

    def test_refuses_keys_and_metadata_the_verifier_would_refuse(self):
        (r1, _), (r2, _), (r3, _), (target, _) = self.key_set()
        good_target = META.parse_public_key(target, META.TARGET_APPLICATION)
        r4 = public_line(ED, bytes([5]) * 32)[0]
        refused = {
            "no hardware root": lambda: META.build_root(self.roots(r2, r3, r4), good_target, 1, EXPIRES, now=NOW),
            "duplicate root": lambda: META.build_root(self.roots(r1, r2, r2), good_target, 1, EXPIRES, now=NOW),
            "two roots": lambda: META.build_root(self.roots(r1, r2), good_target, 1, EXPIRES, now=NOW),
            "target is a root key": lambda: META.build_root(self.roots(r1, r2, r3),
                                                           META.parse_public_key(r1, META.ROOT_APPLICATION), 1,
                                                           EXPIRES, now=NOW),
            "expired": lambda: META.build_root(self.roots(r1, r2, r3), good_target, 1, NOW, now=NOW),
            "version zero": lambda: META.build_root(self.roots(r1, r2, r3), good_target, 0, EXPIRES, now=NOW),
            "plain target": lambda: META.parse_public_key(r2, META.TARGET_APPLICATION),
            "root application on target": lambda: META.parse_public_key(r1, META.TARGET_APPLICATION),
            "target application on root": lambda: META.parse_public_key(target, META.ROOT_APPLICATION),
            "other application": lambda: META.parse_public_key(
                public_line(SK, bytes([6]) * 32, "ssh:other")[0], META.ROOT_APPLICATION),
            "short key": lambda: META.parse_public_key(public_line(ED, bytes([6]) * 31)[0], META.ROOT_APPLICATION),
            "line/blob algorithm": lambda: META.parse_public_key(
                SK + " " + r2.split(" ")[1], META.ROOT_APPLICATION),
            "trailing blob bytes": lambda: META.parse_public_key(
                f"{ED} {base64.b64encode(public_line(ED, bytes([7]) * 32)[1] + b'x').decode()}", META.ROOT_APPLICATION),
            "truncated blob": lambda: META.parse_public_key(
                f"{ED} {base64.b64encode(public_line(ED, bytes([7]) * 32)[1][:-1]).decode()}", META.ROOT_APPLICATION),
            "two lines": lambda: META.parse_public_key(r2 + r3, META.ROOT_APPLICATION),
            "crlf": lambda: META.parse_public_key(r2.replace("\n", "\r\n"), META.ROOT_APPLICATION),
            "not base64": lambda: META.parse_public_key(f"{ED} !!!! c\n", META.ROOT_APPLICATION),
            "empty": lambda: META.parse_public_key("", META.ROOT_APPLICATION),
        }
        for algorithm in ("sk-ecdsa-sha2-nistp256@openssh.com", "ecdsa-sha2-nistp256", "ssh-rsa"):
            refused[algorithm] = (lambda algorithm=algorithm: META.parse_public_key(
                public_line(algorithm, bytes([8]) * 32)[0], META.ROOT_APPLICATION))
        for name, call in refused.items():
            with self.subTest(name=name), self.assertRaises(ValueError):
                call()

    def test_cli_writes_once_and_prints_the_fingerprint(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            paths = []
            for name, (line, _) in zip(("r1", "r2", "r3", "target"), self.key_set()):
                path = directory / f"{name}.pub"
                path.write_text(line)
                paths.append(path)
            output = directory / "root.unsigned.json"
            command = [sys.executable, str(SCRIPT), "--root-key", str(paths[0]), "--root-key", str(paths[1]),
                       "--root-key", str(paths[2]), "--target-key", str(paths[3]),
                       "--expires-at-unix", "4000000000", "--output", str(output)]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            payload = output.read_bytes()
            self.assertEqual(result.stdout, hashlib.sha256(payload).hexdigest() + "\n")
            self.assertEqual(META.load_signer().canonical_bytes(json.loads(payload)), payload)
            again = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(again.returncode, 0)
            self.assertEqual(output.read_bytes(), payload)
            refused = subprocess.run(command[:6] + command[8:10] + ["--expires-at-unix", "4000000000", "--output",
                                     str(directory / "two-roots.json")], capture_output=True, text=True)
            self.assertNotEqual(refused.returncode, 0)
            self.assertFalse((directory / "two-roots.json").exists())

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires system OpenSSH")
    def test_real_openssh_ed25519_public_key_parses(self):
        with tempfile.TemporaryDirectory() as temporary:
            key = Path(temporary) / "r2"
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-C", "duduclaw root r2", "-f", str(key)],
                           check=True)
            line = Path(str(key) + ".pub").read_text()
            parsed = META.parse_public_key(line, META.ROOT_APPLICATION)
            self.assertEqual(bytes(parsed["public_blob"]), base64.b64decode(line.split(" ")[1]))
            self.assertEqual((parsed["algorithm"], parsed["application"]), ("ed25519", None))


if __name__ == "__main__":
    unittest.main()
