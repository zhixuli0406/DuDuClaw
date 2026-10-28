"""Run release dry-run paths with unavailable GitHub and prove no files change."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ReleaseLaneTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "scripts").mkdir()
        for name in ("release.sh", "release_platform_ci.py"):
            shutil.copyfile(ROOT / "scripts" / name, self.root / "scripts" / name)
        (self.root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
        self.bin = self.root / "bin"
        self.bin.mkdir()
        gh = self.bin / "gh"
        gh.write_text("#!/bin/sh\nexit 1\n")
        gh.chmod(0o755)

    def snapshot(self):
        return {str(path.relative_to(self.root)): path.read_bytes()
                for path in self.root.rglob("*") if path.is_file()}

    def run_lane(self, lane, **overrides):
        before = self.snapshot()
        env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                   DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI="", CLOUD_BUILD="1")
        env.update(overrides)
        result = subprocess.run(["bash", "scripts/release.sh", lane, "--dry-run"],
                                cwd=self.root, env=env, text=True, capture_output=True)
        self.assertEqual(before, self.snapshot(), "dry-run mutated files")
        return result

    def test_hotfix_warns_but_plans_all_channels_without_audit(self):
        result = self.run_lane("hotfix")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1.2.3 -> 1.2.4", result.stdout)
        self.assertIn("hotfix continues", result.stdout)
        self.assertIn("ALL channels", result.stdout)
        self.assertIn("CLOUD_BUILD=1", result.stdout)
        self.assertNotIn("Platform version audit", result.stdout)

    def test_patch_alias_notice_and_local_build_override(self):
        result = self.run_lane("patch", CLOUD_BUILD="0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("NOTICE: patch is deprecated", result.stdout)
        self.assertIn("Lane: hotfix", result.stdout)
        self.assertIn("CLOUD_BUILD=0", result.stdout)

    def test_minor_blocks_when_github_is_unavailable(self):
        result = self.run_lane("minor")
        self.assertEqual(result.returncode, 1)
        self.assertIn("1.2.3 -> 1.3.0", result.stdout)
        self.assertIn("Platform version audit", result.stdout)
        self.assertIn("BLOCKED", result.stderr)

    def test_major_override_is_read_only_and_explicit(self):
        result = self.run_lane("major", DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI="incident reviewed")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("1.2.3 -> 2.0.0", result.stdout)
        self.assertIn("incident reviewed", result.stdout)


class SignerIntegrityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.private = self.root / "commercial"
        self.private.mkdir()
        self.remote = self.root / "remote.git"
        self.git(self.root, "init", "--bare", str(self.remote))
        self.git(self.private, "init", "-b", "main")
        self.git(self.private, "config", "user.name", "Fixture")
        self.git(self.private, "config", "user.email", "fixture@example.invalid")
        (self.private / "signer.py").write_text("synthetic signer")
        self.git(self.private, "add", ".")
        self.git(self.private, "commit", "-m", "merged signer")
        self.git(self.private, "remote", "add", "origin", str(self.remote))
        self.git(self.private, "push", "origin", "main")
        body = (ROOT / "scripts/release.sh").read_text()
        start = body.index('    commercial_git() (')
        end = body.index('    HOTFIX_OUTPUT=', start)
        self.gate_body = "set -e\n" + body[start:end] + "echo SIGNER_ALLOWED\n"

    def git(self, root, *args):
        return subprocess.check_output(["git", "-C", str(root), *args], stderr=subprocess.DEVNULL)

    def gate(self, **overrides):
        return subprocess.run(["bash", "-c", self.gate_body], cwd=self.root,
                              capture_output=True, text=True, env=dict(os.environ, **overrides))

    def test_clean_merged_signer_allowed(self):
        result = self.gate()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("SIGNER_ALLOWED", result.stdout)

    def test_tracked_and_untracked_changes_refused(self):
        for name in ("signer.py", "untracked.py"):
            path = self.private / name
            path.write_text("unreviewed bytes")
            result = self.gate()
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("must be clean", result.stdout)
            self.assertNotIn("SIGNER_ALLOWED", result.stdout)
            if name == "signer.py":
                self.git(self.private, "restore", name)
            else:
                path.unlink()

    def test_unmerged_signer_and_failed_fetch_refused(self):
        (self.private / "signer.py").write_text("unmerged signer")
        self.git(self.private, "add", ".")
        self.git(self.private, "commit", "-m", "unmerged signer")
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has not been merged", result.stdout)
        self.git(self.private, "remote", "set-url", "origin", str(self.root / "missing.git"))
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("cannot fetch", result.stdout)

    def unmerged_signer(self):
        (self.private / "signer.py").write_text("unmerged signer")
        self.git(self.private, "add", ".")
        self.git(self.private, "commit", "-m", "unmerged signer")

    def test_replace_graph_cannot_certify_unmerged_signer(self):
        self.unmerged_signer()
        main = self.git(self.private, "rev-parse", "origin/main").decode().strip()
        tree = self.git(self.private, "rev-parse", "origin/main^{tree}").decode().strip()
        replacement = self.git(self.private, "commit-tree", tree, "-p", "HEAD", "-m", "fake ancestry").decode().strip()
        self.git(self.private, "replace", main, replacement)
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has not been merged", result.stdout)
        self.assertNotIn("SIGNER_ALLOWED", result.stdout)

    def test_fetch_refspec_cannot_leave_forged_main_tracking_ref(self):
        self.unmerged_signer()
        self.git(self.private, "config", "--unset-all", "remote.origin.fetch")
        self.git(self.private, "update-ref", "refs/remotes/origin/main", "HEAD")
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("has not been merged", result.stdout)
        self.assertNotIn("SIGNER_ALLOWED", result.stdout)

    def test_inherited_git_index_cannot_hide_dirty_signer(self):
        index = self.root / "forged-index"
        env = dict(os.environ, GIT_INDEX_FILE=str(index))
        (self.private / "signer.py").write_text("unreviewed signer")
        subprocess.run(["git", "-C", str(self.private), "add", "."], env=env,
                       check=True, capture_output=True)
        result = self.gate(GIT_INDEX_FILE=str(index))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must be clean", result.stdout)

    def test_parent_public_repo_cannot_impersonate_private_checkout(self):
        # Keep the real private checkout aside, leaving a plain commercial folder.
        self.private.rename(self.root / "private-backup")
        self.private.mkdir()
        self.git(self.root, "init", "-b", "main")
        result = self.gate()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("separate private Git checkout", result.stdout)


if __name__ == "__main__":
    unittest.main()
