"""Exercise release platform selection and fail-closed lane behavior offline."""
import datetime as dt
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("platform_ci", ROOT / "scripts/release_platform_ci.py")
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)


class PlatformCiTests(unittest.TestCase):
    def run_info(self, sha="a", event="push", date="2026-10-28T10:00:00Z", ident=1,
                 conclusion="success", status="completed"):
        return dict(headSha=sha * 40, event=event, createdAt=date, databaseId=ident,
                    conclusion=conclusion, status=status)

    def check(self):
        return ci.check(ROOT, now=dt.datetime(2026, 10, 28, 12, tzinfo=dt.timezone.utc))

    def query(self, runs, conclusions=("success", "success"), behind=0, unrelated=()):
        def fake(*args):
            if args[:3] == ("gh", "run", "list"):
                self.assertNotIn("--status", args)
                self.assertIn("status", args[args.index("--json") + 1].split(","))
                self.assertIn("conclusion", args[args.index("--json") + 1].split(","))
                return json.dumps(runs)
            if args[:3] == ("gh", "run", "view"):
                return json.dumps({"jobs": [dict(name=name, conclusion=result)
                                           for name, result in zip(ci.PLATFORM_JOBS, conclusions)]})
            if "ls-remote" in args:
                return "a" * 40 + "\trefs/heads/main\n"
            if "merge-base" in args:
                if args[-2] in unrelated:
                    raise subprocess.CalledProcessError(1, args)
                return ""
            if "rev-list" in args:
                return str(behind)
            self.fail(str(args))
        return patch.object(ci, "command", side_effect=fake)

    def test_success_and_boundary(self):
        with self.query([self.run_info()], behind=3):
            self.assertIn("3 commits behind", self.check())

    def test_stale_coverage_blocks(self):
        with self.query([self.run_info()], behind=4):
            with self.assertRaisesRegex(ValueError, "maximum 3"):
                self.check()

    def test_newest_running_blocks(self):
        for status in ("queued", "in_progress", "waiting", "pending", "requested"):
            runs = [self.run_info(date="2026-10-27T12:00:00Z", ident=2),
                    self.run_info(ident=3, status=status, conclusion=None)]
            with self.subTest(status=status), self.query(runs):
                with self.assertRaisesRegex(ValueError, "CI still running on .*; wait"):
                    self.check()

    def test_manually_cancelled_or_skipped_newest_blocks(self):
        for conclusion in ("cancelled", "skipped"):
            runs = [self.run_info(date="2026-10-27T12:00:00Z", ident=2),
                    self.run_info(ident=3, conclusion=conclusion)]
            with self.subTest(conclusion=conclusion), self.query(runs):
                with self.assertRaisesRegex(ValueError, "run 3.*no newer main commit"):
                    self.check()

    def test_superseded_cancelled_run_is_skipped_for_newer_commit(self):
        runs = [self.run_info(sha="b", date="2026-10-27T12:00:00Z", ident=2,
                              conclusion="cancelled"), self.run_info(ident=3)]
        with self.query(runs):
            self.assertIn("run 3", self.check())

    def test_stale_and_future_ci_blocks(self):
        for date in ("2026-10-26T23:59:59Z", "2026-10-28T12:00:01Z"):
            with self.subTest(date=date), self.query([self.run_info(date=date)]):
                with self.assertRaisesRegex(ValueError, "maximum age 36 hours"):
                    self.check()
        with self.query([self.run_info(date="2026-10-27T00:00:00Z")]):
            self.check()

    def test_release_head_missing_remote_main_tip_blocks(self):
        with self.query([self.run_info()], unrelated=("a" * 40,)):
            with self.assertRaisesRegex(ValueError, "HEAD is missing remote main tip"):
                self.check()

    def test_failed_run_then_older_green_blocks_even_with_green_platform_jobs(self):
        for conclusion in ("failure", "timed_out", "action_required", "startup_failure"):
            runs = [self.run_info(date="2026-10-27T12:00:00Z", ident=2),
                    self.run_info(ident=3, conclusion=conclusion)]
            with self.subTest(conclusion=conclusion), self.query(runs) as query:
                with self.assertRaisesRegex(ValueError, "run 3.*" + conclusion):
                    self.check()
                self.assertFalse(any(call.args[:3] == ("gh", "run", "view")
                                     for call in query.call_args_list))

    def test_latest_red_never_falls_back_to_green(self):
        runs = [self.run_info(date="2026-10-27T12:00:00Z", ident=2), self.run_info(ident=3)]
        with self.query(runs, conclusions=("success", "failure")):
            with self.assertRaisesRegex(ValueError, "run 3.*windows-latest=failure"):
                self.check()

    def test_stale_local_main_cannot_hide_newer_failed_main_run(self):
        runs = [self.run_info(date="2026-10-27T12:00:00Z", ident=2), self.run_info(sha="b", ident=3)]
        with self.query(runs, conclusions=("success", "failure")) as query:
            with self.assertRaisesRegex(ValueError, "run 3.*failure"):
                self.check()
            # GitHub's --branch main supplies branch identity. An outdated
            # local tracking ref must not filter out newer ancestor evidence.
            self.assertFalse(any("origin/main" in call.args for call in query.call_args_list))

    def test_missing_cancelled_skipped_platform_blocks(self):
        for results in (("success",), ("cancelled", "success"), ("success", "skipped")):
            with self.subTest(results=results), self.query([self.run_info()], conclusions=results):
                with self.assertRaises(ValueError):
                    self.check()

    def test_manual_or_unrelated_runs_are_not_release_evidence(self):
        runs = [self.run_info(event="workflow_dispatch"), self.run_info(sha="b"),
                self.run_info(event="schedule", date="2026-10-27T12:00:00Z", ident=4)]
        with self.query(runs, unrelated=("b" * 40,)):
            self.assertIn("run 4", self.check())

    def test_absent_evidence_blocks(self):
        with self.query([]):
            with self.assertRaisesRegex(ValueError, "no push/schedule"):
                self.check()

    def test_missing_gh_minor_blocks_hotfix_warns_override_records_reason(self):
        cases = (("minor", "", True), ("major", "  ", True),
                 ("hotfix", "", False), ("minor", "owner reviewed incident", False))
        for lane, override, blocked in cases:
            with self.subTest(lane=lane, override=override), \
                    patch.object(ci, "check", side_effect=FileNotFoundError()), \
                    patch("sys.argv", ["preflight", "--lane", lane]), \
                    patch.dict(os.environ, {"DUDUCLAW_RELEASE_IGNORE_PLATFORM_CI": override}), \
                    patch("builtins.print") as output:
                if blocked:
                    with self.assertRaises(SystemExit) as error:
                        ci.main()
                    self.assertEqual(error.exception.code, 1)
                else:
                    ci.main()
                    self.assertIn(override or "hotfix continues", str(output.call_args))


class RealGitEvidenceTests(unittest.TestCase):
    def test_replace_and_inherited_git_env_cannot_hide_missing_remote_main(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)

            def git(*args):
                return subprocess.check_output(["git", "-C", str(root), *args],
                                               stderr=subprocess.DEVNULL).decode().strip()

            git("init", "-b", "main")
            git("config", "user.name", "Fixture")
            git("config", "user.email", "fixture@example.invalid")
            (root / "file").write_text("base")
            git("add", ".")
            git("commit", "-m", "base")
            base = git("rev-parse", "HEAD")
            (root / "file").write_text("remote main")
            git("commit", "-am", "main")
            tip = git("rev-parse", "HEAD")
            git("checkout", "-b", "release", base)
            (root / "file").write_text("release missing main")
            git("commit", "-am", "release")
            head = git("rev-parse", "HEAD")
            tree = git("rev-parse", "HEAD^{tree}")
            forged = git("commit-tree", tree, "-p", tip, "-m", "fake ancestry")
            git("replace", head, forged)
            original = ci.command

            def query(*args):
                if "ls-remote" in args:
                    return tip + "\trefs/heads/main\n"
                if args[0] == "gh":
                    self.fail("must block before considering CI evidence")
                return original(*args)

            with patch.object(ci, "command", side_effect=query):
                with self.assertRaisesRegex(ValueError, "HEAD is missing remote main tip"):
                    ci.check(root)
            git("replace", "-d", head)
            with patch.object(ci, "command", side_effect=query), \
                    patch.dict(os.environ, {"GIT_DIR": str(root / "absent.git")}):
                with self.assertRaisesRegex(ValueError, "HEAD is missing remote main tip"):
                    ci.check(root)


if __name__ == "__main__":
    unittest.main()
