"""Regression tests for the source inventory guard (no Rust compilation needed)."""
import json
from pathlib import Path
import tempfile
import unittest

from check_spawn_inventory import check, discover, scan_text


class SpawnInventoryTests(unittest.TestCase):
    path = "crates/example/src/lib.rs"

    def scan(self, code):
        return scan_text(code, self.path)

    def registry(self, sites):
        return {"schema_version": 1, "entries": [dict(
            symbol=s.symbol, apis=s.apis, effects=["process_spawn"],
            identity="employee", migration="pending", rationale="Must use backend",
        ) for s in sites]}

    def test_isolated_child_classification_is_exact_and_cannot_exempt_gateway_spawns(self):
        sites = scan_text('impl Bridge { async fn connect() { Command::new("fixed").spawn(); } }',
                          "crates/duduclaw-api-worker/src/linux.rs")
        registry = self.registry(sites)
        entry = registry["entries"][0]
        entry["migration"] = "isolated_child"
        self.assertEqual(check(sites, registry), [])
        entry["identity"] = "gateway"
        self.assertTrue(any("invalid isolated runtime child" in error for error in check(sites, registry)))
        entry["identity"] = "employee"
        entry["symbol"] = "crates/example/src/lib.rs::Bridge::connect"
        self.assertTrue(any("invalid isolated runtime child" in error for error in check(sites, registry)))
        entry["symbol"] = sites[0].symbol
        entry["apis"]["terminal_output"] = 1
        self.assertTrue(any("invalid isolated runtime child" in error for error in check(sites, registry)))

    def test_native_adapter_exec_classification_is_exact(self):
        sites = scan_text('fn run() { std::process::Command::new("fixed").exec(); }',
                          "crates/duduclaw-api-worker/src/native_main.rs")
        registry = self.registry(sites)
        entry = registry["entries"][0]
        entry["migration"] = "isolated_child"
        self.assertEqual(check(sites, registry), [])
        entry["symbol"] = "crates/duduclaw-gateway/src/runtime/codex.rs::run"
        self.assertTrue(any("invalid isolated runtime child" in error for error in check(sites, registry)))

    def test_array_signature_and_macro_body_are_not_skipped(self):
        sites = self.scan('fn run() -> [u8; 4] { Command::new("x").spawn(); [0; 4] }')
        self.assertEqual(sites[0].apis, {"command_constructor": 1, "terminal_spawn": 1})
        sites = self.scan('macro_rules! launch { () => { Command::new("x").spawn() }; }')
        self.assertEqual(sites[0].symbol, self.path + "::<module>")
        self.assertEqual(sites[0].apis, {"command_constructor": 1, "terminal_spawn": 1})

    def test_alias_and_multiline_are_counted(self):
        sites = self.scan('''use tokio::process::Command as ChildCommand;
          async fn start() {
            let mut child = ChildCommand
              :: new("/bin/sh");
            child
              .spawn();
          }''')
        self.assertEqual(sites[0].apis, {"command_constructor": 1, "terminal_spawn": 1})

    def test_imported_raw_syscalls_are_counted(self):
        sites = self.scan('use nix::unistd::{fork as clone_process}; fn launch() { unsafe { clone_process(); } }')
        self.assertEqual(sites[0].apis, {"raw_fork": 1})
        sites = self.scan('use libc as c; fn launch() { unsafe { c::execve(p, a, e); } }')
        self.assertEqual(sites[0].apis, {"raw_execve": 1})

    def test_type_alias_and_command_parameter(self):
        sites = self.scan('''type Launcher = std::process::Command;
            fn make() { Launcher::new("x").status(); }
            fn run(cmd: &mut std::process::Command) { cmd.output(); }''')
        self.assertEqual(len(sites), 2)
        self.assertEqual(sites[1].apis, {"terminal_output": 1})

    def test_command_ufcs_and_alias_terminal_calls_are_counted(self):
        for method in ("spawn", "output", "status", "exec"):
            for declaration, name in (("", "std::process::Command"),
                    ("use tokio::process::Command as ChildCommand;", "ChildCommand"),
                    ("type Launcher = std::process::Command;", "Launcher")):
                with self.subTest(method=method, name=name):
                    sites = self.scan(declaration + f"fn run(cmd: &mut {name}) {{ {name}::{method}(cmd); }}")
                    self.assertEqual(sites[0].apis, {"terminal_" + method: 1})

    def test_comments_and_rust_literals_are_inert(self):
        sites = self.scan('''fn inert() {
          // Command::new("x").spawn();
          /* nested /* Command::new("x") */ .spawn() */
          let text = "Command::new(\\\"x\\\").spawn()";
          let raw = r###"Command::new("x").spawn()"###;
          let bytes = br#"Command::new("x").spawn()"#;
          let char = '}';
        }''')
        self.assertEqual(sites, [])

    def test_impl_trait_parameter_does_not_hide_pty(self):
        sites = self.scan('''impl Handle {
            fn run(cmd: CommandBuilder, notify: impl FnOnce()) {
                native_pty_system(); pair.slave.spawn_command(cmd);
            }
        }''')
        self.assertEqual(sites[0].symbol, self.path + "::Handle::run")
        self.assertEqual(sites[0].apis, {"pty_native_pty_system": 1, "terminal_spawn_command": 1})

    def test_nested_functions_are_not_double_counted(self):
        sites = self.scan('''fn outer() {
            fn inner() { Command::new("x").spawn(); }
            Command::new("y").output();
        }''')
        self.assertEqual(len(sites), 2)
        self.assertEqual(sites[0].apis, {"command_constructor": 1, "terminal_output": 1})
        self.assertEqual(sites[1].apis, {"command_constructor": 1, "terminal_spawn": 1})

    def test_unclassified_and_changed_counts_fail(self):
        old = self.scan('fn start() { Command::new("x").spawn(); }')
        registry = self.registry(old)
        self.assertEqual(check(old, registry), [])
        changed = self.scan('fn start() { Command::new("x").spawn(); cmd.spawn(); }')
        self.assertIn("changed process effects", check(changed, registry)[0])
        extra = self.scan('fn start() { Command::new("x").spawn(); } fn bypass() { cmd.spawn(); }')
        self.assertTrue(any("unclassified" in error for error in check(extra, registry)))

    def test_removal_duplicate_and_missing_rationale_fail(self):
        sites = self.scan('fn start() { Command::new("x").spawn(); }')
        registry = self.registry(sites)
        self.assertIn("stale entry", check([], registry)[0])
        registry["entries"].append(registry["entries"][0].copy())
        self.assertTrue(any("duplicate" in error for error in check(sites, registry)))
        registry = self.registry(sites)
        registry["entries"][0]["rationale"] = " "
        self.assertIn("missing effect/rationale", check(sites, registry)[0])

    def test_privileged_service_classification_cannot_exempt_employee_or_gateway(self):
        sites = self.scan('fn launch() { Command::new("systemctl").spawn(); }')
        registry = self.registry(sites)
        entry = registry["entries"][0]
        entry["migration"] = "privileged_service"
        entry["rationale"] = "Root updater controls its fixed service set; no employee argv."
        for identity in ("employee", "gateway", "operator", "parse_worker", "import_worker"):
            entry["identity"] = identity
            self.assertTrue(any("invalid privileged service exemption" in error
                                for error in check(sites, registry)))
        for identity in ("spawn_daemon", "trampoline", "updater", "downloader", "host_service"):
            entry["identity"] = identity
            self.assertEqual(check(sites, registry), [])

    def test_isolated_helper_exception_cannot_exempt_gateway_or_another_symbol(self):
        sites = self.scan('fn start() { Command::new("x").spawn(); }')
        registry = self.registry(sites)
        entry = registry["entries"][0]
        entry["migration"] = "isolated_helper"
        entry["identity"] = "employee"
        self.assertTrue(any("invalid isolated helper exemption" in error
                            for error in check(sites, registry)))
        entry["symbol"] = "crates/duduclaw-execution/src/program_helper.rs::execute_command"
        for identity in ("gateway", "operator", "host_service", "test"):
            entry["identity"] = identity
            self.assertTrue(any("invalid isolated helper exemption" in error
                                for error in check(sites, registry)))
        entry["identity"] = "employee"
        self.assertFalse(any("invalid isolated helper exemption" in error
                             for error in check(sites, registry)))
        entry["symbol"] = "crates/duduclaw-execution/src/recording_helper.rs::run_driver"
        self.assertFalse(any("invalid isolated helper exemption" in error
                             for error in check(sites, registry)))
        entry["symbol"] = "crates/duduclaw-execution/src/recording_helper.rs::main_entry"
        self.assertTrue(any("invalid isolated helper exemption" in error
                            for error in check(sites, registry)))

    def test_platform_factories_and_raw_exec_are_counted(self):
        sites = self.scan('''fn start() {
          let cmd = duduclaw_core::platform::async_command_for("x");
          unsafe { libc::execve(ptr, argv, env); }
        }''')
        self.assertEqual(sites[0].apis, {"platform_command_factory": 1, "raw_execve": 1})
        self.assertEqual(self.scan('fn command_for() {}'), [])

    def test_discover_includes_new_crates_and_cfg_branches(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / self.path
            path.parent.mkdir(parents=True)
            path.write_text('#[cfg(windows)] fn launch() { Command::new("x"); }')
            (path.parent / "tests.rs").write_text('fn fixture() { Command::new("x"); }')
            sites = discover(root)
            self.assertEqual(len(sites), 2)
            self.assertFalse(sites[0].test_only)
            self.assertTrue(sites[1].test_only)

    def test_cfg_duplicate_symbol_aggregates_api_counts(self):
        sites = self.scan('''#[cfg(unix)] fn run() { Command::new("x").spawn(); }
          #[cfg(windows)] fn run() { Command::new("y").spawn(); }''')
        self.assertEqual(len(sites), 1)
        self.assertEqual(sites[0].apis, {"command_constructor": 2, "terminal_spawn": 2})

    def test_line_number_change_does_not_require_registry_update(self):
        old = self.scan('fn start() { Command::new("x").spawn(); }')
        new = self.scan('\n\nfn start() { Command::new("x").spawn(); }')
        self.assertEqual(check(new, self.registry(old)), [])


    def test_backend_aliases_are_not_hidden_by_extracted_builders(self):
        sites = self.scan('''use duduclaw_core::process_tree::BackendChild as ManagedChild;
          async fn run(cmd: &mut TokioCommand) { ManagedChild::spawn_class(cmd, Mail).await; }
        ''')
        self.assertEqual(sites[0].apis, {"backend_spawn_class": 1})

    def test_authorized_backend_dispatch_remains_inventoried(self):
        sites = self.scan('async fn execute(backend: Backend) { backend.spawn_authorized(policy, authority).await; }')
        self.assertEqual(sites[0].apis, {"terminal_spawn_authorized": 1})
        changed = self.scan('async fn execute(backend: Backend) { backend.spawn(policy).await; }')
        self.assertTrue(any("changed process effects" in error
                            for error in check(changed, self.registry(sites))))

    def test_backend_adapters_are_inventoried_without_native_spawn(self):
        sites = self.scan('''async fn start(cmd: &mut Command) {
          execution_gate::spawn(cmd, EmployeeMain).await;
          duduclaw_core::execution_gate::output(cmd, Judge).await;
          BackendChild::spawn_class(cmd, Mail).await;
          execution_blocking::spawn(cmd, Discovery);
        }''')
        self.assertEqual(sites[0].apis, {"backend_output": 1, "backend_spawn": 2, "backend_spawn_class": 1})
        changed = self.scan('async fn start(cmd: &mut Command) { cmd.spawn(); }')
        self.assertTrue(any("changed process effects" in error
                            for error in check(changed, self.registry(sites))))


if __name__ == "__main__":
    unittest.main()
