// Data model + pure parsing for `create_agent.rs` (WP-S6b2-N, "新增員工") —
// split out purely to keep `create_agent.rs` under this crate's own
// file-size convention, same reason `agents_data.rs`/`goals_data.rs` are
// split from their own page modules (see those files' own header comments
// for the established precedent). No behavior differs from an unsplit
// version.
//
// ── RPC shapes (read directly from `crates/duduclaw-gateway/src/
// handlers.rs`, never guessed — see `create_agent.rs`'s own header comment
// for the full file:line citations, repeated here only where a specific
// field mapping needs justifying) ────────────────────────────────────────
//   `templates.roster` → `handle_templates_roster` (handlers.rs:8017) →
//   `{"industry": string|null, "label": string|null, "roles": [{"role_id",
//   "kind","kit"?,"name","display_name","summary","created":bool,
//   "overlay_count"}], "humans":[...], "excluded":[...]}` — this page reads
//   `industry`/`label`/`roles[].{role_id,kind,name,display_name,summary,
//   created}` only (no `kit`/`overlay_count`/`humans`/`excluded` — none of
//   those have a rendered slot on the approved canvas). TS mirror
//   `TemplateRoster`/`TemplateRoleSummary`, `web/src/lib/api.ts:4066`/`4047`.
//   `templates.role` → `handle_templates_role` (handlers.rs:8070) →
//   `{"role_id","kind","name","display_name","trigger","reports_to",
//   "summary","soul_md","contract_toml","agent_toml","has_extras"}` — TS
//   mirror `TemplateRoleDetail`, api.ts:4079.
//   `departments.list` → `handle_departments_list` (handlers.rs:26379) →
//   `{"departments": [{"name","agent_count","members","wiki_pages",
//   "skills"}]}` — this page reads `.name` only. TS mirror `DepartmentInfo`,
//   api.ts:4136.

use serde_json::Value;

// ── templates.roster ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateRoleSummary {
    pub role_id: String,
    /// `"ceo" | "front_desk" | "worker"` in practice — rendered raw (no
    /// label lookup needed, the canvas never shows the kind token itself,
    /// only uses it for sort order, matching `web/src/pages/agent-form/
    /// defaults.ts::TEMPLATE_KIND_ORDER`).
    pub kind: String,
    pub name: String,
    pub display_name: String,
    pub summary: String,
    /// An agent has already been created from this role — renders the
    /// canvas's dimmed "已建立" disabled card.
    pub created: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TemplateRosterData {
    pub industry: Option<String>,
    pub label: Option<String>,
    pub roles: Vec<TemplateRoleSummary>,
}

/// `None` only when the payload carries no `roles` array at all — an
/// honest "this endpoint is unavailable/foreign", not a finer parse-error
/// distinction the caller needs. `templates.roster` requires an unlocked
/// premium template dir server-side and fails outright on a non-premium
/// install; `create_agent.rs`'s fetch orchestration folds that RPC failure
/// into the same `None` outcome, mirroring `CreateAgentPage.tsx`'s own
/// `.then((r) => setRoster(Array.isArray(r?.roles) ? r : null)).catch(() =>
/// {/* no templates ⇒ plain form */})` — a network/auth error and a
/// malformed-but-200 payload both degrade to the same silent fallback. An
/// empty `roles: []` array IS a valid `Some` (a premium install with an
/// industry staged but every role already created, say) — matches the web's
/// `Array.isArray` check exactly, which does not require the array be
/// non-empty.
pub fn parse_template_roster(payload: &Value) -> Option<TemplateRosterData> {
    let roles_raw = payload.get("roles")?.as_array()?;
    let roles = roles_raw
        .iter()
        .filter_map(|r| {
            let role_id = r.get("role_id")?.as_str()?.to_string();
            let kind = r.get("kind").and_then(Value::as_str).unwrap_or("").to_string();
            let name = r.get("name").and_then(Value::as_str).unwrap_or("").to_string();
            let display_name = r.get("display_name").and_then(Value::as_str).unwrap_or("").to_string();
            let summary = r.get("summary").and_then(Value::as_str).unwrap_or("").to_string();
            let created = r.get("created").and_then(Value::as_bool).unwrap_or(false);
            Some(TemplateRoleSummary { role_id, kind, name, display_name, summary, created })
        })
        .collect();
    let industry = payload.get("industry").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let label = payload.get("label").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    Some(TemplateRosterData { industry, label, roles })
}

/// Stable sort order for the template grid — `ceo` first, then
/// `front_desk`, then `worker`, then anything unrecognized last (a future
/// kind added server-side before this client catches up degrades to "last",
/// never panics or drops the card). Verbatim `TEMPLATE_KIND_ORDER` port
/// (`web/src/pages/agent-form/defaults.ts`).
pub fn kind_sort_key(kind: &str) -> u8 {
    match kind {
        "ceo" => 0,
        "front_desk" => 1,
        "worker" => 2,
        _ => 3,
    }
}

// ── templates.role ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TemplateRoleDetail {
    pub role_id: String,
    pub name: String,
    pub display_name: String,
    pub trigger: String,
    pub reports_to: Option<String>,
    pub soul_md: String,
    pub contract_toml: String,
    pub agent_toml: String,
}

pub fn parse_template_role_detail(payload: &Value) -> Option<TemplateRoleDetail> {
    let role_id = payload.get("role_id")?.as_str()?.to_string();
    let name = payload.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let display_name = payload.get("display_name").and_then(Value::as_str).unwrap_or("").to_string();
    let trigger = payload.get("trigger").and_then(Value::as_str).unwrap_or("").to_string();
    let reports_to = payload.get("reports_to").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string);
    let soul_md = payload.get("soul_md").and_then(Value::as_str).unwrap_or("").to_string();
    let contract_toml = payload.get("contract_toml").and_then(Value::as_str).unwrap_or("").to_string();
    let agent_toml = payload.get("agent_toml").and_then(Value::as_str).unwrap_or("").to_string();
    Some(TemplateRoleDetail { role_id, name, display_name, trigger, reports_to, soul_md, contract_toml, agent_toml })
}

// ── departments.list ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepartmentItem {
    pub name: String,
}

pub fn parse_departments(payload: &Value) -> Vec<DepartmentItem> {
    payload
        .get("departments")
        .and_then(Value::as_array)
        .map(|arr| arr.iter().filter_map(|d| Some(DepartmentItem { name: d.get("name")?.as_str()?.to_string() })).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_template_roster_reads_the_handle_templates_roster_shape() {
        let v = json!({
            "industry": "sales",
            "label": "業務團隊",
            "roles": [
                { "role_id": "ceo", "kind": "ceo", "name": "ceo", "display_name": "CEO", "summary": "s", "created": false },
                { "role_id": "biz-dev", "kind": "worker", "name": "biz-dev", "display_name": "業務開發專員", "summary": "開發名單", "created": true },
            ],
            "humans": [],
            "excluded": [],
        });
        let roster = parse_template_roster(&v).unwrap();
        assert_eq!(roster.industry.as_deref(), Some("sales"));
        assert_eq!(roster.label.as_deref(), Some("業務團隊"));
        assert_eq!(roster.roles.len(), 2);
        assert!(!roster.roles[0].created);
        assert!(roster.roles[1].created);
    }

    #[test]
    fn parse_template_roster_empty_roles_array_is_some_not_none() {
        let v = json!({ "industry": null, "label": null, "roles": [] });
        let roster = parse_template_roster(&v).unwrap();
        assert!(roster.roles.is_empty());
        assert_eq!(roster.industry, None);
    }

    #[test]
    fn parse_template_roster_missing_roles_array_is_none_not_panicking() {
        assert!(parse_template_roster(&json!(null)).is_none());
        assert!(parse_template_roster(&json!({"foo": "bar"})).is_none());
        assert!(parse_template_roster(&json!({"roles": "not-an-array"})).is_none());
    }

    #[test]
    fn parse_template_roster_role_missing_role_id_is_dropped_not_panicking() {
        let v = json!({ "roles": [{"name": "no id"}] });
        assert!(parse_template_roster(&v).unwrap().roles.is_empty());
    }

    #[test]
    fn kind_sort_key_orders_ceo_front_desk_worker_then_unknown_last() {
        assert!(kind_sort_key("ceo") < kind_sort_key("front_desk"));
        assert!(kind_sort_key("front_desk") < kind_sort_key("worker"));
        assert!(kind_sort_key("worker") < kind_sort_key("something-new"));
    }

    #[test]
    fn parse_template_role_detail_reads_the_handle_templates_role_shape() {
        let v = json!({
            "role_id": "biz-dev", "kind": "worker", "name": "biz-dev", "display_name": "業務開發專員",
            "trigger": "@業務開發", "reports_to": "biz-lead", "summary": "s",
            "soul_md": "# 業務開發專員", "contract_toml": "[must_not]", "agent_toml": "[agent]", "has_extras": true,
        });
        let d = parse_template_role_detail(&v).unwrap();
        assert_eq!(d.role_id, "biz-dev");
        assert_eq!(d.reports_to.as_deref(), Some("biz-lead"));
        assert_eq!(d.soul_md, "# 業務開發專員");
    }

    #[test]
    fn parse_template_role_detail_empty_reports_to_is_none_not_empty_string() {
        let v = json!({ "role_id": "ceo", "name": "ceo", "display_name": "CEO", "trigger": "", "reports_to": "", "soul_md": "" });
        let d = parse_template_role_detail(&v).unwrap();
        assert_eq!(d.reports_to, None);
    }

    #[test]
    fn parse_template_role_detail_missing_role_id_is_none_not_panicking() {
        assert!(parse_template_role_detail(&json!({"name": "no id"})).is_none());
        assert!(parse_template_role_detail(&json!(null)).is_none());
    }

    #[test]
    fn parse_departments_reads_the_handle_departments_list_shape() {
        let v = json!({ "departments": [
            { "name": "業務部", "agent_count": 2, "members": ["a", "b"], "wiki_pages": 3, "skills": 1 },
            { "name": "財務部", "agent_count": 0, "members": [], "wiki_pages": 0, "skills": 0 },
        ]});
        let items = parse_departments(&v);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "業務部");
        assert_eq!(items[1].name, "財務部");
    }

    #[test]
    fn parse_departments_malformed_payload_is_empty_not_panicking() {
        assert!(parse_departments(&json!(null)).is_empty());
        assert!(parse_departments(&json!({"departments": "nope"})).is_empty());
        assert!(parse_departments(&json!({"departments": [{"no_name": true}]})).is_empty());
    }
}
