//! The MCP-facing co-drive script schema (`codrive_run`'s `script` param)
//! and its structural validation/sanitization.
//!
//! Design authority: `commercial/docs/DESIGN-codrive-desktop-2026-08.md`.
//! The shape here is exactly the JSON schema given in the CD-1 work
//! package: `target_app` / `task_summary` / `steps[]`, each step an
//! optional `highlight`, an `action` (tagged union), and an optional
//! `consequential` marker gating it behind human approval.

use serde::{Deserialize, Serialize};

use super::client::CodriveButton;

/// Hard cap on the number of steps in one script.
pub const MAX_STEPS: usize = 50;
/// Per-narration char cap, CJK-safe (`duduclaw_core::truncate_chars`).
pub const MAX_NARRATION_CHARS: usize = 200;
/// `target_app` char cap.
pub const MAX_TARGET_APP_CHARS: usize = 200;
/// `task_summary` char cap.
pub const MAX_TASK_SUMMARY_CHARS: usize = 500;
/// `consequential.description` char cap.
pub const MAX_CONSEQUENTIAL_DESC_CHARS: usize = 300;
/// `wait{ms}` is clamped, not rejected — a caller asking for a longer
/// pause just gets the ceiling instead of the whole script failing.
pub const MAX_WAIT_MS: u32 = 10_000;
/// `take_over`'s `reason` char cap — mirrors comp's own `MAX_TAKE_OVER_
/// REASON_BYTES` byte cap (`duduclaw-comp/src/codrive/protocol.rs`) with a
/// CJK-safe char count instead of a byte count (this crate, unlike comp,
/// has `duduclaw_core::truncate_chars` available).
pub const MAX_TAKE_OVER_REASON_CHARS: usize = 200;
/// `api_action.action`'s name char cap (WP-CD4a, C-L2 registry) — these are
/// short programmatic identifiers (`"open_url"`, `"state"`, ...), not
/// free-form narration.
pub const MAX_API_ACTION_NAME_CHARS: usize = 64;
/// `locate.role`'s char cap (WP-CD4b, C-L3 AT-SPI2) — a short programmatic
/// role token (`"button"`, `"entry"`, ...), matched against
/// `atspi_locate::role_from_token`'s closed table, not free-form narration.
pub const MAX_LOCATE_ROLE_CHARS: usize = 32;
/// `locate.name`'s char cap — an accessible-object name/label query
/// (`"儲存"`, `"Save"`), CJK-safe. Generous relative to typical UI labels but
/// still bounded (coding convention: never trust an unbounded caller string).
pub const MAX_LOCATE_NAME_CHARS: usize = 120;

/// The full script one `codrive_run` call executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodriveScript {
    pub target_app: String,
    pub task_summary: String,
    pub steps: Vec<CodriveStep>,
    /// CD-3 (DESIGN §3.4 "Watch mode…敏感情境觸發後，該軌跡剩餘全程要求人在
    /// 場"): opt-in, whole-script idle-supervision request — sent to comp as
    /// `{"op":"watch","enable":true}` right after connecting. `false`
    /// (comp's default) when omitted — existing callers/scripts are
    /// unaffected.
    #[serde(default)]
    pub watch_mode: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodriveStep {
    pub narration: String,
    #[serde(default)]
    pub highlight: Option<CodriveHighlight>,
    pub action: CodriveAction,
    #[serde(default)]
    pub consequential: Option<CodriveConsequential>,
    /// C-L2 (WP-CD4a, DESIGN §3.2 execution ladder rung 2): optional request
    /// to serve this step via a registered third-party app's native API/
    /// CLI/D-Bus action instead of clicking through the GUI. `None` (the
    /// default — every script written before this field existed) means:
    /// skip the registry entirely, dispatch exactly as before — zero
    /// behavior change. When set, the driver looks it up in
    /// `registry::APP_REGISTRY` keyed by `(CodriveScript::target_app,
    /// action)`; a registry MISS or an exec FAILURE both fall back to this
    /// step's own coordinate `action` above, unchanged — see
    /// `step::try_registry_action`. `api_action` is a best-effort
    /// accelerant layered on top of the existing C-L1 path, never a
    /// replacement for it: this step's `action` field is still required and
    /// still the thing that actually runs whenever C-L2 can't serve the
    /// request.
    #[serde(default)]
    pub api_action: Option<ApiActionRequest>,
    /// C-L3 (WP-CD4b, DESIGN §3.2 execution ladder rung 3): optional request
    /// to resolve this step's coordinate `action` via the AT-SPI2
    /// accessibility bus instead of the literal `x`/`y` the caller wrote
    /// into `action`. `None` (the default — every script written before this
    /// field existed) means: skip the locate step entirely, dispatch exactly
    /// as before — zero behavior change. When set, the driver looks up the
    /// control in `CodriveScript::target_app`'s accessible tree keyed by
    /// `(role, name)` — see `atspi_locate::locate`; a locate MISS or FAILURE
    /// both fall back to this step's own literal `action` coordinates,
    /// unchanged — see `step::try_atspi_locate`. Only meaningful for
    /// `Move`/`Click` actions (every other action kind's `locate`, if set,
    /// is silently ignored — there is no coordinate to override); `action`
    /// is still required and still what actually runs whenever C-L3 can't
    /// resolve the request, same non-replacement relationship `api_action`
    /// already has with `action` one rung up the ladder.
    #[serde(default)]
    pub locate: Option<LocateRequest>,
}

/// One C-L2 registry action request: the registered action's `name` plus
/// its call params (validated against the action's own `params_schema` at
/// dispatch time — see `registry.rs`). `params` defaults to `Null` for
/// zero-param actions (e.g. NetworkManager's read-only queries).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiActionRequest {
    pub action: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

/// One C-L3 AT-SPI2 locate query: a role token (`atspi_locate::role_from_token`'s
/// closed vocabulary — `"button"`, `"entry"`, `"password"`, ...) plus a
/// name/label substring to match within `target_app`'s accessible tree.
/// Both fields are free text from the caller and are truncated CJK-safe by
/// `CodriveScript::sanitize` (`MAX_LOCATE_ROLE_CHARS`/`MAX_LOCATE_NAME_CHARS`)
/// — an unrecognized `role` token is not a structural error here, it degrades
/// to an honest `LocateOutcome::Failed` at dispatch time (see
/// `atspi_locate::locate`), same "never reject the whole script, just fall
/// back" spirit as every other soft-validated field in this schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocateRequest {
    pub role: String,
    pub name: String,
}

/// Target highlight box (a pre-click predisplay frame, design §3.3.2(b)) —
/// note this carries no `ms`; the on-screen display duration is a wire
/// concern the driver decides, not something the caller specifies.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct CodriveHighlight {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// One step's action. `kind` tags the JSON shape: `move{x,y}` /
/// `click{x,y,btn}` (= move + button press + release) / `text{s}` /
/// `key_name{name}` (press + release tap) / `wait{ms}` (a local pause —
/// never touches the comp socket).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CodriveAction {
    Move { x: f64, y: f64 },
    Click { x: f64, y: f64, btn: CodriveButton },
    Text { s: String },
    KeyName { name: String },
    Wait { ms: u32 },
    /// CD-3 (DESIGN §3.1 "agent 出 take_over 動作 → 同上（主動交棒）", task
    /// brief item 1 "script step 可宣告 take_over（例如登入步）"): this
    /// step's whole "action" is handing the desktop to the human — see
    /// `driver.rs`/`step.rs`'s `take_over_reason`/`run_take_over_step` for
    /// how it's dispatched (never through `send_step_actions`, and never
    /// gated by `consequential`/ApprovalBroker — a take_over step neither
    /// needs nor gets an approval row, same invariant credential-class
    /// auto-conversion already had).
    TakeOver { reason: String },
}

/// Closed enum of consequential-step classes. `Credential` is CD-3's
/// `take_over` auto-conversion trigger (see `step::take_over_reason`) — a
/// step marked `Credential` never reaches `gate_consequential`/
/// ApprovalBroker at all (an approval dialog asking "approve entering your
/// password?" makes no sense when the whole point is the human types it
/// themselves); it is instead routed straight to a take_over hand-off,
/// same as an explicit `CodriveAction::TakeOver` step. (Superseded CD-1/
/// CD-2 behavior: a `Credential`-classed step used to refuse the WHOLE
/// script upfront — task brief item 1 replaced that with this per-step
/// auto-conversion; the whole-script refuse-list in `driver.rs` for
/// banking-page/CAPTCHA keywords is unrelated and unchanged.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsequentialClass {
    Send,
    Submit,
    Delete,
    Purchase,
    Credential,
    Other,
}

impl ConsequentialClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ConsequentialClass::Send => "send",
            ConsequentialClass::Submit => "submit",
            ConsequentialClass::Delete => "delete",
            ConsequentialClass::Purchase => "purchase",
            ConsequentialClass::Credential => "credential",
            ConsequentialClass::Other => "other",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodriveConsequential {
    pub class: ConsequentialClass,
    pub description: String,
}

impl CodriveScript {
    /// Structural validation + CJK-safe field truncation
    /// (`duduclaw_core::truncate_chars` — coding convention 1: never
    /// raw-slice a string). Returns `Err` only for structural violations a
    /// caller must fix (no steps / too many steps); every string field is
    /// silently clamped rather than rejected, and `wait{ms}` is clamped to
    /// [`MAX_WAIT_MS`] rather than refused.
    pub fn sanitize(mut self) -> Result<Self, String> {
        if self.steps.is_empty() {
            return Err("script has no steps".to_string());
        }
        if self.steps.len() > MAX_STEPS {
            return Err(format!(
                "script has {} steps, max {MAX_STEPS}",
                self.steps.len()
            ));
        }
        self.target_app =
            duduclaw_core::truncate_chars(self.target_app.trim(), MAX_TARGET_APP_CHARS);
        self.task_summary =
            duduclaw_core::truncate_chars(self.task_summary.trim(), MAX_TASK_SUMMARY_CHARS);
        for step in &mut self.steps {
            step.narration =
                duduclaw_core::truncate_chars(step.narration.trim(), MAX_NARRATION_CHARS);
            match &mut step.action {
                CodriveAction::Wait { ms } => *ms = (*ms).min(MAX_WAIT_MS),
                CodriveAction::TakeOver { reason } => {
                    *reason = duduclaw_core::truncate_chars(reason.trim(), MAX_TAKE_OVER_REASON_CHARS);
                }
                CodriveAction::Move { .. } | CodriveAction::Click { .. } | CodriveAction::Text { .. } | CodriveAction::KeyName { .. } => {}
            }
            if let Some(c) = &mut step.consequential {
                c.description = duduclaw_core::truncate_chars(
                    c.description.trim(),
                    MAX_CONSEQUENTIAL_DESC_CHARS,
                );
            }
            if let Some(api) = &mut step.api_action {
                api.action = duduclaw_core::truncate_chars(api.action.trim(), MAX_API_ACTION_NAME_CHARS);
            }
            if let Some(loc) = &mut step.locate {
                loc.role = duduclaw_core::truncate_chars(loc.role.trim(), MAX_LOCATE_ROLE_CHARS);
                loc.name = duduclaw_core::truncate_chars(loc.name.trim(), MAX_LOCATE_NAME_CHARS);
            }
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one_step_script(steps: usize) -> CodriveScript {
        CodriveScript {
            target_app: "foot".into(),
            task_summary: "test".into(),
            steps: (0..steps)
                .map(|i| CodriveStep {
                    narration: format!("step {i}"),
                    highlight: None,
                    action: CodriveAction::Move { x: 0.0, y: 0.0 },
                    consequential: None,
                    api_action: None,
                    locate: None,
                })
                .collect(),
            watch_mode: false,
        }
    }

    #[test]
    fn empty_steps_rejected() {
        let s = one_step_script(0);
        assert!(s.sanitize().is_err());
    }

    #[test]
    fn max_steps_accepted() {
        let s = one_step_script(MAX_STEPS);
        assert!(s.sanitize().is_ok());
    }

    #[test]
    fn over_max_steps_rejected() {
        let s = one_step_script(MAX_STEPS + 1);
        let err = s.sanitize().unwrap_err();
        assert!(err.contains("51"));
    }

    #[test]
    fn narration_truncated_cjk_safe() {
        let mut s = one_step_script(1);
        s.steps[0].narration = "測".repeat(MAX_NARRATION_CHARS + 50);
        let s = s.sanitize().unwrap();
        assert_eq!(s.steps[0].narration.chars().count(), MAX_NARRATION_CHARS);
    }

    #[test]
    fn wait_ms_clamped_not_rejected() {
        let mut s = one_step_script(1);
        s.steps[0].action = CodriveAction::Wait { ms: 999_999 };
        let s = s.sanitize().unwrap();
        assert_eq!(s.steps[0].action_wait_ms(), Some(MAX_WAIT_MS));
    }

    #[test]
    fn api_action_name_truncated_cjk_safe() {
        let mut s = one_step_script(1);
        s.steps[0].api_action = Some(ApiActionRequest {
            action: "動".repeat(MAX_API_ACTION_NAME_CHARS + 10),
            params: serde_json::Value::Null,
        });
        let s = s.sanitize().unwrap();
        assert_eq!(
            s.steps[0].api_action.as_ref().unwrap().action.chars().count(),
            MAX_API_ACTION_NAME_CHARS
        );
    }

    #[test]
    fn locate_role_and_name_truncated_cjk_safe() {
        let mut s = one_step_script(1);
        s.steps[0].locate = Some(LocateRequest {
            role: "動".repeat(MAX_LOCATE_ROLE_CHARS + 10),
            name: "儲".repeat(MAX_LOCATE_NAME_CHARS + 10),
        });
        let s = s.sanitize().unwrap();
        let loc = s.steps[0].locate.as_ref().unwrap();
        assert_eq!(loc.role.chars().count(), MAX_LOCATE_ROLE_CHARS);
        assert_eq!(loc.name.chars().count(), MAX_LOCATE_NAME_CHARS);
    }

    #[test]
    fn locate_defaults_to_none_for_existing_scripts() {
        // A script literal built the way every pre-C-L3 caller already
        // writes them (no `locate` field set in JSON) must deserialize with
        // `locate: None` — zero behavior change.
        let json = serde_json::json!({
            "target_app": "foot",
            "task_summary": "test",
            "steps": [{"narration": "move", "action": {"kind": "move", "x": 0.0, "y": 0.0}}],
        });
        let s: CodriveScript = serde_json::from_value(json).unwrap();
        assert!(s.steps[0].locate.is_none());
    }

    #[test]
    fn api_action_defaults_to_none_for_existing_scripts() {
        // A script literal built the way every pre-C-L2 caller already
        // writes them (no `api_action` field set in JSON) must deserialize
        // with `api_action: None` — zero behavior change.
        let json = serde_json::json!({
            "target_app": "foot",
            "task_summary": "test",
            "steps": [{"narration": "move", "action": {"kind": "move", "x": 0.0, "y": 0.0}}],
        });
        let s: CodriveScript = serde_json::from_value(json).unwrap();
        assert!(s.steps[0].api_action.is_none());
    }

    #[test]
    fn action_json_shapes() {
        let click = CodriveAction::Click {
            x: 1.0,
            y: 2.0,
            btn: CodriveButton::Left,
        };
        assert_eq!(
            serde_json::to_value(&click).unwrap(),
            serde_json::json!({"kind": "click", "x": 1.0, "y": 2.0, "btn": "left"})
        );
        let wait = CodriveAction::Wait { ms: 500 };
        assert_eq!(
            serde_json::to_value(&wait).unwrap(),
            serde_json::json!({"kind": "wait", "ms": 500})
        );
    }

    impl CodriveStep {
        /// Test-only accessor so `wait_ms_clamped_not_rejected` doesn't
        /// need a giant match expression.
        fn action_wait_ms(&self) -> Option<u32> {
            match &self.action {
                CodriveAction::Wait { ms } => Some(*ms),
                _ => None,
            }
        }
    }
}
