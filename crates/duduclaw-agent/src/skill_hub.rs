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
/// anthropics/skills — the official first-party seed repo (WP2.6 P0).
pub const HUB_ANTHROPIC: &str = "anthropic-skills";

/// All hub ids this build knows how to construct.
pub const KNOWN_HUB_IDS: &[&str] =
    &[HUB_GITHUB, HUB_CLAWHUB, HUB_LOBEHUB, HUB_SKILLS_SH, HUB_ANTHROPIC];

/// Hubs enabled by default (WP2.6 P0 set): the official anthropics/skills seed
/// repo plus the three verified, no-auth marketplaces. `skills-sh` is now a
/// public read endpoint (verified 2026-07-27) and included as a discovery /
/// ranking-signal source; it is discovery-only for install (fail-closed at the
/// gate) since it exposes no skill content.
pub const DEFAULT_HUB_IDS: &[&str] =
    &[HUB_ANTHROPIC, HUB_GITHUB, HUB_CLAWHUB, HUB_LOBEHUB, HUB_SKILLS_SH];

/// Per-source search deadline (WP2.6 §1). A hub that does not answer within
/// this window is reported `[unreachable: <hub>: timeout]` and skipped — one
/// slow source never stalls the aggregate.
pub const PER_SOURCE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// SHA-256 (lowercase hex) of `content` when present — the hash of exactly
    /// the bytes the install gate will write. Recorded for provenance and
    /// checked against `expected_hash` when the source supplies one.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Hash the source *claims* for this content (e.g. a pinned commit blob, a
    /// marketplace `contentHash`). When present, the gate DENIES on mismatch
    /// (fail-closed, TOCTOU defence). `None` ⇒ no cross-check available.
    #[serde(default)]
    pub expected_hash: Option<String>,
    /// Source-side security verdict (WP2.6 §3 layer-1): `"clean"` /
    /// `"suspicious"` / `"malicious"` / `"unknown"`, or `None`.
    #[serde(default)]
    pub source_verdict: Option<String>,
    /// Trust floor of the originating hub — non-official sources are forced
    /// through sandbox-TTL trial before graduation (§3 layer-4).
    #[serde(default)]
    pub trust_floor: crate::trust_tier::TrustTier,
}

impl HubManifest {
    /// Compute + attach the SHA-256 of the current `content` (no-op when the
    /// hub served no content). Idempotent.
    pub fn with_computed_hash(mut self) -> Self {
        self.content_hash = self.content.as_deref().map(sha256_hex);
        self
    }
}

/// Lowercase-hex SHA-256 of a string — the WP2.6 content-hash primitive.
/// Uses `ring` (already a crate dependency) so no new hashing crate is pulled in.
pub fn sha256_hex(s: &str) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, s.as_bytes());
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// Verify installed content against a source-claimed hash (fail-closed).
/// `Ok(())` when no `expected` is supplied (nothing to check) or it matches;
/// `Err` on mismatch — the caller must DENY the install.
pub fn verify_content_hash(content: &str, expected: Option<&str>) -> Result<(), String> {
    let Some(expected) = expected.map(|e| e.trim().to_ascii_lowercase()).filter(|e| !e.is_empty())
    else {
        return Ok(());
    };
    let actual = sha256_hex(content);
    if actual == expected {
        Ok(())
    } else {
        Err(format!(
            "content hash mismatch — install DENIED (fail-closed): expected {expected}, got {actual}"
        ))
    }
}

/// Normalize a source's raw verdict string into the WP2.6 vocabulary. Unknown
/// / absent verdicts map to `None` (advisory), while anything indicating
/// suspicion or a block maps to a non-clean tag the gate treats as DENY.
pub fn normalize_verdict(raw: Option<&str>) -> Option<String> {
    let v = raw?.trim().to_ascii_lowercase();
    if v.is_empty() {
        return None;
    }
    let tag = if v.contains("malicious") || v.contains("block") || v.contains("banned") {
        "malicious"
    } else if v.contains("suspicious") || v.contains("flag") || v.contains("warn") {
        "suspicious"
    } else if v.contains("clean") || v.contains("verified") || v.contains("installable") || v == "ok"
    {
        "clean"
    } else {
        "unknown"
    };
    Some(tag.to_string())
}

/// True when a source verdict must block an install (fail-closed): anything
/// that is not explicitly `clean`/`unknown`/absent.
pub fn verdict_is_blocking(verdict: Option<&str>) -> bool {
    matches!(verdict, Some("malicious") | Some("suspicious"))
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

    /// Trust floor for skills from this source (WP2.6 §1). Official first-party
    /// sources (anthropics/skills) return `Official`; community marketplaces
    /// default to `Active` and let per-entry classification demote to `Orphan`.
    fn trust_floor(&self) -> crate::trust_tier::TrustTier {
        crate::trust_tier::TrustTier::Active
    }
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
                    content_hash: None,
                    expected_hash: None,
                    source_verdict: None,
                    trust_floor: crate::trust_tier::TrustTier::Active,
                }))
        })
    }
}

// ── ClawHub ─────────────────────────────────────────────────

/// ClawHub (`https://clawhub.ai`) — OpenClaw's skill marketplace.
/// Verified 2026-07-27: `GET /api/v1/search?q=&nonSuspiciousOnly=true` responds
/// 200 JSON unauthenticated (`{"results":[...]}`) carrying the WP2.6 ranking
/// signals — `metrics.rolling60DayInstalls`, `official`, and the `trust`
/// verdict; `GET /api/v1/skills/<slug>` serves the full SKILL.md in
/// `skill.description` for install.
#[derive(Debug, Default)]
pub struct ClawHubHub;

const CLAWHUB_BASE: &str = "https://clawhub.ai";

/// Map a ClawHub `/api/v1/search` response body to index entries. Pure —
/// unit-tested against a captured live payload (2026-07-27).
pub fn parse_clawhub_search(body: &serde_json::Value) -> Vec<SkillIndexEntry> {
    let results = body["results"].as_array().cloned().unwrap_or_default();
    results
        .iter()
        .filter_map(|item| {
            let slug = item["slug"].as_str()?;
            let display = item["displayName"].as_str().unwrap_or(slug);
            let summary = item["summary"].as_str().unwrap_or("");
            let owner = item["ownerHandle"].as_str().unwrap_or("");
            let official = item["official"].as_bool().unwrap_or(false);
            let native_skill = &item["native"]["skill"];
            let topics: Vec<String> = native_skill["topics"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                        .collect()
                })
                .unwrap_or_default();
            let stars = native_skill["stats"]["stars"].as_u64().unwrap_or(0);
            // 60-day install signal for ranking (the exact metric the formula wants).
            let install_count = item["metrics"]["rolling60DayInstalls"].as_u64().unwrap_or(0);
            let updated_ms = item["metrics"]["updatedAt"]
                .as_i64()
                .or_else(|| native_skill["updatedAt"].as_i64())
                .unwrap_or(0);
            let pushed_at = chrono::DateTime::<Utc>::from_timestamp_millis(updated_ms)
                .map(|dt| dt.to_rfc3339());
            // Canonical human URL: `/owner/skills/slug`.
            let canonical = item["canonicalUrl"].as_str().unwrap_or("");
            let url = if canonical.is_empty() {
                format!("{CLAWHUB_BASE}/skills/{slug}")
            } else {
                format!("{CLAWHUB_BASE}{canonical}")
            };
            // Source-side verdict (layer-1): fold clawHubVerdict + installability
            // + isSuspicious into one normalized tag.
            let raw_verdict = item["trust"]["clawHubVerdict"]
                .as_str()
                .map(|s| s.to_string())
                .or_else(|| {
                    if native_skill["isSuspicious"].as_bool() == Some(true) {
                        Some("suspicious".to_string())
                    } else {
                        item["trust"]["installability"].as_str().map(|s| s.to_string())
                    }
                });
            let source_verdict = normalize_verdict(raw_verdict.as_deref());
            let trust_tier = if official {
                crate::trust_tier::TrustTier::Official
            } else {
                crate::trust_tier::classify_trust_tier(
                    pushed_at.as_deref(),
                    None,
                    stars,
                    Utc::now(),
                )
            };
            let mut description = summary.to_string();
            if description.is_empty() {
                description = display.to_string();
            }
            // Upstream origin for cross-source dedup, when ClawHub knows it.
            let source_url = item["install"]["sourceUrl"]
                .as_str()
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .or_else(|| Some(url.clone()));
            Some(SkillIndexEntry {
                name: slug.to_string(),
                description,
                tags: topics,
                author: owner.to_string(),
                url,
                compatible: vec!["openclaw".to_string()],
                pushed_at,
                owner_type: None,
                stars,
                trust_tier,
                install_count,
                source_url,
                source_verdict,
            })
        })
        .collect()
}

/// Live query against ClawHub search. `nonSuspiciousOnly=true` = the source's
/// own first-pass filter (layer-1); we still rescan every install locally.
async fn clawhub_search(query: &str, limit: usize) -> Result<Vec<SkillIndexEntry>, String> {
    let http = http_client()?;
    let url = format!("{CLAWHUB_BASE}/api/v1/search");
    let resp = http
        .get(&url)
        .query(&[
            ("q", query),
            ("nonSuspiciousOnly", "true"),
            ("limit", &limit.to_string()),
        ])
        .send()
        .await
        .map_err(|e| format!("clawhub search request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("clawhub search API returned {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("clawhub search JSON: {e}"))?;
    let mut out = parse_clawhub_search(&body);
    out.truncate(limit);
    Ok(out)
}

impl SkillHub for ClawHubHub {
    fn id(&self) -> &str {
        HUB_CLAWHUB
    }

    fn verified(&self) -> bool {
        true
    }

    // ClawHub `/search` is inherently query-live (anonymous budget 1200/min),
    // so — unlike the index-cached hubs — it is called directly per query.
    fn search<'a>(
        &'a self,
        _home_dir: &'a Path,
        query: &'a str,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move { clawhub_search(query, limit).await })
    }

    fn list<'a>(
        &'a self,
        _home_dir: &'a Path,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        // No query ⇒ ask for the popular/featured set via an empty-query search.
        Box::pin(async move { clawhub_search("", limit).await })
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
            // Detail-level verdict (layer-1): isSuspicious flag or a moderation
            // status, whichever the endpoint exposes.
            let raw_verdict = if body["skill"]["isSuspicious"].as_bool() == Some(true) {
                Some("suspicious".to_string())
            } else {
                body["moderation"]["status"]
                    .as_str()
                    .or_else(|| body["moderation"]["verdict"].as_str())
                    .map(|s| s.to_string())
            };
            Ok(Some(
                HubManifest {
                    hub: HUB_CLAWHUB.to_string(),
                    name: slug.to_string(),
                    content,
                    url: format!("{CLAWHUB_BASE}/skills/{slug}"),
                    content_hash: None,
                    expected_hash: None,
                    source_verdict: normalize_verdict(raw_verdict.as_deref()),
                    trust_floor: crate::trust_tier::TrustTier::Active,
                }
                .with_computed_hash(),
            ))
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
                install_count: 0,
                source_url: p["homepage"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()),
                source_verdict: None,
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
            Ok(Some(
                HubManifest {
                    hub: HUB_LOBEHUB.to_string(),
                    name: name.to_string(),
                    content: Some(text),
                    url: manifest_url,
                    content_hash: None,
                    expected_hash: None,
                    source_verdict: None,
                    trust_floor: crate::trust_tier::TrustTier::Active,
                }
                .with_computed_hash(),
            ))
        })
    }
}

// ── skills.sh ───────────────────────────────────────────────

/// skills.sh — cross-ecosystem Agent Skills directory.
/// Verified 2026-07-27: `GET https://www.skills.sh/api/search?q=` responds
/// 200 JSON unauthenticated (`{"skills":[{id, skillId, name, installs, source}]}`).
/// This is an **uncommitted** third-party interface, so parsing is fail-closed:
/// a missing/renamed `skills` array disables the source and is reported
/// upstream rather than silently returning nothing.
///
/// Discovery-only: skills.sh exposes no skill *content*, so `fetch_manifest`
/// returns `None` and the install gate denies (fail-closed) — the directory is
/// a ranking + discovery signal, not an install source.
#[derive(Debug, Default)]
pub struct SkillsShHub;

const SKILLS_SH_BASE: &str = "https://www.skills.sh";

/// Map a skills.sh `/api/search` body to entries. Pure — unit-tested.
/// Returns `Err` when the committed `skills` array is absent (interface drift):
/// the caller then reports `[unreachable]` and drops the source, never guesses.
pub fn parse_skills_sh(body: &serde_json::Value) -> Result<Vec<SkillIndexEntry>, String> {
    let Some(arr) = body["skills"].as_array() else {
        return Err(
            "skills.sh response has no `skills` array — interface drift, source disabled \
             (fail-closed, not fabricated)"
                .to_string(),
        );
    };
    Ok(arr
        .iter()
        .filter_map(|s| {
            let name = s["name"].as_str().or_else(|| s["skillId"].as_str())?;
            let source = s["source"].as_str().unwrap_or("");
            let installs = s["installs"].as_u64().unwrap_or(0);
            let author = source.split('/').next().unwrap_or("").to_string();
            // `source` is an `owner/repo` GitHub slug; `id` adds the skill path.
            let source_url = if source.is_empty() {
                None
            } else {
                Some(format!("https://github.com/{source}"))
            };
            let url = source_url
                .clone()
                .unwrap_or_else(|| format!("{SKILLS_SH_BASE}/skills"));
            Some(SkillIndexEntry {
                name: name.to_string(),
                description: String::new(),
                tags: vec![],
                author,
                url,
                compatible: vec!["claude-code".to_string()],
                pushed_at: None,
                owner_type: None,
                stars: 0,
                trust_tier: crate::trust_tier::TrustTier::Active,
                install_count: installs,
                source_url,
                source_verdict: None,
            })
        })
        .collect())
}

async fn skills_sh_search(query: &str, limit: usize) -> Result<Vec<SkillIndexEntry>, String> {
    let http = http_client()?;
    let url = format!("{SKILLS_SH_BASE}/api/search");
    let resp = http
        .get(&url)
        .query(&[("q", query)])
        .send()
        .await
        .map_err(|e| format!("skills.sh request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("skills.sh API returned {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("skills.sh JSON: {e}"))?;
    let mut out = parse_skills_sh(&body)?;
    out.truncate(limit);
    Ok(out)
}

impl SkillHub for SkillsShHub {
    fn id(&self) -> &str {
        HUB_SKILLS_SH
    }

    fn verified(&self) -> bool {
        true
    }

    fn search<'a>(
        &'a self,
        _home_dir: &'a Path,
        query: &'a str,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move { skills_sh_search(query, limit).await })
    }

    fn list<'a>(
        &'a self,
        _home_dir: &'a Path,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move { skills_sh_search("", limit).await })
    }

    fn fetch_manifest<'a>(
        &'a self,
        _home_dir: &'a Path,
        _name: &'a str,
    ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
        // Discovery-only: no content interface ⇒ the gate denies (fail-closed).
        Box::pin(async move { Ok(None) })
    }
}

// ── anthropics/skills (official seed repo) ──────────────────

/// The official first-party skills repo `anthropics/skills` (WP2.6 P0).
/// Skills live at `skills/<name>/SKILL.md`; each is fetched raw from a **pinned
/// commit SHA** (TOCTOU defence — the tree we index and the blob we install are
/// the same immutable commit). Trust floor is `Official`.
#[derive(Debug, Default)]
pub struct AnthropicSkillsHub;

const ANTHROPIC_OWNER_REPO: &str = "anthropics/skills";
/// Pinned commit SHA (captured 2026-07-27). Indexing + install both resolve
/// against this immutable ref; bump deliberately, never float to a branch.
const ANTHROPIC_PIN_SHA: &str = "b29e7cf65e5cb78a5ac33d582270551bc74a14eb";

/// Parse a GitHub git-trees (recursive) body into official skill entries.
/// A skill is any blob at `skills/<name>/SKILL.md`; `name` is the middle
/// segment. Pure — unit-tested against a captured tree.
pub fn parse_anthropic_tree(body: &serde_json::Value) -> Vec<SkillIndexEntry> {
    let tree = body["tree"].as_array().cloned().unwrap_or_default();
    tree.iter()
        .filter_map(|node| {
            if node["type"].as_str() != Some("blob") {
                return None;
            }
            let path = node["path"].as_str()?;
            // Exactly `skills/<name>/SKILL.md` (two-segment prefix, no deeper).
            let rest = path.strip_prefix("skills/")?;
            let name = rest.strip_suffix("/SKILL.md")?;
            if name.is_empty() || name.contains('/') {
                return None;
            }
            Some(SkillIndexEntry {
                name: name.to_string(),
                description: format!("Official anthropics/skills skill: {name}"),
                tags: vec!["official".to_string(), "anthropic".to_string()],
                author: "anthropics".to_string(),
                url: format!("https://github.com/{ANTHROPIC_OWNER_REPO}/tree/{ANTHROPIC_PIN_SHA}/skills/{name}"),
                compatible: vec!["claude-code".to_string()],
                pushed_at: Some(Utc::now().to_rfc3339()),
                owner_type: Some("Organization".to_string()),
                stars: 0,
                trust_tier: crate::trust_tier::TrustTier::Official,
                install_count: 0,
                source_url: Some(format!(
                    "https://github.com/{ANTHROPIC_OWNER_REPO}/tree/main/skills/{name}"
                )),
                source_verdict: Some("clean".to_string()),
            })
        })
        .collect()
}

async fn anthropic_fetch_tree() -> Result<Vec<SkillIndexEntry>, String> {
    let http = http_client()?;
    let url = format!(
        "https://api.github.com/repos/{ANTHROPIC_OWNER_REPO}/git/trees/{ANTHROPIC_PIN_SHA}?recursive=1"
    );
    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("anthropic-skills tree request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("anthropic-skills tree API returned {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("anthropic-skills tree JSON: {e}"))?;
    Ok(parse_anthropic_tree(&body))
}

impl SkillHub for AnthropicSkillsHub {
    fn id(&self) -> &str {
        HUB_ANTHROPIC
    }

    fn verified(&self) -> bool {
        true
    }

    fn trust_floor(&self) -> crate::trust_tier::TrustTier {
        crate::trust_tier::TrustTier::Official
    }

    fn search<'a>(
        &'a self,
        home_dir: &'a Path,
        query: &'a str,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let index = cached_index(home_dir, HUB_ANTHROPIC, anthropic_fetch_tree).await?;
            Ok(index.search(query, limit).into_iter().cloned().collect())
        })
    }

    fn list<'a>(
        &'a self,
        home_dir: &'a Path,
        limit: usize,
    ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
        Box::pin(async move {
            let index = cached_index(home_dir, HUB_ANTHROPIC, anthropic_fetch_tree).await?;
            Ok(index.skills.into_iter().take(limit).collect())
        })
    }

    fn fetch_manifest<'a>(
        &'a self,
        _home_dir: &'a Path,
        name: &'a str,
    ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
        Box::pin(async move {
            // `name` must be a safe single path segment (the MCP handler
            // validates; re-check here as this string lands in a URL path).
            if name.is_empty() || name.contains('/') || name.contains("..") {
                return Err(format!("invalid anthropic-skills skill name '{name}'"));
            }
            let http = http_client()?;
            // Raw fetch pinned to the same immutable commit as the index.
            let url = format!(
                "https://raw.githubusercontent.com/{ANTHROPIC_OWNER_REPO}/{ANTHROPIC_PIN_SHA}/skills/{name}/SKILL.md"
            );
            let resp = http
                .get(&url)
                .send()
                .await
                .map_err(|e| format!("anthropic-skills raw request: {e}"))?;
            if resp.status() == reqwest::StatusCode::NOT_FOUND {
                return Ok(None);
            }
            if !resp.status().is_success() {
                return Err(format!("anthropic-skills raw returned {}", resp.status()));
            }
            let text = resp
                .text()
                .await
                .map_err(|e| format!("anthropic-skills raw body: {e}"))?;
            Ok(Some(
                HubManifest {
                    hub: HUB_ANTHROPIC.to_string(),
                    name: name.to_string(),
                    content: Some(text),
                    url: format!(
                        "https://github.com/{ANTHROPIC_OWNER_REPO}/blob/{ANTHROPIC_PIN_SHA}/skills/{name}/SKILL.md"
                    ),
                    content_hash: None,
                    expected_hash: None,
                    source_verdict: Some("clean".to_string()),
                    trust_floor: crate::trust_tier::TrustTier::Official,
                }
                .with_computed_hash(),
            ))
        })
    }
}

// ── Registry / aggregator ───────────────────────────────────

/// One aggregated search hit, labeled with its source hub.
#[derive(Debug, Clone)]
pub struct HubHit {
    pub hub: String,
    /// Raw relevance (term match) score.
    pub score: usize,
    /// Composite WP2.6 rank (`relevance × trust × installs × freshness`).
    pub rank: f64,
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
        HUB_ANTHROPIC => Some(Box::new(AnthropicSkillsHub)),
        _ => None,
    }
}

// ── Ranking (WP2.6 §1) ──────────────────────────────────────

/// Number of top slots reserved for `Official`-tier hits (`官方 tier 保底前三`).
pub const OFFICIAL_FLOOR_SLOTS: usize = 3;

/// Freshness multiplier from an entry's last-activity timestamp: `1.0` while
/// fresh (≤ [`crate::trust_tier::FRESH_MONTHS`]), decaying linearly to a `0.5`
/// floor by 24 months; unknown dates are a neutral `0.8` (never a hard demote).
pub fn freshness_factor(pushed_at: Option<&str>, now: chrono::DateTime<Utc>) -> f64 {
    const FLOOR: f64 = 0.5;
    const DECAY_END_MONTHS: f64 = 24.0;
    let Some(months) = pushed_at
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|d| (now - d.with_timezone(&Utc)).num_seconds() as f64 / 86_400.0 / 30.44)
    else {
        return 0.8;
    };
    if months <= crate::trust_tier::FRESH_MONTHS {
        1.0
    } else if months >= DECAY_END_MONTHS {
        FLOOR
    } else {
        let span = DECAY_END_MONTHS - crate::trust_tier::FRESH_MONTHS;
        1.0 - (1.0 - FLOOR) * ((months - crate::trust_tier::FRESH_MONTHS) / span)
    }
}

/// The WP2.6 rank score:
/// `relevance × trust_coeff × (1 + ln(1 + installs)) × freshness`.
///
/// The install term is `1 + ln(1+installs)` rather than the bare
/// `log(1+installs)` from the design sketch so that a zero-install skill keeps
/// its full relevance × trust × freshness score instead of collapsing to 0.
pub fn rank_score(entry: &SkillIndexEntry, relevance: usize, now: chrono::DateTime<Utc>) -> f64 {
    let trust = entry.trust_tier.rank_coefficient();
    let install = 1.0 + (1.0 + entry.install_count as f64).ln();
    let fresh = freshness_factor(entry.pushed_at.as_deref(), now);
    relevance as f64 * trust * install * fresh
}

/// Dedup key for cross-source merge: the canonical upstream URL when present
/// (so the same skill mirrored on two hubs collapses to one), else the name.
fn dedup_key(entry: &SkillIndexEntry) -> String {
    entry
        .source_url
        .as_deref()
        .map(|u| u.trim_end_matches('/').to_ascii_lowercase())
        .unwrap_or_else(|| entry.name.to_ascii_lowercase())
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
    /// hubs (`only = None`).
    ///
    /// WP2.6 semantics:
    /// - **Per-source 3s deadline** ([`PER_SOURCE_TIMEOUT`]): a slow hub is
    ///   reported `[unreachable: <hub>: timeout]` and skipped, never blocking.
    /// - **Fail-honest**: every failing/timed-out hub lands in `errors`.
    /// - **Cross-source dedup** by canonical `source_url` (else name): the
    ///   surviving entry keeps the higher relevance, the **max** install count,
    ///   and the stronger trust tier / verdict across the duplicates.
    /// - **Rank** = `relevance × trust × (1+ln(1+installs)) × freshness`.
    /// - **Official floor**: `Official`-tier hits are guaranteed the top
    ///   [`OFFICIAL_FLOOR_SLOTS`] slots when any exist.
    pub async fn search(
        &self,
        home_dir: &Path,
        query: &str,
        limit: usize,
        only: Option<&str>,
    ) -> AggregatedSearch {
        let lower = query.to_lowercase();
        let terms: Vec<&str> = lower.split_whitespace().collect();
        let now = Utc::now();
        let mut out = AggregatedSearch::default();
        // key → index into out.hits, for O(1) cross-source dedup.
        let mut by_key: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for hub in &self.hubs {
            if let Some(want) = only {
                if hub.id() != want {
                    continue;
                }
            }
            let fetched =
                match tokio::time::timeout(PER_SOURCE_TIMEOUT, hub.search(home_dir, query, limit))
                    .await
                {
                    Ok(Ok(entries)) => entries,
                    Ok(Err(e)) => {
                        out.errors.push((hub.id().to_string(), e));
                        continue;
                    }
                    Err(_) => {
                        out.errors.push((
                            hub.id().to_string(),
                            format!("timeout after {}s", PER_SOURCE_TIMEOUT.as_secs()),
                        ));
                        continue;
                    }
                };

            for entry in fetched {
                let score = score_match(&entry, &terms);
                if score == 0 {
                    continue;
                }
                let key = dedup_key(&entry);
                if let Some(&idx) = by_key.get(&key) {
                    merge_duplicate(&mut out.hits[idx], hub.id(), score, entry, now);
                    continue;
                }
                let rank = rank_score(&entry, score, now);
                by_key.insert(key, out.hits.len());
                out.hits.push(HubHit {
                    hub: hub.id().to_string(),
                    score,
                    rank,
                    entry,
                });
            }
        }

        // Primary order: composite rank desc, name tie-break.
        out.hits.sort_by(|a, b| {
            b.rank
                .partial_cmp(&a.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entry.name.cmp(&b.entry.name))
        });
        promote_official_floor(&mut out.hits);
        out.hits.truncate(limit);
        out
    }
}

/// Merge a duplicate hit into the one already kept for its dedup key: adopt the
/// higher-relevance presentation, take the **max** install count, and keep the
/// stronger trust tier + any non-clean verdict (so a suspicious mirror can't be
/// laundered by a clean one).
fn merge_duplicate(
    kept: &mut HubHit,
    hub_id: &str,
    score: usize,
    entry: SkillIndexEntry,
    now: chrono::DateTime<Utc>,
) {
    let max_installs = kept.entry.install_count.max(entry.install_count);
    let stronger_tier = min_tier(kept.entry.trust_tier, entry.trust_tier);
    // Blocking verdict on either side wins (fail-closed toward suspicion).
    let verdict = pick_worse_verdict(
        kept.entry.source_verdict.as_deref(),
        entry.source_verdict.as_deref(),
    );
    if score > kept.score {
        *kept = HubHit {
            hub: hub_id.to_string(),
            score,
            rank: 0.0,
            entry,
        };
    }
    kept.entry.install_count = max_installs;
    kept.entry.trust_tier = stronger_tier;
    kept.entry.source_verdict = verdict;
    kept.rank = rank_score(&kept.entry, kept.score, now);
}

/// The stronger of two trust tiers (Official > Active > Orphan).
fn min_tier(
    a: crate::trust_tier::TrustTier,
    b: crate::trust_tier::TrustTier,
) -> crate::trust_tier::TrustTier {
    use crate::trust_tier::TrustTier::*;
    match (a, b) {
        (Official, _) | (_, Official) => Official,
        (Active, _) | (_, Active) => Active,
        _ => Orphan,
    }
}

/// Prefer a blocking verdict over a clean/absent one (fail-closed).
fn pick_worse_verdict(a: Option<&str>, b: Option<&str>) -> Option<String> {
    for v in [a, b] {
        if verdict_is_blocking(v) {
            return v.map(|s| s.to_string());
        }
    }
    a.or(b).map(|s| s.to_string())
}

/// Guarantee `Official`-tier hits occupy the first [`OFFICIAL_FLOOR_SLOTS`]
/// slots (`官方 tier 保底前三`), preserving relative order otherwise. A stable
/// partial promotion: pull the highest-ranked officials to the front until the
/// floor is filled or officials run out.
pub fn promote_official_floor(hits: &mut [HubHit]) {
    let mut insert_at = 0usize;
    for i in 0..hits.len() {
        if insert_at >= OFFICIAL_FLOOR_SLOTS {
            break;
        }
        if hits[i].entry.trust_tier == crate::trust_tier::TrustTier::Official {
            hits[insert_at..=i].rotate_right(1);
            insert_at += 1;
        }
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
            install_count: 0,
            source_url: None,
            source_verdict: None,
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
                        content_hash: None,
                        expected_hash: None,
                        source_verdict: None,
                        trust_floor: crate::trust_tier::TrustTier::Active,
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
    fn defaults_are_the_wp26_p0_set() {
        let reg = HubRegistry::default_hubs();
        let ids = reg.ids();
        for id in [HUB_ANTHROPIC, HUB_GITHUB, HUB_CLAWHUB, HUB_LOBEHUB, HUB_SKILLS_SH] {
            assert!(ids.contains(&id), "default set must include {id}");
        }
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

    #[test]
    fn clawhub_search_mapping_from_captured_live_payload() {
        // Shape captured live 2026-07-27 from GET clawhub.ai/api/v1/search.
        let body: serde_json::Value = serde_json::json!({
            "results": [{
                "slug": "code",
                "displayName": "Code",
                "summary": "Coding workflow with planning and testing.",
                "official": false,
                "ownerHandle": "ivangdavila",
                "canonicalUrl": "/ivangdavila/skills/code",
                "downloads": 29082,
                "metrics": {"bookmarks": 0, "rolling60DayInstalls": 39, "updatedAt": 1778487899210u64},
                "install": {"kind": "clawhub", "reference": "ivangdavila/code", "sourceUrl": null},
                "trust": {"clawHubVerdict": null, "installability": "installable"},
                "native": {"skill": {
                    "topics": ["Software Development", "Coding"],
                    "isSuspicious": false,
                    "stats": {"stars": 52, "installs": 907}
                }}
            }]
        });
        let entries = parse_clawhub_search(&body);
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.name, "code");
        assert_eq!(e.author, "ivangdavila");
        assert_eq!(e.tags, vec!["software development", "coding"]);
        assert_eq!(e.stars, 52);
        assert_eq!(e.install_count, 39, "60-day install signal");
        assert_eq!(e.url, "https://clawhub.ai/ivangdavila/skills/code");
        assert_eq!(e.source_verdict.as_deref(), Some("clean"), "installable → clean");
        assert!(e.pushed_at.is_some());
    }

    #[test]
    fn clawhub_search_flags_suspicious() {
        let body = serde_json::json!({
            "results": [{
                "slug": "sketchy", "displayName": "Sketchy", "summary": "x",
                "ownerHandle": "who", "canonicalUrl": "/who/skills/sketchy",
                "metrics": {"rolling60DayInstalls": 1},
                "trust": {"clawHubVerdict": "suspicious"},
                "native": {"skill": {"isSuspicious": true, "stats": {"stars": 0}}}
            }]
        });
        let e = &parse_clawhub_search(&body)[0];
        assert_eq!(e.source_verdict.as_deref(), Some("suspicious"));
    }

    #[test]
    fn skills_sh_mapping_and_fail_closed() {
        // Shape captured live 2026-07-27 from GET www.skills.sh/api/search.
        let body = serde_json::json!({
            "query": "code", "searchType": "fuzzy",
            "skills": [
                {"id": "mattpocock/skills/code-review", "skillId": "code-review",
                 "name": "code-review", "installs": 186935, "source": "mattpocock/skills"}
            ]
        });
        let entries = parse_skills_sh(&body).unwrap();
        assert_eq!(entries.len(), 1);
        let e = &entries[0];
        assert_eq!(e.name, "code-review");
        assert_eq!(e.install_count, 186935);
        assert_eq!(e.author, "mattpocock");
        assert_eq!(e.source_url.as_deref(), Some("https://github.com/mattpocock/skills"));

        // Interface drift ⇒ Err (disabled + reported), never a fabricated empty.
        let drift = serde_json::json!({"unexpected": []});
        assert!(parse_skills_sh(&drift).is_err());
    }

    #[test]
    fn anthropic_tree_maps_official_skills_only() {
        let body = serde_json::json!({
            "tree": [
                {"path": "skills/pdf/SKILL.md", "type": "blob", "sha": "d3e0"},
                {"path": "skills/xlsx/SKILL.md", "type": "blob", "sha": "9da5"},
                {"path": "template/SKILL.md", "type": "blob", "sha": "50a4"},      // not under skills/
                {"path": "skills/pdf/reference/deep/SKILL.md", "type": "blob", "sha": "x"}, // nested
                {"path": "skills/pdf", "type": "tree", "sha": "y"}                   // dir, not blob
            ]
        });
        let entries = parse_anthropic_tree(&body);
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["pdf", "xlsx"], "only skills/<name>/SKILL.md blobs");
        assert!(entries.iter().all(|e| e.trust_tier == crate::trust_tier::TrustTier::Official));
        assert!(entries.iter().all(|e| e.source_verdict.as_deref() == Some("clean")));
    }

    #[test]
    fn content_hash_roundtrip_and_verify() {
        let content = "---\nname: x\n---\n# X";
        let h = sha256_hex(content);
        assert_eq!(h.len(), 64);
        assert!(verify_content_hash(content, None).is_ok(), "no expected ⇒ ok");
        assert!(verify_content_hash(content, Some(&h)).is_ok());
        assert!(verify_content_hash(content, Some(&h.to_uppercase())).is_ok(), "case-insensitive");
        assert!(verify_content_hash(content, Some("deadbeef")).is_err(), "mismatch ⇒ DENY");
    }

    #[test]
    fn verdict_normalization_and_blocking() {
        assert_eq!(normalize_verdict(Some("installable")).as_deref(), Some("clean"));
        assert_eq!(normalize_verdict(Some("SUSPICIOUS")).as_deref(), Some("suspicious"));
        assert_eq!(normalize_verdict(Some("malicious")).as_deref(), Some("malicious"));
        assert_eq!(normalize_verdict(Some("weird-status")).as_deref(), Some("unknown"));
        assert_eq!(normalize_verdict(None), None);
        assert!(verdict_is_blocking(Some("suspicious")));
        assert!(verdict_is_blocking(Some("malicious")));
        assert!(!verdict_is_blocking(Some("clean")));
        assert!(!verdict_is_blocking(None));
    }

    #[test]
    fn rank_rewards_installs_trust_and_freshness() {
        let now = Utc::now();
        let mut base = entry("s", "browser helper", &[]);
        let low = rank_score(&base, 5, now);
        base.install_count = 10_000;
        let high_installs = rank_score(&base, 5, now);
        assert!(high_installs > low, "more installs ⇒ higher rank");

        // Official tier beats active at equal relevance/installs.
        let mut official = entry("o", "x", &[]);
        official.trust_tier = crate::trust_tier::TrustTier::Official;
        let mut orphan = entry("p", "x", &[]);
        orphan.trust_tier = crate::trust_tier::TrustTier::Orphan;
        assert!(rank_score(&official, 5, now) > rank_score(&orphan, 5, now));

        // Zero installs must not zero the score (design-sketch fix).
        assert!(rank_score(&entry("z", "browser", &[]), 5, now) > 0.0);
    }

    #[test]
    fn freshness_decays_with_age() {
        let now = Utc::now();
        let fresh = (now - chrono::Duration::days(30)).to_rfc3339();
        let stale = (now - chrono::Duration::days(800)).to_rfc3339();
        assert_eq!(freshness_factor(Some(&fresh), now), 1.0);
        assert_eq!(freshness_factor(Some(&stale), now), 0.5, "floors at 0.5");
        assert_eq!(freshness_factor(None, now), 0.8, "unknown is neutral");
        assert!(freshness_factor(Some("garbage"), now) == 0.8);
    }

    #[test]
    fn official_floor_promotes_to_top_three() {
        let mk = |name: &str, tier| {
            let mut e = entry(name, "x", &[]);
            e.trust_tier = tier;
            HubHit { hub: "h".into(), score: 1, rank: 1.0, entry: e }
        };
        use crate::trust_tier::TrustTier::*;
        // An official buried at the bottom must surface into the top 3.
        let mut hits = vec![
            mk("a", Active),
            mk("b", Active),
            mk("c", Active),
            mk("d", Active),
            mk("off", Official),
        ];
        promote_official_floor(&mut hits);
        let top3: Vec<&str> = hits[..3].iter().map(|h| h.entry.name.as_str()).collect();
        assert!(top3.contains(&"off"), "official must be in top 3: {top3:?}");
    }

    #[tokio::test]
    async fn source_url_dedup_takes_max_installs_and_worse_verdict() {
        // Two hubs surface the same upstream repo — must collapse to one hit.
        let mut a = entry("dup", "browser tool", &[]);
        a.source_url = Some("https://github.com/owner/repo".into());
        a.install_count = 5;
        a.source_verdict = Some("clean".into());
        let mut b = entry("dup", "browser tool", &[]);
        b.source_url = Some("https://github.com/owner/repo/".into()); // trailing slash
        b.install_count = 999;
        b.source_verdict = Some("suspicious".into());

        let hub_a = MockHub { id: "a", entries: vec![a], fail: false };
        let hub_b = MockHub { id: "b", entries: vec![b], fail: false };
        let reg = HubRegistry::with_hubs(vec![Box::new(hub_a), Box::new(hub_b)]);
        let res = reg.search(Path::new("/nonexistent"), "browser", 10, None).await;
        assert_eq!(res.hits.len(), 1, "same source_url ⇒ one hit");
        assert_eq!(res.hits[0].entry.install_count, 999, "max installs kept");
        assert_eq!(
            res.hits[0].entry.source_verdict.as_deref(),
            Some("suspicious"),
            "worse verdict wins (fail-closed)"
        );
    }

    #[tokio::test]
    async fn slow_hub_times_out_and_is_reported() {
        struct SlowHub;
        impl SkillHub for SlowHub {
            fn id(&self) -> &str { "slow" }
            fn verified(&self) -> bool { true }
            fn search<'a>(
                &'a self, _h: &'a Path, _q: &'a str, _l: usize,
            ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
                Box::pin(async move {
                    tokio::time::sleep(PER_SOURCE_TIMEOUT + std::time::Duration::from_secs(2)).await;
                    Ok(vec![])
                })
            }
            fn list<'a>(
                &'a self, _h: &'a Path, _l: usize,
            ) -> HubFuture<'a, Result<Vec<SkillIndexEntry>, String>> {
                Box::pin(async move { Ok(vec![]) })
            }
            fn fetch_manifest<'a>(
                &'a self, _h: &'a Path, _n: &'a str,
            ) -> HubFuture<'a, Result<Option<HubManifest>, String>> {
                Box::pin(async move { Ok(None) })
            }
        }
        let fast = MockHub { id: "fast", entries: vec![entry("s1", "browser", &[])], fail: false };
        let reg = HubRegistry::with_hubs(vec![Box::new(fast), Box::new(SlowHub)]);
        let res =
            tokio::time::timeout(std::time::Duration::from_secs(6), reg.search(Path::new("/x"), "browser", 10, None))
                .await
                .expect("aggregate must not hang past the per-source deadline");
        assert_eq!(res.hits.len(), 1, "fast hub still returns");
        assert!(res.errors.iter().any(|(h, e)| h == "slow" && e.contains("timeout")));
    }

    /// Real-network smoke: one live ClawHub search. Skips (not fails) when the
    /// network is unavailable — CI stays green offline.
    #[tokio::test]
    async fn smoke_clawhub_search_live() {
        match clawhub_search("code", 5).await {
            Ok(hits) => {
                // Live endpoint reachable — sanity-check the mapping holds.
                if let Some(h) = hits.first() {
                    assert!(!h.name.is_empty());
                    assert!(h.url.starts_with("https://clawhub.ai/"));
                }
            }
            Err(e) => {
                eprintln!("[smoke skipped] clawhub unreachable: {e}");
            }
        }
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
