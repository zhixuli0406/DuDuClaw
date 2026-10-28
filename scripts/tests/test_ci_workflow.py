"""Protect full CI coverage when reducing obsolete runs and build overhead."""

from pathlib import Path
import json
import re
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

    def test_prs_run_linux_and_main_nightly_manual_run_all_platforms(self):
        matrix = self.jobs["rust"]["strategy"]["matrix"]["os"]
        expression = re.fullmatch(
            r"\$\{\{ fromJSON\(github.event_name == 'pull_request' && '(.*?)' \|\| '(.*?)'\) \}\}",
            matrix,
        )
        self.assertIsNotNone(expression, "matrix must distinguish PRs from full platform runs")
        self.assertEqual(json.loads(expression[1]), ["ubuntu-latest"])
        for event in ("push", "schedule", "workflow_dispatch"):
            with self.subTest(event=event):
                self.assertIn(event, self.workflow["on"])
                self.assertEqual(json.loads(expression[2]),
                                 ["ubuntu-latest", "macos-latest", "windows-latest"])
        self.assertEqual(self.workflow["on"]["schedule"], [{"cron": "0 18 * * *"}])

    def test_ci_and_tag_docker_cache_write_policy(self):
        rust_caches = [
            step
            for job in self.jobs.values()
            for step in job.get("steps", [])
            if step.get("uses", "").startswith("Swatinem/rust-cache@")
        ]
        self.assertEqual(len(rust_caches), 2)
        for step in rust_caches:
            self.assertEqual(
                step["with"]["save-if"],
                "${{ github.ref == 'refs/heads/main' }}",
            )

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
        self.assertEqual(step["if"], "runner.os == 'Windows'")
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
            self.assertEqual(configured["with"]["shared-key"], name + "-${{ matrix.target }}")
            self.assertEqual(configured["with"]["save-if"], "false")
            self.assertNotIn("key", configured["with"])
            warm_options = dict(warm_cache["with"])
            self.assertEqual(warm_options.pop("save-if"), "${{ github.ref == 'refs/heads/main' }}")
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
                self.assertIn("src-tauri -> target", configured["with"]["workspaces"])
                self.assertNotIn("rebuild_v1750_desktop", str(workflow))
                shell_build = next(s for s in warm_steps if s.get("name") == "Build desktop Rust (no bundles)")
                self.assertEqual(shell_build["working-directory"], "src-tauri")
                self.assertEqual(shell_build["run"], "cargo tauri build --no-bundle --target ${{ matrix.target }}")
            else:
                for step_name in ("Install cross (aarch64-linux)", "Build frontend"):
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
        self.assertEqual(set(warm["jobs"]), {"release", "desktop-release"})
        for job in warm["jobs"].values():
            self.assertEqual(job["if"], "github.ref == 'refs/heads/main'")
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
        conditional_steps = {
            "Install Linux system deps (xcap screen-capture → wayland/xcb/gbm)":
                "runner.os == 'Linux'",
            "Test gateway Discord voice TLS transport": "runner.os == 'Linux'",
            "Run every required Windows case on the native host": "runner.os == 'Windows'",
            "Test workflow stdio checkpoints (CLI, workflow-test-checkpoints)":
                "matrix.os == 'ubuntu-latest'",
            "Test workflow end to end (pilot, races, resume, F1b, F5-A) against the built CLI":
                "matrix.os == 'ubuntu-latest'",
        }
        self.assertEqual(self.jobs["rust"]["strategy"]["fail-fast"], "false")
        for job in ("rust", "dashboard", "python", "security"):
            self.assertNotIn("continue-on-error", self.jobs[job])
            for step in self.jobs[job]["steps"]:
                name = step.get("name", step.get("uses"))
                with self.subTest(job=job, step=name):
                    expected = conditional_steps.get(name) if job == "rust" else None
                    self.assertEqual(step.get("if"), expected)
                    self.assertNotIn("continue-on-error", step)
                    self.assertNotIn("clippy", step.get("run", ""))

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
