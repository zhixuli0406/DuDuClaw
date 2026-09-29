//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Skills ──────────────────────────────────────────────

    /// Seed the bundled skills (docx / xlsx / pptx / pdf / …) into a freshly
    /// created staffer's `SKILLS/`.
    ///
    /// This used to happen on exactly one of the five agent-creation paths —
    /// the MCP `create_agent` tool. Anyone who onboarded through the dashboard,
    /// `duduclaw onboard`, or the industry wizard got an empty `SKILLS/` and,
    /// since nothing else ever writes `<home>/skills/` either, a permanently
    /// blank Skills page. "The skill library shows nothing" was literally true
    /// and had nothing to do with the page's read path.
    ///
    /// Best-effort by design: seeding is a nicety, and an unwritable skills dir
    /// must not fail agent creation. `install_builtin_skills` never overwrites
    /// an existing `<name>/SKILL.md`, so calling it again is a no-op.
    pub(crate) fn seed_builtin_skills(skills_dir: &std::path::Path) {
        match duduclaw_agent::builtin_skills::install_builtin_skills(skills_dir) {
            Ok(names) if !names.is_empty() => {
                info!(dir = %skills_dir.display(), skills = ?names, "seeded built-in skills");
            }
            Ok(_) => {}
            Err(e) => {
                warn!(dir = %skills_dir.display(), error = %e, "failed to seed built-in skills");
            }
        }
    }

    /// One entry of the `scanned` diagnostic block returned by `skills.list`.
    ///
    /// The "技能庫什麼都看不到" support loop is unfalsifiable without this: an
    /// empty list is indistinguishable from "the directory I expected does not
    /// exist" / "it exists but holds zero `.md` files". Returning the exact
    /// paths that were walked plus their per-layer counts lets the dashboard
    /// empty state answer the question on screen instead of requiring shell
    /// access to the customer's machine.
    pub(crate) fn skill_layer_diag(layer: &str, dir: &std::path::Path, count: usize) -> Value {
        json!({
            "layer": layer,
            "path": dir.display().to_string(),
            "exists": dir.is_dir(),
            "count": count,
        })
    }

    /// Re-read an agent's department + per-agent skill layers from disk and
    /// compose them over the (already re-read) global layer.
    ///
    /// Extracted from the single-agent branch so the aggregate branch
    /// (`agent_id` absent) can use the exact same disk-truth path — before this
    /// it fell back to the cached `agent.skills` snapshot and therefore showed
    /// a *different* skill set than the per-agent view, missing every skill
    /// written out-of-band (MCP `skill_graduate` / synthesis pipeline) and the
    /// whole department layer.
    pub(crate) async fn read_agent_skill_layers(
        &self,
        agent: &duduclaw_agent::registry::LoadedAgent,
        global_skills: &[duduclaw_agent::registry::SkillFile],
    ) -> (Vec<Value>, Vec<Value>) {
        let local_dir = agent.dir.join("SKILLS");
        let local_skills = duduclaw_agent::registry::AgentRegistry::load_skills(&local_dir).await;

        let dept = agent.config.agent.department.trim();
        let dept_dir = if !dept.is_empty() && duduclaw_core::is_valid_department(dept) {
            Some(duduclaw_agent::skill_loader::department_skills_dir(
                &self.home_dir,
                dept,
            ))
        } else {
            None
        };
        let dept_skills = match &dept_dir {
            Some(d) => duduclaw_agent::registry::AgentRegistry::load_skills(d).await,
            None => Vec::new(),
        };

        // Layer membership decides the badge. Deriving it from "is this name
        // also in the global layer?" mislabelled an agent-local override of a
        // global skill as `global`.
        let local_names: std::collections::HashSet<String> =
            local_skills.iter().map(|s| s.name.clone()).collect();
        let dept_names: std::collections::HashSet<String> =
            dept_skills.iter().map(|s| s.name.clone()).collect();

        let mut diag = vec![Self::skill_layer_diag(
            "agent",
            &local_dir,
            local_skills.len(),
        )];
        if let Some(d) = &dept_dir {
            diag.push(Self::skill_layer_diag("department", d, dept_skills.len()));
        }

        let verdicts = self.load_skill_scan_verdicts();
        let composed = duduclaw_agent::registry::AgentRegistry::compose_skill_layers(
            global_skills,
            dept_skills,
            local_skills,
        );
        let skills: Vec<Value> = composed
            .iter()
            .map(|s| {
                let scope = if local_names.contains(&s.name) {
                    "agent"
                } else if dept_names.contains(&s.name) {
                    "department"
                } else {
                    "global"
                };
                // Include `content` so the dashboard "My Skills" tab can render a
                // preview — the SkillInfo frontend contract requires it, and omitting
                // it made `skill.content.slice(...)` throw whenever an agent had skills.
                let mut obj = json!({
                    "name": s.name,
                    "size": s.content.len(),
                    "scope": scope,
                    "content": s.content,
                    "agent_id": agent.config.agent.name,
                });
                if let Some(v) = verdicts.get(&s.name) {
                    obj["security_status"] = v.clone();
                }
                obj
            })
            .collect();
        (skills, diag)
    }

    pub(crate) async fn handle_skills_list(&self, params: Value) -> WsFrame {
        let agent_id = params.get("agent_id").and_then(|v| v.as_str());
        let reg = self.registry.read().await;

        // WP6: re-read the GLOBAL layer from disk rather than trusting
        // `reg.global_skills()`. A skill graduated by the synthesis pipeline
        // lands in `<home>/skills/` without a `registry.scan()`, so the cached
        // list would both hide it and mislabel its `scope` badge as "agent".
        let global_dir = self.home_dir.join("skills");
        let global_skills = duduclaw_agent::registry::AgentRegistry::load_skills(&global_dir).await;
        let global_diag = Self::skill_layer_diag("global", &global_dir, global_skills.len());

        // Scan verdicts recorded at install-approval time (Bug#9) — attach as
        // `security_status` so the "My Skills" security column shows the real
        // verdict rather than "Not scanned". Absent entry ⇒ field omitted.
        let verdicts = self.load_skill_scan_verdicts();
        let verdict_of = |name: &str| verdicts.get(name).cloned();

        match agent_id {
            Some(id) => {
                match reg.get(id) {
                    Some(agent) => {
                        // WP6: re-read from disk instead of trusting the
                        // in-memory `agent.skills` snapshot. `registry.scan()`
                        // only runs on dashboard-side installs, so a skill
                        // written out-of-band — the MCP `skill_from_recording` /
                        // `skill_graduate` tools, or the in-gateway synthesis
                        // pipeline — stayed invisible here until a gateway
                        // restart. That was a *read* failure, not just a
                        // staleness one: no amount of refetching would have
                        // surfaced it.
                        //
                        // All THREE layers are re-read and recomposed through
                        // the same `compose_skill_layers` the scan uses (WP7:
                        // global < department < per-agent, nearest wins).
                        // Listing only the agent's own `SKILLS/` would fix the
                        // freshness bug by deleting global and department
                        // skills from the view — a worse lie than the stale one.
                        let (skills, mut scanned) =
                            self.read_agent_skill_layers(agent, &global_skills).await;
                        scanned.push(global_diag);
                        WsFrame::ok_response(
                            "",
                            json!({ "agent_id": id, "skills": skills, "scanned": scanned }),
                        )
                    }
                    None => WsFrame::error_response("", &format!("Agent not found: {id}")),
                }
            }
            None => {
                // Global skills — the freshly-read layer, same reason as above.
                let global: Vec<Value> = global_skills
                    .iter()
                    .map(|s| {
                        let mut obj = json!({
                            "name": s.name,
                            "size": s.content.len(),
                            "scope": "global",
                            "content": s.content,
                        });
                        if let Some(v) = verdict_of(&s.name) {
                            obj["security_status"] = v;
                        }
                        obj
                    })
                    .collect();

                // Per-agent skills — re-read from disk, exactly like the
                // single-agent branch. `agent.skills` is the scan-time snapshot
                // and silently omits anything written out-of-band.
                let mut all_skills = Vec::new();
                let mut scanned = vec![global_diag];
                for agent in reg.list() {
                    let (skills, diag) = self.read_agent_skill_layers(agent, &global_skills).await;
                    // The aggregate view already lists the global layer once
                    // under `global_skills`; repeating it per agent would show
                    // the same skill N times.
                    let own: Vec<Value> = skills
                        .into_iter()
                        .filter(|s| s["scope"] != "global")
                        .collect();
                    scanned.extend(diag);
                    all_skills.push(json!({
                        "agent_id": agent.config.agent.name,
                        "display_name": agent.config.agent.display_name,
                        "skills": own,
                    }));
                }
                WsFrame::ok_response(
                    "",
                    json!({
                        "global_skills": global,
                        "agents": all_skills,
                        "scanned": scanned,
                    }),
                )
            }
        }
    }

    pub(crate) async fn handle_skills_search(&self, params: Value) -> WsFrame {
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        if query.is_empty() {
            return WsFrame::error_response("", "Missing 'query' parameter");
        }

        let lower = query.to_lowercase();
        let reg = self.registry.read().await;
        let mut results = Vec::new();

        // Search across all agents' installed skills
        for agent in reg.list() {
            for skill in &agent.skills {
                let name_match = skill.name.to_lowercase().contains(&lower);
                let content_match = skill.content.to_lowercase().contains(&lower);
                if name_match || content_match {
                    results.push(json!({
                        "name": skill.name,
                        "description": skill.content.lines().take(3).collect::<Vec<_>>().join(" ").chars().take(200).collect::<String>(),
                        "tags": [],
                        "author": agent.config.agent.name,
                        "url": "",
                        "compatible": ["duduclaw"],
                    }));
                }
            }
        }

        // Search the skill market via the multi-hub aggregator — the same
        // HubRegistry the MCP `skill_search` tool uses, so the dashboard and
        // agent-facing tools see one consistent result set (previously this
        // path used the legacy GitHub-only SkillRegistry).
        let hub_registry = duduclaw_agent::skill_hub::HubRegistry::from_home(&self.home_dir);
        let hub_ids: Vec<String> = hub_registry.ids().iter().map(|s| s.to_string()).collect();

        // Collect local skill names for dedup (MCP-L3)
        let local_names: std::collections::HashSet<String> = results
            .iter()
            .filter_map(|r| r["name"].as_str().map(|s| s.to_string()))
            .collect();

        let aggregated = hub_registry.search(&self.home_dir, query, 20, None).await;
        let hit_count = aggregated.hits.len();
        let reachable_hubs = hub_ids.len().saturating_sub(aggregated.errors.len());
        for hit in aggregated.hits {
            let entry = hit.entry;
            if !local_names.contains(&entry.name) {
                results.push(json!({
                    "name": entry.name,
                    "description": entry.description,
                    "tags": entry.tags,
                    "author": entry.author,
                    "url": entry.url,
                    "compatible": entry.compatible,
                    "hub": hit.hub,
                    "trust_tier": entry.trust_tier.as_str(),
                    "install_count": entry.install_count,
                    "source_verdict": entry.source_verdict,
                }));
            }
        }

        // Per-hub failures are surfaced, never swallowed.
        let hub_errors: Vec<Value> = aggregated
            .errors
            .iter()
            .map(|(hub, err)| json!({ "hub": hub, "error": err }))
            .collect();

        WsFrame::ok_response(
            "",
            json!({
                "skills": results,
                "source": format!("hubs:{}", hub_ids.join("+")),
                // UI contract: 0 ⇔ the market itself was unreachable (every
                // hub failed). A reachable-but-no-match query stays non-zero.
                "total_indexed": hit_count + reachable_hubs,
                "hub_errors": hub_errors,
            }),
        )
    }

    pub(crate) async fn handle_skills_content(&self, params: Value) -> WsFrame {
        let agent_id = match params.get("agent_id").and_then(|v| v.as_str()) {
            Some(id) => id,
            None => return WsFrame::error_response("", "Missing 'agent_id' parameter"),
        };
        let skill_name = match params.get("skill_name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return WsFrame::error_response("", "Missing 'skill_name' parameter"),
        };

        let reg = self.registry.read().await;
        match reg.get(agent_id) {
            Some(agent) => match agent.skills.iter().find(|s| s.name == skill_name) {
                Some(skill) => WsFrame::ok_response(
                    "",
                    json!({
                        "agent_id": agent_id,
                        "skill_name": skill_name,
                        "content": skill.content,
                    }),
                ),
                None => WsFrame::error_response("", &format!("Skill not found: {skill_name}")),
            },
            None => WsFrame::error_response("", &format!("Agent not found: {agent_id}")),
        }
    }

    // ── Skill Vetting & Install ──────────────────────────────

    /// Resolve a user-supplied skill source URL to the raw-content URL to fetch.
    ///
    /// Accepts GitHub repo/blob URLs, GitHub gists, GitLab repo/blob URLs, and
    /// any direct https URL to raw SKILL.md content. The scheme/host anchor
    /// checks use the parsed URL host (never substring matching) so
    /// `github.com.evil.example` cannot impersonate GitHub.
    pub(crate) fn resolve_skill_source_url(url: &str) -> Result<String, String> {
        let trimmed = url.trim().trim_end_matches('/');
        let parsed = reqwest::Url::parse(trimmed).map_err(|e| format!("invalid URL: {e}"))?;
        if parsed.scheme() != "https" && parsed.scheme() != "http" {
            return Err(format!("unsupported scheme '{}://'", parsed.scheme()));
        }
        let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
        let path = parsed.path().trim_end_matches('/');

        // Does the last path segment name a file (has a `.ext`)? Used to tell a
        // `/tree/<ref>/<dir>` directory URL (append SKILL.md) apart from a
        // `/tree/<ref>/<file>` URL (treat like a blob, fetch the file itself).
        let path_names_file = |p: &str| -> bool {
            p.rsplit('/')
                .next()
                .map(|seg| seg.contains('.'))
                .unwrap_or(false)
        };

        Ok(match host.as_str() {
            // github.com/user/repo             -> raw HEAD/SKILL.md
            // github.com/user/repo/blob/x/f.md -> raw file
            // github.com/user/repo/tree/x/dir  -> raw dir/SKILL.md (subdir skill —
            //   the most common share format; a tree URL that names a file is
            //   treated like a blob)
            "github.com" | "www.github.com" => {
                if path.contains("/blob/") {
                    format!(
                        "https://raw.githubusercontent.com{}",
                        path.replacen("/blob/", "/", 1)
                    )
                } else if path.contains("/tree/") {
                    let raw = path.replacen("/tree/", "/", 1);
                    if path_names_file(&raw) {
                        format!("https://raw.githubusercontent.com{raw}")
                    } else {
                        format!("https://raw.githubusercontent.com{raw}/SKILL.md")
                    }
                } else {
                    format!("https://raw.githubusercontent.com{path}/HEAD/SKILL.md")
                }
            }
            // gist.github.com/user/id -> gist raw (latest revision)
            "gist.github.com" => format!("https://gist.githubusercontent.com{path}/raw"),
            // gitlab.com/user/repo               -> raw HEAD/SKILL.md
            // gitlab.com/user/repo/-/blob/x/f    -> /-/raw/x/f
            // gitlab.com/user/repo/-/tree/x/dir  -> /-/raw/x/dir/SKILL.md
            "gitlab.com" | "www.gitlab.com" => {
                if path.contains("/-/blob/") {
                    format!(
                        "https://gitlab.com{}",
                        path.replacen("/-/blob/", "/-/raw/", 1)
                    )
                } else if path.contains("/-/tree/") {
                    let raw = path.replacen("/-/tree/", "/-/raw/", 1);
                    if path_names_file(&raw) {
                        format!("https://gitlab.com{raw}")
                    } else {
                        format!("https://gitlab.com{raw}/SKILL.md")
                    }
                } else {
                    format!("https://gitlab.com{path}/-/raw/HEAD/SKILL.md")
                }
            }
            // Anything else: treat as a direct link to raw skill content
            // (raw.githubusercontent.com, company file servers, ...).
            _ => trimmed.to_string(),
        })
    }

    /// Max bytes accepted when fetching remote skill content.
    pub(crate) const SKILL_FETCH_MAX_BYTES: usize = 1024 * 1024;

    /// Fetch a remote text resource with a byte cap and per-redirect SSRF
    /// re-validation (a public URL must not be allowed to 302 into
    /// 169.254.169.254 or the LAN). Shared by skill vetting and MCP import.
    pub(crate) async fn fetch_remote_text(url: &str, max_bytes: usize) -> Result<String, String> {
        let redirect_policy = reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many redirects");
            }
            match crate::web_fetch::validate_url(attempt.url().as_str()) {
                Ok(_) => attempt.follow(),
                Err(e) => attempt.error(format!("redirect blocked: {e}")),
            }
        });
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15))
            .redirect(redirect_policy)
            .user_agent("duduclaw-import")
            .build()
            .map_err(|e| format!("http client init failed: {e}"))?;

        let resp = client.get(url).send().await.map_err(|e| format!("{e}"))?;
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        if let Some(len) = resp.content_length() {
            if len as usize > max_bytes {
                return Err(format!("response too large ({len} bytes, max {max_bytes})"));
            }
        }
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| format!("read failed: {e}"))?;
        if bytes.len() > max_bytes {
            return Err(format!(
                "response too large ({} bytes, max {max_bytes})",
                bytes.len()
            ));
        }
        String::from_utf8(bytes.to_vec())
            .map_err(|_| "response is not valid UTF-8 text".to_string())
    }

    pub(crate) async fn handle_skills_vet(&self, params: Value) -> WsFrame {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) if !u.is_empty() => u,
            _ => return WsFrame::error_response("", "Missing 'url' parameter"),
        };

        // Resolve GitHub/GitLab/gist/direct URLs to raw content, then gate the
        // resolved target through the shared SSRF validator (blocks loopback,
        // private ranges, cloud metadata endpoints, non-http schemes).
        let raw_url = match Self::resolve_skill_source_url(url) {
            Ok(u) => u,
            Err(e) => {
                return WsFrame::error_response("", &format!("Invalid skill source URL: {e}"));
            }
        };
        if let Err(e) = crate::web_fetch::validate_url(&raw_url) {
            return WsFrame::error_response("", &format!("Skill source URL rejected: {e}"));
        }
        let content = match Self::fetch_remote_text(&raw_url, Self::SKILL_FETCH_MAX_BYTES).await {
            Ok(text) => text,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to fetch skill content: {e}"));
            }
        };

        // An HTML page means the URL points at a web view, not raw content —
        // scanning rendered HTML would produce a garbage verdict.
        let head = content.trim_start();
        if head.starts_with("<!DOCTYPE")
            || head.starts_with("<!doctype")
            || head.starts_with("<html")
        {
            return WsFrame::error_response(
                "",
                "URL returned an HTML page, not raw SKILL.md content. Use a raw file link (e.g. raw.githubusercontent.com) or the repo root.",
            );
        }

        // Extract skill name from frontmatter (best-effort)
        let skill_name = content
            .lines()
            .find(|l| l.starts_with("name:"))
            .and_then(|l| l.strip_prefix("name:"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Rust-native security scan (no Python dependency). The same scanner
        // backs the MCP `skill_security_scan` tool and the sandbox-trial gate,
        // so the dashboard, agents, and lifecycle pipeline all share one
        // verdict. CONTRACT.toml boundaries are not available on this path.
        let scan = crate::skill_lifecycle::security_scanner::scan_skill(&content, None);
        let passed = scan.passed;
        let vet_result = json!({
            "passed": scan.passed,
            "risk_level": format!("{:?}", scan.risk_level),
            "findings": scan.findings.iter().map(|f| json!({
                "category": format!("{:?}", f.category),
                "severity": format!("{:?}", f.severity).to_lowercase(),
                "description": f.description,
                "line_number": f.line_number,
                "pattern": f.matched_pattern,
            })).collect::<Vec<_>>(),
        });

        WsFrame::ok_response(
            "",
            json!({
                "skill_name": skill_name,
                "content": content,
                "vet_result": vet_result,
                "passed": passed,
            }),
        )
    }

    pub(crate) async fn handle_skills_install(&self, params: Value) -> WsFrame {
        let url = match params.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return WsFrame::error_response("", "Missing 'url' parameter"),
        };
        let scope = match params.get("scope").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'scope' parameter"),
        };
        let content = match params.get("content").and_then(|v| v.as_str()) {
            Some(c) if !c.is_empty() => c.to_string(),
            _ => return WsFrame::error_response("", "Missing 'content' parameter"),
        };

        // Extract skill name from content frontmatter
        let skill_name = content
            .lines()
            .find(|l| l.starts_with("name:"))
            .and_then(|l| l.strip_prefix("name:"))
            .map(|n| n.trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());

        // Mandatory server-side security scan of the *content that will be
        // installed* — the vet RPC is a separate call the client may skip, so
        // install must re-run the scanner itself. Same fail-closed policy as
        // skill_lifecycle::hub_install: risk ≥ High ⇒ reject with findings;
        // Clean/Low/Medium proceed.
        let scan = crate::skill_lifecycle::security_scanner::scan_skill(&content, None);
        if !scan.passed {
            warn!(
                skill = %skill_name,
                risk = ?scan.risk_level,
                findings = scan.findings.len(),
                "skills.install DENIED by server-side security scan"
            );
            return WsFrame::error_response(
                "",
                &format!(
                    "Security scan rejected skill '{skill_name}': risk {:?}, {} finding(s): {}",
                    scan.risk_level,
                    scan.findings.len(),
                    scan.findings
                        .iter()
                        .take(5)
                        .map(|f| f.description.as_str())
                        .collect::<Vec<_>>()
                        .join("; "),
                ),
            );
        }

        match self.run_skill_install(&scope, &content, &skill_name).await {
            Ok(installed_name) => {
                info!(skill = %installed_name, scope = %scope, url = %url, "Skill installed via dashboard");
                WsFrame::ok_response(
                    "",
                    json!({
                        "success": true,
                        "skill_name": installed_name,
                        "scope": scope,
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("Install failed: {e}")),
        }
    }

    /// Perform the on-disk skill install for an already-scanned, approved
    /// content string. Shared by the direct admin path (`skills.install`) and
    /// the approved-request execution path. Does NOT scan — the caller MUST
    /// have run `scan_skill` and confirmed `passed` first.
    pub(crate) async fn run_skill_install(
        &self,
        scope: &str,
        content: &str,
        skill_name: &str,
    ) -> Result<String, String> {
        let tmp_dir = std::env::temp_dir().join("duduclaw-skill-install");
        std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("create temp dir: {e}"))?;
        // `skill_name` may come from user-supplied frontmatter — sanitize so a
        // `name: ../../x` can never shape a path outside the temp dir.
        let tmp_file = tmp_dir.join(format!(
            "{}.md",
            crate::install_notify::sanitize_tmp_file_stem(skill_name)
        ));
        std::fs::write(&tmp_file, content).map_err(|e| format!("write temp file: {e}"))?;

        let quarantine_dir = self.home_dir.join("quarantine");
        let install_result = if scope == "global" {
            duduclaw_agent::skill_loader::install_skill_global(
                &tmp_file,
                &self.home_dir,
                &quarantine_dir,
            )
            .await
        } else if let Some(dept) = scope.strip_prefix("department:") {
            duduclaw_agent::skill_loader::install_skill_department(
                &tmp_file,
                &self.home_dir,
                dept,
                &quarantine_dir,
            )
            .await
        } else {
            if !is_valid_agent_id(scope) {
                let _ = std::fs::remove_file(&tmp_file);
                return Err("Invalid agent_id for scope".into());
            }
            let agent_skills_dir = self.home_dir.join("agents").join(scope).join("SKILLS");
            duduclaw_agent::skill_loader::install_skill(
                &tmp_file,
                &agent_skills_dir,
                &quarantine_dir,
            )
            .await
        };
        let _ = std::fs::remove_file(&tmp_file);

        match install_result {
            Ok(parsed) => {
                let mut registry = self.registry.write().await;
                if let Err(e) = registry.scan().await {
                    warn!("Failed to rescan agents after skill install: {e}");
                }
                Ok(parsed.meta.name)
            }
            Err(e) => Err(e),
        }
    }
}
