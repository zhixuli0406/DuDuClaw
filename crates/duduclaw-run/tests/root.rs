//! Explicit root-only primitive acceptance, isolated from libtest's threads.
//! These tests do not establish spawnd admission, systemd or live-runtime acceptance.
fn fixture(scenario: &str) {
    #[cfg(feature = "root-test-fixture")]
    {
        assert!(cfg!(target_os = "linux"), "iso_root_ tests require native Linux");
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_duduclaw-run-root-fixture"))
            .env("DUDUCLAW_ROOT_TEST",scenario)
            .stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())
            .spawn().expect("start single-threaded root fixture");
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(60);
        let mut timed_out=false;
        while child.try_wait().expect("fixture status").is_none() {
            if std::time::Instant::now()>=deadline {
                timed_out=true;
                child.kill().expect("stop timed-out fixture supervisor");
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let output=child.wait_with_output().expect("reap fixture and its bounded output");
        assert!(!timed_out,"root fixture {scenario} exceeded parent deadline");
        assert!(output.status.success(), "root fixture {scenario} failed: {}",
            String::from_utf8_lossy(&output.stderr));
    }
    #[cfg(not(feature = "root-test-fixture"))]
    panic!("{scenario} requires --features root-test-fixture; it was not executed");
}

#[test]
#[ignore = "requires root Linux with namespaces and Landlock; --features root-test-fixture"]
fn iso_root_same_employee_pathname_socket_scm_rights_impossible() { fixture("d8"); }

#[test]
#[ignore = "requires root Linux with namespaces and Landlock; --features root-test-fixture"]
fn iso_root_employee_cannot_read_gateway_data_dir() { fixture("gateway"); }

#[test]
#[ignore = "requires root Linux with network namespaces; --features root-test-fixture"]
fn iso_root_employee_cannot_reach_metadata() { fixture("metadata"); }

#[test]
#[ignore = "requires root Linux with seccomp and Landlock; --features root-test-fixture"]
fn iso_root_employee_cannot_ptrace_other_run() { fixture("ptrace"); }

#[test]
#[ignore = "requires root Linux with statx and close_range; --features root-test-fixture"]
fn iso_root_cyclic_fd_reorder_has_exact_whitelist() { fixture("fds"); }

#[test]
#[ignore = "requires root Linux with mount/PID namespaces and devpts; --features root-test-fixture"]
fn iso_root_mount_tree_pty_and_proc_readbacks() { fixture("mounts"); }

#[test]
#[ignore = "requires root Linux with private detached mounts; --features root-test-fixture"]
fn iso_root_detached_mount_private_propagation() { fixture("propagation"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_full_launch_elf_capability_fd_handoff() { fixture("launch-elf"); }

#[test]
#[ignore = "requires root Linux full trampoline and /bin/dash; --features root-test-fixture"]
fn iso_root_full_launch_script_bypasses_shebang() { fixture("launch-script"); }

#[test]
#[ignore = "requires root Linux full trampoline and devpts; --features root-test-fixture"]
fn iso_root_full_launch_pty_control_terminal() { fixture("launch-pty"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_full_launch_reports_exec_failure() { fixture("launch-exec-failure"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_full_launch_forwarder_failure_terminates_run() { fixture("launch-forwarder-failure"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_full_launch_cancellation_forwards_signal() { fixture("launch-cancel"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_full_launch_startup_cancellation_race() { fixture("launch-cancel-startup"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_full_launch_reaps_double_fork_descendants() { fixture("launch-teardown"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_full_launch_supervisor_failure_blocks_runtime() { fixture("launch-supervisor-failure"); }

// Production sealed-manifest transport plus the actual trampoline. The fixture
// supplies root-owned plans; these are not gateway/spawnd/systemd admission tests.
#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_e2e_sealed_bootstrap_trampoline_uid_namespaces_fds() { fixture("launch-elf"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture; not D8 admission"]
fn iso_root_e2e_sealed_bootstrap_trampoline_double_fork_teardown() { fixture("launch-teardown"); }

#[test]
#[ignore = "requires root Linux full trampoline; --features root-test-fixture"]
fn iso_root_e2e_sealed_bootstrap_fast_exit_preserves_status() { fixture("launch-fast-exit"); }

// HX3b in-run helper launcher (D8 revision §7). Need root, cgroup v2 with
// cgroup.kill, Landlock and the open-time-credentials fix (CVE-2021-4197);
// they create and remove a fixture cgroup under /sys/fs/cgroup.
#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_launcher_forked_before_runtime_landlock() { fixture("helper-proc"); }

/// Same scenario: the decisive check is the dumpable helper, whose /proc stays
/// invisible to the same-uid runtime only because its Landlock domain is not
/// nested in the runtime's. Meaningful on any Yama setting (Yama only gates
/// PTRACE_ATTACH); run it on Debian 12 (ptrace_scope=0) as §7 asks.
#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_runtime_cannot_access_proc_of_helper() { fixture("helper-proc"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_launcher_fd_table_exact() { fixture("helper-launcher-fds"); }

/// The isolation receipt (7) is issued only after init read the launcher's
/// ready byte; at that receipt the launcher has already closed its status and
/// ready pipes (checked in the same scenario).
#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_launcher_ready_before_all_isolated() { fixture("helper-launcher-fds"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_helper_fd_table_exact() { fixture("helper-fds"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_only_supervisor_oom_protected() { fixture("helper-oom"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_helper_cannot_open_home_config_tmp_devshm() { fixture("helper-fs"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_offline_helper_socket_eperm() { fixture("helper-socket"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_runtime_cannot_exec_helpers() { fixture("helper-exec"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_runtime_cannot_precreate_helper_scratch() { fixture("helper-scratch"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_runtime_descendants_killed_on_runtime_exit() { fixture("helper-descendants"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_launcher_refuses_start_after_sigterm() { fixture("helper-sigterm"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_init_wait_bounded_with_stopped_launcher() { fixture("helper-stopped"); }

#[test]
#[ignore = "requires root Linux, cgroup v2 + Landlock, full trampoline with launcher; --features root-test-fixture"]
fn iso_root_helper_helper_raise_rlimit_denied() { fixture("helper-rlimit"); }
