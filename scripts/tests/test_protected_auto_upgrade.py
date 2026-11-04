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
        self.floor = self.work / "version-floor"
        self.floor.write_text("1.79.3")
        self.pending = self.work / "version-pending"
        self.response = self.work / "api.json"
        self.calls = self.work / "calls"
        self.uid = self.work / "uid"
        self.uid.write_text("0")
        self.network_failure = self.work / "network-failure"
        self.stub("id", f"cat {shlex.quote(str(self.uid))}\n")
        self.stub("curl", f"[ ! -e {shlex.quote(str(self.network_failure))} ] || exit 22\n"
                  f"cat {shlex.quote(str(self.response))}\n")
        binary = self.stub("duduclaw", f"printf '%s\\n' \"$@\" > {shlex.quote(str(self.calls))}\n")
        text = (SOURCE / "duduclaw-auto-upgrade").read_text()
        # Rewrite fixed constants in a private copy, never production env overrides.
        text = text.replace("PATH=/usr/sbin:/usr/bin:/sbin:/bin",
                            f"PATH={shlex.quote(str(self.work))}:/usr/bin:/bin")
        for old, new in (("/var/lib/duduclaw-protected/version-floor", self.floor),
                         ("/var/lib/duduclaw-protected/version-pending", self.pending),
                         ("/opt/duduclaw/bin/duduclaw", binary)):
            text = text.replace(old, str(new))
        text = text.replace("/tmp/duduclaw-auto-upgrade.XXXXXXXXXX",
                            str(self.work / "api-temp.XXXXXXXXXX"))
        self.script = self.work / "helper"
        self.script.write_text(text)
        self.releases("v1.80.0")

    def stub(self, name, body):
        path = self.work / name
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o755)
        return path

    def releases(self, *tags, pretty=False):
        self.response.write_text(json.dumps([
            {"url": f"https://api.github.com/repos/zhixuli0406/DuDuClaw/releases/{i}",
             "tag_name": tag, "name": f"Release {tag}", "draft": False,
             "prerelease": False,
             "body": 'Notes: "quoted" text; fake "tag_name":"v99.0.0"; \\ paths'}
            for i, tag in enumerate(tags)
        ], indent=2 if pretty else None, separators=None if pretty else (",", ":")))

    def test_primary_fixture_is_minified_without_final_newline(self):
        raw = self.response.read_bytes()
        self.assertNotIn(b"\n", raw)
        self.assertTrue(raw.startswith(b'[{"url":'))
        self.assertTrue(raw.endswith(b"}]"))
        self.assertIn(b'\\"tag_name\\":\\"v99.0.0\\"', raw)

    def test_pretty_printed_response(self):
        self.releases("v1.80.0", pretty=True)
        result = self.run_helper("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--version 1.80.0", result.stdout)

    def test_whitespace_around_array_and_split_field(self):
        self.response.write_text(' \n\t[ {"tag_name"\n: \n"v1.80.0"} ] \n\t')
        result = self.run_helper("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--version 1.80.0", result.stdout)

    def run_helper(self, *args):
        return subprocess.run(["sh", str(self.script), *args], text=True,
                              capture_output=True)

    def assert_refused(self):
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.calls.exists())

    def test_newer_passes_explicit_version(self):
        self.releases("v1.80.0", "v1.100.0", "v1.9.0")
        result = self.run_helper()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.calls.read_text().splitlines(),
                         ["service", "upgrade", "--protected-linux", "--version", "1.100.0"])

    def test_dry_run(self):
        result = self.run_helper("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("would upgrade", result.stdout)
        self.assertIn("--version 1.80.0", result.stdout)
        self.assertFalse(self.calls.exists())

    def test_equal_or_older(self):
        for tag in ("v1.79.3", "v1.79.2"):
            with self.subTest(tag=tag):
                self.releases(tag)
                result = self.run_helper()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("already up to date", result.stdout)
                self.assertFalse(self.calls.exists())

    def test_ignores_desktop_and_malformed_tags(self):
        self.releases("desktop-v99.0.0", "v2.0", "v2.0.0-rc.1", "2.0.0",
                      "v2.0.0;id", "v2.0.0extra", "v2.0.0.1", "v1.80.0")
        result = self.run_helper("--dry-run")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("--version 1.80.0", result.stdout)

    def test_missing_floor(self):
        self.floor.unlink()
        self.assert_refused()

    def test_invalid_floor(self):
        for value in ("", "v1.79.3", "1.79.3\n", "1.79.3\n2.0.0", "1.79.3 ", "bad", "1.79.3\x00"):
            with self.subTest(value=value):
                self.floor.write_text(value)
                self.assert_refused()

    def test_pending_requires_manual_attention(self):
        self.pending.touch()
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("manual attention", result.stderr)
        self.assertFalse(self.calls.exists())

    def test_non_root(self):
        self.uid.write_text("1000")
        self.assert_refused()

    def test_network_failure_even_with_partial_response(self):
        self.network_failure.touch()
        self.assert_refused()

    def test_empty_or_unparseable_response(self):
        for value in ("[]", "not JSON", '{"message": "rate limited"}'):
            self.response.write_text(value)
            self.assert_refused()

    def test_truncated_response_with_valid_tag(self):
        for value in ('[{"tag_name":"v1.80.0"}', '[\n{\n"tag_name": "v1.80.0",\n'):
            with self.subTest(value=value):
                self.response.write_text(value)
                self.assert_refused()

    def test_non_array_response_with_valid_tag(self):
        self.response.write_text(json.dumps({"tag_name": "v1.80.0"}, indent=2))
        self.assert_refused()

    def test_body_mentions_do_not_supply_tags(self):
        for pretty in (False, True):
            with self.subTest(pretty=pretty):
                self.releases("desktop-v9.0.0", pretty=pretty)
                self.assert_refused()

    def test_documented_prerelease_flag_policy(self):
        self.releases("v1.80.0")
        releases = json.loads(self.response.read_text())
        releases[0]["prerelease"] = True
        self.response.write_text(json.dumps(releases, separators=(",", ":")))
        self.assertEqual(self.run_helper("--dry-run").returncode, 0)

    def test_response_size_cap_and_cleanup(self):
        # The curl stub deliberately ignores Content-Length/max-filesize.
        self.response.write_bytes(b" " * 8388609)
        result = self.run_helper()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("exceeds 8 MiB", result.stderr)
        self.assertEqual(list(self.work.glob("api-temp.*")), [])
        self.assertFalse(self.calls.exists())

    def test_upgrade_failure_propagates(self):
        self.stub("duduclaw", "exit 42\n")
        self.assertEqual(self.run_helper().returncode, 42)

    def test_invalid_arguments(self):
        self.assertNotEqual(self.run_helper("--version", "2.0.0").returncode, 0)


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
