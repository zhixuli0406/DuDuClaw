"""Exercise the enterprise preflight and final gate of release.sh in isolation.

v1.76.1 was cut from a worktree without the private checkout; every
enterprise step was skipped silently and enterprise installs stayed on
v1.76.0 (2026-10-27). These tests pin the two blocks that now prevent that.
"""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
RELEASE = (ROOT / "scripts/release.sh").read_text()


def block(start: str, end: str) -> str:
    begin = RELEASE.index(start)
    return RELEASE[begin:RELEASE.index(end, begin)]


PREFLIGHT = block(
    "# --- Enterprise release preflight", "# --- End enterprise release preflight ---"
)
GATE = block("# --- Enterprise release gate ---", 'echo " Release v$NEW_VERSION prepared successfully!"')


class ReleaseProGateTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.home = self.root / "home"
        self.bin = self.root / "bin"
        self.home.mkdir()
        self.bin.mkdir()

    def provide_prerequisites(self):
        script = self.root / "commercial/duduclaw-pro-gateway/build-image.sh"
        script.parent.mkdir(parents=True)
        script.write_text("#!/bin/sh\n")
        script.chmod(0o755)
        key = self.home / ".minisign/duduclaw-pro-release.key"
        key.parent.mkdir()
        key.write_text("key")
        self.fake_tool("minisign")
        self.fake_tool("docker")
        # `gcloud auth list --format=value(account)` prints the active account.
        self.fake_tool("gcloud", "echo deployer@example.iam.gserviceaccount.com")

    def fake_tool(self, name: str, body: str = "exit 0"):
        path = self.bin / name
        path.write_text(f"#!/bin/sh\n{body}\n")
        path.chmod(0o755)

    def run_bash(self, script: str, **env):
        merged = dict(os.environ, HOME=str(self.home), PATH=f"{self.bin}:/usr/bin:/bin", **env)
        return subprocess.run(
            ["bash", "-euo", "pipefail", "-c", script + '\nprintf "REACHED_NEXT_STAGE PRO_RELEASE=%s\\n" "$PRO_RELEASE"\n'],
            cwd=self.root, env=merged, capture_output=True, text=True,
        )

    def test_missing_private_checkout_stops_before_any_change(self):
        result = self.run_bash(PREFLIGHT)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("REACHED_NEXT_STAGE", result.stdout)
        self.assertIn("commercial/duduclaw-pro-gateway", result.stdout)
        self.assertIn("DUDUCLAW_RELEASE_WITHOUT_PRO=1", result.stdout)

    def test_complete_prerequisites_continue_with_enterprise_on(self):
        self.provide_prerequisites()
        result = self.run_bash(PREFLIGHT)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("REACHED_NEXT_STAGE PRO_RELEASE=1", result.stdout)

    def test_stopped_docker_or_signed_out_gcloud_stops_the_release(self):
        self.provide_prerequisites()
        self.fake_tool("docker", "exit 1")
        self.fake_tool("gcloud", "exit 0")
        result = self.run_bash(PREFLIGHT)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("running Docker daemon", result.stdout)
        self.assertIn("active gcloud account", result.stdout)

    def test_ce_only_release_must_be_explicit_and_is_announced(self):
        result = self.run_bash(PREFLIGHT, DUDUCLAW_RELEASE_WITHOUT_PRO="1")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("REACHED_NEXT_STAGE PRO_RELEASE=0", result.stdout)
        self.assertIn("NO enterprise assets", result.stdout)

    def run_gate(self, gaps: str, pro_release: str = "1"):
        prelude = f"NEW_VERSION=9.9.9\nPRO_RELEASE={pro_release}\nPRO_GAPS=({gaps})\n"
        return self.run_bash(prelude + GATE)

    def test_any_gap_fails_the_release_and_lists_it(self):
        result = self.run_gate('"pro-bin-linux v9.9.9 not uploaded"')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INCOMPLETE", result.stdout)
        self.assertIn("pro-bin-linux v9.9.9 not uploaded", result.stdout)
        self.assertIn("release.sh verify 9.9.9", result.stdout)
        self.assertNotIn("REACHED_NEXT_STAGE", result.stdout)

    def test_no_gaps_reaches_the_success_banner(self):
        result = self.run_gate("")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("REACHED_NEXT_STAGE", result.stdout)

    def test_ce_only_release_skips_the_gate_but_says_so(self):
        result = self.run_gate('"anything"', pro_release="0")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("CE-only release", result.stdout)

    def test_blocks_sit_in_the_right_places(self):
        self.assertLess(RELEASE.index(PREFLIGHT), RELEASE.index("# --- Mandatory delivery review"))
        self.assertLess(RELEASE.index(PREFLIGHT), RELEASE.index("# --- Bump every platform manifest"))
        self.assertLess(RELEASE.index("DUDUCLAW_PRO_VERSIONS=${CP_VERSIONS}"), RELEASE.index(GATE))
        # Every opt-out of an enterprise step is recorded as a gap, never silent.
        for skip in ("DUDUCLAW_SKIP_PRO_IMAGE=1)", "DUDUCLAW_SKIP_PRO_BIN=1)", "DUDUCLAW_SKIP_PRO_VERSIONS_ENV=1)"):
            self.assertIn(skip, RELEASE)
            self.assertIn("PRO_GAPS+=(", RELEASE[RELEASE.index(skip) - 400:RELEASE.index(skip) + 200])


if __name__ == "__main__":
    unittest.main()
