//! WP2.2 R2/R3 — pack registry consumption and publishing.
//!
//! Consumption: `duduclaw pack install registry:<slug>` resolves the slug
//! against the registry index (raw GitHub by default, `DUDUCLAW_REGISTRY_URL`
//! to override), then enforces the registry's trust rules CLIENT-side — the
//! index being compromised must not be enough to ship a tampered archive:
//!   - sha256 of the downloaded archive must match the entry (always), and
//!   - code-lane packs (hooks/skills present) must carry a valid minisign
//!     signature by the publisher's registered key (fail-closed: a code-lane
//!     entry with no signature, no key, or a bad signature refuses install).
//!
//! Publishing: `duduclaw expert publish <dir>` packs, hashes, and emits the
//! ready-to-PR `index/<slug>.json` — the human only fills in the archive URL
//! after uploading the zip to their release.

use serde::Deserialize;
use sha2::{Digest, Sha256};

use duduclaw_core::error::{DuDuClawError, Result};

pub const DEFAULT_REGISTRY_BASE: &str =
    "https://raw.githubusercontent.com/zhixuli0406/duduclaw-registry/main";

/// Entry metadata (subset of the registry schema that install needs).
#[derive(Debug, Deserialize)]
pub struct RegistryEntry {
    pub slug: String,
    pub publisher: String,
    pub archive_url: String,
    pub sha256: String,
    #[serde(default)]
    pub minisig_url: Option<String>,
    #[serde(default)]
    pub contains: Contains,
}

#[derive(Debug, Default, Deserialize)]
pub struct Contains {
    #[serde(default)]
    pub hooks: bool,
    #[serde(default)]
    pub skills: bool,
    #[serde(default)]
    pub agents: u32,
    #[serde(default)]
    pub wiki: bool,
}

pub fn registry_base() -> String {
    std::env::var("DUDUCLAW_REGISTRY_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY_BASE.to_string())
}

/// Mirror of the registry slug rule (`^[a-z0-9][a-z0-9-]{1,63}$`) — checked
/// client-side before the slug is interpolated into a URL.
pub fn slug_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=64).contains(&b.len())
        && b[0].is_ascii_lowercase() | b[0].is_ascii_digit()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

pub fn code_lane(e: &RegistryEntry) -> bool {
    e.contains.hooks || e.contains.skills
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// Extract the base64 key line from a minisign `.pub` file (comment line +
/// base64 line). Fails closed on anything that doesn't look like one.
#[cfg(test)]
pub fn pubkey_base64_from_file(text: &str) -> Result<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with("untrusted comment:"))
        .map(str::to_string)
        .ok_or_else(|| cfg("registry publisher key 檔格式不對（找不到 base64 行）"))
}

/// Verify `data` against a minisign signature + publisher pub file content.
#[cfg(test)]
pub fn verify_minisig(data: &[u8], sig_text: &str, pub_file: &str) -> Result<()> {
    let b64 = pubkey_base64_from_file(pub_file)?;
    duduclaw_core::publishers::verify_minisig(data, sig_text, &b64).map_err(cfg)
}

/// Enforce registry origin independently of the transport and the registry host.
pub(crate) fn verify_registry_signature(
    entry: &RegistryEntry, data: &[u8], signature: Option<&str>, store: &duduclaw_core::publishers::TrustStore,
) -> Result<duduclaw_core::publishers::SignatureStatus> {
    if code_lane(entry) && !store.is_pinned(&entry.publisher) {
        return Err(cfg(format!("publisher {} 尚未釘選；先從獨立管道核對公鑰，再使用 duduclaw publishers add <id> <pubkey>；拒絕 code lane 安裝", entry.publisher)));
    }
    if code_lane(entry) && signature.is_none() { return Err(cfg("code lane 缺少 minisig——拒絕安裝")); }
    if let Some(sig) = signature {
        minisign_verify::Signature::decode(sig).map_err(|e| cfg(format!("minisig 簽章格式無效：{e}")))?;
    }
    store.verify_content(Some(&entry.publisher), signature, data).map_err(cfg)
}

/// Index flags are untrusted. Inspect the extracted payload before accepting its lane.
pub(crate) fn extracted_code_lane(root: &std::path::Path) -> Result<bool> {
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0usize;
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).map_err(|e| cfg(e.to_string()))? {
            let entry = entry.map_err(|e| cfg(e.to_string()))?;
            count += 1;
            if count > 10_000 { return Err(cfg("registry pack inventory exceeds limit")); }
            let meta = entry.file_type().map_err(|e| cfg(e.to_string()))?;
            if meta.is_symlink() { return Err(cfg("registry pack contains a symlink")); }
            let name = entry.file_name();
            let name = name.to_string_lossy().to_ascii_lowercase();
            if (meta.is_dir() && matches!(name.as_str(), "skills" | "hooks" | "commands"))
                || (meta.is_file() && matches!(name.as_str(), "skill.md" | "hooks.json")) { return Ok(true); }
            if meta.is_dir() { pending.push(entry.path()); }
        }
    }
    Ok(false)
}

/// WP2.5 — BRAT-style side door: `github:user/repo[@branch]` → the repo's
/// source-archive zip URL. Unregistered, unreviewed — the caller prints the
/// self-responsibility warning; the normal install pipeline (zip fence,
/// content scan, hook quarantine) still applies in full.
pub fn github_archive_url(spec: &str) -> Result<(String, String)> {
    let (repo_part, branch) = match spec.split_once('@') {
        Some((r, b)) if !b.trim().is_empty() => (r.trim(), b.trim()),
        // Trailing '@' (or none) ⇒ default branch.
        Some((r, _)) => (r.trim(), "main"),
        None => (spec.trim(), "main"),
    };
    let ok_seg = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            && s != "."
            && s != ".."
    };
    let Some((user, repo)) = repo_part.split_once('/') else {
        return Err(cfg(format!(
            "github: 來源格式應為 user/repo[@branch]：{spec}"
        )));
    };
    if !ok_seg(user)
        || !ok_seg(repo)
        || !branch
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        return Err(cfg(format!("github: 來源含不合法字元：{spec}")));
    }
    Ok((
        format!("https://github.com/{user}/{repo}/archive/refs/heads/{branch}.zip"),
        format!("{user}/{repo}@{branch}"),
    ))
}

/// WP2.5 — machine-checkable quality tier for a pack directory. Advisory
/// (never blocks): Gold ≥5, Silver ≥3, Bronze otherwise. Each check is a
/// yes/no the publisher can act on; `reasons` lists what's missing.
pub fn compute_score(dir: &std::path::Path) -> (&'static str, Vec<String>) {
    let mut points = 0u32;
    let mut missing: Vec<String> = Vec::new();
    let mut check = |ok: bool, gain: &str, lack: &str| {
        if ok {
            points += 1;
            let _ = gain;
        } else {
            missing.push(lack.to_string());
        }
    };

    let manifest_ok = dir.join("expert.toml").is_file();
    check(manifest_ok, "manifest", "expert.toml 缺失");

    // Any agent soul with an explicit boundary section (「邊界」/ Boundaries).
    let mut has_boundary = false;
    if let Ok(entries) = std::fs::read_dir(dir.join("agents")) {
        for e in entries.flatten() {
            if let Ok(soul) = std::fs::read_to_string(e.path().join("soul.md")) {
                if soul.contains("邊界") || soul.to_lowercase().contains("boundar") {
                    has_boundary = true;
                    break;
                }
            }
        }
    }
    check(
        has_boundary,
        "boundary",
        "SOUL 缺「邊界」段（安裝者最看重的一段）",
    );

    let raw = std::fs::read_to_string(dir.join("expert.toml")).unwrap_or_default();
    check(
        raw.contains("[expert.requires]") || raw.contains("requires]"),
        "requires",
        "未宣告 requires（env/bins 前置條件）",
    );
    check(
        dir.join("evals").is_dir(),
        "evals",
        "未附 eval 案例（附了直接升一級）",
    );
    check(
        dir.join("README.md").is_file() || dir.join("CHANGELOG.md").is_file(),
        "docs",
        "缺 README/CHANGELOG（版本沿革）",
    );
    check(
        dir.join("wiki").is_dir(),
        "wiki",
        "無 wiki 知識頁（SOP/參考資料）",
    );

    let tier = if points >= 5 {
        "Gold"
    } else if points >= 3 {
        "Silver"
    } else {
        "Bronze"
    };
    (tier, missing)
}

fn cfg(msg: impl Into<String>) -> DuDuClawError {
    DuDuClawError::Config(msg.into())
}

/// Fetch a small registry asset with a hard size cap (index JSON / sig / key
/// are all tiny; anything bigger is suspicious).
pub async fn fetch_small(url: &str, cap: usize, what: &str) -> Result<Vec<u8>> {
    if !url.starts_with("https://") {
        return Err(cfg(format!("{what} 必須是 https：{url}")));
    }
    let resp = reqwest::get(url)
        .await
        .map_err(|e| cfg(format!("{what} 下載失敗：{e}")))?;
    if !resp.status().is_success() {
        return Err(cfg(format!("{what} 下載失敗 HTTP {}", resp.status())));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| cfg(format!("{what} 讀取失敗：{e}")))?;
    if bytes.len() > cap {
        return Err(cfg(format!("{what} 過大（{} > {cap} bytes）", bytes.len())));
    }
    Ok(bytes.to_vec())
}

/// Resolve `registry:<slug>` → verified archive bytes + the entry.
/// Every failure is a hard refusal (fail-closed).
pub async fn fetch_verified_archive(slug: &str) -> Result<(RegistryEntry, Vec<u8>, Option<String>)> {
    if !slug_ok(slug) {
        return Err(cfg(format!("registry slug 不合法：{slug}")));
    }
    let base = registry_base();
    let entry_bytes = fetch_small(
        &format!("{base}/index/{slug}.json"),
        64 * 1024,
        "registry entry",
    )
    .await?;
    let entry: RegistryEntry = serde_json::from_slice(&entry_bytes)
        .map_err(|e| cfg(format!("registry entry 解析失敗：{e}")))?;
    if entry.slug != slug {
        return Err(cfg(format!(
            "registry entry slug 不符（{} ≠ {slug}）",
            entry.slug
        )));
    }
    if !entry.archive_url.starts_with("https://") {
        return Err(cfg("registry entry 的 archive_url 必須是 https".to_string()));
    }

    // Archive download (reuses the same size posture as the pack installer's
    // MAX_UNPACK_BYTES; the zip fence re-checks on extraction).
    let archive = fetch_small(
        &entry.archive_url,
        super::safe_zip::MAX_UNPACK_BYTES as usize,
        "pack 檔案",
    )
    .await?;

    // 1) Integrity: sha256 always.
    let actual = sha256_hex(&archive);
    if !actual.eq_ignore_ascii_case(entry.sha256.trim()) {
        return Err(cfg(format!(
            "pack sha256 不符——拒絕安裝（index 記載 {} / 實際 {actual}）",
            entry.sha256
        )));
    }

    // Publisher keys are local pins, never fetched from the registry host.
    let store = duduclaw_core::publishers::TrustStore::load(&crate::duduclaw_home());
    let pinned = store.is_pinned(&entry.publisher);
    if code_lane(&entry) && !pinned {
        return Err(cfg(format!("publisher {} 尚未釘選；先從獨立管道核對公鑰，再使用 duduclaw publishers add <id> <pubkey>；拒絕 code lane 安裝", entry.publisher)));
    }
    if entry.minisig_url.is_none() && (code_lane(&entry) || pinned) {
        return Err(cfg("此來源缺少必要 minisig_url——拒絕安裝"));
    }
    let sig_text = if let Some(sig_url) = entry.minisig_url.as_deref() {
        Some(String::from_utf8(fetch_small(sig_url, 4 * 1024, "minisig 簽章").await?)
            .map_err(|_| cfg("minisig 簽章不是合法 UTF-8"))?)
    } else { None };
    let status = verify_registry_signature(&entry, &archive, sig_text.as_deref(), &store)?;
    println!("🔏 registry 來源：{:?}（publisher: {}）", status.state, entry.publisher);

    Ok((entry, archive, sig_text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actual_payload_cannot_hide_code_by_clearing_index_flags() {
        let root = tempfile::tempdir().unwrap();
        assert!(!extracted_code_lane(root.path()).unwrap());
        for dirname in ["skills", "hooks", "commands", "SKILLS"] {
            let dir = root.path().join(dirname);
            std::fs::create_dir(&dir).unwrap();
            assert!(extracted_code_lane(root.path()).unwrap(), "{dirname}");
            std::fs::remove_dir(dir).unwrap();
        }
        for filename in ["SKILL.md", "hooks.json"] {
            std::fs::write(root.path().join(filename), "payload").unwrap();
            assert!(extracted_code_lane(root.path()).unwrap(), "{filename}");
            std::fs::remove_file(root.path().join(filename)).unwrap();
        }
    }

    #[test]
    fn pinned_registry_signature_ok_and_wrong_key_refused() {
        use duduclaw_core::publishers::{Publisher, SignatureState, TrustStore};
        let data = include_bytes!("../../../duduclaw-core/src/pack/fixtures/signed-pack/MANIFEST.sha256");
        let sig = include_str!("../../../duduclaw-core/src/pack/fixtures/signed-pack/MANIFEST.sha256.minisig");
        let key = "RWRjwiSIobMFBdZAKNK+fZ+02Xp6J4fzbRRbKGtAxLLzCHjdDm7ZuKV/";
        let mut store = TrustStore { publishers: std::collections::BTreeMap::from([("fixture".into(), Publisher { keys: vec![key.into()] })]) };
        let entry: RegistryEntry = serde_json::from_str(r#"{"slug":"x-pack","publisher":"fixture","archive_url":"https://example.test/x.zip","sha256":"aa","contains":{"skills":true}}"#).unwrap();
        assert_eq!(verify_registry_signature(&entry, data, Some(sig), &store).unwrap().state, SignatureState::Verified);
        assert!(verify_minisig(data, sig, key).is_ok());
        store.publishers.get_mut("fixture").unwrap().keys = vec![duduclaw_core::publishers::OFFICIAL_PUBLISHER_KEY.into()];
        assert!(verify_registry_signature(&entry, data, Some(sig), &store).is_err());
    }

    #[test]
    fn registry_never_accepts_an_unpinned_code_lane_or_downgrades_a_pin() {
        let entry: RegistryEntry = serde_json::from_str(r#"{"slug":"x-pack","publisher":"fixture","archive_url":"https://example.test/x.zip","sha256":"aa","contains":{"skills":true}}"#).unwrap();
        let store = duduclaw_core::publishers::TrustStore::default();
        assert!(verify_registry_signature(&entry, b"x", None, &store).is_err());
        let mut entry = entry;
        entry.publisher = "duduclaw".into();
        assert!(verify_registry_signature(&entry, b"x", None, &store).is_err());
        assert!(verify_registry_signature(&entry, b"x", Some("bad"), &store).is_err());
        entry.contains.skills = false;
        assert!(verify_registry_signature(&entry, b"x", None, &store).is_err());
        // A registry-provided key has no argument or transport path here.
        entry.publisher = "unknown".into();
        assert_eq!(verify_registry_signature(&entry, b"x", None, &store).unwrap().state, duduclaw_core::publishers::SignatureState::Unsigned);
    }

    #[test]
    fn slug_rules_mirror_registry() {
        assert!(slug_ok("clinic-team"));
        assert!(slug_ok("a1"));
        assert!(!slug_ok("A-upper"));
        assert!(!slug_ok("x"));
        assert!(!slug_ok("has_underscore"));
        assert!(!slug_ok("../../etc"));
        assert!(!slug_ok(""));
    }

    #[test]
    fn entry_parses_and_code_lane_detects() {
        let e: RegistryEntry = serde_json::from_str(
            r#"{"slug":"x-pack","publisher":"alice","archive_url":"https://x/p.zip",
                "sha256":"aa","contains":{"hooks":true}}"#,
        )
        .unwrap();
        assert!(code_lane(&e));
        let d: RegistryEntry = serde_json::from_str(
            r#"{"slug":"y-pack","publisher":"bob","archive_url":"https://y/p.zip","sha256":"bb"}"#,
        )
        .unwrap();
        assert!(!code_lane(&d));
    }

    #[test]
    fn pubkey_file_parsing_fails_closed() {
        assert!(pubkey_base64_from_file("untrusted comment: k\nRWQBASE64LINE\n").is_ok());
        assert!(pubkey_base64_from_file("untrusted comment: only\n").is_err());
        assert!(pubkey_base64_from_file("").is_err());
    }

    #[test]
    fn github_side_door_url_shapes() {
        let (url, label) = github_archive_url("alice/my-pack").unwrap();
        assert_eq!(
            url,
            "https://github.com/alice/my-pack/archive/refs/heads/main.zip"
        );
        assert_eq!(label, "alice/my-pack@main");
        let (url2, _) = github_archive_url("alice/my-pack@dev").unwrap();
        assert!(url2.ends_with("/refs/heads/dev.zip"));
        assert!(github_archive_url("no-slash").is_err());
        assert!(github_archive_url("../evil/x").is_err());
        assert!(
            github_archive_url("a/b@")
                .map(|(u, _)| u.ends_with("main.zip"))
                .unwrap_or(false)
        );
    }

    #[test]
    fn score_tiers_are_deterministic() {
        let dir = tempfile::tempdir().unwrap();
        // Bare dir ⇒ Bronze with a full missing list.
        let (tier, missing) = compute_score(dir.path());
        assert_eq!(tier, "Bronze");
        assert!(missing.len() >= 4);
        // Manifest + boundary + requires ⇒ Silver.
        std::fs::write(
            dir.path().join("expert.toml"),
            "[expert]
name='x'
[expert.requires]
env=[]
",
        )
        .unwrap();
        std::fs::create_dir_all(dir.path().join("agents/a")).unwrap();
        std::fs::write(
            dir.path().join("agents/a/soul.md"),
            "# A
## 邊界
只是測試
",
        )
        .unwrap();
        let (tier2, _) = compute_score(dir.path());
        assert_eq!(tier2, "Silver");
        // + evals + README + wiki ⇒ Gold.
        std::fs::create_dir_all(dir.path().join("evals")).unwrap();
        std::fs::create_dir_all(dir.path().join("wiki")).unwrap();
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        assert_eq!(compute_score(dir.path()).0, "Gold");
    }

    #[test]
    fn sha256_matches_known_vector() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
