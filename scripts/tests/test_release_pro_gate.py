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
FETCH = block("# Fetch the distributor notice once", 'if [[ "$PRO_RELEASE" == "1" && -x "$PRO_IMAGE_SCRIPT"')
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

    def git(self, *args: str):
        subprocess.run(["git", "-C", str(self.root / "commercial"), *args], check=True,
                       capture_output=True, text=True)

    def make_commercial_repo(self, branch: str):
        lock = self.root / "commercial/duduclaw-pro-gateway/Cargo.lock"
        lock.parent.mkdir(parents=True, exist_ok=True)
        lock.write_text('version = "9.9.8"\n')
        self.git("init", "-q", "-b", "main")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.invalid", "add", ".")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.invalid", "commit", "-q", "-m", "init")
        if branch != "main":
            self.git("switch", "-q", "-c", branch)
        return lock

    def test_missing_notice_token_stops_unless_skipped(self):
        self.provide_prerequisites()
        # Active account, but Secret Manager returns nothing.
        self.fake_tool("gcloud", 'case "$1" in secrets) exit 1;; *) echo deployer@example.iam.gserviceaccount.com;; esac')
        result = self.run_bash(PREFLIGHT)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("distributor-notice token", result.stdout)
        skipped = self.run_bash(PREFLIGHT, DUDUCLAW_DISTRIBUTOR_NOTICE="skip")
        self.assertEqual(skipped.returncode, 0, skipped.stdout + skipped.stderr)
        given = self.run_bash(PREFLIGHT, DUDUCLAW_CONTROL_ADMIN_TOKEN="t0ken")
        self.assertEqual(given.returncode, 0, given.stdout + given.stderr)

    def test_notice_token_is_never_exported(self):
        self.provide_prerequisites()
        result = self.run_bash(PREFLIGHT + '\nenv | grep -c "^DUDUCLAW_CONTROL_ADMIN_TOKEN=" || true\n')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("\n0\n", "\n" + result.stdout)

    def test_commercial_checkout_on_main_stops_the_release(self):
        self.provide_prerequisites()
        self.make_commercial_repo("main")
        result = self.run_bash(PREFLIGHT)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("commercial checkout on its own branch", result.stdout)
        self.git("switch", "-q", "-c", "chore/release")
        result = self.run_bash(PREFLIGHT)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_gate_commits_the_reresolved_lock_instead_of_failing(self):
        lock = self.make_commercial_repo("chore/release")
        lock.write_text('version = "9.9.9"\n')
        result = self.run_bash(
            "NEW_VERSION=9.9.9\nPRO_RELEASE=1\nPRO_GAPS=()\n"
            "export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@example.invalid "
            "GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@example.invalid\n" + GATE
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Committed in commercial (chore/release)", result.stdout)
        self.assertIn("REACHED_NEXT_STAGE", result.stdout)
        status = subprocess.run(["git", "-C", str(self.root / "commercial"), "status", "--porcelain"],
                                capture_output=True, text=True).stdout
        self.assertEqual(status, "")

    def run_fetch(self, fetch_body: str):
        gw = self.root / "commercial/duduclaw-pro-gateway"
        gw.mkdir(parents=True, exist_ok=True)
        (gw / "distributor-notice.html").write_text("stale notice from last release")
        fetch = gw / "fetch-distributor-notice.sh"
        fetch.write_text(f"#!/bin/sh\necho \"argv=$*\" > {self.root}/fetch.log\n"
                         f"echo \"env=$DUDUCLAW_CONTROL_ADMIN_TOKEN\" >> {self.root}/fetch.log\n{fetch_body}\n")
        fetch.chmod(0o755)
        (gw / "build-policy.py").write_text("")
        result = self.run_bash("PRO_RELEASE=1\nPRO_GAPS=()\nPRO_NOTICE_TOKEN=s3cret\n" + FETCH +
                               '\nprintf "GAPS=%s\\n" "${#PRO_GAPS[@]}"\n')
        return result, (self.root / "fetch.log").read_text(), gw / "distributor-notice.html"

    def test_notice_fetch_gets_the_token_only_through_its_environment(self):
        result, log, notice = self.run_fetch('echo fresh > "$1"')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("env=s3cret", log)
        self.assertNotIn("s3cret", log.splitlines()[0])  # argv line
        self.assertNotIn("s3cret", result.stdout + result.stderr)
        self.assertEqual(notice.read_text().strip(), "fresh")
        self.assertIn("GAPS=0", result.stdout)

    def test_failed_notice_fetch_leaves_no_stale_notice(self):
        result, _, notice = self.run_fetch("exit 1")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(notice.exists())
        self.assertIn("GAPS=1", result.stdout)

    def test_darwin_binary_path_follows_cargo_target_dir(self):
        darwin = RELEASE[RELEASE.index("validating policy and building duduclaw-pro"):
                         RELEASE.index("# linux-x64: extract from the duduclaw-pro image")]
        self.assertIn("cargo metadata --no-deps", darwin)
        self.assertNotIn("../../target/release/duduclaw-pro", darwin)
        self.assertNotIn("cp target/release/duduclaw-pro", darwin)

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
