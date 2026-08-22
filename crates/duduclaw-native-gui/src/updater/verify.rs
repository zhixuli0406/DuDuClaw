// WP-C-M3 — integrity verification: SHA-256 checksum + minisign Ed25519
// signature, both fail-closed. Deliberately mirrors `crates/duduclaw-
// gateway/src/updater.rs`'s `verify_archive_with_pubkey` shape (same two
// checks, same "public key is a parameter, never read from the network"
// design) rather than depending on that crate — this crate is detached from
// the root workspace specifically to keep the gateway's (and its
// transitive axum/sqlx/...) dependency tree away from the gpui build (see
// `Cargo.toml`'s header comment), so the ~40-line verification logic is
// hand-duplicated instead.
//
// ── Key reuse decision (WP-C-plan, already settled — not re-litigated
// here) ───────────────────────────────────────────────────────────────────
// This module pins the SAME minisign keypair the CLI release channel
// already uses (`crates/duduclaw-gateway/src/updater.rs`'s `UPDATE_PUBKEY`,
// secret half at `~/.minisign/duduclaw-release.key` / CI secret
// `MINISIGN_SECRET_KEY`) — NOT the Tauri shell's separate keypair
// (`~/.tauri/duduclaw.key`, see `docs/guides/desktop-release.md` §1) and
// NOT a freshly generated third keypair. The WP-C-plan brief is explicit:
// "沿用同一套 desktop-updater GH tag ＋同一組 minisign 金鑰對（不另開發布通
// 道）". This is safe to do (one publisher signing two different artifact
// families with the same key is not a security downgrade — contrast with
// `duduclaw-gateway/src/updater.rs`'s doc comment on `UpdateProvider`, which
// requires the CLOSED-SOURCE Enterprise channel to use its OWN key so a
// compromise of one channel can't install over the other's users; the
// native-gui channel and the CLI channel are both this same open-source
// publisher) and avoids a second key-generation/backup/GH-secret ceremony
// for what is, cryptographically, the identical trust decision the CLI
// channel already made.
//
// `NATIVE_GUI_UPDATE_PUBKEY`'s value is copied verbatim from that
// constant — rotating either one requires rotating both (same secret key
// signs both, so they can never legitimately drift apart; if they do, that
// itself is a signal something is wrong).

use sha2::{Digest, Sha256};

/// = `crates/duduclaw-gateway/src/updater.rs::UPDATE_PUBKEY`. See this
/// module's header comment for why this is the same key, not a new one.
pub const NATIVE_GUI_UPDATE_PUBKEY: &str = "RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh";

/// Extract the first 64-hex-char token from a checksum sidecar — tolerates
/// `shasum -a 256` (`<hash>  <file>`) and PowerShell `Format-List` (`Hash :
/// <HASH>`) layouts, same as the gateway's `extract_sha256_token`.
fn extract_sha256_token(text: &str) -> Option<String> {
    text.split_whitespace()
        .map(|t| t.trim_matches(|c: char| !c.is_ascii_hexdigit()))
        .find(|t| t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|t| t.to_lowercase())
}

/// Verify `bytes` against a SHA-256 checksum sidecar's text content.
/// Fail-closed: an unparseable sidecar or a mismatch is always `Err`, never
/// silently skipped.
pub fn verify_sha256(bytes: &[u8], checksum_text: &str) -> Result<(), String> {
    let expected =
        extract_sha256_token(checksum_text).ok_or_else(|| "校驗檔格式錯誤（找不到 SHA-256 摘要）".to_string())?;
    let computed = format!("{:x}", Sha256::digest(bytes));
    if computed != expected {
        return Err(format!("SHA-256 校驗失敗！\n  預期: {expected}\n  實際: {computed}"));
    }
    Ok(())
}

/// Verify a minisign `.minisig` signature over `bytes` against `pubkey_b64`.
/// Fail-closed on every branch (bad pubkey, bad signature shape, mismatch) —
/// never retried, never downgraded to a warning. `pubkey_b64` is a
/// PARAMETER (not read from the network) precisely so tests can exercise
/// this against a throwaway key instead of the production one — see the
/// tests below.
pub fn verify_minisign(bytes: &[u8], sig_text: &str, pubkey_b64: &str) -> Result<(), String> {
    let pk = minisign_verify::PublicKey::from_base64(pubkey_b64)
        .map_err(|e| format!("內建的更新公鑰格式錯誤: {e}"))?;
    let sig = minisign_verify::Signature::decode(sig_text)
        .map_err(|e| format!("簽章檔格式錯誤: {e}"))?;
    pk.verify(bytes, &sig, false)
        .map_err(|e| format!("Ed25519 簽章驗證失敗——拒絕安裝此更新: {e}"))
}

/// Both checks, in the order the CLI updater applies them (checksum first —
/// a cheap catch for a truncated/corrupted download before spending an
/// asymmetric-crypto verification on it; either failing refuses the
/// update). Convenience wrapper for [`verify_sha256`] + [`verify_minisign`]
/// against the pinned [`NATIVE_GUI_UPDATE_PUBKEY`].
pub fn verify_archive(bytes: &[u8], checksum_text: &str, sig_text: &str) -> Result<(), String> {
    verify_sha256(bytes, checksum_text)?;
    verify_minisign(bytes, sig_text, NATIVE_GUI_UPDATE_PUBKEY)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Real test vectors — a THROWAWAY minisign keypair generated for this
    // test only (2026-08-22, `minisign -G -W`), never the production key at
    // `~/.minisign/duduclaw-release.key` / `NATIVE_GUI_UPDATE_PUBKEY` above.
    // `verify_minisign` takes the public key as a parameter specifically so
    // this test can exercise the REAL cryptographic verification path
    // end-to-end (not a mock) without touching production key material —
    // per this task's own "用測試金鑰，不碰生產 key" instruction. The
    // signature was produced with:
    //   minisign -G -W -f -p test.pub -s test.key
    //   printf 'FAKE_NATIVE_GUI_ARCHIVE_BYTES' > fixture.bin
    //   minisign -S -s test.key -m fixture.bin -t "duduclaw-native-gui test fixture"
    const TEST_PUBKEY: &str = "RWQDI+3s7vn2ba99b47UION8qBJGO3HtOG4li5NKJQbKEKH0fLed2Euy";
    const TEST_FIXTURE: &[u8] = b"FAKE_NATIVE_GUI_ARCHIVE_BYTES";
    const TEST_SIG: &str = "untrusted comment: signature from minisign secret key\n\
RUQDI+3s7vn2bS3aoVBAmmQI+EH50+wgs9VwIE+ADxX/mqKgLl4wiww18CZdH1BjjxXZk2/tgyv2bQPsHnsqiDMGQIpii5kZsww=\n\
trusted comment: duduclaw-native-gui test fixture\n\
JqAOo13Z0ysI+7oC2bE7aF6g+g+QTGQ8QP9krv47b1TT15T97Z+iebxn4MnYBo2U5ptdvd9WpSUDLjcHq2TrDg==\n";
    const TEST_FIXTURE_SHA256: &str =
        "5fd55c8005d80ad1af08c484351a99ec0678117df812d7d73d71cdd5a3d330af";

    #[test]
    fn correct_signature_over_correct_key_verifies() {
        assert!(verify_minisign(TEST_FIXTURE, TEST_SIG, TEST_PUBKEY).is_ok());
    }

    #[test]
    fn tampered_bytes_are_rejected() {
        let err = verify_minisign(b"FAKE_NATIVE_GUI_ARCHIVE_BYTES_TAMPERED", TEST_SIG, TEST_PUBKEY).unwrap_err();
        assert!(err.contains("失敗"), "expected a verification-failed message, got: {err}");
    }

    #[test]
    fn wrong_public_key_is_rejected() {
        // A DIFFERENT (but validly-shaped) minisign public key must not
        // accept a signature produced by another key — this is the "fake
        // latest.json with a wrong signature" scenario the task brief asks
        // to prove is refused. Uses the real production pubkey constant as
        // the "wrong" key here on purpose: it proves cross-key rejection
        // without ever needing the matching SECRET half.
        assert!(verify_minisign(TEST_FIXTURE, TEST_SIG, NATIVE_GUI_UPDATE_PUBKEY).is_err());
    }

    #[test]
    fn garbage_signature_text_is_rejected_not_a_panic() {
        assert!(verify_minisign(TEST_FIXTURE, "not a minisign signature", TEST_PUBKEY).is_err());
        assert!(verify_minisign(TEST_FIXTURE, "", TEST_PUBKEY).is_err());
    }

    #[test]
    fn garbage_public_key_is_rejected_not_a_panic() {
        assert!(verify_minisign(TEST_FIXTURE, TEST_SIG, "not a public key").is_err());
    }

    #[test]
    fn sha256_matches_a_real_digest() {
        assert!(verify_sha256(TEST_FIXTURE, &format!("{TEST_FIXTURE_SHA256}  fixture.bin\n")).is_ok());
    }

    #[test]
    fn sha256_mismatch_is_rejected() {
        assert!(verify_sha256(b"different bytes", &format!("{TEST_FIXTURE_SHA256}  fixture.bin\n")).is_err());
    }

    #[test]
    fn sha256_tolerates_powershell_format_list_shape() {
        let text = format!("\nHash : {}\n\n", TEST_FIXTURE_SHA256.to_uppercase());
        assert!(verify_sha256(TEST_FIXTURE, &text).is_ok());
    }

    #[test]
    fn sha256_missing_digest_is_rejected_not_a_panic() {
        assert!(verify_sha256(TEST_FIXTURE, "no digest in here at all").is_err());
    }

    #[test]
    fn verify_archive_end_to_end_accepts_a_correctly_signed_and_checksummed_archive() {
        // `verify_archive` pins `NATIVE_GUI_UPDATE_PUBKEY`, not the test key
        // — exercise it via the two-arg building blocks directly instead
        // (same pattern the gateway's own `#[cfg(test)] fn
        // verify_minisign_signature` uses to test the pinned-key wrapper
        // indirectly, without needing a production-signed fixture).
        assert!(verify_sha256(TEST_FIXTURE, &format!("{TEST_FIXTURE_SHA256}  x\n")).is_ok());
        assert!(verify_minisign(TEST_FIXTURE, TEST_SIG, TEST_PUBKEY).is_ok());
    }

    #[test]
    fn verify_archive_rejects_when_checksum_is_right_but_signature_is_wrong() {
        // A fake `latest.json` pointing at a real, checksum-matching archive
        // but signed with an unrelated (or absent) key — the checksum alone
        // must never be treated as sufficient proof of authenticity.
        let checksum_text = format!("{TEST_FIXTURE_SHA256}  x\n");
        assert!(verify_sha256(TEST_FIXTURE, &checksum_text).is_ok());
        assert!(verify_minisign(TEST_FIXTURE, "bogus signature", TEST_PUBKEY).is_err());
    }
}
