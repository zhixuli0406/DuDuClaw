//! `duduclaw ops channel-ingress` — operator view and resolution of the LINE
//! durable inbox (hidden; review I-HIGH-4 (3)).
//!
//! - `list` / `show` answer directly (never payloads or reply tokens).
//! - `resolve` (close, or `--retry` for events proven not executed) and
//!   `rerun` (needs `--confirm-duplicate-risk`) never act on the first run:
//!   they file a dashboard-only approval and exit non-zero. After an Admin
//!   approves it in the dashboard, running the same command again applies it
//!   once. The terminal cannot tell the operator from an AI employee with
//!   Bash; the dashboard RPCs `channel_ingress.*`, which know who is asking,
//!   stay immediate. See `duduclaw_gateway::channel_ingress::cli_approval`.
//! - `batch` files one approval for many events in one state (review N5);
//!   applied it resolves the events whose state is unchanged and lists the
//!   ones that changed. See `duduclaw_gateway::channel_ingress::cli_batch`.
//! - Refused inside an AI employee's session ([`SESSION_ENV_VARS`]).

use std::path::Path;

use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::channel_ingress::cli_approval::{
    self, CliAction, CliOutcome, CliRequest, GO_TO_DASHBOARD,
};
use duduclaw_gateway::channel_ingress::cli_batch::{self, BatchRequest};

/// `--action` of `batch`.
#[derive(clap::ValueEnum, Clone, Copy, Debug)]
pub enum BatchAction {
    Close,
    Retry,
    Rerun,
}

#[derive(Subcommand, Debug)]
pub enum ChannelIngressCommands {
    /// List recent LINE inbox events and counts per state
    List {
        /// Page: only events older than this sequence number
        #[arg(long)]
        before_seq: Option<i64>,
    },
    /// Show one event with its attempts and run authorizations
    Show { ingress_id: String },
    /// Close an event, or retry one proven not executed (needs dashboard approval)
    Resolve {
        ingress_id: String,
        /// Why (stored with the resolution)
        #[arg(long)]
        note: String,
        /// Retry instead of close (only for events that never ran)
        #[arg(long)]
        retry: bool,
        /// Provider receipt you checked (for example LINE's request id)
        #[arg(long)]
        provider_receipt: Option<String>,
    },
    /// Run an uncertain or undelivered event again (may duplicate; needs dashboard approval)
    Rerun {
        ingress_id: String,
        #[arg(long)]
        note: String,
        /// Required: the answer may already have been produced or delivered
        #[arg(long)]
        confirm_duplicate_risk: bool,
        #[arg(long)]
        provider_receipt: Option<String>,
    },
    /// Close, retry or rerun many events in one state with one dashboard approval
    Batch {
        #[arg(long, value_enum)]
        action: BatchAction,
        /// uncertain, quarantined, undelivered or failed_before_dispatch
        #[arg(long)]
        status: String,
        /// Only events with this reason code
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        note: String,
        /// Required for rerun: answers may be produced or delivered twice
        #[arg(long)]
        confirm_duplicate_risk: bool,
        /// Most events in this batch (default 200, at most 500)
        #[arg(long, default_value_t = cli_batch::DEFAULT_BATCH)]
        limit: usize,
    },
}

/// Variables a DuDuClaw-spawned process carries. Any of them present and
/// non-empty refuses the command. One list for every operator-only command
/// (`crate::ai_session_guard::AI_SESSION_ENV_VARS`).
pub const SESSION_ENV_VARS: &[&str] = crate::ai_session_guard::AI_SESSION_ENV_VARS;

/// The operator-only subcommands this module owns on the Bash lane. Other
/// features add their own entries to their own lists and call the same
/// shared matcher (`duduclaw_core::bash_operator_command_decision`).
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw ops channel-ingress",
        path: &["ops", "channel-ingress"],
    }];

/// The Bash-lane decision for the agent-file-guard hook: employees and
/// unverified callers may not run `duduclaw ops channel-ingress`. A speed
/// bump: the command is read the way bash reads it (continuations, quote
/// concatenation, stuck redirects), but variables, aliases, scripts and
/// renamed binaries still get past it; the dashboard approval is the gate.
pub fn bash_channel_ingress_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}

fn agent_session(get: &impl Fn(&str) -> Option<String>) -> bool {
    SESSION_ENV_VARS
        .iter()
        .any(|k| get(k).is_some_and(|v| !v.trim().is_empty()))
}

fn print(v: &serde_json::Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

pub async fn run(home: &Path, cmd: ChannelIngressCommands) -> Result<()> {
    // `var_os`, not `var`: a non-UTF-8 value must still count as present
    // (`var(..).ok()` would read it as unset and skip the session check).
    run_with_env(home, cmd, |k| {
        std::env::var_os(k).map(|v| v.to_string_lossy().into_owned())
    })
    .await
}

/// [`run`] with the environment lookup injected (tests share one process).
async fn run_with_env(
    home: &Path,
    cmd: ChannelIngressCommands,
    env: impl Fn(&str) -> Option<String>,
) -> Result<()> {
    let request = match &cmd {
        ChannelIngressCommands::Resolve {
            ingress_id,
            note,
            retry,
            provider_receipt,
        } => Some(CliRequest {
            action: if *retry {
                CliAction::Retry
            } else {
                CliAction::Close
            },
            ingress_id: ingress_id.clone(),
            note: note.clone(),
            provider_receipt: provider_receipt.clone(),
            confirm_duplicate_risk: false,
        }),
        ChannelIngressCommands::Rerun {
            ingress_id,
            note,
            confirm_duplicate_risk,
            provider_receipt,
        } => Some(CliRequest {
            action: CliAction::Rerun,
            ingress_id: ingress_id.clone(),
            note: note.clone(),
            provider_receipt: provider_receipt.clone(),
            confirm_duplicate_risk: *confirm_duplicate_risk,
        }),
        _ => None,
    };
    if agent_session(&env) {
        if matches!(cmd, ChannelIngressCommands::Batch { .. }) {
            cli_approval::audit(home, "refused", "batch", "batch", None, "agent_session_env");
        }
        if let Some(req) = &request {
            cli_approval::audit(
                home,
                "refused",
                req.action.as_str(),
                &req.ingress_id,
                None,
                "agent_session_env",
            );
        }
        return Err(DuDuClawError::Agent(
            "這個指令不能在 AI 員工的工作階段中執行；LINE 收件匣只能由管理者在自己的終端機或儀表板處理。"
                .into(),
        ));
    }
    match cmd {
        ChannelIngressCommands::List { before_seq } => {
            print(&cli_approval::list(home, before_seq).await.map_err(DuDuClawError::Agent)?);
            Ok(())
        }
        ChannelIngressCommands::Show { ingress_id } => {
            print(&cli_approval::show(home, &ingress_id).await.map_err(DuDuClawError::Agent)?);
            Ok(())
        }
        ChannelIngressCommands::Batch {
            action,
            status,
            reason,
            note,
            confirm_duplicate_risk,
            limit,
        } => {
            let req = BatchRequest {
                action: match action {
                    BatchAction::Close => CliAction::Close,
                    BatchAction::Retry => CliAction::Retry,
                    BatchAction::Rerun => CliAction::Rerun,
                },
                status,
                reason,
                note,
                confirm_duplicate_risk,
                limit,
            };
            let outcome = cli_batch::batch_request_or_apply(home, &req)
                .await
                .map_err(DuDuClawError::Agent)?;
            finish(outcome)
        }
        ChannelIngressCommands::Rerun {
            confirm_duplicate_risk: false,
            ..
        } => Err(DuDuClawError::Agent(
            "重新執行可能讓客人收到重複的回覆或重複觸發工具；確定要做請加上 --confirm-duplicate-risk。"
                .into(),
        )),
        _ => {
            let req = request.expect("state-changing command has a request");
            let outcome = cli_approval::request_or_apply(home, &req)
                .await
                .map_err(DuDuClawError::Agent)?;
            finish(outcome)
        }
    }
}

fn finish(outcome: CliOutcome) -> Result<()> {
    match outcome {
        CliOutcome::Applied(v) => {
            print(&v);
            Ok(())
        }
        CliOutcome::Requested(id) | CliOutcome::Pending(id) => Err(DuDuClawError::Agent(format!(
            "{GO_TO_DASHBOARD}（核准編號 {id}）。管理員核准後，在 30 分鐘內再執行一次同一個指令；事件狀態有變動就要重新核准（批次只處理狀態沒有變動的事件）。"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct Probe {
        #[command(subcommand)]
        cmd: ChannelIngressCommands,
    }

    #[test]
    fn parses_the_four_verbs() {
        let id = "a".repeat(64);
        assert!(Probe::try_parse_from(["x", "list"]).is_ok());
        assert!(Probe::try_parse_from(["x", "show", &id]).is_ok());
        assert!(Probe::try_parse_from(["x", "resolve", &id, "--note", "n", "--retry"]).is_ok());
        assert!(
            Probe::try_parse_from(["x", "resolve", &id]).is_err(),
            "note is required"
        );
        assert!(
            Probe::try_parse_from(["x", "rerun", &id, "--note", "n", "--confirm-duplicate-risk"])
                .is_ok()
        );
        assert!(
            Probe::try_parse_from([
                "x",
                "batch",
                "--action",
                "close",
                "--status",
                "quarantined",
                "--reason",
                "snapshot_unavailable",
                "--note",
                "n"
            ])
            .is_ok()
        );
        assert!(
            Probe::try_parse_from([
                "x", "batch", "--action", "delete", "--status", "x", "--note", "n"
            ])
            .is_err()
        );
    }

    #[test]
    fn bash_lane_blocks_employees_and_unverified_callers_only() {
        use duduclaw_core::HookCaller;
        for cmd in [
            "duduclaw ops channel-ingress list",
            "/opt/homebrew/bin/duduclaw-pro --verbose ops channel-ingress rerun x --note n",
            "cd /tmp && 'duduclaw' ops \"channel-ingress\" show x",
            "echo hi; duduclaw.exe OPS channel-ingress resolve x --note n",
            // Review L8: the spellings bash still runs as the same command.
            "duduclaw ops \\\nchannel-ingress list",
            "duduclaw ops chan''nel-ingress list",
            "duduclaw ops channel-ingress>out list",
        ] {
            for caller in [
                HookCaller::Agent("alice".into()),
                HookCaller::Untrusted("mallory".into()),
            ] {
                let d = bash_channel_ingress_decision(cmd, &caller);
                assert!(
                    matches!(
                        d,
                        Some(duduclaw_core::GuardDecision::BlockedOperatorCommand { .. })
                    ),
                    "{cmd}"
                );
                let msg = d.unwrap().block_message().unwrap();
                assert!(msg.contains("LINE") && msg.contains("儀表板"), "{msg}");
            }
            assert!(bash_channel_ingress_decision(cmd, &HookCaller::Absent).is_none());
        }
        for cmd in [
            "duduclaw ops doctor",
            "echo ops channel-ingress",
            "grep channel-ingress docs/guides/durable-line-ingress.md",
            "duduclaw channel-ingress list",
        ] {
            assert!(
                bash_channel_ingress_decision(cmd, &HookCaller::Agent("alice".into())).is_none(),
                "{cmd}"
            );
        }
    }

    #[tokio::test]
    async fn refused_inside_an_ai_session_and_before_any_store_access() {
        let dir = tempfile::tempdir().unwrap();
        let err = run_with_env(
            dir.path(),
            ChannelIngressCommands::List { before_seq: None },
            |k| (k == "DUDUCLAW_AGENT_ID").then(|| "alice".to_string()),
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("AI 員工"));
        assert!(!dir.path().join("channel_ingress.db").exists());
    }

    #[tokio::test]
    async fn rerun_without_confirmation_and_missing_inbox_are_errors() {
        let dir = tempfile::tempdir().unwrap();
        let no_env = |_: &str| None;
        assert!(
            run_with_env(
                dir.path(),
                ChannelIngressCommands::Rerun {
                    ingress_id: "a".repeat(64),
                    note: "n".into(),
                    confirm_duplicate_risk: false,
                    provider_receipt: None,
                },
                no_env,
            )
            .await
            .is_err()
        );
        assert!(
            run_with_env(
                dir.path(),
                ChannelIngressCommands::List { before_seq: None },
                no_env
            )
            .await
            .is_err()
        );
        assert!(!dir.path().join("channel_ingress.db").exists());
    }
}
