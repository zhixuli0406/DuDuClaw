"""Exercise the root helper in isolation; never invoke the real upgrade."""

import json
import os
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


class InstallerPrerequisiteTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.source = self.root / "source"
        shutil.copytree(SOURCE, self.source)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.calls = self.root / "calls"
        self.owner = self.root / "owner"
        self.owner.write_text("0:0")
        self.state = self.root / "state"
        self.state.write_text("active")
        self.launcher = self.root / "usr/libexec/duduclaw-updated-launch"
        self.launcher.parent.mkdir(parents=True)
        self.launcher.write_text("#!/bin/sh\nexit 99\n")
        self.launcher.chmod(0o755)
        gateway = self.root / "opt/duduclaw/bin/duduclaw"
        gateway.parent.mkdir(parents=True)
        gateway.write_text("#!/bin/sh\nexit 99\n")
        gateway.chmod(0o755)
        (self.root / "run").mkdir()
        self.stub("id", "printf '0\\n'\n")
        stat_script = ("#!" + sys.executable + "\nimport os, stat, sys\n"
            f"root = {str(self.root)!r}\nlauncher = {str(self.launcher)!r}\n"
            "path = sys.argv[-1]\ninfo = os.lstat(path)\n"
            "format = sys.argv[2]\n"
            "if format == '%h': print(info.st_nlink)\n"
            "elif format == '%s': print(info.st_size)\n"
            "else:\n"
            "    mode = stat.S_IMODE(info.st_mode) if path == root or path.startswith(root + '/') else 0o755\n"
            f"    owner = open({str(self.owner)!r}).read() if path == launcher else '0:0'\n"
            "    print('{}:{:o}'.format(owner, mode))\n")
        (self.bin / "stat").write_text(stat_script)
        (self.bin / "stat").chmod(0o755)
        self.stub("systemctl", f"printf 'systemctl %s\\n' \"$*\" >> {shlex.quote(str(self.calls))}\n"
            f"if [ \"$1\" = show ]; then cat {shlex.quote(str(self.state))}; fi\n")
        self.stub("install", f"printf 'install %s\\n' \"$*\" >> {shlex.quote(str(self.calls))}\n")
        self.script = self.source / "install.sh"
        text = self.script.read_text().replace("PATH=/usr/sbin:/usr/bin:/sbin:/bin",
            f"PATH={shlex.quote(str(self.bin))}:/usr/bin:/bin")
        for prefix in ("/usr/libexec/", "/opt/duduclaw/", "/usr/local/sbin/", "/etc/systemd/", "/run"):
            text = text.replace(prefix, str(self.root) + prefix)
        self.script.write_text(text)

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o755)

    def run_install(self):
        return subprocess.run(["sh", str(self.script)], text=True, capture_output=True)

    def assert_blocked_before_install(self):
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text() if self.calls.exists() else ""
        self.assertNotIn("install ", calls)
        self.assertNotIn("enable", calls)
        self.assertNotIn("--check-latest", calls)
        return result

    def test_missing_launcher_blocks_before_any_install(self):
        self.launcher.unlink()
        self.assertIn("Unsafe protected updater", self.assert_blocked_before_install().stderr)

    def test_nonexecutable_launcher_blocks(self):
        self.launcher.chmod(0o644)
        self.assertIn("not executable", self.assert_blocked_before_install().stderr)

    def test_untrusted_owner_and_mode_block(self):
        self.owner.write_text("1000:0")
        self.assertIn("root:root", self.assert_blocked_before_install().stderr)
        self.owner.write_text("0:0")
        self.launcher.chmod(0o775)
        self.assertIn("group/other writable", self.assert_blocked_before_install().stderr)

    def test_symlink_and_hardlink_launchers_block(self):
        target = self.launcher.with_name("real")
        self.launcher.rename(target)
        self.launcher.symlink_to(target)
        self.assertIn("symlink", self.assert_blocked_before_install().stderr)
        self.launcher.unlink()
        os.link(target, self.launcher)
        self.assertIn("exactly one link", self.assert_blocked_before_install().stderr)

    def test_writable_or_symlink_ancestor_blocks(self):
        self.launcher.parent.chmod(0o777)
        self.assertIn("group/other writable", self.assert_blocked_before_install().stderr)
        self.launcher.parent.chmod(0o755)
        real = self.root / "real-libexec"
        self.launcher.parent.rename(real)
        self.launcher.parent.symlink_to(real, target_is_directory=True)
        self.assertIn("symlink", self.assert_blocked_before_install().stderr)

    def test_service_missing_inactive_or_unknown_blocks(self):
        for state in ("inactive", "failed", "activating", "", "active\nunknown"):
            with self.subTest(state=state):
                self.state.write_text(state)
                self.assertIn("must be active", self.assert_blocked_before_install().stderr)
        self.stub("systemctl", "exit 1\n")
        self.assertIn("Cannot read", self.assert_blocked_before_install().stderr)

    def test_active_trusted_updater_allows_timer_without_requesting_upgrade(self):
        result = self.run_install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls.read_text().splitlines()
        self.assertEqual(calls[0], "systemctl show -p ActiveState --value duduclaw-updated.service")
        self.assertEqual(calls[-2:], ["systemctl show -p ActiveState --value duduclaw-updated.service",
                                    "systemctl enable --now duduclaw-auto-upgrade.timer"])
        self.assertFalse(any("--check-latest" in call for call in calls))

    def test_service_stopping_during_install_prevents_timer_enable(self):
        self.stub("systemctl", f"printf 'systemctl %s\\n' \"$*\" >> {shlex.quote(str(self.calls))}\n"
            f"if [ \"$1\" = show ]; then cat {shlex.quote(str(self.state))}; fi\n"
            f"if [ \"$1\" = daemon-reload ]; then printf inactive > {shlex.quote(str(self.state))}; fi\n")
        result = self.run_install()
        self.assertNotEqual(result.returncode, 0)
        calls = self.calls.read_text()
        self.assertIn("install ", calls)
        self.assertNotIn("enable", calls)
        self.assertNotIn("--check-latest", calls)


if __name__ == "__main__":
    unittest.main()
