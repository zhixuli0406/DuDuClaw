//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Helpers ─────────────────────────────────────────────

    /// Check if an API key is available (from env var or config.toml [api] section).
    pub(crate) async fn has_api_key(&self) -> bool {
        has_api_key_configured(&self.home_dir).await
    }

    /// Read config.toml into a TOML table, returning an empty table if the file
    /// does not exist or cannot be parsed.
    pub(crate) async fn read_config_table(&self, path: &std::path::Path) -> toml::Table {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => content.parse::<toml::Table>().unwrap_or_default(),
            Err(_) => toml::Table::new(),
        }
    }

    /// Write a TOML table back to disk.
    pub(crate) async fn write_config_table(
        &self,
        path: &std::path::Path,
        table: &toml::Table,
    ) -> std::io::Result<()> {
        let content = toml::to_string_pretty(table)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
        tokio::fs::write(path, content).await
    }

    /// Strict TOML read for security-sensitive mutations (WP-K). Unlike
    /// `read_config_table` above — which silently degrades BOTH "file
    /// missing" and "file present but unparsable" to an empty table, which is
    /// fine for additive writes elsewhere in this file — this distinguishes
    /// the two: absent ⇒ `Ok(empty)` (no config, legitimately nothing to
    /// protect), present-but-malformed ⇒ `Err`. A parse failure must never be
    /// silently treated as "nothing to protect" (fail-closed detection) and
    /// must never be silently overwritten with an empty file by a cleanup
    /// pass that read a corrupt file and didn't notice.
    pub(crate) async fn read_config_table_strict(
        &self,
        path: &std::path::Path,
    ) -> Result<toml::Table, String> {
        match tokio::fs::read_to_string(path).await {
            Ok(content) => content
                .parse::<toml::Table>()
                .map_err(|e| format!("設定檔解析失敗:{e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
            Err(e) => Err(format!("設定檔讀取失敗:{e}")),
        }
    }

    /// Run common health checks used by both doctor and doctor_repair.
    pub(crate) async fn run_doctor_checks(&self) -> Vec<Value> {
        let reg = self.registry.read().await;
        let has_agents = !reg.list().is_empty();
        let has_key = self.has_api_key().await;
        let config_exists = self.home_dir.join("config.toml").exists();

        // The three spawning probes run concurrently — worst case the RPC
        // costs max(docker, grok ping 15s+version 5s, mcp cold-start 10s),
        // not the sum. The dashboard passes an extended per-call timeout.
        let (docker, mcp, grok) = tokio::join!(
            check_docker(),
            crate::doctor_probes::mcp_cold_start_probe(&self.home_dir),
            crate::doctor_probes::grok_probe(&self.home_dir),
        );

        // O16: the three always-present rows come from `crate::os_ops`, which
        // the agent-facing `os_doctor_repair` MCP tool also builds from —
        // one definition of "what config_file/agents/api_key look like",
        // two projections (`can_repair` is dashboard-only).
        let mut checks = crate::os_ops::doctor_base_checks(config_exists, has_agents, has_key, true);
        checks.push({
            let (docker_status, docker_msg) = docker;
            json!({
                "name": "container_runtime",
                "status": docker_status,
                "message": docker_msg,
                "can_repair": false,
            })
        });

        // MCP server cold-start (the "agent has no tools" class): spawns
        // `duduclaw mcp-server` with a runtime-shaped env and checks it
        // survives the M6 fail-closed auth gate. zh-TW messages — this card
        // exists for operators (distributor deployments) whose only surface
        // is the dashboard.
        {
            let (status, message) = crate::doctor_probes::mcp_cold_start_status_and_message(
                &mcp.outcome,
                mcp.provision_error.as_deref(),
            );
            checks.push(crate::os_ops::doctor_mcp_check(status, &message, true));
        }

        // Grok CLI live probe — only when grok is installed (mirrors the CLI
        // doctor: absence is not a failure, the card is simply omitted).
        if let Some(g) = grok {
            use crate::doctor_probes::GrokProbeOutcome as O;
            let ver = g.version.as_deref().unwrap_or("?");
            let (status, message) = match &g.outcome {
                O::Ok { stdout_chars } => (
                    "pass",
                    format!("grok -p 活體試跑正常（版本 {ver}，回應 {stdout_chars} 字元）。"),
                ),
                O::AuthFailed { stderr_tail } => (
                    "fail",
                    format!(
                        "grok 未登入或憑證失效 — 請在主機執行 `grok login --device-auth`（Docker 部署用 dashboard 的 Grok 登入）。stderr：{stderr_tail}"
                    ),
                ),
                O::EmptyExit0 => (
                    "warn",
                    "grok -p 回傳空輸出（exit 0）— headless 管道問題，runtime 會自動以 PTY 重試；若對話持續空回覆請執行 `duduclaw doctor` 取得完整證據包。".to_string(),
                ),
                O::Failed { exit, stderr_tail } => (
                    "fail",
                    format!(
                        "grok -p 執行失敗（exit={}）。stderr：{}",
                        exit.map(|c| c.to_string()).unwrap_or_else(|| "?".into()),
                        if stderr_tail.is_empty() { "(空)" } else { stderr_tail }
                    ),
                ),
                O::SpawnFailed(e) => ("fail", format!("grok 無法啟動：{e}")),
                O::Timeout => ("warn", "grok -p 15 秒內無回應（逾時）。".to_string()),
            };
            checks.push(json!({
                "name": "grok_cli",
                "status": status,
                "message": message,
                "can_repair": false,
            }));
        }

        // Local auto-login exposure (G8 residual-risk finding): passwordless
        // local login is only safe on a loopback bind — flag the dangerous
        // combination before any request ever proves it in production.
        if let Some(message) = crate::doctor_probes::local_auto_login_exposure(&self.home_dir) {
            checks.push(json!({
                "name": "local_auto_login_exposure",
                "status": "warn",
                "message": message,
                "can_repair": false,
            }));
        }

        checks
    }
}
