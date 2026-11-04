//! Read-only subsystem contracts. Failures retain their meaning and never become empty data.
use super::{Block, Data, Pane, array, fetch::Gateway, field};
use crate::{client::RpcReply, sanitize::TerminalText, strings};
use serde_json::{Value, json};

type ReadResult<T> = std::result::Result<T, TerminalText>;
pub(crate) async fn read(gateway: &impl Gateway, method: &str, params: Value) -> ReadResult<Value> {
    let value = match gateway.request(method, params).await {
        Ok(RpcReply::Accepted(v)) => v,
        Ok(RpcReply::Rejected { message, .. }) => {
            let safe = message.plain();
            return Err(TerminalText::new(if safe.chars().count() > 240 {
                format!("{}… {}", safe.chars().take(240).collect::<String>(), strings::BLOCK_TRUNCATED)
            } else { safe }));
        }
        Err(error) => return Err(TerminalText::new(error.to_string())),
    };
    serde_json::to_writer(&mut crate::budget::SnapshotBudget::default(), &value)
        .map_err(|_| TerminalText::new(crate::Error::SnapshotBudget.to_string()))?;
    Ok(value)
}
fn rows(value: &Value, key: &str) -> ReadResult<Vec<Value>> {
    array(value, key).map_err(|e| TerminalText::new(e.to_string()))
}
fn protocol() -> TerminalText { TerminalText::new(crate::Error::Protocol.to_string()) }

pub async fn fetch(gateway: &impl Gateway, pane: &Pane) -> Data {
    match load(gateway, pane).await {
        Ok(mut data) => { data.stale = false; data }
        Err(error) => Data { error: Some(error), ..Data::default() },
    }
}
async fn load(gateway: &impl Gateway, pane: &Pane) -> ReadResult<Data> {
    let mut data = Data::default();
    match pane.kind {
        Block::Workflow => {
            let task = pane.task_id.as_deref().ok_or_else(|| TerminalText::new(strings::SELECT_TASK.into()))?;
            let drafts = read(gateway, "workflow_drafts.list", json!({"task_id":task})).await?;
            // This is deliberately a bounded source-task view, not a global run search.
            let draft_rows = rows(&drafts, "drafts")?;
            let mut budget = crate::budget::SnapshotBudget::default();
            let mut truncated = draft_rows.len() > 20 || !drafts["next_cursor"].is_null();
            for view in draft_rows.iter().take(20) {
                let draft = &view["draft"];
                let id = draft["draft_id"].as_str().ok_or_else(protocol)?;
                let revision = draft["revision"].as_i64().ok_or_else(protocol)?;
                let runs = read(gateway, "workflow_runs.list", json!({"draft_id":id,"revision":revision,"limit":20})).await?;
                budget.charge(&runs, "runs").map_err(|e| TerminalText::new(e.to_string()))?;
                let run_rows = rows(&runs, "runs")?;
                truncated |= run_rows.len() >= 20;
                for mut run in run_rows.into_iter().take(20) {
                    run["activation_id"].as_str().ok_or_else(protocol)?;
                    run["_tui_owner"] = draft["owner"].clone();
                    data.rows.push(run);
                }
            }
            data.total = data.rows.len();
            if let Some(row) = data.rows.get(pane.index.min(data.total.saturating_sub(1))) {
                let id = row["run_id"].as_str().ok_or_else(protocol)?;
                data.selected = read(gateway, "workflow_runs.get", json!({"run_id":id})).await?;
                if data.selected["run_id"] != id { return Err(protocol()); }
                data.selected["_tui_owner"] = row["_tui_owner"].clone();
                rows(&data.selected, "steps")?;
            }
            data.sections.push(("truncated", json!(truncated)));
        }
        Block::Autopilot => {
            let list = read(gateway, "autopilot.list", json!({})).await?;
            data.rows = rows(&list, "rules")?;
            let breakers = read(gateway, "autopilot.breakers", json!({})).await?;
            for breaker in rows(&breakers, "breakers")? {
                if breaker["rule_id"].as_str().is_none() || !matches!(breaker["state"].as_str(), Some("closed" | "open" | "half_open")) {
                    return Err(protocol());
                }
            }
            data.sections.push(("breakers", breakers));
        }
        Block::Evolution => {
            data.selected = read(gateway, "evolution.status", json!({})).await?;
            rows(&data.selected, "agents")?;
            let params = pane.agent.as_ref().map_or_else(|| json!({}), |a| json!({"agent_id":a}));
            let stagnation = read(gateway, "evolution.stagnation", params.clone()).await?;
            rows(&stagnation, "snapshots")?;
            data.sections.push(("stagnation", stagnation));
            if pane.agent.is_some() {
                // Playbook is independently Viewer-gated; retain an explicit refusal.
                match read(gateway, "playbook.list", params).await {
                    Ok(list) => { rows(&list, "entries")?; data.sections.push(("playbook", list)); }
                    Err(error) => data.sections.push(("playbook_error", json!(error.plain()))),
                }
            }
        }
        Block::Budget => {
            let result = read(gateway, "budget.incidents", json!({"limit":50})).await?;
            data.rows = rows(&result, "incidents")?;
        }
        Block::Digest => {
            data.selected = read(gateway, "digest.latest", json!({})).await?;
            if data.selected.get("digest").is_none() { return Err(protocol()); }
            if !data.selected["digest"].is_null() { rows(&data.selected["digest"], "agents")?; }
        }
        _ => {}
    }
    Ok(data)
}

/// Bound each field and the number of rows before layout, including hostile payloads.
pub(crate) fn short(value: &Value, key: &str) -> String {
    let safe = field(value, key);
    if safe.chars().count() > 240 {
        format!("{}… {}", safe.chars().take(240).collect::<String>(), strings::BLOCK_TRUNCATED)
    } else { safe }
}
pub(crate) fn fields(lines: &mut Vec<String>, row: &Value, keys: &[&str]) {
    for key in keys {
        lines.push(format!("{}: {}", strings::field_label(key), short(row, key)));
    }
}
pub(crate) fn table(lines: &mut Vec<String>, rows: Option<&Vec<Value>>, keys: &[&str]) {
    match rows {
        None => lines.push(strings::NOT_PROVIDED.into()),
        Some(rows) if rows.is_empty() => lines.push(strings::VIEW_EMPTY.into()),
        Some(rows) => {
            lines.push(keys.iter().map(|key| strings::field_label(key)).collect::<Vec<_>>().join(" · "));
            for row in rows.iter().take(50) {
                lines.push(keys.iter().map(|key| short(row, key)).collect::<Vec<_>>().join(" · "));
            }
            if rows.len() > 50 { lines.push(strings::BLOCK_TRUNCATED.into()); }
        }
    }
}
pub(crate) fn content(pane: &Pane) -> Vec<String> {
    let d = &pane.data;
    let mut lines = vec![strings::VIEW_READ_ONLY.into()];
    if let Some(error) = &d.error {
        lines.push(format!("{}: {}", strings::VIEW_UNAVAILABLE, error.plain()));
        return lines;
    }
    if d.stale { lines.push(strings::STALE.into()); return lines; }
    match pane.kind {
        Block::Workflow => {
            lines[0] = strings::WORKFLOW_KEYS.into();
            if d.selected.is_null() { lines.push(strings::VIEW_EMPTY.into()); }
            else {
                lines.push(format!("{}/{}", pane.index.min(d.total.saturating_sub(1)) + 1, d.total));
                fields(&mut lines, &d.selected, &["workflow_id","run_id","status","error_code","created_at","deadline_at"]);
                table(&mut lines, d.selected["steps"].as_array(), &["step_id","status","error_code"]);
            }
            if d.section("truncated") == true { lines.push(strings::BLOCK_TRUNCATED.into()); }
        }
        Block::Autopilot => {
            if d.rows.is_empty() { lines.push(strings::VIEW_EMPTY.into()); }
            for rule in d.rows.iter().take(50) {
                fields(&mut lines, rule, &["name","id","enabled","trigger_event"]);
                let breaker = d.section("breakers")["breakers"].as_array()
                    .and_then(|rows| rows.iter().find(|b| b["rule_id"] == rule["id"]));
                lines.push(strings::BREAKERS.into());
                if let Some(breaker) = breaker { fields(&mut lines, breaker, &["state","opened_at"]); }
                else { lines.push(strings::NOT_PROVIDED.into()); }
            }
            if d.rows.len() > 50 { lines.push(strings::BLOCK_TRUNCATED.into()); }
        }
        Block::Evolution => {
            fields(&mut lines, &d.selected, &["enabled","mode","total_rounds","applied_rounds"]);
            table(&mut lines, d.selected["agents"].as_array(), &["agent_id","gvu_enabled","cognitive_memory"]);
            lines.push(strings::STAGNATION.into());
            table(&mut lines, d.section("stagnation")["snapshots"].as_array(), &["agent_id","is_stagnant","summary","checked_at"]);
            lines.push(strings::PLAYBOOK.into());
            if let Some(error) = d.section("playbook_error").as_str() { lines.push(error.into()); }
            else if pane.agent.is_none() { lines.push(strings::SELECT_EMPLOYEE.into()); }
            else { table(&mut lines, d.section("playbook")["entries"].as_array(), &["id","state","category","content"]); }
        }
        Block::Budget => table(&mut lines, Some(&d.rows), &["ts","agent_id","event","scope","spent_cents","cap_cents"]),
        Block::Digest => {
            fields(&mut lines, &d.selected, &["enabled"]);
            let digest = &d.selected["digest"];
            if digest.is_null() { lines.push(strings::VIEW_EMPTY.into()); }
            else {
                fields(&mut lines, digest, &["date","since","generated_at"]);
                table(&mut lines, digest["agents"].as_array(), &["agent_id","finished_total","pending_approvals","runs_done","runs_not_done","spend_usd"]);
                for agent in digest["agents"].as_array().into_iter().flatten().take(50) {
                    lines.push(short(agent,"agent_id"));
                    table(&mut lines, agent["finished"].as_array(), &["title","completed_at"]);
                    table(&mut lines, agent["artifacts"].as_array(), &["name","produced_at"]);
                }
            }
        }
        _ => {}
    }
    let guide = match pane.kind {
        Block::Workflow => strings::EMPTY_WORKFLOW,
        Block::Autopilot => strings::EMPTY_AUTOPILOT,
        Block::Evolution => strings::EMPTY_EVOLUTION,
        Block::Budget => strings::EMPTY_BUDGET,
        Block::Digest => strings::EMPTY_DIGEST,
        _ => "",
    };
    for line in &mut lines {
        if line == strings::VIEW_EMPTY && !guide.is_empty() {
            *line = format!("{}；{guide}", strings::VIEW_EMPTY);
        }
    }
    lines
}
