"""Exercise release control-plane sync/verify with PATH-only cloud mocks."""
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


HELPERS = block("# Control-plane regions are independent", "# --- Enumerate every version-bearing")
SYNC = block("    # --- Control-plane offered-version allowlist", "    # --- End control-plane env sync ---")
GATE = block("# --- Enterprise release gate ---", 'echo " Release v$NEW_VERSION prepared successfully!"')
MANIFEST = json.dumps({
    "schema_version": 1, "profile": "general-distributor", "platform": "linux/amd64",
    "artifacts": [{"version": "v9.9.9", "sha256": "a" * 64}],
}, separators=(",", ":"))

GCLOUD = r'''
import json, os, sys
from pathlib import Path
args = sys.argv[1:]
with open("cloud.log", "a") as log:
    log.write(json.dumps(args) + "\n")
assert args[:1] != ["logging"], "logging.read is not available"
if args[:1] in (["artifacts"], ["storage"]):
    sys.exit(0)
region = args[args.index("--region") + 1]
assert args[3] == "duduclaw-control-plane" or args[:3] == ["run", "revisions", "describe"]
assert args[args.index("--project") + 1] == "mock-project"
state = Path(region + ".env")
if args[:3] == ["run", "services", "update"]:
    if os.getenv("FAIL_UPDATE") == region:
        sys.exit(1)
    value = args[args.index("--update-env-vars") + 1]
    assert value.startswith("^|^")
    state.write_text(json.dumps(dict(pair.split("=", 1) for pair in value[3:].split("|"))))
elif args[:3] == ["run", "services", "update-traffic"]:
    assert "--to-latest" in args
    sys.exit(1 if os.getenv("FAIL_TRAFFIC") == region else 0)
elif args[:3] == ["run", "services", "describe"]:
    if os.getenv("FAIL_READ") == region:
        sys.exit(1)
    traffic = [{"revisionName": region + "-live", "percent": 100}]
    if os.getenv("SPLIT_TRAFFIC") == region:
        traffic = [{"revisionName": "old", "percent": 50}, {"revisionName": "new", "percent": 50}]
    print(json.dumps({"status": {"traffic": traffic}, "spec": {"template": {"spec": {
        "containers": [{"env": [{"name": "DUDUCLAW_GENERAL_DELIVERY_MANIFEST", "value": os.environ["EXPECTED_MANIFEST"]}]}]
    }}}}))
elif args[:3] == ["run", "revisions", "describe"]:
    assert args[3] == region + "-live"
    values = json.loads(state.read_text()) if state.exists() else {
        "DUDUCLAW_PRO_VERSIONS": "v9.9.9,v9.9.8", "DUDUCLAW_GENERAL_DELIVERY_MANIFEST": os.environ["EXPECTED_MANIFEST"]}
    if os.getenv("MISMATCH_REGION") == region:
        values[os.environ.get("MISMATCH_KEY", "DUDUCLAW_PRO_VERSIONS")] = os.environ.get("MISMATCH_VALUE", "stale")
    print(json.dumps({"spec": {"containers": [{"env": [{"name": k, "value": v} for k, v in values.items()]}]}}))
else:
    raise AssertionError(args)
'''


class ReleaseControlPlaneRegionsTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.fake("gcloud", GCLOUD, python=True)
        # Do not run git operations or the rest of the release pipeline.
        self.fake("git", 'if [ "$1" = tag ]; then printf "v9.9.9\\nv9.9.8\\n"; else exit 1; fi')
        manifest_script = self.root / "commercial/duduclaw-pro-gateway/delivery_manifest.py"
        manifest_script.parent.mkdir(parents=True)
        manifest_script.write_text('import os, sys\nfrom pathlib import Path\n'
                                   'assert "--current" in sys.argv and "--add" in sys.argv\n'
                                   'Path("compute.log").open("a").write("computed\\n")\n'
                                   'print(os.environ["EXPECTED_MANIFEST"])\n')
        (self.root / "tar.sha").write_text("v9.9.9 " + "a" * 64 + "\n")

    def fake(self, name, body, python=False):
        path = self.bin / name
        path.write_text(f"#!{sys.executable if python else '/bin/sh'}\n" + body + "\n")
        path.chmod(0o755)

    def run_block(self, body, **overrides):
        env = dict(os.environ)
        for name in ("CP_REGIONS", "CP_REGION", "DUDUCLAW_SKIP_PRO_VERSIONS_ENV", "DUDUCLAW_RELEASE_WITHOUT_PRO"):
            env.pop(name, None)
        env.update(PATH=f"{self.bin}:{Path(sys.executable).parent}:/usr/bin:/bin",
                   DUDUCLAW_GCP_PROJECT="mock-project", EXPECTED_MANIFEST=MANIFEST)
        env.update(overrides)
        return subprocess.run(["bash", "-euo", "pipefail", "-c", HELPERS + body],
                              cwd=self.root, env=env, capture_output=True, text=True)

    def sync(self, **env):
        return self.run_block('NEW_VERSION=9.9.9\nPRO_RELEASE=1\nPRO_IMAGE_PUSHED=1\n'
                              'PRO_BIN_UPLOADED=1\nPRO_GAPS=()\nPRO_TAR_SHA_FILE=tar.sha\n' + SYNC + '\n' + GATE, **env)

    def verify(self, **env):
        return self.run_block('verify_cp_regions duduclaw-control-plane mock-project 9.9.9\n', **env)

    def calls(self, action):
        return [args for args in map(json.loads, (self.root / "cloud.log").read_text().splitlines())
                if args[:3] == ["run", "services", action]]

    def test_default_updates_identical_values_in_both_regions_and_reads_revisions(self):
        result = self.sync()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        updates = self.calls("update")
        self.assertEqual([a[a.index("--region") + 1] for a in updates], ["asia-east1", "us-east4"])
        self.assertEqual(updates[0][-1], updates[1][-1])
        self.assertIn("DUDUCLAW_PRO_VERSIONS=v9.9.9,v9.9.8|DUDUCLAW_GENERAL_DELIVERY_MANIFEST=" + MANIFEST, updates[0][-1])
        self.assertEqual((self.root / "compute.log").read_text(), "computed\n")
        for region in ("asia-east1", "us-east4"):
            self.assertIn(f"[{region}] serving revision: {region}-live", result.stdout)

    def test_either_env_mismatch_fails_release_gate(self):
        for key in ("DUDUCLAW_PRO_VERSIONS", "DUDUCLAW_GENERAL_DELIVERY_MANIFEST"):
            with self.subTest(key=key):
                result = self.sync(MISMATCH_REGION="us-east4", MISMATCH_KEY=key)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertIn("INCOMPLETE", result.stdout)
                self.assertIn("[us-east4]", result.stdout)
                self.assertNotIn("prepared successfully", result.stdout)

    def test_cp_region_only_warns_and_updates_one_region(self):
        result = self.sync(CP_REGION="us-east4")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("WARNING: CP_REGION-only", result.stderr)
        self.assertIn("other control-plane region is skipped", result.stderr)
        self.assertEqual(len(self.calls("update")), 1)
        self.assertIn("us-east4", self.calls("update")[0])

    def test_cp_regions_override_takes_precedence(self):
        result = self.sync(CP_REGIONS="us-east4 custom-region", CP_REGION="ignored")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual([a[a.index("--region") + 1] for a in self.calls("update")], ["us-east4", "custom-region"])
        self.assertNotIn("CP_REGION-only", result.stderr)

    def test_empty_region_list_fails_without_cloud_calls(self):
        result = self.sync(CP_REGIONS=" ")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("must contain at least one region", result.stderr)
        self.assertFalse((self.root / "cloud.log").exists())

    def test_failed_update_does_not_skip_other_region(self):
        result = self.sync(FAIL_UPDATE="asia-east1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(len(self.calls("update")), 2)
        self.assertIn("[us-east4] serving revision", result.stdout)

    def test_unreadable_revision_split_traffic_and_traffic_failure_fail(self):
        for flag in ("FAIL_READ", "SPLIT_TRAFFIC", "FAIL_TRAFFIC"):
            with self.subTest(flag=flag):
                result = self.sync(**{flag: "us-east4"})
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_unreadable_source_manifest_updates_neither_region(self):
        result = self.sync(FAIL_READ="asia-east1")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.calls("update"), [])
        self.assertIn("no regions updated", result.stdout)

    def test_verify_counts_both_regions(self):
        result = self.verify()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("4/4 checks passed across 2 region(s)", result.stdout)
        for region in ("asia-east1", "us-east4"):
            self.assertIn(f"pro-offer [{region}]", result.stdout)
            self.assertIn(f"delivery-manifest [{region}]", result.stdout)

    def test_verify_fails_for_one_region(self):
        for key in ("DUDUCLAW_PRO_VERSIONS", "DUDUCLAW_GENERAL_DELIVERY_MANIFEST"):
            with self.subTest(key=key):
                result = self.verify(MISMATCH_REGION="us-east4", MISMATCH_KEY=key)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("[us-east4]", result.stdout)
                self.assertNotIn("4/4 checks passed", result.stdout)

    def test_verify_rejects_valid_but_different_manifest(self):
        other = MANIFEST.replace("a" * 64, "b" * 64)
        result = self.verify(MISMATCH_REGION="us-east4", MISMATCH_KEY="DUDUCLAW_GENERAL_DELIVERY_MANIFEST", MISMATCH_VALUE=other)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("3/4 checks passed", result.stdout)

    def test_verify_cp_region_only_warns_and_counts_one_region(self):
        result = self.verify(CP_REGION="asia-east1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("CP_REGION-only", result.stderr)
        self.assertIn("2/2 checks passed across 1 region(s)", result.stdout)

    def test_run_verify_propagates_regional_failure(self):
        # Run the full verification function with all external reads mocked.
        build = self.root / "commercial/duduclaw-pro-gateway/build-image.sh"
        build.write_text("#!/bin/sh\nexit 99\n")
        build.chmod(0o755)
        self.fake("curl", "echo '{\"version\":\"9.9.9\"}'")
        verify = block("run_verify() {", "# --- Arg parsing")
        body = ('PYPI_PKG=duduclaw\nNPM_PKG=duduclaw\n'
                "SEMVER='[0-9]+\\.[0-9]+\\.[0-9]+'\n"
                'darwin_platform_label() { echo ""; }\n' + verify + '\nrun_verify 9.9.9\n')
        success = self.run_block(body)
        self.assertEqual(success.returncode, 0, success.stdout + success.stderr)
        self.assertIn("4/4 checks passed", success.stdout)
        failure = self.run_block(body, MISMATCH_REGION="us-east4")
        self.assertNotEqual(failure.returncode, 0)
        self.assertIn("pro-offer [us-east4] v9.9.9 MISSING", failure.stdout)


if __name__ == "__main__":
    unittest.main()
