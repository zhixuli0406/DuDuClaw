//! Wire contracts are taken from gateway handlers, not inferred from UI labels.
use super::{Block, Data, Pane, array};
use crate::{
    Error, Result,
    client::{Client, RpcReply},
    sanitize::TerminalText,
};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::time::Duration;

// async_trait adds must_use to an already must-use boxed future.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait Gateway: Send + Sync {
    async fn request(&self, method: &str, params: Value) -> Result<RpcReply>;
    async fn read(&self, method: &str, params: Value) -> Result<Value> {
        match self.request(method, params).await? {
            RpcReply::Accepted(v) => Ok(v),
            _ => Err(Error::RpcRejected),
        }
    }
}
#[async_trait]
impl Gateway for Client {
    async fn request(&self, method: &str, params: Value) -> Result<RpcReply> {
        self.call_detailed(method, params, Duration::from_secs(10))
            .await
    }
}
pub async fn pane(gateway: &impl Gateway, pane: &Pane, manager: bool) -> Result<Data> {
    let mut data = Data::default();
    match pane.kind {
        Block::Run => {
            let agent = pane.agent.as_deref().ok_or(Error::Protocol)?;
            let list = gateway
                .read("runs.list", json!({"agent_id":agent, "limit":50}))
                .await?;
            data.rows = array(&list, "runs")?;
            data.total = data.rows.len();
            if let Some(run) = data
                .rows
                .get(pane.index.min(data.rows.len().saturating_sub(1)))
            {
                let id = run["id"].as_str().ok_or(Error::Protocol)?;
                let detail = gateway.read("runs.get", json!({"run_id":id})).await?;
                if !detail["run"].is_object()
                    || detail["run"]["id"] != id
                    || detail["run"]["agent_id"] != agent
                {
                    return Err(Error::Protocol);
                }
                data.selected = detail["run"].clone();
                let events = array(&detail, "events")?;
                data.sections.push((
                    "events",
                    json!(
                        events
                            .into_iter()
                            .rev()
                            .take(256)
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev()
                            .collect::<Vec<_>>()
                    ),
                ));
            }
            if pane.data.subscription_checked {
                data.subscribed = pane.data.subscribed;
            } else {
                match gateway
                    .request("runs.subscribe", json!({"agent_id":agent}))
                    .await?
                {
                    RpcReply::Accepted(_) => data.subscribed = true,
                    RpcReply::Rejected { message, .. }
                        if message.is("Unknown method: runs.subscribe") => {}
                    RpcReply::Rejected { .. } => return Err(Error::RpcRejected),
                }
            }
            data.subscription_checked = true;
            // Runtime is employee metadata; never infer it from missing step events.
            let agents = gateway.read("agents.list", json!({})).await?;
            data.runtime = array(&agents, "agents")?
                .iter()
                .find(|r| r["name"] == agent)
                .and_then(|r| r["runtime"]["provider"].as_str())
                .map(|s| TerminalText::new(s.into()));
        }
        Block::Task if pane.responsibilities => {
            let agent = pane.agent.as_deref().ok_or(Error::Protocol)?;
            let list = gateway
                .read("responsibilities.list", json!({"agent_id":agent}))
                .await?;
            data.rows = array(&list, "responsibilities")?;
            data.total = data.rows.len();
            if let Some(row) = data
                .rows
                .get(pane.index.min(data.rows.len().saturating_sub(1)))
            {
                let id = row["responsibility_id"].as_str().ok_or(Error::Protocol)?;
                let params = json!({"responsibility_id":id,"limit":20});
                let summary = gateway.read("responsibilities.get", params.clone()).await?;
                data.selected = summary["summary"]["responsibility"].clone();
                if data.selected["responsibility_id"] != id {
                    return Err(Error::Protocol);
                }
                data.sections.push(("summary", summary));
                let fires = gateway.read("responsibilities.fires", params).await?;
                array(&fires, "fires")?;
                data.sections.push(("fires", fires));
            }
        }
        Block::Task => {
            let agent = pane.agent.as_deref().ok_or(Error::Protocol)?;
            let offset = pane.index / 50 * 50;
            let list = gateway
                .read(
                    "tasks.list_page",
                    json!({"agent_id":agent,"limit":50,"offset":offset,"archived":false}),
                )
                .await?;
            data.rows = array(&list, "tasks")?;
            data.total = list["total"].as_u64().ok_or(Error::Protocol)? as usize;
            let selected_id = pane.task_id.as_deref().or_else(|| {
                data.rows
                    .get(pane.index % 50)
                    .and_then(|t| t["id"].as_str())
            });
            if let Some(id) = selected_id {
                let params = json!({"task_id":id});
                let timeline = gateway.read("tasks.timeline", params.clone()).await?;
                if timeline["task"]["id"] != id {
                    return Err(Error::Protocol);
                }
                data.selected = timeline["task"].clone();
                for key in ["activity", "runs"] {
                    array(&timeline, key)?;
                }
                data.sections.push(("timeline", timeline));
                let iterations = gateway.read("tasks.iterations", params.clone()).await?;
                array(&iterations, "iterations")?;
                data.sections.push(("iterations", iterations));
                let changes = gateway
                    .read("tasks.changes", json!({"task_id":id,"limit":100}))
                    .await?;
                array(&changes, "changes")?;
                data.sections.push(("changes", changes));
                let stop = gateway.read("tasks.stop_status", params).await?;
                data.sections.push(("stop", stop));
            }
        }
        Block::Log if manager => {
            if !pane.data.subscribed {
                gateway.read("logs.subscribe", json!({})).await?;
            }
            data.subscribed = true;
        }
        _ => {}
    }
    data.stale = false;
    Ok(data)
}
