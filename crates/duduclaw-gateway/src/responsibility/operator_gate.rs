//! The dashboard approval in front of every state-changing operator CLI
//! action (P2-A, appendix D.1).
//!
//! `duduclaw responsibility …` cannot prove who typed it: an AI employee with
//! Bash runs as the same OS user. So no command-line action takes effect on
//! its own — not even pause, disable or stop. The first run files one
//! approval of the dashboard-only kind [`ACTION_KIND`] (channel decisions are
//! refused, Admin only), bound to the action, the target and a hash of the
//! exact change plus the target's current state. Running the same command
//! again after approval applies it once:
//!
//! - an approval counts only when an Admin decided it in the dashboard
//!   (`decided_by` starts with `dashboard:`) and it was decided within the
//!   validity window (`[responsibilities] operator_approval_minutes`,
//!   default 30);
//! - the state fingerprint carries `state_changed_at` and `control_epoch`,
//!   so after pause → resume → pause an old approval no longer matches;
//! - the approval is claimed before applying (exactly one run wins);
//! - requests for the same (action, target) and the same content are merged;
//!   different content is a separate request and invalidates nobody; a
//!   waiting or approved request made against an older state is invalidated
//!   (`state_changed`) and a new one filed;
//! - pushes are capped per target per hour ([`PUSH_CAP_PER_HOUR`]).
//!
//! Matching, merging, caps and consumption are the shared operator-CLI gate
//! (`approval::operator_cli_gate`); this module supplies the binding, the
//! cards, the texts and [`SPEC`].
//!
//! The emergency route is the dashboard (the task page's stop button runs
//! with a real identity) and the `[responsibilities] enabled` switch.
//!
//! Limits: this gate binds the product paths. An employee with unrestricted
//! Bash that evades the file guard can still rewrite `approvals.db` /
//! `tasks.db` directly; real isolation is not granting Bash, or the task
//! sandbox.

use std::path::Path;

use chrono::Utc;
use serde_json::{Value, json};

use super::sha256_hex;
use crate::approval::operator_cli_gate::{
    self as shared, Binding, Consume, Filing, KindSpec, StatePolicy, Validity,
};
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord};
use crate::task_store::{ResponsibilityRow, TaskRow, WakeupRow};

pub const ACTION_KIND: &str = "responsibility_operator_change";
/// The actor recorded for anything the terminal does.
pub const UNVERIFIED_ACTOR: &str = shared::UNVERIFIED_ACTOR;
const TTL_SECONDS: i64 = 24 * 3600;
/// Pushes per target per hour for terminal-filed requests.
pub const PUSH_CAP_PER_HOUR: usize = 2;
/// Default validity of an approval after its decision.
pub const DEFAULT_VALID_MINUTES: i64 = 30;

/// Fixed text on every card: the system cannot tell who ran the command.
pub const ORIGIN_NOTICE: &str = "這筆請求由本機指令列建立，系統無法確認下指令的人是誰。\
    請確認是你本人或你授權的人建立的，才核准；不確定就拒絕。";
/// A channel button or reply on one of these requests.
pub const CHANNEL_REFUSAL: &str =
    "這筆持續任務的變更請求只能在儀表板的待辦清單決定，請開啟儀表板處理（這裡的回覆不會生效）。";
/// An approval attempt on an expired request.
pub const EXPIRED_TEXT: &str =
    "這筆持續任務的變更請求已逾期並自動拒絕；需要的話請在終端機重新執行同一個指令。";
/// What the terminal prints while no usable approval exists.
pub const GO_TO_DASHBOARD: &str = "請到儀表板的待辦清單核准，核准後再執行一次同一個指令。";

/// Every action the command line can request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatedAction {
    Create,
    UpdateContract,
    Enable,
    Resume,
    ClearFailures,
    Pause,
    Disable,
    Stop,
}

impl GatedAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::UpdateContract => "update-contract",
            Self::Enable => "enable",
            Self::Resume => "resume",
            Self::ClearFailures => "clear-failures",
            Self::Pause => "pause",
            Self::Disable => "disable",
            Self::Stop => "stop",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "create" => Self::Create,
            "update-contract" => Self::UpdateContract,
            "enable" => Self::Enable,
            "resume" => Self::Resume,
            "clear-failures" => Self::ClearFailures,
            "pause" => Self::Pause,
            "disable" => Self::Disable,
            "stop" => Self::Stop,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Create => "建立新的持續任務",
            Self::UpdateContract => "修改內容或上限",
            Self::Enable => "重新啟用",
            Self::Resume => "恢復執行",
            Self::ClearFailures => "清除連續失敗並恢復",
            Self::Pause => "暫停協調",
            Self::Disable => "停用之後的排程",
            Self::Stop => "停止任務與它的子任務",
        }
    }
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    /// An approval for exactly this change and state was claimed: act now.
    Proceed(ApprovalId),
    /// A request was filed now.
    Requested(ApprovalId),
    /// A request for this change and state is still waiting.
    Pending(ApprovalId),
    /// L-4: this (action, target) already has [`MAX_PENDING_PER_TARGET`]
    /// different requests waiting (or the kind has
    /// `shared::DEFAULT_MAX_PENDING_PER_KIND`); no new row is filed.
    Throttled(usize),
    /// A usable approval existed but another run consumed or invalidated it
    /// first.
    AlreadyClaimed(ApprovalId),
}

/// L-4: different-content requests waiting for one (action, target).
pub const MAX_PENDING_PER_TARGET: usize = shared::DEFAULT_MAX_PENDING_PER_TARGET;

/// What the gate did besides its answer: approvals it voided and why
/// (`not_dashboard_decision` / `state_changed` / `approval_expired` /
/// `duplicate`), so the caller can audit them (D.1-3).
pub type Voided = Vec<(ApprovalId, &'static str)>;

/// The state a responsibility change is bound to (`None` for `create`).
pub fn state_fingerprint(r: Option<&ResponsibilityRow>) -> Value {
    match r {
        None => Value::Null,
        Some(r) => json!({
            "state": r.state,
            "state_changed_at": r.state_changed_at,
            "control_epoch": r.control_epoch,
            "contract_revision": r.contract_revision,
            "contract_hash": r.contract_hash,
            "consecutive_failures": r.consecutive_failures,
        }),
    }
}

/// The state a stop is bound to.
pub fn task_fingerprint(t: &TaskRow) -> Value {
    json!({
        "status": t.status,
        "authority_revision": t.authority_revision,
        "assigned_to": t.assigned_to,
    })
}

/// Hash of the exact change and the state it was requested against.
pub fn request_hash(op: &str, target: &str, args: &Value, state: &Value) -> String {
    sha256_hex(&json!([op, target, args, state]).to_string())
}

/// Hash of the exact change alone (the state is bound separately).
pub fn request_digest(op: &str, target: &str, args: &Value) -> String {
    sha256_hex(&json!([op, target, args]).to_string())
}

fn field<'a>(r: &'a ApprovalRecord, key: &str) -> Option<&'a str> {
    r.payload.get(key).and_then(|v| v.as_str())
}

/// Everything a request carries besides its card text.
pub struct GateRequest<'a> {
    pub action: GatedAction,
    pub target: &'a str,
    /// The employee the change concerns (the card's 「AI 員工」).
    pub owner: &'a str,
    pub args: &'a Value,
    pub state: &'a Value,
    /// Server-built card text ([`card_for_input`], [`card_for_row`],
    /// [`card_for_stop`]).
    pub card: &'a str,
    pub valid_minutes: i64,
}

/// Check for a usable approval, or file a request (see the module doc).
pub async fn gate(broker: &ApprovalBroker, req: &GateRequest<'_>) -> Result<Gate, String> {
    gate_with_voided(broker, req).await.map(|(g, _)| g)
}

/// [`gate`], also returning the approvals it voided.
pub async fn gate_with_voided(
    broker: &ApprovalBroker,
    req: &GateRequest<'_>,
) -> Result<(Gate, Voided), String> {
    let op = req.action.as_str();
    let digest = request_digest(op, req.target, req.args);
    let bind = Binding {
        action: op,
        target: req.target,
        request_digest: digest.clone(),
        state: req.state.to_string(),
        state_policy: StatePolicy::MustMatch,
        push_scope: None,
    };
    let filing = Filing {
        agent_id: req.owner,
        summary: req.card,
        extra: json!({
            "op": op,
            "target": req.target,
            "request_hash": digest,
            "args": req.args,
            "requested_by": UNVERIFIED_ACTOR,
        }),
        ttl_secs: TTL_SECONDS,
    };
    let (decided, voided) =
        shared::gate(broker, &SPEC, &bind, filing, Some(req.valid_minutes), Utc::now()).await?;
    let voided: Voided = voided.into_iter().map(|(id, why)| (id, why.as_str())).collect();
    let gate = match decided {
        shared::Gate::Proceed(claim) => Gate::Proceed(claim.id),
        shared::Gate::Requested(id) => Gate::Requested(id),
        shared::Gate::Pending(id) => Gate::Pending(id),
        shared::Gate::Throttled { waiting, .. } => Gate::Throttled(waiting),
        shared::Gate::AlreadyClaimed(id) => Gate::AlreadyClaimed(id),
    };
    Ok((gate, voided))
}

/// Employee-controlled text on a card: quoted, labelled, cut.
fn quoted(label: &str, text: &str, max_chars: usize) -> String {
    let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "{label}（使用者輸入，僅供參考）：「{}」",
        duduclaw_core::truncate_chars(&one_line, max_chars)
    )
}

fn limits_lines(
    occurrence_cap: i64,
    period: &str,
    period_limit: i64,
    period_count: i64,
    min_interval: i64,
    occurrence_hours: i64,
    stop_at: &str,
) -> String {
    format!(
        "每次花費上限：{occurrence_cap} 分（美元分）；每{period}花費上限：{period_limit} 分；\
         每{period}最多 {period_count} 次；兩次之間至少 {min_interval} 秒；每次最長 {occurrence_hours} 小時；\
         到 {stop_at} 停止",
        period = period_label(period),
    )
}

fn period_label(p: &str) -> &'static str {
    match p {
        "day" => "天",
        "week" => "週",
        "month" => "月",
        _ => "期",
    }
}

fn subscription_lines(subs: &[(String, Option<String>, Option<String>)]) -> String {
    if subs.is_empty() {
        return "事件喚醒：無".into();
    }
    subs.iter()
        .map(|(name, filter, timeout)| {
            format!(
                "事件喚醒：{name}，條件 {}，期限 {}",
                filter
                    .as_deref()
                    .map(|f| duduclaw_core::truncate_chars(f, 200).to_string())
                    .unwrap_or_else(|| "（不限，只看這位員工的事件）".into()),
                timeout.as_deref().unwrap_or("無"),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Card for `create` / `update-contract` from the operator's input file.
pub fn card_for_input(
    action: GatedAction,
    input: &super::service::ResponsibilityInput,
    before: Option<&ResponsibilityRow>,
) -> String {
    let schedule = input
        .schedule
        .as_ref()
        .map(|s| format!("排程：{}（{}）", s.cron, s.timezone))
        .unwrap_or_else(|| "排程：無".into());
    let subs: Vec<_> = input
        .event_subscriptions
        .iter()
        .map(|e| {
            (
                e.event_name.clone(),
                e.filter.as_ref().map(|f| f.to_string()),
                e.timeout_at.map(|t| t.to_rfc3339()),
            )
        })
        .collect();
    let mut out = format!(
        "持續任務：{}\nAI 員工：{}\n{}\n{}\n{}\n{}",
        action.label(),
        input.owner_agent_id,
        limits_lines(
            input.occurrence_cost_cap_cents,
            &input.budget_period,
            input.period_cost_limit_cents,
            input.period_occurrence_limit,
            input.min_wake_interval_secs,
            input.occurrence_hours,
            &input.stop_at.to_rfc3339(),
        ),
        schedule,
        subscription_lines(&subs),
        quoted("工作內容", &input.objective, 80),
    );
    if let Some(b) = before {
        out.push_str(&format!(
            "\n改動前：{}",
            limits_lines(
                b.occurrence_cost_cap_cents,
                &b.budget_period,
                b.period_cost_limit_cents,
                b.period_occurrence_limit,
                b.min_wake_interval_secs,
                b.occurrence_hours,
                &b.stop_at,
            )
        ));
    }
    out.push('\n');
    out.push_str(ORIGIN_NOTICE);
    out
}

/// Card for an action on an existing responsibility.
pub fn card_for_row(action: GatedAction, r: &ResponsibilityRow, subs: &[WakeupRow]) -> String {
    let subs: Vec<_> = subs
        .iter()
        .filter(|w| w.kind == "event" && w.control_epoch == r.control_epoch)
        .map(|w| {
            (
                w.event_name.clone().unwrap_or_default(),
                w.event_filter_json.clone(),
                w.due_at.clone(),
            )
        })
        .collect();
    format!(
        "持續任務：{}\nAI 員工：{}\n目前狀態：{}\n{}\n排程：{}\n{}\n{}\n{ORIGIN_NOTICE}",
        action.label(),
        r.owner_agent_id,
        r.state,
        limits_lines(
            r.occurrence_cost_cap_cents,
            &r.budget_period,
            r.period_cost_limit_cents,
            r.period_occurrence_limit,
            r.min_wake_interval_secs,
            r.occurrence_hours,
            &r.stop_at,
        ),
        r.schedule_json.as_deref().unwrap_or("無"),
        subscription_lines(&subs),
        quoted("工作內容", &r.objective, 80),
    )
}

/// Card for a stop.
pub fn card_for_stop(t: &TaskRow) -> String {
    format!(
        "任務：{}\nAI 員工：{}\n目前狀態：{}\n停止後，還沒開始的工作與所有子任務都會取消，無法恢復。\n{}\n{ORIGIN_NOTICE}",
        GatedAction::Stop.label(),
        t.assigned_to,
        t.status,
        quoted("任務標題", &t.title, 80),
    )
}

/// One security-audit row for a terminal action (`requested` / `applied` /
/// `refused`).
pub fn audit(
    home: &Path,
    outcome: &str,
    action: GatedAction,
    target: &str,
    owner: &str,
    detail: Value,
) {
    let mut detail = detail;
    detail["action"] = json!(action.as_str());
    detail["target"] = json!(target);
    detail["actor"] = json!(UNVERIFIED_ACTOR);
    detail["os_user"] = json!(std::env::var("USER").unwrap_or_default());
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            &format!("responsibility_cli_{outcome}"),
            owner,
            duduclaw_security::audit::Severity::Warning,
            detail,
        ),
    );
}

/// The channel notice: what is waiting, for whom, and that only the
/// dashboard can decide it. No contract text.
pub(crate) fn notice_body(rec: &ApprovalRecord, reminder: bool, deadline: &str) -> String {
    let head = if reminder {
        "⏰ 有一筆持續任務的變更請求快到期了，逾時會自動拒絕"
    } else {
        "📥 有一筆持續任務的變更請求等待管理員核准"
    };
    let op = field(rec, "op")
        .and_then(GatedAction::parse)
        .map(GatedAction::label)
        .unwrap_or("變更");
    format!(
        "{head}\n\
         AI 員工：{agent}\n\
         要做的事：{op}\n\
         {ORIGIN_NOTICE}\n\
         這類請求只能由管理員在儀表板的待辦清單核准。\n\
         期限：{deadline}未核准將自動拒絕\n\
         編號：{id}",
        agent = crate::goal_state::xml_escape(&duduclaw_core::truncate_chars(&rec.agent_id, 64)),
        id = duduclaw_core::truncate_chars(rec.id.as_str(), 8),
    )
}

/// Whether one more push for `rec`'s target fits this hour's cap (the shared
/// operator-CLI push cap). Past it an audit row is written and the request
/// stays in the dashboard inbox only.
pub async fn push_allowed(home: &Path, rec: &ApprovalRecord) -> bool {
    shared::push_allowed(home, rec).await
}

/// Security audit row for a push past the hourly cap.
fn audit_push_suppressed(home: &Path, rec: &ApprovalRecord, pushed: usize) {
    let target = shared::binding_of(rec)
        .map(|b| b.target)
        .or_else(|| field(rec, "target").map(str::to_string))
        .unwrap_or_default();
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "responsibility_approval_push_suppressed",
            &rec.agent_id,
            duduclaw_security::audit::Severity::Info,
            json!({"target": target, "pushed_last_hour": pushed}),
        ),
    );
}

/// This kind in the shared operator-CLI gate registry.
pub const SPEC: KindSpec = KindSpec {
    kind: ACTION_KIND,
    validity: Validity::Config(valid_minutes),
    consume: Consume::Once,
    reminders: false,
    max_pending_per_target: MAX_PENDING_PER_TARGET,
    max_pending_per_kind: shared::DEFAULT_MAX_PENDING_PER_KIND,
    push_cap_per_hour: Some(PUSH_CAP_PER_HOUR),
    legacy_scope_key: "target",
    admin_refusal: "這類持續任務變更只能由管理員決定。",
    expired_text: EXPIRED_TEXT,
    channel_refusal: Some(CHANNEL_REFUSAL),
    notice: notice_body,
    on_push_suppressed: audit_push_suppressed,
};

/// The validity window from `config.toml [responsibilities]
/// operator_approval_minutes` (1..=1440, default 30; unreadable ⇒ default).
pub fn valid_minutes(home: &Path) -> i64 {
    std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|s| s.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("responsibilities")?
                .get("operator_approval_minutes")?
                .as_integer()
        })
        .filter(|m| (1..=1440).contains(m))
        .unwrap_or(DEFAULT_VALID_MINUTES)
}

/// Open the broker (the CLI's entry point).
pub fn broker(home: &Path) -> Result<ApprovalBroker, String> {
    ApprovalBroker::open(home)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn file(broker: &ApprovalBroker, target: &str, n: i64) -> ApprovalId {
        broker
            .request(
                "alice",
                ACTION_KIND,
                "card",
                json!({"op": "pause", "target": target, "request_hash": format!("h{n}")}),
                600,
            )
            .await
            .unwrap()
    }

    /// S-M4: at most [`PUSH_CAP_PER_HOUR`] pushes per target per hour; past
    /// it the request stays in the dashboard only and an audit row says so.
    /// Another target is not affected.
    #[tokio::test]
    async fn pushes_are_capped_per_target_per_hour() {
        let home = tempfile::tempdir().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        for n in 0..PUSH_CAP_PER_HOUR as i64 {
            file(&broker, "r1", n).await;
        }
        let conn = rusqlite::Connection::open(home.path().join("approvals.db")).unwrap();
        conn.execute("UPDATE approvals SET notify_channel = 'telegram'", [])
            .unwrap();
        let next = file(&broker, "r1", 99).await;
        let rec = broker.get(&next).await.unwrap().unwrap();
        assert!(!push_allowed(home.path(), &rec).await);
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("responsibility_approval_push_suppressed"));
        let other = file(&broker, "r2", 100).await;
        let rec = broker.get(&other).await.unwrap().unwrap();
        assert!(push_allowed(home.path(), &rec).await);
    }

    #[test]
    fn the_validity_window_reads_config_and_falls_back() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(valid_minutes(home.path()), DEFAULT_VALID_MINUTES);
        std::fs::write(
            home.path().join("config.toml"),
            "[responsibilities]\noperator_approval_minutes = 5\n",
        )
        .unwrap();
        assert_eq!(valid_minutes(home.path()), 5);
        std::fs::write(
            home.path().join("config.toml"),
            "[responsibilities]\noperator_approval_minutes = 99999\n",
        )
        .unwrap();
        assert_eq!(valid_minutes(home.path()), DEFAULT_VALID_MINUTES);
    }
}
