"""Exercise the console-manuals preflight and publish blocks of release.sh."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RELEASE = (ROOT / "scripts/release.sh").read_text()


def block(start, end):
    begin = RELEASE.index(start)
    return RELEASE[begin:RELEASE.index(end, begin)]


PREFLIGHT_MANUALS = block(
    '    if [[ "${DUDUCLAW_SKIP_MANUALS:-0}" != "1" ]]; then\n        MANUALS_SUMMARY=',
    '    if [[ ${#PRO_MISSING[@]} -gt 0 ]]; then',
)
PUBLISH = block("# --- Console manuals publish ---", "# --- End console manuals publish ---")

PUBLISHER = r'''
import json, os, sys
with open("publish.log", "a") as log:
    log.write(json.dumps(sys.argv[1:]) + "\n")
if "--dry-run" in sys.argv:
    sys.exit(1 if os.getenv("FAIL_DRY") else 0)
sys.exit(1 if os.getenv("FAIL_REAL") else 0)
'''


class ReleaseManualsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)

    def write_summary(self, version="9.9.9", kind="minor", raw=None):
        path = self.root / f"commercial/docs/releases/manual-summaries/v9.9.9.json"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(raw if raw is not None else json.dumps({"version": version, "kind": kind}))

    def write_publisher(self):
        path = self.root / "commercial/scripts/publish-manuals.py"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(PUBLISHER)

    def write_manuals_dir(self):
        (self.root / "commercial/docs/releases/v9.9.9/manuals").mkdir(parents=True, exist_ok=True)

    def run_bash(self, script, **env):
        merged = {k: v for k, v in os.environ.items()
                  if k not in ("DUDUCLAW_SKIP_MANUALS", "DUDUCLAW_MANUALS_BUCKET")}
        merged["PATH"] = f"{Path(sys.executable).parent}:/usr/bin:/bin"
        merged.update(env)
        return subprocess.run(["bash", "-euo", "pipefail", "-c", script],
                              cwd=self.root, env=merged, capture_output=True, text=True)

    def preflight(self, bump="minor", **env):
        script = (f'NEW_VERSION=9.9.9\nBUMP_TYPE={bump}\nPRO_MISSING=()\n' + PREFLIGHT_MANUALS +
                  'printf "MISSING:%s\\n" "${PRO_MISSING[@]:-}"\n')
        return self.run_bash(script, **env)

    def publish(self, pro=1, **env):
        script = f'NEW_VERSION=9.9.9\nPRO_RELEASE={pro}\nPRO_GAPS=()\n' + PUBLISH + \
                 '# --- End console manuals publish ---\nprintf "GAP:%s\\n" "${PRO_GAPS[@]:-}"\n'
        return self.run_bash(script, **env)

    def calls(self):
        log = self.root / "publish.log"
        return [json.loads(line) for line in log.read_text().splitlines()] if log.exists() else []

    # --- preflight ---
    def test_missing_summary_and_publisher_are_reported(self):
        result = self.preflight()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("manual-summaries/v9.9.9.json", result.stdout)
        self.assertIn("publish-manuals.py", result.stdout)
        self.assertIn("DUDUCLAW_SKIP_MANUALS=1", result.stdout)

    def test_complete_minor_passes(self):
        self.write_publisher()
        self.write_summary()
        self.write_manuals_dir()
        result = self.preflight()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "MISSING:")

    def test_wrong_version_kind_or_invalid_json_is_missing(self):
        self.write_publisher()
        self.write_manuals_dir()
        for kwargs in ({"version": "9.9.8"}, {"kind": "hotfix"}, {"raw": "{not json"}):
            with self.subTest(kwargs=kwargs):
                self.write_summary(**kwargs)
                result = self.preflight()
                self.assertIn("valid console manual summary", result.stdout)

    def test_hotfix_requires_hotfix_kind(self):
        self.write_publisher()
        self.write_summary(kind="minor")
        self.assertIn("valid console manual summary", self.preflight("hotfix").stdout)

    def test_minor_without_manuals_dir_is_missing(self):
        self.write_publisher()
        self.write_summary()
        result = self.preflight("minor")
        self.assertIn("v9.9.9/manuals/", result.stdout)
        result = self.preflight("major")
        self.assertIn("v9.9.9/manuals/", result.stdout)

    def test_hotfix_without_manuals_dir_is_ok(self):
        self.write_publisher()
        self.write_summary(kind="hotfix")
        result = self.preflight("hotfix")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "MISSING:")

    def test_skip_env_disables_checks(self):
        result = self.preflight(DUDUCLAW_SKIP_MANUALS="1")
        self.assertEqual(result.stdout.strip(), "MISSING:")

    # --- publish ---
    def test_success_runs_dry_run_then_real_with_public_repo(self):
        self.write_publisher()
        result = self.publish()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        calls = self.calls()
        self.assertEqual(len(calls), 2)
        self.assertIn("--dry-run", calls[0])
        self.assertNotIn("--dry-run", calls[1])
        for call in calls:
            self.assertEqual(call[call.index("--version") + 1], "9.9.9")
            self.assertEqual(call[call.index("--public-repo") + 1], str(self.root.resolve()))
            self.assertNotIn("--bucket", call)
        self.assertIn("GAP:\n", result.stdout)
        self.assertIn("Console manuals published", result.stdout)

    def test_bucket_is_passed_when_set(self):
        self.write_publisher()
        result = self.publish(DUDUCLAW_MANUALS_BUCKET="my-bucket")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for call in self.calls():
            self.assertEqual(call[call.index("--bucket") + 1], "my-bucket")

    def test_dry_run_failure_records_gap_and_skips_real_publish(self):
        self.write_publisher()
        result = self.publish(FAIL_DRY="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(self.calls()), 1)
        self.assertIn("--dry-run", self.calls()[0])
        self.assertIn("GAP:console manuals dry-run failed", result.stdout)
        self.assertIn("python3 commercial/scripts/publish-manuals.py --version 9.9.9", result.stdout)

    def test_real_failure_records_gap_with_rerun_command(self):
        self.write_publisher()
        result = self.publish(FAIL_REAL="1", DUDUCLAW_MANUALS_BUCKET="b")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(len(self.calls()), 2)
        self.assertIn("GAP:console manuals publish failed — re-run: "
                      "python3 commercial/scripts/publish-manuals.py --version 9.9.9 --bucket b",
                      result.stdout)

    def test_skip_env_records_gap_without_calling(self):
        self.write_publisher()
        result = self.publish(DUDUCLAW_SKIP_MANUALS="1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.calls(), [])
        self.assertIn("GAP:console manuals not published (DUDUCLAW_SKIP_MANUALS=1) — re-run: "
                      "python3 commercial/scripts/publish-manuals.py --version 9.9.9", result.stdout)

    def test_ce_only_release_does_not_publish(self):
        self.write_publisher()
        result = self.publish(pro=0)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.calls(), [])
        self.assertIn("NOTE: CE-only release", result.stdout)
        self.assertIn("GAP:\n", result.stdout)

    def test_blocks_are_ordered_in_release_script(self):
        self.assertLess(RELEASE.index("# --- End control-plane env sync ---"),
                        RELEASE.index("# --- Console manuals publish ---"))
        self.assertLess(RELEASE.index("# --- End console manuals publish ---"),
                        RELEASE.index("# --- Enterprise release gate ---"))
        self.assertLess(RELEASE.index("MANUALS_SUMMARY="),
                        RELEASE.index("# --- End enterprise release preflight ---"))


if __name__ == "__main__":
    unittest.main()
