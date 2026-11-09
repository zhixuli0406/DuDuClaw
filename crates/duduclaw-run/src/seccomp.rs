//! Argument-aware classic BPF policies for design r7 §4. Installed only after NNP.
//! Syscall tables are Linux UAPI x86_64 and asm-generic aarch64; no host libc numbers
//! are used by the portable builder, so both architectures are tested on any host.
use duduclaw_isolation::ids::Sha256Digest;

pub const ALLOW: u32 = 0x7fff0000;
pub const KILL: u32 = 0x80000000;
pub const DENY: u32 = 0x00050001; // EPERM
pub const ENOSYS: u32 = 0x00050026;
const LD: u16 = 0x20;
const JEQ: u16 = 0x15;
const JSET: u16 = 0x45;
const RET: u16 = 0x06;
const NEW_NAMESPACES: u32 = 0x00000080 | 0x00020000 | 0x02000000 | 0x04000000
    | 0x08000000 | 0x10000000 | 0x20000000 | 0x40000000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Architecture { X86_64, Aarch64 }
impl Architecture {
    pub const fn audit(self) -> u32 {
        match self { Self::X86_64 => 0xc000003e, Self::Aarch64 => 0xc00000b7 }
    }
    pub fn native() -> Result<Self, FilterError> {
        if cfg!(target_arch = "x86_64") { Ok(Self::X86_64) }
        else if cfg!(target_arch = "aarch64") { Ok(Self::Aarch64) }
        else { Err(FilterError::Architecture) }
    }
    fn nr(self, x86: u32, arm: u32) -> u32 {
        match self { Self::X86_64 => x86, Self::Aarch64 => arm }
    }
    pub fn execveat(self) -> u32 { self.nr(322, 281) }
    pub fn exit_group(self) -> u32 { self.nr(231, 94) }
    /// asm-generic (aarch64) has no `alarm`; init uses `setitimer` there.
    pub fn alarm(self) -> Option<u32> { match self { Self::X86_64 => Some(37), Self::Aarch64 => None } }
    pub fn setitimer(self) -> u32 { self.nr(38, 103) }
    pub fn prlimit64(self) -> u32 { self.nr(302, 261) }
    pub fn prctl(self) -> u32 { self.nr(157, 167) }
    pub fn seccomp(self) -> u32 { self.nr(317, 277) }
    pub fn setpgid(self) -> u32 { self.nr(109, 154) }
    /// HX §2.4 L_restrict calls without argument constraints: Landlock
    /// (create_ruleset, add_rule, restrict_self), `setsid`, `wait4`, `waitid`,
    /// `timerfd_create/settime/gettime`, `close_range`. Each only restricts the
    /// caller or supervises its own children [F3].
    pub fn launcher_restrict_calls(self) -> [u32; 10] {
        [444, 445, 446, self.nr(112, 157), self.nr(61, 260), self.nr(247, 95),
            self.nr(283, 85), self.nr(286, 86), self.nr(287, 87), 436]
    }
    /// Calls the in-trampoline launcher and its pre-exec helper children make
    /// that F_L does NOT add (HX3a: F_L = parent ∪ L_restrict). They must come
    /// from the parent runtime profile; `parent_supports_launcher` refuses a
    /// launcher for a parent that lacks any of them (HX3b, checked at plan and
    /// publication time). read, write, close, lseek, mmap, munmap,
    /// rt_sigaction, rt_sigprocmask, rt_sigreturn, kill, fcntl, chdir, getppid,
    /// fstatfs, getdents64, openat, mkdirat, unlinkat, ppoll, dup3, pipe2,
    /// sendmsg, recvmsg, clone, statx, openat2, pread64, execveat, exit_group.
    pub fn launcher_parent_calls(self) -> [u32; 29] {
        [self.nr(0, 63), self.nr(1, 64), self.nr(3, 57), self.nr(8, 62), self.nr(9, 222), self.nr(11, 215),
            self.nr(13, 134), self.nr(14, 135), self.nr(15, 139), self.nr(62, 129), self.nr(72, 25),
            self.nr(80, 49), self.nr(110, 173), self.nr(138, 44), self.nr(217, 61), self.nr(257, 56),
            self.nr(258, 34), self.nr(263, 35), self.nr(271, 73), self.nr(292, 24), self.nr(293, 59),
            self.nr(46, 211), self.nr(47, 212), self.nr(56, 220), self.nr(332, 291), 437, self.nr(17, 67),
            self.execveat(), self.exit_group()]
    }
}

/// HX3b: the launcher runs under F_L, whose only additions to the parent are
/// L_restrict; everything else it needs must already be in the parent profile.
pub fn parent_supports_launcher(arch: Architecture, parent: &RuntimeProfile) -> bool {
    arch.launcher_parent_calls().iter().all(|nr| parent.allows(*nr))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FilterError {
    #[error("unsupported seccomp architecture")] Architecture,
    #[error("invalid seccomp policy")] Policy,
    #[error("seccomp policy digest mismatch")] Digest,
    #[error("seccomp installation failed")] Install,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction { pub code: u16, pub jt: u8, pub jf: u8, pub k: u32 }
fn ld(offset: u32) -> Instruction { Instruction { code: LD, jt: 0, jf: 0, k: offset } }
fn ret(action: u32) -> Instruction { Instruction { code: RET, jt: 0, jf: 0, k: action } }
fn eq(value: u32, yes: u8, no: u8) -> Instruction { Instruction { code: JEQ, jt: yes, jf: no, k: value } }
fn arg(index: u32) -> u32 { 16 + index * 8 }

/// A prepared program owns no OS handles. Keep it alive through exec/_exit after
/// installation; dropping heap allocations under a restrictive filter may syscall.
#[derive(Debug, Clone)]
pub struct Filter { instructions: Vec<Instruction> }
impl Filter {
    pub fn instructions(&self) -> &[Instruction] { &self.instructions }
    fn new(arch: Architecture) -> Self {
        let mut instructions = vec![ld(4), eq(arch.audit(), 1, 0), ret(KILL), ld(0)];
        if arch == Architecture::X86_64 {
            instructions.extend([Instruction { code: JSET, jt: 0, jf: 1, k: 0x40000000 }, ret(KILL)]);
        }
        Self { instructions }
    }
    fn block(&mut self, syscall: u32, body: &[Instruction]) {
        // All bodies terminate in RET, and are fixed small snippets.
        assert!(body.len() <= u8::MAX as usize);
        self.instructions.push(ld(0));
        self.instructions.push(eq(syscall, 0, body.len() as u8));
        self.instructions.extend_from_slice(body);
    }
    fn action(&mut self, syscall: u32, action: u32) { self.block(syscall, &[ret(action)]); }
    fn finish(mut self, default: u32) -> Self { self.instructions.push(ret(default)); self }

    /// Must be called by the single-threaded trampoline after privilege readback.
    #[cfg(target_os = "linux")]
    #[allow(unsafe_code)]
    pub fn install(&self) -> Result<(), FilterError> {
        if self.instructions.is_empty() || self.instructions.len() > 4096 {
            return Err(FilterError::Policy);
        }
        // SAFETY: Instructions has the Linux sock_filter layout, verified below;
        // kernel copies the bounded program synchronously. NNP must already be set.
        unsafe {
            if libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1 {
                return Err(FilterError::Install);
            }
            let program = libc::sock_fprog {
                len: self.instructions.len() as u16,
                filter: self.instructions.as_ptr().cast::<libc::sock_filter>().cast_mut(),
            };
            if libc::syscall(libc::SYS_seccomp, 1u32, 0u32, &program) != 0 {
                return Err(FilterError::Install);
            }
        }
        Ok(())
    }
}

/// All unconditional common refusals. Additional process-memory operations are
/// denied as defense in depth; PID namespaces remain the primary run boundary.
pub fn denied_syscalls(arch: Architecture) -> Vec<u32> {
    let pairs = [
        (425,425), (426,426), (427,427), (321,280), (298,241), (323,282),
        (250,219), (248,217), (249,218), (165,40), (166,39),
        (430,430), (431,431), (432,432), (433,433), (428,428), (429,429),
        (442,442), (155,41), (272,97), (308,268), (438,438),
        (175,105), (313,273), (176,106), (246,104), (320,294),
        (169,142), (167,224), (168,225), (163,89), (179,60), (443,443),
        (101,117), (310,270), (311,271),
    ];
    pairs.into_iter().map(|(x,a)| arch.nr(x,a)).collect()
}

fn common_rules(filter: &mut Filter, arch: Architecture, allowed: impl Fn(u32) -> u32) {
    for nr in denied_syscalls(arch) { filter.action(nr, DENY); }
    filter.action(435, ENOSYS); // Never dereference clone3's userspace struct.
    let clone = arch.nr(56,220);
    filter.block(clone, &[
        ld(arg(0) + 4), eq(0, 1, 0), ret(DENY), ld(arg(0)),
        Instruction { code: JSET, jt: 0, jf: 1, k: NEW_NAMESPACES }, ret(DENY), ret(allowed(clone)),
    ]);
    let ioctl = arch.nr(16,29);
    filter.block(ioctl, &[
        ld(arg(1)), eq(0x5412, 0, 1), ret(DENY), // TIOCSTI
        eq(0x541c, 0, 1), ret(DENY), ret(allowed(ioctl)), // TIOCLINUX
    ]);
    let socket = arch.nr(41,198);
    filter.block(socket, &[
        ld(arg(0)),
        eq(1, 0, 1), ret(allowed(socket)), // AF_UNIX
        eq(2, 0, 1), ret(allowed(socket)), // AF_INET
        eq(10, 0, 1), ret(allowed(socket)), // AF_INET6
        eq(16, 1, 0), ret(DENY), // AF_NETLINK
        ld(arg(2)), eq(0, 0, 1), ret(allowed(socket)), ret(DENY), // NETLINK_ROUTE only
    ]);
    let socketpair = arch.nr(53,199);
    filter.block(socketpair, &[ld(arg(0)), eq(1, 0, 1), ret(allowed(socketpair)), ret(DENY)]);
}

pub fn common(arch: Architecture) -> Filter {
    let mut filter = Filter::new(arch);
    common_rules(&mut filter, arch, |_| ALLOW);
    filter.finish(ALLOW)
}

fn fd_rule(first: u32, second: Option<u32>, allowed: u32, denied: u32) -> Vec<Instruction> {
    let mut body = vec![ld(arg(0)), eq(first, 0, 1), ret(allowed)];
    if let Some(second) = second { body.extend([eq(second, 0, 1), ret(allowed)]); }
    body.push(ret(denied));
    body
}

/// Only report socket fd 0, a status read end and the child pidfd survive here.
/// Privilege readback must precede installation: capget/prctl are intentionally denied.
pub fn supervisor(arch: Architecture, status_fd: i32, pidfd: i32) -> Result<Filter, FilterError> {
    if status_fd <= 2 || pidfd <= 2 || status_fd == pidfd { return Err(FilterError::Policy); }
    let mut filter = Filter::new(arch);
    for nr in [arch.nr(0,63), arch.nr(1,64), arch.nr(46,211), arch.nr(47,212)] {
        filter.block(nr, &fd_rule(0, Some(status_fd as u32), ALLOW, KILL));
    }
    filter.block(arch.nr(247,95), &[
        ld(arg(0)), eq(3, 1, 0), ret(KILL), // P_PIDFD
        ld(arg(1)), eq(pidfd as u32, 0, 1), ret(ALLOW), ret(KILL),
    ]);
    for nr in [arch.nr(271,73), arch.nr(3,57), arch.nr(15,139), arch.nr(14,135),
        arch.nr(219,128), arch.exit_group()] { filter.action(nr, ALLOW); }
    Ok(filter.finish(KILL))
}

/// PID 1 may reap/forward signals only inside its already-created PID namespace.
/// `alarm`/`setitimer` bound init's wait for a draining launcher (HX §2.5 (c)):
/// they only arm a timer that signals init itself.
pub fn init(arch: Architecture, status_fd: i32) -> Result<Filter, FilterError> { init_with_kill(arch, status_fd, None) }

/// `init` plus, when a launcher is planned, `write` to the root-opened
/// `rt-runtime/cgroup.kill` descriptor (HX §2.5 (a) [F4]): init kills the
/// runtime's daemonized descendants the moment it reaps the runtime.
pub fn init_with_kill(arch: Architecture, status_fd: i32, kill_fd: Option<i32>) -> Result<Filter, FilterError> {
    if status_fd <= 2 || kill_fd.is_some_and(|fd| fd <= 2 || fd == status_fd) { return Err(FilterError::Policy); }
    let mut filter = Filter::new(arch);
    filter.block(arch.nr(1,64), &fd_rule(status_fd as u32, kill_fd.map(|fd| fd as u32), ALLOW, KILL));
    for nr in [arch.nr(61,260), arch.nr(247,95), arch.nr(62,129), arch.nr(13,134),
        arch.nr(3,57), arch.nr(15,139), arch.nr(14,135), arch.exit_group(), arch.setitimer()]
        .into_iter().chain(arch.alarm()) {
        filter.action(nr, ALLOW);
    }
    Ok(filter.finish(KILL))
}

/// Root registry's expanded allowlist. No moving '@system-service' aliases.
/// The signed manifest stores digest(arch); callers cannot assert its own digest.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeProfile { pub allowed_syscalls: Vec<u32> }
impl RuntimeProfile {
    pub fn canonical_bytes(&self, arch: Architecture) -> Result<Vec<u8>, FilterError> {
        if self.allowed_syscalls.is_empty() || self.allowed_syscalls.len() > 512 {
            return Err(FilterError::Policy);
        }
        let mut calls = self.allowed_syscalls.clone();
        calls.sort_unstable();
        if calls.windows(2).any(|pair| pair[0] == pair[1])
            || calls.iter().any(|nr| *nr >= 0x40000000) {
            return Err(FilterError::Policy);
        }
        let mut bytes = b"duduclaw-runtime-seccomp-v1\0".to_vec();
        bytes.extend_from_slice(&arch.audit().to_be_bytes());
        bytes.extend_from_slice(&(calls.len() as u32).to_be_bytes());
        for nr in calls { bytes.extend_from_slice(&nr.to_be_bytes()); }
        Ok(bytes)
    }
    pub fn digest(&self, arch: Architecture) -> Result<Sha256Digest, FilterError> {
        use sha2::{Digest, Sha256};
        Ok(Sha256Digest(Sha256::digest(self.canonical_bytes(arch)?).into()))
    }
}

/// Also embeds common restrictions, so even a mistaken profile cannot widen them.
/// Install on top of common after fd reordering, immediately before execveat.
pub fn runtime(arch: Architecture, profile: &RuntimeProfile,
    expected_digest: Sha256Digest) -> Result<Filter, FilterError> {
    checked_profile(arch, profile, expected_digest)?;
    let mut filter = Filter::new(arch);
    common_rules(&mut filter, arch, |nr| if profile.allows(nr) { ALLOW } else { DENY });
    for nr in &profile.allowed_syscalls { filter.action(*nr, ALLOW); }
    Ok(filter.finish(DENY))
}

impl RuntimeProfile {
    pub fn allows(&self, nr: u32) -> bool { self.allowed_syscalls.contains(&nr) }
}
fn checked_profile(arch: Architecture, profile: &RuntimeProfile, expected: Sha256Digest) -> Result<(), FilterError> {
    if profile.digest(arch)? != expected { return Err(FilterError::Digest); }
    if !profile.allows(arch.execveat()) || !profile.allows(arch.exit_group()) { return Err(FilterError::Policy); }
    Ok(())
}

// Argument-constrained bodies (HX §2.4 [F3]). `int`/`pid_t`/`unsigned int`
// arguments are compared on their low 32 bits, which is all the kernel reads;
// `unsigned long` arguments on both halves. Every body ends in RET.
/// `prlimit64(0, …)`: the caller's own limits only.
fn pid_zero_body() -> Vec<Instruction> { vec![ld(arg(0)), eq(0, 0, 1), ret(ALLOW), ret(DENY)] }
/// `setpgid(0, 0)`: the caller becomes its own group leader.
fn setpgid_self_body() -> Vec<Instruction> {
    vec![ld(arg(0)), eq(0, 0, 3), ld(arg(1)), eq(0, 0, 1), ret(ALLOW), ret(DENY)]
}
/// `seccomp(SECCOMP_SET_MODE_FILTER, 0, …)`: stacking a further filter only; no
/// TSYNC, listener or log flags, no other operation.
fn seccomp_filter_body() -> Vec<Instruction> {
    vec![ld(arg(0)), eq(1, 0, 3), ld(arg(1)), eq(0, 0, 1), ret(ALLOW), ret(DENY)]
}
/// `prctl(option, …)` with `option` in `options` (⊆ `PRCTL_SELF_ONLY`);
/// `PR_SET_DUMPABLE` only with arg2 == 0 (`SUID_DUMP_DISABLE`).
fn prctl_body(options: &[u32]) -> Result<Vec<Instruction>, FilterError> {
    use crate::helper_policy::{PRCTL_SELF_ONLY, PR_SET_DUMPABLE};
    if options.is_empty() || !options.iter().all(|option| PRCTL_SELF_ONLY.contains(option)) {
        return Err(FilterError::Policy);
    }
    let mut simple: Vec<u32> = options.iter().copied().filter(|option| *option != PR_SET_DUMPABLE).collect();
    simple.sort_unstable();
    simple.dedup();
    let mut body = vec![ld(arg(0))];
    for option in simple { body.extend([eq(option, 0, 1), ret(ALLOW)]); }
    if options.contains(&PR_SET_DUMPABLE) {
        body.extend([eq(PR_SET_DUMPABLE, 0, 5), ld(arg(1) + 4), eq(0, 0, 3), ld(arg(1)), eq(0, 0, 1), ret(ALLOW)]);
    }
    body.push(ret(DENY));
    Ok(body)
}

/// Launcher self-filter F_L = parent runtime profile ∪ L_restrict (HX §2.4).
/// L_restrict: `Architecture::launcher_restrict_calls` unconstrained, plus
/// `prctl` (all of `PRCTL_SELF_ONLY`), `prlimit64` (pid 0), `setpgid(0, 0)` and
/// `seccomp(SET_MODE_FILTER, 0)` argument-constrained unless the parent already
/// allows them outright (a union never narrows the parent). The common
/// refusals are embedded first, so no parent entry or L_restrict call can
/// reopen namespaces, mount, ptrace, bpf, io_uring or `pidfd_getfd`.
/// Install on top of common in the launcher child, after its Landlock.
pub fn launcher(arch: Architecture, parent: &RuntimeProfile, parent_digest: Sha256Digest) -> Result<Filter, FilterError> {
    use crate::helper_policy::PRCTL_SELF_ONLY;
    checked_profile(arch, parent, parent_digest)?;
    let mut filter = Filter::new(arch);
    common_rules(&mut filter, arch, |nr| if parent.allows(nr) { ALLOW } else { DENY });
    let constrained = [(arch.prctl(), prctl_body(&PRCTL_SELF_ONLY)?), (arch.prlimit64(), pid_zero_body()),
        (arch.setpgid(), setpgid_self_body()), (arch.seccomp(), seccomp_filter_body())];
    for (nr, body) in &constrained {
        if !parent.allows(*nr) { filter.block(*nr, body); }
    }
    for nr in arch.launcher_restrict_calls() {
        if !parent.allows(nr) { filter.action(nr, ALLOW); }
    }
    for nr in &parent.allowed_syscalls { filter.action(*nr, ALLOW); }
    Ok(filter.finish(DENY))
}

/// Signed helper filter F_H (HX §2.3 check 2, §2.4). Refuses unless the flat
/// helper allowlist ⊆ the parent's and every self-only exception is in the
/// closed set (`helper_policy::seccomp_within_parent`), so the only calls F_H
/// allows beyond the parent are `prlimit64(0, …)` and the listed `prctl`
/// options, compiled as argument checks. Installed by the helper child under
/// F_L immediately before `execveat`; the kernel then enforces F_L ∩ F_H.
/// HX security fix round: `helper_policy::refused_helper_calls(arch, egress)`
/// is EPERM first, whatever the signed list says — for an offline helper
/// (`!egress`) every socket-family call, for every helper the path-based
/// metadata calls Landlock does not mediate and SysV/POSIX-mq IPC.
pub fn helper(arch: Architecture, parent: &RuntimeProfile, parent_digest: Sha256Digest, profile: &RuntimeProfile,
    expected_digest: Sha256Digest, self_only: &[crate::helper_policy::SelfOnlyCall], egress: bool) -> Result<Filter, FilterError> {
    use crate::helper_policy::{refused_helper_calls, seccomp_within_parent, SelfOnlyCall};
    checked_profile(arch, parent, parent_digest)?;
    checked_profile(arch, profile, expected_digest)?;
    if !seccomp_within_parent(arch, parent, profile, self_only) { return Err(FilterError::Policy); }
    let mut filter = Filter::new(arch);
    for nr in refused_helper_calls(arch, egress) { filter.action(nr, DENY); }
    common_rules(&mut filter, arch, |nr| if profile.allows(nr) { ALLOW } else { DENY });
    for call in self_only {
        match call {
            SelfOnlyCall::Prlimit64SelfOnly => filter.block(arch.prlimit64(), &pid_zero_body()),
            SelfOnlyCall::Prctl { options } => filter.block(arch.prctl(), &prctl_body(options)?),
            // HX6 security fix round: Landlock (self-restricting) and a stacked filter only.
            SelfOnlyCall::NestedSandbox => {
                filter.block(arch.seccomp(), &seccomp_filter_body());
                for nr in crate::helper_policy::LANDLOCK_CALLS { filter.action(nr, ALLOW); }
            }
        }
    }
    for nr in &profile.allowed_syscalls { filter.action(*nr, ALLOW); }
    Ok(filter.finish(DENY))
}

#[cfg(test)]
#[path = "seccomp_hxfix_tests.rs"]
mod hxfix_tests;
#[cfg(test)]
#[path = "seccomp_hx6fix_tests.rs"]
mod hx6fix_tests;

#[cfg(test)]
mod tests {
    use super::*;
    // Interpret the emitted cBPF, rather than testing the policy source table only.
    pub(super) fn evaluate(filter: &Filter, arch: u32, nr: u32, args: [u64;6]) -> u32 {
        let mut acc = 0;
        let mut pc = 0;
        for _ in 0..4096 {
            let ins = filter.instructions[pc];
            pc += 1;
            match ins.code {
                LD => acc = match ins.k {
                    0 => nr, 4 => arch,
                    offset @ 16..=60 if offset % 4 == 0 => {
                        let value = args[((offset - 16) / 8) as usize];
                        if (offset - 16) % 8 == 0 { value as u32 } else { (value >> 32) as u32 }
                    }
                    _ => panic!("invalid data load"),
                },
                JEQ => pc += if acc == ins.k { ins.jt } else { ins.jf } as usize,
                JSET => pc += if acc & ins.k != 0 { ins.jt } else { ins.jf } as usize,
                RET => return ins.k,
                _ => panic!("invalid BPF opcode"),
            }
        }
        panic!("unterminated BPF")
    }
    #[test]
    fn seccomp_decoded_common_denies_all_dangerous_calls() {
        for arch in [Architecture::X86_64, Architecture::Aarch64] {
            let filter = common(arch);
            for nr in denied_syscalls(arch) { assert_eq!(evaluate(&filter, arch.audit(), nr, [0;6]), DENY, "{arch:?} {nr}"); }
            assert_eq!(evaluate(&filter, arch.audit(), 435, [0;6]), ENOSYS);
            assert_eq!(evaluate(&filter, 0x40000003, 0, [0;6]), KILL);
            assert_eq!(evaluate(&filter, arch.audit(), arch.nr(0,63), [0;6]), ALLOW);
            if arch == Architecture::X86_64 {
                assert_eq!(evaluate(&filter, arch.audit(), 0x40000000, [0;6]), KILL);
            }
        }
    }
    #[test]
    fn seccomp_decoded_clone_namespace_flags_and_ioctl() {
        for arch in [Architecture::X86_64, Architecture::Aarch64] {
            let filter = common(arch);
            for bit in [0x80,0x20000,0x2000000,0x4000000,0x8000000,0x10000000,0x20000000,0x40000000,1u64<<32] {
                assert_eq!(evaluate(&filter,arch.audit(),arch.nr(56,220),[bit,0,0,0,0,0]),DENY);
            }
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(56,220),[17,0,0,0,0,0]),ALLOW);
            for ioctl in [0x5412,0x541c,0x100005412] {
                assert_eq!(evaluate(&filter,arch.audit(),arch.nr(16,29),[0,ioctl,0,0,0,0]),DENY);
            }
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(16,29),[0,0x540e,0,0,0,0]),ALLOW);
        }
    }
    #[test]
    fn seccomp_decoded_socket_families_and_netlink_protocol() {
        for arch in [Architecture::X86_64, Architecture::Aarch64] {
            let filter=common(arch);
            for family in 0..=45 {
                for proto in [0,1,6,16] {
                    let expected = if matches!(family,1|2|10) || (family==16 && proto==0) { ALLOW } else { DENY };
                    assert_eq!(evaluate(&filter,arch.audit(),arch.nr(41,198),[family,1,proto,0,0,0]),expected);
                }
                let expected=if family==1 { ALLOW } else { DENY };
                assert_eq!(evaluate(&filter,arch.audit(),arch.nr(53,199),[family,1,0,0,0,0]),expected);
            }
        }
    }
    #[test]
    fn seccomp_decoded_supervisor_fd_and_waitid_constraints() {
        for arch in [Architecture::X86_64, Architecture::Aarch64] {
            let filter=supervisor(arch,10,11).unwrap();
            for nr in [arch.nr(0,63),arch.nr(1,64),arch.nr(46,211),arch.nr(47,212)] {
                for fd in 0..=12 {
                    assert_eq!(evaluate(&filter,arch.audit(),nr,[fd,0,0,0,0,0]),if fd==0 || fd==10 { ALLOW } else { KILL });
                }
            }
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(247,95),[3,11,0,0,0,0]),ALLOW);
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(247,95),[1,11,0,0,0,0]),KILL);
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(247,95),[3,12,0,0,0,0]),KILL);
            for nr in [arch.nr(62,129),arch.nr(41,198),arch.execveat(),arch.nr(157,167)] {
                assert_eq!(evaluate(&filter,arch.audit(),nr,[0;6]),KILL);
            }
            assert!(supervisor(arch,2,11).is_err());
            assert!(supervisor(arch,11,11).is_err());
        }
    }
    #[test]
    fn seccomp_decoded_init_only_writes_status_and_cannot_exec() {
        for arch in [Architecture::X86_64,Architecture::Aarch64] {
            let filter=init(arch,10).unwrap();
            for fd in 0..=11 {
                assert_eq!(evaluate(&filter,arch.audit(),arch.nr(1,64),[fd,0,0,0,0,0]),if fd==10 { ALLOW } else { KILL });
            }
            for nr in [arch.nr(61,260),arch.nr(247,95),arch.nr(62,129),arch.nr(13,134),arch.nr(3,57),arch.nr(15,139),arch.nr(14,135),arch.exit_group()] {
                assert_eq!(evaluate(&filter,arch.audit(),nr,[0;6]),ALLOW);
            }
            assert_eq!(evaluate(&filter,arch.audit(),arch.execveat(),[0;6]),KILL);
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(0,63),[0;6]),KILL);
        }
    }
    #[test]
    fn seccomp_runtime_digest_and_allowlist_cannot_widen_common() {
        for arch in [Architecture::X86_64,Architecture::Aarch64] {
            let mut calls=denied_syscalls(arch);
            calls.extend([arch.execveat(),arch.exit_group(),arch.nr(41,198),arch.nr(56,220)]);
            let profile=RuntimeProfile { allowed_syscalls:calls };
            let digest=profile.digest(arch).unwrap();
            let filter=runtime(arch,&profile,digest).unwrap();
            for nr in denied_syscalls(arch) { assert_eq!(evaluate(&filter,arch.audit(),nr,[0;6]),DENY); }
            assert_eq!(evaluate(&filter,arch.audit(),arch.execveat(),[0;6]),ALLOW);
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(0,63),[0;6]),DENY);
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(41,198),[40,1,0,0,0,0]),DENY);
            assert_eq!(evaluate(&filter,arch.audit(),arch.nr(56,220),[0x10000000,0,0,0,0,0]),DENY);
            assert!(runtime(arch,&profile,Sha256Digest([0;32])).is_err());
            let mut reordered=profile.clone(); reordered.allowed_syscalls.reverse();
            assert_eq!(reordered.digest(arch),Ok(digest));
            reordered.allowed_syscalls.push(arch.execveat());
            assert!(reordered.digest(arch).is_err());
        }
    }
    use crate::helper_policy::{SelfOnlyCall, PRCTL_SELF_ONLY, PR_SET_DUMPABLE, PR_SET_NO_NEW_PRIVS, PR_SET_PDEATHSIG};
    const ARCHES: [Architecture; 2] = [Architecture::X86_64, Architecture::Aarch64];
    /// A runtime profile without prctl/prlimit64/setpgid/seccomp/L_restrict calls.
    fn parent_profile(arch: Architecture) -> RuntimeProfile {
        RuntimeProfile { allowed_syscalls: vec![arch.nr(0,63), arch.nr(1,64), arch.nr(3,57), arch.nr(62,129),
            arch.nr(56,220), arch.nr(41,198), arch.nr(9,222), arch.nr(257,56), arch.execveat(), arch.exit_group()] }
    }
    fn helper_profile(arch: Architecture) -> RuntimeProfile {
        RuntimeProfile { allowed_syscalls: vec![arch.nr(0,63), arch.nr(1,64), arch.nr(9,222), arch.execveat(), arch.exit_group()] }
    }
    fn f_l(arch: Architecture, parent: &RuntimeProfile) -> Filter { launcher(arch, parent, parent.digest(arch).unwrap()).unwrap() }
    fn f_h(arch: Architecture, profile: &RuntimeProfile, self_only: &[SelfOnlyCall]) -> Result<Filter, FilterError> {
        let parent = parent_profile(arch);
        helper(arch, &parent, parent.digest(arch).unwrap(), profile, profile.digest(arch).unwrap(), self_only, false)
    }

    #[test]
    fn f_l_prlimit64_other_pid_denied() {
        for arch in ARCHES {
            let nr = arch.prlimit64();
            let fh = f_h(arch, &helper_profile(arch), &[SelfOnlyCall::Prlimit64SelfOnly]).unwrap();
            for filter in [f_l(arch, &parent_profile(arch)), fh] {
                assert_eq!(evaluate(&filter, arch.audit(), nr, [0, 7, 0, 0, 0, 0]), ALLOW, "{arch:?} own limits");
                for pid in [1u64, 2, 4242, 0xffff_ffff /* -1 */, 0xffff_fffe, 0x7fff_ffff] {
                    assert_eq!(evaluate(&filter, arch.audit(), nr, [pid, 7, 0, 0, 0, 0]), DENY, "{arch:?} pid {pid}");
                }
                // pid_t is the low 32 bits for the kernel; high garbage over 0 still names the caller.
                assert_eq!(evaluate(&filter, arch.audit(), nr, [1 << 32, 7, 0, 0, 0, 0]), ALLOW);
                assert_eq!(evaluate(&filter, arch.audit(), nr, [(1 << 32) | 1, 7, 0, 0, 0, 0]), DENY);
            }
            // Without the signed exception F_H has no prlimit64 at all.
            let plain = f_h(arch, &helper_profile(arch), &[]).unwrap();
            assert_eq!(evaluate(&plain, arch.audit(), nr, [0, 7, 0, 0, 0, 0]), DENY);
            // F_L is a union: a parent that already allows prlimit64 outright keeps it.
            let mut wide = parent_profile(arch); wide.allowed_syscalls.push(nr);
            assert_eq!(evaluate(&f_l(arch, &wide), arch.audit(), nr, [1, 7, 0, 0, 0, 0]), ALLOW);
        }
    }

    #[test]
    fn f_l_prctl_option_allowlist() {
        for arch in ARCHES {
            let nr = arch.prctl();
            let filter = f_l(arch, &parent_profile(arch));
            for option in 0..=80u32 {
                let expected = if PRCTL_SELF_ONLY.contains(&option) { ALLOW } else { DENY };
                assert_eq!(evaluate(&filter, arch.audit(), nr, [option as u64, 0, 0, 0, 0, 0]), expected, "{arch:?} {option}");
            }
            // PR_SET_DUMPABLE only to 0; never back to dumpable or "suid safe".
            for (value, expected) in [(0u64, ALLOW), (1, DENY), (2, DENY), (1 << 32, DENY)] {
                assert_eq!(evaluate(&filter, arch.audit(), nr, [PR_SET_DUMPABLE as u64, value, 0, 0, 0, 0]), expected);
            }
            // Options that change other processes, capabilities or the memory map stay denied.
            // (SET_CHILD_SUBREAPER = 36 is self-only since the HX6 security fix round: it only
            // re-parents the caller's own orphaned descendants to it.)
            for option in [8u64 /* SET_KEEPCAPS */, 15 /* SET_NAME */, 22 /* SET_SECCOMP */, 28 /* SET_SECUREBITS */,
                35 /* SET_MM */, 47 /* CAP_AMBIENT */, 0x5961_6d61 /* SET_PTRACER */] {
                assert_eq!(evaluate(&filter, arch.audit(), nr, [option, 0, 0, 0, 0, 0]), DENY, "{option}");
            }
            // F_H holds only the options its binding lists.
            let fh = f_h(arch, &helper_profile(arch),
                &[SelfOnlyCall::Prctl { options: vec![PR_SET_NO_NEW_PRIVS, PR_SET_DUMPABLE] }]).unwrap();
            assert_eq!(evaluate(&fh, arch.audit(), nr, [PR_SET_NO_NEW_PRIVS as u64, 1, 0, 0, 0, 0]), ALLOW);
            assert_eq!(evaluate(&fh, arch.audit(), nr, [PR_SET_DUMPABLE as u64, 0, 0, 0, 0, 0]), ALLOW);
            assert_eq!(evaluate(&fh, arch.audit(), nr, [PR_SET_DUMPABLE as u64, 1, 0, 0, 0, 0]), DENY);
            assert_eq!(evaluate(&fh, arch.audit(), nr, [PR_SET_PDEATHSIG as u64, 9, 0, 0, 0, 0]), DENY);
            for options in [vec![], vec![22], vec![PR_SET_DUMPABLE, 35], vec![38, 38]] {
                assert_eq!(f_h(arch, &helper_profile(arch), &[SelfOnlyCall::Prctl { options }]).err(), Some(FilterError::Policy));
            }
            let fh = f_h(arch, &helper_profile(arch), &[SelfOnlyCall::Prctl { options: vec![PR_SET_PDEATHSIG] }]).unwrap();
            assert_eq!(evaluate(&fh, arch.audit(), nr, [PR_SET_PDEATHSIG as u64, 9, 0, 0, 0, 0]), ALLOW);
            assert_eq!(evaluate(&fh, arch.audit(), nr, [PR_SET_DUMPABLE as u64, 0, 0, 0, 0, 0]), DENY);
        }
    }

    #[test]
    fn init_filter_allows_alarm() {
        for arch in ARCHES {
            let filter = init(arch, 10).unwrap();
            assert_eq!(evaluate(&filter, arch.audit(), arch.setitimer(), [0; 6]), ALLOW, "{arch:?}");
            match arch {
                Architecture::X86_64 => {
                    assert_eq!(arch.alarm(), Some(37));
                    assert_eq!(evaluate(&filter, arch.audit(), 37, [5, 0, 0, 0, 0, 0]), ALLOW);
                }
                Architecture::Aarch64 => assert_eq!(arch.alarm(), None, "asm-generic has no alarm"),
            }
            // Still no exec, no reads, no writes beyond the status fd.
            assert_eq!(evaluate(&filter, arch.audit(), arch.execveat(), [0; 6]), KILL);
            assert_eq!(evaluate(&filter, arch.audit(), arch.nr(0, 63), [0; 6]), KILL);
            assert_eq!(evaluate(&filter, arch.audit(), arch.nr(1, 64), [3, 0, 0, 0, 0, 0]), KILL);
            assert_eq!(evaluate(&filter, arch.audit(), arch.prlimit64(), [0; 6]), KILL);
        }
    }

    #[test]
    fn f_l_is_parent_union_restrict_and_keeps_common_refusals() {
        for arch in ARCHES {
            let mut parent = parent_profile(arch);
            parent.allowed_syscalls.extend(denied_syscalls(arch));
            let filter = f_l(arch, &parent);
            let parent_filter = runtime(arch, &parent, parent.digest(arch).unwrap()).unwrap();
            for nr in denied_syscalls(arch) { assert_eq!(evaluate(&filter, arch.audit(), nr, [0; 6]), DENY, "{nr}"); }
            for nr in &parent.allowed_syscalls {
                for args in [[0; 6], [1, 0, 0, 0, 0, 0], [0x10000000, 0, 0, 0, 0, 0]] {
                    assert_eq!(evaluate(&filter, arch.audit(), *nr, args), evaluate(&parent_filter, arch.audit(), *nr, args));
                }
            }
            for nr in arch.launcher_restrict_calls() { assert_eq!(evaluate(&filter, arch.audit(), nr, [3, 9, 0, 0, 0, 0]), ALLOW); }
            assert_eq!(evaluate(&filter, arch.audit(), arch.setpgid(), [0; 6]), ALLOW);
            for args in [[0, 5, 0, 0, 0, 0], [3, 0, 0, 0, 0, 0], [7, 7, 0, 0, 0, 0]] {
                assert_eq!(evaluate(&filter, arch.audit(), arch.setpgid(), args), DENY, "setpgid {args:?}");
            }
            assert_eq!(evaluate(&filter, arch.audit(), arch.seccomp(), [1, 0, 0x1000, 0, 0, 0]), ALLOW);
            // STRICT, TSYNC/listener flags and GET_ACTION_AVAIL/NOTIF_SIZES are refused.
            for args in [[0, 0, 0, 0, 0, 0], [1, 1, 0, 0, 0, 0], [1, 8, 0, 0, 0, 0], [2, 0, 0, 0, 0, 0], [3, 0, 0, 0, 0, 0]] {
                assert_eq!(evaluate(&filter, arch.audit(), arch.seccomp(), args), DENY, "seccomp {args:?}");
            }
            // Calls in neither set stay denied; the filter still kills foreign arches.
            for nr in [arch.nr(59, 221) /* execve */, arch.nr(105, 146) /* setuid */, arch.nr(90, 52) /* chmod / fchmod */] {
                assert_eq!(evaluate(&filter, arch.audit(), nr, [0; 6]), DENY, "{nr}");
            }
            assert_eq!(evaluate(&filter, 0x40000003, 0, [0; 6]), KILL);
            assert!(launcher(arch, &parent, Sha256Digest([0; 32])).is_err());
        }
    }

    #[test]
    fn f_h_is_within_parent_decoded() {
        for arch in ARCHES {
            let parent = parent_profile(arch);
            let parent_filter = runtime(arch, &parent, parent.digest(arch).unwrap()).unwrap();
            let self_only = [SelfOnlyCall::Prlimit64SelfOnly,
                SelfOnlyCall::Prctl { options: vec![PR_SET_NO_NEW_PRIVS, PR_SET_DUMPABLE, PR_SET_PDEATHSIG] }];
            let filter = f_h(arch, &helper_profile(arch), &self_only).unwrap();
            let samples = [[0; 6], [1, 0, 0, 0, 0, 0], [0, 1, 0, 0, 0, 0], [4, 0, 0, 0, 0, 0], [4, 1, 0, 0, 0, 0],
                [38, 1, 0, 0, 0, 0], [1, 9, 0, 0, 0, 0], [17, 0, 0, 0, 0, 0], [2, 1, 0, 0, 0, 0], [16, 3, 0, 0, 0, 0]];
            let exception = |nr: u32, args: [u64; 6]| (nr == arch.prlimit64() && args[0] as u32 == 0)
                || (nr == arch.prctl() && matches!((args[0] as u32, args[1]), (38, _) | (1, _) | (4, 0)));
            for nr in 0..480 {
                for args in samples {
                    if evaluate(&filter, arch.audit(), nr, args) == ALLOW {
                        assert!(evaluate(&parent_filter, arch.audit(), nr, args) == ALLOW || exception(nr, args),
                            "{arch:?} F_H allows {nr} {args:?} beyond the parent");
                    }
                }
            }
            // Policy-level refusals: a call outside the parent, an unconstrained
            // prctl/prlimit64 the parent lacks, or a profile digest mismatch.
            let mut outside = helper_profile(arch); outside.allowed_syscalls.push(arch.nr(105, 146));
            assert_eq!(f_h(arch, &outside, &[]).err(), Some(FilterError::Policy));
            for nr in [arch.prctl(), arch.prlimit64()] {
                let mut flat = helper_profile(arch); flat.allowed_syscalls.push(nr);
                assert_eq!(f_h(arch, &flat, &[]).err(), Some(FilterError::Policy));
            }
            let parent_digest = parent.digest(arch).unwrap();
            let profile = helper_profile(arch);
            assert_eq!(helper(arch, &parent, parent_digest, &profile, Sha256Digest([0; 32]), &[], false).err(), Some(FilterError::Digest));
            assert_eq!(helper(arch, &parent, Sha256Digest([0; 32]), &profile, profile.digest(arch).unwrap(), &[], false).err(),
                Some(FilterError::Digest));
            // F_H embeds the common refusals and execs nothing but via execveat.
            for nr in denied_syscalls(arch) { assert_eq!(evaluate(&filter, arch.audit(), nr, [0; 6]), DENY); }
            assert_eq!(evaluate(&filter, arch.audit(), arch.execveat(), [0; 6]), ALLOW);
            // Offline helper: no socket in F_H, so every family is EPERM.
            for family in [1u64, 2, 10, 16] {
                assert_eq!(evaluate(&filter, arch.audit(), arch.nr(41, 198), [family, 1, 0, 0, 0, 0]), DENY);
            }
        }
    }

    #[test]
    fn init_filter_allows_cgroup_kill_write_only() {
        for arch in ARCHES {
            let filter = init_with_kill(arch, 10, Some(12)).unwrap();
            for fd in 0..=13 {
                let expected = if fd == 10 || fd == 12 { ALLOW } else { KILL };
                assert_eq!(evaluate(&filter, arch.audit(), arch.nr(1, 64), [fd, 0, 0, 0, 0, 0]), expected, "{arch:?} {fd}");
            }
            assert_eq!(evaluate(&filter, arch.audit(), arch.execveat(), [0; 6]), KILL);
            assert_eq!(evaluate(&filter, arch.audit(), arch.nr(0, 63), [12, 0, 0, 0, 0, 0]), KILL);
            assert_eq!(init(arch, 10).unwrap().instructions(), init_with_kill(arch, 10, None).unwrap().instructions());
            for bad in [Some(2), Some(10)] { assert!(init_with_kill(arch, 10, bad).is_err()); }
        }
    }

    #[test]
    fn launcher_refused_for_parent_without_launcher_calls() {
        for arch in ARCHES {
            let calls = arch.launcher_parent_calls();
            let mut sorted = calls.to_vec(); sorted.sort_unstable(); sorted.dedup();
            assert_eq!(sorted.len(), calls.len(), "{arch:?} duplicate launcher call");
            // None of them is an L_restrict call or a common refusal: they really come from the parent.
            for nr in calls {
                assert!(!arch.launcher_restrict_calls().contains(&nr) && !denied_syscalls(arch).contains(&nr), "{nr}");
            }
            let full = RuntimeProfile { allowed_syscalls: calls.to_vec() };
            assert!(parent_supports_launcher(arch, &full));
            for missing in calls {
                let parent = RuntimeProfile { allowed_syscalls: calls.iter().copied().filter(|nr| *nr != missing).collect() };
                assert!(!parent_supports_launcher(arch, &parent), "{arch:?} without {missing}");
            }
            assert!(!parent_supports_launcher(arch, &parent_profile(arch)));
        }
    }

    #[cfg(target_os="linux")]
    #[test]
    fn seccomp_native_uapi_numbers_for_launcher_parent_calls() {
        let arch = Architecture::native().unwrap();
        let expected = [libc::SYS_read, libc::SYS_write, libc::SYS_close, libc::SYS_lseek, libc::SYS_mmap,
            libc::SYS_munmap, libc::SYS_rt_sigaction, libc::SYS_rt_sigprocmask, libc::SYS_rt_sigreturn, libc::SYS_kill,
            libc::SYS_fcntl, libc::SYS_chdir, libc::SYS_getppid, libc::SYS_fstatfs, libc::SYS_getdents64,
            libc::SYS_openat, libc::SYS_mkdirat, libc::SYS_unlinkat, libc::SYS_ppoll, libc::SYS_dup3, libc::SYS_pipe2,
            libc::SYS_sendmsg, libc::SYS_recvmsg, libc::SYS_clone, libc::SYS_statx, libc::SYS_openat2,
            libc::SYS_pread64, libc::SYS_execveat, libc::SYS_exit_group];
        assert_eq!(arch.launcher_parent_calls(), expected.map(|nr| nr as u32));
    }

    #[cfg(target_os="linux")]
    #[test]
    fn seccomp_native_uapi_numbers_for_launcher_and_helpers() {
        let arch = Architecture::native().unwrap();
        assert_eq!(arch.prlimit64(), libc::SYS_prlimit64 as u32);
        assert_eq!(arch.prctl(), libc::SYS_prctl as u32);
        assert_eq!(arch.seccomp(), libc::SYS_seccomp as u32);
        assert_eq!(arch.setpgid(), libc::SYS_setpgid as u32);
        assert_eq!(arch.setitimer(), libc::SYS_setitimer as u32);
        #[cfg(target_arch = "x86_64")]
        let alarm = Some(libc::SYS_alarm as u32);
        #[cfg(not(target_arch = "x86_64"))]
        let alarm = None;
        assert_eq!(arch.alarm(), alarm);
        let expected = [libc::SYS_landlock_create_ruleset, libc::SYS_landlock_add_rule, libc::SYS_landlock_restrict_self,
            libc::SYS_setsid, libc::SYS_wait4, libc::SYS_waitid, libc::SYS_timerfd_create, libc::SYS_timerfd_settime,
            libc::SYS_timerfd_gettime, libc::SYS_close_range];
        assert_eq!(arch.launcher_restrict_calls(), expected.map(|nr| nr as u32));
        assert_eq!(crate::helper_policy::prlimit64(arch), arch.prlimit64());
        assert_eq!(crate::helper_policy::prctl(arch), arch.prctl());
    }

    #[cfg(target_os="linux")]
    #[test]
    fn seccomp_native_uapi_numbers_and_instruction_layout() {
        let arch=Architecture::native().unwrap();
        assert_eq!(arch.execveat(),libc::SYS_execveat as u32);
        assert_eq!(arch.exit_group(),libc::SYS_exit_group as u32);
        assert_eq!(std::mem::size_of::<Instruction>(),std::mem::size_of::<libc::sock_filter>());
        assert_eq!(std::mem::align_of::<Instruction>(),std::mem::align_of::<libc::sock_filter>());
        let filter=common(arch);
        for nr in [libc::SYS_io_uring_setup,libc::SYS_io_uring_enter,libc::SYS_io_uring_register,
            libc::SYS_bpf,libc::SYS_perf_event_open,libc::SYS_userfaultfd,libc::SYS_keyctl,
            libc::SYS_add_key,libc::SYS_request_key,libc::SYS_mount,libc::SYS_umount2,
            libc::SYS_fsopen,libc::SYS_fsconfig,libc::SYS_fsmount,libc::SYS_fspick,
            libc::SYS_open_tree,libc::SYS_move_mount,libc::SYS_mount_setattr,libc::SYS_pivot_root,
            libc::SYS_unshare,libc::SYS_setns,libc::SYS_pidfd_getfd,libc::SYS_init_module,
            libc::SYS_finit_module,libc::SYS_delete_module,libc::SYS_kexec_load,
            libc::SYS_kexec_file_load,libc::SYS_reboot,libc::SYS_swapon,libc::SYS_swapoff,
            libc::SYS_acct,libc::SYS_quotactl,libc::SYS_quotactl_fd,libc::SYS_ptrace,
            libc::SYS_process_vm_readv,libc::SYS_process_vm_writev] {
            assert_eq!(evaluate(&filter,arch.audit(),nr as u32,[0;6]),DENY);
        }
    }
}
