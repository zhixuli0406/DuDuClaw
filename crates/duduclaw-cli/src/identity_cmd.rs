//! Local-only run introspection. Errors deliberately contain no transport body.

use std::path::Path;
use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::run_credential::{ENV_IDENTITY_URL, ENV_RUN_CREDENTIAL, RunCredential};
use duduclaw_core::run_credential::discovery::DiscoveryDirectory;

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
    require_run_identity_mode(duduclaw_core::execution_gate::isolation_required())?;
    let credential = std::env::var(ENV_RUN_CREDENTIAL).ok().and_then(RunCredential::parse)
        .ok_or_else(inactive)?;
    let url = discover_url(home, std::env::var(ENV_IDENTITY_URL)).ok_or_else(inactive)?;
    let client = reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(5)).build().map_err(|_| inactive())?;
    let mut response = client.post(url)
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

fn require_run_identity_mode(protected: bool) -> Result<()> {
    if protected {
        return Err(DuDuClawError::Security(
            "Protected mode uses its own capability identity; no run credential is issued (受保護模式使用獨立的 capability 身分，不簽發 run credential).".into()));
    }
    Ok(())
}

// Validate the final discovery target before sending a bearer proof. A present
// but invalid environment override fails closed rather than using the file.
fn discover_url(home: &Path, env: std::result::Result<String, std::env::VarError>) -> Option<reqwest::Url> {
    let raw = match env {
        Ok(value) => value,
        Err(std::env::VarError::NotPresent) => DiscoveryDirectory::open(home, false).ok()?.read().ok()?,
        Err(_) => return None,
    };
    let url = reqwest::Url::parse(raw.trim()).ok()?;
    if url.scheme() != "http" || url.host_str() != Some("127.0.0.1")
        || url.port().is_none_or(|port| port == 0) || !url.username().is_empty() || url.password().is_some()
        || url.path() != duduclaw_gateway::run_identity::ROUTE || url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    Some(url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_mode_reports_its_separate_capability_identity() {
        assert!(require_run_identity_mode(false).is_ok());
        let error = require_run_identity_mode(true).unwrap_err().to_string();
        assert!(error.contains("Protected mode uses its own capability identity"));
        assert!(error.contains("no run credential is issued"));
    }

    #[test]
    fn discovery_prefers_env_then_file_and_refuses_nonlocal_targets() {
        let home = tempfile::tempdir().unwrap();
        let url = format!("http://127.0.0.1:12345{}", duduclaw_gateway::run_identity::ROUTE);
        let other = format!("http://127.0.0.1:12346{}", duduclaw_gateway::run_identity::ROUTE);
        assert!(discover_url(home.path(), Err(std::env::VarError::NotPresent)).is_none());
        let discovery = DiscoveryDirectory::open(home.path(), true).unwrap();
        discovery.write(&url).unwrap();
        assert_eq!(discover_url(home.path(), Err(std::env::VarError::NotPresent)).unwrap().as_str(), url);
        assert_eq!(discover_url(home.path(), Ok(other.clone())).unwrap().as_str(), other);
        for raw in [String::new(), url.replace("127.0.0.1", "example.com"),
            url.replace("127.0.0.1", "127.0.0.1.evil.example"), url.replace("http:", "https:"),
            url.replace("127.0.0.1", "user@127.0.0.1"), format!("{url}?credential=x"),
            format!("{url}#x"), url.replace("12345", "0")] {
            assert!(discover_url(home.path(), Ok(raw)).is_none());
        }
    }

    #[cfg(unix)]
    #[test]
    fn discovery_ignores_unsafe_files_and_directories_but_keeps_env_override() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        use duduclaw_core::run_credential::IDENTITY_URL_FILE;

        let url = format!("http://127.0.0.1:12345{}", duduclaw_gateway::run_identity::ROUTE);
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let discovery = DiscoveryDirectory::open(home.path(), true).unwrap();
        discovery.write(&url).unwrap();
        let file = home.path().join(IDENTITY_URL_FILE);
        let directory = home.path().join("run");
        let assert_ignored = || {
            assert!(discover_url(home.path(), Err(std::env::VarError::NotPresent)).is_none());
            assert_eq!(discover_url(home.path(), Ok(url.clone())).unwrap().as_str(), url);
        };

        std::fs::rename(&file, outside.path().join("identity-url")).unwrap();
        symlink(outside.path().join("identity-url"), &file).unwrap();
        assert_ignored();
        std::fs::remove_file(&file).unwrap();
        discovery.write(&url).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_ignored();
        discovery.write(&url).unwrap();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o770)).unwrap();
        assert_ignored();
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::rename(&directory, home.path().join("original-run")).unwrap();
        symlink(home.path().join("original-run"), &directory).unwrap();
        assert_ignored();
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
