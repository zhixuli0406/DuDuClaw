"""Exercise the root helper in isolation; never invoke the real upgrade."""

import json
from pathlib import Path
import shlex
import shutil
import subprocess
import tempfile
import sys
import unittest

ROOT = Path(__file__).resolve().parents[2]
SOURCE = ROOT / "scripts/linux-protected/auto-upgrade"


class AutoUpgradeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.calls = self.work / "calls"
        self.uid = self.work / "uid"
        self.uid.write_text("0")
        self.stub("id", f"cat {shlex.quote(str(self.uid))}\n")
        self.updater = self.stub("updated", f"printf '%s\\n' \"$@\" > {shlex.quote(str(self.calls))}\n")
        text = (SOURCE / "duduclaw-auto-upgrade").read_text()
        text = text.replace("PATH=/usr/sbin:/usr/bin:/sbin:/bin", f"PATH={shlex.quote(str(self.work))}:/usr/bin:/bin")
        text = text.replace("/usr/libexec/duduclaw-updated-launch", str(self.updater))
        self.script = self.work / "helper"
        self.script.write_text(text)

    def stub(self, name, body):
        path = self.work / name
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o755)
        return path

    def run_helper(self, *args):
        return subprocess.run(["sh", str(self.script), *args], text=True, capture_output=True)

    def test_requests_daemon_candidate_discovery(self):
        self.assertEqual(self.run_helper().returncode, 0)
        self.assertEqual(self.calls.read_text().splitlines(), ["--check-latest"])

    def test_dry_run_remains_read_only(self):
        self.assertEqual(self.run_helper("--dry-run").returncode, 0)
        self.assertEqual(self.calls.read_text().splitlines(), ["--check-latest", "--dry-run"])

    def test_non_root_and_authority_arguments_refused(self):
        for args in [("--version", "1.80.0"), ("--url", "https://evil.example"), ("--dry-run", "extra")]:
            self.assertNotEqual(self.run_helper(*args).returncode, 0)
            self.assertFalse(self.calls.exists())
        self.uid.write_text("1000")
        self.assertNotEqual(self.run_helper().returncode, 0)
        self.assertFalse(self.calls.exists())

    def test_updater_failure_propagates_without_legacy_fallback(self):
        self.stub("updated", "exit 42\n")
        self.assertEqual(self.run_helper().returncode, 42)
        self.updater.unlink()
        self.assertNotEqual(self.run_helper().returncode, 0)


class InstallerTrustTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name).resolve()
        self.tree = self.work / "source"
        shutil.copytree(SOURCE, self.tree)
        self.tree.chmod(0o755)
        for path in self.tree.iterdir():
            path.chmod(0o644)
        self.bin = self.work / "bin"
        self.bin.mkdir()
        self.owner = self.work / "owner"
        self.owner.write_text("0:0")
        self.bad_ancestor = self.work / "bad-ancestor"
        # Simulate Linux root ownership and safe host ancestors without root.
        # Modes inside the fixture tree remain real, including symlink tests.
        stat = self.bin / "stat"
        stat.write_text("#!" + sys.executable + "\n" +
                        "import os, stat, sys\n" +
                        f"tree = {str(self.tree)!r}\n" +
                        f"owner = open({str(self.owner)!r}).read()\n" +
                        "path = sys.argv[-1]\n" +
                        "mode = stat.S_IMODE(os.lstat(path).st_mode) if path == tree or path.startswith(tree + '/') else 0o755\n" +
                        f"if os.path.exists({str(self.bad_ancestor)!r}) and path == {str(self.work)!r}: mode = 0o1777\n" +
                        "print('{}:{:o}'.format(owner if path.startswith(tree) else '0:0', mode))\n")
        stat.chmod(0o755)
        (self.bin / "id").write_text("#!/bin/sh\nprintf '0\\n'\n")
        (self.bin / "id").chmod(0o755)
        text = (SOURCE / "install.sh").read_text().replace(
            "PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            f"PATH={shlex.quote(str(self.bin))}:/usr/bin:/bin")
        self.script = self.work / "installer"
        self.script.write_text(text)

    def run_check(self, tree=None):
        return subprocess.run(["sh", str(self.script), "--check-source", str(tree or self.tree)],
                              text=True, capture_output=True)

    def assert_refused(self, reason):
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(reason, result.stderr)
        self.assertIn("cp -r --no-preserve=ownership", result.stderr)

    def test_good_tree_accepted(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_symlink_file_refused(self):
        path = self.tree / "duduclaw-auto-upgrade"
        path.unlink()
        path.symlink_to(SOURCE / path.name)
        self.assert_refused("symlink")

    def test_group_writable_file_refused(self):
        (self.tree / "duduclaw-auto-upgrade.service").chmod(0o664)
        self.assert_refused("group/other writable")

    def test_non_root_owner_refused(self):
        for owner in ("1000:0", "0:1000"):
            with self.subTest(owner=owner):
                self.owner.write_text(owner)
                self.assert_refused("root:root")

    def test_writable_sticky_ancestor_refused(self):
        self.bad_ancestor.touch()
        self.assert_refused("group/other writable")

    def test_symlink_directory_refused(self):
        link = self.work / "link"
        link.symlink_to(self.tree, target_is_directory=True)
        result = self.run_check(link)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("symlink directory", result.stderr)

    def test_non_regular_file_refused(self):
        path = self.tree / "duduclaw-auto-upgrade.timer"
        path.unlink()
        path.mkdir()
        self.assert_refused("not a regular file")


class ArtifactTests(unittest.TestCase):
    def test_shell_syntax(self):
        for name in ("duduclaw-auto-upgrade", "install.sh"):
            with self.subTest(name=name):
                subprocess.run(["sh", "-n", str(SOURCE / name)], check=True)

    @unittest.skipUnless(shutil.which("systemd-analyze"),
                         "systemd-analyze unavailable (e.g. macOS); Linux unit verification skipped")
    def test_systemd_units(self):
        with tempfile.TemporaryDirectory() as temp:
            # Only substitute ExecStart: the helper is intentionally not installed
            # on CI hosts. Verify all other service/timer directives unchanged.
            for name in ("duduclaw-auto-upgrade.service", "duduclaw-auto-upgrade.timer"):
                text = (SOURCE / name).read_text().replace(
                    "/usr/local/sbin/duduclaw-auto-upgrade", shutil.which("true"))
                (Path(temp) / name).write_text(text)
            result = subprocess.run([
                "systemd-analyze", "verify", str(Path(temp) / "duduclaw-auto-upgrade.service"),
                str(Path(temp) / "duduclaw-auto-upgrade.timer")
            ], text=True, capture_output=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
