//! H12b: protected `skill_hub_install` into the caller's own SKILLS.
//!
//! - The hub and slug are slugs from the exact protected schema; the archive
//!   URL is host-selected ([`archive_url`]), never an argument, and must be
//!   permitted by the call's signed `PublicOrigins` grant (D-H2).
//! - The host fetch is the protected public path (`web_fetch_public_bytes`:
//!   DNS pinned, public addresses only, no redirects, no proxy, 5 MiB).
//! - H13c, layer 1 before any download: the hub's detail JSON
//!   ([`detail_url`], same origin and grant) must name exactly this slug, not
//!   be ambiguous (409: several owners) and carry no suspicious / malicious
//!   moderation verdict ([`source_verdict`]); the verdict is kept in the
//!   approved manifest. E's scan refuses High / Critical risk ([`scan_admits`]).
//! - Archive extraction happens only in the disposable parse worker
//!   (`archive_adapter::extract_pack`: traversal, absolute, symlink and
//!   special entries, case collisions, compression ratio and size refuse).
//!   The host re-checks the worker's untrusted manifest and takes exactly one
//!   `SKILL.md` by byte range.
//! - E scans the exact bytes (`InspectSkillManifest`) before approval and again
//!   under the install locks; publication is no-clobber into
//!   `agents/<own label>/SKILLS/<slug>.md` only.
use std::path::Path;
use duduclaw_core::publishers::TrustStore;
use duduclaw_isolation::{capability::ResourceGrant, worker::ArchiveEntry};
use duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall;
use super::hub_install::{GatedManifest, HubInstallReport};

/// Largest archive the host fetch accepts (the protected fetch caps at 5 MiB).
pub const MAX_ARCHIVE_BYTES: usize = 5 * 1024 * 1024;
/// Entry and expanded-size ceilings for one skill archive (the worker's own
/// ceilings are 1000 entries / 50 MiB / ratio 100; these only narrow them).
pub const MAX_ARCHIVE_ENTRIES: usize = 64;
pub const MAX_EXPANDED_BYTES: u64 = 4 * 1024 * 1024;
/// Same cap as every other protected skill publication.
pub const MAX_SKILL_BYTES: u64 = 64 * 1024;
const SKILL_DOCUMENT: &str = "SKILL.md";

fn slug(value: &str) -> bool {
    !value.is_empty() && value.len() <= 64 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The reviewed archive endpoint of `slug` on `hub`, or `None` when the hub has
/// no archive distribution the protected path supports.
pub fn archive_url(hub: &str, name: &str) -> Option<String> {
    if !slug(name) { return None; }
    match hub {
        duduclaw_agent::skill_hub::HUB_CLAWHUB => Some(format!("https://clawhub.ai/api/v1/download?slug={name}")),
        _ => None,
    }
}

/// The single `SKILL.md` of a worker-expanded archive. The manifest is
/// untrusted worker output: every name is re-checked (relative, no `.`/`..`/
/// empty components, no backslash, colon or control characters), ranges must
/// tile `bytes` exactly, and the document must be at the archive root or in its
/// only top-level folder. Anything else (none, several, deeper) refuses.
pub fn select_skill_document(entries: &[ArchiveEntry], bytes: &[u8]) -> Result<Vec<u8>, String> {
    if entries.is_empty() || entries.len() > MAX_ARCHIVE_ENTRIES || bytes.len() as u64 > MAX_EXPANDED_BYTES {
        return Err("技能封存檔超過隔離安裝上限。".into());
    }
    let mut offset = 0u64;
    let mut found = None;
    let mut top = std::collections::BTreeSet::new();
    for entry in entries {
        let name = entry.name.as_str();
        if name.is_empty() || name.starts_with('/') || name.chars().any(|c| c == '\\' || c == ':' || c.is_control())
            || name.split('/').any(|part| matches!(part, "" | "." | "..")) {
            return Err("技能封存檔含有不安全的路徑。".into());
        }
        if entry.offset != offset || (entry.directory && entry.length != 0) {
            return Err("技能封存檔內容不一致。".into());
        }
        offset = offset.checked_add(entry.length).filter(|end| *end <= bytes.len() as u64)
            .ok_or_else(|| "技能封存檔內容不一致。".to_string())?;
        let depth = name.split('/').count();
        top.insert(name.split('/').next().unwrap_or_default());
        if !entry.directory && name.rsplit('/').next() == Some(SKILL_DOCUMENT) {
            if depth > 2 || found.is_some() {
                return Err("技能封存檔必須在根目錄或唯一的頂層資料夾中只含一個 SKILL.md。".into());
            }
            found = Some((depth, entry.offset, entry.length));
        }
    }
    if offset != bytes.len() as u64 { return Err("技能封存檔內容不一致。".into()); }
    let (depth, start, length) = found.ok_or_else(|| "技能封存檔沒有 SKILL.md。".to_string())?;
    if depth == 2 && top.len() != 1 {
        return Err("技能封存檔必須在根目錄或唯一的頂層資料夾中只含一個 SKILL.md。".into());
    }
    if length == 0 || length > MAX_SKILL_BYTES { return Err("技能內容超過隔離安裝上限。".into()); }
    let start = usize::try_from(start).map_err(|_| "技能封存檔內容不一致。".to_string())?;
    let end = start.checked_add(usize::try_from(length).map_err(|_| "技能封存檔內容不一致。".to_string())?)
        .ok_or_else(|| "技能封存檔內容不一致。".to_string())?;
    bytes.get(start..end).map(<[u8]>::to_vec).ok_or_else(|| "技能封存檔內容不一致。".into())
}

fn subject<'a>(call: &'a AuthorizedToolCall, agent: &str) -> Result<(&'a str, &'a str), String> {
    let refused = || "無法確認這次技能安裝的授權。".to_string();
    if call.name() != "skill_hub_install" || !duduclaw_core::is_valid_agent_id(agent)
        || duduclaw_core::is_reserved_agent_id(agent) {
        return Err(refused());
    }
    duduclaw_mcp_boundary::tool_policy::check_own_skill_job_arguments(call.name(), call.arguments()).map_err(|_| refused())?;
    let hub = call.arguments()["hub"].as_str().ok_or_else(refused)?;
    let name = call.arguments()["skill_name"].as_str().ok_or_else(refused)?;
    Ok((hub, name))
}

fn live(call: &AuthorizedToolCall) -> Result<(), String> {
    duduclaw_mcp_boundary::authority::now_seconds().and_then(|now| call.context().verify_live(now))
        .map_err(|_| "這次執行的授權已失效，已停止安裝。".to_string())
}

/// H13c: E's verdict on the exact bytes, refused like the owned synthesis
/// path (`skill_synthesis_pipeline::owned::publish`): not passed, or a High /
/// Critical risk, never installs.
pub fn scan_admits(passed: bool, risk: &str) -> Result<(), String> {
    if !passed || matches!(risk, "High" | "Critical") { return Err("技能未通過隔離安全檢查。".into()); }
    Ok(())
}

async fn inspect(content: &str, name: &str) -> Result<(String, usize), String> {
    let (bytes, slug) = (content.as_bytes().to_vec(), name.to_owned());
    let (passed, risk, findings) = tokio::task::spawn_blocking(move ||
        duduclaw_parse_worker::adapters::inspect_skill_manifest(&bytes, &slug)).await
        .map_err(|_| "隔離技能檢查失敗。".to_string())?.map_err(|_| "隔離技能檢查失敗。".to_string())?;
    scan_admits(passed, &risk)?;
    Ok((risk, findings))
}

/// Largest hub detail response the protected path reads (it carries the
/// skill text; the protected fetch itself caps at 5 MiB).
pub const MAX_DETAIL_BYTES: usize = 1024 * 1024;

/// H13c: the hub's detail (moderation) endpoint of `name`, on the same
/// host-selected origin as [`archive_url`]; `None` exactly when that is `None`.
pub fn detail_url(hub: &str, name: &str) -> Option<String> {
    if !slug(name) { return None; }
    match hub {
        duduclaw_agent::skill_hub::HUB_CLAWHUB => Some(format!("https://clawhub.ai/api/v1/skills/{name}")),
        _ => None,
    }
}

/// H13c: the layer-1 source verdict from the hub detail response, with the
/// unprotected path's rules (`skill_hub::ClawHubHub::fetch_manifest`,
/// `hub_install::gate_hub_manifest`). `response` is the body of a 2xx reply
/// or the HTTP status of any other (0: no reply). Fail closed: an ambiguous
/// slug (409, several owners), a missing skill, an unreadable or unexpected
/// body, a detail for another slug, or a suspicious / malicious verdict
/// refuses. Returns the normalized verdict (`clean`, `unknown` or `None`).
pub fn source_verdict(response: Result<&[u8], u16>, name: &str) -> Result<Option<String>, String> {
    let body = match response {
        Ok(body) => body,
        Err(409) => return Err("技能來源有多位發布者使用這個技能名稱，隔離安裝無法確認來源，已拒絕安裝。".into()),
        Err(404) => return Err("技能來源找不到這個技能。".into()),
        Err(_) => return Err("無法取得技能來源的審核資訊，已拒絕安裝。".into()),
    };
    let unreadable = || "技能來源的審核資訊無法辨識，已拒絕安裝。".to_string();
    if body.len() > MAX_DETAIL_BYTES { return Err(unreadable()); }
    let body: serde_json::Value = serde_json::from_slice(body).map_err(|_| unreadable())?;
    let skill = body.get("skill").filter(|skill| skill.is_object()).ok_or_else(unreadable)?;
    if skill.get("slug").is_some_and(|slug| slug.as_str() != Some(name)) { return Err(unreadable()); }
    // Detail-level verdict: the isSuspicious flag (anything but an explicit
    // `false` counts as suspicious), else the moderation status / verdict.
    let raw = match skill.get("isSuspicious") {
        None | Some(serde_json::Value::Null) | Some(serde_json::Value::Bool(false)) => body["moderation"]["status"]
            .as_str().or_else(|| body["moderation"]["verdict"].as_str()).map(str::to_owned),
        Some(_) => Some("suspicious".to_owned()),
    };
    let verdict = duduclaw_agent::skill_hub::normalize_verdict(raw.as_deref());
    if duduclaw_agent::skill_hub::verdict_is_blocking(verdict.as_deref()) {
        return Err("技能來源將這個技能標記為可疑或惡意，已拒絕安裝。".into());
    }
    Ok(verdict)
}

/// One protected public fetch of a host-selected `url` under the call's
/// `PublicOrigins` grant (DNS pinned, public addresses only, no redirects).
async fn fetch_public(call: &AuthorizedToolCall, grant: &ResourceGrant, url: &str) -> Result<Result<Vec<u8>, u16>, String> {
    duduclaw_mcp_boundary::resources::permitted_public_origin(grant, url)
        .map_err(|_| "受保護的公開網路授權不包含這個技能來源。".to_string())?;
    if !crate::web_fetch::admit_protected_tool_fetch() { return Err("技能來源請求過於頻繁。".into()); }
    live(call)?;
    Ok(match crate::web_fetch::web_fetch_public_bytes(url).await {
        Ok((_, bytes)) => Ok(bytes),
        Err(crate::web_fetch::FetchError::HttpError(status, _)) => Err(status),
        Err(_) => Err(0),
    })
}

/// Phase 1 (no writes): fetch the hub archive on the host, expand it in E,
/// select and scan its `SKILL.md`. `agent` is the capability-bound employee.
pub async fn fetch_owned(home: &Path, call: &AuthorizedToolCall, agent: &str) -> Result<GatedManifest, String> {
    let (hub, name) = subject(call, agent)?;
    if !duduclaw_agent::skill_hub::HubRegistry::from_home(home).ids().contains(&hub) {
        return Err("未設定的技能來源，已拒絕安裝。".into());
    }
    let unsupported = || "隔離安裝不支援這個技能來源的封存檔。".to_string();
    let (url, detail) = (archive_url(hub, name).ok_or_else(unsupported)?, detail_url(hub, name).ok_or_else(unsupported)?);
    let grant = call.resources().iter().find(|grant| matches!(grant, ResourceGrant::PublicOrigins { .. }))
        .ok_or_else(|| "這次執行沒有受保護的公開網路授權。".to_string())?;
    // H13c: layer 1 first — the hub's own moderation verdict and the
    // ambiguous-slug refusal, from the same host-selected origin and grant,
    // before any archive is downloaded.
    let response = fetch_public(call, grant, &detail).await?;
    let verdict = source_verdict(response.as_deref().map_err(|status| *status), name)?;
    let archive = fetch_public(call, grant, &url).await?.map_err(|_| "無法取得技能封存檔。".to_string())?;
    if archive.is_empty() || archive.len() > MAX_ARCHIVE_BYTES { return Err("技能封存檔超過隔離安裝上限。".into()); }
    let pack = tokio::task::spawn_blocking(move ||
        duduclaw_parse_worker::archive_adapter::extract_pack(&mut std::io::Cursor::new(archive))).await
        .map_err(|_| "隔離服務無法展開技能封存檔。".to_string())?
        .map_err(|_| "技能封存檔未通過隔離檢查（路徑、連結、大小或壓縮比）。".to_string())?;
    let document = select_skill_document(&pack.entries, &pack.bytes)?;
    let content = String::from_utf8(document).map_err(|_| "技能內容不是有效的 UTF-8 文字。".to_string())?;
    let (risk_level, findings) = inspect(&content, name).await?;
    let signature_status = TrustStore::load(home)?.verify_content(None, None, content.as_bytes())?;
    duduclaw_agent::skill_hub::check_signature_history(home, hub, name, &signature_status)?;
    Ok(GatedManifest { hub: hub.into(), skill_name: name.into(), manifest_name: name.into(), content,
        publisher: None, signature: None, signature_status, risk_level, findings,
        trust_floor: duduclaw_agent::trust_tier::TrustTier::Active, source_verdict: verdict })
}

/// Phase 2 (after the existing caller approval): rescan the exact approved
/// bytes in E and publish them no-clobber into the caller's own SKILLS, under
/// the same trust-store → signature-history lock order as `install_gated`.
pub async fn install_owned(home: &Path, call: &AuthorizedToolCall, agent: &str, gated: &GatedManifest)
    -> Result<HubInstallReport, String> {
    let (hub, name) = subject(call, agent)?;
    if gated.hub != hub || gated.skill_name != name || gated.manifest_name != name
        || gated.publisher.is_some() || gated.signature.is_some()
        || duduclaw_agent::skill_hub::verdict_is_blocking(gated.source_verdict.as_deref()) {
        return Err("隔離技能來源不受支援。".into());
    }
    let (risk_level, findings) = inspect(&gated.content, name).await?;
    let authority = duduclaw_mcp_boundary::io_authority::scope(call.context().clone(),
        async { duduclaw_mcp_boundary::io_authority::capture() }).await;
    let (home_dir, agent_id, approved, context) = (home.to_path_buf(), agent.to_owned(), gated.clone(), call.context().clone());
    let signature = tokio::task::spawn_blocking(move || {
        let trust = home_dir.join(duduclaw_core::publishers::TRUST_STORE_FILE);
        let history = home_dir.join(duduclaw_agent::skill_hub::SKILL_SIGNATURE_HISTORY_FILE);
        duduclaw_core::with_file_lock(&trust, || duduclaw_core::with_file_lock(&history, || {
            let result = (|| -> Result<_, String> {
                let signature = TrustStore::load(&home_dir)?.verify_content(None, None, approved.content.as_bytes())?;
                if signature != approved.signature_status { return Err("技能簽署者信任已變更，請重新送審。".into()); }
                duduclaw_agent::skill_hub::check_signature_history(&home_dir, &approved.hub, &approved.skill_name, &signature)?;
                duduclaw_mcp_boundary::authority::now_seconds().and_then(|now| context.verify_live(now))
                    .map_err(|_| "這次執行的授權已失效，已停止安裝。".to_string())?;
                authority.run(|| crate::isolation_worker_plans::write_host_file(&home_dir, &["agents", agent_id.as_str(), "SKILLS"],
                    &format!("{}.md", approved.skill_name), approved.content.as_bytes()))
                    .map_err(|_| "技能發布尚未確認；既有技能不會覆寫。".to_string())?;
                Ok(signature)
            })();
            result.map_err(std::io::Error::other)
        })).map_err(|error| error.to_string())
    }).await.map_err(|_| "技能安裝工作失敗。".to_string())??;
    let scope = format!("agent:{agent}");
    let store = crate::custom_skills::CustomSkillStore::open(home)?;
    store.curation_upsert_seen(name, &scope, &chrono::Utc::now().to_rfc3339()).await?;
    let digest = duduclaw_agent::skill_hub::sha256_hex(&gated.content);
    store.record_hub_signature(name, &scope, &signature, &digest).await?;
    Ok(HubInstallReport { hub: hub.into(), skill_name: name.into(), scope: agent.into(), risk_level, findings,
        signature_status: signature, content_sha256: digest })
}

#[cfg(test)]
#[path = "hub_install_owned_tests.rs"]
mod tests;
