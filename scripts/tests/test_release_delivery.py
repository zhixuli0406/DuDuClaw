"""Exercise cryptographic binding and real version-only release trees."""
import base64
import datetime as dt
import importlib.util
import json
import os
import shutil
import sys
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

    def schema2(self, lane="full"):
        self.data.update(schema=2, lane=lane,
                         review_kind="independent" if lane == "full" else "automatic-hotfix-retention")

    def test_schema2_full(self):
        self.schema2()
        self.verify()

    def test_schema2_hotfix_all_unchanged(self):
        self.schema2("hotfix")
        for item in self.data["items"].values():
            item.update(status="unchanged", reason_sha256="d" * 64)
        self.verify()

    def test_schema2_hotfix_rejects_any_updated_item(self):
        for name in delivery.ITEMS:
            self.schema2("hotfix")
            for item in self.data["items"].values():
                item.update(status="unchanged", reason_sha256="d" * 64)
            self.data["items"][name].update(status="updated", reason_sha256=None)
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, "all five items unchanged"):
                self.verify()

    def test_schema2_hotfix_non_patch_rejected(self):
        self.schema2("hotfix")
        for item in self.data["items"].values():
            item.update(status="unchanged", reason_sha256="d" * 64)
        self.run_git("tag", "v1.1.3", "v1.2.3")
        self.data["previous_tag"] = "v1.1.3"
        with self.assertRaisesRegex(ValueError, "patch bump"):
            self.verify()

    def test_cli_pass_identifies_legacy_full_and_automatic_hotfix(self):
        for lane in ("full", "hotfix"):
            if lane == "hotfix":
                self.schema2(lane)
                for item in self.data["items"].values():
                    item.update(status="unchanged", reason_sha256="d" * 64)
            raw, sig = self.sign()
            self.write("candidate.json", raw.decode())
            self.write("candidate.sig", sig.decode())
            result = subprocess.run(["python3", str(ROOT / "scripts/release_delivery.py"),
                "--root", str(self.root), "--version", "1.2.4", "--assertion",
                str(self.root / "candidate.json"), "--signature", str(self.root / "candidate.sig")],
                capture_output=True, text=True,
                env=dict(os.environ, RELEASE_DELIVERY_PUBLIC_KEY_PEM=self.public))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("lane=" + lane, result.stdout)
            self.assertIn("review_kind=" + ("independent" if lane == "full" else "automatic-hotfix-retention"),
                          result.stdout)
            if lane == "hotfix":
                self.assertNotIn("independently reviewed", result.stdout)

    def test_schema2_lane_review_mismatch_and_legacy_extra_fields_rejected(self):
        self.schema2("hotfix")
        self.data["review_kind"] = "independent"
        with self.assertRaisesRegex(ValueError, "lane/review"):
            self.verify()
        self.data.update(schema=1)
        with self.assertRaisesRegex(ValueError, "schema"):
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
            if name == "desktop-release.yml":
                self.assertIn("  build-source:\n    needs: delivery-gate", body)
                self.assertIn("  build:\n    needs: [delivery-gate, build-source]", body)
                self.assertIn("ref: ${{ needs.build-source.outputs.commit }}", body)
                self.assertIn('run: echo "commit=$SIGNED_SOURCE" >> "$GITHUB_OUTPUT"', body)
                self.assertNotIn("rebuild_v1750_desktop", body)
            else:
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



class ShipShellTests(unittest.TestCase):
    """Exercise the real shell entry point with local Git remotes and fake services."""
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        (self.root / "scripts").mkdir()
        shutil.copyfile(ROOT / "scripts/release.sh", self.root / "scripts/release.sh")
        (self.root / "scripts/release_delivery.py").write_text(
            'import os,sys\nfrom pathlib import Path\n'
            'Path("delivery-args").write_text(" ".join(sys.argv[1:]))\n'
            'sys.exit(int(os.environ.get("DELIVERY_FAIL", "0")))\n')
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.4"\n')
        self.git("init", "-b", "main")
        self.git("config", "user.name", "Fixture")
        self.git("config", "user.email", "fixture@example.invalid")
        self.git("add", "Cargo.toml")
        self.git("commit", "-m", "base")
        self.git("init", "--bare", str(self.root / "origin.git"))
        self.git("remote", "add", "origin", str(self.root / "origin.git"))
        self.git("push", "origin", "main")
        self.git("checkout", "-b", "chore/release")
        self.git("commit", "--allow-empty", "-m", "bump")
        for tag in ("v1.2.4", "desktop-v1.2.4"):
            self.git("tag", tag)
        self.settings = dict(exists=True, version="1.2.4", latest="v1.2.4", draft=False,
                             prerelease=False, assets=40, desktop="1.2.4", platforms=9,
                             pypi="1.2.4", npm="1.2.4", pr=False, watch_fail=False,
                             run_missing=False, run_state="in_progress:")
        self.tool("gh", r'''import json,os,subprocess,sys
from pathlib import Path
args=sys.argv[1:]; s=json.loads(Path("settings.json").read_text())
with Path("calls").open("a") as f: f.write("gh " + " ".join(args) + "\\n")
if args[:2] == ["release", "view"]:
    if "v1.2.4" in args:
        if not s["exists"]: sys.exit(1)
        print(json.dumps(dict(tagName="v"+s["version"], isDraft=s["draft"], isPrerelease=s["prerelease"], assets=[{"name":"replacement" if n == s.get("missing_asset") else n} for n in ([base+suffix for base in ("duduclaw-darwin-arm64.tar.gz", "duduclaw-darwin-x64.tar.gz", "duduclaw-linux-arm64.tar.gz", "duduclaw-linux-x64.tar.gz", "duduclaw-windows-x64.zip") for suffix in ("", ".sha256", ".minisig")] + ["extra"]*25)[:s["assets"]]])))
    else: print(s["latest"])
elif args[:2] == ["release", "download"]:
    keys=["darwin-aarch64", "darwin-x86_64", "linux-x86_64", "windows-x86_64"]+[str(i) for i in range(5)]
    print(json.dumps(dict(version=s["desktop"], platforms={("replacement" if k == s.get("missing_platform") else k):{"url":s.get("desktop_url", "https://example/desktop-v1.2.4/file")} for k in keys[:s["platforms"]]})))
elif args[0] == "api": print("42")
elif args[:2] == ["pr", "list"]: print("42" if s["pr"] else "")
elif args[:2] == ["pr", "create"]:
    s["pr"]=True; Path("settings.json").write_text(json.dumps(s))
elif args[:2] == ["pr", "checks"]:
    if "name,bucket" in args:
        p=Path("rust-polls"); n=int(p.read_text()) if p.exists() else 0; p.write_text(str(n+1))
        buckets=s.get("rust_buckets", ["pass"])
        bucket=buckets[min(n,len(buckets)-1)]
        print(json.dumps([{"name":"Rust ubuntu-latest", "bucket":bucket}]))
        sys.exit(8 if bucket == "pending" else 1 if bucket in ("fail", "cancel") else 0)
    elif "--json" in args and "--jq" in args:
        count=Path("required-check-polls"); polls=int(count.read_text()) if count.exists() else 0
        count.write_text(str(polls+1))
        # Existing PRs have required checks immediately; tests can opt into
        # delayed reporting to exercise the bounded wait.
        if s.get("checks_appear"):
            print("1" if polls >= 1 else "0")
        else:
            print(str(s.get("required_checks", 1)))
    else: sys.exit(int(os.environ.get("CHECKS_FAIL", "0")))
elif args[:2] == ["pr", "merge"]:
    subprocess.run(["git", "push", "origin", "HEAD:main"], check=True, capture_output=True)
elif args[:2] == ["run", "list"]: print("" if s["run_missing"] else "100")
elif args[:2] == ["run", "view"]: print(s["run_state"])
elif args[:2] == ["run", "watch"]: sys.exit(1 if s["watch_fail"] else 0)
elif args[:2] == ["variable", "get"]: print("fake-public-key")
else: sys.exit("unexpected gh call: " + repr(args))
'''.replace('\\n', '\n'))
        self.tool("curl", r'''import json,sys
from pathlib import Path
s=json.loads(Path("settings.json").read_text()); row="pypi" if "pypi.org" in sys.argv[-1] else "npm"
with Path("calls").open("a") as f: f.write("curl " + row + "\\n")
print(json.dumps({"info": {"version": s[row]}} if row == "pypi" else {"version": s[row]}))
'''.replace('\\n', '\n'))
        self.tool("date", 'import sys\nfrom pathlib import Path\nprint(Path("clock").read_text() if sys.argv[-1] == "+%s" else "2026-11-03T00:00:00Z")\n')
        self.tool("sleep", 'import sys\nfrom pathlib import Path\np=Path("clock"); p.write_text(str(int(p.read_text())+int(sys.argv[1])))\n')
        (self.root / "clock").write_text("0")
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        DUDUCLAW_RELEASE_WITHOUT_PRO="1", RELEASE_DELIVERY_PUBLIC_KEY_PEM="test-key")
        self.env.pop("DUDUCLAW_VERIFY_NO_WAIT", None)

    def tool(self, name, body):
        path = self.bin / name
        path.write_text("#!" + sys.executable + "\n" + body)
        path.chmod(0o755)

    def git(self, *args):
        return subprocess.check_output(["git", "-C", str(self.root), *args], stderr=subprocess.DEVNULL).decode().strip()

    def run_release(self, *args, **overrides):
        (self.root / "settings.json").write_text(json.dumps(self.settings))
        return subprocess.run(["bash", "scripts/release.sh", *args], cwd=self.root,
                              env=dict(self.env, **overrides), capture_output=True, text=True, timeout=30)

    def calls(self):
        path = self.root / "calls"
        return path.read_text() if path.exists() else ""

    def test_ship_argument_errors_do_not_contact_services(self):
        for args in (("ship",), ("ship", "v1.2.4"), ("ship", "1.2.4", "--oops")):
            with self.subTest(args=args):
                result = self.run_release(*args)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("Error:", result.stdout)
                self.assertEqual(self.calls(), "")

    def test_ship_refuses_main_detached_missing_or_moved_tags(self):
        self.git("checkout", "main")
        result = self.run_release("ship", "1.2.4")
        self.assertIn("non-main branch", result.stdout)
        self.git("checkout", "--detach", "chore/release")
        self.assertIn("non-main branch", self.run_release("ship", "1.2.4").stdout)
        self.git("checkout", "chore/release")
        self.git("tag", "-d", "desktop-v1.2.4")
        self.assertIn("missing or does not point", self.run_release("ship", "1.2.4").stdout)
        self.git("tag", "desktop-v1.2.4", "HEAD^")
        self.assertIn("missing or does not point", self.run_release("ship", "1.2.4").stdout)
        self.assertEqual(self.calls(), "")

    def test_ship_refuses_merge_parent(self):
        self.git("checkout", "-b", "other", "HEAD^")
        self.git("commit", "--allow-empty", "-m", "other")
        self.git("checkout", "chore/release")
        self.git("merge", "--no-ff", "other", "-m", "bad bump")
        for tag in ("v1.2.4", "desktop-v1.2.4"):
            self.git("tag", "-f", tag)
        result = self.run_release("ship", "1.2.4")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exactly one parent", result.stdout)

    def test_no_merge_waits_then_prints_command_without_publishing_tags(self):
        result = self.run_release("ship", "1.2.4", "--no-merge")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn('gh pr merge 42 --merge --match-head-commit "'+self.git("rev-parse", "HEAD")+'"', result.stdout)
        self.assertIn("gh pr checks 42 --required --watch", self.calls())
        self.assertNotIn("gh pr merge", self.calls())
        self.assertEqual(self.git("ls-remote", "origin", "refs/tags/*"), "")

    def test_ship_waits_for_required_checks_to_be_reported(self):
        self.settings["checks_appear"] = True
        result = self.run_release("ship", "1.2.4", "--no-merge")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Waiting for required checks to be reported on PR #42", result.stdout)
        self.assertEqual((self.root / "required-check-polls").read_text(), "2")
        self.assertIn("gh pr checks 42 --required --watch", self.calls())

    def test_admin_warns_waits_for_rust(self):
        self.settings["rust_buckets"] = ["pending", "pass"]
        result = self.run_release("ship", "1.2.4", "--admin")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("WARNING: --admin skips", result.stdout)
        self.assertIn("gh pr checks 42 --json name,bucket", self.calls())
        self.assertEqual((self.root / "rust-polls").read_text(), "2")
        self.assertIn("gh pr merge 42 --merge --admin", self.calls())

    def test_ship_pipeline_and_rerun_skip_merged_steps(self):
        result = self.run_release("ship", "1.2.4")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("--admin", self.calls())
        self.assertEqual(self.calls().count("gh run watch 100 --exit-status"), 4)
        self.assertIn("--version 1.2.4 --release-ref v1.2.4 --require-main", (self.root / "delivery-args").read_text())
        for tag in ("v1.2.4", "desktop-v1.2.4"):
            self.assertIn("refs/tags/" + tag, self.git("ls-remote", "origin", "refs/tags/*"))
        (self.root / "calls").write_text("")
        self.settings["run_state"] = "completed:success"
        result = self.run_release("ship", "1.2.4")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("already on origin/main", result.stdout)
        self.assertIn("gh pr checks 42 --required", self.calls())
        self.assertNotIn("gh run watch", self.calls())

    def test_checks_and_signature_failure_do_not_push_tags(self):
        for env in ({"CHECKS_FAIL": "1"}, {"DELIVERY_FAIL": "1"}):
            with self.subTest(env=env):
                result = self.run_release("ship", "1.2.4", **env)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(self.git("ls-remote", "origin", "refs/tags/*"), "")
                self.assertNotIn("gh run watch", self.calls())

    def test_workflow_failure_stops_with_run_id(self):
        self.settings["watch_fail"] = True
        result = self.run_release("ship", "1.2.4")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("run 100 failed", result.stdout)
        self.assertEqual(self.calls().count("gh run watch"), 1)
        self.assertNotIn("gh release view", self.calls())

    def test_missing_workflow_discovery_is_bounded(self):
        self.settings["run_missing"] = True
        result = self.run_release("ship", "1.2.4")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("after 5 minutes", result.stdout)
        self.assertEqual((self.root / "clock").read_text(), "300")

    def test_verify_new_rows_and_failure_details(self):
        result = self.run_release("verify", "1.2.4")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("GitHub Release", result.stdout)
        self.assertIn("desktop latest.json", result.stdout)
        for key, value, row in (("assets", 39, "GitHub Release"), ("draft", True, "GitHub Release"),
                                ("prerelease", True, "GitHub Release"), ("latest", "v1.2.3", "GitHub Release"),
                                ("exists", False, "GitHub Release"), ("version", "1.2.3", "GitHub Release"),
                                ("platforms", 8, "desktop latest.json"), ("desktop", "1.2.3", "desktop latest.json")):
            old = self.settings[key]
            self.settings[key] = value
            with self.subTest(key=key):
                result = self.run_release("verify", "1.2.4")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(row + " MISMATCH", result.stdout)
            self.settings[key] = old

    def test_registry_retry_is_bounded_and_row_specific(self):
        self.settings["pypi"] = "1.2.3"
        result = self.run_release("verify", "1.2.4")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PyPI waiting", result.stdout)
        self.assertRegex(result.stdout, r"PyPI\s+1\.2\.3\s+MISMATCH")
        self.assertRegex(result.stdout, r"npm\s+1\.2\.4\s+OK")
        self.assertEqual((self.root / "clock").read_text(), "300")
        self.assertLessEqual(self.calls().count("curl pypi"), 16)

    def test_registry_no_wait_when_disabled_or_release_absent(self):
        self.settings.update(pypi="1.2.3", npm="1.2.3")
        for absent in (False, True):
            self.settings["exists"] = not absent
            result = self.run_release("verify", "1.2.4", **({} if absent else {"DUDUCLAW_VERIFY_NO_WAIT": ""}))
            self.assertNotEqual(result.returncode, 0)
            self.assertNotIn("waiting for registry", result.stdout)
            self.assertEqual((self.root / "clock").read_text(), "0")

    def prepare_sync(self):
        folder = self.root / "commercial/duduclaw-pro-gateway"
        folder.mkdir(parents=True)
        (folder / "delivery_manifest.py").write_text(
            'import argparse,json\np=argparse.ArgumentParser()\n'
            'for x in ("offered", "current", "add"): p.add_argument("--"+x)\n'
            'a=p.parse_args()\nif a.current: json.loads(a.current)\n'
            'v,sha=a.add.split("=")\n'
            'print(json.dumps({"schema_version":1,"profile":"general-distributor","platform":"linux/amd64","artifacts":[{"version":v,"sha256":sha}]}))\n')
        sha = self.root / "tar-sha"
        sha.write_text("v1.2.4 " + "a" * 64 + "\n")
        self.manifest = json.dumps({"schema_version": 1, "profile": "general-distributor", "platform": "linux/amd64", "artifacts": [{"version": "v1.2.4", "sha256": "a" * 64}]})
        env = [{"name": "DUDUCLAW_PRO_VERSIONS", "value": "v1.2.4"},
               {"name": "DUDUCLAW_GENERAL_DELIVERY_MANIFEST", "value": self.manifest}]
        self.cp_state = dict(template=env, serving=env, traffic="ready")
        self.git("push", "origin", "v1.2.4")
        self.tool("gcloud", r'''import json,sys
from pathlib import Path
a=sys.argv[1:]; p=Path("cp-state"); s=json.loads(p.read_text())
with Path("calls").open("a") as f: f.write("gcloud " + " ".join(a) + "\n")
if a[:2] == ["storage","cat"]: print(s.get("remote_sha", "a"*64)+"  duduclaw-pro-v1.2.4.tar")
elif a[:3] == ["artifacts","docker","images"] or a[:3] == ["storage","objects","describe"]: pass
elif a[:3] == ["run","services","describe"]:
    print(json.dumps({"spec":{"template":{"spec":{"containers":[{"image":"image:original", "env":s["template"]}]}}},
                      "status":{"traffic":[{"revisionName":s["traffic"],"percent":100}], "latestCreatedRevisionName":s.get("created", "ready"), "latestReadyRevisionName":"ready"}}))
elif a[:3] == ["run","revisions","describe"]:
    print(json.dumps({"spec":{"containers":[{"image":s.get("image", "image:original"), "env":s["serving"] if a[3] == "old" else s["template"]}]}}))
elif a[:3] == ["run","services","update-traffic"]:
    s["traffic"]="ready"; p.write_text(json.dumps(s))
elif a[:3] == ["run","services","update"]:
    raw=a[a.index("--update-env-vars")+1][3:]
    s["template"]=[dict(zip(("name","value"), pair.split("=",1))) for pair in raw.split("|")]
    p.write_text(json.dumps(s))
else: sys.exit(1)
''')

    def run_sync(self):
        (self.root / "cp-state").write_text(json.dumps(self.cp_state))
        prefix = (ROOT / "scripts/release.sh").read_text().split("# --- Arg parsing / sub-commands ---")[0]
        return subprocess.run(["bash", "-c", prefix + '\nsync_control_plane 1.2.4 tar-sha\n'],
                              cwd=self.root, env=dict(self.env, DUDUCLAW_GCP_PROJECT="test"),
                              capture_output=True, text=True, timeout=30)

    def test_control_plane_requires_merge_and_preserves_ready_live_state(self):
        self.prepare_sync()
        result = self.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("gcloud ", self.calls())
        self.git("push", "origin", "HEAD:main")
        result = self.run_sync()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("serving on ready", result.stdout)
        self.assertNotIn("services update", self.calls())

    def test_control_plane_updates_and_reads_back_pinned_traffic(self):
        self.prepare_sync()
        self.git("push", "origin", "HEAD:main")
        self.cp_state.update(template=[], serving=[], traffic="old")
        result = self.run_sync()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("services update ", self.calls())
        self.assertIn("services update-traffic", self.calls())
        self.assertIn("serving on ready", result.stdout)

    def test_control_plane_refuses_wrong_builder_record_and_bad_live_manifest(self):
        self.prepare_sync()
        self.git("push", "origin", "HEAD:main")
        (self.root / "tar-sha").write_text("v1.2.3 " + "a" * 64 + "\n")
        result = self.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("gcloud ", self.calls())
        (self.root / "tar-sha").write_text("v1.2.4 " + "a" * 64 + "\n")
        self.cp_state["template"][1]["value"] = "invalid json"
        result = self.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("services update", self.calls())

    def test_already_merged_checks_fail_closed(self):
        self.git("push", "origin", "HEAD:main")
        result = self.run_release("ship", "1.2.4", CHECKS_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("merged bump PR required checks", result.stdout)
        self.assertNotIn("gh run list", self.calls())

    def test_already_merged_admin_warns(self):
        self.git("push", "origin", "HEAD:main")
        result = self.run_release("ship", "1.2.4", "--admin", CHECKS_FAIL="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("WARNING: --admin", result.stdout)
        self.assertNotIn("gh pr checks", self.calls())

    def test_no_checks_skips_all_checks(self):
        result = self.run_release("ship", "1.2.4", "--admin", "--no-checks", CHECKS_FAIL="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("SKIPS ALL CHECKS", result.stdout)
        self.assertNotIn("gh pr checks", self.calls())

    def test_admin_rust_failure_and_timeout_refuse_merge(self):
        for buckets in (["fail"], ["cancel"], ["pending"]):
            with self.subTest(buckets=buckets):
                self.settings["rust_buckets"] = buckets
                if buckets == ["pending"]:
                    self.tool("sleep", 'from pathlib import Path\np=Path("clock"); p.write_text(str(int(p.read_text())+5400))\n')
                result = self.run_release("ship", "1.2.4", "--admin")
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("gh pr merge", self.calls())

    def test_stale_sha_handoff_refused(self):
        self.prepare_sync()
        self.git("push", "origin", "HEAD:main")
        self.cp_state["remote_sha"] = "b" * 64
        result = self.run_sync()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("differs from remote sidecar", result.stdout)
        self.assertNotIn("services update", self.calls())

    def test_concurrent_revision_refused(self):
        self.prepare_sync()
        self.git("push", "origin", "HEAD:main")
        for change in ({"created": "concurrent"}, {"image": "image:concurrent"}):
            with self.subTest(change=change):
                self.cp_state = dict(template=[], serving=[], traffic="old", **change)
                result = self.run_sync()
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("concurrent deploy detected", result.stdout)
                self.assertNotIn("services update-traffic", self.calls())

    def test_stray_local_tag_excluded(self):
        self.prepare_sync()
        self.git("push", "origin", "HEAD:main")
        self.git("tag", "v99.0.0")
        result = self.run_sync()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertNotIn("v99.0.0", self.calls())

    def test_verify_named_assets_platform_keys_and_urls(self):
        for key, value, row in (("missing_asset", "duduclaw-linux-x64.tar.gz.minisig", "GitHub Release"),
                                ("missing_platform", "darwin-aarch64", "desktop latest.json"),
                                ("desktop_url", "https://example/desktop-v1.2.3/file", "desktop latest.json")):
            with self.subTest(key=key):
                self.settings[key] = value
                result = self.run_release("verify", "1.2.4")
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(row + " MISMATCH", result.stdout)
                del self.settings[key]

    def test_verify_manifest_sha_matches_sidecar(self):
        self.prepare_sync()
        self.tool("rustc", 'print("host: unknown")')
        build = self.root / "commercial/duduclaw-pro-gateway/build-image.sh"
        build.write_text("#!/bin/sh\n"); build.chmod(0o755)
        for sha, expected in (("a" * 64, 0), ("b" * 64, 1)):
            self.cp_state["remote_sha"] = sha
            (self.root / "cp-state").write_text(json.dumps(self.cp_state))
            result = self.run_release("verify", "1.2.4", DUDUCLAW_GCP_PROJECT="test", DUDUCLAW_RELEASE_WITHOUT_PRO="0")
            self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
            self.assertIn("pro-manifest", result.stdout)

    def test_registry_eventually_converges(self):
        self.tool("curl", r'''import json,sys
from pathlib import Path
print(json.dumps({"version":"1.2.4" if int(Path("clock").read_text()) >= 60 else "1.2.3"}))
''')
        result = self.run_release("verify", "1.2.4")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((self.root / "clock").read_text(), "60")

    def test_commercial_push_and_pr_are_resumed_without_duplicate(self):
        private = self.root / "commercial"
        private.mkdir()
        def git(*args):
            return subprocess.check_output(["git", "-C", str(private), *args], stderr=subprocess.DEVNULL).decode().strip()
        git("init", "-b", "main")
        git("config", "user.name", "Fixture")
        git("config", "user.email", "fixture@example.invalid")
        git("commit", "--allow-empty", "-m", "base")
        git("init", "--bare", str(self.root / "private.git"))
        git("remote", "add", "origin", str(self.root / "private.git"))
        git("push", "origin", "main")
        git("checkout", "-b", "chore/private-release")
        git("commit", "--allow-empty", "-m", "locks")
        # Private stub maintains its own PR state, exactly as the public stub does.
        (private / "settings.json").write_text(json.dumps(self.settings))
        result = self.run_release("ship", "1.2.4", "--no-merge")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("refs/heads/chore/private-release", git("ls-remote", "origin"))
        calls = (private / "calls").read_text()
        self.assertEqual(calls.count("gh pr create"), 1)
        result = self.run_release("ship", "1.2.4", "--no-merge")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual((private / "calls").read_text().count("gh pr create"), 1)


if __name__ == "__main__":
    unittest.main()
