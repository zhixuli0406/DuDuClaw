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
    ) -> (String, Vec<(String, String)>, Vec<duduclaw_core::delegation_policy::DelegationWarning>) {
        let cfg = duduclaw_core::delegation_policy::delegation_config_from_table(table);
        let policy = cfg.policy();
        let (allow, _) = duduclaw_core::delegation_policy::resolve_allow_pairs(&cfg.allow);
        let warnings = duduclaw_core::delegation_policy::delegation_section_warnings(table);
        (policy.as_str().to_string(), allow, warnings)
    }

    /// `delegation.get` — current policy + whitelist + any config warnings.
    ///
    /// RM-137: a `config.toml` that exists but cannot be read or parsed is an
    /// error, not the defaults — the dashboard must not show (and then save)
    /// `department` + no pairs as if that were the setting. The gates enforce
    /// `none` meanwhile; `delegation.effective` reports it as `config_error`.
    pub(crate) async fn handle_delegation_get(&self) -> WsFrame {
        let raw = match super::config_commit::read_text_or_empty(&self.home_dir.join("config.toml")) {
            Ok(raw) => raw,
            Err(_) => return WsFrame::error_response("", "無法讀取委派設定，請修正設定後再試。"),
        };
        let hash = super::config_commit::content_hash(&raw);
        let table = match raw.parse::<toml::Table>() {
            Ok(table) => table,
            Err(_) => return WsFrame::error_response("", "無法讀取委派設定，請修正設定後再試。"),
        };
        let (policy, allow, mut warnings) = Self::parse_delegation_section(&table);
        let (overrides, _) = duduclaw_core::delegation_policy::delegation_config_from_table(&table).resolve_overrides();
        warnings.extend(duduclaw_core::delegation_policy::delegation_override_warnings(&table));
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
                "unrestricted_roles": overrides.unrestricted_roles,
                "allow_one_way": overrides.allow_one_way,
                "allow": allow.iter().map(|(a, b)| json!([a, b])).collect::<Vec<_>>(),
                "warnings": warnings,
                "hash": hash,
            }),
        )
    }

    /// `delegation.set` — validate and persist optional policy, pair and role keys.
    ///
    /// Fail-closed: an unknown policy, a malformed pair, or a pair naming an
    /// agent that does not exist rejects the WHOLE payload (nothing is written)
    /// — a half-applied permission boundary is worse than none. Legacy two-way
    /// self-pairs are dropped; directional self-pairs and cross-list duplicates
    /// reject the request. Omitted keys preserve their current values.
    pub(crate) async fn handle_delegation_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        use duduclaw_core::delegation_policy::DelegationPolicy;

        let expected_hash = match params.get("expected_hash").and_then(Value::as_str) {
            Some(hash) => hash.to_string(),
            None => return WsFrame::error_response("", "設定頁的資料已過期，請重新整理後再儲存。"),
        };
        let policy_param = params.get("policy");
        let allow_param = params.get("allow");
        if policy_param.is_none() && allow_param.is_none() && params.get("allow_one_way").is_none() && params.get("unrestricted_roles").is_none() {
            return WsFrame::error_response(
                "",
                "沒有要更新的項目。請提供委派模式、協作配對或角色設定。",
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
        let mut pair_updates = Vec::new();
        for key in ["allow", "allow_one_way"] {
            let new_pairs: Option<Vec<(String, String)>> = match params.get(key) {
                None => None,
                Some(v) => {
                    let items = match v.as_array() {
                        Some(a) => a,
                        None => {
                            return WsFrame::error_response("", "協作配對必須是清單。");
                        }
                    };
                    if let Err(e) = validate_pair_count(items.len()) {
                        return WsFrame::error_response("", e);
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

                    // Dedup uses normalized directory ids. Legacy two-way
                    // self-pairs are dropped; directional self-pairs are rejected.
                    let cleaned = match clean_delegation_pairs(normalized_pairs, key == "allow_one_way") {
                        Ok(pairs) => pairs,
                        Err(e) => return WsFrame::error_response("", e),
                    };
                    Some(cleaned)
                }
            };

            pair_updates.push(new_pairs);
        }
        let new_one_way = pair_updates.pop().unwrap();
        let new_allow = pair_updates.pop().unwrap();
        let new_roles = match validate_unrestricted_roles(params.get("unrestricted_roles")) {
            Ok(roles) => roles,
            Err(e) => return WsFrame::error_response("", e),
        };

        // ── persist ──
        let config_path = self.home_dir.join("config.toml");
        let original=match super::config_commit::read_text_or_empty(&config_path) {
            Ok(raw)=>raw,Err(e)=>return WsFrame::error_response("",&format!("寫入設定失敗:{e}")),
        };
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
        let (before_overrides, _) = duduclaw_core::delegation_policy::delegation_config_from_table(&table).resolve_overrides();
        let after_one_way = new_one_way.clone().unwrap_or_else(|| before_overrides.allow_one_way.clone());
        let after_allow_check = new_allow.as_ref().unwrap_or(&before_allow);
        if pairs_overlap(&after_one_way, after_allow_check) {
            return WsFrame::error_response("", "單向配對與雙向配對重複，請移除其中一組。");
        }

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

        write_optional_delegation_keys(section, new_roles.as_ref(), new_one_way.as_ref());

        // Check the final effective table, including a policy edit that re-enables
        // previously ignored role entries.
        let (after_overrides, _) = duduclaw_core::delegation_policy::delegation_config_from_table(&table).resolve_overrides();
        let after_roles = after_overrides.unrestricted_roles;
        let roles_added = match require_delegation_role_ack(&before_overrides.unrestricted_roles, &after_roles,
            params.get("acknowledge_risk").and_then(Value::as_bool) == Some(true)) {
            Ok(roles) => roles,
            Err(e) => return WsFrame::error_response("", e),
        };

        // Atomic write: temp + rename (same discipline as system.update_config).
        if let Err(e)=super::config_commit::commit_table_locked(&config_path,expected_hash,&table).await {
            if super::config_commit::is_hash_conflict(&config_path, &e) {
                return WsFrame::error_response("", "設定已被其他人變更，請重新整理後再儲存。");
            }
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
                delegation_audit_details(ctx,
                    json!({ "policy": before_policy, "allow": pairs_json(&before_allow), "unrestricted_roles": before_overrides.unrestricted_roles, "allow_one_way": before_overrides.allow_one_way }),
                    json!({ "policy": after_policy, "allow": pairs_json(&after_allow), "unrestricted_roles": after_roles, "allow_one_way": after_one_way }),
                    &roles_added),
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
                "unrestricted_roles": after_roles,
                "allow_one_way": after_one_way,
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
    let owner = crate::delegation_roles::GatewayOwnerRoles::new(
        home, crate::delegation_roles::Requester::background()).avatar_owner(&dir_name);
    // Name lookup is display-only, including when role overrides are disabled.
    let owner_display_name = owner.as_ref().ok().and_then(|owner| owner.as_ref())
        .and_then(|(id, _)| crate::decision_notify::open_user_db_read_only(home).ok().flatten()
            .and_then(|db| db.get_user(id).ok().flatten()).map(|user| user.display_name));
    let (owner_error, owner_role) = if read.overrides.unrestricted_roles.is_empty() {
        (false, None)
    } else {
        (owner.is_err(), owner.ok().flatten().map(|(_, role)| role))
    };
    let unrestricted = owner_role.as_ref().is_some_and(|r| read.overrides.unrestricted_roles.contains(r));
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
    let one_way: Vec<Value> = read.overrides.allow_one_way.iter()
        .filter(|(a, b)| names_this_agent(a) || names_this_agent(b))
        .map(|(a, b)| json!({ "from": shown(a), "to": shown(b),
            "direction": if names_this_agent(a) { "outgoing" } else { "incoming" } }))
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
        "unrestricted": unrestricted,
        "owner_role": owner_role,
        "owner_display_name": owner_display_name,
        "unrestricted_error": if owner_error { Some("users_unreadable") } else { None },
        "allow_one_way": if pairs_apply { one_way.clone() } else { Vec::new() },
        "inactive_allow_one_way": if pairs_apply { Vec::new() } else { one_way },
        // What applies to this employee: `none` when either the global
        // policy or its own switch stops it.
        "policy": policy.as_str(),
        "source": source,
        "global_policy": global.as_str(),
        "configured_policy": read.configured_policy,
        "policy_warning": read.classified_policy_warning,
        // `unreadable` | `unparsable` | `permissions_unreadable` | `unknown`
        // (null when there is no problem) — never config text.
        "config_error": config_error,
        // How many `[delegation] allow` entries were dropped as malformed —
        // the only allow-warning information a non-Admin viewer receives.
        "allow_warning_count": read.allow_warnings.len() + read.override_warnings.len(),
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
        payload["allow_warnings"] = json!(read.allow_warnings.iter().chain(&read.override_warnings).collect::<Vec<_>>());
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

/// Compare effective roles, so ignored legacy entries cannot bypass acknowledgement.
pub(super) fn require_delegation_role_ack(before: &[String], after: &[String], acknowledged: bool) -> Result<Vec<String>, &'static str> {
    let added: Vec<_> = after.iter().filter(|role| !before.contains(role)).cloned().collect();
    if !added.is_empty() && !acknowledged {
        return Err("新增不受委派範圍限制的角色前，請先確認風險。");
    }
    Ok(added)
}

pub(super) fn delegation_audit_details(ctx: &UserContext, before: Value, after: Value, roles_added: &[String]) -> Value {
    let mut details = json!({
        "actor_user_id": ctx.user_id,
        "actor_email": ctx.email,
        "actor_role": format!("{:?}", ctx.role).to_lowercase(),
        "before": before,
        "after": after,
    });
    if !roles_added.is_empty() {
        details["roles_added"] = json!(roles_added);
        details["acknowledged_by"] = json!(ctx.user_id);
        details["acknowledged_at"] = json!(chrono::Utc::now().to_rfc3339());
    }
    details
}

fn validate_unrestricted_roles(value: Option<&Value>) -> Result<Option<Vec<String>>, &'static str> {
    let Some(value) = value else { return Ok(None); };
    let items = value.as_array().ok_or("角色必須是清單。")?;
    let mut roles = Vec::new();
    for item in items {
        let role = item.as_str().filter(|r| duduclaw_core::delegation_policy::UNRESTRICTABLE_ROLE_NAMES.contains(r))
            .ok_or("不受委派範圍限制的角色只能是管理員或主管。");
        let role = role?;
        if !roles.iter().any(|r| r == role) { roles.push(role.to_string()); }
    }
    Ok(Some(roles))
}

fn write_optional_delegation_keys(section: &mut toml::Table, roles: Option<&Vec<String>>, pairs: Option<&Vec<(String, String)>>) {
    if let Some(roles) = roles {
        if roles.is_empty() { section.remove("unrestricted_roles"); }
        else { section.insert("unrestricted_roles".into(), toml::Value::Array(roles.iter().cloned().map(toml::Value::String).collect())); }
    }
    if let Some(pairs) = pairs {
        if pairs.is_empty() { section.remove("allow_one_way"); }
        else { section.insert("allow_one_way".into(), toml::Value::Array(pairs.iter().map(|(a,b)| toml::Value::Array(vec![toml::Value::String(a.clone()), toml::Value::String(b.clone())])).collect())); }
    }
}

fn validate_pair_count(count: usize) -> Result<(), &'static str> {
    if count > duduclaw_core::delegation_policy::MAX_ONE_WAY_PAIRS { Err("跨部門協作配對最多 200 組。") } else { Ok(()) }
}

fn clean_delegation_pairs(pairs: Vec<(String, String)>, directional: bool) -> Result<Vec<(String, String)>, &'static str> {
    let mut seen = std::collections::HashSet::new();
    let mut cleaned = Vec::new();
    for (a, b) in pairs {
        if a == b {
            if directional { return Err("單向配對必須是不同的 AI 員工。"); }
            continue;
        }
        let key = if directional { (a.clone(), b.clone()) } else { MethodHandler::delegation_pair_key(&a, &b) };
        if seen.insert(key) { cleaned.push((a, b)); }
    }
    Ok(cleaned)
}

fn pairs_overlap(one_way: &[(String, String)], two_way: &[(String, String)]) -> bool {
    one_way.iter().any(|(a, b)| two_way.iter().any(|(x, y)| MethodHandler::delegation_pair_key(a, b) == MethodHandler::delegation_pair_key(x, y)))
}

#[cfg(test)]
mod rm183_tests {
    use super::*;

    async fn set_with_hash(handler: &MethodHandler, mut params: Value, ctx: &UserContext) -> WsFrame {
        let raw = super::super::config_commit::read_text_or_empty(&handler.home_dir.join("config.toml")).unwrap();
        params["expected_hash"] = json!(super::super::config_commit::content_hash(&raw));
        handler.handle_delegation_set(params, ctx).await
    }

    #[tokio::test]
    async fn delegation_role_additions_require_ack_and_record_server_evidence() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, "[delegation]\npolicy='none'\n").unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let ctx = UserContext::admin_fallback();
        for params in [json!({"unrestricted_roles": ["employee"], "acknowledge_risk": true}),
            json!({"unrestricted_roles": ["admin"]}),
            json!({"unrestricted_roles": ["admin"], "acknowledge_risk": "true"})] {
            let response = set_with_hash(&handler, params, &ctx).await;
            assert!(matches!(response, WsFrame::Response { ok: false, .. }), "{response:?}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "[delegation]\npolicy='none'\n");
        }
        let response = set_with_hash(&handler, json!({"unrestricted_roles": ["admin"],
            "acknowledge_risk": true, "acknowledged_by": "forged", "acknowledged_at": "forged"}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: true, .. }), "{response:?}");
        let rows = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        let event: Value = rows.lines().map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|v| v["event_type"] == "delegation_config_changed").unwrap();
        assert_eq!(event["details"]["roles_added"], json!(["admin"]));
        assert_eq!(event["details"]["acknowledged_by"], ctx.user_id);
        assert!(chrono::DateTime::parse_from_rfc3339(event["details"]["acknowledged_at"].as_str().unwrap()).is_ok());
        let response = set_with_hash(&handler, json!({"unrestricted_roles": []}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: true, .. }), "{response:?}");
        let rows = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        let event: Value = rows.lines().map(|l| serde_json::from_str::<Value>(l).unwrap())
            .filter(|v| v["event_type"] == "delegation_config_changed").last().unwrap();
        assert!(event["details"].get("acknowledged_by").is_none());
    }

    #[tokio::test]
    async fn restoring_valid_policy_requires_ack_for_legacy_roles() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let original = "[delegation]\npolicy='typo'\nunrestricted_roles=['admin']\n";
        std::fs::write(&path, original).unwrap();
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        let ctx = UserContext::admin_fallback();
        let response = set_with_hash(&handler, json!({"policy": "none"}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: false, .. }), "{response:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        let response = set_with_hash(&handler, json!({"policy": "none", "acknowledge_risk": true}), &ctx).await;
        assert!(matches!(response, WsFrame::Response { ok: true, .. }), "{response:?}");
    }

    fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn rejects_invalid_roles_and_deduplicates_exact_roles() {
        for value in [json!(["root"]), json!(["Admin"]), json!([" admin "]), json!([1]), json!(null)] {
            assert!(validate_unrestricted_roles(Some(&value)).is_err());
        }
        assert_eq!(validate_unrestricted_roles(Some(&json!(["admin", "admin", "manager"]))).unwrap(), Some(vec!["admin".to_string(), "manager".to_string()]));
        assert!(validate_unrestricted_roles(Some(&json!(["employee"]))).is_err());
        assert_eq!(validate_unrestricted_roles(None).unwrap(), None);
        assert_eq!(validate_unrestricted_roles(Some(&json!(["root"]))).unwrap_err(), "不受委派範圍限制的角色只能是管理員或主管。");
    }

    #[test]
    fn ordered_dedupe_preserves_reverse_pair_and_rejects_self_pair() {
        assert_eq!(clean_delegation_pairs(pairs(&[("a", "b"), ("a", "b"), ("b", "a")]), true).unwrap(), pairs(&[("a", "b"), ("b", "a")]));
        assert!(clean_delegation_pairs(pairs(&[("a", "a")]), true).is_err());
        assert!(pairs_overlap(&pairs(&[("b", "a")]), &pairs(&[("a", "b")])));
        assert!(!pairs_overlap(&pairs(&[("c", "a")]), &pairs(&[("a", "b")])));
        assert!(validate_pair_count(200).is_ok());
        assert!(validate_pair_count(201).is_err());
    }

    #[test]
    fn omitted_keys_preserve_values_and_empty_arrays_remove_them() {
        let mut section: toml::Table = "unrestricted_roles=['admin']\nallow_one_way=[['a','b']]".parse().unwrap();
        let before = section.clone();
        write_optional_delegation_keys(&mut section, None, None);
        assert_eq!(section, before);
        write_optional_delegation_keys(&mut section, Some(&vec![]), Some(&vec![]));
        assert!(!section.contains_key("unrestricted_roles"));
        assert!(!section.contains_key("allow_one_way"));
    }

    #[test]
    fn effective_without_unrestricted_roles_does_not_require_users_db() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/a");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='a'\n[permissions]\ncan_send_cross_agent=true").unwrap();
        for roles in ["", "\nunrestricted_roles=[]", "\nunrestricted_roles=['employee']"] {
            std::fs::write(home.path().join("config.toml"), format!("[delegation]\npolicy='department'{roles}")).unwrap();
            let value = effective_delegation_json(home.path(), &agent, "a", &|_| true, true);
            assert_eq!(value["unrestricted"], false);
            assert!(value["owner_role"].is_null());
            assert!(value["unrestricted_error"].is_null());
            assert!(value["config_error"].is_null());
            assert!(value["config_error_detail"].is_null());
            assert!(!home.path().join("users.db").exists());
        }
    }

    #[test]
    fn effective_masks_directional_peers_and_reports_unreadable_users() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/a");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='a'\n[permissions]\ncan_send_cross_agent=true").unwrap();
        std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy='department'\nunrestricted_roles=['admin']\nallow_one_way=[['a','hidden'],['visible','a']]").unwrap();
        let value = effective_delegation_json(home.path(), &agent, "a", &|p| p == "visible", false);
        assert_eq!(value["allow_one_way"][0]["direction"], "outgoing");
        assert_eq!(value["allow_one_way"][0]["to"], HIDDEN_PEER_PLACEHOLDER);
        assert_eq!(value["allow_one_way"][1]["direction"], "incoming");
        assert_eq!(value["allow_one_way"][1]["from"], "visible");
        assert_eq!(value["unrestricted"], false);
        assert_eq!(value["unrestricted_error"], "users_unreadable");
        assert!(value["owner_role"].is_null());
        assert!(value["config_error"].is_null());
        assert!(value.get("config_error_detail").is_none());
        assert!(!home.path().join("users.db").exists());
    }
}

#[cfg(test)]
mod rm183_owner_tests {
    use super::*;

    #[test]
    fn effective_role_override_under_none_requires_unique_active_avatar_owner() {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents/a");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("agent.toml"), "[agent]\nname='a'").unwrap();
        std::fs::write(home.path().join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['admin']\nallow_one_way=[['a','b']]").unwrap();
        let db = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        db.execute_batch("CREATE TABLE users (id TEXT, role TEXT, status TEXT, avatar_agent_id TEXT); INSERT INTO users VALUES ('owner', 'admin', 'active', 'a');").unwrap();
        let value = effective_delegation_json(home.path(), &agent, "a", &|_| false, false);
        assert_eq!(value["unrestricted"], true);
        assert_eq!(value["owner_role"], "admin");
        assert_eq!(value["allow_one_way"], json!([]));
        assert_eq!(value["inactive_allow_one_way"][0]["to"], HIDDEN_PEER_PLACEHOLDER);
        db.execute("UPDATE users SET status='disabled'", []).unwrap();
        assert_eq!(effective_delegation_json(home.path(), &agent, "a", &|_| false, false)["unrestricted"], false);
        db.execute_batch("UPDATE users SET status='active'; INSERT INTO users VALUES ('other', 'admin', 'active', 'a');").unwrap();
        assert_eq!(effective_delegation_json(home.path(), &agent, "a", &|_| false, false)["unrestricted"], false);
    }
}
