//! Unified account rotation for Claude Code SDK.
//!
//! Supports two authentication methods:
//! - **OAuth accounts**: Claude Pro/Team/Max subscriptions via `~/.claude/.credentials.json`
//!   Each profile has its own credentials directory at `~/.claude/profiles/<name>/`
//! - **API Key accounts**: Direct Anthropic API keys via `ANTHROPIC_API_KEY` env var
//!
//! The rotator selects the best account and provides the appropriate env vars
//! for the `claude` CLI subprocess.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use duduclaw_security::secret_manager::SecretManagerConfig;
use duduclaw_security::secret_ref::SecretRef;
use zeroize::Zeroize;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::credential_probe::{
    ANTHROPIC_API_BASE, CredentialKind, CredentialProbe, probe_anthropic_credential_at,
};

// ── Types ───────────────────────────────────────────────────

mod health;
mod load;
mod probe;
mod rotator;
mod select;
mod types;

pub struct AccountRotator {
    accounts: Arc<RwLock<Vec<Account>>>,
    strategy: RotationStrategy,
    round_robin_index: Arc<RwLock<usize>>,
    cooldown_seconds: u64,
    /// API base the health probe authenticates against. Real Anthropic in
    /// production; a local listener under test (see
    /// [`with_probe_base_url`](Self::with_probe_base_url)).
    probe_base_url: String,
}

pub use load::{create_from_config, known_subscription_providers};
pub use probe::CredentialReport;
pub use types::{
    Account, AccountEnv, AccountStatus, AuthFailureKind, AuthMethod, CredentialState,
    RotationStrategy, UnavailableReason,
};

pub(crate) use load::{PoolNarrowing, account_in_pool, narrow_by_pool};
pub(crate) use types::{auth_dead_backoff, probe_backoff};

use load::{
    API_KEY_ENC_FIELDS, OAUTH_TOKEN_ENC_FIELDS, build_account_env, detect_default_oauth_session,
    doubled_cooldown, env_fallback_account_env, has_nonempty_field, resolve_api_key,
    resolve_oauth_credentials, resolve_oauth_token, should_autodetect_anthropic_oauth,
};
use types::{AUTH_DEAD_BASE_MINUTES, AUTH_DEAD_CAP_MINUTES};

// Test-only bindings: these items are used inside their own file plus the test
// modules, so the binding here exists for `use super::*` in the latter.
#[cfg(test)]
use load::provider_env_key_names;
#[cfg(test)]
use probe::{legacy_status_probe_may_restore, probe_secret_for};
#[cfg(test)]
use types::{PROBE_BACKOFF_CAP_MINUTES, default_provider};

#[cfg(test)]
mod account_pool_tests;
#[cfg(test)]
mod credential_hardening_tests;
#[cfg(test)]
mod provider_rotation_tests;
#[cfg(test)]
mod select_env_tests;
#[cfg(test)]
mod subscription_oauth_tests;
#[cfg(test)]
mod wp10_on_error_recovery_tests;
#[cfg(test)]
mod wp10c_provider_env_delegation_tests;
#[cfg(test)]
mod wp8a_secret_ref_consolidation_tests;
#[cfg(test)]
mod wpa_load_from_config_provider_tests;
