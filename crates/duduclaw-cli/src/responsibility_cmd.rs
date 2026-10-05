//! `duduclaw responsibility …` — operator commands for continuous
//! responsibilities (P2-A, until the dashboard page exists).
//!
//! The command line cannot prove who typed a command: an AI employee with
//! Bash runs as the same OS user. So **no** state-changing action here takes
//! effect on its own (appendix D.1) — not even `pause`, `disable` or `stop`.
//! `--confirm` files a dashboard-only approval bound to the exact change and
//! the target's current state; after an Admin approves it in the dashboard
//! (within `[responsibilities] operator_approval_minutes`, default 30),
//! running the same command again applies it once
//! (`duduclaw_gateway::responsibility::operator_gate`). Every request, apply
//! and refusal writes a `responsibility_cli_*` security-audit row.
//!
//! The emergency route does not go through here: the task page's stop
//! button in the dashboard (a real identity) and the
//! `[responsibilities] enabled` switch.
//!
//! The AI-session environment check and the Bash-lane hook
//! (`GuardDecision::BlockedOperatorCommand` through
//! [`bash_responsibility_decision`]) are speed bumps only.

use std::path::{Path, PathBuf};

/// The operator-only subcommand this module owns on the Bash lane (every
/// `responsibility` subcommand, reads included). Same shared matcher as the
/// LINE inbox commands (`duduclaw_core::bash_operator_command_decision`).
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw responsibility",
        path: &["responsibility"],
    }];

/// The Bash-lane decision for the agent-file-guard hook: employees and
/// unverified callers may not run `duduclaw responsibility …`. A speed bump;
/// the dashboard approval in front of every change is the gate.
pub fn bash_responsibility_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}

use chrono::Utc;
use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::responsibility::operator_gate::{
    self as gate, Gate, GateRequest, GatedAction,
};
use duduclaw_gateway::responsibility::service::{self, ResponsibilityInput};
use duduclaw_gateway::task_store::{RespCas, ResponsibilityRow, TaskStore};
use serde_json::json;

#[derive(Debug, Subcommand)]
pub enum ResponsibilityCommands {
    /// List responsibilities (optionally of one employee)
    List {
        #[arg(long)]
        agent: Option<String>,
    },
    /// Show one responsibility with its schedule, window usage and subscriptions
    Get { id: String },
    /// List an responsibility's runs
    Occurrences { id: String },
    /// List an responsibility's wake facts (debugging)
    Fires { id: String },
    /// Create from a JSON contract file (needs dashboard approval)
    Create {
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        confirm: bool,
    },
    /// Change contract or limits from a JSON file (needs dashboard approval)
    UpdateContract {
        id: String,
        #[arg(long)]
        file: PathBuf,
        #[arg(long)]
        confirm: bool,
    },
    /// Pause coordination (needs dashboard approval)
    Pause {
        id: String,
        #[arg(long, default_value = "operator")]
        reason: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Resume after a pause (needs dashboard approval)
    Resume {
        id: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Disable future runs (needs dashboard approval)
    Disable {
        id: String,
        #[arg(long, default_value = "operator")]
        reason: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Re-enable a disabled responsibility (needs dashboard approval)
    Enable {
        id: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Clear the failure streak and resume (needs dashboard approval)
    ClearFailures {
        id: String,
        #[arg(long)]
        confirm: bool,
    },
    /// Stop a task and its sub-tasks (needs dashboard approval; the
    /// dashboard's stop button is the immediate route)
    Stop {
        task_id: String,
        #[arg(long)]
        confirm: bool,
    },
}

/// Where the command line looks up identity variables (injectable for tests).
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

fn process_env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn in_agent_session(env: EnvLookup<'_>) -> bool {
    [duduclaw_core::ENV_AGENT_ID, duduclaw_core::ENV_AGENT_TOKEN]
        .iter()
        .any(|k| env(k).is_some_and(|v| !v.trim().is_empty()))
}

const AGENT_SESSION_REFUSAL: &str =
    "這個指令不能在 AI 員工的工作階段中執行；請由管理者在自己的終端機執行。";

fn err(e: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Gateway(e.to_string())
}

fn print_json(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

async fn load(store: &TaskStore, id: &str) -> Result<ResponsibilityRow> {
    store
        .get_responsibility(id)
        .await
        .map_err(err)?
        .ok_or_else(|| err(format!("找不到持續任務 {id}")))
}

fn read_input(file: &Path) -> Result<ResponsibilityInput> {
    let raw = std::fs::read_to_string(file)
        .map_err(|e| err(format!("讀不到 {}: {e}", file.display())))?;
    serde_json::from_str(&raw).map_err(|e| err(format!("內容格式不正確：{e}")))
}

/// The action and target a command concerns (`None` for read commands).
fn action_of(cmd: &ResponsibilityCommands) -> Option<(GatedAction, String)> {
    use ResponsibilityCommands as C;
    Some(match cmd {
        C::List { .. } | C::Get { .. } | C::Occurrences { .. } | C::Fires { .. } => return None,
        C::Create { .. } => (GatedAction::Create, "new".into()),
        C::UpdateContract { id, .. } => (GatedAction::UpdateContract, id.clone()),
        C::Pause { id, .. } => (GatedAction::Pause, id.clone()),
        C::Resume { id, .. } => (GatedAction::Resume, id.clone()),
        C::Disable { id, .. } => (GatedAction::Disable, id.clone()),
        C::Enable { id, .. } => (GatedAction::Enable, id.clone()),
        C::ClearFailures { id, .. } => (GatedAction::ClearFailures, id.clone()),
        C::Stop { task_id, .. } => (GatedAction::Stop, task_id.clone()),
    })
}

/// Actions that enlarge what an employee may do or spend; refused outright
/// while `[responsibilities] enabled` is off (S-L7). Narrowing ones and
/// `stop` stay available.
fn widens(action: GatedAction) -> bool {
    matches!(
        action,
        GatedAction::Create
            | GatedAction::UpdateContract
            | GatedAction::Enable
            | GatedAction::Resume
            | GatedAction::ClearFailures
    )
}

pub async fn run(home: &Path, cmd: ResponsibilityCommands) -> Result<()> {
    run_with_env(home, cmd, &process_env).await
}

pub async fn run_with_env(
    home: &Path,
    cmd: ResponsibilityCommands,
    env: EnvLookup<'_>,
) -> Result<()> {
    let action = action_of(&cmd);
    if in_agent_session(env) {
        if let Some((a, target)) = &action {
            gate::audit(
                home,
                "refused",
                *a,
                target,
                "",
                json!({"reason": "agent_session"}),
            );
        }
        return Err(DuDuClawError::Agent(AGENT_SESSION_REFUSAL.into()));
    }
    if let Some((a, target)) = &action {
        if widens(*a)
            && !duduclaw_gateway::responsibility::ResponsibilityConfig::from_home(home).enabled
        {
            gate::audit(
                home,
                "refused",
                *a,
                target,
                "",
                json!({"reason": "feature_off"}),
            );
            return Err(err(
                "持續任務功能目前關閉（config.toml [responsibilities] enabled = false），不接受這類變更。",
            ));
        }
    }
    let store = TaskStore::open(home).map_err(err)?;
    let now = Utc::now();
    use ResponsibilityCommands as C;
    match cmd {
        C::List { agent } => {
            let rows = store
                .list_responsibilities(agent.as_deref())
                .await
                .map_err(err)?;
            print_json(&json!({ "responsibilities": rows }));
        }
        C::Get { id } => {
            let cost = duduclaw_gateway::responsibility::TelemetryCostSource::new(home);
            let s = duduclaw_gateway::responsibility::summary::summary(&store, &cost, &id, now)
                .await
                .map_err(err)?;
            print_json(&json!({ "summary": s }));
        }
        C::Occurrences { id } => {
            print_json(&json!({ "occurrences": store.list_occurrences(&id).await.map_err(err)? }));
        }
        C::Fires { id } => {
            print_json(&json!({ "fires": store.list_fires(&id, 200).await.map_err(err)? }));
        }
        C::Create { file, confirm } => {
            let input = read_input(&file)?;
            service::check_input(home, &input, now).map_err(err)?;
            // M3-2: say it before the request is filed, not only after.
            for w in duduclaw_gateway::responsibility::usage_hint::usage_warnings(
                home,
                &input.owner_agent_id,
            ) {
                eprintln!("注意：{w}");
            }
            let card = gate::card_for_input(GatedAction::Create, &input, None);
            let args = serde_json::to_value(&input).map_err(err)?;
            let target = format!("new:{}", input.owner_agent_id);
            let req = GateRequest {
                action: GatedAction::Create,
                target: &target,
                owner: &input.owner_agent_id,
                args: &args,
                state: &serde_json::Value::Null,
                card: &card,
                valid_minutes: gate::valid_minutes(home),
            };
            let Some(actor) = through_gate(home, &req, confirm).await? else {
                return Ok(());
            };
            let applied = service::create(&store, home, &input, &actor, now).await;
            match applied {
                Ok(r) => {
                    gate::audit(
                        home,
                        "applied",
                        req.action,
                        &target,
                        req.owner,
                        json!({"responsibility_id": r.responsibility_id, "decided_by": actor}),
                    );
                    println!("已建立持續任務 {}", r.responsibility_id);
                }
                Err(e) => return Err(refused_after_approval(home, &req, &actor, e)),
            }
        }
        C::UpdateContract { id, file, confirm } => {
            let r = load(&store, &id).await?;
            let input = read_input(&file)?;
            service::check_input(home, &input, now).map_err(err)?;
            let card = gate::card_for_input(GatedAction::UpdateContract, &input, Some(&r));
            let args = serde_json::to_value(&input).map_err(err)?;
            let state = gate::state_fingerprint(Some(&r));
            let req = GateRequest {
                action: GatedAction::UpdateContract,
                target: &id,
                owner: &r.owner_agent_id,
                args: &args,
                state: &state,
                card: &card,
                valid_minutes: gate::valid_minutes(home),
            };
            let Some(actor) = through_gate(home, &req, confirm).await? else {
                return Ok(());
            };
            let cas = service::update_contract(
                &store,
                home,
                &id,
                r.contract_revision,
                &input,
                &actor,
                now,
            )
            .await;
            report(home, &req, &actor, cas)?;
        }
        C::Enable { id, confirm } => {
            state_op(home, &store, GatedAction::Enable, &id, None, confirm, now).await?
        }
        C::Resume { id, confirm } => {
            state_op(home, &store, GatedAction::Resume, &id, None, confirm, now).await?
        }
        C::ClearFailures { id, confirm } => {
            state_op(
                home,
                &store,
                GatedAction::ClearFailures,
                &id,
                None,
                confirm,
                now,
            )
            .await?
        }
        C::Pause {
            id,
            reason,
            confirm,
        } => {
            state_op(
                home,
                &store,
                GatedAction::Pause,
                &id,
                Some(&reason),
                confirm,
                now,
            )
            .await?
        }
        C::Disable {
            id,
            reason,
            confirm,
        } => {
            state_op(
                home,
                &store,
                GatedAction::Disable,
                &id,
                Some(&reason),
                confirm,
                now,
            )
            .await?
        }
        C::Stop { task_id, confirm } => stop(home, &store, &task_id, confirm, now).await?,
    }
    Ok(())
}

/// Without `--confirm`: print the card. With it: run the gate. `Some(actor)`
/// only when a dashboard approval for exactly this change was claimed.
async fn through_gate(home: &Path, req: &GateRequest<'_>, confirm: bool) -> Result<Option<String>> {
    if !confirm {
        println!(
            "{}\n\n這個動作需要管理員在儀表板核准。加上 --confirm 送出請求。",
            req.card
        );
        return Ok(None);
    }
    let broker = gate::broker(home).map_err(err)?;
    let (verdict, voided) = gate::gate_with_voided(&broker, req).await.map_err(err)?;
    for (id, reason) in &voided {
        gate::audit(
            home,
            "refused",
            req.action,
            req.target,
            req.owner,
            json!({"approval_id": id.as_str(), "reason": reason}),
        );
    }
    match verdict {
        Gate::Proceed(id) => {
            let decided_by = broker
                .get(&id)
                .await
                .ok()
                .flatten()
                .and_then(|r| r.decided_by)
                .unwrap_or_default();
            Ok(Some(format!(
                "operator-cli:approved:{}:{}",
                duduclaw_core::truncate_chars(id.as_str(), 8),
                duduclaw_core::truncate_chars(&decided_by, 80)
            )))
        }
        Gate::Requested(id) => {
            gate::audit(
                home,
                "requested",
                req.action,
                req.target,
                req.owner,
                json!({"approval_id": id.as_str()}),
            );
            println!(
                "已送出核准請求（編號 {}）。{}",
                duduclaw_core::truncate_chars(id.as_str(), 8),
                gate::GO_TO_DASHBOARD
            );
            Ok(None)
        }
        Gate::Throttled(n) => {
            gate::audit(
                home,
                "refused",
                req.action,
                req.target,
                req.owner,
                json!({"reason": "too_many_waiting", "waiting": n}),
            );
            Err(err(format!(
                "這個對象已有 {n} 筆不同內容的請求在等核准，沒有再送出新的。請先到儀表板處理。"
            )))
        }
        Gate::Pending(id) => {
            gate::audit(
                home,
                "requested",
                req.action,
                req.target,
                req.owner,
                json!({"approval_id": id.as_str(), "existing": true}),
            );
            println!(
                "這筆變更還在等管理員在儀表板核准（編號 {}）。{}",
                duduclaw_core::truncate_chars(id.as_str(), 8),
                gate::GO_TO_DASHBOARD
            );
            Ok(None)
        }
    }
}

fn refused_after_approval(
    home: &Path,
    req: &GateRequest<'_>,
    actor: &str,
    e: impl std::fmt::Display,
) -> DuDuClawError {
    let msg = e.to_string();
    gate::audit(
        home,
        "refused",
        req.action,
        req.target,
        req.owner,
        json!({"reason": "apply_failed", "decided_by": actor,
               "error": duduclaw_core::truncate_chars(&msg, 200)}),
    );
    err(format!("已核准，但套用失敗：{msg}"))
}

fn report<E: std::fmt::Display>(
    home: &Path,
    req: &GateRequest<'_>,
    actor: &str,
    cas: std::result::Result<RespCas, E>,
) -> Result<()> {
    match cas {
        Ok(RespCas::Applied(r)) => {
            gate::audit(
                home,
                "applied",
                req.action,
                req.target,
                req.owner,
                json!({"decided_by": actor, "after": {"state": r.state,
                       "epoch": r.control_epoch, "revision": r.contract_revision}}),
            );
            println!(
                "已套用：狀態 {}（epoch {}，內容版本 {}）",
                r.state, r.control_epoch, r.contract_revision
            );
            Ok(())
        }
        Ok(RespCas::Conflict(r)) => {
            gate::audit(
                home,
                "refused",
                req.action,
                req.target,
                req.owner,
                json!({"reason": "conflict", "decided_by": actor}),
            );
            Err(err(format!(
                "現況已改變，沒有套用（目前狀態：{}）。請重新查看後再執行。",
                r.map(|r| r.state).unwrap_or_else(|| "不存在".into())
            )))
        }
        Err(e) => Err(refused_after_approval(home, req, actor, e)),
    }
}

/// enable / resume / clear-failures / pause / disable: bound to the
/// responsibility's current state.
async fn state_op(
    home: &Path,
    store: &TaskStore,
    action: GatedAction,
    id: &str,
    reason: Option<&str>,
    confirm: bool,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    let r = load(store, id).await?;
    let subs = store.list_wakeups(id).await.map_err(err)?;
    let card = gate::card_for_row(action, &r, &subs);
    let args = reason.map_or(serde_json::Value::Null, |r| json!({ "reason": r }));
    let state = gate::state_fingerprint(Some(&r));
    let req = GateRequest {
        action,
        target: id,
        owner: &r.owner_agent_id,
        args: &args,
        state: &state,
        card: &card,
        valid_minutes: gate::valid_minutes(home),
    };
    let Some(actor) = through_gate(home, &req, confirm).await? else {
        return Ok(());
    };
    let epoch = r.control_epoch;
    let reason = reason.unwrap_or("operator");
    let cas = match action {
        GatedAction::Enable => service::enable(store, id, epoch, &actor, now).await,
        GatedAction::Resume => service::resume(store, id, epoch, &actor, now).await,
        GatedAction::Pause => service::pause(store, id, epoch, &actor, reason, now).await,
        GatedAction::Disable => service::disable(store, id, epoch, &actor, reason, now).await,
        _ => service::clear_failures(store, id, epoch, &actor, now).await,
    };
    report(home, &req, &actor, cas)
}

async fn stop(
    home: &Path,
    store: &TaskStore,
    task_id: &str,
    confirm: bool,
    now: chrono::DateTime<Utc>,
) -> Result<()> {
    let t = store
        .get_task(task_id)
        .await
        .map_err(err)?
        .ok_or_else(|| err(format!("找不到任務 {task_id}")))?;
    let card = gate::card_for_stop(&t);
    let state = gate::task_fingerprint(&t);
    let req = GateRequest {
        action: GatedAction::Stop,
        target: task_id,
        owner: &t.assigned_to,
        args: &serde_json::Value::Null,
        state: &state,
        card: &card,
        valid_minutes: gate::valid_minutes(home),
    };
    let Some(actor) = through_gate(home, &req, confirm).await? else {
        return Ok(());
    };
    let queue = duduclaw_gateway::message_queue::MessageQueue::open(home).map_err(err)?;
    let broker = gate::broker(home).ok();
    // A dashboard Admin decided this stop: it is a manager decision and does
    // not count as an unsuccessful occurrence (S-H1).
    let st = duduclaw_gateway::responsibility::stop::stop_task(
        store,
        &queue,
        broker.as_ref(),
        None,
        home,
        task_id,
        t.authority_revision,
        &actor,
        false,
        now,
    )
    .await
    .map_err(|e| refused_after_approval(home, &req, &actor, e))?;
    gate::audit(
        home,
        "applied",
        req.action,
        task_id,
        req.owner,
        json!({"decided_by": actor, "state": st.state}),
    );
    println!(
        "停止狀態：{}（{}）",
        st.state,
        match st.state.as_str() {
            "stopped" => "已停止",
            "stopped_uncertain" => "已停止，但有工作或外部動作的結果無法確認，需要人工確認",
            _ => "還有進行中的工作，跑完就停",
        }
    );
    Ok(())
}

#[cfg(test)]
#[path = "responsibility_cmd_tests.rs"]
mod tests;
