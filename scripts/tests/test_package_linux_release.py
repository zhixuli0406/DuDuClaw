"""Exercise release staging, legacy size boundaries and offline signing locally."""
import contextlib
import hashlib
import importlib.util
import io
import json
import os
import re
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/package-linux-release.py"
SPEC = importlib.util.spec_from_file_location("linux_release", SCRIPT)
PACKAGE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(PACKAGE)
TARGET = "x86_64-unknown-linux-gnu"


class LinuxReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.binaries = self.root / "bin"
        self.binaries.mkdir()
        for name in PACKAGE.BIN_PACKAGES:
            binary = self.binaries / name
            binary.write_bytes(b"\x7fELF\x00GLIBC_2.17\x00" + name.encode())
            binary.chmod(0o755)
        self.python = self.root / "python"
        self.python.mkdir()
        (self.python / "example.py").write_text("print('fixture')\n")
        self.output = self.root / "out"

    def package(self, target=TARGET):
        return PACKAGE.create_archive(self.binaries, self.python, target, self.output)

    def test_both_archives_have_every_binary_regular_executable_and_sdk(self):
        for target in PACKAGE.TARGETS:
            archive = self.package(target)
            with tarfile.open(archive) as bundle:
                for name in [*PACKAGE.BIN_PACKAGES, *PACKAGE.ARCHIVE_ALIASES]:
                    entry = bundle.getmember(name)
                    self.assertTrue(entry.isfile(), name)
                    self.assertEqual(entry.mode, 0o755)
                    source = PACKAGE.ARCHIVE_ALIASES.get(name, name)
                    self.assertEqual(bundle.extractfile(entry).read(), (self.binaries / source).read_bytes())
                self.assertIn("python/example.py", bundle.getnames())
                self.assertFalse(any("fixture" in name for name in bundle.getnames()))
            self.assertEqual(Path(str(archive) + ".sha256").read_text(),
                             f"{hashlib.sha256(archive.read_bytes()).hexdigest()}  {archive.name}\n")

    def test_missing_component_refused_before_archive_output(self):
        for name in PACKAGE.BIN_PACKAGES:
            with self.subTest(name=name):
                binary = self.binaries / name
                content = binary.read_bytes()
                binary.unlink()
                with self.assertRaises(FileNotFoundError):
                    self.package()
                self.assertFalse(self.output.exists())
                binary.write_bytes(content)
                binary.chmod(0o755)

    def test_legacy_exact_size_boundary_and_empty_files(self):
        binary = self.binaries / "duduclaw"
        for size in (0, PACKAGE.LEGACY_BINARY_LIMIT, PACKAGE.LEGACY_BINARY_LIMIT + 1):
            with binary.open("wb") as output:
                output.truncate(size)
            with self.assertRaises(ValueError):
                PACKAGE.validate_binary(binary)
        with binary.open("wb") as output:
            output.truncate(PACKAGE.LEGACY_BINARY_LIMIT - 1)
        PACKAGE.validate_binary(binary)

    def test_symlink_hardlink_and_nonexecutable_refused(self):
        source = self.binaries / "duduclaw"
        linked = self.root / "link"
        linked.symlink_to(source)
        with self.assertRaises(ValueError):
            PACKAGE.validate_binary(linked)
        linked.unlink()
        os.link(source, linked)
        with self.assertRaises(ValueError):
            PACKAGE.validate_binary(source)
        linked.unlink()
        source.chmod(0o644)
        with self.assertRaises(ValueError):
            PACKAGE.validate_binary(source)

    def test_output_is_not_overwritten(self):
        archive = self.package()
        before = archive.read_bytes()
        with self.assertRaises(ValueError):
            self.package()
        self.assertEqual(before, archive.read_bytes())

    def test_explicit_production_build_includes_parser_engine(self):
        command = PACKAGE.build_command(TARGET, "cross")
        self.assertEqual(command[:2], ["cross", "build"])
        self.assertIn("--locked", command)
        self.assertNotIn("--bins", command)
        self.assertNotIn("duduclaw-run-root-fixture", command)
        for name in PACKAGE.BIN_PACKAGES:
            self.assertIn(name, [command[i + 1] for i, part in enumerate(command) if part == "--bin"])
        self.assertIn("duduclaw-parse-worker/engine", command[command.index("--features") + 1])

    def test_f_publication_names_and_packaged_component_ids_agree(self):
        source = (ROOT / "crates/duduclaw-updated/src/publication.rs").read_text()
        variants = {"gateway": "Gateway", "spawnd": "Spawnd", "trampoline": "Trampoline",
            "egress": "Egress", "egress_forwarder": "EgressForwarder", "mcp_bridge": "McpBridge",
            "parse_worker": "ParseWorker", "importer": "Importer", "migration": "Migration", "update_slot": "UpdateSlot"}
        for kind, variant in variants.items():
            match = re.search(r'InstallTarget::' + variant + r' => "([^"]+)"', source)
            self.assertIsNotNone(match, kind)
            filename = match[1]
            self.assertEqual(PACKAGE.ARCHIVE_ALIASES.get(filename, filename), PACKAGE.INSTALL_BINARIES[kind])
        self.assertEqual(PACKAGE.ARCHIVE_ALIASES["duduclaw-updated-launch"], "duduclaw-updated")

    def test_relative_target_dir_matches_cargo_working_directory(self):
        original = Path.cwd()
        try:
            os.chdir(self.root)
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": "build"}):
                self.assertEqual(PACKAGE.release_binary_dir(TARGET), ROOT / "build" / TARGET / "release")
            with patch.dict(os.environ, {"CARGO_TARGET_DIR": str(self.binaries)}):
                self.assertEqual(PACKAGE.release_binary_dir(TARGET), self.binaries / TARGET / "release")
        finally:
            os.chdir(original)

    def test_absent_offline_key_is_explicit_skip(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(PACKAGE.sign_release(self.binaries, TARGET, self.output,
                             None, None, "ssh-keygen"), [])
        self.assertIn("SKIP protected release manifest", output.getvalue())
        self.assertFalse(self.output.exists())

    def test_ci_refuses_signing_before_reading_key(self):
        with patch.dict(os.environ, {"CI": "true"}):
            with self.assertRaisesRegex(ValueError, "never be used in CI"):
                PACKAGE.sign_release(self.binaries, TARGET, self.output,
                                     self.root / "missing", self.root / "key", "ssh-keygen")

    def bootstrap_inputs(self):
        self.output.mkdir()
        manifest = self.manifest()
        for artifact in manifest["artifacts"]:
            content = (self.binaries / PACKAGE.INSTALL_BINARIES[artifact["install_target"]["kind"]]).read_bytes()
            (self.output / ("isolated-linux-x86_64-" + artifact["id"])).write_bytes(content)
            artifact["sha256"] = list(hashlib.sha256(content).digest())
            artifact["size_bytes"] = len(content)
        canonical = lambda value: json.dumps(value, sort_keys=True, separators=(",", ":")).encode()
        (self.output / "isolated-linux-x86_64-manifest.json").write_bytes(canonical(manifest))
        (self.output / "isolated-linux-x86_64-manifest.json.signatures.json").write_text('["target-fixture"]')
        root = self.root / "root.json"
        root.write_bytes(canonical({"schema_version": 1, "version": 1}))
        signatures = self.root / "root.signatures.json"
        signatures.write_text('["root-fixture-a","root-fixture-b"]')
        return root, signatures

    def test_bootstrap_inbox_matches_verifier_layout_without_active_state(self):
        root, signatures = self.bootstrap_inputs()
        inbox = PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        self.assertEqual({path.name for path in inbox.iterdir()}, {
            "root.json", "root.json.signatures.json", "manifest.json", "manifest.json.signatures.json", "artifacts"})
        self.assertEqual((inbox / "root.json").read_bytes(), root.read_bytes())
        self.assertEqual(len(list((inbox / "artifacts").iterdir())), len(PACKAGE.REQUIRED_TARGETS))
        for path in (inbox / "artifacts").iterdir():
            self.assertEqual(path.stat().st_mode & 0o7777, 0o600)
        with self.assertRaisesRegex(ValueError, "already exists"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)

    def test_bootstrap_inbox_refuses_changed_signed_bytes_before_publication(self):
        root, signatures = self.bootstrap_inputs()
        (self.output / "isolated-linux-x86_64-gateway").write_bytes(b"changed")
        with self.assertRaisesRegex(ValueError, "changed"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        self.assertFalse((self.output / "isolated-linux-x86_64-bootstrap-inbox").exists())

    def test_bootstrap_inbox_refuses_missing_root_threshold_and_incomplete_generation(self):
        root, signatures = self.bootstrap_inputs()
        signatures.write_text('["one-key"]')
        with self.assertRaisesRegex(ValueError, "threshold"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)
        signatures.write_text('["first","second"]')
        path = self.output / "isolated-linux-x86_64-manifest.json"
        manifest = json.loads(path.read_text())
        manifest["artifacts"] = [a for a in manifest["artifacts"] if a["install_target"]["kind"] != "migration"]
        path.write_text(json.dumps(manifest, sort_keys=True, separators=(",", ":")))
        with self.assertRaisesRegex(ValueError, "incomplete"):
            PACKAGE.create_bootstrap_inbox(TARGET, self.output, root, signatures)

    def test_bootstrap_flags_require_offline_signer_before_build(self):
        result = subprocess.run([sys.executable, str(SCRIPT), "--target", TARGET,
            "--bootstrap-root", str(self.root / "missing"), "--skip-build"], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("bootstrap inbox requires", result.stderr)

    def manifest(self):
        return {"schema_version": 1, "product": "duduclaw", "channel": "stable",
                "os": "linux", "arch": "x86_64",
                "version": PACKAGE.workspace_version(),
                "minimum_version": {"major": 1, "minor": 79, "patch": 3},
                "serial": 1, "created_at_unix": 1793808000, "expires_at_unix": 1893456000,
                "protocols": {name: 1 for name in ("gateway", "spawnd", "trampoline", "egress",
                              "updated", "mcp_bridge", "parse_worker", "importer")},
                "migrations": [], "artifacts": [{"id": name, "mode": 0o755,
                "install_target": {"kind": kind}} for kind, name in
                ((kind, kind) for kind in PACKAGE.INSTALL_BINARIES)]}

    def test_signed_runtime_targets_are_distinct_and_have_fixed_sources_and_modes(self):
        document = self.manifest()
        for registry, mode in [("isolation-policy-v1", 0o444), ("api-worker-v1", 0o755),
                               ("native-adapter-v1", 0o755), ("vendor-codex", 0o755), ("vendor-grok", 0o755)]:
            document["artifacts"].append({"id": registry, "mode": mode,
                "install_target": {"kind": "runtime_entry", "registry_id": registry}})
        PACKAGE.validate_targets(document["artifacts"])
        self.assertEqual(PACKAGE.runtime_source(document["artifacts"][11], self.binaries, None),
                         self.binaries / "duduclaw-api-worker")
        with self.assertRaises(ValueError):
            PACKAGE.runtime_source(document["artifacts"][10], self.binaries, None)
        self.assertEqual(PACKAGE.runtime_source(document["artifacts"][10], self.binaries, self.root),
                         self.root / "runtime-isolation-policy-v1")
        duplicate = document["artifacts"] + [document["artifacts"][-1]]
        with self.assertRaises(ValueError):
            PACKAGE.validate_targets(duplicate)
        for registry, mode in [("../vendor", 0o755), ("vendor-grok", 0o444), ("isolation-policy-v1", 0o755)]:
            bad = {"mode": mode, "install_target": {"kind": "runtime_entry", "registry_id": registry}}
            with self.assertRaises(ValueError):
                PACKAGE.runtime_target(bad)

    def test_wrong_arch_product_version_and_incomplete_template_refused(self):
        template = self.root / "template.json"
        for field, value in (("arch", "aarch64"), ("product", "duduclaw_pro"),
                             ("version", {"major": 0, "minor": 0, "patch": 0}), ("artifacts", [])):
            document = self.manifest()
            document[field] = value
            template.write_text(json.dumps(document))
            with patch.dict(os.environ, {"CI": "", "GITHUB_ACTIONS": ""}):
                with self.assertRaises(ValueError):
                    PACKAGE.sign_release(self.binaries, TARGET, self.output, template,
                                         self.root / "key", "ssh-keygen")

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires OpenSSH")
    def test_offline_signer_generates_matching_assets_and_verified_signature(self):
        key = self.root / "test-key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        template = self.root / "template.json"
        document = self.manifest()
        # Valid IDs can collide with another ID's publication prefix. Raw
        # artifacts must stay intact while publication names are constructed.
        document["artifacts"][1]["id"] = "isolated-linux-x86_64-gateway"
        template.write_text(json.dumps(document))
        with patch.dict(os.environ, {"CI": "", "GITHUB_ACTIONS": ""}):
            outputs = PACKAGE.sign_release(self.binaries, TARGET, self.output, template, key, "ssh-keygen")
        self.assertEqual(len(outputs), 12)
        manifest = self.output / "isolated-linux-x86_64-manifest.json"
        document = json.loads(manifest.read_bytes())
        for artifact in document["artifacts"]:
            payload = (self.output / ("isolated-linux-x86_64-" + artifact["id"])).read_bytes()
            self.assertEqual(artifact["sha256"], list(hashlib.sha256(payload).digest()))
            self.assertEqual(artifact["size_bytes"], len(payload))
        signatures = json.loads(Path(str(manifest) + ".signatures.json").read_text())
        detached = self.root / "signature"
        detached.write_text(signatures[0])
        allowed = self.root / "allowed"
        allowed.write_text("release " + Path(str(key) + ".pub").read_text())
        result = subprocess.run(["ssh-keygen", "-Y", "verify", "-f", str(allowed), "-I", "release",
            "-n", "duduclaw-release-manifest-v1@dudustudio", "-s", str(detached)],
            input=manifest.read_bytes(), capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    @unittest.skipUnless(shutil.which("ssh-keygen"), "requires OpenSSH")
    def test_signed_runtime_policy_and_worker_publish_through_bootstrap_inbox(self):
        key = self.root / "runtime-test-key"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        runtime_dir = self.root / "runtime"
        runtime_dir.mkdir()
        (runtime_dir / "runtime-isolation-policy-v1").write_text('{"fixture_only":true}')
        (runtime_dir / "runtime-vendor-codex").write_bytes(b"\x7fELF-test-only-vendor")
        (runtime_dir / "runtime-vendor-codex").chmod(0o755)
        document = self.manifest()
        for registry, mode in [("isolation-policy-v1", 0o444), ("api-worker-v1", 0o755),
                               ("native-adapter-v1", 0o755), ("vendor-codex", 0o755)]:
            document["artifacts"].append({"id":registry,"mode":mode,
                "install_target":{"kind":"runtime_entry","registry_id":registry}})
        template = self.root / "runtime-template.json"
        template.write_text(json.dumps(document))
        with patch.dict(os.environ, {"CI":"", "GITHUB_ACTIONS":""}):
            outputs = PACKAGE.sign_release(self.binaries,TARGET,self.output,template,key,"ssh-keygen",runtime_dir)
        self.assertEqual(len(outputs),16)
        canonical = lambda value: json.dumps(value,sort_keys=True,separators=(",", ":")).encode()
        root = self.root / "root.json"
        root.write_bytes(canonical({"schema_version":1,"version":1}))
        signatures = self.root / "root.signatures.json"
        signatures.write_text('["root-fixture-a","root-fixture-b"]')
        inbox = PACKAGE.create_bootstrap_inbox(TARGET,self.output,root,signatures)
        manifest = json.loads((inbox / "manifest.json").read_bytes())
        self.assertEqual(len(list((inbox / "artifacts").iterdir())),14)
        for artifact in manifest["artifacts"]:
            payload = (inbox / "artifacts" / artifact["id"]).read_bytes()
            self.assertEqual(artifact["sha256"],list(hashlib.sha256(payload).digest()))
            self.assertEqual(artifact["size_bytes"],len(payload))

    def test_release_entrypoint_and_workflow_share_helper(self):
        result = subprocess.run(["bash", str(ROOT / "scripts/release.sh"), "linux-assets", "--help"],
                                cwd=ROOT, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--signing-key", result.stdout)
        for workflow in ("release.yml", "release-cache-warm.yml"):
            body = (ROOT / ".github/workflows" / workflow).read_text()
            self.assertIn("package-linux-release.py --target ${{ matrix.target }} --build-only", body)


if __name__ == "__main__":
    unittest.main()
