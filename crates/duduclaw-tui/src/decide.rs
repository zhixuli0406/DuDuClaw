//! Decision wire shapes follow approvals_decide.rs and tasks_detail_rpc.rs.
use crate::{client::{Client, RpcReply}, sanitize::TerminalText, strings};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GoalAction { Retry, Continue, Abort, Done, Takeover }
impl GoalAction {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Retry => "retry", Self::Continue => "continue", Self::Abort => "abort",
            Self::Done => "done", Self::Takeover => "takeover",
        }
    }
    pub fn terminal(self) -> bool { matches!(self, Self::Abort | Self::Done | Self::Takeover) }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    Approval { id: String, approve: bool, digest: Option<String> },
    Goal { task_id: String, action: GoalAction, message: String },
}
impl Decision {
    pub fn request(&self) -> (&'static str, Value) {
        match self {
            Self::Approval { id, approve, digest } => {
                let mut params = json!({"id":id, "approve":approve});
                if let Some(digest) = digest { params["expected_digest"] = json!(digest); }
                ("approvals.decide", params)
            }
            Self::Goal { task_id, action, message } => {
                let mut params = json!({"task_id":task_id, "action":action.wire()});
                if *action == GoalAction::Continue { params["message"] = json!(message); }
                // This checkout's goal_decide handler takes no revision parameter.
                // Do not confuse revision_round or tasks.stop's authority revision with one.
                ("tasks.goal_decide", params)
            }
        }
    }
}
#[derive(Debug)]
pub enum Outcome { Refetch, Changed, Error(TerminalText) }
impl Outcome {
    pub fn notice(&self) -> String {
        match self {
            Self::Refetch => strings::DECISION_REFETCH.into(),
            Self::Changed => strings::CARD_CHANGED.into(),
            Self::Error(text) => text.visible(),
        }
    }
}
pub async fn send(client: &Client, decision: &Decision) -> Outcome {
    let (method, params) = decision.request();
    match client.call_detailed(method, params, Duration::from_secs(15)).await {
        Ok(RpcReply::Rejected { digest_mismatch: true, .. }) => Outcome::Changed,
        Ok(RpcReply::Rejected { message, .. }) => Outcome::Error(message),
        Err(error) => Outcome::Error(TerminalText::new(error.to_string())),
        Ok(RpcReply::Accepted(value)) => {
            let valid = match decision {
                Decision::Approval { id, approve, .. } => value["id"] == *id
                    && value["decided"] == if *approve { "approved" } else { "denied" },
                Decision::Goal { .. } => value["ok"] == true,
            };
            if valid { Outcome::Refetch }
            else { Outcome::Error(TerminalText::new(crate::Error::Protocol.to_string())) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn request_shapes_and_optional_digest() {
        for digest in [None, Some("seen-digest".into())] {
            let d = Decision::Approval { id: "a".into(), approve: true, digest: digest.clone() };
            let (method, params) = d.request();
            assert_eq!(method, "approvals.decide");
            assert_eq!(params["id"], "a");
            assert_eq!(params["approve"], true);
            assert_eq!(params.get("expected_digest"), digest.as_ref().map(|s| json!(s)).as_ref());
        }
        let (_, p) = Decision::Goal { task_id:"t".into(), action:GoalAction::Retry, message:String::new() }.request();
        assert_eq!(p, json!({"task_id":"t", "action":"retry"}));
        let (_, p) = Decision::Goal { task_id:"t".into(), action:GoalAction::Continue, message:"next".into() }.request();
        assert_eq!(p, json!({"task_id":"t", "action":"continue", "message":"next"}));
    }
}
