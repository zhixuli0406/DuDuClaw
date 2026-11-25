//! Local-only run introspection. Errors deliberately contain no transport body.

use std::path::Path;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::run_credential::{ipc, ENV_IDENTITY_URL, ENV_IDENTITY_ENDPOINT, ENV_IDENTITY_SERVER, ENV_RUN_CREDENTIAL, RunCredential};

#[derive(clap::Subcommand)]
pub(crate) enum IdentityCommands {
    /// Ask the local gateway who issued this run's identity.
    Whoami {
        #[arg(long)]
        json: bool,
    },
}

pub(crate) async fn run(_home: &Path, command: IdentityCommands) -> Result<()> {
    let IdentityCommands::Whoami { json } = command;
    let inactive = || DuDuClawError::Security("Run identity is missing, inactive or unavailable".into());
    require_run_identity_mode(duduclaw_core::execution_gate::isolation_required())?;
    let credential = std::env::var(ENV_RUN_CREDENTIAL).ok().and_then(RunCredential::parse)
        .ok_or_else(inactive)?;
    if std::env::var_os(ENV_IDENTITY_URL).is_some() { return Err(inactive()); }
    let (endpoint, server) = trusted_target(std::env::var(ENV_IDENTITY_ENDPOINT),
        std::env::var(ENV_IDENTITY_SERVER)).ok_or_else(inactive)?;
    let value = ipc::request(&endpoint, &server, &serde_json::json!({
        "version": 1, "operation": "introspect", "credential": credential.expose(),
    })).await.map_err(|_| inactive())?;
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

/// Only the gateway chooses the conversation and checks its generation.
/// Missing/inactive run credentials never fall back to session environment claims.
pub(crate) async fn record_native_tool_result(_home: &Path, tool: &str, result: &serde_json::Value) -> Result<()> {
    let Some(reference) = duduclaw_gateway::session_tool_refs::reference_from_result(tool, result) else { return Ok(()); };
    let Some(credential) = std::env::var(ENV_RUN_CREDENTIAL).ok().and_then(RunCredential::parse) else { return Ok(()); };
    if std::env::var_os(ENV_IDENTITY_URL).is_some() { return Ok(()); }
    let Some((endpoint, server)) = trusted_target(std::env::var(ENV_IDENTITY_ENDPOINT),
        std::env::var(ENV_IDENTITY_SERVER)) else { return Ok(()); };
    let unavailable = || DuDuClawError::Gateway("Conversation reference writer unavailable".into());
    let response = ipc::request(&endpoint, &server, &serde_json::json!({
        "version": 1, "operation": "tool-reference", "credential": credential.expose(), "reference": reference,
    })).await.map_err(|_| unavailable())?;
    if response["active"] != true { return Err(unavailable()); }
    Ok(())
}

fn require_run_identity_mode(protected: bool) -> Result<()> {
    if protected {
        return Err(DuDuClawError::Security(
            "Protected mode uses its own capability identity; no run credential is issued (受保護模式使用獨立的 capability 身分，不簽發 run credential).".into()));
    }
    Ok(())
}

// Discovery is operator diagnostics only. Both routing and the server generation
// must come from the gateway's spawn environment, even if a discovery file exists.
fn trusted_target(endpoint: std::result::Result<String, std::env::VarError>,
    server: std::result::Result<String, std::env::VarError>) -> Option<(String, String)>
{
    let endpoint = endpoint.ok()?;
    let server = server.ok()?;
    ipc::endpoint_path(&endpoint)?;
    ipc::server_identity(&server)?;
    Some((endpoint, server))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_endpoint(name: &str) -> String {
        #[cfg(unix)]
        { format!("unix:v1:/tmp/{name}/s") }
        #[cfg(windows)]
        { format!(r"pipe:v1:\\.\pipe\duduclaw-identity-{name}") }
    }

    #[test]
    fn reference_writer_never_uses_session_environment_claims() {
        let source = include_str!("identity_cmd.rs");
        let writer = source.split("pub(crate) async fn record_native_tool_result").nth(1).unwrap()
            .split("fn require_run_identity_mode").next().unwrap();
        assert!(!writer.contains("DUDUCLAW_SESSION_ID"));
        assert!(!writer.contains("ENV_TRUST_SESSION_ID"));
        assert!(writer.contains("ENV_RUN_CREDENTIAL"));
        assert!(!writer.contains("SessionManager"));
        let dispatch = include_str!("mcp/dispatch.rs");
        let writer = dispatch.split("// RM-182:").nth(1).unwrap().split("// ── Tool call audit trail").next().unwrap();
        assert!(!writer.contains("ENV_TRUST_SESSION_ID"));
        assert!(writer.contains("crate::identity_cmd::record_native_tool_result"));
    }

    #[test]
    fn protected_mode_reports_its_separate_capability_identity() {
        assert!(require_run_identity_mode(false).is_ok());
        let error = require_run_identity_mode(true).unwrap_err().to_string();
        assert!(error.contains("Protected mode uses its own capability identity"));
        assert!(error.contains("no run credential is issued"));
    }

    #[test]
    fn missing_trusted_env_never_uses_discovery_even_after_redirection() {
        use duduclaw_core::run_credential::discovery::DiscoveryDirectory;
        let home = tempfile::tempdir().unwrap();
        let discovery = DiscoveryDirectory::open(home.path(), true).unwrap();
        let endpoint = test_endpoint("abc");
        let server = "v1:123:456".to_owned();
        // Runs on Windows too: a valid local pipe in discovery is never a destination.
        for destination in [&endpoint, &test_endpoint("def")] {
            discovery.write(destination).unwrap();
            assert!(trusted_target(Err(std::env::VarError::NotPresent), Ok(server.clone())).is_none());
            assert!(trusted_target(Ok(endpoint.clone()), Err(std::env::VarError::NotPresent)).is_none());
            assert_eq!(trusted_target(Ok(endpoint.clone()), Ok(server.clone())),
                Some((endpoint.clone(), server.clone())));
        }
    }

    #[test]
    fn invalid_endpoint_or_server_identity_fails_closed() {
        let endpoint = test_endpoint("abc");
        for raw in [String::new(), "http://127.0.0.1:12345/api/internal/identity/introspect".into(),
            "https://example.com".into(), "unix:v2:/tmp/s".into(), "unix:v1:relative".into(),
            format!(" {endpoint}"), format!("{endpoint}\0")] {
            assert!(trusted_target(Ok(raw), Ok("v1:123:456".into())).is_none());
        }
        for raw in ["", "v2:123:456", "v1:0:456", "v1:123:0", "v1:123:456:7", "v1:+123:456"] {
            assert!(trusted_target(Ok(endpoint.clone()), Ok(raw.into())).is_none());
        }
    }
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
