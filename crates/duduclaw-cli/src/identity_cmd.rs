//! Local-only run introspection. Errors deliberately contain no transport body.

use std::path::Path;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::run_credential::{ENV_RUN_CREDENTIAL, RunCredential};

#[derive(clap::Subcommand)]
pub(crate) enum IdentityCommands {
    /// Ask the local gateway who issued this run's identity.
    Whoami {
        #[arg(long)]
        json: bool,
    },
}

pub(crate) async fn run(home: &Path, command: IdentityCommands) -> Result<()> {
    let IdentityCommands::Whoami { json } = command;
    let inactive = || DuDuClawError::Security("Run identity is missing, inactive or unavailable".into());
    let credential = std::env::var(ENV_RUN_CREDENTIAL).ok().and_then(RunCredential::parse)
        .ok_or_else(inactive)?;
    let port = match std::env::var(duduclaw_core::run_credential::ENV_RUN_GATEWAY_PORT) {
        Ok(port) => port.parse::<u16>().ok().filter(|port| *port != 0).ok_or_else(inactive)?,
        Err(std::env::VarError::NotPresent) => duduclaw_core::gateway_port_for_home(home).0,
        Err(_) => return Err(inactive()),
    };
    let client = reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5)).build().map_err(|_| inactive())?;
    let mut response = client.post(format!("http://127.0.0.1:{port}{}", duduclaw_gateway::run_identity::ROUTE))
        .json(&serde_json::json!({"credential": credential.expose()}))
        .send().await.map_err(|_| inactive())?;
    if response.status() != reqwest::StatusCode::OK { return Err(inactive()); }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| inactive())? {
        if bytes.len().saturating_add(chunk.len()) > 16384 { return Err(inactive()); }
        bytes.extend_from_slice(&chunk);
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| inactive())?;
    if value.get("active").and_then(serde_json::Value::as_bool) != Some(true) {
        if json { println!("{{\"active\":false}}"); }
        return Err(inactive());
    }
    // Typed decoding limits stdout to the endpoint's public claim schema.
    let record: PublicIdentity = serde_json::from_value(value).map_err(|_| inactive())?;
    let output = if json { serde_json::to_string_pretty(&record).map_err(|_| inactive())? } else {
        format!("{} ({})\nprincipal: {}\nsession: {}\nturn: {}", record.agent_id, record.runtime,
            record.principal, record.session_id.as_deref().unwrap_or("—"), record.turn_id)
    };
    println!("{}", duduclaw_core::run_credential::redact(&output));
    Ok(())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct PublicIdentity {
    active: bool,
    agent_id: String,
    principal: serde_json::Value,
    session_id: Option<String>,
    turn_id: String,
    task_id: Option<String>,
    runtime: String,
    issued_at: u64,
    expires_at: u64,
}
