//! Restore Docker's namespace restrictions on non-Codex children in containers.
//! Filters stack and survive exec. Only Codex's bubblewrap entry points may opt
//! out; its MCP servers install this filter themselves with thread synchronization.

use std::io;

/// Shared container detection; an explicit false value cannot hide /.dockerenv.
pub fn is_containerized() -> bool {
    container_detected(std::env::var("DUDUCLAW_IN_CONTAINER").ok().as_deref(),
        std::path::Path::new("/.dockerenv").exists())
}

fn container_detected(marker: Option<&str>, dockerenv: bool) -> bool {
    dockerenv || matches!(marker, Some("1" | "true" | "yes"))
}

/// Native hosts and non-Linux platforms keep their existing behavior.
pub fn is_active() -> bool {
    cfg!(target_os = "linux") && is_containerized()
}

/// Attach a fail-closed filter before exec, without changing the environment.
pub fn guard_command(cmd: &mut tokio::process::Command) {
    guard_command_std(cmd.as_std_mut());
}

/// Synchronous counterpart of [`guard_command`]. Detection runs before fork.
pub fn guard_command_std(cmd: &mut std::process::Command) {
    #[cfg(target_os = "linux")]
    if is_active() {
        attach(cmd);
    }
    #[cfg(not(target_os = "linux"))]
    let _ = cmd;
}

#[cfg(target_os = "linux")]
fn attach(cmd: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    // SAFETY: the child only invokes prctl/seccomp and constructs errno errors.
    // The BPF program is static; no allocation, locks, env or filesystem reads.
    unsafe {
        cmd.pre_exec(|| linux::install(false));
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        // Rust's fork error pipe carries only errno. Log the detailed reason in
        // the parent, then fail the spawn with EOPNOTSUPP without allocating.
        tracing::error!("{}", unsupported());
        unsafe {
            cmd.pre_exec(|| Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP)));
        }
    }
}

#[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
fn unsupported() -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported,
        "container namespace guard supports only Linux x86_64 and aarch64; refusing unfiltered execution")
}

/// Install on every existing thread (and all future descendants). MCP entry
/// points call this before dispatch, even when their runtime already exists.
pub fn install_current_process() -> io::Result<()> {
    if !is_active() {
        return Ok(());
    }
    install_current_process_forced()
}

/// Install with TSYNC regardless of container detection or scrubbed environment.
/// Unsupported platforms refuse execution rather than running unfiltered.
pub fn install_current_process_forced() -> io::Result<()> {
    #[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
    return linux::install(true);
    #[cfg(all(target_os = "linux", not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
    return Err(unsupported());
    #[cfg(not(target_os = "linux"))]
    Err(io::Error::new(io::ErrorKind::Unsupported, "forced namespace guard requires Linux"))
}

#[cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64")))]
mod linux {
    use std::io;

    const LD: u16 = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
    const JEQ: u16 = (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16;
    const JGE: u16 = (libc::BPF_JMP | libc::BPF_JGE | libc::BPF_K) as u16;
    const JSET: u16 = (libc::BPF_JMP | libc::BPF_JSET | libc::BPF_K) as u16;
    const RET: u16 = (libc::BPF_RET | libc::BPF_K) as u16;
    const DENY: u32 = libc::SECCOMP_RET_ERRNO | libc::EPERM as u32;
    const NOSYS: u32 = libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32;
    const ALLOW: u32 = libc::SECCOMP_RET_ALLOW;
    const NAMESPACE_FLAGS: u32 = 0x7e02_0000;
    // AUDIT_ARCH_* values from linux/audit.h (not exported by libc).
    const X86_64: u32 = 0xc000_003e;
    #[cfg(any(target_arch = "aarch64", test))]
    const AARCH64: u32 = 0xc000_00b7;
    #[cfg(target_arch = "x86_64")]
    const ARCH: u32 = X86_64;
    #[cfg(target_arch = "aarch64")]
    const ARCH: u32 = AARCH64;

    const fn op(code: u16, jt: u8, jf: u8, k: u32) -> libc::sock_filter {
        libc::sock_filter { code, jt, jf, k }
    }

    // Offsets are fixed by the seccomp_data UAPI: nr=0, arch=4, args[0]=16.
    // Both supported ABIs are little endian, so offset 16 is its low word.
    const fn program(arch: u32, sys: [u32; 7]) -> [libc::sock_filter; 23] {
        [
            op(LD, 0, 0, 4),
            op(JEQ, 1, 0, arch),
            op(RET, 0, 0, DENY),
            op(LD, 0, 0, 0),
            // AArch64 has no x32 ABI: jump over this denial unconditionally.
            op(JGE, if arch == X86_64 { 0 } else { 1 },
                if arch == X86_64 { 1 } else { 0 },
                if arch == X86_64 { 0x4000_0000 } else { 0 }),
            op(RET, 0, 0, DENY),
            op(JEQ, 0, 1, sys[0]), op(RET, 0, 0, DENY),
            op(JEQ, 0, 1, sys[1]), op(RET, 0, 0, DENY),
            op(JEQ, 0, 1, sys[2]), op(RET, 0, 0, DENY),
            op(JEQ, 0, 1, sys[3]), op(RET, 0, 0, DENY),
            op(JEQ, 0, 1, sys[4]), op(RET, 0, 0, DENY),
            op(JEQ, 0, 1, sys[5]), op(RET, 0, 0, NOSYS),
            op(JEQ, 0, 3, sys[6]),
            op(LD, 0, 0, 16),
            op(JSET, 0, 1, NAMESPACE_FLAGS),
            op(RET, 0, 0, DENY),
            op(RET, 0, 0, ALLOW),
        ]
    }

    static FILTER: [libc::sock_filter; 23] = program(ARCH, [
        libc::SYS_unshare as u32, libc::SYS_mount as u32, libc::SYS_umount2 as u32,
        libc::SYS_pivot_root as u32, libc::SYS_setns as u32,
        libc::SYS_clone3 as u32, libc::SYS_clone as u32,
    ]);

    pub(super) fn install(sync_threads: bool) -> io::Result<()> {
        let filter = libc::sock_fprog {
            len: FILTER.len() as u16,
            filter: FILTER.as_ptr() as *mut libc::sock_filter,
        };
        // SAFETY: only synchronous kernel calls over a valid static BPF array.
        // seccomp copies the array before returning; it does not mutate it.
        unsafe {
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            let flags = if sync_threads { libc::SECCOMP_FILTER_FLAG_TSYNC } else { 0 };
            let result = libc::syscall(libc::SYS_seccomp, libc::SECCOMP_SET_MODE_FILTER,
                flags, &filter as *const libc::sock_fprog);
            if result < 0 {
                return Err(io::Error::last_os_error());
            }
            // TSYNC may return a positive offending TID, with errno unset.
            if result != 0 {
                return Err(io::Error::from_raw_os_error(libc::EBUSY));
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn evaluate(code: &[libc::sock_filter], arch: u32, nr: u32, flags: u64) -> u32 {
            let mut pc = 0;
            let mut acc = 0;
            for _ in 0..code.len() {
                let i = &code[pc];
                pc += 1;
                match i.code {
                    LD => acc = match i.k { 0 => nr, 4 => arch, 16 => flags as u32, _ => panic!("invalid offset") },
                    RET => return i.k,
                    JEQ | JGE | JSET => {
                        let yes = match i.code { JEQ => acc == i.k, JGE => acc >= i.k, _ => acc & i.k != 0 };
                        pc += if yes { i.jt } else { i.jf } as usize;
                    }
                    _ => panic!("invalid BPF instruction"),
                }
            }
            panic!("BPF fell through")
        }

        #[test]
        fn bpf_layout_for_both_architectures() {
            // Linux UAPI syscall numbers; compare the native variant to libc.
            for (arch, sys) in [(X86_64, [272, 165, 166, 155, 308, 435, 56]),
                                (AARCH64, [97, 40, 39, 41, 268, 435, 220])] {
                let p = program(arch, sys);
                assert_eq!(evaluate(&p, 0, 0, 0), DENY);
                assert_eq!(evaluate(&p, if arch == X86_64 { AARCH64 } else { X86_64 }, 0, 0), DENY);
                for nr in &sys[..5] { assert_eq!(evaluate(&p, arch, *nr, 0), DENY); }
                assert_eq!(evaluate(&p, arch, sys[5], 0), NOSYS);
                assert_eq!(evaluate(&p, arch, sys[6], 0), ALLOW);
                for bit in 0..32 {
                    let flag = 1u32 << bit;
                    assert_eq!(evaluate(&p, arch, sys[6], flag as u64),
                        if flag & NAMESPACE_FLAGS == 0 { ALLOW } else { DENY });
                }
                assert_eq!(evaluate(&p, arch, sys[6], (NAMESPACE_FLAGS as u64) << 32), ALLOW);
                for nr in 0..1024 {
                    if !sys.contains(&nr) { assert_eq!(evaluate(&p, arch, nr, 0), ALLOW); }
                }
                if arch == AARCH64 { assert_eq!(evaluate(&p, arch, u32::MAX, 0), ALLOW); }
                if arch == X86_64 {
                    for nr in [0x4000_0000, 0x4000_0038, u32::MAX] {
                        assert_eq!(evaluate(&p, arch, nr, 0), DENY);
                    }
                }
                if arch == ARCH {
                    for (expected, actual) in p.iter().zip(FILTER.iter()) {
                        assert_eq!((expected.code, expected.jt, expected.jf, expected.k),
                            (actual.code, actual.jt, actual.jf, actual.k));
                    }
                }
            }
        }

        #[test]
        fn guarded_children_and_threads() {
            let mut command = std::process::Command::new("/bin/true");
            super::super::attach(&mut command); // Force activation on native Linux.
            assert!(command.status().unwrap().success());
            let mut unshare = std::process::Command::new("unshare");
            unshare.args(["-U", "true"]);
            super::super::attach(&mut unshare);
            match unshare.output() {
                Ok(output) => assert!(!output.status.success()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => eprintln!("SKIP: unshare binary missing"),
                Err(error) => panic!("guarded unshare spawn failed: {error}"),
            }
            let mut child = std::process::Command::new(std::env::current_exe().unwrap());
            child.args(["--exact", "ns_guard::linux::tests::thread_clone_child", "--nocapture"])
                .env("DUDUCLAW_NS_GUARD_TEST_CHILD", "1");
            super::super::attach(&mut child);
            assert!(child.status().unwrap().success());
        }

        #[test]
        fn tsync_covers_existing_threads() {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "ns_guard::linux::tests::tsync_child", "--nocapture"])
                .env("DUDUCLAW_NS_GUARD_TSYNC_CHILD", "1")
                .status().unwrap();
            assert!(status.success());
        }

        #[test]
        fn tsync_child() {
            if std::env::var("DUDUCLAW_NS_GUARD_TSYNC_CHILD").as_deref() != Ok("1") { return; }
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let worker_barrier = barrier.clone();
            let worker = std::thread::spawn(move || {
                worker_barrier.wait();
                worker_barrier.wait();
                assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWUSER) }, -1);
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
                assert_eq!(unsafe { libc::syscall(libc::SYS_clone3, std::ptr::null::<u8>(), 0) }, -1);
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ENOSYS));
            });
            barrier.wait();
            super::super::install_current_process_forced().unwrap();
            barrier.wait();
            worker.join().unwrap();
        }

        #[test]
        fn thread_clone_child() {
            if std::env::var("DUDUCLAW_NS_GUARD_TEST_CHILD").as_deref() != Ok("1") { return; }
            std::thread::spawn(|| {
                assert_eq!(unsafe { libc::unshare(libc::CLONE_NEWUSER) }, -1);
                assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
            }).join().unwrap();
            extern "C" fn child(_: *mut libc::c_void) -> libc::c_int { 0 }
            let mut stack = vec![0u128; 4096];
            let top = unsafe { stack.as_mut_ptr().add(stack.len()) } as *mut libc::c_void;
            let pid = unsafe { libc::clone(child, top, libc::SIGCHLD, std::ptr::null_mut()) };
            assert!(pid > 0, "clone without namespaces: {}", io::Error::last_os_error());
            let mut status = 0;
            assert_eq!(unsafe { libc::waitpid(pid, &mut status, 0) }, pid);
            assert_eq!(status, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn marker_only_containers_and_native_detection() {
        for marker in ["1", "true", "yes"] {
            assert!(super::container_detected(Some(marker), false));
        }
        for marker in [None, Some("0"), Some("false"), Some("")] {
            assert!(!super::container_detected(marker, false));
            assert!(super::container_detected(marker, true));
        }
        assert!(crate::spawn_env::AGENT_CLI_ENV_ALLOWLIST.contains(&"DUDUCLAW_IN_CONTAINER"));
    }

    #[test]
    fn namespace_exemptions_are_only_codex_entry_points() {
        fn visit(path: &std::path::Path, matches: &mut Vec<std::path::PathBuf>) {
            for entry in std::fs::read_dir(path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() { visit(&path, matches); }
                else if path.extension().is_some_and(|ext| ext == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    let needle = ["apply_agent_cli_env_allowlist", "_keep_namespaces"].concat();
                    if text.contains(&needle) { matches.push(path); }
                }
            }
        }
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut found = Vec::new();
        visit(&root, &mut found);
        found.sort();
        let mut expected: Vec<_> = ["duduclaw-core/src/spawn_env.rs",
            "duduclaw-gateway/src/runtime/codex_sandbox.rs"].iter().map(|p| root.join(p)).collect();
        expected.sort();
        assert_eq!(found, expected);
    }
}
