//! Pure importer sandbox policy and systemd readback validation.
use std::collections::BTreeMap;
use crate::Error;

pub(crate) const CALLS: &str = "read write readv writev pread64 pwrite64 ftruncate getcwd readlink readlinkat access faccessat faccessat2 close close_range fstat newfstatat statx lseek mmap mprotect munmap mremap brk madvise rt_sigaction rt_sigprocmask rt_sigreturn sigaltstack ioctl fcntl flock fsync fdatasync openat unlinkat unlink mkdir mkdirat rmdir getdents64 fchmod getrandom clock_gettime futex getpid gettid getuid geteuid getgid getegid prctl prlimit64 sched_getaffinity set_robust_list rseq arch_prctl set_tid_address execve exit exit_group socket connect getsockopt setsockopt recvmsg sendmsg recvfrom sendto shutdown poll ppoll";

// Readback only: systemd inserts @default into a transient allowlist and
// expands it when showing SystemCallFilter. This is the reviewed union for
// v249/v252/v255; it does not add permissions to the requested unit profile.
// https://github.com/systemd/systemd/blob/v255/src/shared/seccomp-util.c
// https://github.com/systemd/systemd/blob/v249/src/core/dbus-execute.c
const IMPLICIT_DEFAULT_CALLS: &str = "arch_prctl brk cacheflush clock_getres clock_getres_time64 clock_gettime clock_gettime64 clock_nanosleep clock_nanosleep_time64 execve exit exit_group futex futex_time64 futex_waitv get_robust_list get_thread_area getegid getegid32 geteuid geteuid32 getgid getgid32 getgroups getgroups32 getpgid getpgrp getpid getppid getrandom getresgid getresgid32 getresuid getresuid32 getrlimit getsid gettid gettimeofday getuid getuid32 membarrier mmap mmap2 mprotect munmap nanosleep pause prlimit64 restart_syscall riscv_flush_icache riscv_hwprobe rseq rt_sigreturn sched_getaffinity sched_yield set_robust_list set_thread_area set_tid_address set_tls sigreturn time ugetrlimit";

pub(crate) fn sandbox(values:&BTreeMap<String,String>,protected:&[String])->Result<(),Error> {
    for (key,value) in [("DynamicUser","yes"),("PrivateNetwork","yes"),("PrivateTmp","yes"),
        ("ProtectSystem","strict"),("ProtectHome","yes"),("NoNewPrivileges","yes"),
        ("CapabilityBoundingSet",""),("AmbientCapabilities",""),("MemoryMax","1073741824"),
        ("TasksMax","1"),("RuntimeMaxUSec","1min"),("LimitNOFILE","32"),("LimitFSIZE","268435456"),("LimitCORE","0"),("RestrictAddressFamilies","AF_UNIX")] {
        if values.get(key).map(String::as_str)!=Some(value) {return Err(Error::Readback);}
    }
    let filter=values.get("SystemCallFilter").ok_or(Error::Readback)?;
    // systemd may omit names absent on the native architecture (e.g. mkdir on
    // aarch64). Only the explicit list and systemd's bounded implicit defaults
    // are accepted; arbitrary additions or unexpanded groups are refused.
    if filter.split_whitespace().next().is_none()
        || filter.split_whitespace().any(|call| !CALLS.split_whitespace()
            .chain(IMPLICIT_DEFAULT_CALLS.split_whitespace()).any(|allowed|allowed==call)) {
        return Err(Error::Readback);
    }
    let paths=values.get("InaccessiblePaths").ok_or(Error::Readback)?;
    for path in protected {
        // systemd versions differ in whether show retains the optional '-' flag.
        let expected=path.strip_prefix('-').unwrap_or(path);
        if !paths.split_whitespace().any(|entry|entry.strip_prefix('-').unwrap_or(entry)==expected) {return Err(Error::Readback);}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn properties() -> BTreeMap<String, String> {
        [("DynamicUser","yes"),("PrivateNetwork","yes"),("PrivateTmp","yes"),
            ("ProtectSystem","strict"),("ProtectHome","yes"),("NoNewPrivileges","yes"),
            ("CapabilityBoundingSet",""),("AmbientCapabilities",""),("MemoryMax","1073741824"),
            ("TasksMax","1"),("RuntimeMaxUSec","1min"),("LimitNOFILE","32"),
            ("LimitFSIZE","268435456"),("LimitCORE","0"),("RestrictAddressFamilies","AF_UNIX"),
            ("SystemCallFilter",CALLS),("InaccessiblePaths","-/var/lib/duduclaw /var/lib/duduclaw-migrate")]
            .into_iter().map(|(k,v)|(k.into(),v.into())).collect()
    }
    #[test]
    fn importer_profile_accepts_systemd_expanded_defaults_without_widening_unit() {
        let mut actual = properties();
        actual.insert("SystemCallFilter".into(), format!("{CALLS} {IMPLICIT_DEFAULT_CALLS}"));
        sandbox(&actual, &[]).unwrap();
        for added in ["mount", "clone3", "@default", "@system-service", "unknown_syscall"] {
            actual.insert("SystemCallFilter".into(), format!("{CALLS} {IMPLICIT_DEFAULT_CALLS} {added}"));
            assert!(sandbox(&actual, &[]).is_err(), "unexpected {added}");
        }
    }
    #[test]
    fn importer_profile_readback_refuses_missing_or_widened_sandbox() {
        let paths = vec!["/var/lib/duduclaw".into(), "-/var/lib/duduclaw-migrate".into()];
        let expected = properties(); sandbox(&expected, &paths).unwrap();
        for key in expected.keys() {
            let mut actual = expected.clone(); actual.remove(key);
            assert!(sandbox(&actual, &paths).is_err(), "missing {key}");
        }
        for (key,value) in [("DynamicUser","no"),("PrivateNetwork","no"),
            ("CapabilityBoundingSet","CAP_SYS_ADMIN"),("MemoryMax","infinity"),
            ("TasksMax","2"),("NoNewPrivileges","no"),("SystemCallFilter","~mount"),
            ("SystemCallFilter","read write mount"),("SystemCallFilter","@system-service"),
            ("InaccessiblePaths","/var/lib/duduclaw-lookalike /var/lib/duduclaw-migrate")] {
            let mut actual = expected.clone(); actual.insert(key.into(),value.into());
            assert!(sandbox(&actual, &paths).is_err(), "widened {key}");
        }
    }
    #[test]
    fn importer_profile_supports_arm64_tempdirs_and_matches_reference_unit() {
        let calls: std::collections::BTreeSet<_> = CALLS.split_whitespace().collect();
        // aarch64 uses mkdirat; SQLite's private temp copy must work on both ISAs.
        for required in ["mkdir", "mkdirat", "openat", "unlinkat", "ftruncate", "fcntl", "fsync"] {
            assert!(calls.contains(required), "missing {required}");
        }
        for forbidden in ["clone", "clone3", "fork", "vfork", "mount", "ptrace", "bpf", "io_uring_setup"] {
            assert!(!calls.contains(forbidden), "unexpected {forbidden}");
        }
        let template = include_str!("../../duduclaw-import/systemd/duduclaw-import@.service");
        let reference = template.lines().find_map(|line|line.strip_prefix("SystemCallFilter=")).unwrap();
        assert_eq!(calls, reference.split_whitespace().collect());
    }
}
