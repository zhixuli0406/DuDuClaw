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
pub fn init(arch: Architecture, status_fd: i32) -> Result<Filter, FilterError> {
    if status_fd <= 2 { return Err(FilterError::Policy); }
    let mut filter = Filter::new(arch);
    filter.block(arch.nr(1,64), &fd_rule(status_fd as u32, None, ALLOW, KILL));
    for nr in [arch.nr(61,260), arch.nr(247,95), arch.nr(62,129), arch.nr(13,134),
        arch.nr(3,57), arch.nr(15,139), arch.nr(14,135), arch.exit_group()] {
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
    if profile.digest(arch)? != expected_digest { return Err(FilterError::Digest); }
    if !profile.allowed_syscalls.iter().any(|nr| *nr == arch.execveat())
        || !profile.allowed_syscalls.iter().any(|nr| *nr == arch.exit_group()) {
        return Err(FilterError::Policy);
    }
    let mut filter = Filter::new(arch);
    common_rules(&mut filter, arch, |nr| {
        if profile.allowed_syscalls.iter().any(|allowed| *allowed == nr) { ALLOW } else { DENY }
    });
    for nr in &profile.allowed_syscalls { filter.action(*nr, ALLOW); }
    Ok(filter.finish(DENY))
}

#[cfg(test)]
mod tests {
    use super::*;
    // Interpret the emitted cBPF, rather than testing the policy source table only.
    fn evaluate(filter: &Filter, arch: u32, nr: u32, args: [u64;6]) -> u32 {
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
