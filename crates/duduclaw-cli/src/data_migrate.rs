//! `duduclaw data-migrate` — CLI front door for the `/data` forward-only
//! settings migrator (H3g).
//!
//! Not to be confused with `duduclaw migrate` (agent.toml → Claude Code
//! format conversion) or `duduclaw migrate-from` (cross-platform state
//! import) — this is a third, unrelated thing: replaying baked-in
//! `/usr/share/duduclaw/migrations/*.sh` scripts against `<DUDUCLAW_HOME>`
//! so `/data` keeps up with format changes that A/B root rollback can never
//! undo. See `duduclaw_core::data_migrations` for the full mechanism design.
//!
//! This is the SAME binary invocation both the boot-time systemd unit
//! (`duduclaw-data-migrate.service`) and an operator use manually — there is
//! no separate shell-script runner. `--run` is what the unit's `ExecStart=`
//! calls; `--pending` / `--check` are read-only and safe to run at any time,
//! including on a machine that isn't an appliance at all (an empty/missing
//! migrations directory is not an error — see
//! `duduclaw_core::data_migrations::list_shipped`).

use std::path::PathBuf;

use duduclaw_core::data_migrations::{self, MigrationScript, RunReport};

/// Parsed `duduclaw data-migrate` flags. Exactly one action is expected;
/// see [`run`] for the precedence when more than one (or none) is passed.
pub struct DataMigrateOptions {
    pub pending: bool,
    pub check: bool,
    pub run: bool,
    pub json: bool,
}

/// Entry point. Returns the process exit code (this command has its own
/// exit-code contract, like `duduclaw secaudit` — not the generic "any Err
/// ⇒ exit 1" the rest of the CLI dispatch uses).
///
/// Exit codes:
///   `--pending` → always 0 (a listing is never a failure).
///   `--check`   → 0 nothing pending, 1 something is pending.
///   `--run`     → 0 all pending applied (or nothing was pending), 1 a
///                 migration failed.
///   none passed → 2 (explicit over implicit — no silent default action for
///                 a command that can mutate `/data`; `--pending` is cheap,
///                 the user just has to ask for it).
pub async fn run(opts: DataMigrateOptions) -> i32 {
    let home = duduclaw_home();
    let migrations_dir = data_migrations::migrations_dir();
    let marker_dir = data_migrations::marker_dir(&home);

    if opts.run {
        return cmd_run(&migrations_dir, &marker_dir, &home, opts.json);
    }
    if opts.check {
        return cmd_check(&migrations_dir, &marker_dir, opts.json);
    }
    if opts.pending {
        return cmd_pending(&migrations_dir, &marker_dir, opts.json);
    }
    eprintln!(
        "duduclaw data-migrate: pass one of --pending, --check, or --run \
         (see `duduclaw data-migrate --help`)."
    );
    2
}

fn duduclaw_home() -> PathBuf {
    if let Ok(custom) = std::env::var("DUDUCLAW_HOME") {
        return PathBuf::from(custom);
    }
    dirs::home_dir()
        .expect("Cannot determine home directory. Set DUDUCLAW_HOME env var.")
        .join(".duduclaw")
}

fn cmd_pending(migrations_dir: &std::path::Path, marker_dir: &std::path::Path, json: bool) -> i32 {
    let pending = match data_migrations::list_pending(migrations_dir, marker_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("duduclaw data-migrate --pending: {e}");
            return 1;
        }
    };
    if json {
        let payload: Vec<_> = pending.iter().map(script_json).collect();
        println!(
            "{}",
            serde_json::json!({ "pending": payload, "count": pending.len() })
        );
    } else if pending.is_empty() {
        println!("No pending /data migrations.");
    } else {
        println!("{} pending /data migration(s):", pending.len());
        for script in &pending {
            println!("  {}  ({})", script.name, format_timestamp(script.timestamp));
        }
        println!("\nRun `duduclaw data-migrate --run` to apply them.");
    }
    // Always 0 — a listing is informational, never a failure, per the task
    // spec's explicit rejection of Omarchy's inverted `--pending` exit code.
    0
}

fn cmd_check(migrations_dir: &std::path::Path, marker_dir: &std::path::Path, json: bool) -> i32 {
    let pending = match data_migrations::list_pending(migrations_dir, marker_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("duduclaw data-migrate --check: {e}");
            return 1;
        }
    };
    let has_pending = !pending.is_empty();
    if json {
        println!(
            "{}",
            serde_json::json!({ "pending_count": pending.len(), "clean": !has_pending })
        );
    } else if has_pending {
        println!("{} /data migration(s) pending.", pending.len());
    } else {
        println!("No pending /data migrations.");
    }
    i32::from(has_pending)
}

fn cmd_run(
    migrations_dir: &std::path::Path,
    marker_dir: &std::path::Path,
    home: &std::path::Path,
    json: bool,
) -> i32 {
    let report = match data_migrations::run_pending(migrations_dir, marker_dir, home) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("duduclaw data-migrate --run: {e}");
            return 1;
        }
    };
    if json {
        println!("{}", run_report_json(&report));
    } else {
        report_human(&report);
    }
    i32::from(!report.ok())
}

fn script_json(s: &MigrationScript) -> serde_json::Value {
    serde_json::json!({ "name": s.name, "timestamp": s.timestamp })
}

fn run_report_json(report: &RunReport) -> serde_json::Value {
    serde_json::json!({
        "ok": report.ok(),
        "applied": report.applied.iter().map(|r| serde_json::json!({
            "name": r.name,
            "duration_ms": r.duration_ms,
        })).collect::<Vec<_>>(),
        "failure": report.failure.as_ref().map(|f| serde_json::json!({
            "script": f.script,
            "exit_code": f.exit_code,
            "output_tail": f.output_tail,
            "failed_at_unix": f.failed_at_unix,
        })),
    })
}

fn report_human(report: &RunReport) {
    if report.applied.is_empty() && report.ok() {
        println!("No pending /data migrations.");
    }
    for record in &report.applied {
        println!("applied {} ({} ms)", record.name, record.duration_ms);
    }
    if let Some(failure) = &report.failure {
        eprintln!(
            "FAILED {} (exit {:?})",
            failure.script, failure.exit_code
        );
        eprintln!("--- output tail ---");
        eprintln!("{}", failure.output_tail);
        eprintln!("-------------------");
        eprintln!(
            "This /data migration did not apply. It will be retried on the \
             next run (boot or `duduclaw data-migrate --run`). Remaining \
             migrations after it, if any, were NOT attempted. This failure \
             is durably recorded — see \
             `duduclaw_core::data_migrations::read_failure` — and does not \
             block the gateway from starting."
        );
    }
}

fn format_timestamp(unix: u64) -> String {
    match chrono::DateTime::from_timestamp(unix as i64, 0) {
        Some(dt) => dt.format("%Y-%m-%d %H:%M:%S UTC").to_string(),
        None => format!("unix:{unix}"),
    }
}
