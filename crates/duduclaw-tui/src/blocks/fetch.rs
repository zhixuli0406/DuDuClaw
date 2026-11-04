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
// Socket selectors are shared by panes and survive discarded data fetches.
#[derive(Default)]
pub(crate) struct RunSubscriptions {
    panes: std::collections::HashMap<String, (Option<String>, bool)>,
}
struct SharedRuns<'a, G> {
    gateway: &'a G,
    retained: Vec<String>,
}
#[async_trait]
impl<G: Gateway> Gateway for SharedRuns<'_, G> {
    async fn request(&self, method: &str, params: Value) -> Result<RpcReply> {
        if method == "runs.unsubscribe" && params["run_id"].as_str().is_some_and(|id| self.retained.iter().any(|r| r == id)) {
            return Ok(RpcReply::Accepted(json!({})));
        }
        self.gateway.request(method, params).await
    }
}
impl RunSubscriptions {
    pub(crate) async fn retain(&mut self, gateway: &impl Gateway, ids: &[String]) -> Result<()> {
        let removed: Vec<_> = self.panes.iter().filter(|(id, _)| !ids.contains(id)).map(|(_, state)| state.clone()).collect();
        let mut released = std::collections::HashSet::new();
        for (run, subscribed) in removed {
            if subscribed && let Some(run) = run
                && !self.panes.iter().any(|(pane, (id, live))| ids.contains(pane) && *live && id.as_ref() == Some(&run)) && released.insert(run.clone()) {
                gateway.request("runs.unsubscribe", json!({"run_id":run})).await?;
            }
        }
        self.panes.retain(|id, _| ids.contains(id));
        Ok(())
    }
    pub(crate) async fn fetch(&mut self, gateway: &impl Gateway, pane: &Pane, role: Option<crate::auth::Role>) -> Result<Data> {
        let mut pane = pane.clone();
        if pane.kind == Block::Run {
            let cached = self.panes.get(&pane.id);
            pane.data.subscription_checked = cached.is_some();
            pane.data.subscribed_run = cached.and_then(|(id, _)| id.clone());
            pane.data.subscribed = cached.is_some_and(|(_, live)| *live);
        }
        let retained = self.panes.iter().filter(|(id, (_, live))| **id != pane.id && *live)
            .filter_map(|(_, (id, _))| id.clone()).collect();
        let data = pane_for_role(&SharedRuns { gateway, retained }, &pane, role).await?;
        if pane.kind == Block::Run {
            self.panes.insert(pane.id, (data.subscribed_run.clone(), data.subscribed));
        }
        Ok(data)
    }
}
pub async fn pane_for_role(gateway: &impl Gateway, pane: &Pane, role: Option<crate::auth::Role>) -> Result<Data> {
    use crate::auth::Role;
    let refusal = match pane.kind {
        Block::Autopilot if role != Some(Role::Admin) => Some(crate::strings::ADMIN_ONLY),
        Block::Evolution | Block::Budget if !matches!(role, Some(Role::Admin | Role::Manager)) => Some(crate::strings::MANAGER_ONLY),
        _ => None,
    };
    if let Some(reason) = refusal { return Ok(Data { error:Some(TerminalText::new(reason.into())), ..Data::default() }); }
    self::pane(gateway, pane, matches!(role, Some(Role::Admin | Role::Manager))).await
}
pub async fn pane(gateway: &impl Gateway, pane: &Pane, manager: bool) -> Result<Data> {
    if pane.kind.autonomy() { return Ok(super::autonomy::fetch(gateway, pane).await); }
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
                if !super::run_matches(run, &detail, agent, id) {
                    return Err(Error::Protocol);
                }
                data.selected = detail["run"].clone();
                // runs.get omits task/round; runs.list is their authoritative source.
                for key in ["task_id", "round"] {
                    if let Some(value) = run.get(key) { data.selected[key] = value.clone(); }
                }
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
            // Runtime is employee metadata; never infer it from missing step events.
            let agents = gateway.read("agents.list", json!({})).await?;
            data.runtime = array(&agents, "agents")?
                .iter()
                .find(|r| r["name"] == agent)
                .and_then(|r| r["runtime"]["provider"].as_str())
                .map(|s| TerminalText::new(s.into()));
            let selected_run = data.selected["id"].as_str();
            data.subscribed_run = selected_run.map(str::to_owned);
            if pane.data.subscription_checked && pane.data.subscribed_run.as_deref() == selected_run {
                data.subscribed = pane.data.subscribed;
            } else {
                if pane.data.subscribed && let Some(id) = &pane.data.subscribed_run {
                    // Refusals are optional-live failures; transport errors still propagate.
                    gateway.request("runs.unsubscribe", json!({"run_id":id})).await?;
                }
                if let Some(id) = selected_run {
                    data.subscribed = matches!(gateway.request("runs.subscribe", json!({"run_id":id})).await?, RpcReply::Accepted(_));
                }
            }
            data.subscription_checked = true;
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
                let roles = super::autonomy::read(gateway, "tasks.role_turns", params.clone()).await;
                match roles {
                    Ok(roles) => { array(&roles, "turns")?; data.sections.push(("roles", roles)); }
                    Err(error) => data.sections.push(("roles_error", json!(error.plain()))),
                }
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
