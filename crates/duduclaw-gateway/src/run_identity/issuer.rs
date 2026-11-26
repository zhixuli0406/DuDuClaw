//! Frozen run claims for Direct-API children, each with an independent lease.
use super::*;

pub struct RunIssuer {
    registry: Arc<Registry>,
    record: RunRecord,
    deadline: Instant,
}

impl RunGuard {
    /// Consume an unbound template. Its token is revoked and never sent to a
    /// child; each MCP spawn receives a new token carrying the same run claims.
    pub fn into_issuer(self) -> Result<RunIssuer, String> {
        let runs = self.registry.runs.lock().map_err(|_| "Run identity registry unavailable")?;
        let entry = runs.get(&self.hash).ok_or("Run identity template expired")?;
        let issuer = RunIssuer { registry: self.registry.clone(), record: entry.record.clone(), deadline: entry.deadline };
        drop(runs);
        Ok(issuer)
    }
}

impl RunIssuer {
    pub fn issue(&self) -> Result<RunGuard, String> {
        let remaining = self.deadline.checked_duration_since(Instant::now()).ok_or("Run identity template expired")?;
        // Registry::issue adds its shutdown grace; do not extend the template.
        self.registry.issue_until(self.record.clone(), remaining.as_secs().saturating_sub(60), Some(self.deadline))
    }
}
