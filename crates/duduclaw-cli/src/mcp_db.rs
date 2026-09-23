//! WP-D (§13.7): agent-facing tool surface for read-only SQL data sources.
//!
//! Four tools — `db_sources` / `db_tables` / `db_select` / `db_query` — over
//! the [`duduclaw_db`] connector. They exist so a customer's PostgreSQL /
//! MySQL / SQLite rows reach the model **through DuDuClaw's own MCP choke
//! point**, where the RFC-23 redaction pipeline can see and de-identify them.
//! A customer-run external MCP server (the §13.6 proxy route) cannot offer
//! that guarantee without the proxy hop; this route is first-party end to end.
//!
//! ## Two independent authorizations, both required
//!
//! 1. **Which agent may touch a database at all** — `Scope::DbRead` plus a
//!    non-empty `agent.toml [capabilities] db_sources`, both enforced upstream
//!    in `mcp_dispatch.rs` before any function here runs.
//! 2. **Which source** — checked here, because this is the layer that sees the
//!    `source` argument. An agent granted `["crm"]` cannot reach `payroll`
//!    even though it passed gate 1.
//!
//! On top of that the source's own `allowed_tables` bounds `db_tables` /
//! `db_select`, and the connector is read-only by construction (see
//! [`duduclaw_db`]'s crate docs for the three layers).
//!
//! ## Why a pool is opened per call
//!
//! `duduclaw mcp-server` is a per-session stdio subprocess, so a cached pool
//! would mostly be built and torn down anyway; and not caching means a
//! rotated credential or an edited `[db_sources.…]` block takes effect on the
//! very next call rather than after a restart. The cost is one handshake per
//! tool call, which is the right trade for a tool an agent invokes a handful
//! of times per turn.

use std::path::Path;

use duduclaw_db::{
    DbError, DbSourceEntry, Filter, FilterOp, LoadedDbSources, SelectRequest, open_source,
};
use serde_json::{Value, json};

// ── Small local result helpers (per-submodule convention, see mcp_os_ops.rs)

fn db_text(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn db_json(value: &Value) -> Value {
    db_text(&serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()))
}

fn db_error(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
}

/// The agent's granted source names, read fresh from `agent.toml`.
///
/// Fail-closed: an invalid agent id, a missing file, or malformed TOML all
/// yield an empty list, which denies everything.
fn granted_sources(home_dir: &Path, agent_id: &str) -> Vec<String> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Vec::new();
    }
    let agent_dir = home_dir.join("agents").join(agent_id);
    duduclaw_core::agent_toml::load(&agent_dir)
        .capabilities
        .db_sources
}

/// Resolve `source` for this caller, or explain exactly why not.
///
/// Order matters: the grant is checked **before** the config is consulted, so
/// an ungranted agent learns nothing about which sources exist.
async fn resolve_granted(
    home_dir: &Path,
    agent_id: &str,
    source: &str,
) -> Result<DbSourceEntry, String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("缺少 source 參數（資料來源名稱）。先呼叫 db_sources 取得可用清單。".into());
    }
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Err("呼叫者身分無效，拒絕存取資料來源。".into());
    }
    let grants = granted_sources(home_dir, agent_id);
    // Exact, trimmed, ASCII-case-insensitive — never substring.
    if !grants
        .iter()
        .any(|g| g.trim().eq_ignore_ascii_case(source))
    {
        return Err(format!(
            "此代理沒有資料來源「{source}」的授權。已授權的來源：{}。如需存取，請在 agent.toml 的 [capabilities] db_sources 加入該名稱。",
            render_list(&grants)
        ));
    }

    let loaded = duduclaw_db::load_db_sources(home_dir).await;
    if let Some(entry) = loaded.get(source) {
        return Ok(entry.clone());
    }
    // Granted but not loadable: say which, and why if we know.
    if let Some(err) = loaded
        .errors
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(source))
    {
        return Err(format!(
            "資料來源「{source}」的設定有誤，無法使用：{}",
            err.message
        ));
    }
    Err(format!(
        "資料來源「{source}」尚未在 config.toml 的 [db_sources.{source}] 設定，請聯絡操作者。"
    ))
}

fn render_list(items: &[String]) -> String {
    if items.is_empty() {
        "（無）".to_string()
    } else {
        items.join("、")
    }
}

fn error_text(e: &DbError) -> String {
    e.to_string()
}

// ── db_sources ──────────────────────────────────────────────────────────────

/// List the sources this agent is granted **and** that actually load.
///
/// A granted-but-broken source is reported with its reason instead of being
/// silently omitted — "the tool says I have no CRM" and "the CRM config has a
/// typo" are very different problems and the agent should not have to guess.
pub async fn handle_db_sources(home_dir: &Path, agent_id: &str) -> Value {
    let grants = granted_sources(home_dir, agent_id);
    if grants.is_empty() {
        return db_error(
            "此代理沒有任何資料庫來源授權。請在 agent.toml 設定 [capabilities] db_sources = [\"<資料來源名稱>\"]。",
        );
    }
    let loaded: LoadedDbSources = duduclaw_db::load_db_sources(home_dir).await;
    let mut out = Vec::new();
    let mut problems = Vec::new();
    for name in &grants {
        match loaded.get(name) {
            Some(entry) => out.push(json!({
                "name": entry.name,
                "label": entry.label,
                "driver": entry.driver.as_str(),
            })),
            None => {
                let reason = loaded
                    .errors
                    .iter()
                    .find(|e| e.name.eq_ignore_ascii_case(name.trim()))
                    .map(|e| e.message.clone())
                    .unwrap_or_else(|| "尚未在 config.toml 設定".to_string());
                problems.push(format!("{name}：{reason}"));
            }
        }
    }
    let mut text = serde_json::to_string_pretty(&Value::Array(out)).unwrap_or_default();
    if !problems.is_empty() {
        text.push_str(&format!(
            "\n\n// 以下已授權但無法使用：\n// {}",
            problems.join("\n// ")
        ));
    }
    db_text(&text)
}

// ── db_tables ───────────────────────────────────────────────────────────────

pub async fn handle_db_tables(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let source = arguments.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let entry = match resolve_granted(home_dir, agent_id, source).await {
        Ok(e) => e,
        Err(msg) => return db_error(&msg),
    };
    let src = match open_source(&entry, home_dir).await {
        Ok(s) => s,
        Err(e) => return db_error(&error_text(&e)),
    };
    let result = src.list_tables().await;
    src.close().await;
    match result {
        Ok(tables) => {
            let payload = json!({
                "tables": tables
                    .iter()
                    .map(|t| json!({
                        "name": t.name,
                        "columns": t.columns
                            .iter()
                            .map(|c| json!({ "name": c.name, "type": c.data_type }))
                            .collect::<Vec<_>>(),
                    }))
                    .collect::<Vec<_>>(),
            });
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                agent_id,
                "db_tables",
                &format!("source={}", entry.name),
                true,
                &[("db_source", json!(entry.name)), ("tables", json!(tables.len()))],
            );
            db_json(&payload)
        }
        Err(e) => db_error(&error_text(&e)),
    }
}

// ── db_select ───────────────────────────────────────────────────────────────

pub async fn handle_db_select(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let source = arguments.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let entry = match resolve_granted(home_dir, agent_id, source).await {
        Ok(e) => e,
        Err(msg) => return db_error(&msg),
    };
    let table = arguments
        .get("table")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    if table.is_empty() {
        return db_error("缺少 table 參數（資料表名稱）。先呼叫 db_tables 取得可用清單。");
    }
    let columns = match parse_string_array(arguments.get("columns")) {
        Ok(c) => c,
        Err(msg) => return db_error(&msg),
    };
    let filter = match parse_filters(arguments.get("filter")) {
        Ok(f) => f,
        Err(msg) => return db_error(&msg),
    };
    let order_by = arguments
        .get("order_by")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let limit = parse_limit(arguments.get("limit"));

    let req = SelectRequest {
        table: table.clone(),
        columns,
        filter,
        order_by,
        limit,
    };
    let src = match open_source(&entry, home_dir).await {
        Ok(s) => s,
        Err(e) => return db_error(&error_text(&e)),
    };
    let result = src.select(&req).await;
    src.close().await;
    // The audit row records the shape of the read, never the filter values —
    // those are customer data and `tool_calls.jsonl` is not where it belongs.
    let summary = format!(
        "source={} table={table} filters={} limit={}",
        entry.name,
        req.filter.len(),
        req.limit.map(|v| v.to_string()).unwrap_or_else(|| "default".into())
    );
    match result {
        Ok(out) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                agent_id,
                "db_select",
                &summary,
                true,
                &[
                    ("db_source", json!(entry.name)),
                    ("db_table", json!(table)),
                    ("row_count", json!(out.row_count)),
                    ("truncated", json!(out.truncated)),
                ],
            );
            db_json(&query_result_json(&out))
        }
        Err(e) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                agent_id,
                "db_select",
                &summary,
                false,
                &[("db_source", json!(entry.name)), ("db_table", json!(table))],
            );
            db_error(&error_text(&e))
        }
    }
}

// ── db_query ────────────────────────────────────────────────────────────────

pub async fn handle_db_query(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let source = arguments.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let entry = match resolve_granted(home_dir, agent_id, source).await {
        Ok(e) => e,
        Err(msg) => return db_error(&msg),
    };
    // Fail-closed BEFORE a connection is opened: a source with a real table
    // allowlist cannot have that allowlist enforced against arbitrary SQL, so
    // free SQL is refused at the source level rather than allowed to read past
    // it. Decided per call, which is why `db_query` still appears in
    // `tools/list` — one granted source may permit it and another may not.
    if !entry.allows_all_tables() {
        return db_error(
            &DbError::FreeSqlNotAllowed {
                source_name: entry.name.clone(),
            }
            .to_string(),
        );
    }
    let sql = arguments.get("sql").and_then(|v| v.as_str()).unwrap_or("");
    if sql.trim().is_empty() {
        return db_error("缺少 sql 參數（唯讀查詢語句，必須以 SELECT 或 WITH 開頭）。");
    }
    let limit = parse_limit(arguments.get("limit"));

    let src = match open_source(&entry, home_dir).await {
        Ok(s) => s,
        Err(e) => return db_error(&error_text(&e)),
    };
    let result = src.query(sql, limit).await;
    src.close().await;
    // Statement length, not the statement: a WHERE clause can carry the very
    // personal data this pipeline exists to keep out of durable logs.
    let summary = format!("source={} sql_chars={}", entry.name, sql.chars().count());
    match result {
        Ok(out) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                agent_id,
                "db_query",
                &summary,
                true,
                &[
                    ("db_source", json!(entry.name)),
                    ("row_count", json!(out.row_count)),
                    ("truncated", json!(out.truncated)),
                ],
            );
            db_json(&query_result_json(&out))
        }
        Err(e) => {
            duduclaw_security::audit::append_tool_call_with_extras(
                home_dir,
                agent_id,
                "db_query",
                &summary,
                false,
                &[("db_source", json!(entry.name))],
            );
            db_error(&error_text(&e))
        }
    }
}

// ── Argument parsing ────────────────────────────────────────────────────────

fn query_result_json(out: &duduclaw_db::QueryResult) -> Value {
    json!({
        "rows": out.rows,
        "row_count": out.row_count,
        "truncated": out.truncated,
    })
}

/// The MCP `inputSchema` builder in `mcp.rs` declares every parameter as
/// `"type": "string"`, so a well-behaved model may well hand back
/// `"[\"id\",\"name\"]"` instead of a real array. Parse a JSON string into
/// its value before type-checking, rather than refusing something the schema
/// we published actually asked for.
fn as_structured(value: &Value) -> std::borrow::Cow<'_, Value> {
    match value.as_str() {
        Some(s) => match serde_json::from_str::<Value>(s.trim()) {
            Ok(parsed) => std::borrow::Cow::Owned(parsed),
            Err(_) => std::borrow::Cow::Borrowed(value),
        },
        None => std::borrow::Cow::Borrowed(value),
    }
}

/// `limit` may arrive as a number or as its decimal string form.
fn parse_limit(value: Option<&Value>) -> Option<usize> {
    let v = value?;
    if let Some(n) = v.as_u64() {
        return Some(n as usize);
    }
    v.as_str()?.trim().parse::<usize>().ok()
}

fn parse_string_array(value: Option<&Value>) -> Result<Vec<String>, String> {
    let Some(v) = value else {
        return Ok(Vec::new());
    };
    if v.is_null() {
        return Ok(Vec::new());
    }
    let v = as_structured(v);
    let v = v.as_ref();
    if v.is_null() {
        return Ok(Vec::new());
    }
    let arr = v
        .as_array()
        .ok_or_else(|| "columns 必須是字串陣列".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| "columns 只能包含字串".to_string())?;
        out.push(s.trim().to_string());
    }
    Ok(out)
}

fn parse_filters(value: Option<&Value>) -> Result<Vec<Filter>, String> {
    let Some(v) = value else {
        return Ok(Vec::new());
    };
    if v.is_null() {
        return Ok(Vec::new());
    }
    let v = as_structured(v);
    let v = v.as_ref();
    if v.is_null() {
        return Ok(Vec::new());
    }
    let arr = v
        .as_array()
        .ok_or_else(|| "filter 必須是陣列，例如 [{\"column\":\"name\",\"op\":\"=\",\"value\":\"王小明\"}]".to_string())?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let obj = item
            .as_object()
            .ok_or_else(|| "filter 的每一項都必須是物件".to_string())?;
        let column = obj
            .get("column")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| "filter 的每一項都需要 column".to_string())?
            .to_string();
        let op_raw = obj
            .get("op")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("=");
        let op = FilterOp::parse(op_raw).ok_or_else(|| {
            format!("filter 的運算子「{op_raw}」不支援，只接受 = != < <= > >= like in")
        })?;
        let value = obj.get("value").cloned().unwrap_or(Value::Null);
        out.push(Filter { column, op, value });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The HTTP / SSE MCP transports hand these futures to axum, which
    /// requires `Send`. A non-`Send` value held across an await inside any
    /// handler below shows up there as an inscrutable `Handler` trait error
    /// three files away — this assertion fails at the source instead.
    #[test]
    fn handler_futures_are_send() {
        fn assert_send<T: Send>(_: T) {}
        let dir = std::path::Path::new("/tmp");
        let args = json!({});
        assert_send(handle_db_sources(dir, "a"));
        assert_send(handle_db_tables(&args, dir, "a"));
        assert_send(handle_db_select(&args, dir, "a"));
        assert_send(handle_db_query(&args, dir, "a"));
    }

    fn text_of(v: &Value) -> String {
        v["content"][0]["text"].as_str().unwrap_or("").to_string()
    }

    #[test]
    fn filters_parse_from_wire_shapes() {
        let f = parse_filters(Some(&json!([
            { "column": "name", "op": "=", "value": "Amy" },
            { "column": "id", "op": "in", "value": [1, 2] },
            { "column": "email", "value": null }
        ])))
        .unwrap();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].op, FilterOp::Eq);
        assert_eq!(f[1].op, FilterOp::In);
        // `op` defaults to `=` when omitted.
        assert_eq!(f[2].op, FilterOp::Eq);
        assert_eq!(f[2].value, Value::Null);
    }

    #[test]
    fn filters_reject_unknown_operators_and_bad_shapes() {
        assert!(
            parse_filters(Some(&json!([{ "column": "a", "op": "; DROP", "value": 1 }])))
                .is_err()
        );
        assert!(parse_filters(Some(&json!([{ "op": "=", "value": 1 }]))).is_err());
        assert!(parse_filters(Some(&json!("not an array"))).is_err());
        assert!(parse_filters(Some(&json!([1, 2]))).is_err());
        // Absent / null are simply "no filter".
        assert!(parse_filters(None).unwrap().is_empty());
        assert!(parse_filters(Some(&Value::Null)).unwrap().is_empty());
    }

    #[test]
    fn json_encoded_strings_are_accepted_because_the_schema_says_string() {
        let f = parse_filters(Some(&json!(
            "[{\"column\":\"name\",\"op\":\"=\",\"value\":\"Amy\"}]"
        )))
        .unwrap();
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].column, "name");

        let c = parse_string_array(Some(&json!("[\"id\",\"name\"]"))).unwrap();
        assert_eq!(c, vec!["id".to_string(), "name".to_string()]);

        assert_eq!(parse_limit(Some(&json!("25"))), Some(25));
        assert_eq!(parse_limit(Some(&json!(25))), Some(25));
        assert_eq!(parse_limit(Some(&json!("abc"))), None);
        assert_eq!(parse_limit(None), None);
    }

    #[test]
    fn columns_parse_and_reject() {
        assert_eq!(
            parse_string_array(Some(&json!(["id", " name "]))).unwrap(),
            vec!["id".to_string(), "name".to_string()]
        );
        assert!(parse_string_array(Some(&json!("id"))).is_err());
        assert!(parse_string_array(Some(&json!([1]))).is_err());
        assert!(parse_string_array(None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn ungranted_agent_is_refused_without_leaking_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.payroll]\ndriver = \"sqlite\"\nurl = \"/tmp/p.sqlite\"\nallowed_tables = [\"salaries\"]\n",
        )
        .unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        let err = resolve_granted(dir.path(), "worker", "payroll")
            .await
            .unwrap_err();
        assert!(err.contains("沒有資料來源"), "{err}");
        // The refusal names what the agent HAS, never what exists in config.
        assert!(!err.contains("salaries"), "{err}");
    }

    #[tokio::test]
    async fn grant_match_is_exact_not_substring() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm_payroll]\ndriver = \"sqlite\"\nurl = \"/tmp/p.sqlite\"\nallowed_tables = [\"*\"]\n",
        )
        .unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        assert!(
            resolve_granted(dir.path(), "worker", "crm_payroll")
                .await
                .is_err(),
            "a grant for `crm` must not reach `crm_payroll`"
        );
    }

    #[tokio::test]
    async fn granted_but_unconfigured_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        let err = resolve_granted(dir.path(), "worker", "crm")
            .await
            .unwrap_err();
        assert!(err.contains("[db_sources.crm]"), "{err}");
    }

    #[tokio::test]
    async fn granted_but_misconfigured_reports_the_reason() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm]\ndriver = \"postgres\"\nurl = \"postgres://u:p@h/db\"\nallowed_tables = [\"t\"]\n",
        )
        .unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        let err = resolve_granted(dir.path(), "worker", "crm")
            .await
            .unwrap_err();
        assert!(err.contains("設定有誤"), "{err}");
        assert!(err.contains("secret://"), "{err}");
    }

    #[tokio::test]
    async fn db_sources_without_any_grant_is_an_error_result() {
        let dir = tempfile::tempdir().unwrap();
        let out = handle_db_sources(dir.path(), "worker").await;
        assert_eq!(out["isError"], json!(true));
        assert!(text_of(&out).contains("db_sources"), "{}", text_of(&out));
    }

    #[tokio::test]
    async fn db_sources_lists_only_granted_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm]\nlabel = \"客戶\"\ndriver = \"sqlite\"\nurl = \"/tmp/c.sqlite\"\nallowed_tables = [\"t\"]\n\
             \n[db_sources.payroll]\ndriver = \"sqlite\"\nurl = \"/tmp/p.sqlite\"\nallowed_tables = [\"t\"]\n",
        )
        .unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        let out = handle_db_sources(dir.path(), "worker").await;
        let text = text_of(&out);
        assert!(text.contains("crm"), "{text}");
        assert!(text.contains("客戶"), "{text}");
        assert!(!text.contains("payroll"), "{text}");
    }

    /// Restricted source → `db_query` refused, and refused *before* any
    /// connection is opened. The fixture points at a file that does not
    /// exist: if the handler reached `open_source` the error would be a
    /// connect failure instead.
    #[tokio::test]
    async fn db_query_refused_on_a_restricted_source_without_connecting() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm]\ndriver = \"sqlite\"\nurl = \"/nonexistent/never-created.sqlite\"\nallowed_tables = [\"customers\"]\n",
        )
        .unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        let out = handle_db_query(
            &json!({ "source": "crm", "sql": "SELECT 1" }),
            dir.path(),
            "worker",
        )
        .await;
        assert_eq!(out["isError"], json!(true));
        let text = text_of(&out);
        assert!(text.contains("設有資料表白名單"), "{text}");
        assert!(text.contains("db_select"), "{text}");
        // Proof it never tried to connect: a connect failure names the file.
        assert!(!text.contains("never-created.sqlite"), "{text}");
    }

    /// Wildcard source → the free-SQL gate lets the call through. (It then
    /// fails on the deliberately-missing file, which is exactly the evidence
    /// that the gate is no longer what stopped it.)
    #[tokio::test]
    async fn db_query_passes_the_gate_on_a_wildcard_source() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm]\ndriver = \"sqlite\"\nurl = \"/nonexistent/never-created.sqlite\"\nallowed_tables = [\"*\"]\n",
        )
        .unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        let out = handle_db_query(
            &json!({ "source": "crm", "sql": "SELECT 1" }),
            dir.path(),
            "worker",
        )
        .await;
        let text = text_of(&out);
        assert!(!text.contains("設有資料表白名單"), "gate must not fire: {text}");
        assert!(text.contains("never-created.sqlite"), "{text}");
    }

    /// `db_select` is unaffected by the free-SQL rule — an allowlisted source
    /// is exactly what it is for.
    #[tokio::test]
    async fn db_select_is_unaffected_by_the_free_sql_rule() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm]\ndriver = \"sqlite\"\nurl = \"/nonexistent/never-created.sqlite\"\nallowed_tables = [\"customers\"]\n",
        )
        .unwrap();
        let agent_dir = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("agent.toml"), "[capabilities]\ndb_sources = [\"crm\"]\n")
            .unwrap();

        let out = handle_db_select(
            &json!({ "source": "crm", "table": "customers" }),
            dir.path(),
            "worker",
        )
        .await;
        let text = text_of(&out);
        assert!(!text.contains("設有資料表白名單"), "{text}");
    }

    #[tokio::test]
    async fn invalid_agent_id_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_granted(dir.path(), "../../etc", "crm")
            .await
            .unwrap_err();
        assert!(err.contains("身分無效"), "{err}");
        assert!(granted_sources(dir.path(), "../../etc").is_empty());
    }

    #[tokio::test]
    async fn missing_source_argument_is_explained() {
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_granted(dir.path(), "worker", "  ").await.unwrap_err();
        assert!(err.contains("source"), "{err}");
    }
}
