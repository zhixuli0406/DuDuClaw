//! Bootstrap seam for the running fresh gateway. Root never opens users.db.
use std::io::{IsTerminal, Write};
use duduclaw_isolation::migration::MigrationState;
use super::{Error, RootMigration, RootMigrationIntegration, RootPlan, root, trusted_parents, path_metadata};

/// The gateway-side implementation must authenticate root SO_PEERCRED, bind this
/// migration to its fresh boot manifest, and use its ALREADY OPEN fresh UserDb's
/// issue_setup_token(false). That transaction refuses a nonempty users table and
/// revokes previous setup tokens. It must build the URL with the actual installed
/// dashboard claim route; inventing a path or opening a legacy DB is forbidden.
/// Secrets are returned only over the root-owned local bootstrap channel.
pub trait RootBootstrapIssuer {
    fn issue_first_admin_link(&mut self, plan: &RootPlan) -> Result<SetupLink, Error>;
}
pub struct UnavailableBootstrapIssuer;
impl RootBootstrapIssuer for UnavailableBootstrapIssuer {
    fn issue_first_admin_link(&mut self, _: &RootPlan) -> Result<SetupLink, Error> { Err(Error::Unavailable) }
}

/// Deliberately has no Debug/Serialize implementation. Never put a setup link in
/// the migration journal, command diagnostics, service logs or audit evidence.
pub struct SetupLink(String);
impl SetupLink {
    pub fn from_verified_gateway_route(value: String) -> Result<Self, Error> {
        if value.is_empty() || value.len() > 8192 || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(Error::Invalid);
        }
        Ok(Self(value))
    }
    /// Consume once, writing only to the root terminal. Redirected output refuses
    /// before token issuance at the command layer and is checked here again.
    pub fn print_once(self) -> Result<(), Error> {
        root()?;
        if !std::io::stdout().is_terminal() { return Err(Error::Authentication); }
        let mut stdout = std::io::stdout().lock();
        stdout.write_all(self.0.as_bytes())?;
        stdout.write_all(b"\n")?;
        stdout.flush()?;
        Ok(())
    }
}
fn allowed_stage(state: MigrationState) -> bool {
    matches!(state, MigrationState::IsolationVerified | MigrationState::AwaitingAdministrator)
}
impl<I: RootMigrationIntegration> RootMigration<I> {
    /// Only an already verified fresh installation can issue/reissue bootstrap.
    /// Caller must provide the gateway adapter; absence never falls back to a
    /// privileged SQLite open or an invented dashboard route.
    pub fn new_setup_token(&mut self, issuer: &mut impl RootBootstrapIssuer) -> Result<SetupLink, Error> {
        root()?;
        if !allowed_stage(self.state()?) { return Err(Error::Conflict); }
        trusted_parents(&self.plan.legacy_home)?;
        path_metadata(&self.plan.legacy_home, true, self.plan.gateway_uid)?;
        issuer.issue_first_admin_link(&self.plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_needs_verified_fresh_home_stage() {
        assert!(allowed_stage(MigrationState::IsolationVerified));
        assert!(allowed_stage(MigrationState::AwaitingAdministrator));
        for state in [MigrationState::Planned, MigrationState::OldEnvironmentStopped,
            MigrationState::LegacyQuarantined, MigrationState::KeysRotated,
            MigrationState::TokenRevocationRecorded, MigrationState::EmployeeProposalsStaged,
            MigrationState::ContentImported, MigrationState::ConfirmationReady, MigrationState::Complete] {
            assert!(!allowed_stage(state));
        }
    }
    #[test]
    fn setup_link_rejects_terminal_control_and_unbounded_output() {
        for text in ["".to_string(), "https://example.invalid/\nsecret".to_string(),
            "\u{1b}[31msecret".to_string(), "x".repeat(8193)] {
            assert!(SetupLink::from_verified_gateway_route(text).is_err());
        }
    }
}
