//! Fixed native-architecture daemon syscall allowlist, installed after bootstrap.
//!
//! R8j round 3 F4: two in-process filters. The unit's `SystemCallFilter=`
//! allows `clone`/`clone3` (and `execve`, which systemd needs to start the
//! binary) ONLY so that the [`install_bootstrap`] filter — the first step of
//! `daemon::host_main` — can narrow them to thread creation: `clone3` ⇒
//! ENOSYS (glibc falls back to `clone`), `clone` without CLONE_THREAD|CLONE_VM
//! ⇒ EPERM, `fork`/`vfork` (x86_64) and `execve`/`execveat` ⇒ EPERM. No
//! independent process can be created during start-up. The one DNS probe
//! worker thread is created under it, then [`install`] stacks the final
//! default-deny filter (TSYNC); stacked filters take the strictest result, so
//! nothing the final filter denies is re-allowed.
#![allow(unsafe_code)]
use duduclaw_isolation::egress::EgressError;

#[derive(Clone, Copy, Debug)]
struct Instruction { code: u16, jt: u8, jf: u8, k: u32 }
const LOAD: u16 = 0x20;
const EQUAL: u16 = 0x15;
/// BPF_JMP | BPF_JGE | BPF_K (the x86_64 x32 check only).
#[cfg_attr(not(target_arch = "x86_64"), allow(dead_code))]
const GREATER_EQUAL: u16 = 0x35;
const RETURN: u16 = 0x06;
const AND: u16 = 0x54;
const WRITE_OPEN_FLAGS: u32 = (libc::O_ACCMODE | libc::O_CREAT | libc::O_TRUNC | libc::O_APPEND | (libc::O_TMPFILE & !libc::O_DIRECTORY)) as u32;
const ALLOW: u32 = 0x7fff0000;
const KILL: u32 = 0x80000000;
const DENY: u32 = 0x00050000 | 1; // EPERM
/// SECCOMP_RET_ERRNO | ENOSYS.
const NO_SYSCALL: u32 = 0x00050000 | libc::ENOSYS as u32;
/// seccomp_data.args[0] (a u64 at offset 16): the 32-bit word holding its
/// low half, which is where CLONE_THREAD and CLONE_VM live.
#[cfg(target_endian = "little")]
const ARG0_LOW: u32 = 16;
#[cfg(target_endian = "big")]
const ARG0_LOW: u32 = 20;
/// `clone` creates a thread only with both: CLONE_THREAD (0x00010000) and
/// CLONE_VM (0x00000100), both in the low word of the flags argument.
const THREAD_FLAGS: u32 = (libc::CLONE_THREAD | libc::CLONE_VM) as u32;
/// x86_64: the x32 ABI marks its syscall numbers with this bit under the same
/// audit arch; such numbers would bypass the per-number rules below.
#[cfg(target_arch = "x86_64")]
const X32_SYSCALL_BIT: u32 = 0x40000000;

#[cfg(target_arch = "x86_64")]
const ARCH: u32 = 0xc000003e;
#[cfg(target_arch = "aarch64")]
const ARCH: u32 = 0xc00000b7;

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn allowed_syscalls() -> Vec<i64> {
    let mut calls = vec![
        libc::SYS_read, libc::SYS_write, libc::SYS_readv, libc::SYS_writev,
        libc::SYS_close, libc::SYS_fstat, libc::SYS_newfstatat, libc::SYS_statx,
        libc::SYS_readlinkat, libc::SYS_lseek, libc::SYS_pread64,
        libc::SYS_getdents64, libc::SYS_fcntl, libc::SYS_ioctl,
        libc::SYS_brk, libc::SYS_mmap, libc::SYS_munmap, libc::SYS_mprotect,
        libc::SYS_mremap, libc::SYS_madvise, libc::SYS_futex,
        libc::SYS_rt_sigaction, libc::SYS_rt_sigprocmask, libc::SYS_rt_sigreturn,
        libc::SYS_sigaltstack, libc::SYS_restart_syscall,
        libc::SYS_getpid, libc::SYS_gettid, libc::SYS_getuid, libc::SYS_geteuid,
        libc::SYS_getgid, libc::SYS_getegid, libc::SYS_getgroups,
        libc::SYS_getresuid, libc::SYS_getresgid, libc::SYS_getrandom,
        libc::SYS_prlimit64, libc::SYS_clock_gettime, libc::SYS_clock_nanosleep,
        libc::SYS_nanosleep, libc::SYS_sched_yield, libc::SYS_uname,
        libc::SYS_epoll_create1, libc::SYS_epoll_ctl, libc::SYS_epoll_pwait,
        libc::SYS_eventfd2, libc::SYS_ppoll, libc::SYS_pselect6,
        libc::SYS_connect, libc::SYS_accept4, libc::SYS_bind, libc::SYS_listen,
        libc::SYS_getsockopt, libc::SYS_setsockopt, libc::SYS_getsockname,
        libc::SYS_getpeername, libc::SYS_sendto, libc::SYS_sendmsg,
        libc::SYS_recvfrom, libc::SYS_recvmsg, libc::SYS_recvmmsg,
        libc::SYS_shutdown, libc::SYS_exit, libc::SYS_exit_group,
    ];
    #[cfg(target_arch = "x86_64")]
    calls.extend([libc::SYS_access, libc::SYS_readlink, libc::SYS_poll, libc::SYS_epoll_wait]);
    calls.sort_unstable();
    calls.dedup();
    calls
}
fn instruction(code: u16, jt: u8, jf: u8, k: u32) -> Instruction { Instruction { code, jt, jf, k } }

#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn program() -> Vec<Instruction> {
    // seccomp_data: nr@0, arch@4, instruction_pointer@8, args[0]@16.
    let mut p = vec![instruction(LOAD,0,0,4), instruction(EQUAL,1,0,ARCH),
        instruction(RETURN,0,0,KILL), instruction(LOAD,0,0,0),
        instruction(EQUAL,0,8,libc::SYS_socket as u32), instruction(LOAD,0,0,16),
        instruction(EQUAL,0,1,libc::AF_UNIX as u32), instruction(RETURN,0,0,ALLOW),
        instruction(EQUAL,0,1,libc::AF_INET as u32), instruction(RETURN,0,0,ALLOW),
        instruction(EQUAL,0,1,libc::AF_INET6 as u32), instruction(RETURN,0,0,ALLOW),
        instruction(RETURN,0,0,DENY),
        instruction(EQUAL,0,5,libc::SYS_openat as u32), instruction(LOAD,0,0,32),
        instruction(AND,0,0,WRITE_OPEN_FLAGS), instruction(EQUAL,1,0,0),
        instruction(RETURN,0,0,DENY), instruction(RETURN,0,0,ALLOW)];
    for call in allowed_syscalls() {
        p.push(instruction(EQUAL,0,1,call as u32));
        p.push(instruction(RETURN,0,0,ALLOW));
    }
    p.push(instruction(RETURN,0,0,DENY));
    p
}

/// The calls the bootstrap filter refuses outright (EPERM): every exec, and
/// on x86_64 the legacy process-creating calls (aarch64 has neither `fork`
/// nor `vfork`). The daemon never execs (`daemon::host_main` and what it
/// calls spawn no program).
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn bootstrap_denied() -> Vec<i64> {
    #[allow(unused_mut)]
    let mut calls = vec![libc::SYS_execve, libc::SYS_execveat];
    #[cfg(target_arch = "x86_64")]
    calls.extend([libc::SYS_fork, libc::SYS_vfork]);
    calls
}

/// R8j round 3 F4: default ALLOW, narrowing process creation to threads.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn bootstrap_program() -> Vec<Instruction> {
    // seccomp_data: nr@0, arch@4, instruction_pointer@8, args[0]@16.
    let mut p = vec![instruction(LOAD,0,0,4), instruction(EQUAL,1,0,ARCH),
        instruction(RETURN,0,0,KILL), instruction(LOAD,0,0,0)];
    #[cfg(target_arch = "x86_64")]
    p.extend([instruction(GREATER_EQUAL,0,1,X32_SYSCALL_BIT), instruction(RETURN,0,0,DENY)]);
    p.extend([instruction(EQUAL,0,1,libc::SYS_clone3 as u32), instruction(RETURN,0,0,NO_SYSCALL),
        // Not clone: skip the five flag instructions to the denied calls.
        instruction(EQUAL,0,5,libc::SYS_clone as u32), instruction(LOAD,0,0,ARG0_LOW),
        instruction(AND,0,0,THREAD_FLAGS), instruction(EQUAL,0,1,THREAD_FLAGS),
        instruction(RETURN,0,0,ALLOW), instruction(RETURN,0,0,DENY)]);
    for call in bootstrap_denied() {
        p.push(instruction(EQUAL,0,1,call as u32));
        p.push(instruction(RETURN,0,0,DENY));
    }
    p.push(instruction(RETURN,0,0,ALLOW));
    p
}

/// Loads `program` with SECCOMP_SET_MODE_FILTER and TSYNC (every existing
/// thread gets it too). Any nonzero result (including a TSYNC thread id) is
/// a failure.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
fn load(program: &[Instruction]) -> Result<(), EgressError> {
    let filters: Vec<libc::sock_filter> = program.iter().map(|p| libc::sock_filter { code:p.code,jt:p.jt,jf:p.jf,k:p.k }).collect();
    let program = libc::sock_fprog { len: filters.len() as u16, filter: filters.as_ptr() as *mut libc::sock_filter };
    // The live filter buffer remains readable until seccomp copies it.
    let rc = unsafe { libc::syscall(libc::SYS_seccomp, 1u32, 1u32, &program) };
    if rc == 0 { Ok(()) } else { Err(EgressError::Unavailable) }
}

/// R8j round 3 F4: the first step of `daemon::host_main`, before identity,
/// configuration, sockets and the DNS probe worker. Sets and reads back
/// NoNewPrivileges itself (also set by the unit and again by
/// `daemon::harden`), which an unprivileged filter requires, then installs
/// [`bootstrap_program`]: from here on only thread-creating `clone` works.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn install_bootstrap() -> Result<(), EgressError> {
    // prctl takes scalar arguments only.
    unsafe {
        if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
            || libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1 { return Err(EgressError::Unavailable); }
    }
    load(&bootstrap_program())
}
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn install_bootstrap() -> Result<(), EgressError> { Err(EgressError::Unavailable) }

/// Called after identity, configuration, listener and DNS startup probes. The
/// daemon uses a current-thread runtime; TSYNC also covers any existing threads.
/// No exec, clone, clone3, namespace or AF_VSOCK syscall is allowed (the
/// default action below denies everything not in `allowed_syscalls`, which
/// lists neither clone nor clone3); the unit admits clone/clone3 only so the
/// bootstrap filter ([`install_bootstrap`]) can narrow them to thread
/// creation for the DNS probe worker created before this runs; this filter
/// stacks on it. Filesystem
/// writes are denied by the unit and openat rejects writable/creating flags.
/// Read-only openat remains for NSS/config reads.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn install() -> Result<(), EgressError> {
    // NNP was set/read back by install_bootstrap and daemon::harden.
    load(&program())
}
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
pub fn install() -> Result<(), EgressError> { Err(EgressError::Unavailable) }

#[cfg(all(test, any(target_arch = "x86_64", target_arch = "aarch64")))]
mod tests {
    use super::*;
    fn evaluate(p: &[Instruction], arch: u32, syscall: u32, family: u32) -> u32 {
        evaluate_flags(p, arch, syscall, family, 0)
    }
    fn evaluate_flags(p: &[Instruction], arch: u32, syscall: u32, family: u32, flags: u32) -> u32 {
        let mut pc=0usize; let mut a=0;
        for _ in 0..p.len()+1 {
            let i=p.get(pc).expect("BPF jump out of range");
            match i.code {
                LOAD => { a=match i.k {0=>syscall,4=>arch,16=>family,32=>flags,_=>panic!("unexpected field")}; pc+=1; }
                EQUAL => { pc+=1+usize::from(if a==i.k {i.jt}else{i.jf}); }
                AND => { a &= i.k; pc+=1; }
                RETURN => return i.k,
                _ => panic!("unexpected opcode"),
            }
        }
        panic!("nonterminating BPF")
    }
    #[test]
    fn seccomp_exhaustive_native_syscalls_and_socket_families() {
        let p=program(); let allowed=allowed_syscalls();
        for syscall in 0..2048u32 {
            let result=evaluate(&p,ARCH,syscall,libc::AF_INET as u32);
            let expected=if allowed.contains(&(syscall as i64)) || syscall==libc::SYS_socket as u32 || syscall==libc::SYS_openat as u32 {ALLOW}else{DENY};
            assert_eq!(result,expected,"syscall {syscall}");
            assert_eq!(evaluate(&p,0,syscall,0),KILL);
            assert_eq!(evaluate(&p,ARCH,syscall|0x40000000,0),DENY);
        }
        for family in 0..256u32 {
            assert_eq!(evaluate(&p,ARCH,libc::SYS_socket as u32,family),
                if [libc::AF_UNIX as u32,libc::AF_INET as u32,libc::AF_INET6 as u32].contains(&family) {ALLOW}else{DENY});
        }
        for flags in (0..4096u32).chain((0..32).map(|bit| 1u32 << bit)) {
            assert_eq!(evaluate_flags(&p,ARCH,libc::SYS_openat as u32,0,flags),
                if flags & WRITE_OPEN_FLAGS == 0 {ALLOW}else{DENY}, "open flags {flags}");
        }
        for syscall in [libc::SYS_execve,libc::SYS_clone,libc::SYS_clone3,libc::SYS_mount,libc::SYS_unshare,libc::SYS_socketpair] {
            assert_eq!(evaluate(&p,ARCH,syscall as u32,0),DENY);
        }
    }
    /// Defect 28: the unit admits clone/clone3 once, for the DNS probe worker
    /// created before `install`; this filter must deny both afterwards.
    #[test]
    fn seccomp_denies_thread_and_process_creation_after_start() {
        let allowed=allowed_syscalls();
        for syscall in [libc::SYS_clone,libc::SYS_clone3,libc::SYS_execve,libc::SYS_execveat] {
            assert!(!allowed.contains(&syscall),"syscall {syscall}");
            assert_eq!(evaluate(&program(),ARCH,syscall as u32,0),DENY,"syscall {syscall}");
        }
        #[cfg(target_arch = "x86_64")]
        for syscall in [libc::SYS_fork,libc::SYS_vfork] {
            assert!(!allowed.contains(&syscall),"syscall {syscall}");
            assert_eq!(evaluate(&program(),ARCH,syscall as u32,0),DENY,"syscall {syscall}");
        }
    }

    /// Classic-BPF interpreter for the bootstrap's opcodes (LOAD of nr / arch
    /// / the low word of args[0], JEQ, JGE, AND, RET), forward jumps only.
    fn run(p: &[Instruction], arch: u32, nr: u32, arg0_low: u32) -> u32 {
        let (mut pc, mut a) = (0usize, 0u32);
        for _ in 0..p.len() + 1 {
            let i = p.get(pc).expect("BPF jump out of range");
            match i.code {
                LOAD => {
                    a = match i.k { 0 => nr, 4 => arch, k if k == ARG0_LOW => arg0_low, k => panic!("unexpected field {k}") };
                    pc += 1;
                }
                EQUAL => pc += 1 + usize::from(if a == i.k { i.jt } else { i.jf }),
                GREATER_EQUAL => pc += 1 + usize::from(if a >= i.k { i.jt } else { i.jf }),
                AND => { a &= i.k; pc += 1; }
                RETURN => return i.k,
                other => panic!("unexpected opcode {other:#x}"),
            }
        }
        panic!("nonterminating BPF")
    }
    fn boot(nr: i64, flags: libc::c_int) -> u32 { run(&bootstrap_program(), ARCH, nr as u32, flags as u32) }

    /// R8j round 3 F4: the bootstrap program's exact layout and constants.
    #[test]
    fn seccomp_bootstrap_program_structure_and_constants() {
        let p = bootstrap_program();
        let at = |index: usize| (p[index].code, p[index].jt, p[index].jf, p[index].k);
        // Architecture check, as in the final filter: unknown arch ⇒ KILL.
        assert_eq!([at(0), at(1), at(2), at(3)], [(LOAD,0,0,4), (EQUAL,1,0,ARCH), (RETURN,0,0,KILL), (LOAD,0,0,0)]);
        #[cfg(target_arch = "x86_64")]
        let base = { assert_eq!([at(4), at(5)], [(GREATER_EQUAL,0,1,0x40000000), (RETURN,0,0,DENY)]); 6 };
        #[cfg(target_arch = "aarch64")]
        let base = 4;
        assert_eq!([at(base), at(base + 1)], [(EQUAL,0,1,libc::SYS_clone3 as u32), (RETURN,0,0,NO_SYSCALL)]);
        assert_eq!([at(base + 2), at(base + 3), at(base + 4), at(base + 5), at(base + 6), at(base + 7)],
            [(EQUAL,0,5,libc::SYS_clone as u32), (LOAD,0,0,ARG0_LOW), (AND,0,0,THREAD_FLAGS),
             (EQUAL,0,1,THREAD_FLAGS), (RETURN,0,0,ALLOW), (RETURN,0,0,DENY)]);
        let denied = bootstrap_denied();
        for (n, call) in denied.iter().enumerate() {
            assert_eq!([at(base + 8 + 2 * n), at(base + 9 + 2 * n)], [(EQUAL,0,1,*call as u32), (RETURN,0,0,DENY)]);
        }
        assert_eq!(at(p.len() - 1), (RETURN,0,0,ALLOW), "default ALLOW");
        assert_eq!(p.len(), base + 8 + 2 * denied.len() + 1);
        #[cfg(target_arch = "x86_64")]
        assert_eq!((p.len(), denied), (23, vec![libc::SYS_execve, libc::SYS_execveat, libc::SYS_fork, libc::SYS_vfork]));
        #[cfg(target_arch = "aarch64")]
        assert_eq!((p.len(), denied), (17, vec![libc::SYS_execve, libc::SYS_execveat]));
        // CLONE_THREAD 0x00010000 | CLONE_VM 0x00000100, both in the low word.
        assert_eq!(THREAD_FLAGS, 0x0001_0100);
        assert_eq!(NO_SYSCALL, 0x0005_0000 | 38);
        #[cfg(target_endian = "little")]
        assert_eq!(ARG0_LOW, 16);
        assert!(u16::try_from(p.len()).is_ok());
    }

    /// The bootstrap's semantics on fake `seccomp_data`.
    #[test]
    fn seccomp_bootstrap_allows_only_thread_creation() {
        // glibc pthread_create's clone flags (and the bare minimum) ⇒ ALLOW.
        let pthread = libc::CLONE_VM | libc::CLONE_FS | libc::CLONE_FILES | libc::CLONE_SIGHAND | libc::CLONE_THREAD
            | libc::CLONE_SYSVSEM | libc::CLONE_SETTLS | libc::CLONE_PARENT_SETTID | libc::CLONE_CHILD_CLEARTID;
        assert_eq!(boot(libc::SYS_clone, pthread), ALLOW);
        assert_eq!(boot(libc::SYS_clone, libc::CLONE_THREAD | libc::CLONE_VM), ALLOW);
        // Fork-like (SIGCHLD only, glibc fork's flags) ⇒ EPERM.
        assert_eq!(boot(libc::SYS_clone, libc::SIGCHLD), DENY);
        assert_eq!(boot(libc::SYS_clone, libc::CLONE_CHILD_SETTID | libc::CLONE_CHILD_CLEARTID | libc::SIGCHLD), DENY);
        // vfork-like / posix_spawn (CLONE_VM without CLONE_THREAD) ⇒ EPERM.
        assert_eq!(boot(libc::SYS_clone, libc::CLONE_VM | libc::CLONE_VFORK | libc::SIGCHLD), DENY);
        assert_eq!(boot(libc::SYS_clone, libc::CLONE_VM), DENY);
        // CLONE_THREAD without CLONE_VM ⇒ EPERM; no flags ⇒ EPERM.
        assert_eq!(boot(libc::SYS_clone, libc::CLONE_THREAD | libc::CLONE_SIGHAND), DENY);
        assert_eq!(boot(libc::SYS_clone, 0), DENY);
        // clone3 ⇒ ENOSYS (glibc falls back to clone), whatever its argument.
        assert_eq!(boot(libc::SYS_clone3, 0), NO_SYSCALL);
        assert_eq!(boot(libc::SYS_clone3, pthread), NO_SYSCALL);
        // exec ⇒ EPERM; fork/vfork ⇒ EPERM (x86_64; absent on aarch64).
        for call in [libc::SYS_execve, libc::SYS_execveat] { assert_eq!(boot(call, 0), DENY, "{call}"); }
        #[cfg(target_arch = "x86_64")]
        for call in [libc::SYS_fork, libc::SYS_vfork] { assert_eq!(boot(call, 0), DENY, "{call}"); }
        // Everything else (what start-up needs) ⇒ ALLOW.
        for call in [libc::SYS_read, libc::SYS_write, libc::SYS_openat, libc::SYS_socket, libc::SYS_connect,
            libc::SYS_prctl, libc::SYS_seccomp, libc::SYS_mmap, libc::SYS_futex] {
            assert_eq!(boot(call, pthread), ALLOW, "{call}");
        }
        // Exhaustive over native numbers with fork-like flags.
        let denied = bootstrap_denied();
        for nr in 0..2048u32 {
            let expected = if nr == libc::SYS_clone3 as u32 { NO_SYSCALL }
                else if nr == libc::SYS_clone as u32 || denied.contains(&i64::from(nr)) { DENY } else { ALLOW };
            assert_eq!(run(&bootstrap_program(), ARCH, nr, libc::SIGCHLD as u32), expected, "syscall {nr}");
            // Unknown architecture ⇒ KILL.
            assert_eq!(run(&bootstrap_program(), 0, nr, 0), KILL);
        }
        // x86_64: x32-ABI numbers (same audit arch) cannot bypass the rules.
        #[cfg(target_arch = "x86_64")]
        for call in [libc::SYS_clone, libc::SYS_clone3, libc::SYS_execve, libc::SYS_fork, libc::SYS_read] {
            assert_eq!(run(&bootstrap_program(), ARCH, call as u32 | 0x40000000, pthread as u32), DENY, "x32 {call}");
        }
    }
}
