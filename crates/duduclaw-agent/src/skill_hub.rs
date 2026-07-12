//! Skill hub abstraction (G5) — multiple skill registries behind one trait.
//!
//! Each hub is a source of installable/discoverable skills. The existing
//! GitHub Search indexer ([`crate::skill_registry::SkillRegistry`]) is the
//! first implementation; additional hubs were verified FIRST-HAND on
//! 2026-07-11 before being wired in:
//!
//! | hub id      | endpoint                                             | status |
//! |-------------|------------------------------------------------------|--------|
//! | `github`    | GitHub Search API (existing indexer, unchanged)      | VERIFIED |
//! | `clawhub`   | `GET https://clawhub.ai/api/v1/skills` (+ `/:slug`)  | VERIFIED — 200 JSON unauthenticated |
//! | `lobehub`   | `GET https://chat-plugins.lobehub.com/index.json`    | VERIFIED — 200 JSON unauthenticated |
//! | `skills-sh` | `https://skills.sh/api/v1/*`                         | UNVERIFIED — requires a Vercel OIDC bearer token (unauthenticated calls return 401 `authentication_required`); stub only, excluded from defaults |
//!
//! Design rules:
//! - **Per-hub 24h cache** at `<home>/skill_hub_cache/<hub>.json`, reusing the
//!   [`SkillIndex`] shape and the same `CACHE_MAX_AGE_SECS` freshness contract
//!   as the GitHub index (which keeps its historical `<home>/skill_index.json`
//!   path — behavior for existing `SkillRegistry` callers is unchanged).
//! - **Aggregation preserves the existing weighting**: every hub's hits are
//!   scored with the same `score_match` the GitHub index search uses, then
//!   merged (dedupe by name, higher score wins, hub declaration order breaks
//!   ties in favor of earlier hubs — `github` first).
//! - **Fail-honest**: a hub that errors contributes an
//!   `[unreachable: <hub>: <error>]` entry instead of silently vanishing.
//! - Hub selection uses **exact id equality** — never substring matching.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::skill_registry::{
    CACHE_MAX_AGE_SECS, SkillIndex, SkillIndexEntry, SkillRegistry, score_match,
};

// ── Hub ids ─────────────────────────────────────────────────

pub const HUB_GITHUB: &str = "github";
pub const HUB_CLAWHUB: &str = "clawhub";
pub const HUB_LOBEHUB: &str = "lobehub";
pub const HUB_SKILLS_SH: &str = "skills-sh";

/// All hub ids this build knows how to construct.
pub const KNOWN_HUB_IDS: &[&str] = &[HUB_GITHUB, HUB_CLAWHUB, HUB_LOBEHUB, HUB_SKILLS_SH];

/// Hubs enabled by default: only first-hand-verified, no-auth sources.
/// `skills-sh` is deliberately excluded (auth-gated API — see module doc).
pub const DEFAULT_HUB_IDS: &[&str] = &[HUB_GITHUB, HUB_CLAWHUB, HUB_LOBEHUB];

/// Boxed future so [`SkillHub`] stays dyn-compatible.
pub type HubFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

// ── Manifest ────────────────────────────────────────────────

/// What a hub can tell us about one concrete skill, sufficient for the
/// gateway's scan-gated install path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubManifest {
    /// Hub the manifest came from.
    pub hub: String,
    /// Machine skill name / slug on that hub.
    pub name: String,
    /// Full skill content (SKILL.md or manifest JSON) when the hub serves it.
    /// `None` means the hub is discovery-only for this skill — the install
    /// gate must then DENY (fail-closed), never guess.
    pub content: Option<String>,
    /// Human-facing URL for the skill.
    pub url: String,
}

// ── Trait ───────────────────────────────────────────────────

/// A skill registry source. Implementations must be side-effect-free except
/// for their own cache files under `<home>/skill_hub_cache/`.
pub trait SkillHub: Send + Sync {
    /// Stable hub id (exact-match key — see module rules).
    fn id(&self) -> &str;

    /// Whether this hub's endpoint was verified first-hand as consumable.
    /// Unverified hubs must not be part of [`HubRegistry::default_hubs`].
    fn verified(&self) -> bool;

    /// Weighted search over this hub's (cached) index.
    fn search<'a>(
        &'a self,
        home_dir: &'a Path,
        query: &'a str,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>>;

    /// List (up to `limit`) entries from this hub's (cached) index.
    fn list<'a>(
        &'a self,
        home_dir: &'a Path,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>>;

    /// Fetch the installable manifest for one skill. `Ok(None)` = not found.
    fn fetch_manifest<'a>(
        &'a self,
        home_dir: &'a Path,
        name: &'a str,
    ) -> HubFuture<'a, Result<Option<HubManifest>, String>>;
}

// ── Shared cache helpers ────────────────────────────────────

fn cache_dir(home_dir: &Path) -> PathBuf {
    home_dir.join("skill_hub_cache")
}

fn cache_path(home_dir: &Path, hub_id: &str) -> PathBuf {
    cache_dir(home_dir).join(format!("{hub_id}.json"))
}

fn load_cache(home_dir: &Path, hub_id: &str) -> Option<SkillIndex> {
    let content = std::fs::read_to_string(cache_path(home_dir, hub_id)).ok()?;
    serde_json::from_str(&content).ok()
}

/// True when the cached index is younger than the 24h freshness window.
pub fn cache_is_fresh(index: &SkillIndex, now: chrono::DateTime<Utc>) -> bool {
    if index.skills.is_empty() {
        return false;
    }
    match chrono::DateTime::parse_from_rfc3339(&index.updated_at) {
        Ok(dt) => {
            now.signed_duration_since(dt.with_timezone(&Utc))
                .num_seconds()
                <= CACHE_MAX_AGE_SECS
        }
        Err(_) => false,
    }
}

fn save_cache(home_dir: &Path, hub_id: &str, index: &SkillIndex) {
    if let Err(e) = std::fs::create_dir_all(cache_dir(home_dir)) {
        warn!(hub = hub_id, "skill hub cache dir: {e}");
        return;
    }
    match serde_json::to_string_pretty(index) {
        Ok(json) => {
            if let Err(e) = std::fs::write(cache_path(home_dir, hub_id), json) {
                warn!(hub = hub_id, "skill hub cache write: {e}");
            }
        }
        Err(e) => warn!(hub = hub_id, "skill hub cache serialize: {e}"),
    }
}

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(12))
        .user_agent("DuDuClaw-SkillHub/1.0")
        .build()
        .map_err(|e| format!("HTTP client: {e}"))
}

/// Cache-through fetch: fresh cache → use it; otherwise call `fetch` and save;
/// on fetch failure fall back to a stale cache when one exists (same contract
/// the GitHub index refresh follows).
async fn cached_index<F, Fut>(home_dir: &Path, hub_id: &str, fetch: F) -> Result<SkillIndex, String>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<SkillIndexEntry>, String>>,
{
    let cached = load_cache(home_dir, hub_id);
    if let Some(idx) = &cached {
        if cache_is_fresh(idx, Utc::now()) {
            return Ok(idx.clone());
        }
    }
    match fetch().await {
        Ok(skills) if !skills.is_empty() => {
            let index = SkillIndex {
                updated_at: Utc::now().to_rfc3339(),
                source: hub_id.to_string(),
                skills,
            };
            save_cache(home_dir, hub_id, &index);
            Ok(index)
        }
        Ok(_) | Err(_) if cached.is_some() => {
            warn!(
                hub = hub_id,
                "hub fetch failed or empty — serving stale cache"
            );
            Ok(cached.unwrap())
        }
        Ok(_) => Err(format!(
            "hub '{hub_id}' returned no entries and no cache exists"
        )),
        Err(e) => Err(e),
    }
}

// ── GitHub hub (existing indexer, wrapped) ──────────────────

/// The existing GitHub Search indexer as a [`SkillHub`]. Delegates entirely to
/// [`SkillRegistry`] — same index file, same refresh/staleness rules, same
/// weighted search — so behavior for existing callers is unchanged.
/// Discovery-only: repos are links, not inline content, so `fetch_manifest`
/// returns `content: None` (the install gate then denies, fail-closed).
#[derive(Debug, Default)]
pub struct GitHubHub;

impl GitHubHub {
    async fn registry(home_dir: &Path) -> SkillRegistry {
        let mut registry = SkillRegistry::load(home_dir);
        if registry.needs_refresh() {
            if let Err(e) = registry.refresh().await {
                warn!("github skill index refresh failed (serving cache): {e}");
            }
        }
        registry
    }
}

impl SkillHub for GitHubHub {
    fn id(&self) -> &str {
        HUB_GITHUB
    }

    fn verified(&self) -> bool {
        true
    }

    fn search<'a>(
        &'a self,
        home_dir: &'a Path,
        query: &'a str,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let registry = Self::registry(home_dir).await;
            Ok(registry.search(query, limit).into_iter().cloned().collect())
        })
    }

    fn list<'a>(
        &'a self,
        home_dir: &'a Path,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let registry = Self::registry(home_dir).await;
            Ok(registry
                .index()
                .skills
                .iter()
                .take(limit)
                .cloned()
                .collect())
        })
    }

    fn fetch_manifest<'a>(
        &'a self,
        home_dir: &'a Path,
        name: &'a str,
    ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
        Box::pin(async move {
            let registry = Self::registry(home_dir).await;
            Ok(registry
                .index()
                .skills
                .iter()
                .find(|s| s.name == name)
                .map(|s| HubManifest {
                    hub: HUB_GITHUB.to_string(),
                    name: s.name.clone(),
                    // GitHub search results are repo links — no inline SKILL.md.
                    content: None,
                    url: s.url.clone(),
                }))
        })
    }
}

// ── ClawHub ─────────────────────────────────────────────────

/// ClawHub (`https://clawhub.ai`) — OpenClaw's skill marketplace.
/// Verified 2026-07-11: `GET /api/v1/skills?limit=N` and
/// `GET /api/v1/skills/<slug>` respond 200 JSON without authentication; the
/// detail response carries the full SKILL.md in `skill.description`.
#[derive(Debug, Default)]
pub struct ClawHubHub;

const CLAWHUB_BASE: &str = "https://clawhub.ai";
const CLAWHUB_LIST_LIMIT: usize = 100;

/// Map a ClawHub `/api/v1/skills` response body to index entries. Pure —
/// unit-tested against a captured live payload.
pub fn parse_clawhub_items(body: &serde_json::Value) -> Vec<SkillIndexEntry> {
    let items = body["items"].as_array().cloned().unwrap_or_default();
    items
        .iter()
        .filter_map(|item| {
            let slug = item["slug"].as_str()?;
            let display = item["displayName"].as_str().unwrap_or(slug);
            let summary = item["summary"].as_str().unwrap_or("");
            let topics: Vec<String> = item["topics"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                        .collect()
                })
                .unwrap_or_default();
            let stars = item["stats"]["stars"].as_u64().unwrap_or(0);
            let updated_ms = item["updatedAt"].as_i64().unwrap_or(0);
            let pushed_at = chrono::DateTime::<Utc>::from_timestamp_millis(updated_ms)
                .map(|dt| dt.to_rfc3339());
            let mut description = summary.to_string();
            if description.is_empty() {
                description = display.to_string();
            }
            Some(SkillIndexEntry {
                name: slug.to_string(),
                description,
                tags: topics,
                author: String::new(),
                url: format!("{CLAWHUB_BASE}/skills/{slug}"),
                compatible: vec!["openclaw".to_string()],
                pushed_at,
                owner_type: None,
                stars,
                trust_tier: crate::trust_tier::TrustTier::Active,
            })
        })
        .collect()
}

async fn clawhub_fetch_list() -> Result<Vec<SkillIndexEntry>, String> {
    let http = http_client()?;
    let url = format!("{CLAWHUB_BASE}/api/v1/skills");
    let resp = http
        .get(&url)
        .query(&[("limit", CLAWHUB_LIST_LIMIT.to_string())])
        .send()
        .await
        .map_err(|e| format!("clawhub request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("clawhub API returned {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("clawhub JSON: {e}"))?;
    Ok(parse_clawhub_items(&body))
}

impl SkillHub for ClawHubHub {
    fn id(&self) -> &str {
        HUB_CLAWHUB
    }

    fn verified(&self) -> bool {
        true
    }

    fn search<'a>(
        &'a self,
        home_dir: &'a Path,
        query: &'a str,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let index = cached_index(home_dir, HUB_CLAWHUB, clawhub_fetch_list).await?;
            Ok(index.search(query, limit).into_iter().cloned().collect())
        })
    }

    fn list<'a>(
        &'a self,
        home_dir: &'a Path,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let index = cached_index(home_dir, HUB_CLAWHUB, clawhub_fetch_list).await?;
            Ok(index.skills.into_iter().take(limit).collect())
        })
    }

    fn fetch_manifest<'a>(
        &'a self,
        _home_dir: &'a Path,
        name: &'a str,
    ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
        Box::pin(async move {
            let http = http_client()?;
            // `name` is either a bare slug or an owner-qualified `owner/slug`
            // (ClawHub reports 409 AMBIGUOUS_SKILL_SLUG when several owners
            // share a slug; disambiguation is the `?owner=` query param —
            // verified live 2026-07-11). Both segments must already be
            // validated as safe path components (the MCP handler does).
            let (owner, slug) = match name.split_once('/') {
                Some((o, s)) => (Some(o), s),
                None => (None, name),
            };
            let url = format!("{CLAWHUB_BASE}/api/v1/skills/{slug}");
            let mut req = http.get(&url);
            if let Some(o) = owner {
                req = req.query(&[("owner", o)]);
            }
            let resp = req
                .send()
                .await
                .map_err(|e| format!("clawhub detail request: {e}"))?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if resp.status() == reqwest::StatusCode::CONFLICT {
                // ClawHub 409 AMBIGUOUS_SKILL_SLUG: multiple owners share the
                // slug. Surface the API's disambiguation message (byte-safe
                // truncation — the body may contain CJK) so the caller can
                // retry with `owner`.
                let body = resp.text().await.unwrap_or_default();
                return Err(format!(
                    "clawhub slug '{slug}' is ambiguous (multiple owners) — retry with the owner \
                     parameter: {}",
                    duduclaw_core::truncate_bytes(&body, 240)
                ));
            }
            if !resp.status().is_success() {
                return Err(format!("clawhub detail API returned {}", resp.status()));
            }
            let body: serde_json::Value = resp
                .json()
                .await
                .map_err(|e| format!("clawhub detail JSON: {e}"))?;
            let content = body["skill"]["description"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .map(|s| s.to_string());
            Ok(Some(HubManifest {
                hub: HUB_CLAWHUB.to_string(),
                name: slug.to_string(),
                content,
                url: format!("{CLAWHUB_BASE}/skills/{slug}"),
            }))
        })
    }
}

// ── LobeHub ─────────────────────────────────────────────────

/// LobeHub / LobeChat public plugin index.
/// Verified 2026-07-11: `GET https://chat-plugins.lobehub.com/index.json`
/// responds 200 JSON without authentication
/// (`{"schemaVersion":1,"plugins":[{identifier, manifest, meta:{...}}]}`).
#[derive(Debug, Default)]
pub struct LobeHubHub;

const LOBEHUB_INDEX_URL: &str = "https://chat-plugins.lobehub.com/index.json";

/// Map the LobeHub `index.json` body to index entries. Pure — unit-tested.
pub fn parse_lobehub_index(body: &serde_json::Value) -> Vec<SkillIndexEntry> {
    let plugins = body["plugins"].as_array().cloned().unwrap_or_default();
    plugins
        .iter()
        .filter_map(|p| {
            let identifier = p["identifier"].as_str()?;
            let meta = &p["meta"];
            let description = meta["description"].as_str().unwrap_or("").to_string();
            let mut tags: Vec<String> = meta["tags"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                        .collect()
                })
                .unwrap_or_default();
            if let Some(cat) = meta["category"].as_str() {
                if !cat.is_empty() {
                    tags.push(cat.to_lowercase());
                }
            }
            let author = p["author"].as_str().unwrap_or("").to_string();
            let url = p["homepage"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| p["manifest"].as_str())
                .unwrap_or("")
                .to_string();
            Some(SkillIndexEntry {
                name: identifier.to_string(),
                description,
                tags,
                author,
                url,
                compatible: vec!["lobechat".to_string()],
                pushed_at: p["createdAt"].as_str().map(|s| s.to_string()),
                owner_type: None,
                stars: 0,
                trust_tier: crate::trust_tier::TrustTier::Active,
            })
        })
        .collect()
}

async fn lobehub_fetch_index() -> Result<Vec<SkillIndexEntry>, String> {
    let http = http_client()?;
    let resp = http
        .get(LOBEHUB_INDEX_URL)
        .send()
        .await
        .map_err(|e| format!("lobehub request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("lobehub index returned {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("lobehub JSON: {e}"))?;
    Ok(parse_lobehub_index(&body))
}

/// Hosts a LobeHub manifest URL may point at. The index itself lives on
/// `chat-plugins.lobehub.com`; the manifests it references live on
/// `*.chat-plugin.lobehub.com` (verified live 2026-07-11), so the allowlist
/// is the index host plus the `lobehub.com` domain (anchored suffix match —
/// subdomains only, never `evillobehub.com`).
const LOBEHUB_MANIFEST_ALLOWED_HOSTS: &[&str] = &["chat-plugins.lobehub.com", "lobehub.com"];

/// Fail-closed SSRF gate for manifest URLs coming from the third-party
/// LobeHub index (untrusted DATA — a poisoned index entry must not make us
/// fetch arbitrary internal/metadata endpoints). Pure — unit-tested.
///
/// Requires: `https://` scheme; a plain DNS host on the allowlist (exact
/// host or dot-anchored subdomain — never substring matching); no userinfo
/// (`@`), no explicit port, no IP-literal host (IPv4 or bracketed IPv6).
pub fn lobehub_manifest_url_allowed(url: &str) -> Result<(), String> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Err(format!(
            "lobehub manifest URL is not https — refusing (fail-closed): {url}"
        ));
    };
    let authority_end = rest
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    if authority.is_empty() {
        return Err("lobehub manifest URL has no host — refusing".to_string());
    }
    // Userinfo (`https://allowed.com@evil.com/`) and explicit ports / IPv6
    // brackets are all rejected outright — a legit manifest URL needs none.
    if authority.contains('@') || authority.contains(':') || authority.contains('[') {
        return Err(format!(
            "lobehub manifest URL host '{authority}' carries userinfo/port/IP-literal syntax — refusing (fail-closed)"
        ));
    }
    let host = authority.to_ascii_lowercase();
    // IPv4 literal (all-numeric labels) — refuse.
    let labels: Vec<&str> = host.split('.').collect();
    if labels
        .iter()
        .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(format!(
            "lobehub manifest URL host '{host}' is an IP literal — refusing (fail-closed)"
        ));
    }
    let allowed = LOBEHUB_MANIFEST_ALLOWED_HOSTS.iter().any(|a| {
        host == *a || host.ends_with(&format!(".{a}")) // dot-anchored subdomain
    });
    if allowed {
        Ok(())
    } else {
        Err(format!(
            "lobehub manifest URL host '{host}' is not on the manifest allowlist \
             ({LOBEHUB_MANIFEST_ALLOWED_HOSTS:?}) — refusing (fail-closed)"
        ))
    }
}

/// Manifest URL for one plugin, straight from the **live** index (exact
/// identifier match — the mapped cache doesn't retain the raw manifest URL).
/// The URL is untrusted index DATA — [`lobehub_manifest_url_allowed`] gates
/// scheme + host before anything is fetched.
async fn lobehub_manifest_url(identifier: &str) -> Result<Option<String>, String> {
    let http = http_client()?;
    let resp = http
        .get(LOBEHUB_INDEX_URL)
        .send()
        .await
        .map_err(|e| format!("lobehub request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("lobehub index returned {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("lobehub JSON: {e}"))?;
    let plugins = body["plugins"].as_array().cloned().unwrap_or_default();
    for p in &plugins {
        if p["identifier"].as_str() == Some(identifier) {
            let manifest = p["manifest"].as_str().unwrap_or("");
            lobehub_manifest_url_allowed(manifest)?;
            return Ok(Some(manifest.to_string()));
        }
    }
    Ok(None)
}

impl SkillHub for LobeHubHub {
    fn id(&self) -> &str {
        HUB_LOBEHUB
    }

    fn verified(&self) -> bool {
        true
    }

    fn search<'a>(
        &'a self,
        home_dir: &'a Path,
        query: &'a str,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let index = cached_index(home_dir, HUB_LOBEHUB, lobehub_fetch_index).await?;
            Ok(index.search(query, limit).into_iter().cloned().collect())
        })
    }

    fn list<'a>(
        &'a self,
        home_dir: &'a Path,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let index = cached_index(home_dir, HUB_LOBEHUB, lobehub_fetch_index).await?;
            Ok(index.skills.into_iter().take(limit).collect())
        })
    }

    fn fetch_manifest<'a>(
        &'a self,
        _home_dir: &'a Path,
        name: &'a str,
    ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
        Box::pin(async move {
            let Some(manifest_url) = lobehub_manifest_url(name).await? else {
                return Ok(None);
            };
            let http = http_client()?;
            let resp = http
                .get(&manifest_url)
                .send()
                .await
                .map_err(|e| format!("lobehub manifest request: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("lobehub manifest returned {}", resp.status()));
            }
            let text = resp
                .text()
                .await
                .map_err(|e| format!("lobehub manifest body: {e}"))?;
            Ok(Some(HubManifest {
                hub: HUB_LOBEHUB.to_string(),
                name: name.to_string(),
                content: Some(text),
                url: manifest_url,
            }))
        })
    }
}

// ── skills.sh (UNVERIFIED stub) ─────────────────────────────

/// skills.sh (Vercel's Agent Skills Directory) — **UNVERIFIED stub**.
///
/// First-hand check 2026-07-11: `GET https://skills.sh/api/v1/skills` and
/// `/api/v1/skills/search` both return 401 `authentication_required` without
/// a Vercel OIDC bearer token, which DuDuClaw cannot mint. Per the G5 rule
/// ("any hub whose API you cannot verify first-hand gets a stub provider
/// marked UNVERIFIED and is excluded from defaults") every method returns an
/// honest error; nothing is fabricated.
#[derive(Debug, Default)]
pub struct SkillsShHub;

const SKILLS_SH_UNAVAILABLE: &str = "skills.sh API requires a Vercel OIDC bearer token \
     (verified 2026-07-11: unauthenticated requests return 401 authentication_required) — \
     hub is an UNVERIFIED stub and is excluded from defaults";

impl SkillHub for SkillsShHub {
    fn id(&self) -> &str {
        HUB_SKILLS_SH
    }

    fn verified(&self) -> bool {
        false
    }

    fn search<'a>(
        &'a self,
        _home_dir: &'a Path,
        _query: &'a str,
        _limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move { Err(SKILLS_SH_UNAVAILABLE.to_string()) })
    }

    fn list<'a>(
        &'a self,
        _home_dir: &'a Path,
        _limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move { Err(SKILLS_SH_UNAVAILABLE.to_string()) })
    }

    fn fetch_manifest<'a>(
        &'a self,
        _home_dir: &'a Path,
        _name: &'a str,
    ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
        Box::pin(async move { Err(SKILLS_SH_UNAVAILABLE.to_string()) })
    }
}

// ── Registry / aggregator ───────────────────────────────────

/// One aggregated search hit, labeled with its source hub.
#[derive(Debug, Clone)]
pub struct HubHit {
    pub hub: String,
    pub score: usize,
    pub entry: SkillIndexEntry,
}

/// Aggregated search result: merged hits plus per-hub failures (never
/// silently dropped).
#[derive(Debug, Default)]
pub struct AggregatedSearch {
    pub hits: Vec<HubHit>,
    /// `(hub_id, error)` for every hub that failed.
    pub errors: Vec<(String, String)>,
}

/// The configured set of hubs. Construction is config-driven; selection is
/// exact-id only.
pub struct HubRegistry {
    hubs: Vec<Box<dyn SkillHub>>,
}

fn make_hub(id: &str) -> Option<Box<dyn SkillHub>> {
    match id {
        HUB_GITHUB => Some(Box::new(GitHubHub)),
        HUB_CLAWHUB => Some(Box::new(ClawHubHub)),
        HUB_LOBEHUB => Some(Box::new(LobeHubHub)),
        HUB_SKILLS_SH => Some(Box::new(SkillsShHub)),
        _ => None,
    }
}

impl HubRegistry {
    /// The default hub set: verified, no-auth hubs only (`github`, `clawhub`,
    /// `lobehub`).
    pub fn default_hubs() -> Self {
        Self {
            hubs: DEFAULT_HUB_IDS
                .iter()
                .filter_map(|id| make_hub(id))
                .collect(),
        }
    }

    /// Build from raw `config.toml` content: `[skill_hubs] enabled = [...]`.
    /// Ids match exactly against [`KNOWN_HUB_IDS`]; unknown ids are warned and
    /// skipped. Missing/malformed section, or an empty valid set ⇒ defaults.
    pub fn from_config_str(content: &str) -> Self {
        let table: toml::Value = match content.parse() {
            Ok(t) => t,
            Err(_) => return Self::default_hubs(),
        };
        let Some(list) = table
            .get("skill_hubs")
            .and_then(|s| s.get("enabled"))
            .and_then(|v| v.as_array())
        else {
            return Self::default_hubs();
        };
        let mut hubs: Vec<Box<dyn SkillHub>> = Vec::new();
        for v in list {
            let Some(id) = v.as_str() else { continue };
            let id = id.trim();
            // Exact token equality — never substring matching.
            if !KNOWN_HUB_IDS.iter().any(|k| *k == id) {
                warn!(
                    hub = id,
                    "unknown skill hub id in [skill_hubs] enabled — skipped"
                );
                continue;
            }
            if hubs.iter().any(|h| h.id() == id) {
                continue; // dedupe
            }
            if let Some(h) = make_hub(id) {
                hubs.push(h);
            }
        }
        if hubs.is_empty() {
            return Self::default_hubs();
        }
        Self { hubs }
    }

    /// Load `[skill_hubs]` from `<home>/config.toml`; absent ⇒ defaults.
    pub fn from_home(home_dir: &Path) -> Self {
        match std::fs::read_to_string(home_dir.join("config.toml")) {
            Ok(c) => Self::from_config_str(&c),
            Err(_) => Self::default_hubs(),
        }
    }

    /// Test/DI constructor.
    pub fn with_hubs(hubs: Vec<Box<dyn SkillHub>>) -> Self {
        Self { hubs }
    }

    pub fn ids(&self) -> Vec<&str> {
        self.hubs.iter().map(|h| h.id()).collect()
    }

    /// Exact-id lookup.
    pub fn get(&self, id: &str) -> Option<&dyn SkillHub> {
        self.hubs.iter().find(|h| h.id() == id).map(|h| h.as_ref())
    }

    /// Search one hub (`only = Some(id)`) or aggregate across all configured
    /// hubs (`only = None`). Every hit is (re-)scored with the same
    /// `score_match` weighting the GitHub index uses; cross-hub duplicates
    /// (same skill name) keep the higher score, hub declaration order breaks
    /// ties (earlier hub wins).
    pub async fn search(
        &self,
        home_dir: &Path,
        query: &str,
        limit: usize,
        only: Option<&str>,
    ) -> AggregatedSearch {
        let lower = query.to_lowercase();
        let terms: Vec<&str> = lower.split_whitespace().collect();
        let mut out = AggregatedSearch::default();

        for hub in &self.hubs {
            if let Some(want) = only {
                if hub.id() != want {
                    continue;
                }
            }
            match hub.search(home_dir, query, limit).await {
                Ok(entries) => {
                    for entry in entries {
                        let score = score_match(&entry, &terms);
                        if score == 0 {
                            continue;
                        }
                        // Cross-hub dedupe by exact name: keep the better hit.
                        if let Some(existing) =
                            out.hits.iter_mut().find(|h| h.entry.name == entry.name)
                        {
                            if score > existing.score {
                                *existing = HubHit {
                                    hub: hub.id().to_string(),
                                    score,
                                    entry,
                                };
                            }
                            continue;
                        }
                        out.hits.push(HubHit {
                            hub: hub.id().to_string(),
                            score,
                            entry,
                        });
                    }
                }
                Err(e) => out.errors.push((hub.id().to_string(), e)),
            }
        }

        out.hits.sort_by(|a, b| {
            b.score
                .cmp(&a.score)
                .then_with(|| a.entry.name.cmp(&b.entry.name))
        });
        out.hits.truncate(limit);
        out
    }
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, desc: &str, tags: &[&str]) -> SkillIndexEntry {
        SkillIndexEntry {
            name: name.to_string(),
            description: desc.to_string(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            author: String::new(),
            url: format!("https://example.com/{name}"),
            compatible: vec![],
            pushed_at: None,
            owner_type: None,
            stars: 0,
            trust_tier: crate::trust_tier::TrustTier::Active,
        }
    }

    /// Deterministic in-memory hub for aggregation tests.
    struct MockHub {
        id: &'static str,
        entries: Vec<SkillIndexEntry>,
        fail: bool,
    }

    impl SkillHub for MockHub {
        fn id(&self) -> &str {
            self.id
        }
        fn verified(&self) -> bool {
            true
        }
        fn search<'a>(
            &'a self,
            _home: &'a Path,
            _query: &'a str,
            limit: usize,
        ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
            Box::pin(async move {
                if self.fail {
                    return Err("mock hub down".to_string());
                }
                Ok(self.entries.iter().take(limit).cloned().collect())
            })
        }
        fn list<'a>(
            &'a self,
            home: &'a Path,
            limit: usize,
        ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
            self.search(home, "", limit)
        }
        fn fetch_manifest<'a>(
            &'a self,
            _home: &'a Path,
            name: &'a str,
        ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
            Box::pin(async move {
                Ok(self
                    .entries
                    .iter()
                    .find(|e| e.name == name)
                    .map(|e| HubManifest {
                        hub: self.id.to_string(),
                        name: e.name.clone(),
                        content: Some(format!("# {}", e.name)),
                        url: e.url.clone(),
                    }))
            })
        }
    }

    #[tokio::test]
    async fn aggregation_merges_scores_and_dedupes_by_name() {
        let hub_a = MockHub {
            id: "a",
            entries: vec![
                entry("browser-skill", "automates a browser", &["browser"]),
                entry("shared-skill", "browser helper", &[]),
            ],
            fail: false,
        };
        let hub_b = MockHub {
            id: "b",
            // Same name, better match (name hit) — must win the dedupe.
            entries: vec![
                entry("shared-skill", "x", &["browser"]),
                entry("other", "nothing", &[]),
            ],
            fail: false,
        };
        let reg = HubRegistry::with_hubs(vec![Box::new(hub_a), Box::new(hub_b)]);
        let res = reg
            .search(Path::new("/nonexistent"), "browser", 10, None)
            .await;

        assert!(res.errors.is_empty());
        // "other" scores 0 for "browser" and must be filtered out.
        assert_eq!(res.hits.len(), 2);
        // name(10)+tag(7)+desc(5) ordering: browser-skill (desc+tag) beats
        // shared-skill; shared-skill's better variant is hub b's (tag 7 > desc 5).
        let shared = res
            .hits
            .iter()
            .find(|h| h.entry.name == "shared-skill")
            .unwrap();
        assert_eq!(shared.hub, "b", "higher-scoring duplicate must win");
        assert!(res.hits[0].score >= res.hits[1].score);
    }

    #[tokio::test]
    async fn failing_hub_is_reported_not_swallowed() {
        let ok = MockHub {
            id: "ok",
            entries: vec![entry("s1", "browser", &[])],
            fail: false,
        };
        let down = MockHub {
            id: "down",
            entries: vec![],
            fail: true,
        };
        let reg = HubRegistry::with_hubs(vec![Box::new(ok), Box::new(down)]);
        let res = reg
            .search(Path::new("/nonexistent"), "browser", 10, None)
            .await;
        assert_eq!(res.hits.len(), 1);
        assert_eq!(res.errors.len(), 1);
        assert_eq!(res.errors[0].0, "down");
        assert!(res.errors[0].1.contains("mock hub down"));
    }

    #[tokio::test]
    async fn only_filter_uses_exact_id() {
        let a = MockHub {
            id: "hub",
            entries: vec![entry("s1", "browser", &[])],
            fail: false,
        };
        // Adversarial id that would match a substring check.
        let b = MockHub {
            id: "hub-evil",
            entries: vec![entry("s2", "browser", &[])],
            fail: false,
        };
        let reg = HubRegistry::with_hubs(vec![Box::new(a), Box::new(b)]);
        let res = reg
            .search(Path::new("/nonexistent"), "browser", 10, Some("hub"))
            .await;
        assert_eq!(res.hits.len(), 1);
        assert_eq!(res.hits[0].hub, "hub");
    }

    #[test]
    fn defaults_exclude_unverified_skills_sh() {
        let reg = HubRegistry::default_hubs();
        let ids = reg.ids();
        assert!(ids.contains(&HUB_GITHUB));
        assert!(ids.contains(&HUB_CLAWHUB));
        assert!(ids.contains(&HUB_LOBEHUB));
        assert!(
            !ids.contains(&HUB_SKILLS_SH),
            "unverified hub must not be a default"
        );
    }

    #[test]
    fn config_parses_exact_ids_and_falls_back_on_garbage() {
        let reg = HubRegistry::from_config_str(
            "[skill_hubs]\nenabled = [\"clawhub\", \"nope\", \"clawhub\"]\n",
        );
        assert_eq!(
            reg.ids(),
            vec![HUB_CLAWHUB],
            "exact ids only, deduped, unknown skipped"
        );

        // Malformed toml / missing section / all-unknown ⇒ defaults.
        assert_eq!(
            HubRegistry::from_config_str("garbage {{{").ids(),
            HubRegistry::default_hubs().ids()
        );
        assert_eq!(
            HubRegistry::from_config_str("[other]\nx=1").ids(),
            HubRegistry::default_hubs().ids()
        );
        assert_eq!(
            HubRegistry::from_config_str("[skill_hubs]\nenabled = [\"bogus\"]").ids(),
            HubRegistry::default_hubs().ids()
        );
    }

    #[tokio::test]
    async fn skills_sh_stub_is_honest_about_unavailability() {
        let hub = SkillsShHub;
        assert!(!hub.verified());
        let err = hub
            .search(Path::new("/nonexistent"), "x", 5)
            .await
            .unwrap_err();
        assert!(
            err.contains("401"),
            "error must cite the first-hand verification: {err}"
        );
    }

    #[test]
    fn clawhub_mapping_from_captured_live_payload() {
        // Shape captured live 2026-07-11 from GET clawhub.ai/api/v1/skills.
        let body: serde_json::Value = serde_json::json!({
            "items": [{
                "slug": "pro-code-reviewer",
                "displayName": "Code Reviewer",
                "summary": "Review code changes against platform-specific rules",
                "topics": ["Code Review"],
                "stats": {"comments":0,"downloads":520,"installs":16,"stars":3,"versions":5},
                "createdAt": 1778223456288u64,
                "updatedAt": 1783775545882u64
            }]
        });
        let entries = parse_clawhub_items(&body);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.name, "pro-code-reviewer");
        assert_eq!(e.tags, vec!["code review"]);
        assert_eq!(e.stars, 3);
        assert!(e.url.starts_with("https://clawhub.ai/skills/"));
        assert!(e.pushed_at.is_some());
    }

    #[test]
    fn lobehub_mapping_from_captured_live_payload() {
        // Shape captured live 2026-07-11 from chat-plugins.lobehub.com/index.json.
        let body: serde_json::Value = serde_json::json!({
            "schemaVersion": 1,
            "plugins": [{
                "author": "webfx",
                "createdAt": "2026-01-12",
                "homepage": "https://webfx.ai",
                "identifier": "seo_assistant",
                "manifest": "https://openai-collections.chat-plugin.lobehub.com/seo-assistant/manifest.json",
                "meta": {
                    "description": "Generate search engine keyword information",
                    "tags": ["seo", "keyword"],
                    "title": "SEO Assistant",
                    "category": "tools"
                }
            }]
        });
        let entries = parse_lobehub_index(&body);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.name, "seo_assistant");
        assert_eq!(e.author, "webfx");
        assert!(e.tags.contains(&"seo".to_string()));
        assert!(
            e.tags.contains(&"tools".to_string()),
            "category folded into tags"
        );
        assert_eq!(e.url, "https://webfx.ai");
    }

    #[test]
    fn lobehub_manifest_host_gate_is_anchored_and_fail_closed() {
        // Legit hosts: index host, apex, and dot-anchored subdomains.
        for ok in [
            "https://chat-plugins.lobehub.com/index.json",
            "https://lobehub.com/m.json",
            "https://openai-collections.chat-plugin.lobehub.com/seo-assistant/manifest.json",
            "https://CHAT-PLUGINS.LOBEHUB.COM/x", // host is case-insensitive
        ] {
            assert!(lobehub_manifest_url_allowed(ok).is_ok(), "{ok}");
        }
        // SSRF shapes: off-allowlist, suffix spoofing, scheme, userinfo,
        // ports, IP literals.
        for bad in [
            "http://lobehub.com/m.json",                       // not https
            "https://evil.com/m.json",                          // off-list
            "https://evillobehub.com/m.json",                   // suffix spoof (no dot anchor)
            "https://lobehub.com.evil.com/m.json",              // prefix spoof
            "https://lobehub.com@evil.com/m.json",              // userinfo trick
            "https://lobehub.com:8443/m.json",                  // explicit port
            "https://169.254.169.254/latest/meta-data",         // IP literal (metadata)
            "https://[::1]/m.json",                             // IPv6 literal
            "https://",                                          // empty host
            "",                                                  // empty
        ] {
            assert!(lobehub_manifest_url_allowed(bad).is_err(), "must refuse {bad}");
        }
    }

    #[test]
    fn cache_freshness_window_is_24h() {
        let mut idx = SkillIndex::empty();
        idx.skills.push(entry("s", "d", &[]));
        let now = Utc::now();
        idx.updated_at = (now - chrono::Duration::hours(23)).to_rfc3339();
        assert!(cache_is_fresh(&idx, now));
        idx.updated_at = (now - chrono::Duration::hours(25)).to_rfc3339();
        assert!(!cache_is_fresh(&idx, now));
        // Unparseable timestamp ⇒ stale (fail-safe).
        idx.updated_at = "not-a-date".to_string();
        assert!(!cache_is_fresh(&idx, now));
        // Empty index is never fresh.
        let empty = SkillIndex::empty();
        assert!(!cache_is_fresh(&empty, now));
    }
}
