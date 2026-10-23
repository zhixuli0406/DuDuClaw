//! The per-employee 「通道」 status shown on the employee's page.

use serde_json::{Value, json};

/// Runtime state of one bot / app (from the gateway's channel status map).
#[derive(Debug, Clone, Default)]
pub struct Runtime {
    pub present: bool,
    pub connected: bool,
    pub error: Option<String>,
    pub last_event: Option<String>,
}

/// Plain state word the dashboard maps to copy.
pub fn state_word(configured: bool, rt: &Runtime) -> &'static str {
    match (configured, rt.present, rt.connected) {
        (false, _, _) => "not_configured",
        (true, _, true) => "connected",
        (true, false, _) => "connecting",
        (true, true, false) => match rt.error.as_deref() {
            Some(e) if e.starts_with("connecting") => "connecting",
            Some(_) => "error",
            None => "connecting",
        },
    }
}

/// Telegram block.
pub fn telegram(
    configured: bool,
    rt: &Runtime,
    bot_username: Option<String>,
    shared_configured: bool,
    can_edit: bool,
) -> Value {
    json!({
        "configured": configured,
        "state": state_word(configured, rt),
        "connected": configured && rt.connected,
        "bot_username": if configured { bot_username } else { None },
        "error": if configured { rt.error.clone() } else { None },
        "last_event": rt.last_event,
        "shared_bot_configured": shared_configured,
        "can_edit": can_edit,
    })
}

/// Google Chat block.
#[allow(clippy::too_many_arguments)]
pub fn googlechat(
    agent_id: &str,
    configured: bool,
    rt: &Runtime,
    project_number: Option<String>,
    service_account_email: Option<String>,
    shared_configured: bool,
    can_edit: bool,
) -> Value {
    json!({
        "configured": configured,
        "state": state_word(configured, rt),
        "connected": configured && rt.connected,
        "project_number": project_number,
        "service_account_email": service_account_email,
        "endpoint_path": super::googlechat_endpoint_path(agent_id),
        "error": if configured { rt.error.clone() } else { None },
        "last_event": rt.last_event,
        "shared_app_configured": shared_configured,
        "can_edit": can_edit,
    })
}
