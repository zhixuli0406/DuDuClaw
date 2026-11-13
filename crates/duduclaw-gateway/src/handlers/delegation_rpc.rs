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
    //   policy = "department"                        # department / hierarchy / open / none
    //   allow  = [["sales-lead", "warehouse-lead"]]  # unordered pair ⇒ two-way
    //
    // `delegation.get` / `delegation.set` are admin-gated in `dispatch` (same
    // gate as `runtime.install` / `system.update_config`), and every accepted
    // write is appended to the security audit log as
    // `delegation_config_changed` with before/after. `delegation.effective`
    // (RM-137) is the read-only per-employee view for the employee page
    // (Manager and up with view access to that employee).

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
    ///
    /// The returned policy is the one the gates enforce: since v1.82 (RM-137)
    /// an unrecognised value, a non-string `policy` and a non-table
    /// `[delegation]` all resolve to `none` (they used to show `department`).
    pub(crate) fn parse_delegation_section(
        table: &toml::Table,
    ) -> (String, Vec<(String, String)>, Vec<String>) {
        let mut warnings: Vec<String> = Vec::new();
        // The same resolution every gate uses (`delegation_config_from_table`).
        let (policy, policy_warning) =
            duduclaw_core::delegation_policy::delegation_config_from_table(table).resolve();
        let section = table.get("delegation").and_then(|v| v.as_table());
        let Some(section) = section else {
            if table.get("delegation").is_some() {
                warnings.push(
                    "config.toml 的 [delegation] 區段格式有誤(不是一個設定區段),已視為 \"none\"(不允許 AI 員工之間委派)。"
                        .to_string(),
                );
            }
            return (policy.as_str().to_string(), Vec::new(), warnings);
        };
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
    ///
    /// RM-137: a `config.toml` that exists but cannot be read or parsed is an
    /// error, not the defaults — the dashboard must not show (and then save)
    /// `department` + no pairs as if that were the setting. The gates enforce
    /// `none` meanwhile; `delegation.effective` reports it as `config_error`.
    pub(crate) async fn handle_delegation_get(&self) -> WsFrame {
        let table = match self
            .read_config_table_strict(&self.home_dir.join("config.toml"))
            .await
        {
            Ok(table) => table,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "無法讀取委派設定:{e}。在修正前 AI 員工之間一律不能委派;請先在原始設定編輯器或終端機修正 config.toml。"
                    ),
                );
            }
        };
        let (policy, allow, warnings) = Self::parse_delegation_section(&table);
        let configured_policy =
            duduclaw_core::delegation_policy::delegation_config_read_from_table(&table)
                .configured_policy;
        WsFrame::ok_response(
            "",
            json!({
                "policy": policy,
                // The value as written, so the page can show what it was
                // when it did not resolve (null when the key is absent).
                "configured_policy": configured_policy,
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
                                "無法辨識的委派模式「{}」。可用值:department(部門)/ hierarchy(階層)/ open(開放)/ none(不允許委派)。",
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
        let original=match super::config_commit::read_text_or_empty(&config_path) {
            Ok(raw)=>raw,Err(e)=>return WsFrame::error_response("",&format!("寫入設定失敗:{e}")),
        };
        let config_expected_hash=super::config_commit::content_hash(&original);
        // RM-137: an unparsable config.toml is refused, never replaced by a
        // default table (nothing is written; the commit path below would
        // refuse too, this says why before any audit row is built).
        let mut table = match original.parse::<toml::Table>() {
            Ok(table) => table,
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!(
                        "config.toml 不是有效的 TOML,委派設定沒有寫入:{}。請先在原始設定編輯器或終端機修正設定檔。",
                        duduclaw_core::truncate_chars(&e.to_string(), 200)
                    ),
                );
            }
        };
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
        if let Err(e)=super::config_commit::commit_table_locked(&config_path,config_expected_hash,&table).await {
            return WsFrame::error_response("",&format!("寫入設定失敗:{e}"));
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

    /// `delegation.effective { agent_id }` (RM-137) — what the gates apply to
    /// one employee: the effective policy, whether it comes from the global
    /// `[delegation] policy` or the employee's own `[permissions]
    /// can_send_cross_agent`, the allow pairs naming it, and `config_error`
    /// when config.toml or its agent.toml could not be read. Read-only.
    ///
    /// RM-137 L1: `viewer` is the LIVE viewer context (`users.db` re-read by
    /// `require_live_agent_access`). The other end of every allow pair is
    /// shown only when that viewer may view it (`has_agent_access`, Viewer —
    /// Personal employees of other people included in the restriction);
    /// otherwise it is replaced by [`HIDDEN_PEER_PLACEHOLDER`] and only the
    /// count of such pairs is kept. The same live context decides whether the
    /// raw `[delegation] allow` warnings may be returned (Admin only; see
    /// [`effective_delegation_json`]).
    pub(crate) async fn handle_delegation_effective(
        &self,
        agent_id: &str,
        viewer: &UserContext,
    ) -> WsFrame {
        let located = {
            let reg = self.registry.read().await;
            reg.get(agent_id)
                .map(|a| (a.dir.clone(), a.config.agent.name.clone()))
        };
        let Some((dir, name)) = located else {
            return WsFrame::error_response("", "找不到這位 AI 員工。");
        };
        let home = self.home_dir.clone();
        let viewer = viewer.clone();
        let viewer_is_admin = viewer.has_role(UserRole::Admin);
        match tokio::task::spawn_blocking(move || {
            effective_delegation_json(
                &home,
                &dir,
                &name,
                &|peer: &str| viewer.has_agent_access(peer, AccessLevel::Viewer),
                viewer_is_admin,
            )
        })
        .await
        {
            Ok(payload) => WsFrame::ok_response("", payload),
            Err(e) => WsFrame::error_response("", &format!("無法讀取委派設定:{e}")),
        }
    }
}

/// The `delegation.effective` payload for the employee whose directory is
/// `agent_dir` (`agent_name` = its `[agent] name`). Reads through the same
/// functions the gates use — `read_delegation_config` (C1/C2 predicate,
/// policy `none` on an unreadable config) and
/// `agent_toml::load_permissions_checked` (the MCP `[permissions]` gate) — so
/// the page shows what is enforced. Pure apart from the two file reads.
///
/// `peer_visible` decides, for the OTHER end of each allow pair naming this
/// employee, whether the viewer may see that id (RM-137 L1). A hidden end is
/// replaced by [`HIDDEN_PEER_PLACEHOLDER`]; `hidden_allow_pairs` /
/// `hidden_inactive_allow_pairs` carry how many pairs were masked.
///
/// RM-137 (re-review L): a dropped-`allow`-entry warning quotes the raw entry
/// (`resolve_allow_pairs`), which can name any employee — including one the
/// viewer cannot view and that has nothing to do with this employee. Those
/// texts are returned (`allow_warnings`) only when `viewer_is_admin`; every
/// viewer gets `allow_warning_count`. `delegation.get` (Admin-only) keeps the
/// full list.
///
/// Same rule for a config problem: `config_error` is only a category
/// (`unreadable` | `unparsable` | `permissions_unreadable` | `unknown`, or
/// null) for every viewer, because the detail can quote raw config text (the
/// TOML parse excerpt); the zh-TW detail is `config_error_detail`, Admin only.
pub(crate) fn effective_delegation_json(
    home: &std::path::Path,
    agent_dir: &std::path::Path,
    agent_name: &str,
    peer_visible: &dyn Fn(&str) -> bool,
    viewer_is_admin: bool,
) -> Value {
    use duduclaw_core::DelegationPolicy;

    let read = duduclaw_core::read_delegation_config(home);
    // Pairs are written as directory names (see `delegation.set`); accept the
    // `[agent] name` too, since the two need not match.
    let dir_name = agent_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(agent_name)
        .to_string();
    let (can_send, can_create, permissions_error) =
        match duduclaw_core::agent_toml::load_permissions_checked(agent_dir) {
            Ok(p) => (p.allows("can_send_cross_agent"), p.allows("can_create_agents"), None),
            // The gate refuses every permission-gated tool on this error.
            Err(e) => (false, false, Some(e)),
        };
    let global = read.rules.policy;
    let (policy, source) = if global == DelegationPolicy::Disabled {
        (DelegationPolicy::Disabled, "global")
    } else if !can_send {
        (DelegationPolicy::Disabled, "agent_permissions")
    } else {
        (global, "global")
    };
    let names_this_agent = |x: &str| x == dir_name || x == agent_name;
    // RM-137 L1: the end that is not this employee is shown only when the
    // viewer may view it; otherwise masked (no id leaves the server).
    let shown = |x: &str| -> Value {
        if names_this_agent(x) || peer_visible(x) {
            json!(x)
        } else {
            json!(HIDDEN_PEER_PLACEHOLDER)
        }
    };
    let mut hidden = 0usize;
    let naming: Vec<Value> = read
        .rules
        .allow_pairs
        .iter()
        .filter(|pair| names_this_agent(&pair.0) || names_this_agent(&pair.1))
        .map(|pair| {
            let entry = json!([shown(&pair.0), shown(&pair.1)]);
            if entry[0] == HIDDEN_PEER_PLACEHOLDER || entry[1] == HIDDEN_PEER_PLACEHOLDER {
                hidden += 1;
            }
            entry
        })
        .collect();
    let pairs_apply = policy.honors_allow_pairs();
    let (active_pairs, inactive_pairs) = if pairs_apply {
        (naming, Vec::new())
    } else {
        (Vec::new(), naming)
    };
    let (hidden_active, hidden_inactive) = if pairs_apply { (hidden, 0) } else { (0, hidden) };
    // RM-137 (re-review): the detail texts can quote raw config (the TOML
    // parse excerpt, the `[permissions]` reader's error), so every viewer gets
    // only the content-free category `config_error`; the text goes to an
    // Admin viewer as `config_error_detail`.
    let (config_error, config_error_detail) = match (&read.config_error, &permissions_error) {
        // `read.config_error_kind` is one of
        // `duduclaw_core::delegation_policy::CONFIG_ERROR_{UNREADABLE,UNPARSABLE}`.
        (Some(c), _) => (
            Some(read.config_error_kind.unwrap_or(CONFIG_ERROR_UNKNOWN)),
            Some(c.clone()),
        ),
        (None, Some(p)) => (
            Some(CONFIG_ERROR_PERMISSIONS_UNREADABLE),
            Some(format!(
                "這位 AI 員工的 agent.toml [permissions] 無法讀取({p}),跨員工與建立員工的工具一律拒絕。"
            )),
        ),
        (None, None) => (None, None),
    };
    let mut payload = json!({
        "agent_id": agent_name,
        // What applies to this employee: `none` when either the global
        // policy or its own switch stops it.
        "policy": policy.as_str(),
        "source": source,
        "global_policy": global.as_str(),
        "configured_policy": read.configured_policy,
        "policy_warning": read.policy_warning,
        // `unreadable` | `unparsable` | `permissions_unreadable` | `unknown`
        // (null when there is no problem) — never config text.
        "config_error": config_error,
        // How many `[delegation] allow` entries were dropped as malformed —
        // the only allow-warning information a non-Admin viewer receives.
        "allow_warning_count": read.allow_warnings.len(),
        "may_delegate": policy != DelegationPolicy::Disabled,
        // `create_agent` / `spawn_ephemeral`: the employee's own switch,
        // and never under a global `none`.
        "may_create_agents": can_create && global != DelegationPolicy::Disabled,
        "permissions": {
            "can_send_cross_agent": can_send,
            "can_create_agents": can_create,
        },
        // Pairs naming this employee that take part in decisions …
        "allow_pairs": active_pairs,
        // … and the ones that are written but do not apply under `policy`.
        "inactive_allow_pairs": inactive_pairs,
        // RM-137 L1: how many of the pairs above name an employee the viewer
        // cannot view (shown as `HIDDEN_PEER_PLACEHOLDER`).
        "hidden_allow_pairs": hidden_active,
        "hidden_inactive_allow_pairs": hidden_inactive,
        "hidden_peer_placeholder": HIDDEN_PEER_PLACEHOLDER,
    });
    if viewer_is_admin {
        // The raw texts quote the dropped entries verbatim — Admin only.
        payload["allow_warnings"] = json!(read.allow_warnings);
        // May quote raw config text (TOML parse excerpt) — Admin only.
        payload["config_error_detail"] = json!(config_error_detail);
    }
    payload
}

/// RM-137 L1: what `delegation.effective` puts in place of an allow-pair
/// peer the viewer may not view. Not a valid agent id (`is_valid_agent_id`
/// refuses `*`), so it can never collide with a real employee. The dashboard
/// renders it as "another employee you cannot view".
pub(crate) const HIDDEN_PEER_PLACEHOLDER: &str = "*hidden*";

/// `delegation.effective.config_error` categories beyond the two
/// `config.toml` ones (`duduclaw_core::delegation_policy::CONFIG_ERROR_*`):
/// this employee's `[permissions]` could not be read …
pub(crate) const CONFIG_ERROR_PERMISSIONS_UNREADABLE: &str = "permissions_unreadable";
/// … or a config error without a recorded category (not produced today;
/// keeps the field non-null — fail closed toward "there is a problem").
pub(crate) const CONFIG_ERROR_UNKNOWN: &str = "unknown";
