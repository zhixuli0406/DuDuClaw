//! RFC-23 §14.2 (WP-F2): agent-facing tools for reading **local data files**.
//!
//! Three tools — `file_read` / `csv_read` / `xlsx_read` — exist for one
//! reason: an agent that reads `customers.csv` with the Claude CLI's built-in
//! `Read` (or `head` via `Bash`) puts every raw cell straight into the model's
//! context, because built-in tools are not MCP tools and therefore never pass
//! DuDuClaw's redaction choke point (`mcp_dispatch.rs`). Routing the same read
//! through an MCP tool is what makes the RFC-23 pipeline able to see, and
//! de-identify, the rows. The companion `data-file-guard` PreToolUse hook
//! (§14.4) blocks the built-in route so the model actually takes this one.
//!
//! ## What bounds these tools
//!
//! 1. **A path fence** — [`vet_path`]: canonicalize (which also resolves
//!    symlinks and proves existence), then require containment in one of the
//!    allowed roots. `..` is refused before canonicalization so the error names
//!    the real problem instead of a confusing "outside the roots".
//! 2. **Byte caps checked before parsing** — the file's metadata length is
//!    compared against the cap and the read is refused, so a mistyped path at a
//!    20 GB log never reaches the parser.
//! 3. **Row caps** — `limit` defaults to 200 and is clamped to
//!    [`ROW_LIMIT_MAX`]; `truncated` tells the model there is more.
//!
//! There is deliberately **no per-agent capability gate** (unlike the
//! `db_*` family): every root in the fence is either the caller's own agent
//! directory or an operator-declared `[files] allowed_roots` entry, so the
//! fence already answers "whose data is this?". `tools/list` therefore always
//! advertises all three.
//!
//! ## Audit
//!
//! Every call appends to `tool_calls.jsonl` with `path` / `table` /
//! `row_count` — the *shape* of the read. Cell content is never written
//! there: the audit log is not where customer data belongs (same rule as
//! `mcp_db.rs`'s filter values).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

// ── Caps (§14.2) ────────────────────────────────────────────────────────────

/// `file_read`: plain-text files only, 512 KiB.
pub const FILE_READ_MAX_BYTES: u64 = 512 * 1024;
/// `csv_read`: 64 MiB on disk.
pub const CSV_MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
/// `xlsx_read`: 32 MiB on disk (a spreadsheet parser expands far past its
/// on-disk size, so this cap is deliberately tighter than the CSV one).
pub const XLSX_MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Extensions `file_read` refuses, redirecting to the structured reader.
///
/// §14.2 scopes `file_read` to plain text. Letting it swallow a spreadsheet
/// would be a redaction bypass, not a convenience: a `db_field` rule binds to
/// `$.rows[*].<column>` (see `duduclaw_redaction::data_source`'s
/// `duduclaw_files` built-in, which deliberately binds only `csv_read` /
/// `xlsx_read`), so the same file returned as one text blob would never meet
/// its column rules — only the pattern rules would fire, and `客戶清單.xlsx.地址`
/// would sail through in the clear.
const STRUCTURED_EXTENSIONS: &[&str] = &["csv", "tsv", "xlsx", "xlsm", "xls", "ods"];

/// Default number of data rows returned by `csv_read` / `xlsx_read`.
pub const ROW_LIMIT_DEFAULT: usize = 200;
/// Hard ceiling on `limit`, whatever the caller asks for.
pub const ROW_LIMIT_MAX: usize = 2000;

// ── Result helpers (per-submodule convention, see mcp_db.rs) ────────────────

fn files_text(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn files_json(value: &Value) -> Value {
    files_text(&serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string()))
}

fn files_error(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
}

// ── Path fence ──────────────────────────────────────────────────────────────

/// The directories a data-file tool may read from, in the order they are
/// reported to the caller when a path is refused.
///
/// `<agent_dir>/attachments` is listed separately even though it sits inside
/// `<agent_dir>`: channel attachments land there (`media::save_attachment_in_base`)
/// and naming it explicitly is what makes the refusal message actionable.
///
/// An empty / malformed `agent_id` contributes no agent roots — fail-closed:
/// an unidentified caller gets only `<home>/attachments` plus whatever the
/// operator declared.
pub fn allowed_roots(home_dir: &Path, agent_id: &str) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if duduclaw_core::is_valid_agent_id(agent_id) {
        let agent_dir = home_dir.join("agents").join(agent_id);
        roots.push(agent_dir.join("attachments"));
        roots.push(agent_dir);
    }
    roots.push(home_dir.join("attachments"));
    roots.extend(configured_roots(home_dir));
    roots
}

/// `config.toml [files] allowed_roots = ["/srv/exports", ...]`.
///
/// Read raw (not through a typed config struct) for the same reason the
/// redaction spawn predicate does: this is one operator-set list on a path
/// that must not grow a new crate-level config type. Non-absolute entries are
/// dropped — a relative root would resolve against whatever cwd the MCP
/// server happened to inherit, which is not a fence.
fn configured_roots(home_dir: &Path) -> Vec<PathBuf> {
    let Ok(raw) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return Vec::new();
    };
    let Ok(doc) = toml::from_str::<toml::Value>(&raw) else {
        return Vec::new();
    };
    doc.get("files")
        .and_then(|v| v.get("allowed_roots"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .collect()
        })
        .unwrap_or_default()
}

fn render_roots(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join("、")
}

/// Resolve `raw` to a real regular file inside one of the allowed roots.
///
/// Every rejection carries the reason and the root list, never file content.
pub fn vet_path(raw: &str, home_dir: &Path, agent_id: &str) -> Result<PathBuf, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("缺少 path 參數（要讀取的檔案路徑）。".to_string());
    }
    let candidate = Path::new(raw);
    // Refused before canonicalization so the message names the real problem.
    // `canonicalize` would resolve `..` away and the path might then land
    // inside a root, which is exactly the traversal this rejects.
    if candidate
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(format!("路徑「{raw}」含有 `..`，不接受相對上層路徑，請改用完整路徑。"));
    }

    let roots = allowed_roots(home_dir, agent_id);
    // Canonicalize resolves symlinks (so a symlink pointing outside a root is
    // caught by the containment test below) and proves the file exists.
    let real = candidate
        .canonicalize()
        .map_err(|e| format!("路徑「{raw}」無法讀取：{e}"))?;

    let contained = roots.iter().any(|root| {
        // Canonicalize the roots too — on macOS `/var` and `/tmp` are
        // themselves symlinks, so an uncanonicalized prefix test would reject
        // legitimate paths on a developer machine.
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        real.starts_with(&root)
    });
    if !contained {
        return Err(format!(
            "路徑「{raw}」不在允許的目錄範圍內。允許的根目錄：{}。如需讀取其他位置，請在 config.toml 的 [files] allowed_roots 加入該目錄。",
            render_roots(&roots)
        ));
    }

    let meta = std::fs::metadata(&real).map_err(|e| format!("路徑「{raw}」無法讀取：{e}"))?;
    if !meta.is_file() {
        return Err(format!("路徑「{raw}」不是一般檔案。"));
    }
    Ok(real)
}

/// `table` for a result: the file's basename **with** its extension, because
/// that is the name a `db_field` rule writes (`customers.csv.name`).
fn table_name(path: &Path) -> String {
    path.file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_string()
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

fn too_large(raw: &str, len: u64, cap: u64) -> String {
    format!(
        "檔案「{raw}」為 {len} bytes，超過此工具的上限 {cap} bytes，拒絕讀取。"
    )
}

// ── Argument parsing ────────────────────────────────────────────────────────

fn arg_str<'a>(arguments: &'a Value, key: &str) -> &'a str {
    arguments.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// `limit` / `offset` accept a JSON number or a numeric string (some runtimes
/// stringify every tool argument). Out-of-range values clamp rather than
/// error — a model asking for 10_000 rows wants "as many as you'll give me".
fn arg_usize(arguments: &Value, key: &str) -> Option<usize> {
    match arguments.get(key) {
        Some(Value::Number(n)) => n.as_u64().map(|v| v as usize),
        Some(Value::String(s)) => s.trim().parse::<usize>().ok(),
        _ => None,
    }
}

fn arg_bool(arguments: &Value, key: &str) -> Option<bool> {
    match arguments.get(key) {
        Some(Value::Bool(b)) => Some(*b),
        Some(Value::String(s)) => match s.trim().to_ascii_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

fn resolve_limit(arguments: &Value) -> usize {
    arg_usize(arguments, "limit")
        .unwrap_or(ROW_LIMIT_DEFAULT)
        .clamp(1, ROW_LIMIT_MAX)
}

fn resolve_offset(arguments: &Value) -> usize {
    arg_usize(arguments, "offset").unwrap_or(0)
}

/// `delimiter` is one byte. `\t` / `tab` are spelled out because a literal tab
/// character does not survive most JSON-authoring paths intact.
fn resolve_delimiter(arguments: &Value) -> Result<u8, String> {
    let raw = arg_str(arguments, "delimiter");
    if raw.is_empty() {
        return Ok(b',');
    }
    match raw {
        "\\t" | "\t" | "tab" | "TAB" => return Ok(b'\t'),
        _ => {}
    }
    let bytes = raw.as_bytes();
    if bytes.len() != 1 || !bytes[0].is_ascii() {
        return Err(format!(
            "delimiter 必須是單一 ASCII 字元（或 \\t 代表 tab），收到「{raw}」。"
        ));
    }
    Ok(bytes[0])
}

/// Turn a raw header row into unique, non-empty JSON object keys.
///
/// A spreadsheet's header row is customer-authored: blanks and repeats are
/// normal. Both must be repaired here, because the rows are emitted as JSON
/// objects and a duplicate key would silently drop a whole column — the kind
/// of data loss that looks like a redaction bug later.
fn normalize_columns(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(raw.len());
    for (idx, name) in raw.iter().enumerate() {
        let trimmed = name.trim();
        let base = if trimmed.is_empty() {
            format!("c{}", idx + 1)
        } else {
            trimmed.to_string()
        };
        let mut candidate = base.clone();
        let mut suffix = idx + 1;
        while out.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        out.push(candidate);
    }
    out
}

fn synthetic_columns(width: usize) -> Vec<String> {
    (1..=width).map(|i| format!("c{i}")).collect()
}

/// Build one row object. Missing trailing cells become `""` (CSV) / `null`
/// (xlsx, whose `Data::Empty` already means "no value") so every row carries
/// the same key set — a stable shape is what makes a JsonPath field rule
/// (`$.rows[*].name`) match reliably.
fn row_object(columns: &[String], cells: Vec<Value>, missing: fn() -> Value) -> Value {
    let mut obj = serde_json::Map::with_capacity(columns.len().max(cells.len()));
    let mut cells = cells.into_iter();
    for column in columns {
        obj.insert(column.clone(), cells.next().unwrap_or_else(missing));
    }
    // Cells past the header width still have to reach the model (and the
    // redaction rules) — name them positionally rather than dropping them.
    for (extra_idx, cell) in cells.enumerate() {
        let key = format!("c{}", columns.len() + extra_idx + 1);
        obj.insert(key, cell);
    }
    Value::Object(obj)
}

fn empty_string() -> Value {
    Value::String(String::new())
}

fn json_null() -> Value {
    Value::Null
}

// ── file_read ───────────────────────────────────────────────────────────────

/// Read a plain-text file (txt / md / json / log / yaml …), capped at
/// [`FILE_READ_MAX_BYTES`].
pub fn handle_file_read(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let raw_path = arg_str(arguments, "path");
    let path = match vet_path(raw_path, home_dir, agent_id) {
        Ok(p) => p,
        Err(msg) => {
            audit_refusal(home_dir, agent_id, "file_read", raw_path);
            return files_error(&msg);
        }
    };
    if let Some(ext) = structured_extension(&path) {
        audit_refusal(home_dir, agent_id, "file_read", raw_path);
        return files_error(&format!(
            "「{raw_path}」是 .{ext} 結構化資料檔，file_read 只讀純文字。請改用 {}——只有結構化讀取才能讓「資料表.欄位」規則生效。",
            if ext == "csv" || ext == "tsv" {
                "csv_read"
            } else {
                "xlsx_read"
            }
        ));
    }
    let max_bytes = arg_usize(arguments, "max_bytes")
        .map(|v| (v as u64).min(FILE_READ_MAX_BYTES))
        .unwrap_or(FILE_READ_MAX_BYTES)
        .max(1);

    let total = file_len(&path);
    let buf = match read_prefix(&path, max_bytes) {
        Ok(b) => b,
        Err(e) => return files_error(&format!("讀取「{raw_path}」失敗：{e}")),
    };
    let truncated = total > buf.len() as u64;
    // Decode only the valid UTF-8 prefix: a byte-capped read lands mid-char on
    // any CJK file, and `from_utf8_lossy` would emit a U+FFFD for it.
    let text = match std::str::from_utf8(&buf) {
        Ok(s) => s.to_string(),
        Err(e) => String::from_utf8_lossy(&buf[..e.valid_up_to()]).into_owned(),
    };
    let table = table_name(&path);
    let payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "text": text,
        "truncated": truncated,
    });
    audit(
        home_dir,
        agent_id,
        "file_read",
        &path,
        &table,
        None,
        true,
    );
    files_json(&payload)
}

/// The file's extension when it is one `file_read` must not swallow.
fn structured_extension(path: &Path) -> Option<String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    STRUCTURED_EXTENSIONS
        .contains(&ext.as_str())
        .then_some(ext)
}

/// Read at most `max_bytes` from `path` without materializing the whole file.
fn read_prefix(path: &Path, max_bytes: u64) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(path)?;
    let mut buf = Vec::new();
    file.take(max_bytes).read_to_end(&mut buf)?;
    Ok(buf)
}

// ── csv_read ────────────────────────────────────────────────────────────────

/// Read a delimited text file into `{columns, rows, row_count, truncated}`.
///
/// Cells are always strings: a CSV has no types, and guessing them (is
/// `0912345678` a number or a phone?) would both mangle data and hand the
/// redaction rules a shape that changes per file.
pub fn handle_csv_read(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    let raw_path = arg_str(arguments, "path");
    let path = match vet_path(raw_path, home_dir, agent_id) {
        Ok(p) => p,
        Err(msg) => {
            audit_refusal(home_dir, agent_id, "csv_read", raw_path);
            return files_error(&msg);
        }
    };
    let len = file_len(&path);
    if len > CSV_MAX_FILE_BYTES {
        audit_refusal(home_dir, agent_id, "csv_read", raw_path);
        return files_error(&too_large(raw_path, len, CSV_MAX_FILE_BYTES));
    }
    let delimiter = match resolve_delimiter(arguments) {
        Ok(d) => d,
        Err(msg) => return files_error(&msg),
    };
    let has_header = arg_bool(arguments, "has_header").unwrap_or(true);
    let limit = resolve_limit(arguments);
    let offset = resolve_offset(arguments);

    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) => return files_error(&format!("讀取「{raw_path}」失敗：{e}")),
    };
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        // Headers are handled here, not by the csv crate, so the no-header
        // case can synthesize `c1..cN` from the first data record.
        .has_headers(false)
        // Ragged rows are normal in exported spreadsheets; refusing the whole
        // file over one short line would be the wrong trade.
        .flexible(true)
        .from_reader(file);

    let mut records = reader.records();
    let mut columns: Vec<String> = Vec::new();
    if has_header {
        match records.next() {
            Some(Ok(rec)) => {
                columns = normalize_columns(
                    &rec.iter().map(str::to_string).collect::<Vec<_>>(),
                );
            }
            Some(Err(e)) => {
                return files_error(&format!("解析「{raw_path}」的表頭失敗：{e}"));
            }
            None => {
                // Empty file: an honest empty result, not an error.
                return empty_table_result(home_dir, agent_id, "csv_read", &path, None);
            }
        }
    }

    let mut rows: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut skipped = 0usize;
    for record in records {
        let record = match record {
            Ok(r) => r,
            Err(e) => return files_error(&format!("解析「{raw_path}」第 {} 列失敗：{e}", skipped + rows.len() + 1)),
        };
        if skipped < offset {
            skipped += 1;
            continue;
        }
        if rows.len() >= limit {
            truncated = true;
            break;
        }
        if columns.is_empty() {
            columns = synthetic_columns(record.len());
        }
        let cells: Vec<Value> = record
            .iter()
            .map(|c| Value::String(c.to_string()))
            .collect();
        rows.push(row_object(&columns, cells, empty_string));
    }

    let table = table_name(&path);
    let row_count = rows.len();
    let payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "columns": columns,
        "rows": rows,
        "row_count": row_count,
        "truncated": truncated,
    });
    audit(
        home_dir,
        agent_id,
        "csv_read",
        &path,
        &table,
        Some(row_count),
        true,
    );
    files_json(&payload)
}

fn empty_table_result(
    home_dir: &Path,
    agent_id: &str,
    tool: &str,
    path: &Path,
    sheet: Option<(&str, Vec<String>)>,
) -> Value {
    let table = table_name(path);
    let mut payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "columns": [],
        "rows": [],
        "row_count": 0,
        "truncated": false,
    });
    if let Some((name, sheets)) = sheet
        && let Some(obj) = payload.as_object_mut()
    {
        obj.insert("sheet".into(), json!(name));
        obj.insert("sheets".into(), json!(sheets));
    }
    audit(home_dir, agent_id, tool, path, &table, Some(0), true);
    files_json(&payload)
}

// ── xlsx_read ───────────────────────────────────────────────────────────────

/// Read one worksheet of a workbook (xlsx / xlsm / xls / ods) into the same
/// shape `csv_read` returns, plus `sheet` and `sheets`.
pub fn handle_xlsx_read(arguments: &Value, home_dir: &Path, agent_id: &str) -> Value {
    use calamine::Reader;

    let raw_path = arg_str(arguments, "path");
    let path = match vet_path(raw_path, home_dir, agent_id) {
        Ok(p) => p,
        Err(msg) => {
            audit_refusal(home_dir, agent_id, "xlsx_read", raw_path);
            return files_error(&msg);
        }
    };
    let len = file_len(&path);
    if len > XLSX_MAX_FILE_BYTES {
        audit_refusal(home_dir, agent_id, "xlsx_read", raw_path);
        return files_error(&too_large(raw_path, len, XLSX_MAX_FILE_BYTES));
    }
    let limit = resolve_limit(arguments);
    let offset = resolve_offset(arguments);

    let mut workbook = match calamine::open_workbook_auto(&path) {
        Ok(w) => w,
        Err(e) => {
            return files_error(&format!(
                "無法開啟「{raw_path}」：{e}。支援 xlsx／xlsm／xls／ods。"
            ));
        }
    };
    let sheets = workbook.sheet_names().to_vec();
    if sheets.is_empty() {
        return files_error(&format!("「{raw_path}」沒有任何工作表。"));
    }
    let requested = arg_str(arguments, "sheet").trim().to_string();
    let sheet = if requested.is_empty() {
        sheets[0].clone()
    } else {
        match sheets.iter().find(|s| s.as_str() == requested) {
            Some(s) => s.clone(),
            None => {
                return files_error(&format!(
                    "「{raw_path}」沒有名為「{requested}」的工作表。可用的工作表：{}。",
                    sheets.join("、")
                ));
            }
        }
    };
    let range = match workbook.worksheet_range(&sheet) {
        Ok(r) => r,
        Err(e) => return files_error(&format!("讀取工作表「{sheet}」失敗：{e}")),
    };

    let mut iter = range.rows();
    let header = match iter.next() {
        Some(h) => h,
        None => {
            return empty_table_result(
                home_dir,
                agent_id,
                "xlsx_read",
                &path,
                Some((&sheet, sheets)),
            );
        }
    };
    let columns = normalize_columns(
        &header
            .iter()
            .map(cell_to_header_string)
            .collect::<Vec<_>>(),
    );

    let mut rows: Vec<Value> = Vec::new();
    let mut truncated = false;
    let mut skipped = 0usize;
    for record in iter {
        if skipped < offset {
            skipped += 1;
            continue;
        }
        if rows.len() >= limit {
            truncated = true;
            break;
        }
        let cells: Vec<Value> = record.iter().map(cell_to_json).collect();
        rows.push(row_object(&columns, cells, json_null));
    }

    let table = table_name(&path);
    let row_count = rows.len();
    let payload = json!({
        "path": path.to_string_lossy(),
        "table": table,
        "sheet": sheet,
        "sheets": sheets,
        "columns": columns,
        "rows": rows,
        "row_count": row_count,
        "truncated": truncated,
    });
    audit(
        home_dir,
        agent_id,
        "xlsx_read",
        &path,
        &table,
        Some(row_count),
        true,
    );
    files_json(&payload)
}

/// §14.2 cell mapping: number → number, bool → bool, empty → null,
/// date/time → ISO 8601 string, everything else → string.
pub fn cell_to_json(cell: &calamine::Data) -> Value {
    use calamine::Data;
    match cell {
        Data::Int(i) => json!(i),
        Data::Float(f) => {
            // A worksheet stores every number as f64; an integral one reads
            // far better as `3` than `3.0` on the wire, and a JsonPath rule
            // comparing an id column should see an integer.
            if f.fract() == 0.0 && f.abs() < 9.0e15 {
                json!(*f as i64)
            } else {
                json!(f)
            }
        }
        Data::Bool(b) => json!(b),
        Data::Empty => Value::Null,
        Data::String(s) => json!(s),
        Data::DateTimeIso(s) => json!(s),
        Data::DurationIso(s) => json!(s),
        Data::DateTime(dt) => match dt.as_datetime() {
            // Seconds precision: a serial-date round-trip has sub-second
            // float noise that is never real data.
            Some(ndt) => json!(ndt.format("%Y-%m-%dT%H:%M:%S").to_string()),
            None => json!(dt.as_f64()),
        },
        Data::Error(e) => json!(format!("#ERROR:{e:?}")),
    }
}

/// Header cells are always rendered as text — a numeric header is still a
/// column name, and `normalize_columns` needs a `String`.
fn cell_to_header_string(cell: &calamine::Data) -> String {
    match cell_to_json(cell) {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

// ── Audit ───────────────────────────────────────────────────────────────────

/// A refused read still gets a row: an agent reaching for a path outside the
/// fence, or for a file over the cap, is exactly the thing an operator wants
/// to be able to see afterwards. The REQUESTED path is recorded (there is no
/// canonical one when the fence rejected it) and nothing else — a refusal
/// never read any content to leak.
fn audit_refusal(home_dir: &Path, agent_id: &str, tool: &str, requested: &str) {
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir,
        agent_id,
        tool,
        "refused",
        false,
        &[("path", json!(requested)), ("refused", json!(true))],
    );
}

/// One audit row per call. Records the read's SHAPE only — path, table name,
/// and how many rows came back. Cell content never goes to `tool_calls.jsonl`.
fn audit(
    home_dir: &Path,
    agent_id: &str,
    tool: &str,
    path: &Path,
    table: &str,
    row_count: Option<usize>,
    success: bool,
) {
    let mut extras: Vec<(&str, Value)> = vec![
        ("path", json!(path.to_string_lossy())),
        ("table", json!(table)),
    ];
    if let Some(n) = row_count {
        extras.push(("row_count", json!(n)));
    }
    let summary = match row_count {
        Some(n) => format!("table={table} rows={n}"),
        None => format!("table={table}"),
    };
    duduclaw_security::audit::append_tool_call_with_extras(
        home_dir, agent_id, tool, &summary, success, &extras,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_home() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn agent_dir(home: &Path, id: &str) -> PathBuf {
        let dir = home.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn payload(v: &Value) -> Value {
        let text = v
            .pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("{}");
        serde_json::from_str(text).unwrap_or(Value::Null)
    }

    fn is_error(v: &Value) -> bool {
        v.get("isError").and_then(|b| b.as_bool()).unwrap_or(false)
    }

    fn err_text(v: &Value) -> String {
        v.pointer("/content/0/text")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string()
    }

    // ── Path fence ──────────────────────────────────────────────────────────

    #[test]
    fn vet_path_accepts_a_file_inside_the_agent_dir() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("customers.csv");
        std::fs::write(&file, "id,name\n1,Amy\n").unwrap();
        let ok = vet_path(file.to_str().unwrap(), home.path(), "sales").unwrap();
        assert!(ok.ends_with("customers.csv"));
    }

    #[test]
    fn vet_path_accepts_the_home_attachments_dir() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let att = home.path().join("attachments");
        std::fs::create_dir_all(&att).unwrap();
        let file = att.join("report.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        assert!(vet_path(file.to_str().unwrap(), home.path(), "sales").is_ok());
    }

    #[test]
    fn vet_path_refuses_a_path_outside_the_roots_and_names_them() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("secret.csv");
        std::fs::write(&file, "x\n").unwrap();
        let err = vet_path(file.to_str().unwrap(), home.path(), "sales").unwrap_err();
        assert!(err.contains("不在允許的目錄範圍內"), "{err}");
        assert!(err.contains("agents"), "root list must be reported: {err}");
    }

    #[test]
    fn vet_path_refuses_parent_dir_components() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let traversal = format!("{}/../../../etc/hosts", dir.display());
        let err = vet_path(&traversal, home.path(), "sales").unwrap_err();
        assert!(err.contains(".."), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn vet_path_refuses_a_symlink_escaping_the_roots() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("payroll.csv");
        std::fs::write(&target, "x\n").unwrap();
        let link = dir.join("innocent.csv");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let err = vet_path(link.to_str().unwrap(), home.path(), "sales").unwrap_err();
        assert!(err.contains("不在允許的目錄範圍內"), "{err}");
    }

    #[test]
    fn vet_path_refuses_a_directory() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let err = vet_path(dir.to_str().unwrap(), home.path(), "sales").unwrap_err();
        assert!(err.contains("不是一般檔案"), "{err}");
    }

    #[test]
    fn configured_roots_widen_the_fence() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let extra = tempfile::tempdir().unwrap();
        let canonical = extra.path().canonicalize().unwrap();
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[files]\nallowed_roots = [\"{}\"]\n",
                canonical.display().to_string().replace('\\', "\\\\")
            ),
        )
        .unwrap();
        let file = canonical.join("exports.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        assert!(vet_path(file.to_str().unwrap(), home.path(), "sales").is_ok());
    }

    #[test]
    fn relative_configured_roots_are_ignored() {
        let home = tmp_home();
        std::fs::write(
            home.path().join("config.toml"),
            "[files]\nallowed_roots = [\"relative/dir\", \"\"]\n",
        )
        .unwrap();
        assert!(configured_roots(home.path()).is_empty());
    }

    #[test]
    fn an_invalid_agent_id_gets_no_agent_roots() {
        let home = tmp_home();
        let roots = allowed_roots(home.path(), "../evil");
        assert_eq!(roots.len(), 1, "only <home>/attachments: {roots:?}");
        assert!(roots[0].ends_with("attachments"));
    }

    // ── csv_read ────────────────────────────────────────────────────────────

    #[test]
    fn csv_read_returns_header_keyed_rows() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("customers.csv");
        std::fs::write(&file, "id,name,email\n1,王小明,a@b.c\n2,李小華,d@e.f\n").unwrap();

        let out = handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(!is_error(&out));
        let p = payload(&out);
        assert_eq!(p["table"], "customers.csv");
        assert_eq!(p["columns"], json!(["id", "name", "email"]));
        assert_eq!(p["row_count"], 2);
        assert_eq!(p["truncated"], false);
        assert_eq!(p["rows"][0]["name"], "王小明");
        assert_eq!(p["rows"][1]["email"], "d@e.f");
        // Cells are always strings — never coerced.
        assert!(p["rows"][0]["id"].is_string());
    }

    #[test]
    fn csv_read_synthesizes_columns_without_a_header() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("raw.csv");
        std::fs::write(&file, "1,Amy\n2,Bob\n").unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap(), "has_header": false }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["columns"], json!(["c1", "c2"]));
        assert_eq!(p["rows"][0]["c2"], "Amy");
        assert_eq!(p["row_count"], 2);
    }

    #[test]
    fn csv_read_honours_limit_offset_and_reports_truncation() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("many.csv");
        let mut body = String::from("n\n");
        for i in 0..10 {
            body.push_str(&format!("{i}\n"));
        }
        std::fs::write(&file, body).unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap(), "limit": 3, "offset": 2 }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["row_count"], 3);
        assert_eq!(p["rows"][0]["n"], "2");
        assert_eq!(p["truncated"], true);
    }

    #[test]
    fn csv_read_limit_is_clamped_to_the_maximum() {
        let args = json!({ "limit": 99_999 });
        assert_eq!(resolve_limit(&args), ROW_LIMIT_MAX);
        assert_eq!(resolve_limit(&json!({})), ROW_LIMIT_DEFAULT);
        assert_eq!(resolve_limit(&json!({ "limit": "5" })), 5);
    }

    #[test]
    fn csv_read_accepts_a_tab_delimiter() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("data.tsv");
        std::fs::write(&file, "a\tb\n1\t2\n").unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap(), "delimiter": "\\t" }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["columns"], json!(["a", "b"]));
        assert_eq!(p["rows"][0]["b"], "2");
    }

    #[test]
    fn csv_read_rejects_a_multi_char_delimiter() {
        assert!(resolve_delimiter(&json!({ "delimiter": "||" })).is_err());
        assert_eq!(resolve_delimiter(&json!({})).unwrap(), b',');
    }

    #[test]
    fn csv_read_refuses_a_path_outside_the_roots() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("payroll.csv");
        std::fs::write(&file, "a\n1\n").unwrap();
        let out = handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(is_error(&out));
        assert!(err_text(&out).contains("不在允許的目錄範圍內"));
    }

    #[test]
    fn csv_read_on_an_empty_file_is_an_empty_result_not_an_error() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("empty.csv");
        std::fs::write(&file, "").unwrap();
        let out = handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(!is_error(&out));
        assert_eq!(payload(&out)["row_count"], 0);
    }

    #[test]
    fn duplicate_and_blank_headers_never_collide() {
        let cols = normalize_columns(&[
            "name".into(),
            "".into(),
            "name".into(),
            "  ".into(),
        ]);
        assert_eq!(cols.len(), 4);
        let unique: std::collections::HashSet<_> = cols.iter().collect();
        assert_eq!(unique.len(), 4, "columns must be unique: {cols:?}");
        assert_eq!(cols[0], "name");
        assert_eq!(cols[1], "c2");
    }

    #[test]
    fn ragged_rows_keep_the_full_column_key_set() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("ragged.csv");
        std::fs::write(&file, "a,b,c\n1\n1,2,3,4\n").unwrap();
        let p = payload(&handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["rows"][0]["b"], "");
        assert_eq!(p["rows"][1]["c4"], "4");
    }

    // ── file_read ───────────────────────────────────────────────────────────

    #[test]
    fn file_read_returns_text_and_truncation_state() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("notes.md");
        std::fs::write(&file, "# 標題\n內容\n").unwrap();
        let p = payload(&handle_file_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["table"], "notes.md");
        assert_eq!(p["text"], "# 標題\n內容\n");
        assert_eq!(p["truncated"], false);
    }

    #[test]
    fn file_read_truncates_on_a_char_boundary() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("cjk.txt");
        // Every char is 3 bytes; a 4-byte cap lands mid-char.
        std::fs::write(&file, "王小明測試").unwrap();
        let p = payload(&handle_file_read(
            &json!({ "path": file.to_str().unwrap(), "max_bytes": 4 }),
            home.path(),
            "sales",
        ));
        assert_eq!(p["text"], "王", "must cut back to a char boundary");
        assert_eq!(p["truncated"], true);
    }

    #[test]
    fn file_read_refuses_a_structured_data_file_and_names_the_right_tool() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        for (name, expected_tool) in [
            ("customers.csv", "csv_read"),
            ("a.TSV", "csv_read"),
            ("客戶清單.xlsx", "xlsx_read"),
            ("legacy.xls", "xlsx_read"),
            ("open.ods", "xlsx_read"),
        ] {
            let file = dir.join(name);
            std::fs::write(&file, "a,b\n1,2\n").unwrap();
            let out = handle_file_read(
                &json!({ "path": file.to_str().unwrap() }),
                home.path(),
                "sales",
            );
            assert!(is_error(&out), "{name} must be refused");
            assert!(
                err_text(&out).contains(expected_tool),
                "{name} should point at {expected_tool}: {}",
                err_text(&out)
            );
        }
        // Plain text is unaffected.
        let ok = dir.join("readme.txt");
        std::fs::write(&ok, "hello").unwrap();
        assert!(!is_error(&handle_file_read(
            &json!({ "path": ok.to_str().unwrap() }),
            home.path(),
            "sales"
        )));
    }

    #[test]
    fn file_read_max_bytes_cannot_exceed_the_cap() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("big.txt");
        std::fs::write(&file, vec![b'x'; (FILE_READ_MAX_BYTES + 1024) as usize]).unwrap();
        let p = payload(&handle_file_read(
            &json!({ "path": file.to_str().unwrap(), "max_bytes": 99_999_999u64 }),
            home.path(),
            "sales",
        ));
        assert_eq!(
            p["text"].as_str().unwrap().len(),
            FILE_READ_MAX_BYTES as usize
        );
        assert_eq!(p["truncated"], true);
    }

    // ── xlsx_read ───────────────────────────────────────────────────────────

    #[test]
    fn xlsx_read_refuses_a_path_outside_the_roots() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("payroll.xlsx");
        std::fs::write(&file, b"PK\x03\x04").unwrap();
        let out = handle_xlsx_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(is_error(&out));
    }

    #[test]
    fn xlsx_read_reports_a_non_workbook_honestly() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("not-a-workbook.xlsx");
        std::fs::write(&file, b"this is not a zip").unwrap();
        let out = handle_xlsx_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );
        assert!(is_error(&out));
        assert!(err_text(&out).contains("無法開啟"), "{}", err_text(&out));
    }

    // ── Audit ───────────────────────────────────────────────────────────────

    fn audit_rows(home: &Path) -> Vec<Value> {
        let raw = std::fs::read_to_string(home.join("tool_calls.jsonl")).unwrap_or_default();
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(|l| serde_json::from_str::<Value>(l).ok())
            .collect()
    }

    #[test]
    fn a_successful_read_audits_its_shape_and_never_a_cell() {
        let home = tmp_home();
        let dir = agent_dir(home.path(), "sales");
        let file = dir.join("customers.csv");
        std::fs::write(&file, "id,name\n1,王小明\n").unwrap();

        handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );

        let rows = audit_rows(home.path());
        assert_eq!(rows.len(), 1, "one row per call: {rows:?}");
        let row = &rows[0];
        assert_eq!(row["tool_name"], "csv_read");
        assert_eq!(row["agent_id"], "sales");
        assert_eq!(row["table"], "customers.csv");
        assert_eq!(row["row_count"], 1);
        assert_eq!(row["success"], true);
        let whole = serde_json::to_string(row).unwrap();
        assert!(
            !whole.contains("王小明"),
            "cell content must never reach tool_calls.jsonl: {whole}"
        );
    }

    #[test]
    fn a_refused_read_is_audited_as_a_failure() {
        let home = tmp_home();
        agent_dir(home.path(), "sales");
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("payroll.csv");
        std::fs::write(&file, "salary\n999\n").unwrap();

        handle_csv_read(
            &json!({ "path": file.to_str().unwrap() }),
            home.path(),
            "sales",
        );

        let rows = audit_rows(home.path());
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["success"], false);
        assert_eq!(rows[0]["refused"], true);
        assert_eq!(rows[0]["tool_name"], "csv_read");
    }

    #[test]
    fn cell_mapping_follows_the_contract() {
        use calamine::{Data, ExcelDateTime, ExcelDateTimeType};
        assert_eq!(cell_to_json(&Data::Int(7)), json!(7));
        assert_eq!(cell_to_json(&Data::Float(1.5)), json!(1.5));
        // Integral floats read back as integers (a worksheet has no int type).
        assert_eq!(cell_to_json(&Data::Float(3.0)), json!(3));
        assert_eq!(cell_to_json(&Data::Bool(true)), json!(true));
        assert_eq!(cell_to_json(&Data::Empty), Value::Null);
        assert_eq!(cell_to_json(&Data::String("台北".into())), json!("台北"));
        assert_eq!(
            cell_to_json(&Data::DateTimeIso("2026-09-22T10:00:00".into())),
            json!("2026-09-22T10:00:00")
        );
        // Serial 45000 is 2023-03-15 in the 1900 date system.
        let dt = Data::DateTime(ExcelDateTime::new(
            45000.0,
            ExcelDateTimeType::DateTime,
            false,
        ));
        let rendered = cell_to_json(&dt);
        let s = rendered.as_str().expect("ISO string");
        assert!(s.starts_with("2023-03-15T"), "got {s}");
    }
}
