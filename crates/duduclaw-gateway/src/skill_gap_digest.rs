//! WP2.6 P1 — daily aggregated skill-recommendation digest.
//!
//! P0 records attachment extension gaps at the single attachment chokepoint
//! (`duduclaw_agent::skill_ext_gap`) and exposes them on demand via the
//! `skill_gaps` MCP tool. P1 (this module) closes the loop proactively: once a
//! day, the last 24 h of gap records are aggregated into at most one short
//! zh-TW recommendation message ("your team keeps receiving X files — consider
//! installing skill Y", skills ranked by the existing federated hub search)
//! and pushed to the default agent's notify channel.
//!
//! Design constraints:
//! - **No new scheduler** — the check rides the existing `CronScheduler` 30 s
//!   tick (scheduler-level, like the heartbeat task-board pull); a persisted
//!   `skill_gap_digest_state.json` cursor enforces the 24 h cadence.
//! - **Silent when empty** — no gaps in the window ⇒ no message, no noise
//!   (the "無事不報" rule). The daily cursor still advances.
//! - **Default OFF** — `config.toml [skills] gap_digest_enabled = true` opts
//!   in (same `[skills]` family as the curated `recommended` list read from
//!   `agent.toml`).
//! - **Best-effort** — a hub outage degrades to capability-only suggestions;
//!   a missing notify destination or token logs and skips. Nothing here can
//!   fail a channel turn.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use duduclaw_agent::skill_ext_gap::{aggregate_gaps_for_agent, AggregatedExtGap, ExtGapRecord};
use tracing::{debug, info, warn};

/// Aggregation window: only records seen in the last 24 h count.
const WINDOW_HOURS: i64 = 24;

/// At most this many capability gaps make it into one digest.
const TOP_GAPS: usize = 3;

/// At most this many suggested skills are named per gap.
const SKILLS_PER_GAP: usize = 2;

// ── Config gate ──────────────────────────────────────────────────────────────

/// Parse the opt-in flag from raw `config.toml` content:
/// `[skills] gap_digest_enabled = true`. Anything missing/malformed ⇒ `false`
/// (default off).
pub fn gap_digest_enabled_from_str(config: &str) -> bool {
    let Ok(table) = config.parse::<toml::Table>() else {
        return false;
    };
    table
        .get("skills")
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("gap_digest_enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

async fn gap_digest_enabled(home_dir: &Path) -> bool {
    match tokio::fs::read_to_string(home_dir.join("config.toml")).await {
        Ok(c) => gap_digest_enabled_from_str(&c),
        Err(_) => false,
    }
}

// ── Daily cursor ─────────────────────────────────────────────────────────────

fn state_path(home_dir: &Path) -> PathBuf {
    home_dir.join("skill_gap_digest_state.json")
}

/// Whether a new daily run is due. `last_run = None` (first ever run, or an
/// unreadable state file) ⇒ due.
pub fn digest_due(last_run: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match last_run {
        None => true,
        Some(t) => now.signed_duration_since(t).num_hours() >= WINDOW_HOURS,
    }
}

fn read_last_run(home_dir: &Path) -> Option<DateTime<Utc>> {
    let content = std::fs::read_to_string(state_path(home_dir)).ok()?;
    let v: serde_json::Value = serde_json::from_str(&content).ok()?;
    let raw = v.get("last_run")?.as_str()?;
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|d| d.with_timezone(&Utc))
}

fn write_last_run(home_dir: &Path, now: DateTime<Utc>) {
    let body = serde_json::json!({ "last_run": now.to_rfc3339() }).to_string();
    if let Err(e) = std::fs::write(state_path(home_dir), body) {
        warn!("skill-gap digest: state write failed: {e}");
    }
}

// ── Pure aggregation ─────────────────────────────────────────────────────────

/// Select the top capability gaps for `agent_id` from raw records: keep only
/// observations inside the 24 h window ending at `now` (unparseable timestamps
/// are dropped), aggregate + exclude already-covered capabilities via the
/// existing P0 aggregator (which dedupes by capability and sorts by count
/// desc), then cap at [`TOP_GAPS`].
pub fn select_top_gaps(
    records: &[ExtGapRecord],
    agent_id: &str,
    installed_terms: &[String],
    now: DateTime<Utc>,
) -> Vec<AggregatedExtGap> {
    let windowed: Vec<ExtGapRecord> = records
        .iter()
        .filter(|r| {
            DateTime::parse_from_rfc3339(&r.at)
                .map(|t| {
                    let age = now.signed_duration_since(t.with_timezone(&Utc));
                    age.num_seconds() >= 0 && age.num_hours() < WINDOW_HOURS
                })
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    let mut gaps = aggregate_gaps_for_agent(&windowed, agent_id, installed_terms);
    gaps.truncate(TOP_GAPS);
    gaps
}

/// One digest line's worth of data: an unmet capability plus the hub-ranked
/// skill names suggested for it (may be empty when the search found nothing).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigestItem {
    pub capability: String,
    pub exts: Vec<String>,
    pub count: usize,
    pub skills: Vec<String>,
}

/// Render the zh-TW digest message. `None` when there is nothing to say —
/// the caller must stay completely silent in that case.
pub fn render_digest(items: &[DigestItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let mut lines = vec!["📚 技能市集每日推薦".to_string(), String::new()];
    lines.push("你的團隊最近 24 小時內：".to_string());
    for it in items {
        let exts = it.exts.join(" / ");
        let mut line = format!(
            "• 收到 {} 個 {exts} 檔案，目前沒有對應技能（{}）",
            it.count, it.capability
        );
        if !it.skills.is_empty() {
            line.push_str(&format!("，建議安裝：{}", it.skills.join("、")));
        }
        lines.push(line);
    }
    lines.push(String::new());
    lines.push("可用 skill_search 指令或儀表板的技能市集安裝。".to_string());
    Some(lines.join("\n"))
}

// ── Driver (rides the CronScheduler tick) ────────────────────────────────────

/// Read `config.toml [general] default_agent`. Local copy of the private
/// `channel_reply` reader (that file is owned by another workstream).
async fn read_default_agent(home_dir: &Path) -> Option<String> {
    let content = tokio::fs::read_to_string(home_dir.join("config.toml"))
        .await
        .ok()?;
    let table: toml::Table = content.parse().ok()?;
    let name = table
        .get("general")?
        .as_table()?
        .get("default_agent")?
        .as_str()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Daily digest entry point, called from the `CronScheduler` 30 s tick.
///
/// Cheap early exits: feature disabled, or the daily cursor is not due. When a
/// run IS due the cursor advances immediately (one attempt per day — a
/// transient send failure is logged and dropped, never retried in a storm).
pub async fn maybe_send_daily_digest(home_dir: &Path) {
    if !gap_digest_enabled(home_dir).await {
        return;
    }
    let now = Utc::now();
    if !digest_due(read_last_run(home_dir), now) {
        return;
    }
    // Advance the cursor first: whatever happens below, we try once per day.
    write_last_run(home_dir, now);

    // Resolve the default agent (the digest's subject AND push target).
    let Some(agent_id) = read_default_agent(home_dir).await else {
        info!("skill-gap digest: no [general] default_agent configured; skipping");
        return;
    };

    // Installed skill names (global + agent-local) — the "already covered"
    // exclusion set, same shape as the `skill_gaps` MCP tool.
    let mut installed_terms: Vec<String> = Vec::new();
    for dir in [
        home_dir.join("skills"),
        home_dir.join("agents").join(&agent_id).join("SKILLS"),
    ] {
        for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&dir).await {
            installed_terms.push(sk.name.to_lowercase());
        }
    }

    let records = duduclaw_agent::skill_ext_gap::read_all(home_dir);
    let gaps = select_top_gaps(&records, &agent_id, &installed_terms, now);
    if gaps.is_empty() {
        // 無事不報: nothing seen in the window ⇒ full silence.
        debug!("skill-gap digest: no gaps in the last 24h; staying silent");
        return;
    }

    // Federated hub search per gap (best-effort; an outage degrades to a
    // capability-only line, never blocks the digest).
    let registry = duduclaw_agent::skill_hub::HubRegistry::from_home(home_dir);
    let mut items: Vec<DigestItem> = Vec::new();
    for gap in &gaps {
        let result = registry.search(home_dir, &gap.capability, 5, None).await;
        for (hub, err) in &result.errors {
            debug!("skill-gap digest: hub {hub} search failed: {err}");
        }
        let skills: Vec<String> = result
            .hits
            .iter()
            .take(SKILLS_PER_GAP)
            .map(|h| h.entry.name.clone())
            .collect();
        items.push(DigestItem {
            capability: gap.capability.clone(),
            exts: gap.exts.clone(),
            count: gap.count,
            skills,
        });
    }

    let Some(message) = render_digest(&items) else {
        return;
    };

    // Push to the default agent's notify destination via the shared governed
    // path (W2-4): same `[proactive]` convention + reports_to token cascade
    // the goal loop uses, plus quiet-hours deferral and action-rate telemetry.
    // L1 — a list of capability gaps is a reading item, never a page.
    let outcome = crate::goal_notify::notify_agent_plain(
        home_dir,
        &agent_id,
        crate::notify_governance::NotifyLevel::Fyi,
        "skill_gap.digest",
        &message,
    )
    .await;
    match outcome {
        crate::goal_notify::NotifyOutcome::Sent => {
            info!(agent = %agent_id, gaps = items.len(), "skill-gap digest sent")
        }
        crate::goal_notify::NotifyOutcome::Deferred => {
            info!(agent = %agent_id, gaps = items.len(), "skill-gap digest queued until quiet hours end")
        }
        crate::goal_notify::NotifyOutcome::NoTarget => info!(
            agent = %agent_id,
            "skill-gap digest: agent has no [proactive] notify destination or bot token; skipping push"
        ),
        crate::goal_notify::NotifyOutcome::SendFailed => {
            warn!(agent = %agent_id, "skill-gap digest send failed (will retry tomorrow)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(agent: &str, ext: &str, cap: &str, at: DateTime<Utc>) -> ExtGapRecord {
        ExtGapRecord {
            agent_id: agent.to_string(),
            ext: ext.to_string(),
            capability: cap.to_string(),
            filename: format!("sample.{ext}"),
            at: at.to_rfc3339(),
        }
    }

    #[test]
    fn empty_gaps_render_none() {
        assert_eq!(render_digest(&[]), None);
    }

    #[test]
    fn select_filters_window_and_caps_top3_deduped() {
        let now = Utc::now();
        let fresh = now - chrono::Duration::hours(2);
        let stale = now - chrono::Duration::hours(30);
        let records = vec![
            // photoshop ×2 fresh (dupes aggregate into one line).
            rec("boss", "psd", "photoshop", fresh),
            rec("boss", "psd", "photoshop", fresh),
            // three more distinct fresh capabilities → 4 candidates total.
            rec("boss", "dwg", "cad autocad", fresh),
            rec("boss", "blend", "blender 3d", fresh),
            rec("boss", "sav", "spss statistics", fresh),
            // outside the 24h window — must not count.
            rec("boss", "fig", "figma design", stale),
            // other agent — must not leak in.
            rec("other", "xd", "adobe xd", fresh),
            // unparseable timestamp — dropped, not a panic.
            ExtGapRecord {
                agent_id: "boss".into(),
                ext: "obj".into(),
                capability: "3d model".into(),
                filename: "x.obj".into(),
                at: "not-a-date".into(),
            },
        ];
        let gaps = select_top_gaps(&records, "boss", &[], now);
        // 4 in-window capabilities, capped at top 3; photoshop (2×) first.
        assert_eq!(gaps.len(), 3);
        assert_eq!(gaps[0].capability, "photoshop");
        assert_eq!(gaps[0].count, 2);
        let caps: Vec<&str> = gaps.iter().map(|g| g.capability.as_str()).collect();
        assert!(!caps.contains(&"figma design"), "stale record leaked in");
        assert!(!caps.contains(&"adobe xd"), "other agent leaked in");
        // Dedup: photoshop appears exactly once despite two records.
        assert_eq!(caps.iter().filter(|c| **c == "photoshop").count(), 1);
    }

    #[test]
    fn installed_skills_are_excluded() {
        let now = Utc::now();
        let fresh = now - chrono::Duration::hours(1);
        let records = vec![
            rec("boss", "psd", "photoshop", fresh),
            rec("boss", "dwg", "cad autocad", fresh),
        ];
        let installed = vec!["photoshop toolkit".to_string()];
        let gaps = select_top_gaps(&records, "boss", &installed, now);
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].capability, "cad autocad");
    }

    #[test]
    fn render_includes_counts_skills_and_stays_zh() {
        let items = vec![
            DigestItem {
                capability: "photoshop".into(),
                exts: vec!["psd".into()],
                count: 3,
                skills: vec!["psd-editor".into(), "design-kit".into()],
            },
            DigestItem {
                capability: "cad autocad".into(),
                exts: vec!["dwg".into(), "dxf".into()],
                count: 1,
                skills: vec![],
            },
        ];
        let msg = render_digest(&items).expect("non-empty items must render");
        assert!(msg.contains("3 個 psd"), "got: {msg}");
        assert!(msg.contains("psd-editor、design-kit"), "got: {msg}");
        // A gap with no hub hits still gets a capability-only line.
        assert!(msg.contains("dwg / dxf"), "got: {msg}");
        assert!(msg.contains("技能市集"), "got: {msg}");
    }

    #[test]
    fn digest_due_only_after_24h() {
        let now = Utc::now();
        assert!(digest_due(None, now));
        assert!(!digest_due(Some(now - chrono::Duration::hours(23)), now));
        assert!(digest_due(Some(now - chrono::Duration::hours(24)), now));
        assert!(digest_due(Some(now - chrono::Duration::days(3)), now));
    }

    #[test]
    fn config_flag_defaults_off() {
        assert!(!gap_digest_enabled_from_str(""));
        assert!(!gap_digest_enabled_from_str("[skills]\n"));
        assert!(!gap_digest_enabled_from_str("not toml ["));
        assert!(!gap_digest_enabled_from_str(
            "[skills]\ngap_digest_enabled = false\n"
        ));
        assert!(gap_digest_enabled_from_str(
            "[skills]\ngap_digest_enabled = true\n"
        ));
    }
}
