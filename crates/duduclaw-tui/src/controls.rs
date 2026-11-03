//! Narrow control requests. No arbitrary RPC entry or grants. Global autonomy controls require Admin.
use crate::{
    blocks::fetch::Gateway, client::RpcReply, decide::Outcome, sanitize::TerminalText, strings,
};
use serde_json::{Value, json};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
    WorkflowCancel { run: String },
    GoalCreate { agent: String, body: String, plan_first: bool },
    Autonomy { agent: Option<String>, resume: bool },
    Steer {
        task: String,
        body: String,
        request_id: String,
    },
    Stop {
        task: String,
        revision: i64,
    },
    Employee {
        agent: String,
        resume: bool,
    },
    Cron {
        id: String,
        action: CronAction,
        request_id: String,
    },
    Heartbeat {
        agent: String,
    },
    Responsibility {
        id: String,
        epoch: i64,
        disable: bool,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CronAction {
    Pause,
    Resume,
    Run,
}
impl Control {
    pub fn label(&self) -> &'static str {
        match self {
            Self::WorkflowCancel { .. } => strings::WORKFLOW_CANCEL,
            Self::GoalCreate { .. } => strings::GOAL_CREATE,
            Self::Autonomy { agent: None, resume: false } => strings::AUTONOMY_PAUSE_ALL,
            Self::Autonomy { agent: None, resume: true } => strings::AUTONOMY_RESUME_ALL,
            Self::Autonomy { resume: false, .. } => strings::AUTONOMY_PAUSE,
            Self::Autonomy { resume: true, .. } => strings::AUTONOMY_RESUME,
            Self::Steer { .. } => strings::STEER,
            Self::Stop { .. } => strings::STOP,
            Self::Employee { resume: true, .. } => strings::EMPLOYEE_RESUME,
            Self::Employee { .. } => strings::EMPLOYEE_PAUSE,
            Self::Cron {
                action: CronAction::Pause,
                ..
            } => strings::CRON_PAUSE,
            Self::Cron {
                action: CronAction::Resume,
                ..
            } => strings::CRON_RESUME,
            Self::Cron { .. } | Self::Heartbeat { .. } => strings::RUN_NOW,
            Self::Responsibility { disable: true, .. } => strings::RESP_DISABLE,
            Self::Responsibility { .. } => strings::RESP_PAUSE,
        }
    }
    pub fn edit_body(&mut self) -> Option<&mut String> {
        match self { Self::Steer { body, .. } | Self::GoalCreate { body, .. } => Some(body), _ => None }
    }
    pub fn request(&self) -> (&'static str, Value) {
        match self {
            Self::WorkflowCancel { run } => ("workflow_runs.cancel", json!({"run_id":run})),
            Self::GoalCreate { agent, body, plan_first } => ("tasks.goal_create", json!({"agent_id":agent,"description":body,"plan_first":plan_first})),
            Self::Autonomy { agent, resume } => (
                if *resume { "autonomy.resume" } else { "autonomy.pause" },
                agent.as_ref().map_or_else(|| json!({"all":true}), |id| json!({"agent_id":id})),
            ),
            Self::Steer {
                task,
                body,
                request_id,
            } => (
                "tasks.steer",
                json!({"task_id":task,"body":body,"client_request_id":request_id}),
            ),
            Self::Stop { task, revision } => (
                "tasks.stop",
                json!({"task_id":task,"expected_authority_revision":revision}),
            ),
            Self::Employee { agent, resume } => (
                if *resume {
                    "agents.resume"
                } else {
                    "agents.pause"
                },
                json!({"agent_id":agent}),
            ),
            Self::Cron {
                id,
                action,
                request_id,
            } => (
                match action {
                    CronAction::Pause => "cron.pause",
                    CronAction::Resume => "cron.resume",
                    CronAction::Run => "cron.run_now",
                },
                json!({"id":id,"request_id":request_id}),
            ),
            Self::Heartbeat { agent } => ("heartbeat.trigger", json!({"agent_id":agent})),
            Self::Responsibility { id, epoch, disable } => (
                if *disable {
                    "responsibilities.disable"
                } else {
                    "responsibilities.pause"
                },
                json!({"responsibility_id":id,"expected_control_epoch":epoch}),
            ),
        }
    }
}
pub fn request_id() -> String {
    format!("tui-{:032x}", rand::random::<u128>())
}
pub async fn send(gateway: &impl Gateway, control: &Control) -> Outcome {
    let (method, params) = control.request();
    let reply = gateway.request(method, params).await;
    if let Control::Autonomy { agent, resume } = control {
        return match reply {
            Ok(RpcReply::Accepted(v)) if v["applied"] == true
                && v["all"] == agent.is_none() && v.get("agent_id") == Some(&json!(agent))
                && v["paused"] == !*resume
                && v.get("pause").is_some_and(|p| if *resume { p.is_null() } else { serde_json::from_value::<crate::autonomy::Pause>(p.clone()).is_ok() }) => {
                    let warning = v.get("durability_warning").is_some() || v.get("audit_error").is_some();
                    Outcome::Notice(TerminalText::new(format!("{}：{}{}", control.label(),
                        strings::AUTONOMY_APPLIED, if warning { strings::AUTONOMY_WARNING } else { "" })))
                }
            Ok(RpcReply::Accepted(_)) => Outcome::Error(TerminalText::new(strings::AUTONOMY_UNCONFIRMED.into())),
            Ok(RpcReply::Rejected { message, .. }) => Outcome::Error(TerminalText::new(strings::autonomy_refusal(&message.plain()))),
            Err(error) => Outcome::Error(TerminalText::new(format!("{}：{error}", strings::AUTONOMY_UNCONFIRMED))),
        };
    }
    match reply {
        Ok(RpcReply::Accepted(v)) if v["conflict"] == true => Outcome::Changed,
        // Accepted responses are only a reason to refetch, never a state update.
        Ok(RpcReply::Accepted(_)) => Outcome::Refetch,
        Ok(RpcReply::Rejected { message, .. }) => Outcome::Error(message),
        Err(e) => Outcome::Error(TerminalText::new(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Reply(Value);
    #[async_trait::async_trait]
    impl Gateway for Reply {
        async fn request(&self, _: &str, _: Value) -> crate::Result<RpcReply> {
            Ok(RpcReply::Accepted(self.0.clone()))
        }
    }
    #[tokio::test]
    async fn autonomy_receipts_are_validated_and_durability_warnings_are_visible() {
        for agent in [None, Some("adu".to_owned())] {
            for resume in [false, true] {
                let control = Control::Autonomy { agent:agent.clone(), resume };
                let valid = json!({"agent_id":agent,"all":agent.is_none(),"paused":!resume,"applied":true,
                    "pause":if resume { Value::Null } else { json!({"reason":"","who":"u1","at":"now"}) }});
                assert!(matches!(send(&Reply(valid.clone()), &control).await, Outcome::Notice(_)));
                let mut warning = valid.clone(); warning["durability_warning"] = json!("applied_durability_unknown");
                assert!(send(&Reply(warning),&control).await.notice().contains(strings::AUTONOMY_WARNING));
                for field in ["agent_id","all","paused","applied","pause"] {
                    let mut malformed = valid.clone(); malformed.as_object_mut().unwrap().remove(field);
                    assert!(matches!(send(&Reply(malformed), &control).await, Outcome::Error(_)), "{field}");
                }
                let mut wrong = valid; wrong["paused"] = json!(resume);
                assert!(matches!(send(&Reply(wrong), &control).await, Outcome::Error(_)));
            }
        }
    }
}
