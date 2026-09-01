//! OS security line P0 — C4: seed a default autopilot rule pack reacting to
//! `AutopilotEvent::SecurityEvent` (design §2 支柱三 C4,
//! `DESIGN-os-security-line-2026-09.md`).
//!
//! ## Why only two rules, not three
//!
//! The design brief asked for (1) Critical → notify, (2) Warning+
//! injection/circuit_breaker → notify, and (3) a demonstrative rule for
//! "Posture=Red → failsafe degrade", with an explicit fallback to two rules
//! if the third's action surface couldn't carry it. It can't: the autopilot
//! `notify`/`delegate`/`run_skill` action vocabulary has no "degrade
//! failsafe" action type, and per the design that reaction is D2's tier-1
//! "自動" behavior — it must fire unconditionally and immediately on every
//! Red transition, not be gate-able by a rule an operator could disable or
//! misconfigure. It is wired directly in code instead: see
//! `crate::posture_watch::poll_once`'s `PostureAction::Restrict` branch. This
//! module ships exactly the two rules whose action DOES fit the existing
//! vocabulary.
//!
//! ## Why the two rules ship `enabled: false`
//!
//! `notify` requires a real `channel` + `chat_id` (`handlers::
//! validate_autopilot_action`) — there is no "notify the operator's default
//! channel" primitive; every existing `notify` rule (including
//! `rule_induction.rs`'s induced ones) is written against a chat platform
//! the operator already configured. At first-boot seed time this module has
//! no way to know which channel（if any）an install even has configured, let
//! alone a valid `chat_id` for it. Shipping these ENABLED with a fabricated
//! placeholder `chat_id` would fire on the first Warning/Critical event and
//! fail every single time (the channel send would 4xx), producing nothing
//! but noisy `autopilot_history` failure rows and zero actual notification —
//! worse than not shipping the rule at all. Shipping them `enabled: false`
//! with an unmistakable name + `action.chat_id` placeholder is the honest
//! middle ground: they document the intended pattern, appear in the
//! dashboard Automation tab immediately, and require exactly one edit
//! (fill in channel + chat_id, flip the switch) to go live — never a silent
//! failure loop.
//!
//! ## Seeding contract (mirrors `builtin_skills_seed_migration.rs`)
//!
//! Runs once (marker file at `<home>/migrations/g19-security-rules-seed.done`),
//! never overwrites/re-creates a rule the operator has since edited or
//! deleted (deterministic IDs + a plain existence check, THEN the marker),
//! and never blocks boot: every failure degrades to a `warn!`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde_json::json;
use tracing::{info, warn};

use crate::autopilot_store::{AutopilotRuleRow, AutopilotStore};

/// Marker filename under `<home>/migrations/`. Its presence (regardless of
/// contents) suppresses re-runs — see module docs for why this must never
/// resurrect a deliberately-deleted or edited rule.
const MARKER_NAME: &str = "g19-security-rules-seed.done";

/// Deterministic IDs (not UUIDs) so a re-run — however it happens — can
/// never create a duplicate: `insert_rule`'s `id` is a SQLite PRIMARY KEY,
/// so a second attempt at the same ID fails cleanly instead of doubling the
/// rule set.
const RULE_ID_CRITICAL: &str = "g19-security-critical-notify";
const RULE_ID_WARN_INJECTION_BREAKER: &str = "g19-security-injection-breaker-notify";

/// Placeholder the operator must replace before enabling — deliberately NOT
/// a plausible-looking value (a real-looking fake chat_id invites "did I
/// already configure this?" confusion).
const PLACEHOLDER_CHAT_ID: &str = "REPLACE_ME";

fn marker_path(home_dir: &Path) -> PathBuf {
    home_dir.join("migrations").join(MARKER_NAME)
}

/// Outcome of one seed pass (returned for tests + boot logging).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SeedReport {
    /// Rule IDs actually inserted by this pass.
    pub seeded: Vec<String>,
    /// True when a marker already existed, so nothing was written.
    pub already_done: bool,
    /// Populated when a rule insert itself failed (the pass still returns).
    pub error: Option<String>,
}

fn critical_notify_rule(now: &str) -> AutopilotRuleRow {
    AutopilotRuleRow {
        id: RULE_ID_CRITICAL.to_string(),
        name: "🔴 安全：Critical 事件通知（需設定通知管道後啟用）".to_string(),
        enabled: false,
        trigger_event: "security_event".to_string(),
        conditions: json!({
            "field": "severity", "op": "eq", "value": "critical"
        })
        .to_string(),
        action: json!({
            "type": "notify",
            "channel": "telegram",
            "chat_id": PLACEHOLDER_CHAT_ID,
            "text": "🔴 [Critical 安全事件] {event_type}（agent={agent_id}, source={source}）",
        })
        .to_string(),
        created_at: now.to_string(),
        last_triggered_at: None,
        trigger_count: 0,
        sequence: None,
        metadata: Some(
            json!({
                "seeded": true,
                "seeded_by": "g19_security_rules_seed",
                "seeded_at": now,
                "needs_configuration": true,
                "note": "編輯 action.channel / action.chat_id 為實際通知管道，並將 enabled 設為 true 後生效。",
            })
            .to_string(),
        ),
    }
}

fn warn_injection_breaker_rule(now: &str) -> AutopilotRuleRow {
    AutopilotRuleRow {
        id: RULE_ID_WARN_INJECTION_BREAKER.to_string(),
        name: "⚠️ 安全：注入／斷路器警示（需設定通知管道後啟用）".to_string(),
        enabled: false,
        trigger_event: "security_event".to_string(),
        conditions: json!({
            "all": [
                { "field": "severity", "op": "in", "value": ["warning", "critical"] },
                {
                    "field": "event_type",
                    "op": "in",
                    "value": ["prompt_injection", "circuit_breaker_tripped"]
                },
            ]
        })
        .to_string(),
        action: json!({
            "type": "notify",
            "channel": "telegram",
            "chat_id": PLACEHOLDER_CHAT_ID,
            "text": "⚠️ [安全警示] {event_type}（severity={severity}, agent={agent_id}）",
        })
        .to_string(),
        created_at: now.to_string(),
        last_triggered_at: None,
        trigger_count: 0,
        sequence: None,
        metadata: Some(
            json!({
                "seeded": true,
                "seeded_by": "g19_security_rules_seed",
                "seeded_at": now,
                "needs_configuration": true,
                "note": "編輯 action.channel / action.chat_id 為實際通知管道，並將 enabled 設為 true 後生效。",
            })
            .to_string(),
        ),
    }
}

/// Run the seed pass if it has not run before. Never returns an error: a
/// wedged seed must not stop the gateway from booting. Callers get a
/// [`SeedReport`] for logging/tests.
pub async fn run(home_dir: &Path, store: &AutopilotStore) -> SeedReport {
    let mut report = SeedReport::default();
    let marker = marker_path(home_dir);
    if marker.exists() {
        report.already_done = true;
        return report;
    }

    let now = Utc::now().to_rfc3339();
    for rule in [
        critical_notify_rule(&now),
        warn_injection_breaker_rule(&now),
    ] {
        // Belt-and-suspenders: skip (don't overwrite) if a rule with this ID
        // already exists — e.g. an operator hand-authored the same ID, or a
        // prior partial run got this far before crashing. `insert_rule`
        // would fail on the PRIMARY KEY anyway; checking first keeps the
        // failure path clean and avoids a scary-looking (but harmless) SQL
        // error in the boot log for the common "already there" case.
        match store.get_rule(&rule.id).await {
            Ok(Some(_)) => {
                info!(rule_id = %rule.id, "g19 security rules seed: rule already exists — skipping");
                continue;
            }
            Ok(None) => {}
            Err(e) => {
                warn!(rule_id = %rule.id, error = %e, "g19 security rules seed: get_rule check failed — skipping this rule");
                report.error = Some(e);
                continue;
            }
        }
        match store.insert_rule(&rule).await {
            Ok(()) => {
                report.seeded.push(rule.id.clone());
            }
            Err(e) => {
                warn!(rule_id = %rule.id, error = %e, "g19 security rules seed: insert_rule failed — will retry on next boot (no marker written)");
                report.error = Some(e);
                return report;
            }
        }
    }

    write_marker(&marker, &report, &now);
    info!(
        seeded = ?report.seeded,
        "g19 security rules seed: default security autopilot rule pack seeded (disabled — \
         needs channel/chat_id configuration before use)"
    );
    report
}

/// Best-effort marker write — same rationale as
/// `builtin_skills_seed_migration.rs`'s: a failure here only risks a
/// re-run next boot, which is harmless (the per-rule existence check above
/// makes the whole pass idempotent even without the marker).
fn write_marker(marker: &Path, report: &SeedReport, now: &str) {
    if let Some(parent) = marker.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(error = %e, "g19 security rules seed: could not create migrations dir — marker not written");
            return;
        }
    }
    let record = json!({
        "migration": "g19-security-rules-seed",
        "completed_at": now,
        "seeded_rule_ids": report.seeded,
        "reason": "OS 安全線 P0（C4）：安裝時種入兩條預設安全自動化規則（Critical 安全事件通知、\
                   注入／斷路器警示），預設停用——需要通知管道（Telegram/Discord/…）才能實際送達，\
                   而種子階段無法得知每台裝置的通道設定。操作者編輯 action.channel / \
                   action.chat_id 並啟用後即可生效。本檔存在即代表種子做過了；之後刻意刪除或\
                   停用的規則不會被種回來。",
    });
    if let Err(e) = std::fs::write(marker, format!("{record}\n")) {
        warn!(error = %e, "g19 security rules seed: could not write completion marker — may re-run next boot");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn open_store(home: &Path) -> AutopilotStore {
        AutopilotStore::open(home).expect("open autopilot store")
    }

    #[tokio::test]
    async fn first_run_seeds_two_disabled_rules() {
        let home = TempDir::new().unwrap();
        let store = open_store(home.path()).await;

        let report = run(home.path(), &store).await;

        assert!(!report.already_done);
        assert!(report.error.is_none());
        assert_eq!(report.seeded.len(), 2);
        assert!(marker_path(home.path()).exists());

        let rules = store.list_rules().await.unwrap();
        assert_eq!(rules.len(), 2);
        for r in &rules {
            assert!(!r.enabled, "seeded rules must start disabled: {}", r.id);
            assert_eq!(r.trigger_event, "security_event");
            let action: serde_json::Value = serde_json::from_str(&r.action).unwrap();
            assert_eq!(action["type"], "notify");
            assert_eq!(action["chat_id"], PLACEHOLDER_CHAT_ID);
        }
    }

    #[tokio::test]
    async fn seeded_rule_conditions_are_well_formed_json() {
        let home = TempDir::new().unwrap();
        let store = open_store(home.path()).await;
        run(home.path(), &store).await;

        let critical = store.get_rule(RULE_ID_CRITICAL).await.unwrap().unwrap();
        let cond: serde_json::Value = serde_json::from_str(&critical.conditions).unwrap();
        assert_eq!(cond["field"], "severity");
        assert_eq!(cond["op"], "eq");
        assert_eq!(cond["value"], "critical");

        let warn_rule = store
            .get_rule(RULE_ID_WARN_INJECTION_BREAKER)
            .await
            .unwrap()
            .unwrap();
        let cond: serde_json::Value = serde_json::from_str(&warn_rule.conditions).unwrap();
        assert!(cond["all"].is_array());
    }

    /// The load-bearing guarantee: after the seed has run once, an operator
    /// who deliberately deletes (or disables-and-ignores) a seeded rule must
    /// not find it resurrected on the next boot.
    #[tokio::test]
    async fn marker_prevents_a_second_run_from_resurrecting_a_deleted_rule() {
        let home = TempDir::new().unwrap();
        let store = open_store(home.path()).await;

        let first = run(home.path(), &store).await;
        assert_eq!(first.seeded.len(), 2);

        store.remove_rule(RULE_ID_CRITICAL).await.unwrap();
        assert!(store.get_rule(RULE_ID_CRITICAL).await.unwrap().is_none());

        let second = run(home.path(), &store).await;
        assert!(second.already_done);
        assert!(second.seeded.is_empty());
        assert!(
            store.get_rule(RULE_ID_CRITICAL).await.unwrap().is_none(),
            "a deliberately deleted seeded rule must stay deleted"
        );
    }

    /// An operator's edit (e.g. flipping `enabled` to true after configuring
    /// a real channel) must survive a re-run even if the marker were somehow
    /// absent (belt-and-suspenders existence check, independent of the
    /// marker).
    #[tokio::test]
    async fn existing_rule_id_is_never_overwritten_even_without_the_marker() {
        let home = TempDir::new().unwrap();
        let store = open_store(home.path()).await;
        run(home.path(), &store).await;

        // Operator configures + enables the rule.
        store
            .update_rule(
                RULE_ID_CRITICAL,
                &json!({
                    "enabled": true,
                    "action": {
                        "type": "notify",
                        "channel": "telegram",
                        "chat_id": "123456789",
                        "text": "real text"
                    }
                }),
            )
            .await
            .unwrap();

        // Simulate a re-run with the marker removed (defensive scenario —
        // should never happen in production, but the per-rule existence
        // check must hold regardless).
        std::fs::remove_file(marker_path(home.path())).unwrap();
        let second = run(home.path(), &store).await;
        assert!(
            second.seeded.is_empty(),
            "must not re-seed an existing rule id"
        );

        let rule = store.get_rule(RULE_ID_CRITICAL).await.unwrap().unwrap();
        assert!(rule.enabled, "operator's enable edit must survive");
        let action: serde_json::Value = serde_json::from_str(&rule.action).unwrap();
        assert_eq!(
            action["chat_id"], "123456789",
            "operator's chat_id edit must survive"
        );
    }
}
