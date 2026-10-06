//! The one path hash of computer-use workspace files (review L13).
//!
//! The browser audit (gateway) and the tool-call audit (`tool_calls.jsonl`,
//! MCP server) both record a workspace path only as this hash, so the two
//! logs can be joined. NFC first: the gateway stores and validates paths in
//! NFC, so the same file typed in NFD or NFC hashes the same.

use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;

/// Lowercase hex sha256 of the NFC form of `path` (64 hex digits). Audit
/// rows show the first 16.
pub fn workspace_path_hash(path: &str) -> String {
    let nfc: String = path.nfc().collect();
    format!("{:x}", Sha256::digest(nfc.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::workspace_path_hash;

    #[test]
    fn nfc_and_nfd_spellings_hash_the_same() {
        // "é" precomposed vs "e" + combining acute.
        assert_eq!(
            workspace_path_hash("caf\u{e9}/a.md"),
            workspace_path_hash("cafe\u{301}/a.md")
        );
        assert_ne!(workspace_path_hash("a.md"), workspace_path_hash("b.md"));
        assert_eq!(workspace_path_hash("a").len(), 64);
    }
}
