//! HX6 security fix round, findings 1 and 2 (D8 revision §4 [F10], threat
//! rows T5/T16): the nested seccomp filter the recording helper installs on
//! its node/Chromium tree, stacked under F_L ∩ F_H (a stacked filter can only
//! narrow; everything not named here falls through to the outer filters).
//!
//! - Landlock mediates neither pathname `AF_UNIX` sockets nor UDP/raw sockets
//!   on any ABI, so the browser may create only TCP sockets: `socket()` with
//!   `AF_INET`/`AF_INET6`, type `SOCK_STREAM` (the `SOCK_NONBLOCK` and
//!   `SOCK_CLOEXEC` flag bits are masked off) and protocol 0 or `IPPROTO_TCP`
//!   (no SCTP/MPTCP stream sockets, which the TCP port rules do not cover).
//!   The browser Landlock layer pins TCP connect to the forwarder port on
//!   ABI ≥ 4.
//! - `socketpair()` stays for Chromium's IPC, `AF_UNIX` stream/seqpacket only:
//!   a datagram pair could `sendto()` any pathname datagram socket.
//! - `sendto`/`sendmsg`/`sendmmsg` with `MSG_FASTOPEN` are refused: TCP Fast
//!   Open connects from a send call, not from `connect(2)`.
//! - `setsid` and `setpgid` are refused (any arguments), so no browser process
//!   can leave the process group the helper kills and waits for.
//!
//! Denials are EPERM, a foreign architecture (and x32 on x86_64) is killed.
//! The program is built before `fork`; the child only issues one
//! `seccomp(SECCOMP_SET_MODE_FILTER, 0, …)`, which F_L allows argument-
//! constrained and F_H through `SelfOnlyCall::NestedSandbox`.

pub const ALLOW: u32 = 0x7fff_0000;
pub const KILL: u32 = 0x8000_0000;
/// `SECCOMP_RET_ERRNO | EPERM`.
pub const DENY: u32 = 0x0005_0001;
pub const AF_UNIX: u32 = 1;
pub const AF_INET: u32 = 2;
pub const AF_INET6: u32 = 10;
pub const SOCK_STREAM: u32 = 1;
pub const SOCK_SEQPACKET: u32 = 5;
pub const SOCK_NONBLOCK: u32 = 0o4000;
pub const SOCK_CLOEXEC: u32 = 0o2_000_000;
pub const IPPROTO_TCP: u32 = 6;
pub const MSG_FASTOPEN: u32 = 0x2000_0000;
const TYPE_MASK: u32 = !(SOCK_NONBLOCK | SOCK_CLOEXEC);
const X32_BIT: u32 = 0x4000_0000;
const MAX_INSTRUCTIONS: usize = 4096;

const LD: u16 = 0x20;
const JEQ: u16 = 0x15;
const JSET: u16 = 0x45;
const AND: u16 = 0x54;
const RET: u16 = 0x06;

/// Linux UAPI numbers (x86_64, asm-generic aarch64); no host libc numbers, so
/// both programs are built and decoded on any builder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch { X86_64, Aarch64 }
impl Arch {
    pub fn native() -> Option<Self> {
        if cfg!(target_arch = "x86_64") { Some(Self::X86_64) }
        else if cfg!(target_arch = "aarch64") { Some(Self::Aarch64) }
        else { None }
    }
    pub const fn audit(self) -> u32 { match self { Self::X86_64 => 0xc000_003e, Self::Aarch64 => 0xc000_00b7 } }
    const fn nr(self, x86: u32, arm: u32) -> u32 { match self { Self::X86_64 => x86, Self::Aarch64 => arm } }
    pub const fn socket(self) -> u32 { self.nr(41, 198) }
    pub const fn socketpair(self) -> u32 { self.nr(53, 199) }
    pub const fn sendto(self) -> u32 { self.nr(44, 206) }
    pub const fn sendmsg(self) -> u32 { self.nr(46, 211) }
    pub const fn sendmmsg(self) -> u32 { self.nr(307, 269) }
    pub const fn setsid(self) -> u32 { self.nr(112, 157) }
    pub const fn setpgid(self) -> u32 { self.nr(109, 154) }
}

/// The kernel's `struct sock_filter` layout.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Instruction { pub code: u16, pub jt: u8, pub jf: u8, pub k: u32 }
const fn ld(offset: u32) -> Instruction { Instruction { code: LD, jt: 0, jf: 0, k: offset } }
const fn ret(action: u32) -> Instruction { Instruction { code: RET, jt: 0, jf: 0, k: action } }
const fn jeq(value: u32, jt: u8, jf: u8) -> Instruction { Instruction { code: JEQ, jt, jf, k: value } }
const fn jset(bits: u32, jt: u8, jf: u8) -> Instruction { Instruction { code: JSET, jt, jf, k: bits } }
const fn and(mask: u32) -> Instruction { Instruction { code: AND, jt: 0, jf: 0, k: mask } }
/// Low 32 bits of syscall argument `index` (`int`/`unsigned int` arguments:
/// all the kernel reads). Both architectures are little-endian.
const fn arg(index: u32) -> u32 { 16 + index * 8 }

/// `socket(family, type, protocol)`: TCP only.
fn socket_body() -> [Instruction; 11] {
    [ld(arg(0)),
        jeq(AF_INET, 1, 0),
        jeq(AF_INET6, 0, 7),       // neither family ⇒ DENY (index 10)
        ld(arg(1)), and(TYPE_MASK),
        jeq(SOCK_STREAM, 0, 4),    // not a stream socket ⇒ DENY
        ld(arg(2)),
        jeq(0, 1, 0),              // default protocol ⇒ ALLOW (index 9)
        jeq(IPPROTO_TCP, 0, 1),
        ret(ALLOW), ret(DENY)]
}
/// `socketpair(AF_UNIX, SOCK_STREAM | SOCK_SEQPACKET, …)` only.
fn socketpair_body() -> [Instruction; 8] {
    [ld(arg(0)),
        jeq(AF_UNIX, 0, 5),        // not AF_UNIX ⇒ DENY (index 7)
        ld(arg(1)), and(TYPE_MASK),
        jeq(SOCK_STREAM, 1, 0),
        jeq(SOCK_SEQPACKET, 0, 1),
        ret(ALLOW), ret(DENY)]
}
/// A send call whose `flags` (argument `index`) carry `MSG_FASTOPEN`.
fn no_fastopen_body(index: u32) -> [Instruction; 4] {
    [ld(arg(index)), jset(MSG_FASTOPEN, 0, 1), ret(DENY), ret(ALLOW)]
}

fn block(program: &mut Vec<Instruction>, syscall: u32, body: &[Instruction]) -> Option<()> {
    program.push(ld(0));
    program.push(jeq(syscall, 0, u8::try_from(body.len()).ok()?));
    program.extend_from_slice(body);
    Some(())
}

/// The whole program for `arch`. `None` only if a body could not be encoded
/// (never for the fixed bodies above; the caller fails closed).
pub fn program(arch: Arch) -> Option<Vec<Instruction>> {
    let mut program = vec![ld(4), jeq(arch.audit(), 1, 0), ret(KILL), ld(0)];
    if arch == Arch::X86_64 { program.extend([jset(X32_BIT, 0, 1), ret(KILL)]); }
    block(&mut program, arch.socket(), &socket_body())?;
    block(&mut program, arch.socketpair(), &socketpair_body())?;
    block(&mut program, arch.sendto(), &no_fastopen_body(3))?;
    block(&mut program, arch.sendmsg(), &no_fastopen_body(2))?;
    block(&mut program, arch.sendmmsg(), &no_fastopen_body(3))?;
    block(&mut program, arch.setsid(), &[ret(DENY)])?;
    block(&mut program, arch.setpgid(), &[ret(DENY)])?;
    program.push(ret(ALLOW));
    (program.len() <= MAX_INSTRUCTIONS).then_some(program)
}

/// A prepared, not yet installed filter. Keep it alive until `spawn`
/// returned: the forked child reads the program through [`Self::raw`].
#[cfg(target_os = "linux")]
pub struct BrowserFilter {
    _instructions: Box<[Instruction]>,
    program: Box<libc::sock_fprog>,
}
#[cfg(target_os = "linux")]
impl BrowserFilter {
    /// Built before any fork. `Err` ⇒ the browser never starts.
    pub fn prepare() -> Result<Self, ()> {
        let arch = Arch::native().ok_or(())?;
        let instructions = program(arch).ok_or(())?.into_boxed_slice();
        let len = u16::try_from(instructions.len()).map_err(|_| ())?;
        let program = Box::new(libc::sock_fprog {
            len, filter: instructions.as_ptr().cast::<libc::sock_filter>().cast_mut() });
        Ok(Self { _instructions: instructions, program })
    }
    /// Address of the `sock_fprog` for a `pre_exec` closure (which must be
    /// `Send + Sync`, so it carries the address, not the pointer). Both heap
    /// allocations are stable while `self` lives.
    pub fn raw(&self) -> usize { &*self.program as *const libc::sock_fprog as usize }
}

/// In the forked child (async-signal-safe: one syscall, no allocation). NNP
/// is already set for the whole helper tree by the trampoline.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn install_raw(program: usize) -> std::io::Result<()> {
    // SAFETY: `program` is the address of a live `sock_fprog` whose filter
    // array outlives this call (`BrowserFilter::raw`); the kernel copies it.
    if unsafe { libc::syscall(libc::SYS_seccomp, 1u32, 0u32, program as *const libc::sock_fprog) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const ARCHES: [Arch; 2] = [Arch::X86_64, Arch::Aarch64];

    /// Interpret the emitted cBPF (not the source tables).
    fn evaluate(program: &[Instruction], arch: u32, nr: u32, args: [u64; 6]) -> u32 {
        let mut acc = 0u32;
        let mut pc = 0usize;
        for _ in 0..MAX_INSTRUCTIONS {
            let ins = *program.get(pc).expect("jump out of range");
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
                AND => acc &= ins.k,
                JEQ => pc += usize::from(if acc == ins.k { ins.jt } else { ins.jf }),
                JSET => pc += usize::from(if acc & ins.k != 0 { ins.jt } else { ins.jf }),
                RET => return ins.k,
                _ => panic!("invalid BPF opcode"),
            }
        }
        panic!("unterminated BPF")
    }

    #[test]
    fn hx6fix_browser_filter_decoded_allows_only_tcp_and_denies_session_escape() {
        for arch in ARCHES {
            let program = program(arch).unwrap();
            let run = |nr: u32, args: [u64; 6]| evaluate(&program, arch.audit(), nr, args);
            let socket = |family: u64, kind: u64, protocol: u64| run(arch.socket(), [family, kind, protocol, 0, 0, 0]);
            // TCP over IPv4/IPv6, with or without the flag bits.
            for family in [AF_INET, AF_INET6] {
                for kind in [SOCK_STREAM, SOCK_STREAM | SOCK_NONBLOCK, SOCK_STREAM | SOCK_CLOEXEC,
                    SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC] {
                    for protocol in [0, IPPROTO_TCP] {
                        assert_eq!(socket(family.into(), kind.into(), protocol.into()), ALLOW, "{arch:?} {family} {kind} {protocol}");
                    }
                }
            }
            // Pathname/abstract UNIX sockets, UDP, raw, netlink, packet, SCTP/MPTCP streams.
            for family in [0u64, 1, 3, 4, 16, 17, 38, 40, 44] {
                assert_eq!(socket(family, SOCK_STREAM.into(), 0), DENY, "{arch:?} family {family}");
            }
            for kind in [2u64 /* DGRAM */, 3 /* RAW */, 5 /* SEQPACKET */, 10 /* PACKET */, 1 | 0x100, 1 | (1 << 31)] {
                assert_eq!(socket(AF_INET.into(), kind, 0), DENY, "{arch:?} type {kind:#x}");
            }
            for protocol in [17u64 /* UDP */, 132 /* SCTP */, 262 /* MPTCP */, 255] {
                assert_eq!(socket(AF_INET6.into(), SOCK_STREAM.into(), protocol), DENY, "{arch:?} protocol {protocol}");
            }
            // `int` arguments: the kernel reads the low 32 bits only.
            assert_eq!(socket((1 << 32) | u64::from(AF_INET), SOCK_STREAM.into(), 0), ALLOW);
            assert_eq!(socket((1 << 32) | u64::from(AF_UNIX), SOCK_STREAM.into(), 0), DENY);
            // socketpair: AF_UNIX stream/seqpacket only (Chromium IPC).
            let pair = |family: u64, kind: u64| run(arch.socketpair(), [family, kind, 0, 0, 0, 0]);
            assert_eq!(pair(AF_UNIX.into(), SOCK_STREAM.into()), ALLOW);
            assert_eq!(pair(AF_UNIX.into(), (SOCK_SEQPACKET | SOCK_CLOEXEC).into()), ALLOW);
            assert_eq!(pair(AF_UNIX.into(), 2 /* DGRAM: could sendto a pathname socket */), DENY);
            assert_eq!(pair(AF_INET.into(), SOCK_STREAM.into()), DENY);
            // TCP Fast Open from a send call is refused on all three send forms.
            for (nr, index) in [(arch.sendto(), 3usize), (arch.sendmsg(), 2), (arch.sendmmsg(), 3)] {
                let mut args = [0u64; 6];
                assert_eq!(run(nr, args), ALLOW, "{arch:?} {nr} plain send");
                args[index] = u64::from(MSG_FASTOPEN);
                assert_eq!(run(nr, args), DENY, "{arch:?} {nr} MSG_FASTOPEN");
                args[index] = u64::from(MSG_FASTOPEN | 0x4000 /* MSG_NOSIGNAL */);
                assert_eq!(run(nr, args), DENY);
            }
            // No new session or process group, whatever the arguments.
            for args in [[0u64; 6], [0, 1, 0, 0, 0, 0], [7, 7, 0, 0, 0, 0]] {
                assert_eq!(run(arch.setsid(), args), DENY, "{arch:?} setsid");
                assert_eq!(run(arch.setpgid(), args), DENY, "{arch:?} setpgid {args:?}");
            }
            // Anything else is left to F_L ∩ F_H (this layer only narrows).
            let read = match arch { Arch::X86_64 => 0, Arch::Aarch64 => 63 };
            let connect = match arch { Arch::X86_64 => 42, Arch::Aarch64 => 203 };
            assert_eq!(run(read, [0; 6]), ALLOW);
            assert_eq!(run(connect, [3, 0, 16, 0, 0, 0]), ALLOW);
            // Foreign architecture (and x32) is killed.
            assert_eq!(evaluate(&program, 0x4000_0003, arch.socket(), [0; 6]), KILL);
            if arch == Arch::X86_64 { assert_eq!(run(X32_BIT | 41, [0; 6]), KILL); }
            assert!(program.len() <= MAX_INSTRUCTIONS);
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn hx6fix_browser_filter_native_numbers_and_layout() {
        let arch = Arch::native().unwrap();
        assert_eq!([arch.socket(), arch.socketpair(), arch.sendto(), arch.sendmsg(), arch.sendmmsg(), arch.setsid(), arch.setpgid()],
            [libc::SYS_socket, libc::SYS_socketpair, libc::SYS_sendto, libc::SYS_sendmsg, libc::SYS_sendmmsg,
                libc::SYS_setsid, libc::SYS_setpgid].map(|nr| nr as u32));
        assert_eq!([AF_UNIX, AF_INET, AF_INET6], [libc::AF_UNIX, libc::AF_INET, libc::AF_INET6].map(|v| v as u32));
        assert_eq!([SOCK_STREAM, SOCK_SEQPACKET, SOCK_NONBLOCK, SOCK_CLOEXEC],
            [libc::SOCK_STREAM, libc::SOCK_SEQPACKET, libc::SOCK_NONBLOCK, libc::SOCK_CLOEXEC].map(|v| v as u32));
        assert_eq!(MSG_FASTOPEN, libc::MSG_FASTOPEN as u32);
        assert_eq!(std::mem::size_of::<Instruction>(), std::mem::size_of::<libc::sock_filter>());
        assert_eq!(std::mem::align_of::<Instruction>(), std::mem::align_of::<libc::sock_filter>());
        let filter = BrowserFilter::prepare().unwrap();
        assert_ne!(filter.raw(), 0);
    }
}
