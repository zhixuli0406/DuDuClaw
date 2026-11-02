//! Standard minisign signatures over an exhaustive SHA-256 file manifest.
use std::{collections::BTreeMap, path::{Component, Path}};
use sha2::{Digest, Sha256};
use crate::publishers::{SignatureState, SignatureStatus, TrustStore, validate_publisher_id};
use super::Pack;
pub const MANIFEST: &str = "MANIFEST.sha256";
pub const SIGNATURE: &str = "MANIFEST.sha256.minisig";
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_FILES: usize = 10_000;
fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }
fn safe_path(path: &str) -> bool {
    !path.is_empty() && !path.contains(['\\', '\r', '\n', ':'])
        && path.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
        && Path::new(path).components().all(|c| matches!(c, Component::Normal(_)))
}
fn files(root: &Path) -> Result<BTreeMap<String, String>, String> {
    fn visit(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>, bytes: &mut u64, count: &mut usize, depth: usize) -> Result<(), String> {
        if depth > 64 { return Err("pack directory nesting exceeds 64".into()); }
        let meta = std::fs::symlink_metadata(dir).map_err(|e| e.to_string())?;
        if meta.file_type().is_symlink() || !meta.is_dir() { return Err("pack contains a symlink or special directory".into()); }
        for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if meta.file_type().is_symlink() { return Err("pack contains a symlink".into()); }
            if meta.is_dir() { visit(root, &path, out, bytes, count, depth + 1)?; continue; }
            if !meta.is_file() { return Err("pack contains a special file".into()); }
            *count += 1;
            if *count > MAX_FILES { return Err("pack exceeds file limit".into()); }
            let rel = path.strip_prefix(root).map_err(|e| e.to_string())?.components()
                .map(|c| c.as_os_str().to_str().ok_or("pack path is not UTF-8")).collect::<Result<Vec<_>, _>>()?.join("/");
            if !safe_path(&rel) { return Err(format!("invalid pack path: {rel}")); }
            if rel == MANIFEST || rel == SIGNATURE { continue; }
            *bytes = bytes.checked_add(meta.len()).ok_or("pack too large")?;
            if *bytes > MAX_BYTES { return Err("pack exceeds size limit".into()); }
            let content = std::fs::read(&path).map_err(|e| e.to_string())?;
            if content.len() as u64 != meta.len() { return Err("pack changed while reading".into()); }
            out.insert(rel, hash(&content));
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    visit(root, root, &mut out, &mut 0, &mut 0, 0)?;
    Ok(out)
}
fn identity(pack: &Pack) -> (&str, &str) { (pack.id_mismatch.as_deref().unwrap_or(&pack.id), &pack.version) }
/// Produces only the manifest; private keys and signing stay in the minisign tool.
pub fn write_manifest(root: &Path, pack: &Pack, publisher: &str) -> Result<(), String> {
    validate_publisher_id(publisher)?;
    if !root.join("pack.toml").is_file() { return Err("signed packs require pack.toml".into()); }
    if !pack.publisher.is_empty() && pack.publisher != publisher { return Err("publisher does not match pack.toml".into()); }
    let (name, version) = identity(pack);
    if name.is_empty() || version.is_empty() || name.contains(['\r', '\n']) || version.contains(['\r', '\n']) { return Err("pack name/version is missing or invalid".into()); }
    let files = files(root)?;
    let mut text = format!("name: {name}\nversion: {version}\npublisher: {publisher}\n");
    for (path, hash) in files { text.push_str(&format!("{hash}  {path}\n")); }
    std::fs::write(root.join(MANIFEST), text).map_err(|e| e.to_string())
}
fn verify_inner(root: &Path, pack: &Pack, store: &TrustStore) -> Result<SignatureStatus, String> {
    // Enumerate even unsigned packs: links may never hide unlisted content.
    let actual = files(root)?;
    let manifest_exists = root.join(MANIFEST).try_exists().map_err(|e| e.to_string())?;
    let signature_exists = root.join(SIGNATURE).try_exists().map_err(|e| e.to_string())?;
    if !manifest_exists {
        if signature_exists { return Err("signature exists without MANIFEST.sha256".into()); }
        return store.verify_content((!pack.publisher.is_empty()).then_some(pack.publisher.as_str()), None, &[]);
    }
    if !root.join("pack.toml").is_file() { return Err("signed packs require pack.toml".into()); }
    if std::fs::metadata(root.join(MANIFEST)).map_err(|e| e.to_string())?.len() > 2 * 1024 * 1024 { return Err("manifest too large".into()); }
    let text = std::fs::read_to_string(root.join(MANIFEST)).map_err(|e| e.to_string())?;
    let mut lines = text.lines();
    let mut header = |prefix: &str| -> Result<String, String> {
        lines.next().and_then(|s| s.strip_prefix(prefix)).filter(|s| !s.is_empty()).map(str::to_owned).ok_or_else(|| format!("invalid manifest header: {prefix}"))
    };
    let name = header("name: ")?;
    let version = header("version: ")?;
    let publisher = header("publisher: ")?;
    validate_publisher_id(&publisher)?;
    let (expected_name, expected_version) = identity(pack);
    if name != expected_name || version != expected_version { return Err("manifest name/version does not match pack.toml (including replayed versions)".into()); }
    if !pack.publisher.is_empty() && publisher != pack.publisher { return Err("manifest publisher does not match pack.toml".into()); }
    let mut declared = BTreeMap::new();
    let mut last = String::new();
    for line in lines {
        let (digest, path) = line.split_once("  ").ok_or("invalid manifest hash line")?;
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()) || !safe_path(path)
            || path == MANIFEST || path == SIGNATURE || path <= last.as_str() { return Err("invalid, duplicate or unsorted manifest path/hash".into()); }
        last = path.to_owned();
        declared.insert(path.to_owned(), digest.to_owned());
    }
    if actual != declared { return Err("pack has missing, extra or tampered files".into()); }
    let signature = if signature_exists {
        if std::fs::metadata(root.join(SIGNATURE)).map_err(|e| e.to_string())?.len() > 4096 { return Err("signature too large".into()); }
        Some(std::fs::read_to_string(root.join(SIGNATURE)).map_err(|e| e.to_string())?)
    } else { None };
    store.verify_content(Some(&publisher), signature.as_deref(), text.as_bytes())
}
/// Invalid packs retain a reason for diagnostics and must never be installed.
pub fn verify(root: &Path, pack: &Pack, store: &TrustStore) -> (SignatureStatus, Option<String>) {
    match verify_inner(root, pack, store) {
        Ok(status) => (status, None),
        Err(reason) => (SignatureStatus { state: SignatureState::Invalid, publisher: (!pack.publisher.is_empty()).then(|| pack.publisher.clone()) }, Some(reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publishers::Publisher;
    const KEY: &str = "RWRjwiSIobMFBdZAKNK+fZ+02Xp6J4fzbRRbKGtAxLLzCHjdDm7ZuKV/";
    fn fixture() -> (tempfile::TempDir, Pack, TrustStore) {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("pack.toml", include_str!("fixtures/signed-pack/pack.toml")),
            ("README.md", include_str!("fixtures/signed-pack/README.md")),
            (MANIFEST, include_str!("fixtures/signed-pack/MANIFEST.sha256")),
            (SIGNATURE, include_str!("fixtures/signed-pack/MANIFEST.sha256.minisig")),
        ] { std::fs::write(dir.path().join(name), bytes).unwrap(); }
        let pack = super::super::parse_pack("signed-pack", include_str!("fixtures/signed-pack/pack.toml")).unwrap();
        let store = TrustStore { publishers: BTreeMap::from([("fixture".into(), Publisher { keys: vec![KEY.into()] })]) };
        (dir, pack, store)
    }
    #[test]
    fn pinned_ok_wrong_key_and_signature_removed() {
        let (dir, pack, mut store) = fixture();
        assert_eq!(verify(dir.path(), &pack, &store).0.state, SignatureState::Verified);
        store.publishers.get_mut("fixture").unwrap().keys = vec![crate::publishers::OFFICIAL_PUBLISHER_KEY.into()];
        assert_eq!(verify(dir.path(), &pack, &store).0.state, SignatureState::Invalid);
        std::fs::remove_file(dir.path().join(SIGNATURE)).unwrap();
        assert_eq!(verify(dir.path(), &pack, &store).0.state, SignatureState::Invalid);
    }
    #[test]
    fn extra_and_tampered_files_invalid() {
        let (dir, pack, store) = fixture();
        std::fs::write(dir.path().join("extra"), "unlisted").unwrap();
        assert_eq!(verify(dir.path(), &pack, &store).0.state, SignatureState::Invalid);
        std::fs::remove_file(dir.path().join("extra")).unwrap();
        std::fs::write(dir.path().join("README.md"), "tampered").unwrap();
        assert_eq!(verify(dir.path(), &pack, &store).0.state, SignatureState::Invalid);
    }
    #[test]
    fn name_version_and_old_version_replay_refused() {
        let (dir, mut pack, store) = fixture();
        pack.version = "2.0.0".into();
        assert!(verify(dir.path(), &pack, &store).1.unwrap().contains("name/version"));
        pack.version = "1.0.0".into();
        pack.id = "another-pack".into();
        assert!(verify(dir.path(), &pack, &store).1.unwrap().contains("name/version"));
    }
    #[test]
    fn traversal_absolute_duplicate_unsorted_and_links_refused() {
        let (dir, pack, store) = fixture();
        let header = "name: signed-pack\nversion: 1.0.0\npublisher: fixture\n";
        for path in ["../outside", "/absolute", "C:/absolute", "a/../b", "a\\b"] {
            std::fs::write(dir.path().join(MANIFEST), format!("{header}{}  {path}\n", "a".repeat(64))).unwrap();
            assert_eq!(verify(dir.path(), &pack, &store).0.state, SignatureState::Invalid, "{path}");
        }
        #[cfg(unix)] {
            std::os::unix::fs::symlink("README.md", dir.path().join("link")).unwrap();
            assert!(verify(dir.path(), &pack, &store).1.unwrap().contains("symlink"));
        }
    }
    #[test]
    fn unknown_publisher_unsigned_and_manifest_roundtrip() {
        let (dir, pack, _) = fixture();
        assert_eq!(verify(dir.path(), &pack, &TrustStore::default()).0.state, SignatureState::Unsigned);
        let before = std::fs::read(dir.path().join(MANIFEST)).unwrap();
        write_manifest(dir.path(), &pack, "fixture").unwrap();
        assert_eq!(std::fs::read(dir.path().join(MANIFEST)).unwrap(), before);
    }
    #[test]
    fn stripping_both_sidecars_cannot_downgrade_a_pinned_pack() {
        let (dir, pack, store) = fixture();
        std::fs::remove_file(dir.path().join(MANIFEST)).unwrap();
        std::fs::remove_file(dir.path().join(SIGNATURE)).unwrap();
        assert_eq!(verify(dir.path(), &pack, &store).0.state, SignatureState::Invalid);
    }
}
