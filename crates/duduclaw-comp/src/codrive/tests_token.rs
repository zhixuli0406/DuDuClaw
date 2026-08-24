//! `mod.rs`'s own free-function tests (socket-auth token generation and hex
//! encoding), split out here in the A2 round.
//!
//! `codrive/mod.rs` was already past this project's 800-line per-file cap
//! when A2 arrived, and A2 has to add driving-mode transition calls to three
//! of its functions. Moving this block out is the same debt-payment pattern
//! `tests_listener.rs` (WP-CD4a-COMP) and `tests_takeover.rs` (CD-3) already
//! established: tests get their own `tests_<topic>.rs`, code stays put. Every
//! test below moved verbatim; `super::` reaches `mod.rs`'s private
//! `generate_token_bytes`/`hex_encode` because this module is a direct child
//! of `codrive` (Rust's "visible within the defining module's whole subtree"
//! privacy rule, not a special-case export).

use super::{generate_token_bytes, hex_encode};

#[test]
fn generate_token_bytes_returns_32_fresh_random_bytes() {
    let a = generate_token_bytes().expect("failed to read /dev/urandom");
    let b = generate_token_bytes().expect("failed to read /dev/urandom");
    assert_eq!(a.len(), 32);
    assert_ne!(
        a, b,
        "two consecutive reads of /dev/urandom must not collide"
    );
}

#[test]
fn hex_encode_produces_lowercase_hex_of_expected_length() {
    let bytes = [0u8, 1, 255, 16];
    assert_eq!(hex_encode(&bytes), "0001ff10");
    let token = hex_encode(&[7u8; 32]);
    assert_eq!(token.len(), 64);
    assert!(token
        .chars()
        .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
}
