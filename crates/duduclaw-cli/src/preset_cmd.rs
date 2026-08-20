//! `duduclaw preset` — WP-6F (agent presets P1) operator CLI surface.
//!
//! P1 deliberately ships no switching UI (see
//! `commercial/docs/DESIGN-agent-presets-2026-08.md` §9's P1 scope: "不含切
//! 換 UI、registry、版本流") — this module is the one write path for P1,
//! mirroring `cmd_org_sync`'s shape: refuse from inside an agent session
//! (binding changes an agent's capabilities — letting an agent bind itself
//! to an arbitrary preset would be a self-escalation channel, the same class
//! of risk `org sync`'s refusal already guards against), validate before
//! writing, then leave all three traces design §3.2 requires:
//!
//! 1. `security_audit.jsonl` (`duduclaw_security::audit`)
//! 2. Activity Feed (`duduclaw_gateway::task_store::TaskStore`, opened
//!    directly — no live gateway process required, same as how the audit
//!    log is a plain file)
//! 3. The agent-visible dynamic-tail line (`duduclaw_gateway::preset_prompt`)
//!    — nothing to *write* here; it reads the binding store live on the
//!    agent's next wake-up, so trace ③ is automatic once ① has landed.
//!
//! **Known P1 gap**: if a gateway is already running, it does not notice a
//! CLI-side bind until its own next natural rescan trigger (an
//! `agent_update` from the dashboard, or a restart) — `AgentRegistry` has no
//! file watcher (see `duduclaw_core::agent_toml` module docs). Live-reload
//! wiring belongs with the switching UI in P2.

use std::path::{Path, PathBuf};

use console::style;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::preset::{self, PresetError, PresetResolution};

fn agents_dir(home: &Path) -> PathBuf {
    home.join("agents")
}

fn agent_dir_or_err(home: &Path, agent: &str) -> Result<PathBuf> {
    let dir = agents_dir(home).join(agent);
    if !dir.join("agent.toml").is_file() {
        return Err(DuDuClawError::Agent(format!(
            "找不到 AI 員工「{agent}」的 agent.toml（請用目錄名稱）"
        )));
    }
    Ok(dir)
}

fn preset_error_zh(err: &PresetError) -> String {
    match err {
        PresetError::NotFound(id) => format!(
            "找不到職務組合「{id}」。用 `duduclaw preset list` 查看目前可用的職務組合，\
             或先執行 `duduclaw preset install-builtin` 安裝內建的部門組合。"
        ),
        PresetError::Invalid(reason) => format!("職務組合內容無法解析：{reason}"),
        PresetError::OrgFieldsRejected(fields) => format!(
            "職務組合內容夾帶組織權威欄位（{}），已拒絕套用。\n\
             這類欄位（誰是誰的主管、屬於哪個部門）只能由管理者透過 `duduclaw org sync` \
             逐一調整，不能經由職務組合一次改寫一批 AI 員工。",
            fields.join(", ")
        ),
    }
}

/// `duduclaw preset list` — every preset id under `~/.duduclaw/presets/`.
pub fn cmd_preset_list() -> Result<()> {
    let home = crate::duduclaw_home();
    let ids = preset::list_presets(&home);
    if ids.is_empty() {
        println!(
            "目前沒有任何職務組合。執行 `duduclaw preset install-builtin` 安裝內建的部門組合，\n\
             或在 {} 底下手動新增。",
            preset::presets_dir(&home).display()
        );
        return Ok(());
    }
    println!("職務組合（{} 個）：", ids.len());
    for id in ids {
        match preset::load_preset(&home, &id) {
            Ok(p) => {
                let label = if p.meta.label.trim().is_empty() { "（無標籤）".to_string() } else { p.meta.label };
                println!("  • {id} v{}　{label}", p.meta.version);
            }
            Err(e) => println!("  • {id}　{}", style(format!("⚠️ 無法解析（{e}）")).red()),
        }
    }
    Ok(())
}

/// `duduclaw preset show <id>` — one preset's metadata + resolved config.
pub fn cmd_preset_show(id: &str) -> Result<()> {
    let home = crate::duduclaw_home();
    let p = preset::load_preset(&home, id).map_err(|e| DuDuClawError::Agent(preset_error_zh(&e)))?;
    println!("職務組合：{id}");
    println!("  版本：v{}", p.meta.version);
    println!("  標籤：{}", p.meta.label);
    if !p.meta.description.is_empty() {
        println!("  說明：{}", p.meta.description);
    }
    println!("  內容雜湊：{}", p.content_sha256);
    if !p.stripped_other.is_empty() {
        println!(
            "  {} 已忽略以下僅限單一員工的欄位（不會套用到任何綁定的 AI 員工）：{}",
            style("提示").yellow(),
            p.stripped_other.join(", ")
        );
    }
    println!();
    println!("{}", toml::to_string_pretty(&p.config).unwrap_or_default());
    Ok(())
}

/// `duduclaw preset bind --agent <id> --preset <ref> [--reason <text>]`.
pub async fn cmd_preset_bind(agent: &str, preset_ref: &str, reason: &str) -> Result<()> {
    if let Some(claimed) = crate::agent_session_identity() {
        return Err(DuDuClawError::Agent(refuse_preset_write_in_agent_session(&claimed)));
    }
    let home = crate::duduclaw_home();
    let dir = agent_dir_or_err(&home, agent)?;

    let outcome = preset::bind(&home, agent, &dir, preset_ref, &actor(), reason)
        .map_err(|e| DuDuClawError::Agent(preset_error_zh(&e)))?;

    write_preset_mirror(&dir, &outcome.binding)
        .map_err(|e| DuDuClawError::Agent(format!("寫入 agent.toml [preset] 鏡像失敗：{e}")))?;

    record_traces(
        &home,
        agent,
        "preset_switched",
        serde_json::json!({
            "from": outcome.previous.as_ref().map(|b| serde_json::json!({
                "preset_id": b.preset_id, "version": b.version,
            })),
            "to": { "preset_id": outcome.binding.preset_id, "version": outcome.binding.version },
            "actor": outcome.binding.bound_by,
            "reason": outcome.binding.reason,
            "changed_fields": outcome.changed_fields,
        }),
        &format!(
            "AI 員工「{agent}」套用職務組合「{}」v{}",
            outcome.binding.preset_id, outcome.binding.version
        ),
    )
    .await;

    println!(
        "{} 「{agent}」已套用職務組合「{}」v{}",
        style("✓").green().bold(),
        outcome.binding.preset_id,
        outcome.binding.version
    );
    if let Some(prev) = &outcome.previous {
        println!("  （原本套用：{} v{}）", prev.preset_id, prev.version);
    }
    if !outcome.changed_fields.is_empty() {
        println!(
            "  {} 這個 AI 員工的 agent.toml 已明寫、會覆寫職務組合的欄位：{}",
            style("已覆寫").yellow(),
            outcome.changed_fields.join(", ")
        );
    }
    println!(
        "  {} 若 gateway 正在執行中，需等下一次設定重新載入（重啟或經由儀表板編輯該員工）\n\
             才會套用到執行中的行程。",
        style("提醒").dim()
    );
    Ok(())
}

/// `duduclaw preset unbind --agent <id> [--reason <text>]`.
pub async fn cmd_preset_unbind(agent: &str, reason: &str) -> Result<()> {
    if let Some(claimed) = crate::agent_session_identity() {
        return Err(DuDuClawError::Agent(refuse_preset_write_in_agent_session(&claimed)));
    }
    let home = crate::duduclaw_home();
    let dir = agent_dir_or_err(&home, agent)?;

    let removed = preset::unbind(&home, agent, &actor(), reason)
        .map_err(|e| DuDuClawError::Agent(format!("解除綁定失敗：{e}")))?;
    let Some(prev) = removed else {
        println!("「{agent}」目前沒有套用任何職務組合，無需解除。");
        return Ok(());
    };

    let _ = clear_preset_mirror(&dir);
    record_traces(
        &home,
        agent,
        "preset_unbound",
        serde_json::json!({
            "from": { "preset_id": prev.preset_id, "version": prev.version },
            "to": serde_json::Value::Null,
            "actor": actor(),
            "reason": reason,
        }),
        &format!("AI 員工「{agent}」解除職務組合「{}」", prev.preset_id),
    )
    .await;
    println!(
        "{} 「{agent}」已解除職務組合「{}」v{}，現在完全以自己的 agent.toml 運作。",
        style("✓").green().bold(),
        prev.preset_id,
        prev.version
    );
    Ok(())
}

/// `duduclaw preset status <agent>` — current binding + live resolution.
pub fn cmd_preset_status(agent: &str) -> Result<()> {
    let home = crate::duduclaw_home();
    let dir = agent_dir_or_err(&home, agent)?;
    let table: toml::value::Table = std::fs::read_to_string(dir.join("agent.toml"))
        .map_err(|e| DuDuClawError::Agent(format!("讀取 agent.toml 失敗：{e}")))?
        .parse()
        .map_err(|e| DuDuClawError::Agent(format!("解析 agent.toml 失敗：{e}")))?;
    let (_, resolution) = preset::resolve_for_agent(&home, agent, &table);

    match resolution {
        PresetResolution::Unbound => println!("「{agent}」目前沒有套用任何職務組合。"),
        PresetResolution::Applied { preset_id, version, label, changed_fields, .. } => {
            println!("「{agent}」目前套用：{preset_id} v{version}（{label}）");
            if changed_fields.is_empty() {
                println!("  沒有任何欄位被本機 agent.toml 覆寫。");
            } else {
                println!("  已覆寫欄位：{}", changed_fields.join(", "));
            }
        }
        PresetResolution::Unresolved { preset_id, version, reason } => {
            println!(
                "{} 「{agent}」綁定了「{preset_id}」v{version}，但目前無法套用：{reason}",
                style("⚠️").yellow()
            );
            println!("  這個 AI 員工現在只用自己 agent.toml 裡明寫的設定運作（fail-closed）。");
        }
    }
    Ok(())
}

/// `duduclaw preset install-builtin [--force]` — install every built-in
/// preset this build knows about into `~/.duduclaw/presets/`:
///
/// 1. The one **free** preset (`system-operator`, P2) — compiled into the
///    binary from the public `templates/presets/system-operator/` tree, no
///    license check, no `commercial/` checkout required. Always attempted.
/// 2. The **premium** department-kit presets (converted from
///    `_departments/<kit>/agent.toml`, design §4.2 Step A) copied from the
///    premium templates tree. Gated by the same `premium_templates` license
///    feature the source kits already require — unchanged from before this
///    change; a license-less caller now still gets item 1 instead of a hard
///    error.
pub fn cmd_preset_install_builtin(force: bool) -> Result<()> {
    let home = crate::duduclaw_home();
    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    match preset::install_system_operator_preset(&home, force) {
        Ok(true) => installed.push(preset::SYSTEM_OPERATOR_PRESET_ID.to_string()),
        Ok(false) => skipped.push(preset::SYSTEM_OPERATOR_PRESET_ID.to_string()),
        Err(e) => {
            return Err(DuDuClawError::Agent(format!(
                "安裝免費內建職務組合「{}」失敗：{e}",
                preset::SYSTEM_OPERATOR_PRESET_ID
            )));
        }
    }

    let premium_available = crate::premium_templates::premium_unlocked();
    if premium_available {
        if let Some(premium_dir) = crate::premium_templates::find_premium_templates_dir() {
            let source_root = premium_dir.join("presets");
            if let Ok(entries) = std::fs::read_dir(&source_root) {
                for entry in entries.flatten() {
                    if !entry.path().is_dir() {
                        continue;
                    }
                    let Some(id) = entry.file_name().to_str().map(str::to_string) else { continue };
                    let src_file = entry.path().join(preset::PRESET_FILE);
                    if !src_file.is_file() {
                        continue;
                    }
                    let dest_dir = preset::preset_dir(&home, &id);
                    let dest_file = dest_dir.join(preset::PRESET_FILE);
                    if dest_file.is_file() && !force {
                        skipped.push(id);
                        continue;
                    }
                    std::fs::create_dir_all(&dest_dir)
                        .map_err(|e| DuDuClawError::Agent(format!("建立 {} 失敗：{e}", dest_dir.display())))?;
                    std::fs::copy(&src_file, &dest_file)
                        .map_err(|e| DuDuClawError::Agent(format!("複製 {id} 失敗：{e}")))?;
                    installed.push(id);
                }
            }
        }
    }

    if !installed.is_empty() {
        println!("{} 已安裝 {} 個內建職務組合：{}", style("✓").green().bold(), installed.len(), installed.join(", "));
    }
    if !skipped.is_empty() {
        println!(
            "{} 已跳過 {} 個（本機已存在，未加 --force）：{}",
            style("略過").dim(),
            skipped.len(),
            skipped.join(", ")
        );
    }
    if !premium_available {
        println!(
            "{} 目前的授權方案未開放內建產業/部門職務組合（premium_templates），\
             只安裝了免費的「{}」。可自行在 `~/.duduclaw/presets/` 下新增其他職務組合，\
             機制本身完全免費。",
            style("提示").dim(),
            preset::SYSTEM_OPERATOR_PRESET_ID
        );
    }
    Ok(())
}

fn actor() -> String {
    std::env::var("USER").map(|u| format!("cli:{u}")).unwrap_or_else(|_| "cli:operator".to_string())
}

/// zh-TW refusal shown when a preset-binding write is attempted from inside
/// an agent session — mirrors `refuse_org_write_in_agent_session`'s
/// reasoning: binding changes what tools/model/evolution posture an agent
/// runs with, so letting an agent bind itself (or a peer) would be the same
/// class of self-escalation `org sync`'s refusal already exists to block.
fn refuse_preset_write_in_agent_session(claimed: &str) -> String {
    format!(
        "這個指令不能在 AI 員工的工作階段中執行（偵測到身分:{claimed}）。\n\
         職務組合會改變一個 AI 員工實際擁有的工具與能力範圍,只能由管理者調整——\n\
         否則 AI 員工能透過改變自己的職務組合幫自己加開權限。\n\
         請由管理者在自己的終端機執行 `duduclaw preset bind`,或使用儀表板。"
    )
}

/// Write the read-only `[preset]` mirror into `<agent_dir>/agent.toml`
/// (design §3.1). Table-based read-modify-write, same shape as
/// `expert::patch_agent_toml` — comments elsewhere in the file are not
/// preserved, which is the pre-existing behaviour of every agent.toml
/// rewrite path in this codebase (`agent_update`, `update_agent_toml_with`),
/// not a new regression. The mirror is informational only — resolution
/// always reads `preset_bindings.toml`, never this block.
fn write_preset_mirror(agent_dir: &Path, binding: &preset::PresetBinding) -> std::io::Result<()> {
    let path = agent_dir.join("agent.toml");
    let content = std::fs::read_to_string(&path)?;
    let mut table: toml::value::Table = content
        .parse::<toml::Table>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut preset_tbl = toml::value::Table::new();
    preset_tbl.insert("ref".to_string(), toml::Value::String(binding.preset_id.clone()));
    preset_tbl.insert("version".to_string(), toml::Value::String(binding.version.clone()));
    preset_tbl.insert("bound_at".to_string(), toml::Value::String(binding.bound_at.clone()));
    table.insert("preset".to_string(), toml::Value::Table(preset_tbl));
    write_agent_toml_atomic(&path, &table)
}

fn clear_preset_mirror(agent_dir: &Path) -> std::io::Result<()> {
    let path = agent_dir.join("agent.toml");
    let content = std::fs::read_to_string(&path)?;
    let mut table: toml::value::Table = content
        .parse::<toml::Table>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    table.remove("preset");
    write_agent_toml_atomic(&path, &table)
}

fn write_agent_toml_atomic(path: &Path, table: &toml::value::Table) -> std::io::Result<()> {
    let out = toml::to_string_pretty(table)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, out)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_binding() -> preset::PresetBinding {
        preset::PresetBinding {
            preset_id: "sales-followup".to_string(),
            version: "1.0.0".to_string(),
            content_sha256: "deadbeef".to_string(),
            bound_at: "2026-08-15T00:00:00Z".to_string(),
            bound_by: "cli:tester".to_string(),
            reason: "test".to_string(),
        }
    }

    fn write_agent(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("agent.toml"), body).unwrap();
    }

    #[test]
    fn write_preset_mirror_adds_a_read_only_block_without_touching_other_sections() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent(tmp.path(), "[agent]\nname = \"clinic-sales\"\ndisplay_name = \"x\"\n");

        write_preset_mirror(tmp.path(), &sample_binding()).unwrap();

        let table: toml::value::Table =
            std::fs::read_to_string(tmp.path().join("agent.toml")).unwrap().parse().unwrap();
        let mirror = table.get("preset").and_then(|v| v.as_table()).expect("[preset] must exist");
        assert_eq!(mirror.get("ref").and_then(|v| v.as_str()), Some("sales-followup"));
        assert_eq!(mirror.get("version").and_then(|v| v.as_str()), Some("1.0.0"));
        // The [agent] section the write didn't touch must survive.
        assert_eq!(
            table.get("agent").and_then(|v| v.as_table()).and_then(|t| t.get("name")).and_then(|v| v.as_str()),
            Some("clinic-sales")
        );
    }

    #[test]
    fn clear_preset_mirror_removes_the_block_and_is_a_noop_if_absent() {
        let tmp = tempfile::tempdir().unwrap();
        write_agent(tmp.path(), "[agent]\nname = \"clinic-sales\"\n");
        write_preset_mirror(tmp.path(), &sample_binding()).unwrap();

        clear_preset_mirror(tmp.path()).unwrap();
        let table: toml::value::Table =
            std::fs::read_to_string(tmp.path().join("agent.toml")).unwrap().parse().unwrap();
        assert!(!table.contains_key("preset"));

        // Calling again (nothing to clear) must not error.
        assert!(clear_preset_mirror(tmp.path()).is_ok());
    }

    #[test]
    fn preset_error_zh_maps_every_variant_to_a_readable_zh_tw_message() {
        let not_found = preset_error_zh(&PresetError::NotFound("ghost".to_string()));
        assert!(not_found.contains("ghost"));
        assert!(not_found.contains("preset list"));

        let org_rejected = preset_error_zh(&PresetError::OrgFieldsRejected(vec![
            "agent.reports_to".to_string(),
        ]));
        assert!(org_rejected.contains("agent.reports_to"));
        assert!(org_rejected.contains("org sync"));

        let invalid = preset_error_zh(&PresetError::Invalid("bad toml".to_string()));
        assert!(invalid.contains("bad toml"));
    }
}

/// Design §3.2's three traces ① (audit) + ② (Activity Feed). ③ (the
/// agent-visible line) needs no write here — `preset_prompt` reads the
/// binding store live on the agent's next wake-up.
async fn record_traces(
    home: &Path,
    agent_id: &str,
    event_type: &str,
    details: serde_json::Value,
    activity_summary: &str,
) {
    duduclaw_security::audit::append_audit_event(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            event_type,
            agent_id,
            duduclaw_security::audit::Severity::Info,
            details.clone(),
        ),
    );

    // Best-effort: a missing/locked task-board DB must never fail the bind
    // itself (the binding store write already committed — the audit trail
    // above is the trace of record).
    if let Ok(store) = duduclaw_gateway::task_store::TaskStore::open(home) {
        let row = duduclaw_gateway::task_store::ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: event_type.to_string(),
            agent_id: agent_id.to_string(),
            task_id: None,
            summary: activity_summary.to_string(),
            timestamp: chrono::Utc::now().to_rfc3339(),
            metadata: Some(details.to_string()),
        };
        let _ = store.append_activity(&row).await;
    }
}
