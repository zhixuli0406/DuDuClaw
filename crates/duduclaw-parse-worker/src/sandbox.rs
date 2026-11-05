//! A second, process-local boundary after systemd has created the DynamicUser.
//! No pathname opens, process creation, connect or socket creation are allowed
//! after setup. Parsers use only validated, sealed inputs and memory buffers.
#![allow(unsafe_code)]
use duduclaw_isolation::worker::{ParseError, WorkerLimits};
use nix::sys::resource::{setrlimit, Resource};

#[cfg(target_arch = "x86_64")]
const ARCH: u32 = 0xc000003e;
#[cfg(target_arch = "aarch64")]
const ARCH: u32 = 0xc00000b7;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn allowed() -> Vec<i64> {
    let mut calls = vec![
        libc::SYS_read, libc::SYS_write, libc::SYS_close, libc::SYS_pread64,
        libc::SYS_readv, libc::SYS_writev, libc::SYS_ftruncate, libc::SYS_lseek, libc::SYS_fstat,
        libc::SYS_statx, libc::SYS_newfstatat,
        libc::SYS_mmap, libc::SYS_mprotect, libc::SYS_munmap, libc::SYS_mremap,
        libc::SYS_brk, libc::SYS_madvise, libc::SYS_futex,
        libc::SYS_clock_gettime, libc::SYS_clock_nanosleep, libc::SYS_nanosleep,
        libc::SYS_rt_sigaction, libc::SYS_rt_sigprocmask, libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack, libc::SYS_getrandom, libc::SYS_getpid,
        libc::SYS_gettid, libc::SYS_sched_yield, libc::SYS_restart_syscall,
        libc::SYS_sendmsg, libc::SYS_recvmsg, libc::SYS_ppoll,
        libc::SYS_exit, libc::SYS_exit_group,
    ];
    #[cfg(target_arch = "x86_64")]
    calls.push(libc::SYS_poll);
    calls.sort_unstable();
    calls.dedup();
    calls
}

fn stmt(code: u16, k: u32) -> libc::sock_filter { libc::sock_filter { code, jt: 0, jf: 0, k } }
fn jump(k: u32, yes: u8, no: u8) -> libc::sock_filter {
    libc::sock_filter { code: (libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K) as u16, jt: yes, jf: no, k }
}

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn filter() -> Vec<libc::sock_filter> {
    let load = (libc::BPF_LD | libc::BPF_W | libc::BPF_ABS) as u16;
    let ret = (libc::BPF_RET | libc::BPF_K) as u16;
    let mut program = vec![stmt(load, 4), jump(ARCH, 1, 0), stmt(ret, libc::SECCOMP_RET_KILL_PROCESS), stmt(load, 0)];
    for call in allowed() {
        program.push(jump(call as u32, 0, 1));
        program.push(stmt(ret, libc::SECCOMP_RET_ALLOW));
    }
    program.push(stmt(ret, libc::SECCOMP_RET_ERRNO | libc::EPERM as u32));
    program
}

pub fn apply(limits: &WorkerLimits, output_bytes: u64) -> Result<(), ParseError> {
    let uid = nix::unistd::geteuid().as_raw();
    if !(61184..=65519).contains(&uid) { return Err(ParseError::Unavailable); }
    nix::sys::prctl::set_dumpable(false).map_err(|_| ParseError::Unavailable)?;
    if nix::sys::prctl::get_dumpable().map_err(|_| ParseError::Unavailable)? {
        return Err(ParseError::Unavailable);
    }
    nix::sys::prctl::set_no_new_privs().map_err(|_| ParseError::Unavailable)?;
    for (resource, limit) in [
        (Resource::RLIMIT_CORE, 0),
        (Resource::RLIMIT_AS, limits.memory_bytes.get()),
        (Resource::RLIMIT_FSIZE, output_bytes),
        (Resource::RLIMIT_NOFILE, 64),
        (Resource::RLIMIT_CPU, u64::from(limits.wall_time_ms.get().div_ceil(1000))),
    ] {
        setrlimit(resource, limit, limit).map_err(|_| ParseError::Unavailable)?;
    }
    // Default SIGALRM terminates even a wedged parser. Client timeout is exact;
    // this watchdog rounds up to one second, systemd kills the cgroup at 35s.
    nix::unistd::alarm::set(limits.wall_time_ms.get().div_ceil(1000));
    install_filter()
}

fn install_filter() -> Result<(), ParseError> {
    #[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
    {
        let mut instructions = filter();
        let program = libc::sock_fprog { len: instructions.len() as u16, filter: instructions.as_mut_ptr() };
        // SAFETY: single-threaded worker, valid BPF slice alive for this syscall;
        // the kernel copies the filter. NNP is set before loading it.
        let status = unsafe { libc::syscall(libc::SYS_seccomp, libc::SECCOMP_SET_MODE_FILTER, 0, &program) };
        if status != 0 { return Err(ParseError::Unavailable); }
        Ok(())
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    { Err(ParseError::Unavailable) }
}

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
    use super::*;
    fn evaluate(arch: u32, call: u32) -> u32 {
        let program = filter();
        let mut pc = 0;
        let mut accumulator = 0;
        loop {
            let instruction = &program[pc];
            match instruction.code as u32 {
                x if x == libc::BPF_LD | libc::BPF_W | libc::BPF_ABS => {
                    accumulator = match instruction.k { 0 => call, 4 => arch, _ => panic!("offset") };
                }
                x if x == libc::BPF_JMP | libc::BPF_JEQ | libc::BPF_K => {
                    pc += if accumulator == instruction.k { instruction.jt } else { instruction.jf } as usize;
                }
                x if x == libc::BPF_RET | libc::BPF_K => return instruction.k,
                _ => panic!("unknown BPF"),
            }
            pc += 1;
            assert!(pc < program.len());
        }
    }
    #[test]
    fn seccomp_allowlist_exhaustive_syscall_table_and_architecture() {
        let allow = allowed();
        for call in 0..2048 {
            let expected = if allow.contains(&(call as i64)) { libc::SECCOMP_RET_ALLOW }
                else { libc::SECCOMP_RET_ERRNO | libc::EPERM as u32 };
            assert_eq!(evaluate(ARCH, call), expected, "syscall {call}");
            assert_eq!(evaluate(ARCH ^ 1, call), libc::SECCOMP_RET_KILL_PROCESS);
            assert_ne!(evaluate(ARCH, call | 0x40000000), libc::SECCOMP_RET_ALLOW);
        }
        for call in [libc::SYS_socket, libc::SYS_connect, libc::SYS_openat,
            libc::SYS_execve, libc::SYS_clone, libc::SYS_ptrace, libc::SYS_io_uring_setup] {
            assert_ne!(evaluate(ARCH, call as u32), libc::SECCOMP_RET_ALLOW);
        }
    }

    #[test]
    fn seccomp_child() {
        if std::env::var_os("DUDUCLAW_PARSE_TEST_SECCOMP_CHILD").is_none() { return; }
        use duduclaw_isolation::worker::{ContentFormat, ParseOperation, ParseResult};
        let input = crate::transport::snapshot(&mut &b"hello"[..]).unwrap();
        let request = crate::client::request(ParseOperation::ExtractDocument, ContentFormat::Text, 5);
        let mut inputs = [input];
        crate::transport::validate_inputs(&request, &mut inputs).unwrap();
        nix::sys::prctl::set_no_new_privs().unwrap();
        install_filter().unwrap();
        assert_eq!(inputs[0].metadata().unwrap().len(), 5);
        assert!(matches!(crate::parsers::parse(&request, &mut inputs).result, ParseResult::Complete { .. }));
        assert!(std::fs::File::open("/etc/passwd").is_err());
        for address in [nix::sys::socket::AddressFamily::Inet, nix::sys::socket::AddressFamily::Inet6,
            nix::sys::socket::AddressFamily::Unix] {
            assert!(nix::sys::socket::socket(address, nix::sys::socket::SockType::Stream,
                nix::sys::socket::SockFlag::empty(), None).is_err());
        }
        std::process::exit(0);
    }

    #[test]
    fn seccomp_real_child_can_parse_but_cannot_open_or_network() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "sandbox::tests::seccomp_child", "--nocapture"])
            .env("DUDUCLAW_PARSE_TEST_SECCOMP_CHILD", "1").output().unwrap();
        assert!(output.status.success(), "child failed: {}", String::from_utf8_lossy(&output.stderr));
    }
    #[test]
    fn resource_probe_child() {
        let Some(mode) = std::env::var_os("DUDUCLAW_PARSE_ROOT_PROBE") else { return; };
        use std::os::fd::AsRawFd;
        let socket = nix::sys::socket::socket(nix::sys::socket::AddressFamily::Inet,
            nix::sys::socket::SockType::Stream, nix::sys::socket::SockFlag::empty(), None).unwrap();
        let mut limits = crate::client::request(duduclaw_isolation::worker::ParseOperation::ExtractDocument,
            duduclaw_isolation::worker::ContentFormat::Text, 0).limits;
        limits.memory_bytes = (64 * 1024 * 1024u64).try_into().unwrap();
        limits.wall_time_ms = 1000u32.try_into().unwrap();
        nix::unistd::setgroups(&[]).unwrap();
        nix::unistd::setgid(nix::unistd::Gid::from_raw(61184)).unwrap();
        nix::unistd::setuid(nix::unistd::Uid::from_raw(61184)).unwrap();
        apply(&limits, 0).unwrap();
        if mode == "timeout" {
            // SIGALRM must end a parser that never returns to the transport.
            std::thread::sleep(std::time::Duration::from_secs(3));
            std::process::exit(77);
        }
        // SAFETY: deliberate failing allocation, no dereference on MAP_FAILED.
        let memory = unsafe { libc::mmap(std::ptr::null_mut(), 256 * 1024 * 1024,
            libc::PROT_READ | libc::PROT_WRITE, libc::MAP_PRIVATE | libc::MAP_ANONYMOUS, -1, 0) };
        assert_eq!(memory, libc::MAP_FAILED);
        for address in [[127, 0, 0, 1], [192, 168, 0, 1], [169, 254, 169, 254]] {
            let target = libc::sockaddr_in { sin_family: libc::AF_INET as u16,
                sin_port: 80u16.to_be(), sin_addr: libc::in_addr { s_addr: u32::from_ne_bytes(address) }, sin_zero: [0; 8] };
            // SAFETY: valid pre-created socket and initialized IPv4 address.
            let result = unsafe { libc::connect(socket.as_raw_fd(),
                (&target as *const libc::sockaddr_in).cast(), std::mem::size_of_val(&target) as u32) };
            assert_eq!(result, -1);
            assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(libc::EPERM));
        }
        std::process::exit(0);
    }

    #[test]
    #[ignore = "requires root; uses disposable child uid to verify production limits and syscall policy"]
    fn iso_root_memory_limit_ssrf_and_watchdog() {
        use std::os::unix::process::ExitStatusExt;
        assert!(nix::unistd::geteuid().is_root());
        for mode in ["resources", "timeout"] {
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "sandbox::tests::resource_probe_child", "--nocapture"])
                .env("DUDUCLAW_PARSE_ROOT_PROBE", mode).status().unwrap();
            if mode == "timeout" { assert_eq!(status.signal(), Some(libc::SIGALRM)); }
            else { assert!(status.success()); }
        }
    }

}
