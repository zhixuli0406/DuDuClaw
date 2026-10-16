//! Credential redaction for operator-visible channel diagnostics.
//!
//! The implementation moved to [`duduclaw_core::secret_redact`] (2026-10,
//! maintenance SSH P-2) so `duduclaw-maint-helper` can share it without linking
//! the gateway. This module re-exports it unchanged; existing
//! `crate::secret_redact::…` and `duduclaw_gateway::secret_redact::…` callers
//! keep compiling with identical behaviour. The tests live with the
//! implementation in `duduclaw-core`.

pub use duduclaw_core::secret_redact::{mask_secret, redact_opt, redact_secrets};
