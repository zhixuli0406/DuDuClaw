//! Workflow cost ledger and count limits (F1b, E-H3 / A-M-1, option A).
//!
//! What is measured: nothing. Each step dispatch is *charged* an
//! operator-set unit price (`config.toml [workflow.unit_cost_micros]`,
//! keys `read` / `effect` / `process` / `artifact` / `approval`, integer
//! micro-units, default 0). That price is an operator estimate, not an API
//! bill, electricity or CPU time. With every price at 0 (the default) the
//! money budgets cannot bind, and `money_limits_effective` says so; the
//! count limits (`[workflow.limits]`) are then the real bound.
//!
//! The charge is written by the server, in the same IMMEDIATE transaction
//! as the step's `running` checkpoint, before anything is dispatched: one
//! immutable row per dispatch attempt in `workflow_cost_entries`, the run's
//! `cost` rebuilt from those rows, and the per-run and calendar-month
//! budgets checked against the ledger (never against a JSON projection that
//! a stale save could overwrite). A price that cannot be read is unknown and
//! is charged at the cap: it consumes whatever per-run budget remains.
//! Running out of budget or hitting a count limit ends the run `blocked`
//! with failure class `limit`, which does not count toward the activation's
//! consecutive-failure breaker.
use super::schema::{CostBreakdown, StepAction, WorkflowRun};
use rusqlite::{Transaction, params};
use std::path::Path;

pub const BASIS_UNIT_PRICE: &str = "operator_unit_price";
pub const BASIS_UNKNOWN_AT_CAP: &str = "unknown_at_cap";

pub const LIMIT_STEPS: &str = "workflow_limit_steps_per_run";
pub const LIMIT_READS: &str = "workflow_limit_reads_per_run";
pub const LIMIT_EFFECTS: &str = "workflow_limit_effects_per_run";
pub const LIMIT_RUNS: &str = "workflow_limit_runs_per_month";
pub const LIMIT_CONFIG: &str = "workflow_limit_config_invalid";
pub const BUDGET_RUN: &str = "workflow_budget_exhausted";
pub const BUDGET_MONTH: &str = "workflow_monthly_budget_exhausted";

/// What a step dispatch is charged as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChargeKind {
    Read,
    Effect,
    Process,
    Artifact,
    Approval,
}
impl ChargeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Effect => "effect",
            Self::Process => "process",
            Self::Artifact => "artifact",
            Self::Approval => "approval",
        }
    }
    pub fn of(action: &StepAction) -> Self {
        match action {
            StepAction::McpRead { .. } => Self::Read,
            StepAction::McpEffect { .. } => Self::Effect,
            StepAction::Process { .. } => Self::Process,
            StepAction::Artifact { .. } => Self::Artifact,
            StepAction::Approval { .. } | StepAction::Question { .. } => Self::Approval,
        }
    }
    const ALL: [Self; 5] = [
        Self::Read,
        Self::Effect,
        Self::Process,
        Self::Artifact,
        Self::Approval,
    ];
}

/// Count limits; each is the real bound whether or not prices are set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CountLimits {
    pub max_runs_per_month: u64,
    pub max_steps_per_run: u64,
    pub max_reads_per_run: u64,
    pub max_effects_per_run: u64,
}
impl Default for CountLimits {
    fn default() -> Self {
        Self {
            max_runs_per_month: 500,
            max_steps_per_run: 128,
            max_reads_per_run: 20,
            max_effects_per_run: 10,
        }
    }
}

/// Prices and limits read from `config.toml` at the moment of a charge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pricing {
    /// Per kind (in [`ChargeKind`] order); `None` = unknown, charged at cap.
    prices: [Option<u64>; 5],
    pub limits: CountLimits,
    /// Why the limits could not be read; every charge is then refused.
    pub limits_error: Option<String>,
}
impl Pricing {
    pub fn price(&self, kind: ChargeKind) -> Option<u64> {
        self.prices[ChargeKind::ALL.iter().position(|k| *k == kind).unwrap()]
    }
    /// Money budgets can bind only when some price is above zero or unknown.
    pub fn money_limits_effective(&self) -> bool {
        self.prices.iter().any(|p| p.is_none_or(|v| v > 0))
    }
    pub fn view(&self) -> serde_json::Value {
        let prices: serde_json::Map<String, serde_json::Value> = ChargeKind::ALL
            .iter()
            .map(|k| (k.as_str().to_string(), serde_json::json!(self.price(*k))))
            .collect();
        serde_json::json!({
            "basis": BASIS_UNIT_PRICE,
            "unit_cost_micros": prices,
            "money_limits_effective": self.money_limits_effective(),
            "limits": {
                "max_runs_per_month": self.limits.max_runs_per_month,
                "max_steps_per_run": self.limits.max_steps_per_run,
                "max_reads_per_run": self.limits.max_reads_per_run,
                "max_effects_per_run": self.limits.max_effects_per_run,
            },
            "limits_error": self.limits_error,
        })
    }
}

fn read_u64(table: Option<&toml::Table>, key: &str) -> Result<Option<u64>, ()> {
    match table.and_then(|t| t.get(key)) {
        None => Ok(None),
        Some(toml::Value::Integer(v)) if *v >= 0 => Ok(Some(*v as u64)),
        Some(_) => Err(()),
    }
}

/// Read prices and limits. A missing file or section gives the defaults
/// (prices 0); an unreadable file makes every price unknown and the limits
/// invalid.
pub fn load_pricing(home: &Path) -> Pricing {
    let defaults = CountLimits::default();
    let config = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => match raw.parse::<toml::Table>() {
            Ok(t) => Some(t),
            Err(_) => None,
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(toml::Table::new()),
        Err(_) => None,
    };
    let Some(config) = config else {
        return Pricing {
            prices: [None; 5],
            limits: defaults,
            limits_error: Some("config.toml unreadable".into()),
        };
    };
    let workflow = config.get("workflow").and_then(toml::Value::as_table);
    let prices_table = workflow
        .and_then(|w| w.get("unit_cost_micros"))
        .and_then(toml::Value::as_table);
    let mut prices = [Some(0); 5];
    for (slot, kind) in prices.iter_mut().zip(ChargeKind::ALL) {
        *slot = match read_u64(prices_table, kind.as_str()) {
            Ok(v) => Some(v.unwrap_or(0)),
            Err(()) => None,
        };
    }
    let limits_table = workflow
        .and_then(|w| w.get("limits"))
        .and_then(toml::Value::as_table);
    let mut limits = defaults;
    let mut error = None;
    for (key, slot) in [
        ("max_runs_per_month", &mut limits.max_runs_per_month),
        ("max_steps_per_run", &mut limits.max_steps_per_run),
        ("max_reads_per_run", &mut limits.max_reads_per_run),
        ("max_effects_per_run", &mut limits.max_effects_per_run),
    ] {
        match read_u64(limits_table, key) {
            Ok(Some(v)) => *slot = v,
            Ok(None) => {}
            Err(()) => {
                error = Some(format!(
                    "[workflow.limits] {key} is not a non-negative integer"
                ))
            }
        }
    }
    Pricing {
        prices,
        limits,
        limits_error: error,
    }
}

fn category(cost: &mut CostBreakdown, kind: &str, amount: u64) -> Result<(), String> {
    let slot = match kind {
        "process" => &mut cost.compute,
        "artifact" => &mut cost.disk,
        _ => &mut cost.tools,
    };
    *slot = slot.checked_add(amount).ok_or("workflow cost overflow")?;
    Ok(())
}

/// The run's cost as recorded in the ledger.
pub fn run_cost_in(
    tx: &rusqlite::Connection,
    schema: &str,
    run_id: &str,
) -> Result<CostBreakdown, String> {
    let mut q = tx
        .prepare(&format!(
            "SELECT kind,amount_micros FROM {schema}workflow_cost_entries WHERE run_id=?1"
        ))
        .map_err(|e| e.to_string())?;
    let rows = q
        .query_map(params![run_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })
        .map_err(|e| e.to_string())?;
    let mut cost = CostBreakdown::default();
    for row in rows {
        let (kind, amount) = row.map_err(|e| e.to_string())?;
        category(
            &mut cost,
            &kind,
            u64::try_from(amount).map_err(|_| "invalid cost entry")?,
        )?;
    }
    Ok(cost)
}

/// Ledger total of a workflow's formal (activated) runs in a UTC month.
pub fn month_total_in(
    tx: &rusqlite::Connection,
    schema: &str,
    workflow_id: &str,
    month: &str,
) -> Result<u64, String> {
    let total: i64 = tx
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(amount_micros),0) FROM {schema}workflow_cost_entries
                    WHERE workflow_id=?1 AND month=?2 AND formal=1"
            ),
            params![workflow_id, month],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    u64::try_from(total).map_err(|_| "invalid cost ledger".into())
}

/// Whether the ledger holds a reservation for this effect step.
pub fn effect_reserved_in(
    tx: &rusqlite::Connection,
    schema: &str,
    run_id: &str,
    step_id: &str,
) -> Result<bool, String> {
    tx.query_row(
        &format!(
            "SELECT EXISTS(SELECT 1 FROM {schema}workflow_cost_entries WHERE run_id=?1
                AND step_id=?2 AND kind='effect')"
        ),
        params![run_id, step_id],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

pub fn current_month() -> String {
    chrono::Utc::now().format("%Y-%m").to_string()
}

/// Charge one dispatch attempt of `step_id` inside the caller's IMMEDIATE
/// transaction. Returns the run's ledger cost after the charge; the caller
/// stores it on the run.
pub fn charge_in_tx(
    tx: &Transaction<'_>,
    run: &WorkflowRun,
    step_id: &str,
    kind: ChargeKind,
    pricing: &Pricing,
) -> Result<CostBreakdown, String> {
    if pricing.limits_error.is_some() {
        return Err(LIMIT_CONFIG.into());
    }
    let (steps, reads, effects, step_attempts, total): (i64, i64, i64, i64, i64) = tx
        .query_row(
            "SELECT COUNT(*),COALESCE(SUM(kind='read'),0),COALESCE(SUM(kind='effect'),0),
                COALESCE(SUM(step_id=?2),0),COALESCE(SUM(amount_micros),0)
                FROM workflow_cost_entries WHERE run_id=?1",
            params![run.run_id, step_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .map_err(|e| e.to_string())?;
    // A decision step and an effect step are charged once: re-entering
    // after a person decides (or after a never-begun effect is retried)
    // is not a new purchase. Reads are charged per attempt.
    if matches!(kind, ChargeKind::Approval | ChargeKind::Effect) && step_attempts > 0 {
        return run_cost_in(tx, "", &run.run_id);
    }
    let limits = pricing.limits;
    if steps as u64 >= limits.max_steps_per_run {
        return Err(LIMIT_STEPS.into());
    }
    if kind == ChargeKind::Read && reads as u64 >= limits.max_reads_per_run {
        return Err(LIMIT_READS.into());
    }
    if kind == ChargeKind::Effect && effects as u64 >= limits.max_effects_per_run {
        return Err(LIMIT_EFFECTS.into());
    }
    let total = total as u64;
    let per_run = run.budget.per_run_micros;
    let (amount, basis) = match pricing.price(kind) {
        Some(price) => (price, BASIS_UNIT_PRICE),
        None => {
            let remaining = per_run.saturating_sub(total);
            if remaining == 0 {
                return Err(BUDGET_RUN.into());
            }
            (remaining, BASIS_UNKNOWN_AT_CAP)
        }
    };
    let after = total.checked_add(amount).ok_or("workflow cost overflow")?;
    if after > per_run {
        return Err(BUDGET_RUN.into());
    }
    let formal = run.activation_id.is_some();
    let month = current_month();
    if formal {
        let month_total = month_total_in(tx, "", &run.workflow_id, &month)?;
        if month_total
            .checked_add(amount)
            .ok_or("workflow cost overflow")?
            > run.budget.monthly_micros
        {
            return Err(BUDGET_MONTH.into());
        }
    }
    let amount_db = i64::try_from(amount).map_err(|_| "workflow cost overflow")?;
    tx.execute(
        "INSERT INTO workflow_cost_entries VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
        params![
            format!("{}:{}:{}", run.run_id, step_id, step_attempts + 1),
            run.run_id,
            run.workflow_id,
            formal,
            month,
            step_id,
            step_attempts + 1,
            kind.as_str(),
            amount_db,
            basis,
            chrono::Utc::now().to_rfc3339(),
        ],
    )
    .map_err(|e| e.to_string())?;
    let cost = run_cost_in(tx, "", &run.run_id)?;
    tx.execute(
        "UPDATE workflow_runs SET record_json=json_set(record_json,'$.cost',json(?1)) WHERE run_id=?2",
        params![serde_json::to_string(&cost).map_err(|e| e.to_string())?, run.run_id],
    )
    .map_err(|e| e.to_string())?;
    Ok(cost)
}

/// Formal runs of a workflow created in the current UTC month.
pub fn formal_runs_this_month(tx: &rusqlite::Connection, workflow_id: &str) -> Result<u64, String> {
    let n: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM workflow_runs WHERE workflow_id=?1
                AND json_extract(record_json,'$.activation_id') IS NOT NULL
                AND substr(created_at,1,7)=?2",
            params![workflow_id, current_month()],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    Ok(n as u64)
}

/// Codes that end a run because a budget or count limit was reached; these
/// runs carry failure class `limit` and never trip the breaker.
/// An invalid `[workflow.limits]` is a configuration error, not a limit
/// (R-M3): it is classed as a gate failure and counts toward the breaker.
pub fn is_limit_error(error: &str) -> bool {
    error != LIMIT_CONFIG
        && (error == BUDGET_RUN || error == BUDGET_MONTH || error.starts_with("workflow_limit_"))
}

/// The definition alone needs more than the limits allow (R-M3): each step
/// dispatched once, every read and effect step counted, and, when prices are
/// set, their sum against the per-run budget. Checked at activation and at
/// each trigger (limits can be lowered later).
pub fn structural_limit_error(
    definition: &super::schema::WorkflowDefinition,
    per_run_micros: u64,
    pricing: &Pricing,
) -> Option<&'static str> {
    if pricing.limits_error.is_some() {
        return Some(LIMIT_CONFIG);
    }
    let limits = pricing.limits;
    let steps = definition.steps.len() as u64;
    let reads = definition
        .steps
        .iter()
        .filter(|s| ChargeKind::of(&s.action) == ChargeKind::Read)
        .count() as u64;
    let effects = definition
        .steps
        .iter()
        .filter(|s| ChargeKind::of(&s.action) == ChargeKind::Effect)
        .count() as u64;
    if steps > limits.max_steps_per_run {
        return Some(LIMIT_STEPS);
    }
    if reads > limits.max_reads_per_run {
        return Some(LIMIT_READS);
    }
    if effects > limits.max_effects_per_run {
        return Some(LIMIT_EFFECTS);
    }
    let mut minimum: u64 = 0;
    for step in &definition.steps {
        // An unknown price is charged at the cap, so it alone may fill it.
        let price = pricing.price(ChargeKind::of(&step.action)).unwrap_or(0);
        minimum = minimum.saturating_add(price);
    }
    (minimum > per_run_micros).then_some(BUDGET_RUN)
}

impl super::WorkflowStore {
    /// Charge a dispatch attempt outside the runner (operator tools and the
    /// stdio fixtures use this; the runner charges inside its checkpoint).
    pub async fn charge_step(
        &self,
        home: &Path,
        run_id: &str,
        step_id: &str,
        kind: ChargeKind,
    ) -> Result<CostBreakdown, String> {
        let pricing = load_pricing(home);
        let run = self.get_run(run_id).await?.ok_or("workflow run missing")?;
        self.with_transaction(|tx| charge_in_tx(tx, &run, step_id, kind, &pricing))
            .await
    }
}
