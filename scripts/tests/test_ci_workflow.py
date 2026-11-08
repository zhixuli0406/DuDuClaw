"""Protect full CI coverage when reducing obsolete runs and build overhead."""

from pathlib import Path
import json
import re
import shlex
import unittest
import os
import subprocess
import tempfile
import tomllib

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
            shlex.split(re.sub(r"\$\{\{.*?\}\}", "", line))
            for step in self.jobs[job]["steps"]
            for line in step.get("run", "").splitlines()
            if line.strip().startswith("cargo ")
        ]

    def test_ci_permissions_are_read_only(self):
        self.assertEqual(self.workflow["permissions"], {"contents": "read"})
        for job in self.jobs.values():
            self.assertNotIn("permissions", job)

    def test_linux_release_smoke_trigger_and_safety_contract(self):
        text = (ROOT / ".github/workflows/linux-release-smoke.yml").read_text()
        smoke = yaml.load(text, Loader=yaml.BaseLoader)
        self.assertEqual(set(smoke["on"]), {"pull_request", "workflow_dispatch"})
        self.assertEqual(smoke["on"]["pull_request"], {"paths": [
            ".github/workflows/release.yml", ".github/workflows/linux-release-smoke.yml",
            "Cross.toml", ".cargo/**", "Cargo.lock", "Cargo.toml", "crates/*/Cargo.toml",
            "scripts/check_glibc.py", "crates/duduclaw-cli/assets/native-protected-launcher.sh",
            "crates/duduclaw-cli/src/service/**", "crates/duduclaw-core/src/root_key*.rs",
        ]})
        self.assertEqual(smoke["permissions"], {"contents": "read"})
        self.assertNotRegex(text, r"(?i)secrets\s*(?:\.|\[|\}\})")
        self.assertEqual(set(smoke["jobs"]), {"build"})
        job = smoke["jobs"]["build"]
        self.assertNotIn("permissions", job)
        self.assertNotIn("env", smoke)
        self.assertNotIn("env", job)
        self.assertEqual(job["runs-on"], "ubuntu-latest")
        self.assertEqual(job["strategy"], {"fail-fast": "false", "matrix": {"target": [
            "x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu",
        ]}})
        upload = next(s for s in job["steps"]
                      if s.get("uses", "").startswith("actions/upload-artifact@"))
        self.assertEqual(upload["with"], {
            "name": "duduclaw-${{ matrix.target }}-smoke",
            "path": "target/${{ matrix.target }}/release/duduclaw",
            "if-no-files-found": "error", "retention-days": "3",
        })
        start = next(s for s in job["steps"] if s.get("name", "").startswith("Start x86_64"))
        self.assertEqual(start["if"], "matrix.target == 'x86_64-unknown-linux-gnu'")
        self.assertEqual(start["run"], 'docker run --rm -v "$PWD/target/x86_64-unknown-linux-gnu/release:/b:ro" ubuntu:22.04 /b/duduclaw --version')

    def test_linux_release_smoke_matches_release_build_steps(self):
        def build_job(name):
            return yaml.load((ROOT / f".github/workflows/{name}.yml").read_text(),
                             Loader=yaml.BaseLoader)["jobs"]["build"]

        release = build_job("release")
        smoke = build_job("linux-release-smoke")
        # Compare full ordered steps: action pins/options, commands, features,
        # conditions, and environment must all follow the shipping Linux path.
        prefixes = ("actions/setup-node@", "dtolnay/rust-toolchain@", "Swatinem/rust-cache@")
        names = {"Install cross (Linux)", "Build frontend", "Check Linux glibc baseline"}
        def linux_steps(job):
            return [s for s in job["steps"]
                    if s.get("uses", "").startswith(prefixes)
                    or s.get("name") in names
                    or s.get("run", "").startswith("cross build --release")]

        expected = linux_steps(release)
        self.assertEqual(len(expected), 7)
        self.assertEqual(linux_steps(smoke), expected)
        cache = next(s for s in smoke["steps"]
                     if s.get("uses", "").startswith("Swatinem/rust-cache@"))
        self.assertEqual(cache["with"]["save-if"], "false")
        # An exact step allowlist also prevents publishing/signing or hidden
        # cache writers from being introduced into this unprivileged PR build.
        self.assertEqual(smoke["steps"][1:-2], expected)
        for prefix in ("actions/checkout@", "actions/upload-artifact@"):
            def action(job):
                return next(s["uses"] for s in job["steps"]
                            if s.get("uses", "").startswith(prefix))
            self.assertEqual(action(smoke), action(release))
        checkout = next(s["uses"] for s in release["steps"]
                        if s.get("uses", "").startswith("actions/checkout@"))
        self.assertEqual(smoke["steps"][0], {"uses": checkout})

    def test_latest_runs_do_not_cancel_other_prs_or_main(self):
        concurrency = self.workflow["concurrency"]
        group = concurrency["group"]
        for context in ("github.workflow", "github.event_name",
                        "github.event.pull_request.number", "github.ref"):
            self.assertIn(context, group)
        self.assertNotIn("github.run_id", group)
        self.assertEqual(concurrency["cancel-in-progress"],
                         "${{ github.event_name == 'pull_request' }}")
        for event in ("push", "pull_request"):
            trigger = self.workflow["on"][event]
            self.assertEqual(trigger["branches"], ["main"])
            self.assertFalse({"paths", "paths-ignore"} & trigger.keys())
        self.assertIn("workflow_dispatch", self.workflow["on"])

    def test_required_jobs_run_even_if_settle_fails(self):
        for job, name in (("rust",
                           "Rust ${{ matrix.os }}${{ matrix.os != 'ubuntu-latest' && "
                           "format(' (shard {0}/3)', matrix.shard) || '' }}"),
                          ("dashboard", "Dashboard"), ("python", "Python"),
                          ("security", "Security Audit")):
            with self.subTest(job=job):
                self.assertEqual(self.jobs[job]["name"], name)
                self.assertIn(self.jobs[job]["needs"], ("settle", ["settle"]))
                # A status function avoids GitHub's implicit success() guard.
                expected = ("${{ !cancelled() && needs.settle.outputs.dashboard != 'skip' }}"
                            if job == "dashboard" else "${{ !cancelled() }}")
                self.assertEqual(self.jobs[job]["if"], expected)

    def test_python_checks_embedded_paths_on_every_pr(self):
        steps = self.jobs["python"]["steps"]
        commands = (
            "python3 -m unittest discover -s scripts/tests -p 'test_check_embedded_paths.py' -v",
            "python3 scripts/check_embedded_paths.py . container/Dockerfile.server",
            "python3 scripts/check_embedded_paths.py . container/Dockerfile.edition-smoke --stage builder",
        )
        for command in commands:
            with self.subTest(command=command):
                step = next(s for s in steps if command in s.get("run", "").splitlines())
                self.assertNotIn("if", step)

    def test_prs_run_linux_and_main_nightly_manual_run_all_platforms(self):
        matrix = self.jobs["rust"]["strategy"]["matrix"]["include"]
        expression = re.fullmatch(
            r"\$\{\{ fromJSON\(github.event_name == 'pull_request' && '(.*?)' \|\| '(.*?)'\) \}\}",
            matrix,
        )
        self.assertIsNotNone(expression, "matrix must distinguish PRs from full platform runs")
        self.assertEqual(json.loads(expression[1]), [{"os": "ubuntu-latest", "shard": 1}])
        for event in ("push", "schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                self.assertIn(event, self.workflow["on"])
                self.assertEqual(json.loads(expression[2]),
                                 [{"os": "ubuntu-latest", "shard": 1}] + [
                                     {"os": os_name, "shard": shard}
                                     for os_name in ("macos-latest", "windows-latest")
                                     for shard in range(1, 4)
                                 ])
        self.assertEqual(self.workflow["on"]["schedule"], [{"cron": "0 18 * * *"}])

    def test_every_native_shard_partitions_both_nextest_suites(self):
        suites = [s for s in self.jobs["rust"]["steps"]
                  if s.get("id") in {"nextest_workspace", "nextest_gateway"}]
        self.assertEqual(len(suites), 2)
        partition = ("${{ matrix.os != 'ubuntu-latest' && "
                     "format('--partition hash:{0}/3', matrix.shard) || '' }}")
        for step in suites:
            with self.subTest(suite=step["id"]):
                self.assertTrue(step["run"].endswith(" " + partition))
                self.assertEqual(step["run"].count("--partition"), 1)
                self.assertNotIn("matrix.shard", step["if"])
                self.assertNotIn("continue-on-error", step)

    def test_ci_and_tag_docker_cache_write_policy(self):
        rust_caches = [
            step
            for job in self.jobs.values()
            for step in job.get("steps", [])
            if step.get("uses", "").startswith("Swatinem/rust-cache@")
        ]
        self.assertEqual(len(rust_caches), 2)
        self.assertEqual(rust_caches[0]["with"], {
            "shared-key": "ci-${{ matrix.os }}",
            "save-if": "${{ github.ref == 'refs/heads/main' }}",
        })
        self.assertEqual(rust_caches[1]["with"], {
            "shared-key": "ci-ubuntu-latest", "save-if": "false",
        })

        docker_path = ROOT / ".github/workflows/docker-image.yml"
        docker_text = docker_path.read_text()
        docker = yaml.load(docker_text, Loader=yaml.BaseLoader)
        build = next(
            step for step in docker["jobs"]["build"]["steps"]
            if step.get("uses", "").startswith("docker/build-push-action@")
        )
        self.assertNotIn("cache-from", build.get("with", {}))
        self.assertNotIn("cache-to", build.get("with", {}))
        self.assertIn(
            "# Tag refs cannot share GHA caches, and main writes no Docker cache scope.",
            docker_text,
        )

    def test_windows_native_cases_remain_serial_and_cannot_silently_match_zero(self):
        # These exact cases previously ran in release-readiness.yml. The
        # workspace suite also covers duduclaw-fork, but explicit invocations
        # keep the named Windows acceptance cases visible and serialized.
        step = next(s for s in self.jobs["rust"]["steps"]
                    if s.get("name") == "Run every required Windows case on the native host")
        self.assertIn("runner.os == 'Windows'", step["if"])
        self.assertEqual(step["shell"], "pwsh")
        for case in (
            "publication::windows::tests::protected_owner_mutex_and_private_recovery_directory_are_validated",
            "publication::windows::tests::namespace_squatter_with_broad_acl_is_rejected",
            "test_runner::tests::windows::passing_command_exit_zero",
            "test_runner::tests::windows::failing_command_nonzero",
            "test_runner::tests::windows::runs_in_workspace_cwd",
            "test_runner::tests::windows::timeout_kills_and_marks",
            "test_runner::tests::windows::timeout_kills_child_and_grandchild",
        ):
            self.assertIn("'" + case + "'", step["run"])
        self.assertIn("cargo test --locked -p duduclaw-fork --lib $test -- --exact --nocapture --test-threads=1", step["run"])
        self.assertIn("$LASTEXITCODE", step["run"])
        self.assertIn("1 passed; 0 failed; 0 ignored;", step["run"])
        self.assertFalse((ROOT / ".github/workflows/release-readiness.yml").exists())

    def test_release_rust_caches_are_pinned_and_target_specific(self):
        cache = next(s["uses"] for s in self.jobs["rust"]["steps"]
                     if s.get("uses", "").startswith("Swatinem/rust-cache@"))
        warm = yaml.load(
            (ROOT / ".github/workflows/release-cache-warm.yml").read_text(),
            Loader=yaml.BaseLoader,
        )
        for name in ("release", "desktop-release"):
            workflow = yaml.load(
                (ROOT / f".github/workflows/{name}.yml").read_text(), Loader=yaml.BaseLoader
            )
            steps = workflow["jobs"]["build"]["steps"]
            configured = next(s for s in steps if s.get("uses") == cache)
            warmed = warm["jobs"][name]
            warm_cache = next(s for s in warmed["steps"] if s.get("uses") == cache)
            key_prefix = "release-cross-glibc231" if name == "release" else name
            self.assertEqual(configured["with"]["shared-key"], key_prefix + "-${{ matrix.target }}")
            self.assertEqual(configured["with"]["save-if"], "false")
            self.assertNotIn("key", configured["with"])
            warm_options = dict(warm_cache["with"])
            expected_save = ("${{ github.ref == 'refs/heads/main' && runner.os == 'macOS' }}"
                             if name == "desktop-release"
                             else "${{ github.ref == 'refs/heads/main' }}")
            self.assertEqual(warm_options.pop("save-if"), expected_save)
            self.assertEqual(warm_options,
                             {k: v for k, v in configured["with"].items() if k != "save-if"})
            self.assertEqual(warmed["runs-on"], workflow["jobs"]["build"]["runs-on"])
            self.assertEqual(
                [(m["os"], m["target"]) for m in warmed["strategy"]["matrix"]["include"]],
                [(m["os"], m["target"]) for m in workflow["jobs"]["build"]["strategy"]["matrix"]["include"]],
            )
            # Compare toolchain/setup and all explicit compiler environment inputs
            # at the cache step, before step-local signing/build variables exist.
            def cache_setup(document, job):
                setup = []
                env = dict(document.get("env", {}), **job.get("env", {}))
                for step in job["steps"]:
                    if step.get("uses") == cache:
                        return setup, env
                    if step.get("uses", "").startswith(("dtolnay/rust-toolchain@", "actions/setup-node@")):
                        setup.append((step["uses"], step.get("with", {})))
                    self.assertNotIn("GITHUB_ENV", step.get("run", ""))
                self.fail("cache step missing")
            self.assertEqual(cache_setup(warm, warmed), cache_setup(workflow, workflow["jobs"]["build"]))
            warm_steps = warmed["steps"]
            for step in steps:
                if step.get("run", "").startswith(("cargo build --release", "cross build --release")):
                    self.assertIn(step, warm_steps)
            if name == "desktop-release":
                for step_name in ("Install cross for Linux sidecar", "Build & stage gateway sidecar"):
                    self.assertIn(next(s for s in steps if s.get("name") == step_name), warm_steps)
                self.assertIn("src-tauri -> target", configured["with"]["workspaces"])
                self.assertNotIn("rebuild_v1750_desktop", str(workflow))
                shell_build = next(s for s in warm_steps if s.get("name") == "Build desktop Rust (no bundles)")
                self.assertEqual(shell_build["working-directory"], "src-tauri")
                self.assertEqual(shell_build["run"], "cargo tauri build --no-bundle --target ${{ matrix.target }}")
            else:
                for step_name in ("Install cross (Linux)", "Build frontend"):
                    self.assertIn(next(s for s in steps if s.get("name") == step_name), warm_steps)

    def test_release_cache_warming_is_main_only_read_only_and_secretless(self):
        path = ROOT / ".github/workflows/release-cache-warm.yml"
        self.assertTrue(path.is_file())
        body = path.read_text()
        warm = yaml.load(body, Loader=yaml.BaseLoader)
        self.assertEqual(set(warm["on"]), {"push", "schedule", "workflow_dispatch"})
        self.assertEqual(warm["on"]["push"]["branches"], ["main"])
        self.assertEqual(set(warm["on"]["push"]["paths"]), {
            "Cargo.lock", "Cargo.toml", "crates/**/Cargo.toml",
            "src-tauri/Cargo.lock", "src-tauri/Cargo.toml", "rust-toolchain*",
        })
        self.assertEqual(warm["on"]["schedule"], [{"cron": "0 18 * * 0"}])
        self.assertEqual(warm["permissions"], {"contents": "read"})
        self.assertNotIn("secrets", body.lower())
        self.assertEqual(set(warm["jobs"]), {"decide", "release", "desktop-release"})
        for job in warm["jobs"].values():
            if job is warm["jobs"]["decide"]:
                self.assertEqual(job["if"], "github.ref == 'refs/heads/main'")
            else:
                self.assertEqual(job["needs"], "decide")
                self.assertEqual(job["if"], "${{ !cancelled() && github.ref == 'refs/heads/main' && needs.decide.outputs.run != 'false' }}")
            self.assertNotIn("permissions", job)
            checkout = next(s for s in job["steps"] if s.get("uses", "").startswith("actions/checkout@"))
            self.assertEqual(checkout["with"]["persist-credentials"], "false")
            for step in job["steps"]:
                self.assertNotIn("cargo test", step.get("run", ""))
                self.assertNotIn("continue-on-error", step)
                self.assertNotIn("tauri-apps/tauri-action", step.get("uses", ""))

    def test_build_profiles_keep_debug_assertions(self):
        env = self.workflow["env"]
        for profile in ("DEV", "TEST"):
            self.assertEqual(env[f"CARGO_PROFILE_{profile}_DEBUG"], "0")
        self.assertFalse(any("ASSERTIONS" in key or "OPT_LEVEL" in key
                             for key in env))

    def test_execution_guards_preserve_blocking_coverage(self):
        full = "needs.settle.outputs.rust_mode != 'skip' && needs.settle.outputs.rust_mode != 'check'"
        active = "needs.settle.outputs.rust_mode != 'skip'"
        setup = {
            "Install Linux system deps (xcap screen-capture → wayland/xcb/gbm)",
            "Stub dashboard assets for compile",
        }
        platform = {
            "Install Linux system deps (xcap screen-capture → wayland/xcb/gbm)": "runner.os == 'Linux'",
            "Run every required Windows case on the native host": "runner.os == 'Windows'",
            "Test workflow stdio checkpoints (CLI, workflow-test-checkpoints)": "matrix.os == 'ubuntu-latest'",
            "Test workflow end to end (pilot, races, resume, F1b, F5-A) against the built CLI": "matrix.os == 'ubuntu-latest'",
        }
        self.assertEqual(self.jobs["rust"]["strategy"]["fail-fast"], "false")
        for job in ("rust", "dashboard", "python", "security"):
            self.assertNotIn("continue-on-error", self.jobs[job])
            for step in self.jobs[job]["steps"]:
                name = step.get("name", "")
                uses = step.get("uses", "")
                expected = None
                if job == "rust":
                    gate = active if name in setup or uses.startswith((
                        "actions/checkout@", "dtolnay/rust-toolchain@", "Swatinem/rust-cache@"
                    )) else full
                    if name == "Skip Rust for documentation and web-only changes":
                        gate = "needs.settle.outputs.rust_mode == 'skip'"
                    elif name == "Check release version bump":
                        gate = "needs.settle.outputs.rust_mode == 'check'"
                        self.assertEqual(step["run"], "cargo check --locked --workspace --exclude duduclaw-bridge --exclude duduclaw-dashboard")
                    elif name == "Test cli with app-compat feature (OS-only surface)":
                        gate += " && needs.settle.outputs.app_compat != 'false'"
                    if name in ("Test workspace doctests", "Test gateway doctests"):
                        suite = name.split()[1]
                        gate = "!cancelled() && " + gate + (
                            f" && (steps.nextest_{suite}.outcome == 'success'"
                            f" || steps.nextest_{suite}.outcome == 'failure')")
                    if name in platform:
                        gate += " && (" + platform[name] + ")"
                    if name in {
                        "Verify edition ceiling in release artifacts with assertions",
                        "Check gateway with OpenTelemetry feature",
                        "Test cli with app-compat feature (OS-only surface)",
                        "Test workspace doctests",
                        "Test gateway doctests",
                        "Run every required Windows case on the native host",
                    }:
                        gate += " && (matrix.shard == 1)"
                    if name.startswith(("Summarize nextest", "Upload nextest")):
                        gate = "always() && " + gate
                    expected = "${{ " + gate + " }}"
                with self.subTest(job=job, step=name or uses):
                    self.assertEqual(step.get("if"), expected)
                    self.assertNotIn("continue-on-error", step)
                    self.assertNotIn("clippy", step.get("run", ""))
        # Empty/unknown outputs satisfy both negative comparisons and run full CI.
        for mode in ("", "full", "unexpected"):
            self.assertTrue(mode != "skip" and mode != "check")
        for command in self.commands("rust"):
            self.assertIn("--locked", command)

    def test_settle_only_delays_pr_runs(self):
        settle = self.jobs["settle"]
        self.assertNotIn("if", settle)
        delay = next(s for s in settle["steps"] if "sleep" in s.get("run", ""))
        self.assertEqual(delay["if"], "github.event_name == 'pull_request'")
        self.assertEqual(shlex.split(delay["run"]), ["sleep", "60"])

    def test_default_and_optional_feature_coverage_remains(self):
        commands = self.commands("rust")
        workspace = next(c for c in commands if c[:3] == ["cargo", "nextest", "run"]
                         and "--workspace" in c)
        exclusions = [workspace[i + 1] for i, arg in enumerate(workspace) if arg == "--exclude"]
        self.assertEqual(exclusions, ["duduclaw-bridge", "duduclaw-dashboard", "duduclaw-gateway"])
        self.assertNotIn("--no-default-features", workspace)
        self.assertIn("--no-fail-fast", workspace)
        for selector in ("--lib", "--tests", "--bins", "--doc", "-E", "--filter-expr"):
            self.assertNotIn(selector, workspace)
        for package, feature, target in (
            ("duduclaw-core", "app-compat", "test"),
            ("duduclaw-cli", "app-compat", "test"),
            ("duduclaw-gateway", "otel", "check"),
            ("duduclaw-cli", "workflow-test-checkpoints", "test"),
        ):
            with self.subTest(feature=feature, package=package):
                self.assertTrue(any(c[:2] == ["cargo", target]
                                    and package in c and feature in c for c in commands))
        gateway = next(c for c in commands if c[:3] == ["cargo", "nextest", "run"]
                       and "-p" in c and "duduclaw-gateway" in c)
        self.assertEqual(gateway[3:], ["--locked", "--no-fail-fast", "-p", "duduclaw-gateway",
            "--no-default-features", "-E", "not (test(three_page_pilot_) or test(security_race_tests) or test(resume_tests) or test(f1b_e2e_tests) or test(f5a_e2e_tests))"])
        docs = [c for c in commands if c[:2] == ["cargo", "test"] and "--doc" in c]
        self.assertEqual(len(docs), 2)
        self.assertEqual(docs[0], ["cargo", "test", "--locked", "--doc", "--no-fail-fast"] + workspace[5:])
        self.assertEqual(docs[1][:8], ["cargo", "test", "--locked", "--doc", "--no-fail-fast",
                                     "-p", "duduclaw-gateway", "--no-default-features"])
        self.assertEqual(docs[1][8:], ["--"] + [arg for name in E2E_FILTERS
                         for arg in ("--skip", name.removeprefix("workflow::"))])

    def test_windows_process_tree_tests_reserve_all_cpus(self):
        config = tomllib.loads((ROOT / ".config/nextest.toml").read_text())
        self.assertEqual(config["profile"]["ci"]["overrides"], [{
            "filter": "package(duduclaw-fork) & test(/^test_runner::tests::windows::/)",
            "threads-required": "num-cpus",
        }])
        # Nextest follows the profile chain when resolving per-test overrides.
        self.assertEqual(config["profile"]["ci-native"]["inherits"], "ci")

    def test_nextest_profiles_and_reports(self):
        config = tomllib.loads((ROOT / ".config/nextest.toml").read_text())
        self.assertEqual({key: value for key, value in config["profile"]["ci"].items()
                          if key != "overrides"}, {
            "retries": 0, "fail-fast": False,
            "slow-timeout": {"period": "60s", "terminate-after": 10},
            "final-status-level": "flaky", "junit": {"path": "junit.xml"},
        })
        self.assertEqual(config["profile"]["ci-native"], {"inherits": "ci", "retries": 1})
        self.assertEqual(self.jobs["rust"]["env"]["NEXTEST_PROFILE"],
            "${{ (github.event_name == 'pull_request' || matrix.os == 'ubuntu-latest') && 'ci' || 'ci-native' }}")
        steps = self.jobs["rust"]["steps"]
        install = next(s for s in steps if s.get("name") == "Install nextest")
        self.assertEqual(install["uses"], "taiki-e/install-action@1919c4a95903d2c33e4a221102edca87309fc14b")
        self.assertEqual(install["with"]["tool"], "nextest@0.9.150")
        for suite, name in (("workspace", "Test (no dashboard)"), ("gateway", "Test gateway")):
            index = next(i for i, step in enumerate(steps) if step.get("name") == name)
            self.assertEqual(steps[index]["id"], f"nextest_{suite}")
            summary = steps[index + 1]
            self.assertIn("always()", summary["if"])
            self.assertIn('"target/nextest/$NEXTEST_PROFILE/junit.xml"', summary["run"])
            self.assertIn(f'"target/nextest/reports/{suite}.xml"', summary["run"])
        artifact = steps[-1]
        self.assertEqual(artifact["uses"], "actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9")
        self.assertIn("always()", artifact["if"])
        self.assertEqual(artifact["with"]["name"],
                         "nextest-${{ matrix.os }}-shard${{ matrix.shard }}")
        self.assertEqual(artifact["with"]["path"], "target/nextest/reports/*.xml")
        self.assertEqual(artifact["with"]["retention-days"], "7")
        self.assertIn("-p 'test_ci*.py'", str(self.jobs["python"]))

    def test_classifier_uses_base_and_publishes_only_successful_outputs(self):
        settle = self.jobs["settle"]
        checkout = settle["steps"][1]
        self.assertEqual(checkout["with"], {"fetch-depth": "0", "persist-credentials": "false"})
        step = settle["steps"][2]
        self.assertEqual(step["env"], {"BASE_SHA": "${{ github.event.pull_request.base.sha }}",
                                        "HEAD_SHA": "${{ github.event.pull_request.head.sha }}"})
        self.assertNotIn("${{", step["run"])
        self.assertIn('git show "$BASE_SHA:scripts/ci_classify.py"', step["run"])
        self.assertIn('python3 -I "$RUNNER_TEMP/ci_classify.py"', step["run"])
        self.assertEqual(step["continue-on-error"], "true")
        self.assertEqual(set(settle["outputs"]), {"rust_mode", "dashboard", "app_compat"})
        for key in settle["outputs"]:
            self.assertEqual(settle["outputs"][key], "${{ steps.classify.outputs." + key + " }}")
        # Execute the actual workflow shell for non-PR and first-rollout failure cases.
        for event in ("push", "schedule", "workflow_dispatch", "pull_request"):
            with self.subTest(event=event), tempfile.TemporaryDirectory() as directory:
                output = Path(directory) / "output"
                output.touch()
                env = dict(os.environ, GITHUB_EVENT_NAME=event, BASE_SHA="0" * 40,
                           HEAD_SHA="1" * 40, RUNNER_TEMP=directory, GITHUB_OUTPUT=str(output))
                subprocess.run(["bash", "-e", "-c", step["run"]], cwd=ROOT,
                               env=env, check=True, capture_output=True)
                expected = ("" if event == "pull_request" else
                            "rust_mode=full\ndashboard=run\napp_compat=true\n")
                self.assertEqual(output.read_text(), expected)

    def test_classifier_failure_cannot_publish_partial_skip_outputs(self):
        shell = self.jobs["settle"]["steps"][2]["run"]
        with tempfile.TemporaryDirectory() as directory:
            repo = Path(directory)
            def git(*args):
                return subprocess.check_output(["git", *args], cwd=repo, text=True).strip()
            git("init", "-q")
            git("config", "user.name", "CI test")
            git("config", "user.email", "ci@example.invalid")
            git("config", "commit.gpgsign", "false")
            script = repo / "scripts/ci_classify.py"
            script.parent.mkdir()
            script.write_text('import os\nfrom pathlib import Path\n'
                              'Path(os.environ["GITHUB_OUTPUT"]).write_text("rust_mode=skip\\n")\n'
                              'raise RuntimeError("classification failed")\n')
            git("add", ".")
            git("commit", "-qm", "base classifier")
            base = git("rev-parse", "HEAD")
            script.write_text('raise AssertionError("must not execute head script")\n')
            git("add", ".")
            git("commit", "-qm", "untrusted head")
            output = repo / "output"
            output.touch()
            env = dict(os.environ, GITHUB_EVENT_NAME="pull_request", BASE_SHA=base,
                       HEAD_SHA=git("rev-parse", "HEAD"), RUNNER_TEMP=directory,
                       GITHUB_OUTPUT=str(output))
            result = subprocess.run(["bash", "-e", "-c", shell], cwd=repo,
                                    env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("classification failed", result.stderr)
            self.assertNotIn("must not execute head script", result.stderr)
            self.assertEqual(output.read_text(), "")

    def test_release_action_pins_and_cache_write_limits(self):
        for name in ("release", "desktop-release", "docker-image", "computer-use-image", "release-cache-warm", "linux-release-smoke"):
            text = (ROOT / f".github/workflows/{name}.yml").read_text()
            workflow = yaml.load(text, Loader=yaml.BaseLoader)
            for job in workflow["jobs"].values():
                for step in job.get("steps", []):
                    if uses := step.get("uses"):
                        if uses != "dtolnay/rust-toolchain@stable":
                            self.assertRegex(uses, r"^[\w-]+/[\w-]+@[a-f0-9]{40}$")
            if name in {"desktop-release", "release-cache-warm"}:
                installs = [shlex.split(line) for job in workflow["jobs"].values()
                            for step in job.get("steps", [])
                            for line in step.get("run", "").splitlines()
                            if line.strip().startswith("cargo install tauri-cli")]
                self.assertEqual(installs, [["cargo", "install", "tauri-cli", "--version",
                                             "=2.12.4", "--locked"]])
            if name == "release":
                self.assertEqual(workflow["jobs"]["build"]["strategy"]["fail-fast"], "false")
            if name == "computer-use-image":
                builds = [step for step in workflow["jobs"]["build"]["steps"]
                          if step.get("uses", "").startswith("docker/build-push-action@")]
                self.assertEqual(len(builds), 2)
                for step in builds:
                    self.assertEqual(step["with"]["cache-from"], "type=gha,scope=computer-use-${{ matrix.arch }}")
                    self.assertIn("github.ref == 'refs/heads/main'", step["with"]["cache-to"])
                    self.assertTrue(step["with"]["cache-to"].endswith("|| '' }}"))
        install = next(s for s in self.jobs["security"]["steps"]
                       if s.get("name") == "Install cargo-audit")
        self.assertEqual(install["uses"], "taiki-e/install-action@1919c4a95903d2c33e4a221102edca87309fc14b")
        self.assertEqual(install["with"]["tool"], "cargo-audit@0.22.2")

    def test_serial_e2e_uses_restored_production_cli(self):
        step = next(s for s in self.jobs["rust"]["steps"]
                    if "DUDUCLAW_P1_PILOT_EVIDENCE_DIR" in s.get("env", {}))
        self.assertIn("matrix.os == 'ubuntu-latest'", step["if"])
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

    def test_default_gateway_port_is_consistent_across_shipping_surfaces(self):
        expected = {
            "crates/duduclaw-core/src/config.rs":
                "pub const DEFAULT_GATEWAY_PORT: u16 = 18889;",
            "crates/duduclaw-cli/src/lib.rs":
                ".default(duduclaw_core::DEFAULT_GATEWAY_PORT)",
            "src-tauri/src/lifecycle.rs": "pub const DEFAULT_PORT: u16 = 18889;",
            "config/duduclaw.example.toml": "port = 18889",
            "container/Dockerfile.server": "EXPOSE 18789 18889",
            "container/Dockerfile.edition-smoke": "EXPOSE 18789 18889",
            "container/docker-compose.quickstart.yml": '"18889:18889"',
            "web/vite.config.ts": "http://127.0.0.1:18889",
            "clients/vscode/package.json": "http://127.0.0.1:18889",
            "clients/vscode/src/config.ts":
                "body.service === 'duduclaw-gateway'",
            "clients/obsidian/duduclaw/src/main.ts":
                "body.service === 'duduclaw-gateway'",
            "clients/streamdeck/src/plugin.ts":
                "body.service === 'duduclaw-gateway'",
            "clients/chrome/options.js": "http://127.0.0.1:18889",
            "clients/chrome/sw.js": "gatewayMigrationPending: !selected",
            "distribution/demo-sandbox/home-seed/config.toml": "port = 18889",
            "crates/duduclaw-gateway/src/handlers/system_status.rs":
                '"localhost:18889"',
            "README.md": "http://localhost:18889",
            "README.zh-TW.md": "http://localhost:18889",
            "README.ja.md": "http://localhost:18889",
        }
        for relative, needle in expected.items():
            with self.subTest(path=relative):
                self.assertIn(needle, (ROOT / relative).read_text())

        for relative in expected:
            if relative in {
                "crates/duduclaw-core/src/config.rs",
                "src-tauri/src/lifecycle.rs",
                "clients/vscode/src/config.ts",
                "clients/obsidian/duduclaw/src/main.ts",
                "clients/streamdeck/src/plugin.ts",
                "clients/chrome/sw.js",
                "container/Dockerfile.server",
                "container/Dockerfile.edition-smoke",
            }:
                continue
            with self.subTest(no_legacy_default=relative):
                self.assertNotIn("18789", (ROOT / relative).read_text())

        for relative in (
            "docker-compose.yml",
            "docker-compose.quickstart.yml",
            "container/docker-compose.quickstart.yml",
            "distribution/nas/docker-compose.yml",
        ):
            with self.subTest(config_precedence=relative):
                self.assertNotIn("DUDUCLAW_PORT=", (ROOT / relative).read_text())

        for relative in (
            "docker-compose.yml",
            "docker-compose.quickstart.yml",
            "distribution/nas/docker-compose.yml",
        ):
            text = (ROOT / relative).read_text()
            with self.subTest(persisted_platform=relative):
                self.assertIn('"18789:18789"', text)
                self.assertIn("DUDUCLAW_INITIAL_PORT=18789", text)
                self.assertIn("localhost:18789/healthz", text)

        self.assertNotIn(
            "DUDUCLAW_INITIAL_PORT",
            (ROOT / "container/docker-compose.quickstart.yml").read_text(),
        )

        vscode_config = (ROOT / "clients/vscode/src/config.ts").read_text()
        self.assertIn("const selected = gatewayUrl();", vscode_config)
        identity_guard = vscode_config.split(
            "export async function ensureGatewayIdentity", 1
        )[1].split("/** Workspace-scoped", 1)[0]
        self.assertNotIn("explicitGatewayUrl()", identity_guard)

        streamdeck = (ROOT / "clients/streamdeck/src/plugin.ts").read_text()
        self.assertIn(": NEW_GATEWAY;", streamdeck)

        obsidian = (
            ROOT / "clients/obsidian/duduclaw/src/main.ts"
        ).read_text()
        self.assertIn("async updateGatewayUrl", obsidian)
        self.assertIn("view.resetConnection();", obsidian)
        self.assertIn("this.settings.accessToken = '';", obsidian)
        self.assertIn("this.wsGateway !== this.plugin.gatewayBase()", obsidian)

        chrome_sidepanel = (ROOT / "clients/chrome/sidepanel.js").read_text()
        self.assertIn("function resetConnections", chrome_sidepanel)
        self.assertIn("rpcPending.clear();", chrome_sidepanel)
        self.assertIn(
            "ch.accessToken || ch.gatewayUrl || ch.refreshToken",
            chrome_sidepanel,
        )

        self.assertIn(
            "port: 18789",
            (ROOT / "distribution/umbrel/duduclaw/umbrel-app.yml").read_text(),
        )


if __name__ == "__main__":
    unittest.main()
