//! Local publisher pins. A terminal change requires a single-use dashboard
//! Admin decision bound to the exact request and current trust-store bytes.

use std::io::Write;
use std::path::Path;

use clap::Subcommand;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::publishers::{Publisher, TrustStore, validate_pubkey, validate_publisher_id};
use duduclaw_gateway::approval::{ApprovalBroker, operator_cli_gate as gate};
use serde_json::json;

#[derive(Debug, Subcommand)]
pub enum PublisherCommands {
    /// List pinned keys (including the built-in official release key).
    List,
    /// Add a publisher key after dashboard Admin approval.
    Add { id: String, pubkey: String },
    /// Remove a publisher's configured keys after dashboard Admin approval.
    Remove { id: String },
}

pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] =
    &[duduclaw_core::OperatorCommand {
        name: "duduclaw publishers",
        path: &["publishers"],
    }];

pub fn bash_publishers_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}

fn err(message: impl Into<String>) -> DuDuClawError {
    DuDuClawError::Config(message.into())
}

// Missing is distinct from an existing empty file in the approval binding.
fn snapshot(home: &Path) -> Result<Option<Vec<u8>>> {
    let path = home.join("trusted_publishers.toml");
    match std::fs::symlink_metadata(&path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
            return Err(err("信任發行者檔案必須是一般檔案，不能是符號連結。"));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(err(format!("無法讀取信任發行者檔案：{e}"))),
    }
    std::fs::read(&path)
        .map(Some)
        .map_err(|e| err(format!("無法讀取信任發行者檔案：{e}")))
}

fn state_of(bytes: Option<&[u8]>) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update([u8::from(bytes.is_some())]);
    hash.update(bytes.unwrap_or_default());
    hex::encode(hash.finalize())
}

fn parse_snapshot(bytes: Option<&[u8]>) -> Result<TrustStore> {
    match bytes {
        None => Ok(TrustStore::default()),
        Some(bytes) => TrustStore::parse(
            std::str::from_utf8(bytes)
                .map_err(|_| err("信任發行者檔案不是 UTF-8，沒有修改檔案。"))?,
        )
        .map_err(|e| err(format!("信任發行者檔案無效，沒有修改檔案：{e}"))),
    }
}

fn audit(home: &Path, phase: &str, action: &str, id: &str, approval: Option<&str>) {
    let event = duduclaw_security::audit::AuditEvent::new(
        "trusted_publishers_cli_action",
        gate::UNVERIFIED_ACTOR,
        duduclaw_security::audit::Severity::Info,
        json!({"phase": phase, "action": action, "publisher": id, "approval_id": approval}),
    );
    duduclaw_security::audit::append_audit_event(home, &event);
}

pub async fn run(home: &Path, command: PublisherCommands) -> Result<()> {
    run_with_markers(home, command, &crate::ai_session_guard::command_markers()).await
}

async fn run_with_markers(home: &Path, command: PublisherCommands, markers: &[&str]) -> Result<()> {
    // Present-but-empty and non-UTF-8 environment values count too.
    if !markers.is_empty() {
        return Err(err(
            "信任發行者指令不能在 AI 員工的工作階段中執行，請由管理者在自己的終端機操作。",
        ));
    }
    if matches!(command, PublisherCommands::List) {
        let store = TrustStore::load(home).map_err(err)?;
        println!(
            "duduclaw (built-in): {}",
            duduclaw_core::publishers::OFFICIAL_PUBLISHER_KEY
        );
        for (id, publisher) in store.publishers {
            for key in publisher.keys {
                println!("{id}: {key}");
            }
        }
        return Ok(());
    }
    let (action, id, key) = match &command {
        PublisherCommands::Add { id, pubkey } => ("add", id.as_str(), Some(pubkey.as_str())),
        PublisherCommands::Remove { id } => ("remove", id.as_str(), None),
        PublisherCommands::List => unreachable!(),
    };
    validate_publisher_id(id).map_err(err)?;
    if id == "duduclaw" {
        return Err(err(
            "duduclaw 使用程式內建的正式發行金鑰，不能在信任清單中新增、覆寫或移除。",
        ));
    }
    if let Some(key) = key {
        // Refuse malformed keys before opening the broker or filing a card.
        validate_pubkey(key).map_err(err)?;
    }
    let before = snapshot(home)?;
    let mut updated = parse_snapshot(before.as_deref())?;
    match key {
        Some(key) => {
            let publisher = updated
                .publishers
                .entry(id.into())
                .or_insert_with(|| Publisher { keys: Vec::new() });
            if !publisher.keys.iter().any(|k| k == key) {
                publisher.keys.push(key.into());
            }
        }
        None => {
            updated.publishers.remove(id);
        }
    }
    let state = state_of(before.as_deref());
    let binding = gate::Binding {
        action,
        target: id,
        request_digest: gate::digest(&[action, id, key.unwrap_or("")]),
        state: state.clone(),
        state_policy: gate::StatePolicy::MustMatch,
        push_scope: None,
    };
    let summary = format!(
        "信任發行者：{action}，發行者 {id}，金鑰 {}。這筆請求由本機指令列建立，系統無法確認下指令的人是誰。簽章只證明來源，安裝仍須掃描、隔離與核准。",
        key.unwrap_or("（移除全部設定金鑰）"),
    );
    let broker = ApprovalBroker::open(home).map_err(err)?;
    let (decision, voided) = gate::gate(
        &broker,
        &gate::PUBLISHERS_SPEC,
        &binding,
        gate::Filing {
            agent_id: "system",
            summary: &summary,
            extra: json!({"publisher": id, "action": action, "pubkey": key}),
            ttl_secs: 86_400,
        },
        Some(30),
        chrono::Utc::now(),
    )
    .await
    .map_err(err)?;
    for (approval, _) in voided {
        audit(home, "refused", action, id, Some(approval.as_str()));
    }
    let claim = match decision {
        gate::Gate::Proceed(claim) => claim,
        gate::Gate::Requested(approval) | gate::Gate::Pending(approval) => {
            audit(home, "requested", action, id, Some(approval.as_str()));
            return Err(err(format!(
                "請到儀表板的待辦核准，由管理員核准後再執行同一個指令（核准編號 {approval}）。"
            )));
        }
        gate::Gate::Throttled { .. } => {
            return Err(err("已有太多信任發行者請求等待核准，請先到儀表板處理。"));
        }
        gate::Gate::AlreadyClaimed(_) => {
            return Err(err("這筆核准已使用或作廢，請重新執行指令申請。"));
        }
    };
    let text = toml::to_string_pretty(&updated).map_err(|e| err(e.to_string()))?;
    let path = home.join("trusted_publishers.toml");
    let applied = duduclaw_core::fs_lock::with_file_lock(&path, || {
        let current = snapshot(home).map_err(|e| std::io::Error::other(e.to_string()))?;
        if !claim.still_bound(&state_of(current.as_deref())) {
            return Err(std::io::Error::other(
                "信任清單在核准後已改變，沒有修改檔案；核准已使用，請重新申請。",
            ));
        }
        let mut temporary = tempfile::NamedTempFile::new_in(home)?;
        temporary.write_all(text.as_bytes())?;
        temporary.as_file().sync_all()?;
        temporary.persist(&path).map_err(|e| e.error)?;
        #[cfg(unix)]
        std::fs::File::open(home)?.sync_all()?;
        Ok(())
    });
    match applied {
        Ok(()) => {
            audit(home, "applied", action, id, Some(claim.id.as_str()));
            println!("已依管理員核准更新信任發行者：{id}");
            Ok(())
        }
        Err(e) => {
            audit(home, "refused", action, id, Some(claim.id.as_str()));
            Err(err(e.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn any_session_marker_refuses_even_list() {
        let home = tempfile::tempdir().unwrap();
        for key in crate::ai_session_guard::AI_SESSION_ENV_VARS {
            assert!(
                run_with_markers(home.path(), PublisherCommands::List, &[*key])
                    .await
                    .is_err()
            );
        }
        assert!(!home.path().join("approvals.db").exists());
    }

    #[tokio::test]
    async fn malformed_key_is_refused_before_filing() {
        let home = tempfile::tempdir().unwrap();
        assert!(
            run_with_markers(
                home.path(),
                PublisherCommands::Add {
                    id: "example".into(),
                    pubkey: "bad".into(),
                },
                &[]
            )
            .await
            .is_err()
        );
        assert!(!home.path().join("approvals.db").exists());
    }

    #[tokio::test]
    async fn add_and_remove_require_single_use_dashboard_decisions() {
        let home = tempfile::tempdir().unwrap();
        let add = || PublisherCommands::Add {
            id: "example".into(),
            pubkey: duduclaw_core::publishers::OFFICIAL_PUBLISHER_KEY.into(),
        };
        assert!(run_with_markers(home.path(), add(), &[]).await.is_err());
        assert!(!home.path().join("trusted_publishers.toml").exists());
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let rec = broker
            .list_by_kind(gate::PUBLISHERS_ACTION_KIND)
            .await
            .unwrap()
            .remove(0);
        broker
            .decide(&rec.id, true, "telegram:admin")
            .await
            .unwrap();
        assert!(run_with_markers(home.path(), add(), &[]).await.is_err());
        let pending = broker
            .list_by_kind(gate::PUBLISHERS_ACTION_KIND)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.status == duduclaw_gateway::approval::ApprovalStatus::Pending)
            .unwrap();
        broker
            .decide(&pending.id, true, "dashboard:admin")
            .await
            .unwrap();
        run_with_markers(home.path(), add(), &[]).await.unwrap();
        assert_eq!(
            TrustStore::load(home.path()).unwrap().publishers["example"]
                .keys
                .len(),
            1
        );
        assert!(
            run_with_markers(
                home.path(),
                PublisherCommands::Remove {
                    id: "example".into()
                },
                &[]
            )
            .await
            .is_err()
        );
        let pending = broker
            .list_by_kind(gate::PUBLISHERS_ACTION_KIND)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.status == duduclaw_gateway::approval::ApprovalStatus::Pending)
            .unwrap();
        broker
            .decide(&pending.id, true, "dashboard:admin")
            .await
            .unwrap();
        run_with_markers(
            home.path(),
            PublisherCommands::Remove {
                id: "example".into(),
            },
            &[],
        )
        .await
        .unwrap();
        assert!(
            !TrustStore::load(home.path()).unwrap()
                .publishers
                .contains_key("example")
        );
    }

    #[tokio::test]
    async fn changed_store_requires_a_new_approval() {
        let home = tempfile::tempdir().unwrap();
        let command = || PublisherCommands::Add {
            id: "example".into(),
            pubkey: duduclaw_core::publishers::OFFICIAL_PUBLISHER_KEY.into(),
        };
        assert!(run_with_markers(home.path(), command(), &[]).await.is_err());
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let rec = broker
            .list_by_kind(gate::PUBLISHERS_ACTION_KIND)
            .await
            .unwrap()
            .remove(0);
        broker
            .decide(&rec.id, true, "dashboard:admin")
            .await
            .unwrap();
        std::fs::write(
            home.path().join("trusted_publishers.toml"),
            "# state changed\n",
        )
        .unwrap();
        assert!(run_with_markers(home.path(), command(), &[]).await.is_err());
        assert_eq!(
            std::fs::read_to_string(home.path().join("trusted_publishers.toml")).unwrap(),
            "# state changed\n"
        );
        assert_eq!(
            broker
                .list_by_kind(gate::PUBLISHERS_ACTION_KIND)
                .await
                .unwrap()
                .into_iter()
                .filter(|r| r.status == duduclaw_gateway::approval::ApprovalStatus::Pending)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn malformed_store_is_preserved_and_builtin_is_immutable() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("trusted_publishers.toml");
        std::fs::write(&path, "[broken").unwrap();
        assert!(
            run_with_markers(
                home.path(),
                PublisherCommands::Remove {
                    id: "example".into()
                },
                &[]
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), "[broken");
        assert!(
            run_with_markers(
                home.path(),
                PublisherCommands::Remove {
                    id: "duduclaw".into()
                },
                &[]
            )
            .await
            .is_err()
        );
        assert!(!home.path().join("approvals.db").exists());
    }

    #[test]
    fn bash_gate_blocks_employee_and_unknown_callers() {
        for caller in [
            duduclaw_core::HookCaller::Agent("alice".into()),
            duduclaw_core::HookCaller::Untrusted("alice".into()),
        ] {
            assert!(
                bash_publishers_decision("duduclaw publishers add example key", &caller).is_some()
            );
            assert!(
                bash_publishers_decision("duduclaw-pro publishers remove example", &caller)
                    .is_some()
            );
            assert!(bash_publishers_decision("duduclaw publishersx list", &caller).is_none());
        }
        assert!(
            bash_publishers_decision(
                "duduclaw publishers remove example",
                &duduclaw_core::HookCaller::Absent
            )
            .is_none()
        );
    }

    #[test]
    fn missing_and_empty_store_are_different_states() {
        assert_ne!(state_of(None), state_of(Some(b"")));
    }
}
