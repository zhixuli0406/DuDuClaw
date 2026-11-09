//! Fixed native-architecture daemon syscall allowlist, installed after bootstrap.
#![allow(unsafe_code)]
use duduclaw_isolation::egress::EgressError;

#[derive(Clone, Copy, Debug)]
struct Instruction { code: u16, jt: u8, jf: u8, k: u32 }
const LOAD: u16 = 0x20;
const EQUAL: u16 = 0x15;
const RETURN: u16 = 0x06;
const AND: u16 = 0x54;
const WRITE_OPEN_FLAGS: u32 = (libc::O_ACCMODE | libc::O_CREAT | libc::O_TRUNC | libc::O_APPEND | (libc::O_TMPFILE & !libc::O_DIRECTORY)) as u32;
const ALLOW: u32 = 0x7fff0000;
const KILL: u32 = 0x80000000;
const DENY: u32 = 0x00050000 | 1; // EPERM

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

/// Called after identity, configuration, listener and DNS startup probes. The
/// daemon uses a current-thread runtime; TSYNC also covers any existing threads.
/// No exec, clone, namespace or AF_VSOCK syscall is allowed. Filesystem
/// writes are denied by the unit and openat rejects writable/creating flags.
/// Read-only openat remains for NSS/config reads.
#[cfg(any(target_arch = "x86_64", target_arch = "aarch64"))]
pub fn install() -> Result<(), EgressError> {
    let filters: Vec<libc::sock_filter> = program().iter().map(|p| libc::sock_filter { code:p.code,jt:p.jt,jf:p.jf,k:p.k }).collect();
    let program = libc::sock_fprog { len: filters.len() as u16, filter: filters.as_ptr() as *mut libc::sock_filter };
    // The live filter buffer remains readable until seccomp copies it. NNP was
    // set/read back by daemon::harden. Any nonzero TSYNC result is a failure.
    let rc = unsafe { libc::syscall(libc::SYS_seccomp, 1u32, 1u32, &program) };
    if rc == 0 { Ok(()) } else { Err(EgressError::Unavailable) }
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
        for syscall in [libc::SYS_execve,libc::SYS_clone,libc::SYS_mount,libc::SYS_unshare,libc::SYS_socketpair] {
            assert_eq!(evaluate(&p,ARCH,syscall as u32,0),DENY);
        }
    }
}
