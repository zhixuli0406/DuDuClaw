//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Expert packs (專家包) — dashboard admin surface ─────────────────────
    //
    // Shared on-disk contracts + remove/hooks semantics live in
    // `crate::expert_admin` (also consumed by the `duduclaw expert` CLI).
    // Install reuses the FULL CLI pipeline (format detection, safe_zip
    // zip-slip fence + 50 MB cap, prompt-injection / skill-security scanning,
    // hook quarantine) by spawning `duduclaw pack install <path>` — the
    // installer is entangled with cli-only agent scaffolding, so a subprocess
    // is the zero-drift reuse path (same pattern as doctor_probes → mcp-server).

    pub(crate) async fn handle_experts_list(&self) -> WsFrame {
        let packs: Vec<Value> = crate::expert_admin::list_records(&self.home_dir)
            .into_iter()
            .map(|r| {
                let hooks = crate::expert_admin::read_hooks_state(&self.home_dir, &r.slug);
                json!({
                    "slug": r.slug,
                    "kind": r.kind.label(),
                    "display_name": if r.display_name.is_empty() { r.slug.clone() } else { r.display_name.clone() },
                    "version": r.version,
                    "description": r.description,
                    "agents": r.agents,
                    "skills_count": r.global_skills.len(),
                    "wiki_count": r.wiki_files.len(),
                    "installed_at": r.installed_at,
                    // null ⇒ the pack ships no managed hooks.
                    "hooks_status": hooks.as_ref().map(|h| h.status.as_str()),
                    "hooks_files": hooks.map(|h| h.files.len()).unwrap_or(0),
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "packs": packs }))
    }

    pub(crate) async fn handle_experts_install(&self, params: Value) -> WsFrame {
        let Some(path) = params.get("path").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "path parameter is required");
        };
        let src = std::path::Path::new(path);
        if !src.exists() {
            return WsFrame::error_response("", "安裝來源不存在（請重新上傳）");
        }
        let is_zip = src.is_file()
            && src
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("zip"))
                .unwrap_or(false);
        if !src.is_dir() && !is_zip {
            return WsFrame::error_response("", "安裝來源必須是資料夾或 .zip 檔");
        }

        // Reuse the full CLI install pipeline via our own binary. Hooks are
        // NEVER auto-trusted from the dashboard — they land disabled with an
        // approval request (fail-closed), decided in the approval center.
        let mut args = vec!["install".into(), src.as_os_str().to_os_string()];
        match Self::attach_under_args(&params) {
            Ok(extra) => args.extend(extra),
            Err(frame) => return frame,
        }
        match self.spawn_pack_cli(&args, 300).await {
            Ok(output) => WsFrame::ok_response("", json!({ "success": true, "output": output })),
            Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
        }
    }

    /// WP-ORG: optional `attach_under` param → `--attach-under <id>` CLI args.
    /// The id shape is fenced here (fail-closed error, never silently dropped)
    /// so an arbitrary string can never become a stray flag; the CLI then
    /// validates the target actually exists before installing.
    pub(crate) fn attach_under_args(params: &Value) -> Result<Vec<std::ffi::OsString>, WsFrame> {
        match params
            .get("attach_under")
            .and_then(|v| v.as_str())
            .map(str::trim)
        {
            None | Some("") => Ok(Vec::new()),
            Some(id) if crate::premium_templates::is_safe_slug(id) => {
                Ok(vec!["--attach-under".into(), id.into()])
            }
            Some(id) => Err(WsFrame::error_response(
                "",
                &format!("attach_under '{}' 非合法 agent id", id.escape_debug()),
            )),
        }
    }

    /// Spawn our own binary's `duduclaw expert <args…>` sub-command against
    /// the gateway home — the zero-drift reuse path for the CLI install /
    /// convert pipelines (same pattern as `doctor_probes` → `mcp-server`).
    /// Returns the CJK-safe-truncated stdout tail on success.
    pub(crate) async fn spawn_expert_cli(
        &self,
        args: &[std::ffi::OsString],
        timeout_secs: u64,
    ) -> Result<String, String> {
        self.spawn_cli_group("expert", args, timeout_secs).await
    }

    /// Spawn our own binary's `duduclaw pack <args…>` sub-command — the
    /// install front door (`expert install` was removed in v1.69.0). Same
    /// home pinning, timeout and output handling as [`Self::spawn_expert_cli`].
    pub(crate) async fn spawn_pack_cli(
        &self,
        args: &[std::ffi::OsString],
        timeout_secs: u64,
    ) -> Result<String, String> {
        self.spawn_cli_group("pack", args, timeout_secs).await
    }

    async fn spawn_cli_group(
        &self,
        group: &str,
        args: &[std::ffi::OsString],
        timeout_secs: u64,
    ) -> Result<String, String> {
        let bin = duduclaw_core::resolve_duduclaw_bin();
        self.spawn_cli_group_with_bin(&bin, group, args, timeout_secs).await
    }

    /// [`Self::spawn_cli_group`] with the binary given, so tests can observe
    /// the argv without touching the process-wide `DUDUCLAW_BIN`.
    pub(crate) async fn spawn_cli_group_with_bin(
        &self,
        bin: &std::path::Path,
        group: &str,
        args: &[std::ffi::OsString],
        timeout_secs: u64,
    ) -> Result<String, String> {
        let fut = tokio::process::Command::new(bin)
            .arg(group)
            .args(args)
            .env("DUDUCLAW_HOME", &self.home_dir)
            .kill_on_drop(true)
            .output();
        let output =
            match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut).await {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => return Err(format!("子程序啟動失敗: {e}")),
                Err(_) => return Err(format!("子程序逾時（{timeout_secs} 秒），已中止")),
            };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Strip ANSI-free console tail for surfacing (CJK-safe truncation).
        let tail = |s: &str| duduclaw_core::truncate_chars(s.trim(), 1200);
        if !output.status.success() {
            let detail = if stderr.trim().is_empty() {
                &stdout
            } else {
                &stderr
            };
            return Err(tail(detail));
        }
        Ok(tail(&stdout))
    }

    pub(crate) async fn handle_experts_remove(&self, params: Value) -> WsFrame {
        let Some(slug) = params.get("slug").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "slug parameter is required");
        };
        match crate::expert_admin::remove_pack(&self.home_dir, slug).await {
            Ok(items) => WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "items": serde_json::to_value(&items).unwrap_or(Value::Null),
                }),
            ),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_experts_hooks_apply(&self, params: Value) -> WsFrame {
        let Some(slug) = params.get("slug").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "slug parameter is required");
        };
        use crate::expert_admin::HooksApplyOutcome as O;
        match crate::expert_admin::apply_hooks_decision(&self.home_dir, slug).await {
            Ok(O::Enabled { files }) => {
                WsFrame::ok_response("", json!({ "status": "enabled", "files": files }))
            }
            Ok(O::Disabled) => WsFrame::ok_response("", json!({ "status": "disabled" })),
            Ok(O::StillPending { approval_id }) => WsFrame::ok_response(
                "",
                json!({ "status": "pending_approval", "approval_id": approval_id }),
            ),
            Ok(O::DeniedOrExpired { status }) => {
                WsFrame::ok_response("", json!({ "status": status }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    // ── Built-in expert-pack catalog (22 industry teams → one-click install) ──

    /// `experts.catalog` — built-in industry packs available for one-click
    /// install. Fail-safe: absent premium tree ⇒ `deployed: false`, never an
    /// error. Locked license ⇒ list withheld with the upsell flag (same
    /// convention as `templates.industries`).
    pub(crate) async fn handle_experts_catalog(&self) -> WsFrame {
        let unlocked = self.premium_templates_unlocked().await;
        let dir = crate::premium_templates::find_premium_templates_dir();
        let records = crate::expert_admin::list_records(&self.home_dir);
        let catalog = crate::expert_generate::builtin_catalog(dir.as_deref(), &records);
        let deployed = catalog["deployed"].as_bool().unwrap_or(false);
        WsFrame::ok_response(
            "",
            json!({
                "deployed": deployed,
                "unlocked": unlocked,
                "present_but_locked": deployed && !unlocked,
                "packs": if unlocked { catalog["packs"].clone() } else { json!([]) },
            }),
        )
    }

    // ── Inspiration gallery (P2-b "一鍵做同款", curated-only MVP) ──

    /// `gallery.list` — read-only fan-out of the same `team.toml` task
    /// examples `experts.catalog` already reads (`expert_generate::
    /// gallery_cards`), reformatted as one showcase card per example instead
    /// of grouped under its team. No new storage: this assembles straight
    /// from the builtin catalog's source data on every call. Same
    /// license/deployment gating as `experts.catalog` (same underlying
    /// premium tree) — `present_but_locked` lets the dashboard show an
    /// upsell instead of an empty grid.
    ///
    /// Deliberately NOT included in this MVP wave (tracked in the P2-b design
    /// doc): user-submitted "我的" gallery entries from real completed goal
    /// runs — that depends on artifact objectification (I-2b), which has not
    /// shipped yet.
    pub(crate) async fn handle_gallery_list(&self) -> WsFrame {
        let unlocked = self.premium_templates_unlocked().await;
        let dir = crate::premium_templates::find_premium_templates_dir();
        let records = crate::expert_admin::list_records(&self.home_dir);
        let gallery = crate::expert_generate::gallery_cards(dir.as_deref(), &records);
        let deployed = gallery["deployed"].as_bool().unwrap_or(false);
        WsFrame::ok_response(
            "",
            json!({
                "deployed": deployed,
                "unlocked": unlocked,
                "present_but_locked": deployed && !unlocked,
                "cards": if unlocked { gallery["cards"].clone() } else { json!([]) },
            }),
        )
    }

    /// `experts.install_builtin` — convert (cached, idempotent) + install one
    /// built-in industry pack. Both steps reuse the CLI pipelines via
    /// subprocess: `expert convert-teams` then `pack install` (full
    /// security scanning, hooks fail-closed).
    pub(crate) async fn handle_experts_install_builtin(&self, params: Value) -> WsFrame {
        use crate::expert_generate as eg;
        let attach_args = match Self::attach_under_args(&params) {
            Ok(a) => a,
            Err(frame) => return frame,
        };

        // WP-ORG: `slug` (no `industry`) selects a standalone pack shipped
        // under `<premium>/experts/<slug>/` — installed straight from the
        // premium tree, no conversion step.
        let industry = params
            .get("industry")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if industry.is_empty() {
            let slug = params.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            if !crate::premium_templates::is_safe_slug(slug) || slug.ends_with("-team") {
                return WsFrame::error_response("", "找不到此內建專家包");
            }
            let premium_dir = match self.premium_dir_unlocked().await {
                Ok(d) => d,
                Err(frame) => return frame,
            };
            let pack_dir = premium_dir.join("experts").join(slug);
            if !pack_dir.join("expert.toml").is_file() {
                return WsFrame::error_response("", "找不到此內建專家包");
            }
            let mut args = vec!["install".into(), pack_dir.into_os_string()];
            args.extend(attach_args);
            return match self.spawn_pack_cli(&args, 300).await {
                Ok(output) => WsFrame::ok_response(
                    "",
                    json!({ "success": true, "slug": slug, "output": output }),
                ),
                Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
            };
        }

        // Slug fence FIRST — traversal is rejected before license checks or
        // any subprocess (deterministic fail-closed ordering).
        let cache_pack = match eg::builtin_pack_cache_dir(&self.home_dir, industry) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let premium_dir = match self.premium_dir_unlocked().await {
            Ok(d) => d,
            Err(frame) => return frame,
        };
        if let Err(e) = crate::premium_templates::load_team_manifest(&premium_dir, industry) {
            warn!(industry, error = %e, "install_builtin: unknown/invalid team manifest");
            return WsFrame::error_response("", "找不到此產業的內建專家包");
        }

        // Ensure the converted cache. `convert-teams` has no per-industry
        // filter, so the whole tree is converted once (idempotent,
        // byte-deterministic) and reused afterwards.
        if !cache_pack.join("expert.toml").is_file() {
            let out_dir = eg::builtin_cache_dir(&self.home_dir);
            if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
                return WsFrame::error_response("", &format!("建立快取目錄失敗: {e}"));
            }
            let convert = self
                .spawn_expert_cli(
                    &[
                        "convert-teams".into(),
                        premium_dir.join("teams").into_os_string(),
                        "--out".into(),
                        out_dir.into_os_string(),
                    ],
                    120,
                )
                .await;
            if let Err(e) = convert {
                // The batch errs when ANY team fails; THIS industry may still
                // have converted fine — proceed only when its pack exists.
                if !cache_pack.join("expert.toml").is_file() {
                    return WsFrame::error_response("", &format!("內建包轉換失敗：{e}"));
                }
                warn!(industry, error = %e, "convert-teams reported failures; target pack present — proceeding");
            }
        }

        let mut args = vec!["install".into(), cache_pack.into_os_string()];
        args.extend(attach_args);
        match self.spawn_pack_cli(&args, 300).await {
            Ok(output) => WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "slug": eg::builtin_pack_slug(industry),
                    "output": output,
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
        }
    }

    // LLM-guided authoring (experts.generate*, experts.install_draft) lives
    // in `experts_generate_rpc.rs`.
}
