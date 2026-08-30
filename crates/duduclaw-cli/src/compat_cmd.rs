//! `duduclaw compat` — CP-1/A3 operator CLI surface for the `compat.d`
//! declarative app-compatibility runner registry
//! (`duduclaw_core::compat_runners`). See
//! `commercial/docs/DESIGN-app-compat-layer-2026-08.md` §1 and
//! `docs/guides/app-compat.md`.
//!
//! Read-only in this wave (CP-1/A3): this surface reports what `compat.d`
//! declares and whether each runner's `require_tool` set currently
//! resolves on `$PATH` — it never invokes a declaration's `entrypoint`.
//! Launching a runner is later work (see the TODO's "整合" row).

use console::style;
use duduclaw_core::compat_runners::{self, RunnerStatus};
use duduclaw_core::error::Result;

/// `duduclaw compat list [--json]`.
pub fn cmd_compat_list(json: bool) -> Result<()> {
    let statuses = compat_runners::discover_runners();

    if json {
        // Exactly one JSON array on stdout — same "clean protocol channel"
        // convention as `duduclaw agent list --json`.
        let rows: Vec<serde_json::Value> = statuses
            .iter()
            .map(|status| match status {
                RunnerStatus::Ok { decl, source, missing_tools } => serde_json::json!({
                    "status": if missing_tools.is_empty() { "ready" } else { "missing" },
                    "id": decl.id,
                    "display_name": decl.display_name,
                    "from_os": decl.from_os.as_str(),
                    "to_os": decl.to_os,
                    "entrypoint": decl.entrypoint,
                    "require_tool": decl.require_tool,
                    "install_hint": decl.install_hint,
                    "notes": decl.notes,
                    "source": source.to_string_lossy(),
                    "missing_tools": missing_tools,
                }),
                RunnerStatus::Malformed { path, error } => serde_json::json!({
                    "status": "malformed",
                    "path": path.to_string_lossy(),
                    "error": error,
                }),
            })
            .collect();
        println!("{}", serde_json::to_string(&rows)?);
        return Ok(());
    }

    if statuses.is_empty() {
        println!(
            "沒有發現任何相容層 runner。\n出貨層：{}\n資料層：{}",
            compat_runners::SHIPPED_COMPAT_DIR,
            duduclaw_core::platform::duduclaw_home()
                .join(compat_runners::DATA_COMPAT_SUBDIR)
                .display(),
        );
        return Ok(());
    }

    println!("相容層 runner（{} 個）：\n", statuses.len());
    println!("{:<14} {:<28} {:<14} {}", "ID", "名稱", "來源系統", "狀態");
    println!("{}", "-".repeat(72));

    for status in &statuses {
        match status {
            RunnerStatus::Ok { decl, missing_tools, .. } => {
                let state = if missing_tools.is_empty() {
                    style("ready".to_string()).green().to_string()
                } else {
                    style(format!("missing: {}", missing_tools.join(", "))).yellow().to_string()
                };
                println!("{:<14} {:<28} {:<14} {}", decl.id, decl.display_name, decl.from_os.as_str(), state);
                if !missing_tools.is_empty() {
                    if let Some(hint) = &decl.install_hint {
                        println!("               → {hint}");
                    }
                }
            }
            RunnerStatus::Malformed { path, error } => {
                println!(
                    "{:<14} {:<28} {:<14} {}",
                    "-",
                    path.display(),
                    "-",
                    style(format!("malformed: {error}")).red()
                );
            }
        }
    }

    Ok(())
}
