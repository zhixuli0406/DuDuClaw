//! `forget-source apply` and `resume` (P2-B): run a recorded plan behind the
//! approval gate, then the follow-up steps, and print the outcome.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::memory_forget_steps as steps;
use duduclaw_memory::{ApplyOutcome, ExternalInputs, ForgetPlan, SqliteMemoryEngine};

use super::{
    AUDIT_APPLIED, CmdOutput, extras, forget_source_enabled, gate, open_engine, refused, render,
    stray_db,
};

pub(super) async fn apply(home: &Path, plan_id: &str, confirm: bool) -> Result<CmdOutput> {
    let engine = open_engine(home)?;
    let Some(plan) = engine.get_forget_plan(plan_id).await? else {
        return Err(DuDuClawError::Agent(format!("找不到計畫 {plan_id}")));
    };
    let agent = plan.document.agent_id.clone();
    if !forget_source_enabled(home) {
        return Err(refused(
            home,
            &agent,
            Some(plan_id),
            "disabled",
            "config.toml 的 [memory] forget_source 已關閉（或無法讀取），不套用。".into(),
        ));
    }
    if stray_db(home, &agent) {
        return Err(refused(
            home,
            &agent,
            Some(plan_id),
            "stray_db",
            "還有尚未合併的個別 memory.db，請先重新啟動 gateway。".into(),
        ));
    }
    if !confirm {
        let x = extras(&engine, home, &plan).await;
        let mut o = render::render_plan(&plan, &x);
        o.push_str(&gate::status_line(home, &plan).await);
        o.push_str("\n尚未套用。確認無誤後加上 --confirm 執行。\n");
        return Ok(CmdOutput::ok(o));
    }
    // C-1: an Admin must have approved exactly this plan in the dashboard.
    if let Err((reason, msg)) = gate::check(home, &plan).await {
        return Err(refused(home, &agent, Some(plan_id), &reason, msg));
    }
    let external = steps::collect_external_for_apply(home, &plan)
        .await
        .map_err(DuDuClawError::Memory)?;
    let outcome = engine.apply_forget_plan(plan_id, &external).await?;
    apply_outcome(home, &engine, &plan, outcome, external).await
}

async fn apply_outcome(
    home: &Path,
    engine: &SqliteMemoryEngine,
    plan: &ForgetPlan,
    outcome: ApplyOutcome,
    _external: ExternalInputs,
) -> Result<CmdOutput> {
    let agent = plan.document.agent_id.as_str();
    let pid = plan.plan_id.as_str();
    let mut o = String::new();
    match outcome {
        ApplyOutcome::Applied(r) => {
            duduclaw_security::audit::append_audit_event(
                home,
                &duduclaw_security::audit::AuditEvent::new(
                    AUDIT_APPLIED,
                    agent,
                    duduclaw_security::audit::Severity::Warning,
                    serde_json::json!({
                        "plan_id": r.plan_id,
                        "agent_id": r.agent_id,
                        "plan_hash": r.plan_hash,
                        "memories_deleted": r.memories_deleted,
                        "key_facts_deleted": r.key_facts_deleted,
                        "archive_deleted": r.archive_deleted,
                        "entity_embeddings_deleted": r.entity_embeddings_deleted,
                        "supersession_links_cut": r.supersession_links_cut,
                        "steps_pending": r.steps_pending,
                        "forget_epoch": r.forget_epoch,
                        "untracked_in_namespace": {
                            "planned": r.untracked_in_namespace_planned,
                            "at_apply": r.untracked_in_namespace_at_apply,
                        },
                        "other_namespaces_referencing": {
                            "planned": r.other_namespaces_referencing_planned,
                            "at_apply": r.other_namespaces_referencing_at_apply,
                        },
                    }),
                ),
            );
            if r.memories_deleted + r.key_facts_deleted + r.archive_deleted == 0 {
                o.push_str(&format!(
                    "沒有可刪的記憶，但已設下之後不再學到的紀錄（{} ms）。\n",
                    r.elapsed_ms
                ));
            } else {
                o.push_str(&format!(
                    "已刪除 {} 筆記憶、{} 筆關鍵事實、{} 份封存副本，並設好防止復活的紀錄（{} ms）。\n",
                    r.memories_deleted, r.key_facts_deleted, r.archive_deleted, r.elapsed_ms
                ));
            }
            o.push_str(&format!(
                "參考數字（不影響套用）：{}：計畫時 {} 筆、套用時 {} 筆；\
                 其他命名空間引用同一段對話的記憶：計畫時 {} 筆、套用時 {} 筆。\n",
                render::UNTRACKED_PHRASE,
                r.untracked_in_namespace_planned,
                r.untracked_in_namespace_at_apply,
                r.other_namespaces_referencing_planned,
                r.other_namespaces_referencing_at_apply
            ));
        }
        ApplyOutcome::AlreadyApplied => {
            o.push_str("這個計畫先前已套用，只補跑未完成的後續步驟。\n")
        }
        ApplyOutcome::NotFound => return Err(DuDuClawError::Agent(format!("找不到計畫 {pid}"))),
        ApplyOutcome::Expired => {
            return Err(refused(
                home,
                agent,
                Some(pid),
                "expired",
                "計畫已過期，請重新執行 plan。".into(),
            ));
        }
        ApplyOutcome::DbMismatch => {
            return Err(refused(
                home,
                agent,
                Some(pid),
                "db_mismatch",
                "這個計畫是在另一個資料庫上建立的，不能套用。".into(),
            ));
        }
        ApplyOutcome::Stale(reason) => {
            let detail = render::stale_detail(&reason);
            return Err(refused(
                home,
                agent,
                Some(pid),
                "stale",
                format!("{detail}，沒有刪除任何東西。請先暫停該員工，再重新執行 plan。"),
            ));
        }
    }
    let report = steps::run_forget_steps(engine, home, pid)
        .await
        .map_err(DuDuClawError::Memory)?;
    o.push_str(&render_steps(pid, &report));
    Ok(CmdOutput {
        text: o,
        complete: report.complete(),
    })
}

fn render_steps(plan_id: &str, r: &steps::StepsReport) -> String {
    if r.complete() {
        if r.done == 0 {
            return "沒有需要補跑的後續步驟（先前都已完成）。結果：COMPLETE\n".to_string();
        }
        let done: Vec<String> = r
            .by_step
            .iter()
            .filter(|(_, (d, _))| *d > 0)
            .map(|(k, (d, _))| format!("{} {d} 項", render::step_label(k)))
            .collect();
        return format!(
            "後續步驟完成 {} 項（{}）。結果：COMPLETE\n",
            r.done,
            done.join("、")
        );
    }
    let failed: Vec<String> = r
        .by_step
        .iter()
        .filter(|(_, (_, f))| *f > 0)
        .map(|(k, (_, f))| format!("{} {f} 項", render::step_label(k)))
        .collect();
    format!(
        "結果：DEGRADED。記憶已刪除並設好防止復活的紀錄，但以下後續步驟尚未完成：{}。\n\
         可用 `duduclaw memory forget-source resume --plan {plan_id}` 重試（gateway 開機時與每 10 分鐘也會自動重試）。\n",
        failed.join("、")
    )
}

pub(super) async fn resume(home: &Path, plan_id: &str) -> Result<CmdOutput> {
    let engine = open_engine(home)?;
    let report = steps::run_forget_steps(&engine, home, plan_id)
        .await
        .map_err(DuDuClawError::Memory)?;
    Ok(CmdOutput {
        text: render_steps(plan_id, &report),
        complete: report.complete(),
    })
}
