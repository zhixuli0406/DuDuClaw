//! The install report every importer appends to: honest per-item lines,
//! rendered on the console and, for the dashboard, as JSON
//! (`pack install --report-json`). Split out of `mod.rs` (file-size ceiling).

use console::style;

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
}

/// A running import report — honest, never silently drops.
#[derive(Debug, Default)]
pub(super) struct Report {
    items: Vec<Item>,
    /// Summary fields for the machine-readable report (`slug`, `format`,
    /// `disabled_skills`, …).
    meta: std::collections::BTreeMap<String, String>,
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

    /// The report as JSON for the dashboard (`pack install --report-json`).
    /// `kind` / `status` are stable codes the UI translates; `name` /
    /// `detail` are already human-readable zh-TW.
    pub(super) fn to_json(&self, dry_run: bool) -> serde_json::Value {
        let count = |st: ItemStatus| self.items.iter().filter(|i| i.status == st).count();
        serde_json::json!({
            "dry_run": dry_run,
            "meta": self.meta,
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
            })).collect::<Vec<_>>(),
        })
    }

    pub(super) fn add(&mut self, status: ItemStatus, kind: &str, name: &str, detail: impl Into<String>) {
        self.items.push(Item {
            status,
            kind: kind.to_string(),
            name: name.to_string(),
            detail: detail.into(),
        });
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

