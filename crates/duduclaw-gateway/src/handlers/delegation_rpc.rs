//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── WP21 §2.8 — delegation policy admin surface ──────────────────────────
    //
    // `config.toml [delegation]` is the org-wide answer to "which AI staffer may
    // hand work to which". The enforcement points re-read it on every decision,
    // so a dashboard write takes effect without a restart.
    //
    //   [delegation]
    //   policy = "department"                        # department / hierarchy / open
    //   allow  = [["sales-lead", "warehouse-lead"]]  # unordered pair ⇒ two-way
    //
    // Both RPCs are admin-gated in `dispatch` (same gate as `runtime.install` /
    // `system.update_config`), and every accepted write is appended to the
    // security audit log as `delegation_config_changed` with before/after.

    /// Order-insensitive canonical form of a whitelist pair, so `["a","b"]` and
    /// `["b","a"]` are the same entry for dedup purposes.
    pub(crate) fn delegation_pair_key(a: &str, b: &str) -> (String, String) {
        if a <= b {
            (a.to_string(), b.to_string())
        } else {
            (b.to_string(), a.to_string())
        }
    }

    /// Read `[delegation]` out of an already-parsed config table.
    ///
    /// Never fails: an absent section ⇒ the defaults (`department`, empty
    /// whitelist); malformed values are dropped individually and reported in
    /// `warnings` (zh-TW, operator-facing) rather than failing the whole read —
    /// the dashboard must still be able to show and repair a bad config.
    pub(crate) fn parse_delegation_section(
        table: &toml::Table,
    ) -> (String, Vec<(String, String)>, Vec<String>) {
        use duduclaw_core::delegation_policy::DelegationPolicy;

        let mut warnings: Vec<String> = Vec::new();
        let section = table.get("delegation").and_then(|v| v.as_table());
        let Some(section) = section else {
            if table.get("delegation").is_some() {
                warnings.push(
                    "config.toml 的 [delegation] 區段格式有誤(不是一個設定區段),已改用預設值。"
                        .to_string(),
                );
            }
            return (
                DelegationPolicy::default().as_str().to_string(),
                Vec::new(),
                warnings,
            );
        };

        // policy — unknown value falls back to the (stricter) default + warning.
        let raw_policy = section
            .get("policy")
            .and_then(|v| v.as_str())
            .unwrap_or(DelegationPolicy::default().as_str());
        let (policy, policy_warning) = DelegationPolicy::from_config_value(raw_policy);
        if let Some(w) = policy_warning {
            warnings.push(w);
        }

        // allow — every item must be exactly two non-empty strings. Bad items
        // are ignored individually (spec §2.2) so one typo cannot disable the
        // whole whitelist.
        let mut allow: Vec<(String, String)> = Vec::new();
        match section.get("allow") {
            None => {}
            Some(toml::Value::Array(items)) => {
                let mut seen: std::collections::HashSet<(String, String)> =
                    std::collections::HashSet::new();
                for (idx, item) in items.iter().enumerate() {
                    let pair = item.as_array().filter(|a| a.len() == 2).and_then(|a| {
                        let x = a[0].as_str().map(str::trim).filter(|s| !s.is_empty())?;
                        let y = a[1].as_str().map(str::trim).filter(|s| !s.is_empty())?;
                        Some((x.to_string(), y.to_string()))
                    });
                    match pair {
                        None => warnings.push(format!(
                            "config.toml [delegation] allow 第 {} 組設定格式有誤(每組必須剛好兩位 AI 員工),已忽略。",
                            idx + 1
                        )),
                        Some((x, y)) if x == y => warnings.push(format!(
                            "config.toml [delegation] allow 第 {} 組是同一位 AI 員工({x}),已忽略。",
                            idx + 1
                        )),
                        Some((x, y)) => {
                            let key = Self::delegation_pair_key(&x, &y);
                            if seen.insert(key) {
                                allow.push((x, y));
                            }
                        }
                    }
                }
            }
            Some(_) => warnings
                .push("config.toml [delegation] allow 必須是配對清單,格式有誤已忽略。".to_string()),
        }

        (policy.as_str().to_string(), allow, warnings)
    }

    /// `delegation.get` — current policy + whitelist + any config warnings.
    pub(crate) async fn handle_delegation_get(&self) -> WsFrame {
        let table = self
            .read_config_table(&self.home_dir.join("config.toml"))
            .await;
        let (policy, allow, warnings) = Self::parse_delegation_section(&table);
        WsFrame::ok_response(
            "",
            json!({
                "policy": policy,
                "allow": allow.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
                "warnings": warnings,
            }),
        )
    }

    /// `delegation.set` — validate and persist `{ policy?, allow? }`.
    ///
    /// Fail-closed: an unknown policy, a malformed pair, or a pair naming an
    /// agent that does not exist rejects the WHOLE payload (nothing is written)
    /// — a half-applied permission boundary is worse than none. Self-pairs and
    /// duplicates are cleaned rather than rejected, because they carry no
    /// meaning either way.
    pub(crate) async fn handle_delegation_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        use duduclaw_core::delegation_policy::DelegationPolicy;

        /// Guardrail on config size — a whitelist this long is a policy smell,
        /// not a legitimate setup, and keeps the config file reviewable.
        const MAX_ALLOW_PAIRS: usize = 200;

        let policy_param = params.get("policy");
        let allow_param = params.get("allow");
        if policy_param.is_none() && allow_param.is_none() {
            return WsFrame::error_response(
                "",
                "沒有要更新的項目。請提供 policy 或 allow(跨部門協作配對)。",
            );
        }

        // ── policy ──
        let new_policy = match policy_param {
            None => None,
            Some(v) => {
                let raw = match v.as_str() {
                    Some(s) => s,
                    None => {
                        return WsFrame::error_response("", "policy 必須是文字。");
                    }
                };
                match DelegationPolicy::parse(raw) {
                    Some(p) => Some(p),
                    None => {
                        return WsFrame::error_response(
                            "",
                            &format!(
                                "無法辨識的委派模式「{}」。可用值:department(部門)/ hierarchy(階層)/ open(開放)。",
                                duduclaw_core::truncate_chars(raw.trim(), 40)
                            ),
                        );
                    }
                }
            }
        };

        // ── allow ──
        let new_allow: Option<Vec<(String, String)>> = match allow_param {
            None => None,
            Some(v) => {
                let items = match v.as_array() {
                    Some(a) => a,
                    None => {
                        return WsFrame::error_response("", "allow 必須是配對清單。");
                    }
                };
                if items.len() > MAX_ALLOW_PAIRS {
                    return WsFrame::error_response(
                        "",
                        &format!("跨部門協作配對最多 {MAX_ALLOW_PAIRS} 組。"),
                    );
                }
                let mut raw_pairs: Vec<(String, String)> = Vec::new();
                for (idx, item) in items.iter().enumerate() {
                    let arr = match item.as_array() {
                        Some(a) if a.len() == 2 => a,
                        _ => {
                            return WsFrame::error_response(
                                "",
                                &format!(
                                    "第 {} 組配對格式有誤:每組必須剛好選兩位 AI 員工。",
                                    idx + 1
                                ),
                            );
                        }
                    };
                    let a = arr[0].as_str().map(str::trim).unwrap_or("");
                    let b = arr[1].as_str().map(str::trim).unwrap_or("");
                    if a.is_empty() || b.is_empty() {
                        return WsFrame::error_response(
                            "",
                            &format!("第 {} 組配對還沒選滿兩位 AI 員工。", idx + 1),
                        );
                    }
                    raw_pairs.push((a.to_string(), b.to_string()));
                }

                // WP21 欠帳③ — namespace unification: `gate_bus_dispatch` /
                // `DispatchOrgView` / mcp.rs `org_snapshot` all key agents by
                // their **directory name** (bus tasks carry `sender_agent` /
                // `target` as directory names), while the registry's `get()` /
                // `list()` index by `[agent] name`. Those two need not match.
                // Validating (and persisting) whatever the caller typed against
                // `config.agent.name` alone would accept a value that then
                // never matches at enforcement time — a whitelist entry that
                // passes save but silently grants nothing. So: resolve each id
                // against the directory-name namespace first; if that misses,
                // resolve via `name → directory name` and normalize to the
                // directory name before it is ever written to config.toml.
                //
                // WP22 T4 — `[agent] name` is supposed to be unique, but
                // nothing enforced that before this WP (create-time checks are
                // new; existing installs can already have two directories
                // sharing a name). Note this deliberately does NOT read
                // through `self.registry`: `AgentRegistry::scan` already
                // collapses same-name directories into a single last-wins
                // entry keyed by name (see `duduclaw_agent::registry`), so by
                // the time `reg.list()` is observable the duplicate-directory
                // information needed to DETECT the collision is already gone
                // — only one of the two directories would ever be visible.
                // Scanning `<home>/agents/` directly here preserves both, so
                // a name that maps to more than one directory can be flagged
                // and rejected outright instead of silently resolved to
                // whichever directory the registry happened to keep.
                let (known_dirs, name_to_dir, ambiguous_names): (
                    std::collections::HashSet<String>,
                    std::collections::HashMap<String, String>,
                    std::collections::HashSet<String>,
                ) = {
                    let mut dirs = std::collections::HashSet::new();
                    let mut map: std::collections::HashMap<String, String> =
                        std::collections::HashMap::new();
                    let mut ambiguous = std::collections::HashSet::new();
                    if let Ok(entries) = std::fs::read_dir(self.home_dir.join("agents")) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if !path.is_dir() {
                                continue;
                            }
                            let dir_name = match path.file_name().and_then(|n| n.to_str()) {
                                Some(n) => n.to_string(),
                                None => continue,
                            };
                            dirs.insert(dir_name.clone());
                            // Shared typed parse point (R2 unification): an
                            // absent file, malformed TOML, an absent
                            // `[agent]` table and an absent/wrong-typed
                            // `name` all still skip this directory.
                            let Some(name) = duduclaw_core::agent_toml::load(&path)
                                .agent
                                .and_then(|a| a.name)
                            else {
                                continue;
                            };
                            let name = name.as_str();
                            if let Some(existing_dir) = map.get(name) {
                                if existing_dir != &dir_name {
                                    ambiguous.insert(name.to_string());
                                }
                            }
                            map.insert(name.to_string(), dir_name);
                        }
                    }
                    (dirs, map, ambiguous)
                };
                // Returns `(resolved directory name, is_ambiguous)`. Directory
                // names always take priority and are never ambiguous — the
                // caller can always disambiguate by typing the directory name
                // (design doc §5).
                let resolve_to_dir_name = |id: &str| -> (Option<String>, bool) {
                    if known_dirs.contains(id) {
                        (Some(id.to_string()), false)
                    } else if ambiguous_names.contains(id) {
                        (None, true)
                    } else {
                        (name_to_dir.get(id).cloned(), false)
                    }
                };

                // Every named agent must resolve — a whitelist pointing at a
                // typo'd id silently grants nothing and looks like it works.
                // An ambiguous name is rejected before "missing": it did
                // resolve to *something*, just not unambiguously, which is a
                // sharper problem than a plain typo.
                let mut missing: Vec<String> = Vec::new();
                let mut ambiguous_hits: Vec<String> = Vec::new();
                let mut normalized_pairs: Vec<(String, String)> = Vec::new();
                for (a, b) in &raw_pairs {
                    let (ra, ra_ambiguous) = resolve_to_dir_name(a);
                    let (rb, rb_ambiguous) = resolve_to_dir_name(b);
                    if ra_ambiguous {
                        if !ambiguous_hits.contains(a) {
                            ambiguous_hits.push(a.clone());
                        }
                    } else if ra.is_none() && !missing.contains(a) {
                        missing.push(a.clone());
                    }
                    if rb_ambiguous {
                        if !ambiguous_hits.contains(b) {
                            ambiguous_hits.push(b.clone());
                        }
                    } else if rb.is_none() && !missing.contains(b) {
                        missing.push(b.clone());
                    }
                    if let (Some(ra), Some(rb)) = (ra, rb) {
                        normalized_pairs.push((ra, rb));
                    }
                }
                if !ambiguous_hits.is_empty() {
                    return WsFrame::error_response(
                        "",
                        &format!(
                            "名稱 {} 對應多個 AI 員工,請改用目錄名指定。",
                            ambiguous_hits.join("、")
                        ),
                    );
                }
                if !missing.is_empty() {
                    return WsFrame::error_response(
                        "",
                        &format!(
                            "找不到這些 AI 員工:{}。請確認名稱後再儲存。",
                            missing.join("、")
                        ),
                    );
                }

                // Self-pairs mean nothing (self-delegation is denied by the
                // policy itself) — drop instead of rejecting the save. Dedup
                // runs on the *normalized* (directory-name) ids, since a name
                // and its own directory name could otherwise be entered as
                // "two different" agents in the same pair.
                let mut cleaned: Vec<(String, String)> = Vec::new();
                let mut seen: std::collections::HashSet<(String, String)> =
                    std::collections::HashSet::new();
                for (a, b) in normalized_pairs {
                    if a == b {
                        continue;
                    }
                    if seen.insert(Self::delegation_pair_key(&a, &b)) {
                        cleaned.push((a, b));
                    }
                }
                Some(cleaned)
            }
        };

        // ── persist ──
        let config_path = self.home_dir.join("config.toml");
        let mut table = self.read_config_table(&config_path).await;
        let (before_policy, before_allow, _) = Self::parse_delegation_section(&table);

        // An existing-but-non-table [delegation] is operator data we refuse to
        // silently destroy; report it instead of overwriting.
        if table
            .get("delegation")
            .is_some_and(|v| v.as_table().is_none())
        {
            return WsFrame::error_response(
                "",
                "config.toml 的 [delegation] 區段格式有誤,請先修正設定檔後再儲存。",
            );
        }
        let section = table
            .entry("delegation")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .expect("delegation section verified as a table above");

        if let Some(p) = new_policy {
            section.insert("policy".into(), toml::Value::String(p.as_str().to_string()));
        }
        if let Some(pairs) = &new_allow {
            section.insert(
                "allow".into(),
                toml::Value::Array(
                    pairs
                        .iter()
                        .map(|(a, b)| {
                            toml::Value::Array(vec![
                                toml::Value::String(a.clone()),
                                toml::Value::String(b.clone()),
                            ])
                        })
                        .collect(),
                ),
            );
        }

        // Atomic write: temp + rename (same discipline as system.update_config).
        let tmp_path = config_path.with_extension("toml.tmp");
        if let Err(e) = self.write_config_table(&tmp_path, &table).await {
            return WsFrame::error_response("", &format!("寫入設定失敗:{e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp_path, &config_path).await {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return WsFrame::error_response("", &format!("儲存設定失敗:{e}"));
        }

        let after_policy = new_policy
            .map(|p| p.as_str().to_string())
            .unwrap_or_else(|| before_policy.clone());
        let after_allow = new_allow.clone().unwrap_or_else(|| before_allow.clone());

        // Audit: who changed the org-wide delegation boundary, from what to what.
        let pairs_json = |pairs: &Vec<(String, String)>| -> Value {
            Value::Array(pairs.iter().map(|(a, b)| json!([a, b])).collect())
        };
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "delegation_config_changed",
                "dashboard",
                duduclaw_security::audit::Severity::Warning,
                json!({
                    "actor_user_id": ctx.user_id,
                    "actor_email": ctx.email,
                    "actor_role": format!("{:?}", ctx.role).to_lowercase(),
                    "before": { "policy": before_policy, "allow": pairs_json(&before_allow) },
                    "after": { "policy": after_policy, "allow": pairs_json(&after_allow) },
                }),
            ),
        );

        info!(
            actor = %ctx.email,
            policy = %after_policy,
            pairs = after_allow.len(),
            "delegation.set committed"
        );

        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "policy": after_policy,
                "allow": pairs_json(&after_allow),
                // Enforcement re-reads config.toml per decision — no restart.
                "applied": true,
            }),
        )
    }
}
