//! Shared run budget. Every CLI spawn, including infrastructure retries and
//! policy development, must reserve a call here before starting a process.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::contracts::{AttemptInfraError, RunBudget};

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub agent_calls: u32,
    /// Conservative accounted exposure, including unknown-call liabilities.
    /// This is not an invoice or a measured total.
    pub spent_usd: f64,
    pub reported_usd: f64,
    pub estimated_usd: f64,
    #[serde(default)]
    pub unclassified_observed_usd: f64,
    pub unknown_reserved_usd: f64,
    pub unknown_calls: u32,
    pub pending_calls: u32,
    /// Outstanding ceilings remain explicit after a crash; they are not measured spend.
    #[serde(default)]
    pub pending_reserved_usd: f64,
    pub wall_secs: f64,
}

impl BudgetSnapshot {
    pub(crate) fn valid(&self) -> bool {
        [self.spent_usd, self.reported_usd, self.estimated_usd, self.unclassified_observed_usd, self.unknown_reserved_usd,
            self.pending_reserved_usd, self.wall_secs].into_iter().all(|v| v.is_finite() && v >= 0.0)
            && self.pending_calls <= self.agent_calls
            && self.unknown_calls <= self.agent_calls.saturating_sub(self.pending_calls)
    }
}

#[derive(Debug)]
struct Binding { home: std::path::PathBuf, run_id: String, sequence: u64 }

#[derive(Debug)]
struct State {
    calls: u32,
    spent: f64,
    reported: f64,
    estimated: f64,
    unclassified_observed: f64,
    unknown_reserved: f64,
    unknown_calls: u32,
    live: BTreeMap<u32, f64>,
    reserved: BTreeMap<u32, f64>,
    rate_limit_stopped: bool,
    binding: Option<Binding>,
}

#[derive(Debug, Clone)]
pub struct SharedBudget {
    limits: RunBudget,
    started: Instant,
    state: Arc<Mutex<State>>,
    cancelled: tokio_util::sync::CancellationToken,
}

impl SharedBudget {
    pub fn bind_run(&self, home: &std::path::Path, run_id: &str) -> Result<(), String> {
        let home = home.canonicalize().map_err(|e| e.to_string())?;
        let mut state = self.state.lock().map_err(|_| "budget mutex poisoned")?;
        if let Some(binding) = &state.binding {
            return if binding.home == home && binding.run_id == run_id { Ok(()) }
                else { Err("a budget cannot be rebound to another run or home".into()) };
        }
        if state.calls != 0 { return Err("bind budget before reserving any call".into()); }
        state.binding = Some(Binding { home, run_id: run_id.into(), sequence: 0 });
        self.persist_locked(&mut state)
    }

    fn snapshot_locked(&self, s: &State) -> BudgetSnapshot {
        BudgetSnapshot { agent_calls: s.calls, spent_usd: s.spent + s.live.values().sum::<f64>(),
            reported_usd: s.reported, estimated_usd: s.estimated, unclassified_observed_usd: s.unclassified_observed,
            unknown_reserved_usd: s.unknown_reserved, unknown_calls: s.unknown_calls,
            pending_calls: s.live.len() as u32, pending_reserved_usd: s.reserved.values().sum(),
            wall_secs: self.started.elapsed().as_secs_f64() }
    }

    fn persist_locked(&self, s: &mut State) -> Result<(), String> {
        let snapshot = self.snapshot_locked(s);
        let Some(binding) = &mut s.binding else { return Ok(()); };
        binding.sequence = binding.sequence.checked_add(1).ok_or("budget sequence exhausted")?;
        let result = super::store::budget_ledger::persist(&binding.home, &binding.run_id,
            self.limits, binding.sequence, snapshot).map_err(|e| e.to_string());
        if result.is_err() { self.cancelled.cancel(); }
        result
    }

    pub fn persist_snapshot(&self) -> Result<(), String> {
        let mut state = self.state.lock().map_err(|_| "budget mutex poisoned")?;
        self.persist_locked(&mut state)
    }
    pub fn new(limits: RunBudget) -> Result<Self, String> {
        if limits.max_agent_calls == 0
            || limits.max_rounds == 0
            || limits.max_wall_secs == 0
            || !limits.max_usd.is_finite()
            || limits.max_usd <= 0.0
        {
            return Err("discovery budgets must be finite and positive".into());
        }
        Ok(Self {
            limits,
            cancelled: tokio_util::sync::CancellationToken::new(),
            started: Instant::now(),
            state: Arc::new(Mutex::new(State {
                calls: 0,
                spent: 0.0,
                reported: 0.0,
                estimated: 0.0,
                unclassified_observed: 0.0,
                unknown_reserved: 0.0,
                unknown_calls: 0,
                live: BTreeMap::new(),
                reserved: BTreeMap::new(),
                rate_limit_stopped: false,
                binding: None,
            })),
        })
    }

    pub fn reserve_call(&self) -> Result<u32, AttemptInfraError> {
        let mut s = self
            .state
            .lock()
            .map_err(|_| AttemptInfraError::BudgetExhausted)?;
        if self.cancelled.is_cancelled()
            || s.calls >= self.limits.max_agent_calls
            || s.spent + s.live.values().sum::<f64>() >= self.limits.max_usd
            || self.started.elapsed().as_secs_f64() >= self.limits.max_wall_secs as f64
        {
            return Err(AttemptInfraError::BudgetExhausted);
        }
        let committed = s
            .reserved
            .iter()
            .map(|(id, cap)| cap.max(*s.live.get(id).unwrap_or(&0.0)))
            .sum::<f64>();
        let available = self.limits.max_usd - s.spent - committed;
        if available <= 0.0 {
            return Err(AttemptInfraError::BudgetExhausted);
        }
        let ceiling = available / f64::from(self.limits.max_agent_calls - s.calls);
        s.calls += 1;
        let id = s.calls;
        s.live.insert(id, 0.0);
        s.reserved.insert(id, ceiling);
        self.persist_locked(&mut s).map_err(|_| AttemptInfraError::BudgetExhausted)?;
        Ok(id)
    }

    /// Costs are monotonic even when a provider reports a lower later value.
    /// Returning false tells the observing process to stop immediately.
    pub fn observe_cost(&self, call: u32, usd: f64) -> bool {
        let Ok(mut s) = self.state.lock() else {
            return false;
        };
        if !usd.is_finite() || usd < 0.0 {
            return false;
        }
        let Some(live) = s.live.get_mut(&call) else {
            return false;
        };
        *live = live.max(usd);
        if self.persist_locked(&mut s).is_err() { return false; }
        s.live.get(&call).copied().unwrap_or(f64::INFINITY)
            < s.reserved.get(&call).copied().unwrap_or(0.0)
            && s.spent + s.live.values().sum::<f64>() < self.limits.max_usd
            && self.remaining_wall() > Duration::ZERO
    }

    /// Always charge spent work, even when the CLI failed before an outcome.
    pub fn finish_call(&self, call: u32, usd: f64) {
        self.finish_accounted_call(call, usd, if usd.is_finite() && usd >= 0.0 {
            super::tree::CostSource::Reported
        } else {
            super::tree::CostSource::Unknown
        });
    }

    /// Missing final usage cannot release an outstanding reservation. An
    /// estimate is recorded separately from a provider-reported charge.
    pub fn finish_accounted_call(&self, call: u32, usd: f64, source: super::tree::CostSource) {
        use super::tree::CostSource;
        if let Ok(mut s) = self.state.lock() {
            if let Some(live) = s.live.remove(&call) {
                let reserved = s.reserved.remove(&call).unwrap_or(live);
                let observed = if usd.is_finite() && usd >= 0.0 { usd.max(live) } else { live };
                let charge = match source {
                    CostSource::Reported if usd.is_finite() && usd >= 0.0 => {
                        s.reported += usd;
                        s.unclassified_observed += observed - usd;
                        observed
                    }
                    CostSource::Estimated if usd.is_finite() && usd >= 0.0 => {
                        s.estimated += usd;
                        s.unclassified_observed += observed - usd;
                        observed
                    }
                    _ => {
                        let liability = observed.max(reserved);
                        s.unknown_reserved += liability;
                        s.unknown_calls = s.unknown_calls.saturating_add(1);
                        liability
                    }
                };
                s.spent += charge;
                let _ = self.persist_locked(&mut s);
            }
        }
    }

    /// Provider-side per-invocation limit. Reservations sum to at most the
    /// remaining run budget; unused reservations are released at settlement.
    pub fn call_limit(&self, call: u32) -> Option<f64> {
        self.state.lock().ok()?.reserved.get(&call).copied()
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }
    pub fn cancel(&self) {
        self.cancelled.cancel();
    }
    pub fn stop_for_rate_limit(&self) {
        if let Ok(mut state) = self.state.lock() { state.rate_limit_stopped = true; }
        self.cancel();
    }
    pub fn rate_limit_stopped(&self) -> bool {
        self.state.lock().is_ok_and(|state| state.rate_limit_stopped)
    }
    pub async fn cancelled(&self) {
        self.cancelled.cancelled().await;
    }

    pub fn remaining_wall(&self) -> Duration {
        if self.cancelled.is_cancelled() {
            Duration::ZERO
        } else {
            Duration::from_secs(self.limits.max_wall_secs).saturating_sub(self.started.elapsed())
        }
    }

    pub fn snapshot(&self) -> BudgetSnapshot {
        let s = self.state.lock().expect("budget mutex poisoned");
        self.snapshot_locked(&s)
    }

    pub fn limits(&self) -> RunBudget {
        self.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn budget(calls: u32, usd: f64) -> SharedBudget {
        SharedBudget::new(RunBudget {
            max_agent_calls: calls,
            max_usd: usd,
            max_wall_secs: 60,
            max_rounds: 1,
        })
        .unwrap()
    }
    #[test]
    fn retries_and_parallel_calls_share_limits() {
        let b = budget(2, 1.0);
        let a = b.reserve_call().unwrap();
        let c = b.clone().reserve_call().unwrap();
        assert!(b.reserve_call().is_err());
        assert!(!b.observe_cost(a, 0.6));
        assert!(!b.observe_cost(c, 0.5));
        b.finish_call(a, 0.2);
        b.finish_call(c, 0.5);
        assert_eq!(b.snapshot().agent_calls, 2);
        assert!((b.snapshot().spent_usd - 1.1).abs() < 1e-9);
    }
    #[test]
    fn invalid_or_unknown_live_cost_fails_closed() {
        let b = budget(3, 1.0);
        let a = b.reserve_call().unwrap();
        assert!(!b.observe_cost(a, f64::NAN));
        assert!(!b.observe_cost(42, 0.1));
        b.finish_call(a, 1.0);
        assert!(b.reserve_call().is_err());
    }

    #[test]
    fn missing_final_cost_must_not_release_the_reserved_liability() {
        let b = budget(2, 1.0);
        let call = b.reserve_call().unwrap();
        let reserved = b.call_limit(call).unwrap();
        b.finish_call(call, f64::NAN);
        assert!(b.snapshot().spent_usd >= reserved);
    }
}

#[cfg(test)]
mod cost_classification_tests {
    use super::*;
    #[test]
    fn lower_reported_final_cost_does_not_relabel_prior_exposure_as_a_provider_bill() {
        let budget = SharedBudget::new(RunBudget { max_agent_calls: 2, max_usd: 2.0,
            max_wall_secs: 10, max_rounds: 1 }).unwrap();
        let call = budget.reserve_call().unwrap();
        assert!(budget.observe_cost(call, 0.6));
        budget.finish_accounted_call(call, 0.2, super::super::tree::CostSource::Reported);
        let snapshot = budget.snapshot();
        assert_eq!(snapshot.reported_usd, 0.2, "reported means the actual provider-reported final charge");
        assert_eq!(snapshot.spent_usd, 0.6, "conservative exposure must remain monotonic");
    }
}
