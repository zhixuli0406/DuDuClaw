//! The install report every importer appends to: honest per-item lines,
//! rendered on the console and, for the dashboard, as JSON
//! (`pack install --report-json`). Split out of `mod.rs` (file-size ceiling).

use std::path::Path;

use console::style;

use duduclaw_core::error::Result;

use super::{cfg_err, io_err};

// ─────────────────────────── Report ───────────────────────────

/// Status of one planned/applied item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ItemStatus {
    Imported,
    Skipped,
    Conflict,
    Warning,
    Ignored,
}

#[derive(Debug, Clone)]
struct Item {
    status: ItemStatus,
    kind: String,
    name: String,
    detail: String,
    /// Stable message code + parameters the dashboard renders in the
    /// viewer's language (`experts.report.msg.<code>`); `detail` stays the
    /// zh-TW console text and the fallback.
    code: Option<&'static str>,
    params: serde_json::Value,
}

/// A running import report — honest, never silently drops.
#[derive(Debug, Default)]
pub(super) struct Report {
    items: Vec<Item>,
    /// Summary fields for the machine-readable report (`slug`, `format`,
    /// `disabled_skills`, …).
    meta: std::collections::BTreeMap<String, String>,
    /// Structured extras (`replace`: what a replace moves away, …).
    details: std::collections::BTreeMap<String, serde_json::Value>,
    /// Set when the run failed: `{code, params, message}`.
    error: Option<serde_json::Value>,
}

impl ItemStatus {
    pub(super) fn code(self) -> &'static str {
        match self {
            ItemStatus::Imported => "imported",
            ItemStatus::Skipped => "skipped",
            ItemStatus::Conflict => "conflict",
            ItemStatus::Warning => "warning",
            ItemStatus::Ignored => "ignored",
        }
    }
}

impl Report {
    pub(super) fn meta(&mut self, key: &str, value: &str) {
        self.meta.insert(key.to_string(), value.to_string());
    }

    pub(super) fn detail(&mut self, key: &str, value: serde_json::Value) {
        self.details.insert(key.to_string(), value);
    }

    pub(super) fn set_error(&mut self, error: serde_json::Value) {
        self.error = Some(error);
    }

    /// Employee-level lines that kept an employee out (skipped / conflict),
    /// for the "nothing was imported" error: each one's name, reason code
    /// and parameters, so the dashboard can say why per employee.
    pub(super) fn agent_problems(&self) -> Vec<serde_json::Value> {
        self.items
            .iter()
            .filter(|i| i.kind == "agent" && matches!(i.status, ItemStatus::Skipped | ItemStatus::Conflict))
            .map(|i| {
                serde_json::json!({
                    "name": i.name,
                    "status": i.status.code(),
                    "code": i.code,
                    "params": i.params,
                    "detail": i.detail,
                })
            })
            .collect()
    }

    /// The report as JSON for the dashboard (`pack install --report-json`).
    /// `kind` / `status` are stable codes the UI translates; `name` /
    /// `detail` are already human-readable zh-TW.
    pub(super) fn to_json(&self, dry_run: bool) -> serde_json::Value {
        let count = |st: ItemStatus| self.items.iter().filter(|i| i.status == st).count();
        serde_json::json!({
            "dry_run": dry_run,
            "meta": self.meta,
            "details": self.details,
            "error": self.error,
            "counts": {
                "imported": count(ItemStatus::Imported),
                "skipped": count(ItemStatus::Skipped),
                "conflict": count(ItemStatus::Conflict),
                "warning": count(ItemStatus::Warning),
                "ignored": count(ItemStatus::Ignored),
            },
            "items": self.items.iter().map(|i| serde_json::json!({
                "status": i.status.code(),
                "kind": i.kind,
                "name": i.name,
                "detail": i.detail,
                "code": i.code,
                "params": i.params,
            })).collect::<Vec<_>>(),
        })
    }

    pub(super) fn add(&mut self, status: ItemStatus, kind: &str, name: &str, detail: impl Into<String>) {
        self.items.push(Item {
            status,
            kind: kind.to_string(),
            name: name.to_string(),
            detail: detail.into(),
            code: None,
            params: serde_json::Value::Null,
        });
    }

    /// Attach a message code and its parameters to the line just added.
    pub(super) fn tag(&mut self, code: &'static str, params: serde_json::Value) {
        if let Some(last) = self.items.last_mut() {
            last.code = Some(code);
            last.params = params;
        }
    }
    pub(super) fn imported(&mut self, kind: &str, name: &str) {
        self.add(ItemStatus::Imported, kind, name, "");
    }
    pub(super) fn skipped(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Skipped, kind, name, detail);
    }
    pub(super) fn conflict(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Conflict, kind, name, detail);
    }
    pub(super) fn warning(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Warning, kind, name, detail);
    }
    pub(super) fn ignored(&mut self, kind: &str, name: &str, detail: impl Into<String>) {
        self.add(ItemStatus::Ignored, kind, name, detail);
    }

    pub(super) fn render_console(&self, dry_run: bool) {
        let header = if dry_run {
            "安裝計畫（--dry-run，未寫入）"
        } else {
            "安裝結果"
        };
        println!("\n  {}\n", style(header).bold());
        for it in &self.items {
            let (mark, tag) = match it.status {
                ItemStatus::Imported => (style("✓").green(), style("匯入").green()),
                ItemStatus::Skipped => (style("–").yellow(), style("略過").yellow()),
                ItemStatus::Conflict => (style("!").red(), style("衝突").red()),
                ItemStatus::Warning => (style("⚠").yellow(), style("警告").yellow()),
                ItemStatus::Ignored => (style("·").dim(), style("忽略").dim()),
            };
            let detail = if it.detail.is_empty() {
                String::new()
            } else {
                format!(" — {}", it.detail)
            };
            println!("  {mark} [{tag}] {}: {}{detail}", it.kind, it.name);
        }
        println!();
    }
}


/// A fresh `.json` file for the report (`create_new`: never follows or
/// replaces an existing file).
pub(super) fn open_report_file(path: &Path) -> Result<std::fs::File> {
    let is_json = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("json"))
        .unwrap_or(false);
    if !is_json {
        return Err(cfg_err("--report-json 必須是 .json 檔".into()));
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| io_err(format!("無法建立匯入報告 {}（檔案不能已存在）：{e}", path.display())))
}


pub(super) fn report_zip_summary(z: &super::safe_zip::ExtractSummary, report: &mut Report) {
    if z.junk_skipped > 0 {
        report.ignored(
            "zip",
            "",
            format!("已略過 {} 個系統暫存檔（__MACOSX、.DS_Store 等）", z.junk_skipped),
        );
        report.tag("zip.junk", serde_json::json!({ "count": z.junk_skipped }));
    }
    if let Some(enc) = z.legacy_encoding {
        report.warning("zip", "", format!("壓縮檔的中文檔名使用 {}，已自動轉換", enc.label()));
        let code = match enc {
            super::zip_names::LegacyEncoding::Gbk => "gbk",
            super::zip_names::LegacyEncoding::Big5 => "big5",
        };
        report.tag("zip.legacy", serde_json::json!({ "encoding": code }));
    }
    for link in &z.links_skipped {
        report.skipped("zip", link, "壓縮檔內的捷徑（符號連結）不會解開");
        report.tag("zip.link", serde_json::json!({}));
    }
}

/// A report for a run that stopped before the pipeline had one (the front
/// door's source resolution, tier gate, …): just the error.
pub(crate) fn failure_report_json(dry_run: bool, e: &duduclaw_core::error::DuDuClawError) -> serde_json::Value {
    let mut r = Report::default();
    r.meta("error", &super::import_error::plain_message(e));
    r.set_error(super::import_error::error_json(e));
    r.to_json(dry_run)
}
