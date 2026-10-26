"""Protect full CI coverage when reducing obsolete runs and build overhead."""

from pathlib import Path
import shlex
import unittest

import yaml


ROOT = Path(__file__).resolve().parents[2]
E2E_FILTERS = (
    "three_page_pilot_",
    "workflow::security_race_tests",
    "workflow::resume_tests",
    "workflow::f1b_e2e_tests",
    "workflow::f5a_e2e_tests",
)


class CiWorkflowTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # BaseLoader preserves GitHub's `on` key instead of YAML 1.1's boolean.
        cls.workflow = yaml.load(
            (ROOT / ".github/workflows/ci.yml").read_text(), Loader=yaml.BaseLoader
        )
        cls.jobs = cls.workflow["jobs"]

    def commands(self, job):
        return [
            shlex.split(line)
            for step in self.jobs[job]["steps"]
            for line in step.get("run", "").splitlines()
            if line.strip().startswith("cargo ")
        ]

    def test_latest_runs_do_not_cancel_other_prs_or_main(self):
        concurrency = self.workflow["concurrency"]
        group = concurrency["group"]
        for context in ("github.workflow", "github.event_name",
                        "github.event.pull_request.number", "github.ref"):
            self.assertIn(context, group)
        self.assertNotIn("github.run_id", group)
        self.assertEqual(concurrency["cancel-in-progress"], "true")
        for event in ("push", "pull_request"):
            trigger = self.workflow["on"][event]
            self.assertEqual(trigger["branches"], ["main"])
            self.assertFalse({"paths", "paths-ignore"} & trigger.keys())
        self.assertIn("workflow_dispatch", self.workflow["on"])

    def test_required_jobs_run_even_if_settle_fails(self):
        for job, name in (("rust", "Rust ${{ matrix.os }}"),
                          ("dashboard", "Dashboard"), ("python", "Python"),
                          ("security", "Security Audit")):
            with self.subTest(job=job):
                self.assertEqual(self.jobs[job]["name"], name)
                self.assertIn(self.jobs[job]["needs"], ("settle", ["settle"]))
                # A status function avoids GitHub's implicit success() guard.
                self.assertEqual(self.jobs[job]["if"], "${{ !cancelled() }}")
        self.assertEqual(self.jobs["rust"]["strategy"]["matrix"]["os"],
                         ["ubuntu-latest", "macos-latest", "windows-latest"])

    def test_build_profiles_keep_debug_assertions(self):
        env = self.workflow["env"]
        for profile in ("DEV", "TEST"):
            self.assertEqual(env[f"CARGO_PROFILE_{profile}_DEBUG"], "0")
        self.assertFalse(any("ASSERTIONS" in key or "OPT_LEVEL" in key
                             for key in env))

    def test_execution_guards_preserve_blocking_coverage(self):
        conditional_steps = {
            "Install Linux system deps (xcap screen-capture → wayland/xcb/gbm)":
                "runner.os == 'Linux'",
            "Test gateway Discord voice TLS transport": "runner.os == 'Linux'",
            "Test workflow stdio checkpoints (CLI, workflow-test-checkpoints)":
                "matrix.os == 'ubuntu-latest'",
            "Test workflow end to end (pilot, races, resume, F1b, F5-A) against the built CLI":
                "matrix.os == 'ubuntu-latest'",
        }
        advisory_steps = {"Clippy (no dashboard)", "Clippy gateway"}
        self.assertEqual(self.jobs["rust"]["strategy"]["fail-fast"], "false")
        self.assertEqual(self.jobs["rust"]["continue-on-error"],
                         "${{ matrix.os == 'windows-latest' }}")
        for job in ("rust", "dashboard", "python", "security"):
            if job != "rust":
                self.assertNotIn("continue-on-error", self.jobs[job])
            for step in self.jobs[job]["steps"]:
                name = step.get("name", step.get("uses"))
                with self.subTest(job=job, step=name):
                    expected = conditional_steps.get(name) if job == "rust" else None
                    self.assertEqual(step.get("if"), expected)
                    if job == "rust" and name in advisory_steps:
                        self.assertEqual(step.get("continue-on-error"), "true")
                    else:
                        self.assertNotIn("continue-on-error", step)

    def test_settle_only_delays_pr_runs(self):
        settle = self.jobs["settle"]
        self.assertNotIn("if", settle)
        delay = next(s for s in settle["steps"] if "sleep" in s.get("run", ""))
        self.assertEqual(delay["if"], "github.event_name == 'pull_request'")
        self.assertEqual(shlex.split(delay["run"]), ["sleep", "60"])

    def test_default_and_optional_feature_coverage_remains(self):
        commands = self.commands("rust")
        workspace = next(c for c in commands if c[:2] == ["cargo", "test"]
                         and "--workspace" in c)
        self.assertNotIn("--no-default-features", workspace)
        self.assertIn("--no-fail-fast", workspace)
        for selector in ("--lib", "--tests", "--bins", "--doc"):
            self.assertNotIn(selector, workspace)
        for package, feature, target in (
            ("duduclaw-core", "app-compat", "test"),
            ("duduclaw-cli", "app-compat", "test"),
            ("duduclaw-gateway", "otel", "check"),
            ("duduclaw-gateway", "discord-voice", "test"),
            ("duduclaw-cli", "workflow-test-checkpoints", "test"),
        ):
            with self.subTest(feature=feature, package=package):
                self.assertTrue(any(c[:2] == ["cargo", target]
                                    and package in c and feature in c
                                    for c in commands))
        gateway = next(c for c in commands if c[:2] == ["cargo", "test"]
                       and "duduclaw-gateway" in c and "--skip" in c)
        self.assertNotIn("--lib", gateway)  # Integration and doctests stay.
        self.assertIn("--no-default-features", gateway)

    def test_serial_e2e_uses_restored_production_cli(self):
        step = next(s for s in self.jobs["rust"]["steps"]
                    if "DUDUCLAW_P1_PILOT_EVIDENCE_DIR" in s.get("env", {}))
        self.assertEqual(step["if"], "matrix.os == 'ubuntu-latest'")
        lines = [shlex.split(line) for line in step["run"].splitlines()
                 if line.strip().startswith("cargo ")]
        self.assertEqual(lines[0][:2], ["cargo", "build"])
        self.assertIn("duduclaw-cli", lines[0])
        self.assertNotIn("--features", lines[0])
        self.assertNotIn("--no-default-features", lines[0])
        self.assertEqual(len(lines[1:]), len(E2E_FILTERS))
        for command, test_filter in zip(lines[1:], E2E_FILTERS):
            self.assertIn(test_filter, command)
            self.assertIn("--lib", command)
            self.assertIn("--no-fail-fast", command)
            self.assertIn("--test-threads=1", command)

    def test_dashboard_and_security_still_execute_real_checks(self):
        dashboard = "\n".join(s.get("run", "")
                              for s in self.jobs["dashboard"]["steps"])
        for command in ("npx tsc --noEmit", "npm test", "npm run build",
                        "duduclaw-gateway/dashboard", "duduclaw-cli/dashboard"):
            self.assertIn(command, dashboard)
        security = "\n".join(s.get("run", "")
                             for s in self.jobs["security"]["steps"])
        for command in ("cargo audit", "npm", "verify_security_vendor.py",
                        "src-tauri/Cargo.lock", "variant_str_iter"):
            self.assertIn(command, security)

    def test_dashboard_embedding_excludes_vulnerable_zstd_stack(self):
        workspace = (ROOT / "Cargo.toml").read_text()
        rust_embed_line = next(
            line for line in workspace.splitlines()
            if line.startswith("rust-embed =")
        )
        self.assertNotIn("compression", rust_embed_line)

        lock = (ROOT / "Cargo.lock").read_text()
        for package in (
            "include-flate",
            "include-flate-codegen",
            "include-flate-compress",
            "zstd",
            "zstd-safe",
            "zstd-sys",
        ):
            with self.subTest(package=package):
                self.assertNotIn(f'name = "{package}"', lock)


if __name__ == "__main__":
    unittest.main()
