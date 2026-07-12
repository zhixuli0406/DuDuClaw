//! White-label branding configuration for the distributor portal.
//!
//! A distributor whose license carries the `white_label` feature (tier = Oem)
//! may override the dashboard's product name, logo, and support/contact fields.
//! The **upstream vendor attribution** ("嘟嘟數位科技有限公司") is NOT part of
//! this config — it is assembled into every response from the Rust constants
//! below, so it can never be blanked out by editing `branding.json` or by a
//! crafted `branding.set` payload.
//!
//! Persistence: `~/.duduclaw/branding.json` (non-secret, atomic tmp+rename).
//! Validation is fail-closed: any field that does not pass its check rejects
//! the whole `set` with a zh-TW error, and the logo is validated by magic
//! bytes (not just the declared MIME) with a 512 KB decoded ceiling. SVG is
//! rejected outright (it is an active-content / XSS vector).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use duduclaw_license::PublicKeyRegistry;

// ── Immutable upstream vendor attribution (never from config) ─────────────

/// Legal name of the software vendor, shown on the About page. Compiled in;
/// never read from config and never writable via any RPC.
pub const UPSTREAM_VENDOR_ZH: &str = "嘟嘟數位科技有限公司";
/// English legal name of the software vendor.
pub const UPSTREAM_VENDOR_EN: &str = "DuDu Digital Technology Co., Ltd.";
/// Canonical upstream product URL.
pub const UPSTREAM_VENDOR_URL: &str = "https://duduclaw.dudustudio.monster";

/// Default product name when no white-label override is installed.
pub const DEFAULT_PRODUCT_NAME: &str = "DuDuClaw";

// ── Validation limits (codepoint counts, not bytes) ───────────────────────

const MAX_PRODUCT_NAME_CHARS: usize = 60;
const MAX_SUBTITLE_CHARS: usize = 120;
const MAX_COMPANY_NAME_CHARS: usize = 120;
const MAX_WEBSITE_CHARS: usize = 300;
const MAX_SUPPORT_EMAIL_CHARS: usize = 200;
const MAX_DESCRIPTION_CHARS: usize = 2000;

/// Maximum raw `about_html` size (64 KB, bytes). Over-limit is **rejected
/// outright** (not truncated — truncating HTML mid-tag is meaningless), §10.2.
const MAX_ABOUT_HTML_BYTES: usize = 64 * 1024;

/// Maximum decoded logo size (512 KB). The dashboard CSP is `img-src 'self'
/// data:`, so the logo travels inline as a data URI; this ceiling keeps the
/// system prompt / config from ballooning.
const MAX_LOGO_DECODED_BYTES: usize = 512 * 1024;

/// Accepted logo data-URI prefixes. **SVG is deliberately excluded** — it can
/// carry `<script>` and is an XSS vector even inside `<img>` on some engines.
const LOGO_PREFIXES: &[(&str, ImageKind)] = &[
    ("data:image/png;base64,", ImageKind::Png),
    ("data:image/jpeg;base64,", ImageKind::Jpeg),
    ("data:image/webp;base64,", ImageKind::Webp),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageKind {
    Png,
    Jpeg,
    Webp,
}

// ── Types ─────────────────────────────────────────────────────────────────

// The persisted / returned branding record + the signed-bundle format now live
// in the shared OSS `duduclaw-license` crate (`bundle` module) so the gateway
// verifier and every signer (gateway owner-offline + cloud control-plane)
// compute byte-identical canonical bytes. `validate_input` / `sanitize_*` /
// persistence / the vendor block stay here (policy, not format). See
// `duduclaw_license::bundle` for the wire format and its stability contract.
pub use duduclaw_license::BrandingConfig;

/// The exact set of writable fields accepted by `branding.set`.
///
/// `deny_unknown_fields` is the second line of defence behind the explicit
/// field list: a payload trying to smuggle a `vendor` / `updated_at` override
/// is rejected at deserialization rather than silently ignored.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrandingInput {
    #[serde(default)]
    pub product_name: Option<String>,
    #[serde(default)]
    pub subtitle: Option<String>,
    #[serde(default)]
    pub logo_data_uri: Option<String>,
    #[serde(default)]
    pub company_name: Option<String>,
    #[serde(default)]
    pub website: Option<String>,
    #[serde(default)]
    pub support_email: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub about_html: Option<String>,
    #[serde(default)]
    pub accent_color: Option<String>,
}

impl From<BrandingConfig> for BrandingInput {
    /// Project a stored/received config back onto the writable input surface
    /// (dropping server-stamped `updated_at`). Used when the owner re-validates
    /// a distributor's submitted branding before signing a bundle (§10.3): the
    /// owner is the authoritative sanitizer, so the incoming config is run
    /// through `validate_input` exactly like a fresh `branding.set`.
    fn from(c: BrandingConfig) -> Self {
        Self {
            product_name: c.product_name,
            subtitle: c.subtitle,
            logo_data_uri: c.logo_data_uri,
            company_name: c.company_name,
            website: c.website,
            support_email: c.support_email,
            description: c.description,
            about_html: c.about_html,
            accent_color: c.accent_color,
        }
    }
}

/// Const-assembled vendor block returned to the UI. Never sourced from config.
#[derive(Debug, Clone, Serialize)]
pub struct VendorBlock {
    pub name_zh: &'static str,
    pub name_en: &'static str,
    pub url: &'static str,
}

impl VendorBlock {
    /// The single source of truth for the vendor attribution.
    pub const fn upstream() -> Self {
        Self {
            name_zh: UPSTREAM_VENDOR_ZH,
            name_en: UPSTREAM_VENDOR_EN,
            url: UPSTREAM_VENDOR_URL,
        }
    }
}

// ── Persistence ───────────────────────────────────────────────────────────

/// Path to the branding file under a home dir.
fn branding_path(home_dir: &Path) -> PathBuf {
    home_dir.join("branding.json")
}

/// Canonical file name of the signed branding bundle, shared by the home-dir
/// persistence path and the first-run seed candidate discovery (§11.2).
const BUNDLE_FILENAME: &str = "branding.bundle.json";

/// Path to the signed branding bundle under a home dir (§10.1). Applied
/// automatically when present and its signature verifies — no license needed.
fn bundle_path(home_dir: &Path) -> PathBuf {
    home_dir.join(BUNDLE_FILENAME)
}

/// Where a loaded branding config came from. Surfaced to the dashboard as the
/// top-level `source` field so the UI can explain why a brand is active.
pub const SOURCE_LOCAL: &str = "local";
pub const SOURCE_BUNDLE: &str = "bundle";
pub const SOURCE_DEFAULT: &str = "default";

/// Load the branding config, returning an all-default config when neither a
/// local file nor a valid bundle is present. Never errors — a broken branding
/// file must not break the dashboard; the worst case is "no override applied".
///
/// Convenience wrapper over [`load_with_source`] for the many callers that only
/// need the config.
pub fn load(home_dir: &Path) -> BrandingConfig {
    load_with_source(home_dir).0
}

/// Load branding with its provenance (§10.1 resolution order):
///   1. local `branding.json` (RPC-set, license-gated) → `SOURCE_LOCAL`
///   2. signature-verified `branding.bundle.json` → `SOURCE_BUNDLE`
///   3. built-in DuDuClaw defaults → `SOURCE_DEFAULT`
///
/// Every non-default config is re-sanitized on read (`sanitize_loaded`) so a
/// hand-edited `branding.json` or a tampered bundle body cannot smuggle unsafe
/// HTML/colour past the write-time checks. An invalid bundle is ignored with a
/// single `warn!` (fail-closed → defaults), never a panic.
pub fn load_with_source(home_dir: &Path) -> (BrandingConfig, &'static str) {
    load_with_source_using(home_dir, &crate::license_runtime::production_registry())
}

/// Registry-injectable core of [`load_with_source`] (the public entry pins the
/// baked production registry). Split out so the resolution order can be unit
/// tested with a throwaway issuer key.
fn load_with_source_using(
    home_dir: &Path,
    registry: &PublicKeyRegistry,
) -> (BrandingConfig, &'static str) {
    // 1) Local file wins.
    let local_path = branding_path(home_dir);
    if let Ok(raw) = std::fs::read_to_string(&local_path) {
        if let Ok(cfg) = serde_json::from_str::<BrandingConfig>(&raw) {
            return (sanitize_loaded(cfg), SOURCE_LOCAL);
        }
        // A corrupt local file: fall through to bundle/default rather than error.
    }

    // 2) Signature-verified bundle.
    let bpath = bundle_path(home_dir);
    if let Ok(raw) = std::fs::read_to_string(&bpath) {
        match serde_json::from_str::<BrandingBundle>(&raw) {
            Ok(bundle) => match verify_bundle(&bundle, registry) {
                Ok(()) => return (sanitize_loaded(bundle.branding), SOURCE_BUNDLE),
                Err(e) => {
                    warn!(
                        error = %e,
                        "ignoring branding.bundle.json — signature/verification failed (running default branding)"
                    );
                }
            },
            Err(e) => {
                warn!(error = %e, "ignoring branding.bundle.json — malformed");
            }
        }
    }

    // 3) Default.
    (BrandingConfig::default(), SOURCE_DEFAULT)
}

/// Re-apply the write-time sanitizers to a config read from disk / a bundle.
/// Fail-*safe* (not fail-closed) — a field that no longer passes is dropped to
/// `None` rather than erroring the whole load, because a broken branding record
/// must never break the dashboard. The `set` path is the fail-closed gate.
fn sanitize_loaded(mut cfg: BrandingConfig) -> BrandingConfig {
    cfg.about_html = cfg
        .about_html
        .and_then(|raw| sanitize_about_html(&raw).ok().flatten());
    cfg.accent_color = cfg
        .accent_color
        .and_then(|c| validate_accent_color(&c).ok());
    cfg
}

/// Persist the branding config atomically (tmp + rename). Non-secret, so mode
/// is left at the default umask (unlike license/db files).
pub fn save(home_dir: &Path, cfg: &BrandingConfig) -> Result<(), String> {
    std::fs::create_dir_all(home_dir).map_err(|e| format!("建立設定目錄失敗：{e}"))?;
    let path = branding_path(home_dir);
    let json = serde_json::to_vec_pretty(cfg).map_err(|e| format!("序列化品牌設定失敗：{e}"))?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("開啟暫存檔失敗：{e}"))?;
        f.write_all(&json)
            .map_err(|e| format!("寫入暫存檔失敗：{e}"))?;
        f.sync_all().map_err(|e| format!("同步暫存檔失敗：{e}"))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("原子換名失敗：{e}"))?;
    Ok(())
}

/// Delete the branding file (revert to DuDuClaw defaults). Absent file is a
/// no-op success.
pub fn reset(home_dir: &Path) -> Result<(), String> {
    let path = branding_path(home_dir);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("刪除品牌設定失敗：{e}")),
    }
}

// ── First-run bundle seeding (§11.2) ──────────────────────────────────────
//
// When the `duduclaw` binary ships co-located with a signed
// `branding.bundle.json` (env override / executable sibling / macOS `.app`
// Resources), the first boot verifies that bundle and copies it into
// `~/.duduclaw/branding.bundle.json` so `load_with_source` picks it up. This is
// what makes "binary + bundle in the same folder" self-install a distributor's
// brand on every platform without a rebuild. It is:
//   - **idempotent** — an existing home-dir bundle is never overwritten (the
//     user's later customisation wins);
//   - **fail-closed** — a candidate that is unreadable, malformed, or fails the
//     Ed25519 signature check is warned once and NOT seeded (no default fallback
//     write). The private issuer key is never involved; bundle contents are
//     never logged.

/// Outcome of [`seed_bundle_if_absent`], surfaced for unit-test assertions and
/// caller logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeedOutcome {
    /// A candidate bundle verified and was atomically copied into the home dir.
    Seeded { source: PathBuf },
    /// `branding.bundle.json` already existed in the home dir — left untouched.
    AlreadyPresent,
    /// No candidate source file was found on any known path.
    NoCandidate,
    /// The first existing candidate failed verification (or could not be read /
    /// parsed / persisted) — nothing was seeded.
    VerifyFailed { source: PathBuf },
}

/// Candidate seed sources, in precedence order (§11.2 step 2):
///   1. `DUDUCLAW_BRANDING_BUNDLE` env var → explicit file path
///   2. `branding.bundle.json` next to the running executable
///   3. macOS `.app/Contents/Resources/branding.bundle.json` — walk up to 4
///      ancestor levels from the executable (the desktop sidecar lives under
///      `.app/Contents/{MacOS,Resources}/binaries/`, so `Resources` is several
///      levels up).
fn seed_candidate_paths() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(p) = std::env::var_os("DUDUCLAW_BRANDING_BUNDLE") {
        candidates.push(PathBuf::from(p));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            candidates.push(dir.join(BUNDLE_FILENAME));
        }
        // Conservative upward search for the macOS App bundle Resources dir.
        for ancestor in exe.ancestors().skip(1).take(4) {
            candidates.push(ancestor.join("Contents").join("Resources").join(BUNDLE_FILENAME));
        }
    }
    candidates
}

/// First-run branding-bundle seeding. See the module note above. Uses the baked
/// production issuer registry (identical to the gateway's `load_with_source`
/// verifier) and the standard candidate paths. Call once at gateway bootstrap,
/// before any `branding::load*`.
pub fn seed_bundle_if_absent(home_dir: &Path) -> SeedOutcome {
    seed_bundle_using(
        home_dir,
        &crate::license_runtime::production_registry(),
        &seed_candidate_paths(),
    )
}

/// Registry- and candidate-injectable core of [`seed_bundle_if_absent`], split
/// out so the precedence / fail-closed behaviour is unit-testable with a
/// throwaway issuer key and explicit candidate paths.
fn seed_bundle_using(
    home_dir: &Path,
    registry: &PublicKeyRegistry,
    candidates: &[PathBuf],
) -> SeedOutcome {
    let dest = bundle_path(home_dir);
    // Idempotent: presence (not validity) of an existing bundle blocks seeding.
    if dest.exists() {
        return SeedOutcome::AlreadyPresent;
    }

    // The FIRST existing candidate is authoritative (§11.2 "找到第一個存在的候選").
    let source = match candidates.iter().find(|p| p.exists()) {
        Some(p) => p.clone(),
        None => return SeedOutcome::NoCandidate,
    };

    let raw = match std::fs::read_to_string(&source) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                source = %source.display(),
                error = %e,
                "branding bundle seed candidate unreadable — not seeding"
            );
            return SeedOutcome::VerifyFailed { source };
        }
    };
    let bundle = match serde_json::from_str::<BrandingBundle>(&raw) {
        Ok(b) => b,
        Err(e) => {
            warn!(
                source = %source.display(),
                error = %e,
                "branding bundle seed candidate malformed — not seeding"
            );
            return SeedOutcome::VerifyFailed { source };
        }
    };
    if let Err(e) = verify_bundle(&bundle, registry) {
        warn!(
            source = %source.display(),
            error = %e,
            "branding bundle seed candidate failed signature verification — not seeding"
        );
        return SeedOutcome::VerifyFailed { source };
    }

    // Verified → atomically install (tmp + rename), mirroring `save`. Persist
    // the verified bytes as-read so the on-disk copy is signature-identical.
    if let Err(e) = atomic_write_bundle(home_dir, &dest, raw.as_bytes()) {
        warn!(
            source = %source.display(),
            error = %e,
            "failed to persist seeded branding bundle — not seeded"
        );
        return SeedOutcome::VerifyFailed { source };
    }
    info!(
        source = %source.display(),
        "seeded branding.bundle.json into home dir on first run"
    );
    SeedOutcome::Seeded { source }
}

/// Atomic write of raw bundle bytes to `dest` (tmp + fsync + rename). Non-secret
/// file, default umask — same discipline as [`save`].
fn atomic_write_bundle(home_dir: &Path, dest: &Path, bytes: &[u8]) -> Result<(), String> {
    std::fs::create_dir_all(home_dir).map_err(|e| format!("建立設定目錄失敗：{e}"))?;
    let tmp = dest.with_extension("json.tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| format!("開啟暫存檔失敗：{e}"))?;
        f.write_all(bytes)
            .map_err(|e| format!("寫入暫存檔失敗：{e}"))?;
        f.sync_all().map_err(|e| format!("同步暫存檔失敗：{e}"))?;
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("原子換名失敗：{e}"))?;
    Ok(())
}

// ── Validation ────────────────────────────────────────────────────────────

/// Validate + normalize a `branding.set` payload into a persistable config.
///
/// Fail-closed: the first failing field rejects the whole update with a
/// zh-TW message. `updated_at` is stamped by this function (never taken from
/// the caller), and the vendor block is not representable here at all.
pub fn validate_input(input: BrandingInput) -> Result<BrandingConfig, String> {
    let product_name = validate_text_field(input.product_name, MAX_PRODUCT_NAME_CHARS, "產品名稱")?;
    let subtitle = validate_text_field(input.subtitle, MAX_SUBTITLE_CHARS, "副標題")?;
    let company_name = validate_text_field(input.company_name, MAX_COMPANY_NAME_CHARS, "公司名稱")?;
    let description = validate_text_field(input.description, MAX_DESCRIPTION_CHARS, "說明文字")?;

    let website = match normalize_optional(input.website) {
        Some(w) => Some(validate_website(&w)?),
        None => None,
    };
    let support_email = match normalize_optional(input.support_email) {
        Some(e) => Some(validate_support_email(&e)?),
        None => None,
    };
    let logo_data_uri = match normalize_optional(input.logo_data_uri) {
        Some(l) => Some(validate_logo(&l)?),
        None => None,
    };

    // about_html: reject over-limit, then sanitize. Empty-after-sanitize → None.
    let about_html = match normalize_optional(input.about_html) {
        Some(raw) => sanitize_about_html(&raw)?,
        None => None,
    };
    let accent_color = match normalize_optional(input.accent_color) {
        Some(c) => Some(validate_accent_color(&c)?),
        None => None,
    };

    Ok(BrandingConfig {
        product_name,
        subtitle,
        logo_data_uri,
        company_name,
        website,
        support_email,
        description,
        about_html,
        accent_color,
        updated_at: Some(chrono::Utc::now().to_rfc3339()),
    })
}

/// Validate an accent colour: strict `#rrggbb` (6 lowercase/uppercase hex
/// digits after `#`). Anything else (named colours, `rgb()`, `#rgb` shorthand,
/// `#rrggbbaa`) is rejected so the value can be interpolated into CSS safely.
pub fn validate_accent_color(c: &str) -> Result<String, String> {
    let bytes = c.as_bytes();
    let ok = bytes.len() == 7
        && bytes[0] == b'#'
        && bytes[1..].iter().all(|b| b.is_ascii_hexdigit());
    if !ok {
        return Err("主題色格式無效（須為 #rrggbb，例如 #f59e0b）".to_string());
    }
    Ok(c.to_ascii_lowercase())
}

/// Sanitize an `about_html` block with a conservative `ammonia` allowlist
/// (§10.2). Returns `Ok(None)` when the sanitized result is empty/whitespace.
///
/// Security posture:
///   - Over-64 KB (raw bytes) → hard reject (fail-closed, no silent truncation).
///   - Allowed tags only; `style`/`class`/`id`/`on*` are dropped (no generic
///     attributes are permitted).
///   - `<a href>` restricted to `http`/`https` and force-stamped
///     `rel="nofollow noopener noreferrer" target="_blank"`.
///   - `<img src>` restricted to `data:image/png|jpeg|webp;base64,…` and run
///     through the SAME magic-byte + 512 KB decode check as the logo — a body
///     that lies about its type or is oversized has its `src` dropped.
pub fn sanitize_about_html(raw: &str) -> Result<Option<String>, String> {
    if raw.len() > MAX_ABOUT_HTML_BYTES {
        return Err(format!(
            "關於區塊 HTML 過大（上限 {} KB）",
            MAX_ABOUT_HTML_BYTES / 1024
        ));
    }

    let mut tags = std::collections::HashSet::new();
    for t in [
        "p",
        "br",
        "h1",
        "h2",
        "h3",
        "h4",
        "ul",
        "ol",
        "li",
        "strong",
        "em",
        "u",
        "s",
        "a",
        "blockquote",
        "code",
        "pre",
        "hr",
        "img",
        "div",
        "span",
    ] {
        tags.insert(t);
    }

    let mut tag_attributes: std::collections::HashMap<&str, std::collections::HashSet<&str>> =
        std::collections::HashMap::new();
    tag_attributes.insert("a", ["href"].into_iter().collect());
    tag_attributes.insert("img", ["src"].into_iter().collect());

    // Force target on links (rel is set separately via link_rel).
    let mut a_attr_values: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    a_attr_values.insert("target", "_blank");
    let mut set_tag_attribute_values: std::collections::HashMap<
        &str,
        std::collections::HashMap<&str, &str>,
    > = std::collections::HashMap::new();
    set_tag_attribute_values.insert("a", a_attr_values);

    let url_schemes = ["http", "https", "data"].into_iter().collect();

    let mut builder = ammonia::Builder::default();
    builder
        .tags(tags)
        .tag_attributes(tag_attributes)
        .set_tag_attribute_values(set_tag_attribute_values)
        .generic_attributes(std::collections::HashSet::new())
        .url_schemes(url_schemes)
        .link_rel(Some("nofollow noopener noreferrer"))
        .strip_comments(true)
        // Per-tag URL policy that scheme allowlisting alone can't express:
        // `a[href]` must be http(s); `img[src]` must be a whitelisted, size- and
        // magic-byte-verified data-image. Returning None drops the attribute.
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("a", "href") => {
                let lower = value.to_ascii_lowercase();
                if lower.starts_with("http://") || lower.starts_with("https://") {
                    Some(std::borrow::Cow::Borrowed(value))
                } else {
                    None
                }
            }
            ("img", "src") => {
                if validate_logo(value).is_ok() {
                    Some(std::borrow::Cow::Borrowed(value))
                } else {
                    None
                }
            }
            _ => Some(std::borrow::Cow::Borrowed(value)),
        });

    let cleaned = builder.clean(raw).to_string();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Collapse an `Option<String>` to `None` when empty/whitespace-only.
fn normalize_optional(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// A plain text field: trimmed, empty → None, length checked by CODEPOINT
/// count (never a byte slice — CJK-safe per project rule #1).
fn validate_text_field(
    v: Option<String>,
    max_chars: usize,
    label: &str,
) -> Result<Option<String>, String> {
    match normalize_optional(v) {
        None => Ok(None),
        Some(s) => {
            if s.chars().count() > max_chars {
                return Err(format!("{label}過長（上限 {max_chars} 字）"));
            }
            Ok(Some(s))
        }
    }
}

/// Website must be an absolute http(s) URL and within the length cap.
fn validate_website(w: &str) -> Result<String, String> {
    if w.chars().count() > MAX_WEBSITE_CHARS {
        return Err(format!("網址過長（上限 {MAX_WEBSITE_CHARS} 字）"));
    }
    let lower = w.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err("網址必須以 http:// 或 https:// 開頭".to_string());
    }
    // Reject whitespace / control chars that could enable header/URL tricks.
    if w.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("網址不得包含空白或控制字元".to_string());
    }
    Ok(w.to_string())
}

/// Basic email shape check (local@domain.tld), no exotic RFC-5322 forms.
fn validate_support_email(e: &str) -> Result<String, String> {
    if e.chars().count() > MAX_SUPPORT_EMAIL_CHARS {
        return Err(format!("客服信箱過長（上限 {MAX_SUPPORT_EMAIL_CHARS} 字）"));
    }
    if e.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err("客服信箱不得包含空白或控制字元".to_string());
    }
    let parts: Vec<&str> = e.split('@').collect();
    let valid = parts.len() == 2
        && !parts[0].is_empty()
        && parts[1].contains('.')
        && !parts[1].starts_with('.')
        && !parts[1].ends_with('.');
    if !valid {
        return Err("客服信箱格式無效".to_string());
    }
    Ok(e.to_string())
}

/// Validate a logo data URI: prefix whitelist (no SVG), base64 decode, size
/// ceiling, and magic-byte confirmation of the declared image kind.
fn validate_logo(uri: &str) -> Result<String, String> {
    let (b64, kind) = LOGO_PREFIXES
        .iter()
        .find_map(|(prefix, kind)| uri.strip_prefix(prefix).map(|rest| (rest, *kind)))
        .ok_or_else(|| {
            "Logo 格式不支援：僅接受 PNG / JPEG / WebP 的 base64 data URI（不接受 SVG）".to_string()
        })?;

    let bytes = BASE64
        .decode(b64.trim())
        .map_err(|_| "Logo base64 解碼失敗".to_string())?;

    if bytes.len() > MAX_LOGO_DECODED_BYTES {
        return Err(format!(
            "Logo 檔案過大（上限 {} KB）",
            MAX_LOGO_DECODED_BYTES / 1024
        ));
    }
    if !magic_matches(&bytes, kind) {
        return Err("Logo 內容與宣告的格式不符（magic bytes 驗證失敗）".to_string());
    }
    Ok(uri.to_string())
}

/// Confirm the decoded bytes carry the signature of the declared image kind.
/// This defeats a payload that declares `image/png` but ships an SVG/HTML body.
fn magic_matches(bytes: &[u8], kind: ImageKind) -> bool {
    match kind {
        ImageKind::Png => {
            bytes.len() >= 8 && bytes[..8] == [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]
        }
        ImageKind::Jpeg => bytes.len() >= 3 && bytes[..3] == [0xFF, 0xD8, 0xFF],
        ImageKind::Webp => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
    }
}

// ── Signed branding bundle (§10.1 / §10.3) ────────────────────────────────
//
// The bundle format + Ed25519 signing/verification moved to the shared OSS
// `duduclaw_license::bundle` module so the cloud control-plane can sign bundles
// that verify byte-identically here. Re-exported so existing call sites
// (`crate::branding::sign_bundle`, `BUNDLE_KEY_ID`, …) are unchanged.
pub use duduclaw_license::{sign_bundle, verify_bundle, BrandingBundle, BUNDLE_KEY_ID, BUNDLE_SCHEMA};

// ── Effective product name (channel white-label, §10.6) ───────────────────

/// Short-lived cache of the resolved product name so per-message channel code
/// doesn't hit the disk on every send. Keyed by the resolved home path.
static PRODUCT_NAME_CACHE: std::sync::LazyLock<
    std::sync::Mutex<Option<(PathBuf, String, std::time::Instant)>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(None));

/// Cache TTL — long enough to spare the disk under message bursts, short enough
/// that a brand change (or a freshly-dropped bundle) shows up within seconds.
const PRODUCT_NAME_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// The user-visible product name for channel copy: the branding override when
/// set (local file or verified bundle), else `DuDuClaw`. Cached with a short
/// TTL. Only user-facing strings should use this — logs/internal identifiers
/// keep the literal `DuDuClaw`.
pub fn effective_product_name(home_dir: &Path) -> String {
    let now = std::time::Instant::now();
    {
        let guard = PRODUCT_NAME_CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((path, name, at)) = guard.as_ref() {
            if path == home_dir && now.duration_since(*at) < PRODUCT_NAME_TTL {
                return name.clone();
            }
        }
    }
    let name = load(home_dir)
        .product_name
        .unwrap_or_else(|| DEFAULT_PRODUCT_NAME.to_string());
    let mut guard = PRODUCT_NAME_CACHE.lock().unwrap_or_else(|e| e.into_inner());
    *guard = Some((home_dir.to_path_buf(), name.clone(), now));
    name
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn png_data_uri() -> String {
        // 8-byte PNG signature + a couple filler bytes.
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&[0u8; 16]);
        format!("data:image/png;base64,{}", BASE64.encode(&bytes))
    }

    #[test]
    fn vendor_block_is_constant() {
        let v = VendorBlock::upstream();
        assert_eq!(v.name_zh, "嘟嘟數位科技有限公司");
        assert_eq!(v.name_en, "DuDu Digital Technology Co., Ltd.");
        assert!(v.url.starts_with("https://"));
    }

    #[test]
    fn set_input_denies_unknown_fields() {
        // A payload attempting to smuggle a vendor override must not deserialize.
        let json = serde_json::json!({
            "product_name": "Acme",
            "vendor": { "name_zh": "偽造" }
        });
        let parsed: Result<BrandingInput, _> = serde_json::from_value(json);
        assert!(parsed.is_err(), "unknown field must be rejected");
    }

    #[test]
    fn validate_accepts_clean_payload() {
        let input = BrandingInput {
            product_name: Some("Acme 智慧助理".to_string()),
            subtitle: Some("你的 AI 團隊".to_string()),
            logo_data_uri: Some(png_data_uri()),
            company_name: Some("Acme 股份有限公司".to_string()),
            website: Some("https://acme.example".to_string()),
            support_email: Some("support@acme.example".to_string()),
            description: Some("白牌部署範例".to_string()),
            ..Default::default()
        };
        let cfg = validate_input(input).expect("clean payload should pass");
        assert_eq!(cfg.product_name.as_deref(), Some("Acme 智慧助理"));
        assert!(
            cfg.updated_at.is_some(),
            "updated_at is stamped by the server"
        );
    }

    #[test]
    fn validate_rejects_svg_logo() {
        let svg = "data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=";
        let input = BrandingInput {
            logo_data_uri: Some(svg.to_string()),
            ..Default::default()
        };
        let err = validate_input(input).unwrap_err();
        assert!(err.contains("SVG"), "SVG must be rejected: {err}");
    }

    #[test]
    fn validate_rejects_mismatched_magic_bytes() {
        // Declares PNG but the decoded body is not a PNG.
        let fake = format!(
            "data:image/png;base64,{}",
            BASE64.encode(b"<html>hi</html>")
        );
        let input = BrandingInput {
            logo_data_uri: Some(fake),
            ..Default::default()
        };
        let err = validate_input(input).unwrap_err();
        assert!(
            err.contains("magic bytes"),
            "magic-byte check must fire: {err}"
        );
    }

    #[test]
    fn validate_rejects_oversized_logo() {
        let mut bytes = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.resize(MAX_LOGO_DECODED_BYTES + 1024, 0);
        let uri = format!("data:image/png;base64,{}", BASE64.encode(&bytes));
        let input = BrandingInput {
            logo_data_uri: Some(uri),
            ..Default::default()
        };
        let err = validate_input(input).unwrap_err();
        assert!(
            err.contains("過大"),
            "oversized logo must be rejected: {err}"
        );
    }

    #[test]
    fn validate_rejects_overlong_product_name_cjk_safe() {
        // 61 CJK codepoints — a byte-slice cap would panic; codepoint count must
        // reject cleanly.
        let name: String = "字".repeat(MAX_PRODUCT_NAME_CHARS + 1);
        let input = BrandingInput {
            product_name: Some(name),
            ..Default::default()
        };
        let err = validate_input(input).unwrap_err();
        assert!(err.contains("產品名稱過長"), "{err}");
    }

    #[test]
    fn validate_rejects_non_http_website() {
        let input = BrandingInput {
            website: Some("javascript:alert(1)".to_string()),
            ..Default::default()
        };
        assert!(validate_input(input).is_err());
    }

    #[test]
    fn validate_rejects_bad_email() {
        let input = BrandingInput {
            support_email: Some("not-an-email".to_string()),
            ..Default::default()
        };
        assert!(validate_input(input).is_err());
    }

    #[test]
    fn save_load_reset_roundtrip() {
        let dir = tempdir().unwrap();
        let cfg = BrandingConfig {
            product_name: Some("Acme".to_string()),
            updated_at: Some("2026-07-11T00:00:00+00:00".to_string()),
            ..Default::default()
        };
        save(dir.path(), &cfg).unwrap();
        let loaded = load(dir.path());
        assert_eq!(loaded.product_name.as_deref(), Some("Acme"));

        reset(dir.path()).unwrap();
        let after = load(dir.path());
        assert!(after.product_name.is_none(), "reset should clear the file");
        // Second reset on a missing file is a no-op success.
        reset(dir.path()).unwrap();
    }

    #[test]
    fn empty_input_is_valid_and_clears_fields() {
        let cfg = validate_input(BrandingInput::default()).unwrap();
        assert!(cfg.product_name.is_none());
        assert!(cfg.logo_data_uri.is_none());
    }

    // ── §10.2 HTML sanitization ─────────────────────────────────

    #[test]
    fn sanitize_strips_script_and_event_handlers_and_style() {
        let raw = r#"<p onclick="steal()">hi</p><script>alert(1)</script><p style="color:red" class="x" id="y">ok</p>"#;
        let out = sanitize_about_html(raw).unwrap().unwrap();
        assert!(!out.contains("<script"), "script tag removed: {out}");
        assert!(!out.contains("onclick"), "event handler removed: {out}");
        assert!(!out.contains("style="), "style attr removed: {out}");
        assert!(!out.contains("class="), "class attr removed: {out}");
        assert!(!out.contains("id="), "id attr removed: {out}");
        assert!(out.contains("ok"));
    }

    #[test]
    fn sanitize_drops_img_onerror() {
        let raw = r#"<img src="x" onerror="alert(1)">"#;
        let out = sanitize_about_html(raw).unwrap();
        let s = out.unwrap_or_default();
        assert!(!s.contains("onerror"), "onerror must be stripped: {s}");
    }

    #[test]
    fn sanitize_rejects_javascript_href() {
        let raw = r#"<a href="javascript:alert(1)">x</a>"#;
        let out = sanitize_about_html(raw).unwrap().unwrap();
        assert!(!out.contains("javascript:"), "js href dropped: {out}");
    }

    #[test]
    fn sanitize_keeps_http_href_with_forced_rel_and_target() {
        let raw = r#"<a href="https://acme.example">acme</a>"#;
        let out = sanitize_about_html(raw).unwrap().unwrap();
        assert!(out.contains("https://acme.example"));
        assert!(out.contains("rel=") && out.contains("nofollow"), "rel forced: {out}");
        assert!(out.contains("target=\"_blank\""), "target forced: {out}");
    }

    #[test]
    fn sanitize_allows_whitelisted_data_image_and_drops_svg() {
        let good = format!(r#"<img src="{}">"#, png_data_uri());
        let out = sanitize_about_html(&good).unwrap().unwrap();
        assert!(out.contains("data:image/png;base64,"), "png data uri kept: {out}");

        let svg = r#"<img src="data:image/svg+xml;base64,PHN2Zz48L3N2Zz4=">"#;
        let out2 = sanitize_about_html(svg).unwrap().unwrap_or_default();
        assert!(!out2.contains("svg+xml"), "svg data uri dropped: {out2}");
    }

    #[test]
    fn sanitize_rejects_oversized_html() {
        let big = "<p>".to_string() + &"a".repeat(MAX_ABOUT_HTML_BYTES) + "</p>";
        assert!(sanitize_about_html(&big).is_err());
    }

    #[test]
    fn validate_input_stores_sanitized_about_html() {
        let input = BrandingInput {
            about_html: Some(r#"<p>hi</p><script>x</script>"#.to_string()),
            accent_color: Some("#F59E0B".to_string()),
            ..Default::default()
        };
        let cfg = validate_input(input).unwrap();
        let html = cfg.about_html.unwrap();
        assert!(!html.contains("script"));
        // accent normalized to lowercase.
        assert_eq!(cfg.accent_color.as_deref(), Some("#f59e0b"));
    }

    // ── §10.4 accent colour ─────────────────────────────────────

    #[test]
    fn accent_color_validation() {
        assert_eq!(validate_accent_color("#f59e0b").unwrap(), "#f59e0b");
        assert_eq!(validate_accent_color("#ABCDEF").unwrap(), "#abcdef");
        assert!(validate_accent_color("f59e0b").is_err()); // no hash
        assert!(validate_accent_color("#fff").is_err()); // shorthand
        assert!(validate_accent_color("#f59e0bff").is_err()); // rgba
        assert!(validate_accent_color("red").is_err());
        assert!(validate_accent_color("#gggggg").is_err());
    }

    // ── §10.1/§10.3 signed bundle ───────────────────────────────

    fn throwaway_registry() -> (PublicKeyRegistry, [u8; 32]) {
        use ed25519_dalek::SigningKey;
        use rand::rngs::OsRng;
        let signing = SigningKey::generate(&mut OsRng);
        let seed: [u8; 32] = signing.to_bytes();
        let registry =
            PublicKeyRegistry::new().with_key("v2", signing.verifying_key().to_bytes().to_vec());
        (registry, seed)
    }

    fn sample_branding() -> BrandingConfig {
        BrandingConfig {
            product_name: Some("Acme 智慧助理".to_string()),
            accent_color: Some("#123456".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn bundle_sign_verify_roundtrip() {
        let (registry, seed) = throwaway_registry();
        let bundle = sign_bundle(
            &seed,
            "dist-1",
            "dist-1-sub",
            &sample_branding(),
            "2026-07-11T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        assert!(verify_bundle(&bundle, &registry).is_ok());
    }

    #[test]
    fn bundle_tamper_is_rejected() {
        let (registry, seed) = throwaway_registry();
        let mut bundle = sign_bundle(
            &seed,
            "dist-1",
            "dist-1-sub",
            &sample_branding(),
            "2026-07-11T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        // Flip a signed field — signature must no longer match.
        bundle.branding.product_name = Some("Evil Corp".to_string());
        assert!(verify_bundle(&bundle, &registry).is_err());
    }

    #[test]
    fn bundle_wrong_key_and_schema_rejected() {
        let (_registry, seed) = throwaway_registry();
        let (other_registry, _other_seed) = throwaway_registry();
        let bundle = sign_bundle(
            &seed,
            "d",
            "d-sub",
            &sample_branding(),
            "2026-07-11T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        // Verified against a different key → reject.
        assert!(verify_bundle(&bundle, &other_registry).is_err());
        // Wrong schema → reject even with the right key.
        let mut bad = bundle.clone();
        bad.schema = 99;
        let (registry, _) = {
            use ed25519_dalek::SigningKey;
            let sk = SigningKey::from_bytes(&seed);
            (
                PublicKeyRegistry::new().with_key("v2", sk.verifying_key().to_bytes().to_vec()),
                seed,
            )
        };
        assert!(verify_bundle(&bad, &registry).is_err());
    }

    #[test]
    fn load_resolution_order_local_over_bundle_over_default() {
        let dir = tempdir().unwrap();
        let (registry, seed) = throwaway_registry();

        // (a) No files → default.
        let (cfg, src) = load_with_source_using(dir.path(), &registry);
        assert_eq!(src, SOURCE_DEFAULT);
        assert!(cfg.product_name.is_none());

        // (b) Bundle only → bundle applies.
        let bundle = sign_bundle(
            &seed,
            "dist-1",
            "dist-1-sub",
            &sample_branding(),
            "2026-07-11T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("branding.bundle.json"),
            serde_json::to_vec_pretty(&bundle).unwrap(),
        )
        .unwrap();
        let (cfg, src) = load_with_source_using(dir.path(), &registry);
        assert_eq!(src, SOURCE_BUNDLE);
        assert_eq!(cfg.product_name.as_deref(), Some("Acme 智慧助理"));

        // (c) Local file present → local wins over the bundle.
        let local = BrandingConfig {
            product_name: Some("Local Brand".to_string()),
            ..Default::default()
        };
        save(dir.path(), &local).unwrap();
        let (cfg, src) = load_with_source_using(dir.path(), &registry);
        assert_eq!(src, SOURCE_LOCAL);
        assert_eq!(cfg.product_name.as_deref(), Some("Local Brand"));
    }

    #[test]
    fn tampered_bundle_body_falls_back_to_default() {
        let dir = tempdir().unwrap();
        let (registry, seed) = throwaway_registry();
        let mut bundle = sign_bundle(
            &seed,
            "dist-1",
            "dist-1-sub",
            &sample_branding(),
            "2026-07-11T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        // Hand-edit one byte of the signed content after signing.
        bundle.branding.product_name = Some("Hacked".to_string());
        std::fs::write(
            dir.path().join("branding.bundle.json"),
            serde_json::to_vec_pretty(&bundle).unwrap(),
        )
        .unwrap();
        let (cfg, src) = load_with_source_using(dir.path(), &registry);
        assert_eq!(src, SOURCE_DEFAULT, "tampered bundle must be ignored");
        assert!(cfg.product_name.is_none());
    }

    #[test]
    fn load_resanitizes_hand_edited_local_file() {
        let dir = tempdir().unwrap();
        // Write a branding.json directly carrying unsafe HTML (bypassing set()).
        let raw = r#"{"about_html":"<p onclick=\"x()\">hi<script>bad()</script></p>","accent_color":"not-a-color"}"#;
        std::fs::write(dir.path().join("branding.json"), raw).unwrap();
        let (registry, _seed) = throwaway_registry();
        let (cfg, src) = load_with_source_using(dir.path(), &registry);
        assert_eq!(src, SOURCE_LOCAL);
        let html = cfg.about_html.unwrap();
        assert!(!html.contains("script"), "read-time sanitize strips script: {html}");
        assert!(!html.contains("onclick"), "read-time sanitize strips handler: {html}");
        // Invalid accent dropped on read (fail-safe).
        assert!(cfg.accent_color.is_none());
    }

    // ── §11.2 first-run bundle seeding ──────────────────────────

    fn write_signed_bundle(path: &Path, seed: &[u8; 32]) {
        let bundle = sign_bundle(
            seed,
            "dist-1",
            "dist-1-sub",
            &sample_branding(),
            "2026-07-12T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        std::fs::write(path, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();
    }

    #[test]
    fn seed_installs_bundle_when_absent() {
        let home = tempdir().unwrap();
        let src = tempdir().unwrap();
        let (registry, seed) = throwaway_registry();
        let cand = src.path().join("branding.bundle.json");
        write_signed_bundle(&cand, &seed);

        let outcome = seed_bundle_using(home.path(), &registry, &[cand.clone()]);
        assert_eq!(outcome, SeedOutcome::Seeded { source: cand });

        // The seeded bundle now resolves as the active branding source.
        let (cfg, source) = load_with_source_using(home.path(), &registry);
        assert_eq!(source, SOURCE_BUNDLE);
        assert_eq!(cfg.product_name.as_deref(), Some("Acme 智慧助理"));
    }

    #[test]
    fn seed_does_not_overwrite_existing() {
        let home = tempdir().unwrap();
        let src = tempdir().unwrap();
        let (registry, seed) = throwaway_registry();

        // A pre-existing home-dir bundle (arbitrary sentinel bytes) must win —
        // idempotency keys on presence, not validity.
        let dest = home.path().join("branding.bundle.json");
        std::fs::write(&dest, b"USER-EXISTING").unwrap();

        let cand = src.path().join("branding.bundle.json");
        write_signed_bundle(&cand, &seed);

        let outcome = seed_bundle_using(home.path(), &registry, &[cand]);
        assert_eq!(outcome, SeedOutcome::AlreadyPresent);
        assert_eq!(std::fs::read(&dest).unwrap(), b"USER-EXISTING");
    }

    #[test]
    fn seed_rejects_bundle_that_fails_verification() {
        let home = tempdir().unwrap();
        let src = tempdir().unwrap();
        let (registry, seed) = throwaway_registry();

        // Sign, then tamper a signed field → signature no longer verifies.
        let mut bundle = sign_bundle(
            &seed,
            "d",
            "d-sub",
            &sample_branding(),
            "2026-07-12T00:00:00+00:00",
            "v2",
        )
        .unwrap();
        bundle.branding.product_name = Some("Evil Corp".to_string());
        let cand = src.path().join("branding.bundle.json");
        std::fs::write(&cand, serde_json::to_vec_pretty(&bundle).unwrap()).unwrap();

        let outcome = seed_bundle_using(home.path(), &registry, &[cand.clone()]);
        assert_eq!(outcome, SeedOutcome::VerifyFailed { source: cand });
        assert!(
            !home.path().join("branding.bundle.json").exists(),
            "an unverified bundle must not be seeded (fail-closed)"
        );
    }

    #[test]
    fn seed_env_override_takes_precedence() {
        let home = tempdir().unwrap();
        let env_src = tempdir().unwrap();
        let exe_src = tempdir().unwrap();
        let (registry, seed) = throwaway_registry();

        // Both candidates are valid; the env candidate is listed first exactly
        // as `seed_candidate_paths` orders it. The first existing one wins.
        let env_cand = env_src.path().join("branding.bundle.json");
        let exe_cand = exe_src.path().join("branding.bundle.json");
        write_signed_bundle(&env_cand, &seed);
        write_signed_bundle(&exe_cand, &seed);

        let outcome =
            seed_bundle_using(home.path(), &registry, &[env_cand.clone(), exe_cand]);
        assert_eq!(outcome, SeedOutcome::Seeded { source: env_cand });
    }

    #[test]
    fn seed_reports_no_candidate_when_none_exist() {
        let home = tempdir().unwrap();
        let (registry, _seed) = throwaway_registry();
        let missing = home.path().join("nowhere").join("branding.bundle.json");
        let outcome = seed_bundle_using(home.path(), &registry, &[missing]);
        assert_eq!(outcome, SeedOutcome::NoCandidate);
    }

    #[test]
    fn effective_product_name_reflects_branding() {
        let dir = tempdir().unwrap();
        // Default when nothing set — but the cache is process-global, so use a
        // fresh temp home (its path is unique) to avoid cross-test cache hits.
        assert_eq!(effective_product_name(dir.path()), DEFAULT_PRODUCT_NAME);
        let cfg = BrandingConfig {
            product_name: Some("Acme AI".to_string()),
            ..Default::default()
        };
        save(dir.path(), &cfg).unwrap();
        // The 30s TTL cache may still hold "DuDuClaw" for this path; a distinct
        // sub-home dodges the cache and proves resolution reads product_name.
        let sub = dir.path().join("inst2");
        std::fs::create_dir_all(&sub).unwrap();
        save(&sub, &cfg).unwrap();
        assert_eq!(effective_product_name(&sub), "Acme AI");
    }
}
