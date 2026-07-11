//! Migration report model + rendering (console + markdown).
//!
//! Every imported item carries an honest per-item status — `IMPORTED`,
//! `PARTIAL`, `SKIPPED`, or `CONFLICT` — and the whole run rolls up to a
//! single verdict (`COMPLETE` / `DEGRADED` / `PARTIAL`). Nothing is ever
//! silently dropped: a parse failure or a fail-closed security block is
//! recorded as `SKIPPED(<reason>)`, never omitted.

use std::fmt::Write as _;

/// Per-item outcome. The `String` payloads are always a human-readable,
/// zh-TW reason so the report is actionable, not just a status flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Imported,
    Partial(String),
    Skipped(String),
    Conflict(String),
}

impl Status {
    /// Uppercase tag used in both console + markdown rendering.
    fn tag(&self) -> &'static str {
        match self {
            Status::Imported => "IMPORTED",
            Status::Partial(_) => "PARTIAL",
            Status::Skipped(_) => "SKIPPED",
            Status::Conflict(_) => "CONFLICT",
        }
    }

    fn reason(&self) -> Option<&str> {
        match self {
            Status::Imported => None,
            Status::Partial(r) | Status::Skipped(r) | Status::Conflict(r) => Some(r.as_str()),
        }
    }

    /// Lowercase status token for the JSON API contract (frontend-locked).
    fn json_str(&self) -> &'static str {
        match self {
            Status::Imported => "imported",
            Status::Partial(_) => "partial",
            Status::Skipped(_) => "skipped",
            Status::Conflict(_) => "conflict",
        }
    }
}

/// One line in the migration report.
#[derive(Debug, Clone)]
pub(crate) struct Item {
    /// Coarse bucket, e.g. `agent`, `memory`, `channel`, `cron`, `skill`.
    pub category: String,
    /// The specific thing (agent id, channel name, cron name, ...).
    pub name: String,
    pub status: Status,
}

/// A full migration report for one platform run.
#[derive(Debug, Clone)]
pub(crate) struct Report {
    pub platform: String,
    pub source: String,
    pub apply: bool,
    pub items: Vec<Item>,
    /// Extra freeform notes surfaced to the operator (v1 non-goals, hints).
    pub notes: Vec<String>,
}

impl Report {
    pub fn new(platform: &str, source: &str, apply: bool) -> Self {
        Report {
            platform: platform.to_string(),
            source: source.to_string(),
            apply,
            items: Vec::new(),
            notes: Vec::new(),
        }
    }

    pub fn push(&mut self, category: &str, name: &str, status: Status) {
        self.items.push(Item {
            category: category.to_string(),
            name: name.to_string(),
            status,
        });
    }

    pub fn imported(&mut self, category: &str, name: &str) {
        self.push(category, name, Status::Imported);
    }

    pub fn skipped(&mut self, category: &str, name: &str, reason: impl Into<String>) {
        self.push(category, name, Status::Skipped(reason.into()));
    }

    pub fn conflict(&mut self, category: &str, name: &str, reason: impl Into<String>) {
        self.push(category, name, Status::Conflict(reason.into()));
    }

    pub fn partial(&mut self, category: &str, name: &str, reason: impl Into<String>) {
        self.push(category, name, Status::Partial(reason.into()));
    }

    pub fn note(&mut self, note: impl Into<String>) {
        self.notes.push(note.into());
    }

    fn count(&self, tag: &str) -> usize {
        self.items.iter().filter(|i| i.status.tag() == tag).count()
    }

    /// Roll-up verdict.
    ///
    /// - `COMPLETE`  — everything imported cleanly (no skip/conflict/partial).
    /// - `PARTIAL`   — nothing was imported at all.
    /// - `DEGRADED`  — some imported, some not.
    pub fn overall(&self) -> &'static str {
        let imported = self.count("IMPORTED");
        let total = self.items.len();
        if total == 0 || imported == 0 {
            "PARTIAL"
        } else if imported == total {
            "COMPLETE"
        } else {
            "DEGRADED"
        }
    }

    /// Render the human-facing plan/result to stdout (zh-TW).
    pub fn render_console(&self) {
        use console::style;
        let mode = if self.apply {
            "套用 (--apply)"
        } else {
            "預覽 (dry-run)"
        };
        println!();
        println!(
            "  {} 從 {} 轉移到 DuDuClaw — {}",
            style("🐾").cyan(),
            style(&self.platform).bold(),
            style(mode).dim()
        );
        println!("  {} 來源: {}", style("→").cyan(), self.source);
        println!();

        for item in &self.items {
            let (icon, tag) = match &item.status {
                Status::Imported => (style("✓").green().to_string(), "IMPORTED"),
                Status::Partial(_) => (style("◐").yellow().to_string(), "PARTIAL"),
                Status::Skipped(_) => (style("−").dim().to_string(), "SKIPPED"),
                Status::Conflict(_) => (style("✗").red().to_string(), "CONFLICT"),
            };
            let reason = item
                .status
                .reason()
                .map(|r| format!(" — {r}"))
                .unwrap_or_default();
            println!(
                "  {icon} [{}] {} {}{}",
                item.category,
                item.name,
                style(tag).bold(),
                style(reason).dim()
            );
        }

        if !self.notes.is_empty() {
            println!();
            println!("  {}", style("備註").bold());
            for note in &self.notes {
                println!("    {} {}", style("•").dim(), note);
            }
        }

        println!();
        let verdict = self.overall();
        let styled = match verdict {
            "COMPLETE" => style(verdict).green().bold(),
            "DEGRADED" => style(verdict).yellow().bold(),
            _ => style(verdict).red().bold(),
        };
        println!(
            "  {} 總結: {}  (匯入 {} / 衝突 {} / 跳過 {} / 部分 {})",
            style("▶").cyan(),
            styled,
            self.count("IMPORTED"),
            self.count("CONFLICT"),
            self.count("SKIPPED"),
            self.count("PARTIAL"),
        );
        if !self.apply {
            println!(
                "  {} 這是預覽。加上 {} 才會實際寫入。",
                style("ℹ").blue(),
                style("--apply").bold()
            );
        }
        println!();
    }

    /// Serialize the report to the frontend-locked JSON contract consumed by
    /// the dashboard (`migrate.scan` / `migrate.apply` gateway RPCs).
    ///
    /// Field names are a stable API surface — do NOT rename `platform`,
    /// `source`, `dry_run`, `items[].kind/name/status/reason`, `summary`,
    /// `verdict`, `notes`, `report_path`. `status` is lowercase; `dry_run` is
    /// the inverse of `apply`; `report_path` is `Some(path)` only after apply.
    pub fn to_json(&self, report_path: Option<String>) -> serde_json::Value {
        let items: Vec<serde_json::Value> = self
            .items
            .iter()
            .map(|i| {
                serde_json::json!({
                    "kind": i.category,
                    "name": i.name,
                    "status": i.status.json_str(),
                    "reason": i.status.reason(),
                })
            })
            .collect();
        serde_json::json!({
            "platform": self.platform,
            "source": self.source,
            "dry_run": !self.apply,
            "items": items,
            "summary": {
                "imported": self.count("IMPORTED"),
                "partial": self.count("PARTIAL"),
                "skipped": self.count("SKIPPED"),
                "conflict": self.count("CONFLICT"),
            },
            "verdict": self.overall(),
            "notes": self.notes,
            "report_path": report_path,
        })
    }

    /// Render the report as a Markdown document for the on-disk archive.
    pub fn render_markdown(&self) -> String {
        let mut md = String::new();
        let now = chrono::Utc::now().to_rfc3339();
        let _ = writeln!(md, "# 轉移報告: {} → DuDuClaw", self.platform);
        let _ = writeln!(md);
        let _ = writeln!(md, "- 時間: {now}");
        let _ = writeln!(md, "- 來源: `{}`", self.source);
        let _ = writeln!(
            md,
            "- 模式: {}",
            if self.apply {
                "套用 (--apply)"
            } else {
                "預覽 (dry-run)"
            }
        );
        let _ = writeln!(md, "- 總結: **{}**", self.overall());
        let _ = writeln!(md);
        let _ = writeln!(md, "| 類別 | 項目 | 狀態 | 原因 |");
        let _ = writeln!(md, "|---|---|---|---|");
        for item in &self.items {
            let reason = item.status.reason().unwrap_or("");
            // Escape pipe chars so a token/reason can't break the table.
            let name = item.name.replace('|', "\\|");
            let reason = reason.replace('|', "\\|");
            let _ = writeln!(
                md,
                "| {} | {} | {} | {} |",
                item.category,
                name,
                item.status.tag(),
                reason
            );
        }
        let _ = writeln!(md);
        let _ = writeln!(
            md,
            "統計: 匯入 {} / 衝突 {} / 跳過 {} / 部分 {} / 共 {} 項",
            self.count("IMPORTED"),
            self.count("CONFLICT"),
            self.count("SKIPPED"),
            self.count("PARTIAL"),
            self.items.len(),
        );
        if !self.notes.is_empty() {
            let _ = writeln!(md);
            let _ = writeln!(md, "## 備註");
            let _ = writeln!(md);
            for note in &self.notes {
                let _ = writeln!(md, "- {note}");
            }
        }
        md
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overall_complete_when_all_imported() {
        let mut r = Report::new("hermes", "/x", true);
        r.imported("agent", "a");
        r.imported("memory", "m");
        assert_eq!(r.overall(), "COMPLETE");
    }

    #[test]
    fn overall_degraded_when_mixed() {
        let mut r = Report::new("hermes", "/x", true);
        r.imported("agent", "a");
        r.skipped("channel", "whatsapp", "linked-device");
        assert_eq!(r.overall(), "DEGRADED");
    }

    #[test]
    fn overall_partial_when_nothing_imported() {
        let mut r = Report::new("hermes", "/x", false);
        r.skipped("agent", "a", "exists");
        r.conflict("channel", "telegram", "already set");
        assert_eq!(r.overall(), "PARTIAL");
        // empty report also PARTIAL
        let empty = Report::new("x", "/y", false);
        assert_eq!(empty.overall(), "PARTIAL");
    }

    #[test]
    fn markdown_escapes_pipes_and_lists_reasons() {
        let mut r = Report::new("openclaw", "/src", true);
        r.conflict("channel", "tele|gram", "既有 token");
        let md = r.render_markdown();
        assert!(md.contains("tele\\|gram"));
        assert!(md.contains("CONFLICT"));
        assert!(md.contains("既有 token"));
        assert!(md.contains("**DEGRADED**") || md.contains("**PARTIAL**"));
    }
}
