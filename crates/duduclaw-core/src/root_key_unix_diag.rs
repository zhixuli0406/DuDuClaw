//! Length-delimited UNIX_DIAG_NAME validation complements /proc/net/unix.
//! A name can contain a newline followed by a perfectly valid forged proc row;
//! no text-table parser can distinguish that from two sockets. Never fall back
//! to the text table alone if this kernel dump is unavailable or incomplete.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]

const DIAG: u16 = 20; // SOCK_DIAG_BY_FAMILY
const DONE: u16 = 3; // NLMSG_DONE
const SEQUENCE: u32 = 1; // Each probe owns a fresh, unicast netlink socket.

fn invalid() -> String { "sock_diag: incomplete or malformed UNIX name dump".into() }
fn u16_at(bytes: &[u8], at: usize) -> u16 { u16::from_ne_bytes([bytes[at], bytes[at + 1]]) }
fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
fn aligned(length: usize) -> usize { (length + 3) & !3 }

/// Validate every message/attribute, including non-engine names. DONE is the
/// only success terminator. Error, overrun, truncated and interrupted dumps deny.
fn parse_datagram(mut bytes: &[u8]) -> Result<bool, String> {
    while !bytes.is_empty() {
        if bytes.len() < 16 { return Err(invalid()); }
        let length = u32_at(bytes, 0) as usize;
        if length < 16 || length > bytes.len() || aligned(length) > bytes.len()
            || u32_at(bytes, 8) != SEQUENCE || u16_at(bytes, 6) & 0x10 != 0 {
            return Err(invalid());
        }
        let payload = &bytes[16..length];
        match u16_at(bytes, 4) {
            DONE => {
                // A dump DONE carries a signed status; no trailing messages.
                if payload.len() != 4 || u32_at(payload, 0) != 0 || aligned(length) != bytes.len() {
                    return Err(invalid());
                }
                return Ok(true);
            }
            DIAG => {
                // unix_diag_msg: AF_UNIX, type, state, pad, inode, cookie[2].
                if payload.len() < 16 || payload[0] != 1 { return Err(invalid()); }
                let mut attrs = &payload[16..];
                let mut named = false;
                while !attrs.is_empty() {
                    if attrs.len() < 4 { return Err(invalid()); }
                    let size = u16_at(attrs, 0) as usize;
                    if size < 4 || aligned(size) > attrs.len() { return Err(invalid()); }
                    // UNIX_DIAG_NAME is attribute 0; reject flags on a name.
                    let kind = u16_at(attrs, 2);
                    if kind & 0x3fff == 0 {
                        if kind != 0 || named || size == 4 { return Err(invalid()); }
                        named = true;
                        if attrs[4..size].contains(&b'\n') {
                            return Err("sock_diag: socket name contains a newline (ambiguous proc rows)".into());
                        }
                    }
                    attrs = &attrs[aligned(size)..];
                }
            }
            _ => return Err(invalid()), // Includes NLMSG_ERROR / NLMSG_OVERRUN.
        }
        bytes = &bytes[aligned(length)..];
    }
    Ok(false)
}

#[cfg(target_os = "linux")]
pub(super) fn validate_names() -> Result<(), String> {
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let raw = unsafe { libc::socket(libc::AF_NETLINK, libc::SOCK_RAW | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC, libc::NETLINK_SOCK_DIAG) };
    if raw < 0 { return Err("sock_diag: name dump unavailable".into()); }
    // SAFETY: the socket is fresh and owned for the entire request/dump.
    let socket = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut kernel: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    kernel.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    // nlmsghdr (16) + unix_diag_req (24), defined by Linux UAPI unix_diag.h.
    let mut request = [0u8; 40];
    request[0..4].copy_from_slice(&40u32.to_ne_bytes());
    request[4..6].copy_from_slice(&DIAG.to_ne_bytes());
    request[6..8].copy_from_slice(&0x301u16.to_ne_bytes()); // REQUEST | DUMP
    request[8..12].copy_from_slice(&SEQUENCE.to_ne_bytes());
    request[16] = libc::AF_UNIX as u8;
    request[20..24].copy_from_slice(&u32::MAX.to_ne_bytes()); // All socket states.
    request[28..32].copy_from_slice(&1u32.to_ne_bytes()); // UDIAG_SHOW_NAME
    request[32..40].fill(0xff); // No cookie filter.
    if unsafe { libc::sendto(socket.as_raw_fd(), request.as_ptr().cast(), request.len(), 0,
        (&kernel as *const libc::sockaddr_nl).cast(), std::mem::size_of_val(&kernel) as libc::socklen_t) } != request.len() as isize {
        return Err("sock_diag: name dump request failed".into());
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
    let mut buffer = vec![0u8; 65536];
    let mut total = 0usize;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() { return Err("sock_diag: name dump timed out".into()); }
        let mut poll = libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        if unsafe { libc::poll(&mut poll, 1, remaining.as_millis().max(1) as i32) } <= 0 {
            return Err("sock_diag: name dump timed out or interrupted".into());
        }
        let mut sender: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        let mut iov = libc::iovec { iov_base: buffer.as_mut_ptr().cast(), iov_len: buffer.len() };
        let mut message: libc::msghdr = unsafe { std::mem::zeroed() };
        message.msg_name = (&mut sender as *mut libc::sockaddr_nl).cast();
        message.msg_namelen = std::mem::size_of_val(&sender) as libc::socklen_t;
        message.msg_iov = &mut iov;
        message.msg_iovlen = 1;
        let received = unsafe { libc::recvmsg(socket.as_raw_fd(), &mut message, libc::MSG_DONTWAIT) };
        if received <= 0 || message.msg_flags & (libc::MSG_TRUNC | libc::MSG_CTRUNC) != 0
            || message.msg_namelen as usize != std::mem::size_of_val(&sender)
            || sender.nl_family != libc::AF_NETLINK as libc::sa_family_t || sender.nl_pid != 0 || sender.nl_groups != 0 {
            return Err(invalid());
        }
        total += received as usize;
        if total > 8 * 1024 * 1024 { return Err("sock_diag: name dump exceeds limit".into()); }
        if parse_datagram(&buffer[..received as usize])? { return Ok(()); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn message(kind: u16, payload: &[u8]) -> Vec<u8> {
        let length = 16 + payload.len();
        let mut bytes = vec![0; aligned(length)];
        bytes[0..4].copy_from_slice(&(length as u32).to_ne_bytes());
        bytes[4..6].copy_from_slice(&kind.to_ne_bytes());
        bytes[8..12].copy_from_slice(&SEQUENCE.to_ne_bytes());
        bytes[16..length].copy_from_slice(payload);
        bytes
    }
    fn named(name: &[u8]) -> Vec<u8> {
        let mut payload = vec![0; 16 + aligned(4 + name.len())];
        payload[0] = 1;
        payload[16..18].copy_from_slice(&((4 + name.len()) as u16).to_ne_bytes());
        payload[20..20 + name.len()].copy_from_slice(name);
        message(DIAG, &payload)
    }
    #[test]
    fn root_key_diag_detects_newline_even_when_continuation_is_a_valid_proc_row() {
        for name in [b"\0docker\n0000: 2 0 00000000 0001 03 42 @unrelated".as_slice(),
            b"\0docker_cli_3e708842d9322cef25983565c19d0866\n0000: 2 0 0 1 03 42 @unrelated",
            b"\0unrelated\n0000: 2 0 0 1 03 42 @also-unrelated"] {
            assert!(parse_datagram(&named(name)).is_err());
        }
        assert_eq!(parse_datagram(&named(b"\0unrelated\xff")), Ok(false));
        assert_eq!(parse_datagram(&named(b"\0docker.sock")), Ok(false));
        assert_eq!(parse_datagram(&message(DONE, &0i32.to_ne_bytes())), Ok(true));
    }
    #[test]
    fn root_key_diag_refuses_malformed_or_incomplete_dumps() {
        for bytes in [message(2, &0i32.to_ne_bytes()), message(DONE, &(-1i32).to_ne_bytes()),
            message(DONE, &[]), message(DIAG, &[1]), vec![0; 15], named(b""),
            [message(DONE, &0i32.to_ne_bytes()), named(b"\0extra")].concat()] {
            assert!(parse_datagram(&bytes).is_err());
        }
        let valid = named(b"\0docker");
        for (at, value) in [(0, 0xff), (6, 0x10), (8, 2), (16, 2), (32, 0xff)] {
            let mut bytes = valid.clone(); bytes[at] = value;
            assert!(parse_datagram(&bytes).is_err(), "{at}");
        }
        for end in 1..valid.len() { assert!(parse_datagram(&valid[..end]).is_err()); }
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn root_key_diag_live_dump_rejects_newline_socket() {
        use std::os::linux::net::SocketAddrExt;
        use std::os::unix::net::{SocketAddr, UnixListener};
        let name = format!("keyrecover-{}\n0000: 2 0 0 1 03 42 @unrelated", std::process::id());
        let address = SocketAddr::from_abstract_name(name.as_bytes()).unwrap();
        let _listener = UnixListener::bind_addr(&address).unwrap();
        assert!(validate_names().unwrap_err().contains("newline"));
    }
}
