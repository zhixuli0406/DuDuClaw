"""Offline release-tool tests. All private keys live only in temporary folders."""
import importlib.util
import hashlib
import json
import os
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

    def test_policy_template_version_domain_and_launcher_shape(self):
        binding = {"profile": [2] * 32, "slot": "foreground", "seccomp": {"allowed_syscalls": [0, 231, 322]},
                   "seccomp_digest": [3] * 32, "memory_bytes": 1, "tasks": 1, "cpu_quota_percent": 1,
                   "open_files": 64, "file_size_bytes": 1, "timeout_ms": 300_000,
                   "filesystem": ["system_ro", "work"], "egress": False,
                   "self_only": [{"kind": "prlimit64_self_only"}, {"kind": "prctl", "options": [1, 4, 38]}]}
        launcher = {"reserve": {"memory_bytes": 1, "tasks": 1, "cpu_quota_percent": 1},
                    "helpers": [binding], "helper_tmpfs_bytes": 4096}
        def policy(version=2, domain=SIGN.POLICY_DOMAIN_V2, launcher=launcher):
            document = {"version": version, "runtimes": [{}],
                        "native": [{"profile": [1] * 32, "tmpfs_bytes": 4096, "launcher": launcher}]}
            if domain is not None:
                document["domain"] = domain
            return document
        SIGN.validate_policy_document(policy())
        SIGN.validate_policy_document(policy(version=1, domain=None, launcher=None))
        bad = [policy(version=1, domain=None), policy(version=1), policy(domain=None),
               policy(domain="duduclaw-isolation-policy-v1"), policy(version=3)]
        for change in [("timeout_ms", 300_001), ("filesystem", ["work", "home"]), ("filesystem", ["work_recordings"]),
                       ("egress", True), ("slot", "background"), ("self_only", [{"kind": "prctl", "options": [22]}]),
                       ("self_only", [{"kind": "prlimit64_any"}]), ("tasks", 0), ("extra", 1)]:
            wide = dict(binding)
            wide[change[0]] = change[1]
            bad.append(policy(launcher=dict(launcher, helpers=[wide])))
        bad.append(policy(launcher=dict(launcher, helpers=[binding] * 17)))
        bad.append(policy(launcher=dict(launcher, helper_tmpfs_bytes=8192)))
        for document in bad:
            with self.assertRaises(ValueError):
                SIGN.validate_policy_document(document)

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

    # ---------------------------------------------------------------- R2: --kind bundle

    def test_bundle_namespace_matches_the_rust_verifier(self):
        source = (SCRIPT.parents[1] / "crates/duduclaw-isolation/src/update.rs").read_text()
        constants = {"root": "ROOT_SSHSIG_NAMESPACE", "release": "RELEASE_SSHSIG_NAMESPACE",
                     "runtime": "RUNTIME_SSHSIG_NAMESPACE", "emergency": "EMERGENCY_SSHSIG_NAMESPACE",
                     "bundle": "MIGRATION_BUNDLE_SSHSIG_NAMESPACE"}
        for kind, constant in constants.items():
            self.assertIn(f'pub const {constant}: &str = "{SIGN.NAMESPACES[kind]}";', source)
        self.assertEqual(len(set(SIGN.NAMESPACES.values())), len(SIGN.NAMESPACES))
        self.assertIn("pub const MAX_MIGRATION_BUNDLE_BYTES: u64 = 2 * 1024 * 1024 * 1024;", source)
        self.assertEqual(SIGN.MAX_BUNDLE_BYTES, 2 * 1024 * 1024 * 1024)

    def test_bundle_authorization_binds_tar_digest_size_version_and_arch(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            data = b"ustar bundle bytes" * 100
            tar = directory / "duduclaw-isolated-linux-aarch64-1.83.0.tar"
            tar.write_bytes(data)
            document = SIGN.bundle_authorization(tar, 2_000_000_000, now=1_800_000_000)
            self.assertEqual(document, {
                "schema_version": 1, "product": "duduclaw", "version": {"major": 1, "minor": 83, "patch": 0},
                "arch": "aarch64", "bundle_sha256": list(hashlib.sha256(data).digest()),
                "bundle_size": len(data), "expires_at_unix": 2_000_000_000})
            # Canonical bytes have exactly the BundleAuthorization field set (deny_unknown_fields).
            self.assertEqual(SIGN.canonical_bytes(document)[:24], b'{"arch":"aarch64","bundl')
            for expires in (1_800_000_000, 1_700_000_000, True, 2 ** 63):
                with self.subTest(expires=expires), self.assertRaises(ValueError):
                    SIGN.bundle_authorization(tar, expires, now=1_800_000_000)
            for name in ("duduclaw-isolated-linux-x86_64-1.83.0.tar.gz", "duduclaw-isolated-linux-arm64-1.83.0.tar",
                         "duduclaw-isolated-linux-x86_64-01.83.0.tar", "duduclaw-isolated-linux-x86_64-1.83.tar",
                         "duduclaw-isolated-linux-x86_64-1.83.0-rc1.tar", "isolated-linux-x86_64-1.83.0.tar"):
                renamed = directory / name
                renamed.write_bytes(data)
                with self.subTest(name=name), self.assertRaises(ValueError):
                    SIGN.bundle_authorization(renamed, 2_000_000_000, now=1_800_000_000)
            empty = directory / "duduclaw-isolated-linux-x86_64-1.83.1.tar"
            empty.write_bytes(b"")
            with self.assertRaises(ValueError):
                SIGN.bundle_authorization(empty, 2_000_000_000, now=1_800_000_000)
            linked = directory / "duduclaw-isolated-linux-x86_64-1.83.2.tar"
            linked.symlink_to(tar)
            with self.assertRaises(OSError):
                SIGN.bundle_authorization(linked, 2_000_000_000, now=1_800_000_000)
            hardlinked = directory / "duduclaw-isolated-linux-x86_64-1.83.3.tar"
            os.link(tar, hardlinked)
            with self.assertRaises(ValueError):
                SIGN.bundle_authorization(hardlinked, 2_000_000_000, now=1_800_000_000)

    def test_bundle_kind_arguments_need_a_root_threshold(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            tar = directory / "duduclaw-isolated-linux-x86_64-1.83.0.tar"
            tar.write_bytes(b"bundle")
            template = directory / "template.json"
            template.write_text("{}")
            base = [sys.executable, str(SCRIPT), "--kind", "bundle", "--output", str(directory / "out.json")]
            cases = [
                (["--bundle", str(tar), "--expires-at-unix", "4000000000", "--key", "a"], "root threshold"),
                (["--bundle", str(tar), "--expires-at-unix", "4000000000", "--key", "a", "--key", "b",
                  "--key", "c", "--key", "d"], "root threshold"),
                (["--bundle", str(tar), "--key", "a", "--key", "b"], "requires --bundle and --expires-at-unix"),
                (["--template", str(template), "--bundle", str(tar), "--expires-at-unix", "4000000000",
                  "--key", "a", "--key", "b"], "never a template"),
                (["--bundle", str(tar), "--expires-at-unix", "1", "--key", "a", "--key", "b"], "expire in the future"),
                (["--bundle", str(tar), "--expires-at-unix", "4000000000", "--key", "a", "--key", "b",
                  "--test-signer", "x", "--ssh-keygen", "/opt/homebrew/bin/ssh-keygen"], "mutually exclusive"),
            ]
            for extra, message in cases:
                with self.subTest(extra=extra):
                    result = subprocess.run(base + extra, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(message, result.stderr)
                    self.assertFalse((directory / "out.json").exists())
            result = subprocess.run([sys.executable, str(SCRIPT), "--kind", "release", "--bundle", str(tar),
                                     "--output", str(directory / "out.json"), "--key", "a"],
                                    capture_output=True, text=True)
            self.assertIn("belong to --kind bundle", result.stderr)

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires system OpenSSH")
    def test_real_ssh_keygen_bundle_threshold_round_trip(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            keys = []
            for name in ("r1", "r2"):
                key = directory / name
                subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
                keys.append(key)
            tar = directory / "duduclaw-isolated-linux-x86_64-1.83.0.tar"
            tar.write_bytes(b"\0" * 2048)
            output = Path(str(tar) + ".authorization.json")
            result = subprocess.run([sys.executable, str(SCRIPT), "--kind", "bundle", "--bundle", str(tar),
                                     "--expires-at-unix", "4000000000", "--output", str(output),
                                     "--key", str(keys[0]), "--key", str(keys[1])], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            payload = output.read_bytes()
            self.assertEqual(json.loads(payload)["bundle_sha256"], list(hashlib.sha256(b"\0" * 2048).digest()))
            self.assertEqual(result.stdout.strip(), hashlib.sha256(payload).hexdigest())
            signatures = json.loads(Path(str(output) + ".signatures.json").read_text())
            self.assertEqual(len(signatures), 2)
            for key, signature in zip(keys, signatures):
                detached = directory / "signature"
                detached.write_text(signature)
                allowed = directory / "allowed"
                allowed.write_text("root " + Path(str(key) + ".pub").read_text())
                for namespace, ok in ((SIGN.NAMESPACES["bundle"], True), (SIGN.NAMESPACES["root"], False)):
                    verification = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed), "-I", "root",
                        "-n", namespace, "-s", str(detached)], input=payload, capture_output=True)
                    self.assertEqual(verification.returncode == 0, ok, verification.stderr)


if __name__ == "__main__":
    unittest.main()
