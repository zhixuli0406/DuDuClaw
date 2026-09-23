//! Operator-facing RPCs for `config.toml [db_sources.*]` (WP-D,
//! `DESIGN-redaction-field-rules-2026-09` §13.7).
//!
//! Five admin-only methods behind one dispatch arm in `handlers.rs`:
//! `db_sources.list` / `test` / `upsert` / `remove` / `tables`.
//!
//! ## The connection string never comes back out
//!
//! `list` answers with a `SecretStatus` — *is it set, where does it live, can
//! the dashboard overwrite it* — and never the value, exactly like every other
//! credential surface since the WP-H1 doctrine. A literal PostgreSQL / MySQL
//! DSN handed to `upsert` is encrypted with the machine keyfile and stored as
//! `url_enc`; a plaintext `url` is written only for SQLite, where the value is
//! a filesystem path rather than a credential. Driver errors are scrubbed and
//! capped before they reach the client, so a failed connect cannot echo a DSN
//! back into a browser tab.
//!
//! ## Test before save
//!
//! `upsert` refuses to persist a source it could not connect to, unless the
//! caller explicitly passes `skip_test: true` (the escape hatch for a database
//! that is temporarily down, or a `secret://vault/…` reference whose backend
//! is not reachable from this host). `test` itself accepts inline parameters
//! and writes nothing, so the dashboard can validate a form before committing
//! it — the same shape `odoo.test` already has.

use std::path::{Path, PathBuf};

use duduclaw_db::{
    ALLOW_ALL_TABLES, DbError, Driver, LoadedDbSources, clamp_max_rows, clamp_timeout_ms,
    config::{MAX_ALLOWED_TABLES, is_valid_source_name},
    is_valid_identifier, parse_db_sources,
};
use serde_json::{Value, json};

use crate::protocol::WsFrame;

/// Longest error text forwarded to the dashboard (mirrors `scrub_odoo_error`).
const MAX_ERROR_LEN: usize = 240;

/// The five methods this module owns. Kept here so the `handlers.rs` arm and
/// this dispatcher can never disagree about the surface.
pub const METHODS: &[&str] = &[
    "db_sources.list",
    "db_sources.test",
    "db_sources.upsert",
    "db_sources.remove",
    "db_sources.tables",
];

/// Route one already-admin-authorized `db_sources.*` call.
///
/// Admin gating lives in the `handlers.rs` arm (`require_admin!()`), next to
/// every other privileged method, rather than being re-derived here.
pub async fn dispatch(home_dir: &Path, method: &str, params: Value) -> WsFrame {
    match method {
        "db_sources.list" => list(home_dir).await,
        "db_sources.test" => test(home_dir, params).await,
        "db_sources.upsert" => upsert(home_dir, params).await,
        "db_sources.remove" => remove(home_dir, params).await,
        "db_sources.tables" => tables(home_dir, params).await,
        other => WsFrame::error_response("", &format!("Unknown db_sources method: {other}")),
    }
}

// ── list ────────────────────────────────────────────────────────────────────

async fn list(home_dir: &Path) -> WsFrame {
    let loaded = duduclaw_db::load_db_sources(home_dir).await;
    let sources: Vec<Value> = loaded
        .sources
        .iter()
        .map(|s| {
            json!({
                "name": s.name,
                "label": s.label,
                "driver": s.driver.as_str(),
                "allowed_tables": s.allowed_tables,
                "max_rows": s.max_rows,
                "timeout_ms": s.timeout_ms,
                // Never the URL — only whether one is set and where it lives.
                "url_status": serde_json::to_value(s.url_status()).unwrap_or(Value::Null),
            })
        })
        .collect();
    let errors: Vec<Value> = loaded
        .errors
        .iter()
        .map(|e| json!({ "name": e.name, "message": e.message }))
        .collect();
    WsFrame::ok_response("", json!({ "sources": sources, "errors": errors }))
}

// ── test ────────────────────────────────────────────────────────────────────

async fn test(home_dir: &Path, params: Value) -> WsFrame {
    // Inline mode when the form supplied a driver; stored mode otherwise.
    let candidate = if params.get("driver").is_some() {
        match build_candidate(home_dir, &params, None).await {
            Ok(c) => c,
            Err(msg) => return WsFrame::error_response("", &msg),
        }
    } else {
        let name = match required_name(&params) {
            Ok(n) => n,
            Err(msg) => return WsFrame::error_response("", &msg),
        };
        let loaded = duduclaw_db::load_db_sources(home_dir).await;
        match loaded.get(&name) {
            Some(entry) => Candidate {
                table: None,
                entry: entry.clone(),
            },
            None => {
                return WsFrame::error_response(
                    "",
                    &unknown_source_message(&name, &loaded),
                );
            }
        }
    };

    // One shape only: `success` / `message` / `tables`, matching `odoo.test`
    // (which this RPC is modelled on) and `api.ts`'s `DbSourceTestResult`.
    match probe(home_dir, &candidate).await {
        Ok(tables) => WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "message": format!("連線成功，可讀取 {} 個資料表", tables.len()),
                "tables": tables,
            }),
        ),
        Err(msg) => WsFrame::ok_response(
            "",
            json!({ "success": false, "message": msg, "tables": [] }),
        ),
    }
}

// ── upsert ──────────────────────────────────────────────────────────────────

async fn upsert(home_dir: &Path, params: Value) -> WsFrame {
    let name = match required_name(&params) {
        Ok(n) => n,
        Err(msg) => return WsFrame::error_response("", &msg),
    };

    let config_path = home_dir.join("config.toml");
    let mut doc = match read_config_doc(&config_path).await {
        Ok(d) => d,
        Err(msg) => return WsFrame::error_response("", &msg),
    };
    let existing = existing_block(&doc, &name);

    let candidate = match build_candidate(home_dir, &params, existing.as_ref()).await {
        Ok(c) => c,
        Err(msg) => return WsFrame::error_response("", &msg),
    };

    // Test before save, unless the operator explicitly opted out.
    let skip_test = params
        .get("skip_test")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !skip_test && let Err(msg) = probe(home_dir, &candidate).await {
        return WsFrame::error_response(
            "",
            &format!("連線測試失敗，未儲存：{msg}（確定要先存下來，請勾選「略過連線測試」）"),
        );
    }

    let Some(block) = candidate.table.clone() else {
        return WsFrame::error_response("", "內部錯誤：upsert 缺少要寫入的設定內容");
    };
    if let Err(msg) = write_block(&mut doc, &name, &block) {
        return WsFrame::error_response("", &msg);
    }
    if let Err(msg) = commit(&config_path, &doc).await {
        return WsFrame::error_response("", &msg);
    }

    tracing::info!(source = %name, "db_sources.upsert completed");
    WsFrame::ok_response("", json!({ "success": true, "name": name }))
}

// ── remove ──────────────────────────────────────────────────────────────────

async fn remove(home_dir: &Path, params: Value) -> WsFrame {
    let name = match required_name(&params) {
        Ok(n) => n,
        Err(msg) => return WsFrame::error_response("", &msg),
    };
    let config_path = home_dir.join("config.toml");
    let mut doc = match read_config_doc(&config_path).await {
        Ok(d) => d,
        Err(msg) => return WsFrame::error_response("", &msg),
    };
    let removed = doc
        .get_mut("db_sources")
        .and_then(|v| v.as_table_mut())
        .map(|t| t.remove(&name).is_some())
        .unwrap_or(false);
    if !removed {
        return WsFrame::error_response("", &format!("資料來源「{name}」不存在"));
    }
    if let Err(msg) = commit(&config_path, &doc).await {
        return WsFrame::error_response("", &msg);
    }
    // Agents that still list this source in `[capabilities] db_sources` are
    // left alone on purpose: the grant is harmless once the source is gone
    // (every tool refuses an unconfigured source by name), and silently
    // rewriting other people's agent.toml files is not this RPC's business.
    tracing::info!(source = %name, "db_sources.remove completed");
    WsFrame::ok_response("", json!({ "success": true, "name": name }))
}

// ── tables ──────────────────────────────────────────────────────────────────

async fn tables(home_dir: &Path, params: Value) -> WsFrame {
    let name = match required_name(&params) {
        Ok(n) => n,
        Err(msg) => return WsFrame::error_response("", &msg),
    };
    let loaded = duduclaw_db::load_db_sources(home_dir).await;
    let Some(entry) = loaded.get(&name) else {
        return WsFrame::error_response("", &unknown_source_message(&name, &loaded));
    };
    let src = match duduclaw_db::open_source(entry, home_dir).await {
        Ok(s) => s,
        Err(e) => return WsFrame::error_response("", &scrub_db_error(&e)),
    };
    let result = src.list_tables().await;
    src.close().await;
    match result {
        Ok(tables) => WsFrame::ok_response("", json!({ "tables": tables_json(&tables) })),
        Err(e) => WsFrame::error_response("", &scrub_db_error(&e)),
    }
}

// ── Candidate construction ──────────────────────────────────────────────────

/// A source about to be tested and/or written.
struct Candidate {
    /// The `[db_sources.<name>]` block to persist. `None` for stored-mode
    /// `test`, which writes nothing.
    table: Option<toml::Table>,
    entry: duduclaw_db::DbSourceEntry,
}

/// Validate form parameters into a candidate, reusing the crate's own loader
/// so the dashboard can never persist a block the MCP side would then refuse.
///
/// `existing` is the currently-stored block, used to preserve a credential the
/// form did not resend (the same "omit keeps, empty clears" contract the Odoo
/// form uses).
async fn build_candidate(
    home_dir: &Path,
    params: &Value,
    existing: Option<&toml::Table>,
) -> Result<Candidate, String> {
    let name = required_name(params)?;

    let driver_raw = params
        .get("driver")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .or_else(|| existing.and_then(|t| t.get("driver")).and_then(|v| v.as_str()))
        .ok_or_else(|| "缺少 driver 參數（postgres / mysql / sqlite）".to_string())?;
    let driver = Driver::parse(driver_raw)
        .ok_or_else(|| format!("driver「{driver_raw}」不支援，只接受 postgres / mysql / sqlite"))?;

    let mut block = toml::Table::new();
    block.insert(
        "driver".into(),
        toml::Value::String(driver.as_str().to_string()),
    );

    let label = params
        .get("label")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            existing
                .and_then(|t| t.get("label"))
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| name.clone());
    if label.chars().count() > 128 {
        return Err("label 過長（上限 128 字元）".into());
    }
    block.insert("label".into(), toml::Value::String(label));

    // ── credential ──────────────────────────────────────────────────────
    let secret_ref = params
        .get("url_secret_ref")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let literal_url = params
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if secret_ref.is_some() && literal_url.is_some() {
        return Err("url 與 url_secret_ref 只能擇一提供".into());
    }

    match (secret_ref, literal_url) {
        (Some(reference), _) => {
            if !reference.starts_with("secret://") {
                return Err(
                    "url_secret_ref 必須是 secret://<backend>/<name> 形式的參照".into(),
                );
            }
            block.insert("url".into(), toml::Value::String(reference.to_string()));
        }
        (None, Some(url)) => {
            if url.starts_with("secret://") {
                // A reference typed into the plain field is still a reference,
                // not a credential — store it as one rather than encrypting the
                // pointer (the exact bug the credentials doctrine was written
                // to kill).
                block.insert("url".into(), toml::Value::String(url.to_string()));
            } else if driver == Driver::Sqlite {
                block.insert("url".into(), toml::Value::String(url.to_string()));
            } else {
                let enc = crate::config_crypto::encrypt_value(url, home_dir).ok_or_else(|| {
                    "無法加密連線字串 — 金鑰檔寫入失敗（磁碟已滿或權限不足），請查看 gateway log。"
                        .to_string()
                })?;
                block.insert("url_enc".into(), toml::Value::String(enc));
            }
        }
        (None, None) => {
            // Preserve whatever is already stored. Both twins are carried over
            // so an existing `secret://` reference or ciphertext survives a
            // form submit that only changed, say, `allowed_tables`.
            let mut carried = false;
            for key in ["url_enc", "url"] {
                if let Some(v) = existing
                    .and_then(|t| t.get(key))
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                {
                    block.insert(key.into(), toml::Value::String(v.to_string()));
                    carried = true;
                }
            }
            if !carried {
                return Err(
                    "缺少連線字串：請提供 url（SQLite 路徑）或 url_secret_ref（secret:// 參照）"
                        .into(),
                );
            }
        }
    }

    // ── allowed_tables ──────────────────────────────────────────────────
    let allowed: Vec<String> = match params.get("allowed_tables") {
        Some(Value::Array(arr)) => {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .map(str::trim)
                    .ok_or_else(|| "allowed_tables 只能包含字串".to_string())?;
                if s.is_empty() {
                    continue;
                }
                if s != ALLOW_ALL_TABLES && !is_valid_identifier(s) {
                    return Err(format!(
                        "資料表名稱「{s}」不合法（僅允許英數字與底線，且不可數字開頭；或使用 \"*\"）"
                    ));
                }
                out.push(s.to_string());
            }
            out
        }
        Some(Value::Null) | None => existing
            .and_then(|t| t.get("allowed_tables"))
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default(),
        Some(_) => return Err("allowed_tables 必須是字串陣列".into()),
    };
    if allowed.is_empty() {
        return Err(
            "allowed_tables 不可為空：請列出允許讀取的資料表，或用 [\"*\"] 允許全部（會在 log 留下警告）"
                .into(),
        );
    }
    if allowed.len() > MAX_ALLOWED_TABLES {
        return Err(format!(
            "allowed_tables 過長（{} 項，上限 {MAX_ALLOWED_TABLES} 項）",
            allowed.len()
        ));
    }
    block.insert(
        "allowed_tables".into(),
        toml::Value::Array(allowed.into_iter().map(toml::Value::String).collect()),
    );

    // ── caps ────────────────────────────────────────────────────────────
    let max_rows = params
        .get("max_rows")
        .and_then(|v| v.as_i64())
        .or_else(|| existing.and_then(|t| t.get("max_rows")).and_then(|v| v.as_integer()));
    block.insert(
        "max_rows".into(),
        toml::Value::Integer(clamp_max_rows(max_rows) as i64),
    );
    let timeout_ms = params
        .get("timeout_ms")
        .and_then(|v| v.as_i64())
        .or_else(|| {
            existing
                .and_then(|t| t.get("timeout_ms"))
                .and_then(|v| v.as_integer())
        });
    block.insert(
        "timeout_ms".into(),
        toml::Value::Integer(clamp_timeout_ms(timeout_ms) as i64),
    );

    // ── dry-compile ─────────────────────────────────────────────────────
    // Round-trip the candidate through the connector's own loader. Anything
    // the MCP side would refuse is refused here, before it reaches disk.
    let entry = compile_one(&name, &block)?;

    Ok(Candidate {
        table: Some(block),
        entry,
    })
}

/// Parse a single candidate block with the real loader.
fn compile_one(name: &str, block: &toml::Table) -> Result<duduclaw_db::DbSourceEntry, String> {
    let mut sources = toml::Table::new();
    sources.insert(name.to_string(), toml::Value::Table(block.clone()));
    let mut root = toml::Table::new();
    root.insert("db_sources".into(), toml::Value::Table(sources));
    let loaded = parse_db_sources(&root);
    if let Some(entry) = loaded.get(name) {
        return Ok(entry.clone());
    }
    Err(loaded
        .errors
        .first()
        .map(|e| e.message.clone())
        .unwrap_or_else(|| format!("資料來源「{name}」設定無效")))
}

/// Connect, `SELECT 1`, and list tables. Never writes anything.
async fn probe(home_dir: &Path, candidate: &Candidate) -> Result<Vec<Value>, String> {
    let src = duduclaw_db::open_source(&candidate.entry, home_dir)
        .await
        .map_err(|e| scrub_db_error(&e))?;
    let ping = src.ping().await;
    if let Err(e) = ping {
        src.close().await;
        return Err(scrub_db_error(&e));
    }
    let listed = src.list_tables().await;
    src.close().await;
    listed.map(|t| tables_json(&t)).map_err(|e| scrub_db_error(&e))
}

fn tables_json(tables: &[duduclaw_db::TableInfo]) -> Vec<Value> {
    tables
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "columns": t.columns
                    .iter()
                    .map(|c| json!({ "name": c.name, "type": c.data_type }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect()
}

// ── Config file I/O ─────────────────────────────────────────────────────────

/// Read `config.toml` as an editable document.
///
/// A missing file is an empty document; a **malformed** one is an error rather
/// than an empty document, so a rewrite can never silently discard an operator's
/// existing config because of an unrelated typo (the `read_config_table_strict`
/// rule in `handlers.rs`).
async fn read_config_doc(path: &Path) -> Result<toml_edit::DocumentMut, String> {
    match tokio::fs::read_to_string(path).await {
        Ok(content) => content
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| format!("設定檔解析失敗，拒絕覆寫：{e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok(toml_edit::DocumentMut::new())
        }
        Err(e) => Err(format!("設定檔讀取失敗：{e}")),
    }
}

fn existing_block(doc: &toml_edit::DocumentMut, name: &str) -> Option<toml::Table> {
    let item = doc.get("db_sources")?.get(name)?;
    let text = item.to_string();
    // `toml_edit` → `toml` via the one representation both agree on.
    format!("x = {{{text}}}")
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("x").and_then(|v| v.as_table()).cloned())
        .or_else(|| {
            // Table syntax (`[db_sources.name]`) renders as key/value lines,
            // not an inline table — parse those directly.
            text.parse::<toml::Table>().ok()
        })
}

fn write_block(
    doc: &mut toml_edit::DocumentMut,
    name: &str,
    block: &toml::Table,
) -> Result<(), String> {
    let root = doc.as_table_mut();
    if !root.contains_key("db_sources") {
        let mut t = toml_edit::Table::new();
        t.set_implicit(true);
        root.insert("db_sources", toml_edit::Item::Table(t));
    }
    let sources = root
        .get_mut("db_sources")
        .and_then(|i| i.as_table_mut())
        .ok_or_else(|| "[db_sources] 存在但不是表格，拒絕覆寫".to_string())?;

    let mut entry = toml_edit::Table::new();
    for (key, value) in block {
        let item = match value {
            toml::Value::String(s) => toml_edit::value(s.clone()),
            toml::Value::Integer(i) => toml_edit::value(*i),
            toml::Value::Boolean(b) => toml_edit::value(*b),
            toml::Value::Array(arr) => {
                let mut a = toml_edit::Array::new();
                for v in arr {
                    if let Some(s) = v.as_str() {
                        a.push(s);
                    }
                }
                toml_edit::value(a)
            }
            _ => return Err(format!("不支援的設定值型別：{key}")),
        };
        entry.insert(key, item);
    }
    // Replacing the whole block is deliberate: it guarantees the plaintext and
    // encrypted credential twins never coexist (WP-H1 "residue"), which a
    // key-by-key merge would allow.
    sources.insert(name, toml_edit::Item::Table(entry));
    Ok(())
}

async fn commit(path: &Path, doc: &toml_edit::DocumentMut) -> Result<(), String> {
    let tmp: PathBuf = path.with_extension("toml.tmp");
    tokio::fs::write(&tmp, doc.to_string())
        .await
        .map_err(|e| format!("寫入設定檔失敗：{e}"))?;
    if let Err(e) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(format!("套用設定檔失敗：{e}"));
    }
    Ok(())
}

// ── Small helpers ───────────────────────────────────────────────────────────

fn required_name(params: &Value) -> Result<String, String> {
    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .unwrap_or("");
    if name.is_empty() {
        return Err("缺少 name 參數（資料來源名稱）".into());
    }
    if !is_valid_source_name(name) {
        return Err(format!(
            "資料來源名稱「{name}」不合法（只允許小寫英文字母開頭，之後是小寫字母、數字或底線）"
        ));
    }
    Ok(name.to_string())
}

fn unknown_source_message(name: &str, loaded: &LoadedDbSources) -> String {
    if let Some(err) = loaded
        .errors
        .iter()
        .find(|e| e.name.eq_ignore_ascii_case(name))
    {
        return format!("資料來源「{name}」設定有誤：{}", err.message);
    }
    format!("資料來源「{name}」不存在")
}

/// Cap and de-credential driver error text before it leaves the gateway.
///
/// [`DbError`] already refuses to carry the connection URL, so this is a
/// second pass plus the same 240-character cap `scrub_odoo_error` applies —
/// a megabyte of database server HTML has no business in a WebSocket frame.
/// Truncation is by character, never by byte index (coding convention 1).
fn scrub_db_error(e: &DbError) -> String {
    let scrubbed = duduclaw_db::scrub_connection_details(&e.to_string(), None);
    let mut out: String = scrubbed.chars().take(MAX_ERROR_LEN).collect();
    if scrubbed.chars().count() > MAX_ERROR_LEN {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_payload(frame: &WsFrame) -> Value {
        serde_json::to_value(frame).unwrap_or(Value::Null)
    }

    async fn upsert_ok(dir: &Path, params: Value) -> Value {
        let frame = upsert(dir, params).await;
        frame_payload(&frame)
    }

    fn config_text(dir: &Path) -> String {
        std::fs::read_to_string(dir.join("config.toml")).unwrap_or_default()
    }

    #[tokio::test]
    async fn list_is_empty_without_config() {
        let dir = tempfile::tempdir().unwrap();
        let payload = frame_payload(&list(dir.path()).await);
        let text = payload.to_string();
        assert!(text.contains("\"sources\":[]"), "{text}");
    }

    #[tokio::test]
    async fn list_reports_status_but_never_the_url() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm]\ndriver = \"postgres\"\nurl = \"secret://env/CRM_DSN\"\nallowed_tables = [\"customers\"]\n",
        )
        .unwrap();
        let text = frame_payload(&list(dir.path()).await).to_string();
        assert!(text.contains("\"name\":\"crm\""), "{text}");
        assert!(text.contains("url_status"), "{text}");
        assert!(text.contains("env:CRM_DSN"), "{text}");
        // The status names the source, never a value.
        assert!(!text.contains("\"url\":"), "{text}");
    }

    #[tokio::test]
    async fn list_surfaces_broken_blocks_instead_of_hiding_them() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[db_sources.crm]\ndriver = \"postgres\"\nurl = \"postgres://u:p@h/db\"\nallowed_tables = [\"t\"]\n",
        )
        .unwrap();
        let text = frame_payload(&list(dir.path()).await).to_string();
        assert!(text.contains("\"sources\":[]"), "{text}");
        assert!(text.contains("crm"), "{text}");
        assert!(text.contains("secret://"), "{text}");
    }

    #[tokio::test]
    async fn upsert_writes_a_sqlite_source_and_survives_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("demo.sqlite");
        // A real file so the pre-save connection test passes.
        make_sqlite(&db).await;

        let out = upsert_ok(
            dir.path(),
            json!({
                "name": "demo",
                "driver": "sqlite",
                "url": db.to_string_lossy(),
                "allowed_tables": ["customers"],
                "label": "示範",
            }),
        )
        .await;
        assert!(out.to_string().contains("\"success\":true"), "{out}");

        let loaded = duduclaw_db::load_db_sources(dir.path()).await;
        assert_eq!(loaded.names(), vec!["demo".to_string()]);
        let entry = loaded.get("demo").unwrap();
        assert_eq!(entry.label, "示範");
        assert_eq!(entry.allowed_tables, vec!["customers".to_string()]);
    }

    #[tokio::test]
    async fn upsert_encrypts_a_literal_postgres_dsn() {
        let dir = tempfile::tempdir().unwrap();
        let out = upsert_ok(
            dir.path(),
            json!({
                "name": "crm",
                "driver": "postgres",
                "url": "postgres://u:hunter2@db.internal/app",
                "allowed_tables": ["customers"],
                // The database is not reachable from a unit test.
                "skip_test": true,
            }),
        )
        .await;
        assert!(out.to_string().contains("\"success\":true"), "{out}");
        let text = config_text(dir.path());
        assert!(text.contains("url_enc"), "{text}");
        assert!(!text.contains("hunter2"), "plaintext DSN must not hit disk: {text}");
    }

    #[tokio::test]
    async fn upsert_keeps_a_secret_reference_as_a_reference() {
        let dir = tempfile::tempdir().unwrap();
        let out = upsert_ok(
            dir.path(),
            json!({
                "name": "crm",
                "driver": "mysql",
                "url_secret_ref": "secret://vault/crm-dsn",
                "allowed_tables": ["*"],
                "skip_test": true,
            }),
        )
        .await;
        assert!(out.to_string().contains("\"success\":true"), "{out}");
        let text = config_text(dir.path());
        assert!(text.contains("secret://vault/crm-dsn"), "{text}");
        assert!(!text.contains("url_enc"), "a reference must not be encrypted: {text}");
    }

    #[tokio::test]
    async fn upsert_rejects_bad_input() {
        let dir = tempfile::tempdir().unwrap();
        for (label, params) in [
            ("bad name", json!({ "name": "Bad-Name", "driver": "sqlite", "url": "/tmp/x", "allowed_tables": ["t"] })),
            ("unknown driver", json!({ "name": "a", "driver": "oracle", "url": "/tmp/x", "allowed_tables": ["t"] })),
            ("no credential", json!({ "name": "a", "driver": "sqlite", "allowed_tables": ["t"] })),
            ("empty allowlist", json!({ "name": "a", "driver": "sqlite", "url": "/tmp/x", "allowed_tables": [] })),
            ("injection table", json!({ "name": "a", "driver": "sqlite", "url": "/tmp/x", "allowed_tables": ["t; DROP TABLE x"] })),
            ("both url forms", json!({ "name": "a", "driver": "sqlite", "url": "/tmp/x", "url_secret_ref": "secret://env/X", "allowed_tables": ["t"] })),
            ("bogus reference", json!({ "name": "a", "driver": "postgres", "url_secret_ref": "not-a-reference", "allowed_tables": ["t"] })),
        ] {
            let out = upsert_ok(dir.path(), params).await;
            assert!(
                out.to_string().contains("error"),
                "{label} must be refused, got: {out}"
            );
            assert!(
                !dir.path().join("config.toml").exists(),
                "{label} must not have written config.toml"
            );
        }
    }

    #[tokio::test]
    async fn upsert_refuses_to_save_what_it_cannot_connect_to() {
        let dir = tempfile::tempdir().unwrap();
        let out = upsert_ok(
            dir.path(),
            json!({
                "name": "demo",
                "driver": "sqlite",
                "url": dir.path().join("missing.sqlite").to_string_lossy(),
                "allowed_tables": ["customers"],
            }),
        )
        .await;
        assert!(out.to_string().contains("連線測試失敗"), "{out}");
        assert!(!dir.path().join("config.toml").exists());
    }

    #[tokio::test]
    async fn upsert_preserves_an_untouched_credential() {
        let dir = tempfile::tempdir().unwrap();
        upsert_ok(
            dir.path(),
            json!({
                "name": "crm",
                "driver": "postgres",
                "url_secret_ref": "secret://env/CRM_DSN",
                "allowed_tables": ["customers"],
                "skip_test": true,
            }),
        )
        .await;
        // Second submit changes only the allowlist and sends no credential.
        let out = upsert_ok(
            dir.path(),
            json!({
                "name": "crm",
                "driver": "postgres",
                "allowed_tables": ["customers", "orders"],
                "skip_test": true,
            }),
        )
        .await;
        assert!(out.to_string().contains("\"success\":true"), "{out}");
        let loaded = duduclaw_db::load_db_sources(dir.path()).await;
        let entry = loaded.get("crm").unwrap();
        assert_eq!(entry.allowed_tables.len(), 2);
        assert!(entry.url_status().configured, "credential must survive");
    }

    #[tokio::test]
    async fn upsert_refuses_to_rewrite_a_malformed_config() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "this is = = not toml").unwrap();
        let out = upsert_ok(
            dir.path(),
            json!({ "name": "a", "driver": "sqlite", "url": "/tmp/x", "allowed_tables": ["t"], "skip_test": true }),
        )
        .await;
        assert!(out.to_string().contains("拒絕覆寫"), "{out}");
        assert_eq!(config_text(dir.path()), "this is = = not toml");
    }

    #[tokio::test]
    async fn upsert_leaves_unrelated_config_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[settings]\nfoo = 1\n\n[odoo]\nurl = \"https://erp.example.com\"\n",
        )
        .unwrap();
        upsert_ok(
            dir.path(),
            json!({ "name": "a", "driver": "sqlite", "url": "/tmp/x.sqlite", "allowed_tables": ["t"], "skip_test": true }),
        )
        .await;
        let text = config_text(dir.path());
        assert!(text.contains("[settings]"), "{text}");
        assert!(text.contains("erp.example.com"), "{text}");
        assert!(text.contains("[db_sources.a]"), "{text}");
    }

    #[tokio::test]
    async fn remove_deletes_only_the_named_block() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[settings]\nfoo = 1\n\n[db_sources.a]\ndriver = \"sqlite\"\nurl = \"/tmp/a\"\nallowed_tables = [\"t\"]\n\n[db_sources.b]\ndriver = \"sqlite\"\nurl = \"/tmp/b\"\nallowed_tables = [\"t\"]\n",
        )
        .unwrap();
        let out = frame_payload(&remove(dir.path(), json!({ "name": "a" })).await);
        assert!(out.to_string().contains("\"success\":true"), "{out}");
        let loaded = duduclaw_db::load_db_sources(dir.path()).await;
        assert_eq!(loaded.names(), vec!["b".to_string()]);
        assert!(config_text(dir.path()).contains("[settings]"));
    }

    #[tokio::test]
    async fn remove_reports_a_missing_source() {
        let dir = tempfile::tempdir().unwrap();
        let out = frame_payload(&remove(dir.path(), json!({ "name": "nope" })).await);
        assert!(out.to_string().contains("不存在"), "{out}");
    }

    #[tokio::test]
    async fn test_and_tables_work_against_a_real_sqlite_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("demo.sqlite");
        make_sqlite(&db).await;

        // Inline mode — nothing is written.
        let out = frame_payload(
            &test(
                dir.path(),
                json!({
                    "name": "demo",
                    "driver": "sqlite",
                    "url": db.to_string_lossy(),
                    "allowed_tables": ["customers"],
                }),
            )
            .await,
        );
        assert!(out.to_string().contains("\"success\":true"), "{out}");
        assert!(out.to_string().contains("customers"), "{out}");
        assert!(!dir.path().join("config.toml").exists(), "test must not write");

        // Stored mode, via db_sources.tables.
        upsert_ok(
            dir.path(),
            json!({
                "name": "demo",
                "driver": "sqlite",
                "url": db.to_string_lossy(),
                "allowed_tables": ["customers"],
            }),
        )
        .await;
        let out = frame_payload(&tables(dir.path(), json!({ "name": "demo" })).await);
        let text = out.to_string();
        assert!(text.contains("customers"), "{text}");
        assert!(text.contains("email"), "{text}");
        // The allowlist still applies.
        assert!(!text.contains("secrets"), "{text}");
    }

    #[tokio::test]
    async fn test_reports_failure_without_leaking_the_dsn() {
        let dir = tempfile::tempdir().unwrap();
        let out = frame_payload(
            &test(
                dir.path(),
                json!({
                    "name": "crm",
                    "driver": "postgres",
                    "url_secret_ref": "secret://env/DDC_TEST_MISSING_DSN",
                    "allowed_tables": ["*"],
                }),
            )
            .await,
        );
        let text = out.to_string();
        assert!(text.contains("\"success\":false"), "{text}");
        // The failure explains itself without echoing a DSN. (`secret://` may
        // appear as guidance prose — what must never appear is an actual
        // connection string or credential.)
        assert!(!text.contains("postgres://"), "{text}");
        assert!(!text.contains("@"), "no host/credential may escape: {text}");
    }

    /// The test result has exactly one spelling — `success` / `message` /
    /// `tables`. The short-lived `ok` / `table_count` / `error` aliases are
    /// gone; this locks them out so they cannot creep back.
    #[tokio::test]
    async fn test_payload_has_one_canonical_shape() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("demo.sqlite");
        make_sqlite(&db).await;

        let ok = frame_payload(
            &test(
                dir.path(),
                json!({
                    "name": "demo",
                    "driver": "sqlite",
                    "url": db.to_string_lossy(),
                    "allowed_tables": ["customers"],
                }),
            )
            .await,
        );
        let p = &ok["payload"];
        assert_eq!(p["success"], json!(true));
        assert_eq!(p["tables"].as_array().unwrap().len(), 1);
        assert!(p.get("ok").is_none(), "{p}");
        assert!(p.get("table_count").is_none(), "{p}");

        let bad = frame_payload(
            &test(
                dir.path(),
                json!({
                    "name": "demo",
                    "driver": "sqlite",
                    "url": dir.path().join("missing.sqlite").to_string_lossy(),
                    "allowed_tables": ["customers"],
                }),
            )
            .await,
        );
        let p = &bad["payload"];
        assert_eq!(p["success"], json!(false));
        assert!(p["message"].is_string(), "{p}");
        assert!(p.get("ok").is_none(), "{p}");
        assert!(p.get("error").is_none(), "{p}");
    }

    #[tokio::test]
    async fn unknown_method_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let out = frame_payload(&dispatch(dir.path(), "db_sources.drop_everything", json!({})).await);
        assert!(out.to_string().contains("Unknown db_sources method"), "{out}");
    }

    #[test]
    fn methods_list_matches_the_dispatcher() {
        assert_eq!(METHODS.len(), 5);
        for m in METHODS {
            assert!(m.starts_with("db_sources."), "{m}");
        }
    }

    /// Build the fixture with `rusqlite` (already a gateway dependency) rather
    /// than pulling sqlx into this crate just for a test.
    async fn make_sqlite(path: &Path) {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE customers (id INTEGER PRIMARY KEY, name TEXT, email TEXT, phone TEXT);\n\
             CREATE TABLE secrets (id INTEGER PRIMARY KEY, token TEXT);",
        )
        .unwrap();
    }
}
