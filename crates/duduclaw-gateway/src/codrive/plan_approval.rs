//! DESIGN §3.5 plan-approval card: one session-level human gate in front of
//! a whole co-drive script, opt-in via `config.toml [codrive]
//! plan_approval` (default `false` — see [`super::config::CodriveConfig::
//! plan_approval`] for why that default is a rollout state, not a safety
//! judgement).
//!
//! **Zero new wheels** (DESIGN §3.4 「守門與審批全復用」). This is not a new
//! approval mechanism: it files an ordinary row with the same
//! [`ApprovalBroker`] every consequential step already uses, so the same
//! dashboard card, the same channel push, the same TTL sweep, and the same
//! decision surfaces handle it. The only things this module owns are (a)
//! *when* the row is filed — before the comp socket is opened, so a denied
//! plan never establishes a co-drive session at all — and (b) *what goes in
//! it*.
//!
//! **What goes in it, and what deliberately does not.** The card carries
//! `target_app`, `task_summary`, and a DIGEST of the action classes the
//! script will use — never the script body. A script's `text` steps can
//! carry anything the agent was about to type; putting that verbatim into
//! an approval row would push it into `approvals.db`, the dashboard, and
//! whichever channel the push lands on. The per-step `codrive_action` cards
//! (`step::gate_consequential`) stay exactly as they are — this is a layer
//! ON TOP of them, never a replacement, so approving a plan approves
//! starting the session, not any individual consequential action inside it.
//!
//! **Fail-closed, every branch.** Denied / Expired / still-Pending / broker
//! error / broker won't even open all abort the run with `final_state =
//! "aborted_plan_denied"` and an audit line. Mirrors
//! `step::gate_consequential`'s four-state mapping so there is one
//! approval-outcome doctrine in this module tree, not two.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use crate::approval::{ApprovalBroker, ApprovalStatus, SimulationNarrative};

use super::config::CodriveConfig;
use super::script::{CodriveAction, CodriveScript};
use super::step::TOOL_NAME;

/// `ApprovalBroker::await_decision` poll interval — same cadence the
/// per-step gate uses.
const APPROVAL_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// The `action_kind` this card is filed under. Distinct from the per-step
/// `codrive_action` so an operator (and any future notification routing)
/// can tell "may this agent start driving at all" apart from "may it press
/// THIS button".
pub(super) const PLAN_ACTION_KIND: &str = "codrive_session";

/// The `final_state` a refused plan produces.
pub(super) const PLAN_DENIED_STATE: &str = "aborted_plan_denied";

/// Ask a human to approve the whole plan. `Ok(approval_id)` means the
/// session may proceed; every `Err` is a refusal whose `String` is the
/// operator-facing detail already written to the audit log.
pub(super) async fn gate_plan_approval(
    home_dir: &Path,
    agent_id: &str,
    script: &CodriveScript,
    cfg: &CodriveConfig,
) -> Result<String, String> {
    // Fail-closed at the very first hurdle: if the broker cannot open there
    // is no way to ask anyone, and "nobody could be asked" must never read
    // as "nobody objected".
    let Ok(broker) = ApprovalBroker::open(home_dir) else {
        return Err(deny(
            home_dir,
            agent_id,
            "審批系統無法建立共駕計畫核准卡，已拒絕（fail-closed）".to_string(),
        ));
    };

    let digest = action_digest(script);
    let summary = format!(
        "共駕計畫核准：在「{}」執行「{}」（共 {} 步；{}）",
        script.target_app,
        script.task_summary,
        script.steps.len(),
        digest
    );
    let simulation = SimulationNarrative {
        world_state_change: format!(
            "在共享桌面上以 agent 身分操作「{}」，直到完成或人類接手",
            script.target_app
        ),
        risk_points: risk_points(script, &digest),
    }
    .to_json();
    // Digest only — never the script body. See this module's doc.
    let payload = json!({
        "target_app": script.target_app,
        "task_summary": script.task_summary,
        "step_count": script.steps.len(),
        "watch_mode": script.watch_mode,
        "action_digest": digest,
    });

    let id = match broker
        .request_with_simulation(
            agent_id,
            PLAN_ACTION_KIND,
            &summary,
            payload,
            cfg.approval_ttl_secs,
            simulation,
        )
        .await
    {
        Ok(id) => id,
        Err(e) => {
            return Err(deny(
                home_dir,
                agent_id,
                format!("建立共駕計畫核准卡失敗，已拒絕（fail-closed）：{e}"),
            ));
        }
    };

    match broker.await_decision(&id, APPROVAL_POLL_INTERVAL).await {
        Ok(ApprovalStatus::Approved) => Ok(id.to_string()),
        Ok(ApprovalStatus::Denied) => Err(deny(
            home_dir,
            agent_id,
            format!("共駕計畫已被拒絕（審核編號 {id}）"),
        )),
        Ok(ApprovalStatus::Expired) => Err(deny(
            home_dir,
            agent_id,
            format!("共駕計畫逾時未核可，已自動拒絕（審核編號 {id}）"),
        )),
        Ok(ApprovalStatus::Pending) => Err(deny(
            home_dir,
            agent_id,
            format!("共駕計畫審批狀態異常（仍為待審），已拒絕（審核編號 {id}）"),
        )),
        Err(e) => Err(deny(
            home_dir,
            agent_id,
            format!("等待共駕計畫核准時發生錯誤，已拒絕（fail-closed）：{e}"),
        )),
    }
}

/// Write the audit line for a refusal and hand the detail back unchanged —
/// one call site per branch so no branch can forget the audit row.
fn deny(home_dir: &Path, agent_id: &str, detail: String) -> String {
    duduclaw_security::audit::append_tool_call_denied(
        home_dir,
        agent_id,
        TOOL_NAME,
        "codrive_plan_denied",
        &detail,
        None,
    );
    detail
}

/// A deterministic, body-free summary of what the script is going to do:
/// action kinds with counts, the consequential classes it declares, and
/// which execution-ladder rungs it asks for. Deterministic ordering matters
/// — this string is what a human compares against last time's card.
fn action_digest(script: &CodriveScript) -> String {
    let mut kinds: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut classes: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut api_actions = 0usize;
    let mut locates = 0usize;

    for step in &script.steps {
        *kinds.entry(action_kind_token(&step.action)).or_insert(0) += 1;
        if let Some(cons) = &step.consequential {
            *classes.entry(cons.class.as_str()).or_insert(0) += 1;
        }
        if step.api_action.is_some() {
            api_actions += 1;
        }
        if step.locate.is_some() {
            locates += 1;
        }
    }

    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("動作：{}", join_counts(&kinds)));
    if classes.is_empty() {
        parts.push("無需逐步核可的動作".to_string());
    } else {
        parts.push(format!("需逐步核可：{}", join_counts(&classes)));
    }
    if api_actions > 0 {
        parts.push(format!("原生 API 動作 {api_actions} 步"));
    }
    if locates > 0 {
        parts.push(format!("無障礙定位 {locates} 步"));
    }
    if script.watch_mode {
        parts.push("全程要求人在場（watch mode）".to_string());
    }
    parts.join("；")
}

fn join_counts(counts: &BTreeMap<&'static str, usize>) -> String {
    counts
        .iter()
        .map(|(k, n)| format!("{k}×{n}"))
        .collect::<Vec<_>>()
        .join("、")
}

/// The `kind` token this action serializes as on the wire — reused here so
/// the card names actions the same way the script the operator wrote does.
fn action_kind_token(action: &CodriveAction) -> &'static str {
    match action {
        CodriveAction::Move { .. } => "move",
        CodriveAction::Click { .. } => "click",
        CodriveAction::Text { .. } => "text",
        CodriveAction::KeyName { .. } => "key_name",
        CodriveAction::Wait { .. } => "wait",
        CodriveAction::TakeOver { .. } => "take_over",
    }
}

/// The bullet list on the approval card's simulation block. Always names
/// the target app and the digest; adds a line for each thing a reader
/// should not have to infer from the digest.
fn risk_points(script: &CodriveScript, digest: &str) -> Vec<String> {
    let mut points = vec![
        format!("目標應用：{}", script.target_app),
        format!("步驟摘要：{digest}"),
    ];
    let consequential = script
        .steps
        .iter()
        .filter(|s| s.consequential.is_some())
        .count();
    if consequential > 0 {
        points.push(format!(
            "其中 {consequential} 步另有逐步核可卡，核准本計畫不等於核准那些動作"
        ));
    }
    if script
        .steps
        .iter()
        .any(|s| matches!(s.action, CodriveAction::TakeOver { .. }))
    {
        points.push("含交棒步驟：過程中會把鍵鼠交還給人".to_string());
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codrive::script::{
        CodriveConsequential, CodriveStep, ConsequentialClass, LocateRequest,
    };

    fn step(action: CodriveAction) -> CodriveStep {
        CodriveStep {
            narration: "n".to_string(),
            highlight: None,
            action,
            consequential: None,
            api_action: None,
            locate: None,
        }
    }

    fn script(steps: Vec<CodriveStep>, watch_mode: bool) -> CodriveScript {
        CodriveScript {
            target_app: "foot".to_string(),
            task_summary: "測試".to_string(),
            steps,
            watch_mode,
        }
    }

    #[test]
    fn digest_counts_action_kinds_deterministically() {
        let s = script(
            vec![
                step(CodriveAction::Move { x: 1.0, y: 1.0 }),
                step(CodriveAction::Move { x: 2.0, y: 2.0 }),
                step(CodriveAction::Text {
                    s: "hunter2".into(),
                }),
            ],
            false,
        );
        let digest = action_digest(&s);
        assert_eq!(digest, "動作：move×2、text×1；無需逐步核可的動作");
        // Same input, same string — an operator comparing two cards must
        // not see spurious differences from map iteration order.
        assert_eq!(digest, action_digest(&s));
    }

    /// The whole point of a digest: a `text` step's payload never reaches
    /// the approval row.
    #[test]
    fn digest_never_leaks_the_script_body() {
        let s = script(
            vec![step(CodriveAction::Text {
                s: "super-secret-passphrase".into(),
            })],
            false,
        );
        let digest = action_digest(&s);
        assert!(!digest.contains("super-secret-passphrase"));
        for point in risk_points(&s, &digest) {
            assert!(!point.contains("super-secret-passphrase"));
        }
    }

    #[test]
    fn digest_reports_consequential_classes_and_ladder_rungs() {
        let mut consequential = step(CodriveAction::KeyName {
            name: "enter".into(),
        });
        consequential.consequential = Some(CodriveConsequential {
            class: ConsequentialClass::Submit,
            description: "送出".into(),
        });
        let mut located = step(CodriveAction::Click {
            x: 1.0,
            y: 1.0,
            btn: crate::codrive::client::CodriveButton::Left,
        });
        located.locate = Some(LocateRequest {
            role: "button".into(),
            name: "儲存".into(),
        });
        let s = script(vec![consequential, located], true);

        let digest = action_digest(&s);
        assert!(digest.contains("需逐步核可：submit×1"), "digest: {digest}");
        assert!(digest.contains("無障礙定位 1 步"), "digest: {digest}");
        assert!(digest.contains("watch mode"), "digest: {digest}");
    }

    #[test]
    fn risk_points_flag_a_take_over_step() {
        let s = script(
            vec![step(CodriveAction::TakeOver {
                reason: "登入".into(),
            })],
            false,
        );
        let points = risk_points(&s, &action_digest(&s));
        assert!(
            points.iter().any(|p| p.contains("交棒")),
            "points: {points:?}"
        );
    }
}
