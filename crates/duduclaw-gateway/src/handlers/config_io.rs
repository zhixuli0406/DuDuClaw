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

    /// Write a TOML table back to disk, format-preserving: only the keys
    /// that differ from the file on disk are edited (see
    /// `config_commit::render_preserving`).
    ///
    /// v1.68: refuses when the file being replaced exists but does not parse.
    /// Callers read through the lenient [`Self::read_config_table`], which
    /// turns an unparsable file into an empty table — writing that back would
    /// replace the whole config with the few keys the RPC carried. `path` may
    /// be the `*.tmp` staging file; the check then looks at the real file.
    pub(crate) async fn write_config_table(
        &self,
        path: &std::path::Path,
        table: &toml::Table,
    ) -> std::io::Result<()> {
        let target = if path.extension().is_some_and(|e| e == "tmp") {
            path.with_extension("")
        } else {
            path.to_path_buf()
        };
        if target.file_name().and_then(|n|n.to_str())==Some("config.toml") {
            return Err(std::io::Error::other("config.toml requires snapshot-bound final commit"));
        }
        refuse_overwriting_unparsable(&target).await?;
        // Edit the file as it stands rather than re-serialising the table, so
        // comments, blank lines and key order survive (`render_preserving`).
        let original = match tokio::fs::read_to_string(&target).await {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let content = super::config_commit::render_preserving(&original, table)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        crate::product_policy::validate_write(&self.home_dir, &target, &original, &content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::PermissionDenied, e))?;
        tokio::fs::write(path, content).await
    }

    pub(crate) async fn write_config_table_snapshot(&self,path:&std::path::Path,expected_hash:String,table:&toml::Table)->std::io::Result<()> {
        super::config_commit::commit_table_locked(path,expected_hash,table).await.map_err(std::io::Error::other)
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
        let active_agents = reg.list().iter()
            .filter(|agent| agent.config.agent.status.is_operational()).count();
        let has_key = self.has_api_key().await;
        let config_exists = self.home_dir.join("config.toml").exists();

        // The spawning probes run concurrently — worst case the RPC
        // costs max(docker, grok ping 15s+version 5s, mcp cold-start 10s),
        // not the sum. The dashboard passes an extended per-call timeout.
        let (docker, mcp, grok, codex_sandbox) = tokio::join!(
            check_docker(),
            crate::doctor_probes::mcp_cold_start_probe(&self.home_dir),
            crate::doctor_probes::grok_probe(&self.home_dir),
            codex_sandbox_status_and_message(),
        );

        // O16: the three always-present rows come from `crate::os_ops`, which
        // the agent-facing `os_doctor_repair` MCP tool also builds from —
        // one definition of "what config_file/agents/api_key look like",
        // two projections (`can_repair` is dashboard-only).
        let mut checks = crate::os_ops::doctor_base_checks(config_exists, has_agents, has_key, true);
        checks.extend(crate::runtime_readiness::configured_checks(&self.home_dir));
        checks.push(container_memory_check(
            duduclaw_core::ns_guard::cgroup_memory_limit(std::path::Path::new("/sys/fs/cgroup")),
            active_agents,
            crate::webchat::configured_global_limit(&self.home_dir),
        ));
        checks.push(json!({
            "name": "skill_default_layers",
            "status": "pass",
            "message": "新員工共用公司層的 docx、pdf、pptx、xlsx，不再複製預設技能。舊員工的副本會保留；清理前請逐檔比對內容雜湊（含腳本），確認與公司版本相同後，再於技能頁移除個人副本，必要時從封存還原。",
            "can_repair": false,
        }));
        checks.push(json!({
            "name": "codex_employee_shell",
            "status": codex_sandbox.0,
            "message": codex_sandbox.1,
            "can_repair": false,
        }));
        if let Some((status, message)) = container_process_isolation_status_and_message(true) {
            checks.push(json!({
                "name": "container_process_isolation",
                "status": status,
                "message": message,
                "can_repair": false,
            }));
        }
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

        #[cfg(target_os = "macos")]
        if let Some(message) = crate::doctor_probes::launchd_restart_warning().await {
            checks.push(json!({"name": "launchd_restart", "status": "warn", "message": message, "can_repair": false}));
        }
        // RM-155: last background probe of the protected folders this
        // gateway uses (DUDUCLAW_HOME, `[files] allowed_roots`, dispatch work
        // dirs). Read from the probe registry — the doctor never touches a
        // protected path itself, which could raise (or hang on) a prompt.
        #[cfg(target_os = "macos")]
        if let Some(card) = crate::macos_folder_access::doctor_check(&self.home_dir) {
            checks.push(card);
        }

        // Live dashboard permissions (`live_context.rs`): shown only after a
        // read of the account store failed, which refused that call. Silence
        // here means every check since start-up read the store fine.
        let (acl_failures, acl_last) = super::live_context::read_failure_stats();
        if acl_failures > 0 {
            let last = chrono::DateTime::from_timestamp(acl_last as i64, 0)
                .map(|t| t.to_rfc3339())
                .unwrap_or_default();
            checks.push(json!({
                "name": "dashboard_permissions",
                "status": "warn",
                "message": format!(
                    "閘道啟動後有 {acl_failures} 次讀不到帳號資料庫（最近一次 {last}），那幾次 dashboard 操作都被拒絕。請檢查磁碟空間與資料目錄的讀寫權限。"
                ),
                "can_repair": false,
            }));
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

        // v1.84.0: employees whose saved model is a Claude alias their engine
        // cannot run; the engine's default model was used instead.
        let substitutions = crate::runtime_config::model_substitution_notices();
        if !substitutions.is_empty() {
            let lines: Vec<String> = substitutions
                .iter()
                .map(|(agent, notice)| format!("{agent}：{notice}"))
                .collect();
            checks.push(json!({
                "name": "employee_engine_model",
                "status": "warn",
                "message": lines.join("\n"),
                "can_repair": false,
            }));
        }
        let employees: Vec<(String, std::path::PathBuf)> = reg
            .list()
            .iter()
            .map(|a| (a.config.agent.name.clone(), a.dir.clone()))
            .collect();
        if let Some(line) = crate::runtime_config::utility_skipped_doctor_line(&self.home_dir, &employees) {
            checks.push(json!({
                "name": "background_work_skipped",
                "status": "warn",
                "message": line,
                "can_repair": false,
            }));
        }

        if let Some(message) = duduclaw_core::config::home_permissions_warning(&self.home_dir) {
            checks.push(json!({
                "name": "home_permissions",
                "status": "warn",
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

        // Subscription (OAuth) accounts configured: the policy risk line.
        if let Some(message) = crate::doctor_probes::subscription_risk_notice(&self.home_dir) {
            checks.push(json!({
                "name": "subscription_login_risk",
                "status": "warn",
                "message": message,
                "can_repair": false,
            }));
        }

        checks
    }
}

/// `Err` when `path` exists and is not valid TOML (see `write_config_table`).
pub(crate) async fn refuse_overwriting_unparsable(path: &std::path::Path) -> std::io::Result<()> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) if text.parse::<toml::Table>().is_err() => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is not valid TOML — refusing to overwrite it (fix it in the raw editor or a terminal first)",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("config file")
            ),
        )),
        _ => Ok(()),
    }
}

// Use the same binary units as Docker's memory limits. The UI uses familiar
// GB/MB labels because these are approximate planning figures.
const MEMORY_BASE_BYTES: u64 = 1024 * 1024 * 1024;
const MEMORY_PER_REPLY_BYTES: u64 = 400 * 1024 * 1024;

fn container_memory_check(limit: Option<u64>, active_agents: usize, global_turns: usize) -> Value {
    let concurrent = active_agents.min(global_turns) as u64;
    let estimate = MEMORY_BASE_BYTES.saturating_add(MEMORY_PER_REPLY_BYTES.saturating_mul(concurrent));
    let (status, message) = match limit {
        Some(limit) if limit < estimate => {
            let gb = limit as f64 / MEMORY_BASE_BYTES as f64;
            let capacity = limit.saturating_sub(MEMORY_BASE_BYTES) / MEMORY_PER_REPLY_BYTES;
            let recommended = estimate.div_ceil(MEMORY_BASE_BYTES);
            ("warn", format!(
                "容器記憶體上限為 {gb:.2} GB。每位同時回覆的 AI 員工約需 400 MB，目前的上限大約只夠 {capacity} 位同時回覆；超過時系統會強制結束正在回覆的工作。建議把上限調到至少 {recommended} GB，或取消上限。"
            ))
        }
        Some(limit) => {
            let gb = limit as f64 / MEMORY_BASE_BYTES as f64;
            ("pass", format!("容器記憶體上限為 {gb:.2} GB，足以支應目前預估的 {concurrent} 位 AI 員工同時回覆。"))
        }
        None => ("pass", "容器記憶體：未設定上限（未偵測到有限的記憶體上限）。".to_string()),
    };
    json!({
        "name": "container_memory",
        "status": status,
        "message": message,
        "can_repair": false,
    })
}

#[cfg(test)]
mod container_memory_tests {
    use super::*;

    #[test]
    fn below_estimate_warns_with_capacity_and_rounded_recommendation() {
        let estimate = MEMORY_BASE_BYTES + 5 * MEMORY_PER_REPLY_BYTES;
        let check = container_memory_check(Some(estimate - 1), 5, 10);
        assert_eq!(check["status"], "warn");
        assert!(check["message"].as_str().unwrap().contains("4 位同時回覆"));
        assert!(check["message"].as_str().unwrap().contains("至少 3 GB"));
        assert_eq!(check["can_repair"], false);
    }

    #[test]
    fn equal_estimate_passes() {
        let estimate = MEMORY_BASE_BYTES + 5 * MEMORY_PER_REPLY_BYTES;
        assert_eq!(container_memory_check(Some(estimate), 5, 10)["status"], "pass");
    }

    #[test]
    fn above_estimate_passes() {
        let estimate = MEMORY_BASE_BYTES + 5 * MEMORY_PER_REPLY_BYTES;
        assert_eq!(container_memory_check(Some(estimate + 1), 5, 10)["status"], "pass");
    }

    #[test]
    fn no_limit_passes() {
        let check = container_memory_check(None, 20, 10);
        assert_eq!(check["status"], "pass");
        assert!(check["message"].as_str().unwrap().contains("未設定上限"));
    }

    #[test]
    fn estimate_caps_concurrency_at_global_turns() {
        let estimate = MEMORY_BASE_BYTES + 2 * MEMORY_PER_REPLY_BYTES;
        assert_eq!(container_memory_check(Some(estimate), 20, 2)["status"], "pass");
        assert_eq!(container_memory_check(Some(estimate - 1), 20, 2)["status"], "warn");
        assert_eq!(container_memory_check(Some(MEMORY_BASE_BYTES), 0, 10)["status"], "pass");
        assert_eq!(container_memory_check(Some(MEMORY_BASE_BYTES), 10, 0)["status"], "pass");
    }

    #[test]
    fn below_base_has_zero_capacity() {
        for limit in [0, MEMORY_BASE_BYTES - 1, MEMORY_BASE_BYTES] {
            let check = container_memory_check(Some(limit), 5, 10);
            assert_eq!(check["status"], "warn");
            assert!(check["message"].as_str().unwrap().contains("0 位同時回覆"));
        }
    }

    #[test]
    fn estimate_uses_configured_global_limit_and_default() {
        let home = tempfile::tempdir().unwrap();
        let limit = MEMORY_BASE_BYTES + 10 * MEMORY_PER_REPLY_BYTES;
        assert_eq!(container_memory_check(Some(limit), 20,
            crate::webchat::configured_global_limit(home.path()))["status"], "pass");
        assert_eq!(container_memory_check(Some(limit - 1), 20,
            crate::webchat::configured_global_limit(home.path()))["status"], "warn");
        std::fs::write(home.path().join("config.toml"), "[webchat]\nmax_global_turns = 2\n").unwrap();
        assert_eq!(container_memory_check(Some(MEMORY_BASE_BYTES + 2 * MEMORY_PER_REPLY_BYTES), 20,
            crate::webchat::configured_global_limit(home.path()))["status"], "pass");
    }
}
