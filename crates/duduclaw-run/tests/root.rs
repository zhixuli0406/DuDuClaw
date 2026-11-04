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
