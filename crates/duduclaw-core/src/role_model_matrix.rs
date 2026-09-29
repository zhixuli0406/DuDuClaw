//! `role_model_matrix.toml` — the static role→model capability matrix
//! (Team-as-Agent P2).
//!
//! One measured cell per `(domain, role, runtime, model)`: how well that
//! vendor model does that role's job on that domain's eval suite, with the
//! confidence interval and the declared minimum detectable effect that
//! bound the claim.
//!
//! ## What this type is, and what it deliberately is not
//!
//! It is a **prior**, written by `duduclaw eval --matrix` and read later (P5)
//! by the team composer when it has to pick a model for a role. It is not a
//! leaderboard: a matrix measured at a declared MDE of 10 percentage points
//! cannot rank two models 3pp apart, and every cell therefore carries its own
//! [`MatrixCell::verdict`] — a cell whose confidence interval does not resolve
//! the declared MDE is [`MatrixVerdict::Unresolved`] and must not be read as a
//! ranking (arXiv:2605.30315; Miller 2024 "Adding Error Bars to Evals").
//!
//! ## Honesty rules encoded in the types
//!
//! * `ci95_low` / `ci95_high` / `mde` are `Option<f64>`: a statistic that could
//!   not be computed (too few samples, zero variance) is an **absent key**,
//!   never a fabricated number and never a `NaN` masquerading as one. TOML has
//!   no reliable non-finite float story either way.
//! * [`MatrixHeader::planner`] exists so a reader can tell "the planner role
//!   scored badly" from "the planner role was never measured". P2 measures
//!   executor and verifier only — a planner cell needs a full team-round
//!   harness (one planner call produces sub-task packets, not an answer a
//!   deterministic assertion can grade), so P2 always writes
//!   [`PlannerState::Deferred`].
//! * [`RoleModelMatrix::validate`] is run on **both** save and load, so a
//!   hand-edited file with a duplicate cell, an unknown runtime id or an
//!   out-of-range mean is refused rather than silently believed.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Canonical filename, written next to the `--report` JSON.
pub const MATRIX_FILE_NAME: &str = "role_model_matrix.toml";

/// Which team role a cell measures.
///
/// Mirrors the `[team.roles.*]` role names. `utility` is deliberately absent —
/// it never enters the matrix (design §3.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixRole {
    Planner,
    Executor,
    Verifier,
}

impl MatrixRole {
    /// Every role the matrix schema can express, in report order.
    pub const ALL: &'static [MatrixRole] = &[
        MatrixRole::Planner,
        MatrixRole::Executor,
        MatrixRole::Verifier,
    ];

    /// Roles P2 actually measures. `Planner` is excluded — see
    /// [`PlannerState::Deferred`].
    pub const MEASURED_IN_P2: &'static [MatrixRole] = &[MatrixRole::Executor, MatrixRole::Verifier];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Planner => "planner",
            Self::Executor => "executor",
            Self::Verifier => "verifier",
        }
    }

    /// Strict id → variant. `None` for anything else — a typo'd `--roles`
    /// value must be refused, never silently dropped (which would produce a
    /// matrix missing a role nobody asked about).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim() {
            "planner" => Some(Self::Planner),
            "executor" => Some(Self::Executor),
            "verifier" => Some(Self::Verifier),
            _ => None,
        }
    }
}

/// Three-state cell verdict. Mirrors `duduclaw-cli::eval::stats::Verdict`'s
/// vocabulary (it is that enum's persisted form; core cannot depend on the CLI
/// crate, and the CLI's version carries the CI arithmetic this one only
/// records).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixVerdict {
    Pass,
    Fail,
    /// The suite was too small to resolve the declared MDE, or the interval
    /// straddles the decision line. **Not** a ranking input.
    Unresolved,
}

impl MatrixVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unresolved => "unresolved",
        }
    }

    /// True when this cell may be used as a ranking/selection input at all.
    pub fn is_resolved(&self) -> bool {
        !matches!(self, Self::Unresolved)
    }
}

/// Whether the planner role was measured in the run that wrote this file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlannerState {
    /// P2: not measured. A planner call emits sub-task packets, not an answer a
    /// deterministic `[expect]` assertion can grade, so scoring it needs a full
    /// team-round harness (P2b). Absence of planner cells means "unmeasured",
    /// never "scored zero".
    #[default]
    Deferred,
    /// Reserved for P2b — a run that really did measure planner cells.
    Measured,
}

impl PlannerState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Deferred => "deferred",
            Self::Measured => "measured",
        }
    }
}

/// The run-wide facts every cell in the file was measured under. A cell read
/// without its header is uninterpretable — which is why they live in one file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixHeader {
    /// Declared minimum detectable effect, as a pass-rate fraction
    /// (`0.10` = 10 percentage points). The matrix cannot support any claim
    /// finer than this.
    pub declared_mde: f64,
    /// Significance level the intervals were computed at.
    pub alpha: f64,
    /// Target statistical power the sample-size math used.
    pub power: f64,
    /// `K` — repeats per case (Miller's K-repeat design).
    pub repeats: u32,
    /// Cluster key for the cluster-robust standard errors (today: `"dir"`).
    pub cluster_by: String,
    /// See [`PlannerState`].
    #[serde(default)]
    pub planner: PlannerState,
    /// RFC3339 timestamp of the run that wrote this file.
    pub generated_at: String,
    /// Whether `--paired-seeds` was in effect (cells share a deterministic
    /// per-`(case, repeat)` seed so the same draws line up across models).
    #[serde(default)]
    pub paired_seeds: bool,
}

/// One measured `(domain, role, runtime, model)` cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatrixCell {
    /// Eval suite directory name (`hr-recruit`, …).
    pub domain: String,
    pub role: MatrixRole,
    /// Runtime id — must be a `RuntimeType` id on this build.
    pub runtime: String,
    /// Model id within that runtime.
    pub model: String,
    /// Distinct cases contributing to `mean` (NOT runs: `K` repeats of one
    /// case aggregate into that case's own pass rate first).
    pub n: usize,
    /// Cell score in `[0, 1]`. Executor cells: deterministic-assertion pass
    /// rate. Verifier cells: agreement with the deterministic gold verdict.
    pub mean: f64,
    /// 95% interval bounds. Absent when not computable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci95_low: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ci95_high: Option<f64>,
    pub verdict: MatrixVerdict,
    /// RFC3339 timestamp of this cell's measurement.
    pub measured_at: String,
    /// The MDE this cell's own sample size actually achieves (Miller Eq. 10) —
    /// which can be much coarser than [`MatrixHeader::declared_mde`]. Absent
    /// when not computable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mde: Option<f64>,
    /// What the OTHER roles were fixed at while this cell was measured, as a
    /// short producer-defined token (`"executor=strong"`, `"planner=strong"`,
    /// `"solo"`).
    ///
    /// 2026-09-28 review (`review_team.md` §3 "評測"): a full-team 2×2 probe
    /// measures the planner arm with a **strong executor** in the loop and the
    /// executor arm with a **strong planner** in the loop. Both land in the
    /// same file, and until this field existed nothing in the file said they
    /// were not measured under the same conditions — a reader comparing a
    /// planner cell against an executor cell was comparing two different
    /// experiments. Cells whose conditioning differs are not comparable, and
    /// the file now says which.
    ///
    /// Absent ⇒ the producer recorded no conditioning (an older file). It is
    /// never written as an empty string: that would be a claim ("measured
    /// under no conditions") rather than an absence, and
    /// [`RoleModelMatrix::validate`] refuses one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditioned_on: Option<String>,
}

/// Upper bound on [`MatrixCell::conditioned_on`]. It is a short label for a
/// human reading a TOML file, not a free-text note — a producer that needs
/// more than this is describing a different experiment, not a condition.
pub const CONDITIONED_ON_MAX_CHARS: usize = 64;

impl MatrixCell {
    /// Build a cell, turning every non-finite statistic into an absent key
    /// (the ONLY sanctioned construction path — see the module docs' honesty
    /// rules). `mean` is clamped into `[0, 1]`: a floating-point mean of 0/1
    /// samples can land a hair outside by rounding, and a cell whose mean is
    /// truly out of range is refused by [`RoleModelMatrix::validate`] instead.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        domain: impl Into<String>,
        role: MatrixRole,
        runtime: impl Into<String>,
        model: impl Into<String>,
        n: usize,
        mean: f64,
        ci95_low: f64,
        ci95_high: f64,
        verdict: MatrixVerdict,
        measured_at: impl Into<String>,
        mde: f64,
    ) -> Self {
        MatrixCell {
            domain: domain.into(),
            role,
            runtime: runtime.into(),
            model: model.into(),
            n,
            mean: if mean.is_finite() {
                mean.clamp(0.0, 1.0)
            } else {
                0.0
            },
            ci95_low: finite_or_none(ci95_low),
            ci95_high: finite_or_none(ci95_high),
            verdict,
            measured_at: measured_at.into(),
            mde: finite_or_none(mde),
            conditioned_on: None,
        }
    }

    /// Record what the other roles were fixed at for this measurement — see
    /// [`MatrixCell::conditioned_on`]. Builder-shaped so the existing
    /// [`MatrixCell::new`] call sites (and the honesty rules they encode) stay
    /// untouched.
    pub fn with_conditioned_on(mut self, conditioned_on: impl Into<String>) -> Self {
        self.conditioned_on = Some(conditioned_on.into());
        self
    }

    /// Identity key — what [`RoleModelMatrix::validate`] enforces uniqueness on
    /// and what [`RoleModelMatrix::cell`] looks up.
    pub fn key(&self) -> (String, MatrixRole, String, String) {
        (
            self.domain.clone(),
            self.role,
            self.runtime.clone(),
            self.model.clone(),
        )
    }

    /// `runtime:model` — the same spelling `--models` takes on the CLI.
    pub fn model_ref(&self) -> String {
        format!("{}:{}", self.runtime, self.model)
    }
}

fn finite_or_none(v: f64) -> Option<f64> {
    if v.is_finite() { Some(v) } else { None }
}

/// The whole file: one header plus zero or more `[[cell]]` tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoleModelMatrix {
    pub header: MatrixHeader,
    /// `[[cell]]` array-of-tables.
    #[serde(default, rename = "cell")]
    pub cells: Vec<MatrixCell>,
}

impl RoleModelMatrix {
    pub fn new(header: MatrixHeader) -> Self {
        RoleModelMatrix {
            header,
            cells: Vec::new(),
        }
    }

    /// Structural + range validation. Run on every save AND every load, so a
    /// hand-edited or truncated file is refused rather than silently believed.
    pub fn validate(&self) -> Result<(), String> {
        let h = &self.header;
        if !(h.declared_mde > 0.0 && h.declared_mde < 1.0) {
            return Err(format!(
                "[header] declared_mde must be a fraction in (0, 1), got {}",
                h.declared_mde
            ));
        }
        if !(h.alpha > 0.0 && h.alpha < 1.0) {
            return Err(format!("[header] alpha must be in (0, 1), got {}", h.alpha));
        }
        if !(h.power > 0.0 && h.power < 1.0) {
            return Err(format!("[header] power must be in (0, 1), got {}", h.power));
        }
        if h.repeats == 0 {
            return Err("[header] repeats must be >= 1".to_string());
        }
        if h.cluster_by.trim().is_empty() {
            return Err("[header] cluster_by must not be empty".to_string());
        }
        if h.generated_at.trim().is_empty() {
            return Err("[header] generated_at must not be empty".to_string());
        }
        if h.planner == PlannerState::Deferred
            && self.cells.iter().any(|c| c.role == MatrixRole::Planner)
        {
            return Err(
                "[header] planner = \"deferred\" but the file carries planner cells — a deferred \
                 role must have no cells (otherwise a reader cannot tell unmeasured from measured)"
                    .to_string(),
            );
        }

        let mut seen: BTreeSet<(String, MatrixRole, String, String)> = BTreeSet::new();
        for (i, c) in self.cells.iter().enumerate() {
            if c.domain.trim().is_empty() {
                return Err(format!("[[cell]] #{i} domain must not be empty"));
            }
            if c.model.trim().is_empty() {
                return Err(format!("[[cell]] #{i} model must not be empty"));
            }
            if crate::types::RuntimeType::from_id(c.runtime.trim()).is_none() {
                return Err(format!(
                    "[[cell]] #{i} runtime {:?} is not a runtime on this build",
                    c.runtime
                ));
            }
            if c.n == 0 {
                return Err(format!(
                    "[[cell]] #{i} n must be >= 1 (a cell with no cases is not a measurement)"
                ));
            }
            if !(c.mean.is_finite() && (0.0..=1.0).contains(&c.mean)) {
                return Err(format!(
                    "[[cell]] #{i} mean must be a finite fraction in [0, 1], got {}",
                    c.mean
                ));
            }
            for (name, v) in [
                ("ci95_low", c.ci95_low),
                ("ci95_high", c.ci95_high),
                ("mde", c.mde),
            ] {
                if let Some(v) = v {
                    if !v.is_finite() {
                        return Err(format!(
                            "[[cell]] #{i} {name} must be finite when present (omit the key \
                             instead of writing a non-finite value)"
                        ));
                    }
                }
            }
            if let (Some(lo), Some(hi)) = (c.ci95_low, c.ci95_high) {
                if lo > hi {
                    return Err(format!(
                        "[[cell]] #{i} ci95_low ({lo}) is above ci95_high ({hi})"
                    ));
                }
            }
            if c.measured_at.trim().is_empty() {
                return Err(format!("[[cell]] #{i} measured_at must not be empty"));
            }
            if let Some(cond) = &c.conditioned_on {
                if cond.trim().is_empty() {
                    return Err(format!(
                        "[[cell]] #{i} conditioned_on must not be blank — omit the key instead \
                         (an empty string claims \"measured under no conditions\")"
                    ));
                }
                if cond.chars().count() > CONDITIONED_ON_MAX_CHARS {
                    return Err(format!(
                        "[[cell]] #{i} conditioned_on must be at most \
                         {CONDITIONED_ON_MAX_CHARS} characters, got {}",
                        cond.chars().count()
                    ));
                }
            }
            if !seen.insert(c.key()) {
                return Err(format!(
                    "[[cell]] #{i} duplicates an earlier cell for (domain={}, role={}, \
                     runtime={}, model={}) — one measurement per cell",
                    c.domain,
                    c.role.as_str(),
                    c.runtime,
                    c.model
                ));
            }
        }
        Ok(())
    }

    /// Exact cell lookup.
    pub fn cell(
        &self,
        domain: &str,
        role: MatrixRole,
        runtime: &str,
        model: &str,
    ) -> Option<&MatrixCell> {
        self.cells.iter().find(|c| {
            c.domain == domain && c.role == role && c.runtime == runtime && c.model == model
        })
    }

    /// Every cell for one `(domain, role)`, in file order.
    pub fn cells_for(&self, domain: &str, role: MatrixRole) -> impl Iterator<Item = &MatrixCell> {
        self.cells
            .iter()
            .filter(move |c| c.domain == domain && c.role == role)
    }

    /// Serialize to TOML text. Validates first — this type never writes a file
    /// it would refuse to read back.
    pub fn to_toml_string(&self) -> Result<String, String> {
        self.validate()?;
        toml::to_string_pretty(self)
            .map_err(|e| format!("cannot serialize {MATRIX_FILE_NAME}: {e}"))
    }

    /// Parse + validate TOML text.
    pub fn from_toml_str(text: &str) -> Result<Self, String> {
        let parsed: RoleModelMatrix =
            toml::from_str(text).map_err(|e| format!("{MATRIX_FILE_NAME} parse error: {e}"))?;
        parsed.validate()?;
        Ok(parsed)
    }

    /// Write atomically (temp file in the same directory + rename) so a reader
    /// never observes a half-written matrix.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = self.to_toml_string()?;
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot rename into {}: {e}", path.display())
        })
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::from_toml_str(&text)
    }
}

/// `<dir>/role_model_matrix.toml`.
pub fn matrix_path(dir: &Path) -> PathBuf {
    dir.join(MATRIX_FILE_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> MatrixHeader {
        MatrixHeader {
            declared_mde: 0.10,
            alpha: 0.05,
            power: 0.8,
            repeats: 3,
            cluster_by: "dir".to_string(),
            planner: PlannerState::Deferred,
            generated_at: "2026-09-25T00:00:00Z".to_string(),
            paired_seeds: true,
        }
    }

    fn cell(role: MatrixRole, runtime: &str, model: &str, mean: f64) -> MatrixCell {
        MatrixCell::new(
            "hr-recruit",
            role,
            runtime,
            model,
            6,
            mean,
            mean - 0.1,
            mean + 0.1,
            MatrixVerdict::Unresolved,
            "2026-09-25T00:00:00Z",
            0.21,
        )
    }

    #[test]
    fn toml_round_trip_preserves_every_field() {
        let mut m = RoleModelMatrix::new(header());
        m.cells.push(cell(
            MatrixRole::Executor,
            "claude",
            "claude-haiku-4-5",
            0.5,
        ));
        m.cells
            .push(cell(MatrixRole::Verifier, "codex", "gpt-5.6-sol", 0.75));
        let text = m.to_toml_string().expect("serializes");
        let back = RoleModelMatrix::from_toml_str(&text).expect("round-trips");
        assert_eq!(back, m);
        // Shape the P5 reader will grep for.
        assert!(text.contains("[header]"), "{text}");
        assert!(text.contains("[[cell]]"), "{text}");
        assert!(text.contains("planner = \"deferred\""), "{text}");
        assert!(text.contains("role = \"executor\""), "{text}");
        assert!(text.contains("verdict = \"unresolved\""), "{text}");
    }

    /// Regression (2026-09-28 review, `review_team.md` §3 "評測"): a 2×2 probe
    /// measures the planner cell with a STRONG executor in the loop and the
    /// executor cell with a STRONG planner in the loop, and the persisted file
    /// carried no field saying so — two cells in one matrix that were not
    /// measured under the same conditions read as directly comparable.
    #[test]
    fn conditioned_on_survives_a_save_load_round_trip_and_absence_stays_absent() {
        let dir = tempfile::tempdir().unwrap();
        let path = matrix_path(dir.path());
        let mut m = RoleModelMatrix::new(MatrixHeader {
            planner: PlannerState::Measured,
            ..header()
        });
        m.cells.push(
            cell(MatrixRole::Planner, "claude", "claude-haiku-4-5", 0.5)
                .with_conditioned_on("executor=strong"),
        );
        m.cells.push(
            cell(MatrixRole::Executor, "codex", "gpt-5.6-sol", 0.75)
                .with_conditioned_on("planner=strong"),
        );
        // A producer that records no conditioning leaves the key absent —
        // never an empty string pretending to be one.
        m.cells
            .push(cell(MatrixRole::Verifier, "claude", "claude-opus-4-6", 0.9));
        m.save(&path).expect("saves");

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("conditioned_on = \"executor=strong\""),
            "{text}"
        );
        assert!(
            text.contains("conditioned_on = \"planner=strong\""),
            "{text}"
        );

        let back = RoleModelMatrix::load(&path).expect("round-trips");
        assert_eq!(back, m);
        assert_eq!(
            back.cells[0].conditioned_on.as_deref(),
            Some("executor=strong")
        );
        assert_eq!(
            back.cells[1].conditioned_on.as_deref(),
            Some("planner=strong")
        );
        assert_eq!(back.cells[2].conditioned_on, None);
    }

    /// A blank / over-long `conditioned_on` is refused rather than persisted:
    /// an empty string would read as "measured under no conditions", which is
    /// a claim, not an absence.
    #[test]
    fn blank_or_oversized_conditioned_on_is_refused() {
        let mut m = RoleModelMatrix::new(header());
        m.cells
            .push(cell(MatrixRole::Executor, "claude", "m", 0.5).with_conditioned_on("   "));
        let err = m.validate().expect_err("blank conditioned_on is refused");
        assert!(err.contains("conditioned_on"), "{err}");

        let mut m = RoleModelMatrix::new(header());
        m.cells.push(
            cell(MatrixRole::Executor, "claude", "m", 0.5)
                .with_conditioned_on("x".repeat(CONDITIONED_ON_MAX_CHARS + 1)),
        );
        let err = m
            .validate()
            .expect_err("over-long conditioned_on is refused");
        assert!(err.contains("conditioned_on"), "{err}");
    }

    #[test]
    fn non_finite_statistics_become_absent_keys_not_fabricated_numbers() {
        let c = MatrixCell::new(
            "d",
            MatrixRole::Executor,
            "claude",
            "m",
            3,
            0.5,
            f64::NAN,
            f64::INFINITY,
            MatrixVerdict::Unresolved,
            "t",
            f64::NAN,
        );
        assert_eq!(c.ci95_low, None);
        assert_eq!(c.ci95_high, None);
        assert_eq!(c.mde, None);
        let mut m = RoleModelMatrix::new(header());
        m.cells.push(c);
        let text = m.to_toml_string().expect("serializes without NaN");
        assert!(!text.contains("nan"), "{text}");
        assert!(!text.contains("ci95_low"), "{text}");
        let back = RoleModelMatrix::from_toml_str(&text).unwrap();
        assert_eq!(back.cells[0].ci95_low, None);
    }

    #[test]
    fn duplicate_cell_key_is_refused() {
        let mut m = RoleModelMatrix::new(header());
        m.cells.push(cell(
            MatrixRole::Executor,
            "claude",
            "claude-haiku-4-5",
            0.5,
        ));
        m.cells.push(cell(
            MatrixRole::Executor,
            "claude",
            "claude-haiku-4-5",
            0.9,
        ));
        let err = m.validate().unwrap_err();
        assert!(err.contains("duplicates"), "{err}");
    }

    #[test]
    fn unknown_runtime_id_is_refused() {
        let mut m = RoleModelMatrix::new(header());
        m.cells
            .push(cell(MatrixRole::Executor, "not-a-runtime", "m", 0.5));
        let err = m.validate().unwrap_err();
        assert!(err.contains("not a runtime"), "{err}");
    }

    #[test]
    fn deferred_planner_may_not_carry_planner_cells() {
        let mut m = RoleModelMatrix::new(header());
        m.cells
            .push(cell(MatrixRole::Planner, "claude", "claude-haiku-4-5", 0.5));
        let err = m.validate().unwrap_err();
        assert!(err.contains("deferred"), "{err}");
        // …and is accepted once the header says the run measured it.
        m.header.planner = PlannerState::Measured;
        m.validate().expect("measured planner is legal");
    }

    #[test]
    fn out_of_range_header_and_cell_values_are_refused() {
        let mut m = RoleModelMatrix::new(header());
        m.header.declared_mde = 0.0;
        assert!(m.validate().unwrap_err().contains("declared_mde"));
        m.header = header();
        m.header.repeats = 0;
        assert!(m.validate().unwrap_err().contains("repeats"));
        m.header = header();
        m.cells.push(cell(MatrixRole::Executor, "claude", "m", 0.5));
        m.cells[0].n = 0;
        assert!(m.validate().unwrap_err().contains("n must be >= 1"));
        m.cells[0].n = 4;
        m.cells[0].mean = 1.5;
        assert!(m.validate().unwrap_err().contains("mean"));
        m.cells[0].mean = 0.5;
        m.cells[0].ci95_low = Some(0.9);
        m.cells[0].ci95_high = Some(0.1);
        assert!(m.validate().unwrap_err().contains("above ci95_high"));
    }

    #[test]
    fn save_then_load_round_trips_through_a_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = matrix_path(dir.path());
        let mut m = RoleModelMatrix::new(header());
        m.cells.push(cell(
            MatrixRole::Verifier,
            "claude",
            "claude-sonnet-4-6",
            0.8,
        ));
        m.save(&path).expect("saves");
        assert_eq!(path.file_name().unwrap(), MATRIX_FILE_NAME);
        let back = RoleModelMatrix::load(&path).expect("loads");
        assert_eq!(back, m);
        // The temp file must not survive a successful save.
        assert!(!path.with_extension("toml.tmp").exists());
    }

    #[test]
    fn a_hand_edited_invalid_file_is_refused_on_load_not_believed() {
        let dir = tempfile::tempdir().unwrap();
        let path = matrix_path(dir.path());
        std::fs::write(
            &path,
            "[header]\ndeclared_mde = 0.1\nalpha = 0.05\npower = 0.8\nrepeats = 1\n\
             cluster_by = \"dir\"\ngenerated_at = \"t\"\n\n\
             [[cell]]\ndomain = \"d\"\nrole = \"executor\"\nruntime = \"claude\"\n\
             model = \"m\"\nn = 0\nmean = 0.5\nverdict = \"pass\"\nmeasured_at = \"t\"\n",
        )
        .unwrap();
        let err = RoleModelMatrix::load(&path).unwrap_err();
        assert!(err.contains("n must be >= 1"), "{err}");
    }

    #[test]
    fn lookup_helpers_find_cells_by_identity() {
        let mut m = RoleModelMatrix::new(header());
        m.cells.push(cell(
            MatrixRole::Executor,
            "claude",
            "claude-haiku-4-5",
            0.5,
        ));
        m.cells
            .push(cell(MatrixRole::Executor, "codex", "gpt-5.6-sol", 0.6));
        assert!(
            m.cell("hr-recruit", MatrixRole::Executor, "codex", "gpt-5.6-sol")
                .is_some()
        );
        assert!(
            m.cell("hr-recruit", MatrixRole::Verifier, "codex", "gpt-5.6-sol")
                .is_none()
        );
        assert_eq!(m.cells_for("hr-recruit", MatrixRole::Executor).count(), 2);
        assert_eq!(
            m.cells[1].model_ref(),
            "codex:gpt-5.6-sol",
            "model_ref must spell the same `runtime:model` the CLI takes"
        );
    }

    #[test]
    fn role_parse_is_strict() {
        assert_eq!(MatrixRole::parse("executor"), Some(MatrixRole::Executor));
        assert_eq!(MatrixRole::parse(" verifier "), Some(MatrixRole::Verifier));
        assert_eq!(MatrixRole::parse("Executor"), None);
        assert_eq!(MatrixRole::parse("utility"), None);
        assert_eq!(MatrixRole::MEASURED_IN_P2.len(), 2);
        assert!(!MatrixRole::MEASURED_IN_P2.contains(&MatrixRole::Planner));
    }

    #[test]
    fn unresolved_is_never_a_ranking_input() {
        assert!(!MatrixVerdict::Unresolved.is_resolved());
        assert!(MatrixVerdict::Pass.is_resolved());
        assert!(MatrixVerdict::Fail.is_resolved());
    }
}
