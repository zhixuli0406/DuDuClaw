//! Direct-API MCP roots: one fresh host lease per actual stdio server spawn.
use super::*;
use duduclaw_core::run_credential::{RunLease, ENV_RUN_CREDENTIAL, ENV_IDENTITY_URL, ENV_IDENTITY_ENDPOINT, ENV_IDENTITY_SERVER};

impl McpClient {
    pub async fn connect_with_lease(
        command: &str, args: &[String], envs: &[(String, String)], timeout: Duration,
        lease: Box<dyn RunLease>, clear_env: bool,
    ) -> Result<Self, McpError> {
        let mut env: Vec<(String, String)> = if clear_env {
            duduclaw_core::spawn_env::agent_cli_spawn_env_pairs().into_iter()
                .map(|(key, value)| (key.to_string(), value)).collect()
        } else { Vec::new() };
        env.extend(envs.iter().cloned());
        env.retain(|(key, _)| !key.eq_ignore_ascii_case(ENV_RUN_CREDENTIAL) && !key.eq_ignore_ascii_case(ENV_IDENTITY_URL) && !key.eq_ignore_ascii_case(ENV_IDENTITY_ENDPOINT) && !key.eq_ignore_ascii_case(ENV_IDENTITY_SERVER));
        env.push((ENV_RUN_CREDENTIAL.into(), lease.credential().expose().into()));
        // Empty explicitly replaces an inherited endpoint if no listener exists.
        env.push((ENV_IDENTITY_ENDPOINT.into(), lease.identity_endpoint().unwrap_or("").into()));
        env.push((ENV_IDENTITY_SERVER.into(), lease.identity_server().unwrap_or("").into()));
        let mut client = Self::spawn_and_handshake_bound(command, args, &env, timeout,
            std::process::Stdio::null(), clear_env, Some(lease.spawn_binding())).await?;
        client.run_identity = Some(lease);
        Ok(client)
    }
}
