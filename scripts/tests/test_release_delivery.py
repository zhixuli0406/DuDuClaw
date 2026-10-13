"""Exercise cryptographic binding and real version-only release trees."""
import base64
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("delivery", ROOT / "scripts/release_delivery.py")
delivery = importlib.util.module_from_spec(spec)
spec.loader.exec_module(delivery)


class DeliveryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.keys = tempfile.TemporaryDirectory()
        cls.key = Path(cls.keys.name) / "key"
        subprocess.run(["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048",
                        "-out", str(cls.key)], check=True, capture_output=True)
        cls.public = subprocess.check_output(["openssl", "pkey", "-in", str(cls.key), "-pubout"]).decode()

    @classmethod
    def tearDownClass(cls):
        cls.keys.cleanup()

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.run_git("init", "-b", "main")
        self.run_git("config", "user.name", "Fixture")
        self.run_git("config", "user.email", "fixture@example.invalid")
        # Every actual manifest family, lock format, and a titled changelog.
        self.original = {
            "Cargo.toml": '[workspace.package]\nversion = "1.2.3"\n',
            "Cargo.lock": '[[package]]\nname = "duduclaw-cli"\nversion = "1.2.3"\n\n[[package]]\nname = "third-party"\nversion = "1.2.3"\n',
            "crates/duduclaw-cli/Cargo.toml": '[package]\nname = "duduclaw-cli"\nversion.workspace = true\n',
            "crates/standalone/Cargo.toml": '[package]\nname = "standalone"\nversion = "1.2.3"\n[dependencies.example]\nversion = "0.7.0"\n',
            "crates/standalone/Cargo.lock": '[[package]]\nname = "standalone"\nversion = "1.2.3"\n',
            "pyproject.toml": '[project]\nversion = "1.2.3"\n',
            "python/duduclaw/__init__.py": '__version__ = "1.2.3"\n',
            "npm/wrapper/package.json": '{"version": "1.2.3", "dependencies":{"@duduclaw/linux-x64":"1.2.3"}}\n',
            "README.md": 'badge/version-1.2.3-blue\n',
            "README.zh-TW.md": 'badge/version-1.2.3-blue\n',
            "README.ja.md": 'badge/version-1.2.3-blue\n',
            "scripts/install.sh": 'FALLBACK_VERSION="1.2.3"\n',
            "scripts/install.ps1": '$FallbackVersion = "1.2.3"\n',
            "src-tauri/tauri.conf.json": '{"version":"1.2.3"}\n',
            "distribution/registries/mcp/server.json": '{"version":"1.2.3","packages":[{"version":"1.2.3"}]}\n',
            "CHANGELOG.md": '# Changelog\n\n## [Unreleased]\n\n### Added\n- Real feature note\n',
        }
        for path, content in self.original.items():
            self.write(path, content)
        self.commit("previous")
        self.run_git("tag", "v1.2.3")
        self.write("feature.txt", "new feature\n")
        self.commit("feature")
        self.source = self.run_git("rev-parse", "HEAD").strip()
        self.run_git("checkout", "-b", "release/1.2.4")
        now = dt.datetime.now(dt.timezone.utc)
        self.data = {
            "schema": 1, "version": "1.2.4", "source_commit": self.source,
            "previous_tag": "v1.2.3", "delta_sha256": delivery.delta(self.root, "v1.2.3", self.source),
            "private_review_sha256": "a" * 64, "issued_at": now.isoformat(),
            "expires_at": (now + dt.timedelta(days=14)).isoformat(),
            "items": {name: {"status": "updated", "artifact_sha256": "b" * 64,
                             "evidence_sha256": "c" * 64, "reason_sha256": None} for name in delivery.ITEMS},
        }

    def run_git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], stderr=subprocess.DEVNULL).decode()

    def write(self, path, contents):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents)

    def commit(self, message):
        self.run_git("add", ".")
        self.run_git("commit", "-m", message)

    def sign(self):
        raw = json.dumps(self.data).encode()
        result = subprocess.run(["openssl", "dgst", "-sha256", "-sign", str(self.key)], input=raw,
                                check=True, capture_output=True)
        return raw, base64.b64encode(result.stdout)

    def verify(self):
        raw, sig = self.sign()
        return delivery.verify_assertion(raw, sig, self.public, "1.2.4", self.root, self.source)

    def bump(self):
        names = {"duduclaw-cli", "standalone"}
        for path, contents in self.original.items():
            if path == "CHANGELOG.md":
                changed = contents.replace("## [Unreleased]\n", "## [Unreleased]\n\n## [1.2.4] - 2026-10-11 — Release theme\n", 1)
            else:
                changed = delivery.expected_bump(path, "1.2.3", contents, "1.2.4", names)
            self.write(path, changed)
        raw, sig = self.sign()
        self.write("distribution/release-delivery/v1.2.4.json", raw.decode())
        self.write("distribution/release-delivery/v1.2.4.sig", sig.decode() + "\n")
        self.commit("bump")
        return self.run_git("rev-parse", "HEAD").strip()

    def test_valid_signature_and_all_five_deliverables(self):
        self.verify()

    def test_tampered_assertion(self):
        raw, sig = self.sign()
        with self.assertRaises(ValueError):
            delivery.verify_assertion(raw.replace(b'1.2.4', b'1.2.5'), sig, self.public, "1.2.4", self.root, self.source)

    def test_wrong_source(self):
        self.data["source_commit"] = "0" * 40
        with self.assertRaises(ValueError): self.verify()

    def test_wrong_version(self):
        self.data["version"] = "1.2.5"
        with self.assertRaises(ValueError): self.verify()

    def test_missing_one_site(self):
        del self.data["items"]["company_site"]
        with self.assertRaises(ValueError): self.verify()

    def test_unreviewed_no_impact(self):
        self.data["items"]["os_site"]["status"] = "unchanged"
        with self.assertRaises(ValueError): self.verify()

    def test_reviewed_no_impact(self):
        self.data["items"]["os_site"].update(status="unchanged", reason_sha256="d" * 64)
        self.verify()

    def test_stale_delta(self):
        self.data["delta_sha256"] = "0" * 64
        with self.assertRaises(ValueError): self.verify()

    def test_delta_ignores_object_id_abbreviation(self):
        # The signer's checkout and a CI clone may abbreviate object ids to
        # different lengths; the bound delta must not depend on that.
        before = delivery.delta(self.root, "v1.2.3", self.source)
        for abbrev in ("7", "12", "40"):
            self.run_git("config", "core.abbrev", abbrev)
            self.assertEqual(delivery.delta(self.root, "v1.2.3", self.source), before)

    def test_expired(self):
        self.data["expires_at"] = self.data["issued_at"]
        with self.assertRaises(ValueError): self.verify()

    def test_version_only_bump_all_manifest_families_and_title(self):
        revision = self.bump()
        delivery.verify_release_commit(self.root, revision, self.source, "1.2.4")

    def test_feature_edit_in_bump_refused(self):
        self.bump()
        self.write("feature.txt", "extra unreviewed feature\n")
        self.run_git("add", ".")
        self.run_git("commit", "--amend", "--no-edit")
        with self.assertRaises(ValueError):
            delivery.verify_release_commit(self.root, "HEAD", self.source, "1.2.4")

    def test_skip_npm_bump_refused(self):
        self.bump()
        self.write("npm/wrapper/package.json", self.original["npm/wrapper/package.json"])
        self.run_git("add", ".")
        self.run_git("commit", "--amend", "--no-edit")
        with self.assertRaises(ValueError):
            delivery.verify_release_commit(self.root, "HEAD", self.source, "1.2.4")

    def test_changelog_feature_note_change_refused(self):
        self.bump()
        p = self.root / "CHANGELOG.md"
        p.write_text(p.read_text().replace("Real feature note", "False feature claim"))
        self.run_git("add", ".")
        self.run_git("commit", "--amend", "--no-edit")
        with self.assertRaises(ValueError):
            delivery.verify_release_commit(self.root, "HEAD", self.source, "1.2.4")

    def test_file_mode_change_refused(self):
        self.bump()
        self.run_git("update-index", "--chmod=+x", "README.md")
        self.run_git("commit", "--amend", "--no-edit")
        with self.assertRaises(ValueError):
            delivery.verify_release_commit(self.root, "HEAD", self.source, "1.2.4")

    def test_pr_merge_same_tree_allowed(self):
        bump = self.bump()
        self.run_git("checkout", "main")
        self.run_git("merge", "--no-ff", "release/1.2.4", "-m", "PR merge")
        delivery.verify_release_commit(self.root, "HEAD", self.source, "1.2.4")
        self.assertEqual(delivery.release_bump(self.root, "HEAD")[0], bump)

    def test_pr_merge_new_feature_refused(self):
        self.bump()
        self.run_git("checkout", "main")
        self.write("other-feature.txt", "not reviewed\n")
        self.commit("new main feature")
        self.run_git("merge", "--no-ff", "release/1.2.4", "-m", "PR merge")
        with self.assertRaises(ValueError): delivery.release_bump(self.root, "HEAD")

    def test_git_override_cannot_redirect(self):
        original = os.environ.get("GIT_DIR")
        os.environ["GIT_DIR"] = str(self.root / "nonexistent")
        try: self.verify()
        finally:
            if original is None: del os.environ["GIT_DIR"]
            else: os.environ["GIT_DIR"] = original

    def test_publish_workflows_gate_all_builds_and_publish_source(self):
        for name in ("release.yml", "desktop-release.yml", "docker-image.yml", "computer-use-image.yml"):
            body = (ROOT / ".github/workflows" / name).read_text()
            self.assertIn("uses: ./.github/workflows/release-delivery-gate.yml", body)
            self.assertIn("  build:\n    needs: delivery-gate", body)
            self.assertIn("ref: ${{ needs.delivery-gate.outputs.commit }}", body)

    def test_install_exact_verified_bytes(self):
        raw, sig = self.sign()
        delivery.install_assertion(self.root, "1.2.4", raw, sig)
        self.assertEqual((self.root / "distribution/release-delivery/v1.2.4.json").read_bytes(), raw)
        with self.assertRaises(ValueError): delivery.install_assertion(self.root, "1.2.4", raw, sig)

    def test_target_symlink_never_overwrites_outside_file(self):
        raw, sig = self.sign()
        folder = self.root / "distribution/release-delivery"; folder.mkdir()
        outside = self.root / "sentinel"; outside.write_bytes(b"protected")
        (folder / "v1.2.4.json").symlink_to(outside)
        with self.assertRaises(ValueError): delivery.install_assertion(self.root, "1.2.4", raw, sig)
        self.assertEqual(outside.read_bytes(), b"protected")

    def test_directory_symlink_never_writes_outside(self):
        raw, sig = self.sign()
        outside = self.root / "outside"; outside.mkdir()
        (self.root / "distribution/release-delivery").symlink_to(outside)
        with self.assertRaises(OSError): delivery.install_assertion(self.root, "1.2.4", raw, sig)
        self.assertEqual(list(outside.iterdir()), [])

    def test_dangling_signature_link_does_not_create_target(self):
        raw, sig = self.sign()
        folder = self.root / "distribution/release-delivery"; folder.mkdir()
        outside = self.root / "absent-target"
        (folder / "v1.2.4.sig").symlink_to(outside)
        with self.assertRaises(ValueError): delivery.install_assertion(self.root, "1.2.4", raw, sig)
        self.assertFalse(outside.exists())


if __name__ == "__main__":
    unittest.main()
