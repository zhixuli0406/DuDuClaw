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

/// The full script one `codrive_run` call executes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodriveScript {
    pub target_app: String,
    pub task_summary: String,
    pub steps: Vec<CodriveStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodriveStep {
    pub narration: String,
    #[serde(default)]
    pub highlight: Option<CodriveHighlight>,
    pub action: CodriveAction,
    #[serde(default)]
    pub consequential: Option<CodriveConsequential>,
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
}

/// Closed enum of consequential-step classes. `Credential` is refused at
/// the whole-script level before any connection is attempted (see
/// `driver::credential_hit`), so it should never reach the per-step
/// approval path — it stays a real variant here (not special-cased out of
/// the schema) so a script that only *mentions* it still parses and gets
/// an honest "refused" report instead of a schema error.
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
            if let CodriveAction::Wait { ms } = &mut step.action {
                *ms = (*ms).min(MAX_WAIT_MS);
            }
            if let Some(c) = &mut step.consequential {
                c.description = duduclaw_core::truncate_chars(
                    c.description.trim(),
                    MAX_CONSEQUENTIAL_DESC_CHARS,
                );
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
                })
                .collect(),
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
