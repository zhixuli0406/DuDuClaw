//! Narrow control requests. No arbitrary RPC entry, global switches, or grants.
use crate::{
    blocks::fetch::Gateway, client::RpcReply, decide::Outcome, sanitize::TerminalText, strings,
};
use serde_json::{Value, json};
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Control {
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
    pub fn request(&self) -> (&'static str, Value) {
        match self {
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
    match gateway.request(method, params).await {
        Ok(RpcReply::Accepted(v)) if v["conflict"] == true => Outcome::Changed,
        // Accepted responses are only a reason to refetch, never a state update.
        Ok(RpcReply::Accepted(_)) => Outcome::Refetch,
        Ok(RpcReply::Rejected { message, .. }) => Outcome::Error(message),
        Err(e) => Outcome::Error(TerminalText::new(e.to_string())),
    }
}
