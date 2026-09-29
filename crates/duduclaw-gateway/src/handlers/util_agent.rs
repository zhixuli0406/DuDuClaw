//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Copy an industry pack's knowledge extras (FAQ.json + flat wiki/) into a
/// freshly-created agent directory. Editable core files (SOUL.md /
/// CONTRACT.toml / agent.toml) are written by the caller from the (possibly
/// admin-edited) template content, so they are deliberately NOT copied here.
pub(crate) async fn copy_template_extras(src: &Path, dst: &Path) -> Result<(), String> {
    let faq = src.join("FAQ.json");
    if faq.is_file() {
        tokio::fs::copy(&faq, dst.join("FAQ.json"))
            .await
            .map_err(|e| format!("FAQ.json: {e}"))?;
    }
    let wiki_src = src.join("wiki");
    if wiki_src.is_dir() {
        let wiki_dst = dst.join("wiki");
        tokio::fs::create_dir_all(&wiki_dst)
            .await
            .map_err(|e| format!("wiki dir: {e}"))?;
        let mut entries = tokio::fs::read_dir(&wiki_src)
            .await
            .map_err(|e| format!("wiki read_dir: {e}"))?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            let path = entry.path();
            if path.is_file() {
                let Some(fname) = path.file_name() else {
                    continue;
                };
                tokio::fs::copy(&path, wiki_dst.join(fname))
                    .await
                    .map_err(|e| format!("wiki/{}: {e}", fname.to_string_lossy()))?;
            }
        }
    }
    Ok(())
}

/// Replace the premium-tree absolute prefix in an error message before it is
/// forwarded to the dashboard, so internal filesystem layout doesn't leak to
/// the UI (same discipline as `scrub_odoo_error`). Callers log the full error
/// via tracing first.
pub(crate) fn scrub_premium_path(err: &str, premium_dir: &Path) -> String {
    err.replace(&premium_dir.display().to_string(), "templates-premium")
}

/// Validate agent ID is safe for filesystem paths (no traversal).
///
/// WP-4I (2026-08): used to be an independent hand-rolled copy of the
/// lowercase-slug rule (byte-identical to `duduclaw-cli::lib.rs`'s copy, up
/// to a dead `!id.contains("..")` check — impossible to trigger once the
/// charset already excludes `.`) — now delegates to
/// [`duduclaw_core::is_valid_new_agent_id`], the single authoritative copy
/// of that rule. Deliberately narrower than the general-purpose
/// [`duduclaw_core::is_valid_agent_id`] (which also accepts uppercase and
/// `_`, for agents that predate this slug convention) because this name is
/// used both to validate ids at creation time (where the stricter slug rule
/// is the actual product requirement — see `handle_agents_create`'s
/// "lowercase alphanumeric with hyphens" error message) and, more broadly
/// throughout this file, to validate a caller-supplied agent id before it is
/// used to build a filesystem path. See the WP-4I report for the residual
/// risk this dual role carries: an existing agent whose id predates the
/// slug convention (mixed case or `_`) would fail these checks even though
/// `duduclaw_core::is_valid_agent_id` would accept it as path-safe.
pub(crate) fn is_valid_agent_id(id: &str) -> bool {
    duduclaw_core::is_valid_new_agent_id(id)
}

/// Validate a `[gateway] bind` value: fail-closed to a literal IP address only.
/// The dashboard offers `127.0.0.1` / `0.0.0.0` and custom IPs — a hostname,
/// blank, or any injection string (`0.0.0.0; rm -rf`, `evil.com`) must be
/// rejected so the listen address can never be steered to an unexpected target.
pub(crate) fn is_valid_bind_addr(v: &str) -> bool {
    v.parse::<std::net::IpAddr>().is_ok()
}

/// Validate a skill file stem is safe to join into a path (no separators, no
/// leading dot, no traversal). Skill names may keep mixed case and `_`/`.`
/// (e.g. GitHub-sourced skills), unlike the stricter agent-id charset.
pub(crate) fn is_valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

// ── WP4 agent avatar upload ───────────────────────────────────────────────
//
// Avatars are stored as a single `agents/<id>/avatar.<ext>` file (raw bytes),
// mirroring the branding-logo policy: PNG/JPEG/WebP only (SVG excluded — it can
// carry script), a 512 KB decoded ceiling, and magic-byte confirmation of the
// declared mime. Read back into an inline data URI for the dashboard (same
// `img-src data:` CSP as the logo).

/// On-disk avatar file extensions, in read-preference order.
pub(crate) const AVATAR_EXTS: &[&str] = &["png", "jpg", "webp"];

/// Extension → mime for reconstructing the data URI on read.
pub(crate) const AVATAR_EXT_MIME: &[(&str, &str)] = &[
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("webp", "image/webp"),
];

/// Maximum decoded avatar size (512 KB) — matches the branding-logo ceiling.
pub(crate) const MAX_AVATAR_DECODED_BYTES: usize = 512 * 1024;

/// Decode + validate an avatar data URI. Returns `(bytes, extension)` on success.
/// Thin wrapper over the shared `branding::validate_image_data_uri` (single
/// source of truth for raster data-URI validation) — fails closed on unknown
/// mime, oversized payload (bounded pre-decode), or a magic-byte mismatch.
pub(crate) fn decode_avatar_data_uri(uri: &str) -> Result<(Vec<u8>, &'static str), String> {
    let img = crate::branding::validate_image_data_uri(uri, MAX_AVATAR_DECODED_BYTES, "Avatar")?;
    let ext = img.kind.ext();
    Ok((img.bytes, ext))
}

/// Wardrobe slot names — the fixed shape of `outfit.json` (see the web
/// `lib/outfit.ts` catalogue; the server validates shape + charset only, so
/// newer clients can ship new item ids without a gateway release).
pub(crate) const OUTFIT_SLOTS: &[&str] = &["hat", "head", "body", "hands", "feet", "accessory"];

/// Validate + normalize an untrusted `agents.set_outfit` payload into the
/// canonical stored form. Fail-closed: unknown keys, non-string slots, long or
/// non-ASCII item ids, and out-of-range tints are all rejected.
pub(crate) fn normalize_outfit(raw: &Value) -> Result<Value, String> {
    let obj = raw.as_object().ok_or("outfit must be an object or null")?;
    for key in obj.keys() {
        if key != "schema" && key != "tint" && !OUTFIT_SLOTS.contains(&key.as_str()) {
            return Err(format!("unknown outfit field '{key}'"));
        }
    }
    let tint = match obj.get("tint") {
        None => 0,
        Some(v) => {
            let t = v.as_i64().ok_or("outfit.tint must be an integer 0-10")?;
            if !(0..=10).contains(&t) {
                return Err("outfit.tint must be 0-10 (0 = seeded tint)".into());
            }
            t
        }
    };
    let mut out = serde_json::Map::new();
    out.insert("schema".into(), json!(1));
    out.insert("tint".into(), json!(tint));
    for slot in OUTFIT_SLOTS {
        let item = match obj.get(*slot) {
            None => "",
            Some(v) => v
                .as_str()
                .ok_or_else(|| format!("outfit.{slot} must be a string item id"))?,
        };
        if item.len() > 24
            || !item
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        {
            return Err(format!(
                "outfit.{slot} item id must be lowercase ASCII (a-z0-9_-), max 24 chars"
            ));
        }
        out.insert((*slot).into(), json!(item));
    }
    Ok(Value::Object(out))
}

/// Read an agent's saved outfit (`agents/<id>/outfit.json`). `None` when the
/// agent has never been dressed — the client renders the seeded default.
pub(crate) fn read_agent_outfit(agent_dir: &std::path::Path) -> Option<Value> {
    let raw = std::fs::read_to_string(agent_dir.join("outfit.json")).ok()?;
    serde_json::from_str::<Value>(&raw)
        .ok()
        .filter(|v| v.is_object())
}
