//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::is_valid_bind_addr;

#[test]
fn accepts_literal_ips() {
    for ok in [
        "127.0.0.1",
        "0.0.0.0",
        "192.168.1.5",
        "::1",
        "::",
        "10.0.0.1",
    ] {
        assert!(is_valid_bind_addr(ok), "should accept {ok:?}");
    }
}

#[test]
fn rejects_hostnames_and_injection_fail_closed() {
    // Anything that isn't a literal IP is rejected: hostnames, blanks,
    // command/argument injection, IP-with-port, CIDR, whitespace.
    for bad in [
        "",
        "  ",
        "localhost",
        "evil.com",
        "gateway.company.com",
        "0.0.0.0; rm -rf /",
        "0.0.0.0 --disable-auth",
        "127.0.0.1:18789",
        "0.0.0.0/0",
        "999.999.999.999",
        "0x7f000001",
    ] {
        assert!(!is_valid_bind_addr(bad), "should reject {bad:?}");
    }
}
