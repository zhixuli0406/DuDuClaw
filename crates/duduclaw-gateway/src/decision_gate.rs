//! `config.toml [decision]` — the Decision Lab / decision-twin kill switch.
//!
//! The decision-twin line (`decision_*.rs`, the `/api/decision/*` HTTP surface
//! and the `/app/system/decision-lab` dashboard page) shipped with **zero
//! config keys**, which meant an operator had no way to turn it off short of
//! rebuilding the binary. The 2026-09-29 feature audit flagged that as the
//! line's single largest governance gap: a surface that is explicitly labelled
//! "exploratory / synthetic-only" in its own spec must be switchable off by
//! the person running the gateway.
//!
//! This module is that switch, and nothing else:
//!
//! * `enabled` defaults to **`true`** — the user's 2026-09-29 decision was to
//!   keep all three experimental lines mounted, so turning the key on by
//!   default keeps behaviour byte-identical to before it existed.
//! * Setting `enabled = false` makes every `/api/decision/*` route answer
//!   `404` before it reaches a handler. Nothing else changes: the decision
//!   stores are not deleted, the CLI subcommands still work, and flipping the
//!   key back restores the surface on the next gateway restart.
//! * Parsing follows the same convention as every other section read straight
//!   from `config.toml` (`GoalLoopConfig::from_home`,
//!   `TaskForwardModelConfig::from_home`): a missing/malformed `[decision]`
//!   section, or a missing/malformed `config.toml`, resolves to
//!   [`DecisionConfig::default`] rather than failing the boot.
//!
//! Sunset clause (audit X1, 方案 6): if no real-data pilot has run against this
//! line by 2026-12-31 it is to be archived — see
//! `docs/todo/TODO-reversible-context-causal-simulation.md`.

use std::path::Path;

use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

/// Exact path of the decision surface's collection root.
///
/// Matched with `==`, not `starts_with`, so `/api/decisionsomething` can never
/// be mistaken for part of this surface (CLAUDE.md coding convention 2 — no
/// unanchored substring checks in routing decisions).
const DECISION_API_ROOT: &str = "/api/decision";

/// Prefix of every decision-surface sub-route. Includes the trailing slash so
/// the check is anchored at a path-segment boundary.
const DECISION_API_PREFIX: &str = "/api/decision/";

/// `config.toml [decision]` — Decision Lab kill switch.
///
/// Only the kill switch lives here. The task-board feed's own keys
/// (`task_board_shadow`, `task_board_shadow_every_hours`,
/// `task_board_retention_days`, `task_board_horizon_days`,
/// `task_board_queue`) are read by
/// [`crate::decision_task_board_shadow::TaskBoardShadowConfig`] out of the
/// same `[decision]` section — deliberately NOT `deny_unknown_fields` here,
/// so the two readers can share one section without either failing on the
/// other's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct DecisionConfig {
    /// Master switch for the decision-twin HTTP surface. **Defaults `true`**
    /// (the line stays mounted, as decided on 2026-09-29). `false` ⇒ every
    /// `/api/decision/*` request is answered `404` by
    /// [`decision_surface_gate`] before routing.
    pub enabled: bool,
}

impl Default for DecisionConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

impl DecisionConfig {
    /// Read `[decision]` from `<home>/config.toml`. Absent / malformed section,
    /// or absent / malformed `config.toml` ⇒ [`Self::default`] (`enabled =
    /// true`).
    pub fn from_home(home_dir: &Path) -> Self {
        let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        Self::from_table(&table)
    }

    /// Read `[decision]` out of an already-parsed `config.toml` table.
    pub fn from_table(table: &toml::Table) -> Self {
        match table.get("decision") {
            Some(section) => section.clone().try_into().unwrap_or_default(),
            None => Self::default(),
        }
    }
}

/// Returns `true` when `path` belongs to the decision-twin HTTP surface.
///
/// Anchored on purpose: the collection root matches exactly, every sub-route
/// must carry the `/api/decision/` prefix, so neighbouring paths such as
/// `/api/decisions` or `/api/decision-lab` are untouched.
pub fn is_decision_path(path: &str) -> bool {
    path == DECISION_API_ROOT || path.starts_with(DECISION_API_PREFIX)
}

/// Axum middleware: 404 every `/api/decision/*` request while
/// `config.toml [decision] enabled = false`.
///
/// Mounted once, over the whole app, at the end of `server.rs`'s router
/// construction. The flag is resolved at boot (one `config.toml` read) and
/// carried as middleware state, so the hot path is a single path comparison —
/// changing the key takes effect on the next gateway restart, exactly like the
/// bind address and port.
pub async fn decision_surface_gate(
    State(enabled): State<bool>,
    request: Request<Body>,
    next: Next,
) -> Response {
    if !enabled && is_decision_path(request.uri().path()) {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({
                "error": "decision_surface_disabled",
                "hint": "config.toml [decision] enabled = true (then restart the gateway)",
            })),
        )
            .into_response();
    }
    next.run(request).await
}
