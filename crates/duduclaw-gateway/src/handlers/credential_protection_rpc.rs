//! `security.credential_protection*` — the B2c protected key in the OS
//! credential store (design `DESIGN-credential-keychain-2026-10` §12).
//!
//! Status, enable and disable, all Admin. None of them ever returns key
//! material: the status carries only the level, the state, a key id and the
//! zh-TW text the dashboard shows. Keychain calls are blocking IPC, so they
//! run on the blocking pool.

#[allow(unused_imports)]
use super::*;

use duduclaw_security::audit::{AuditEvent, Severity};
use duduclaw_security::credential_protection::{CredentialProtection, ProtectionState, ProtectionStatus};

/// The protection for this home. Production uses the platform store; unit
/// tests get a per-home in-memory store so no test ever touches a real
/// keychain. This seam covers only this crate's unit tests: an integration
/// test under `crates/duduclaw-gateway/tests/` that calls
/// `security.credential_protection_enable` would reach the real login
/// keychain, so none may (status with no marker never touches the store).
/// A `live-test-mocks` build (debug only, never shipped; `lib.rs` refuses a
/// release build with it) uses the same in-memory store, so an isolated
/// live test never reaches the login keychain either.
pub(crate) fn protection_for(home: &Path) -> CredentialProtection {
    #[cfg(any(test, feature = "live-test-mocks"))]
    {
        CredentialProtection::with_store(home, Box::new(test_store::for_home(home)))
    }
    #[cfg(not(any(test, feature = "live-test-mocks")))]
    {
        CredentialProtection::platform(home)
    }
}

/// The JSON the dashboard reads. Explicit fields — never a serialization of
/// anything that could one day grow a secret.
pub(crate) fn status_json(s: &ProtectionStatus) -> Value {
    json!({
        "level": s.level,
        "platform_level": s.platform_level,
        "backend": s.backend,
        "state": s.state,
        "key_id": s.key_id,
        "enabled_at": s.enabled_at,
        "person_identity_allowed": s.person_identity_allowed,
        "summary": s.summary_zh,
        "detail": s.detail_zh,
    })
}

async fn on_blocking<T: Send + 'static>(
    home: PathBuf,
    f: impl FnOnce(CredentialProtection) -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(move || f(protection_for(&home)))
        .await
        .map_err(|e| format!("憑證保護作業中斷：{e}"))
}

fn audit(
    home: &Path,
    event: &str,
    severity: Severity,
    ctx: Option<&UserContext>,
    s: Option<&ProtectionStatus>,
    previous: Option<ProtectionState>,
) {
    crate::security_autopilot::audit_and_emit(
        home,
        &AuditEvent::new(
            event,
            "dashboard",
            severity,
            json!({
                "actor_user_id": ctx.map(|c| c.user_id.clone()),
                "actor_email": ctx.map(|c| c.email.clone()),
                "state": s.map(|s| s.state),
                "previous_state": previous,
                "level": s.map(|s| s.level),
                "backend": s.map(|s| s.backend),
                "key_id": s.and_then(|s| s.key_id.clone()),
            }),
        ),
    );
}

/// A broken state (missing / replaced key, bad marker) is written to the
/// security audit the first time this process sees it per home, so a
/// substitution leaves a record even if nobody opens the Security page twice.
fn audit_broken_once(home: &Path, s: &ProtectionStatus) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    if !matches!(s.state, ProtectionState::KeyChanged | ProtectionState::Missing | ProtectionState::Invalid) {
        return;
    }
    static SEEN: OnceLock<Mutex<HashSet<(PathBuf, &'static str)>>> = OnceLock::new();
    let first = SEEN
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .insert((home.to_path_buf(), s.state.code()));
    if first {
        audit(home, "credential_protection_broken", Severity::Critical, None, Some(s), None);
    }
}

impl MethodHandler {
    /// `security.credential_protection` — current level and state.
    pub(crate) async fn handle_security_credential_protection(&self) -> WsFrame {
        match on_blocking(self.home_dir.clone(), |cp| cp.status()).await {
            Ok(s) => {
                audit_broken_once(&self.home_dir, &s);
                WsFrame::ok_response("", status_json(&s))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `security.credential_protection_enable {reset?}` — create the
    /// protected key. `reset: true` is required to replace a key that exists
    /// but cannot be used (it voids everything sealed with the old one).
    pub(crate) async fn handle_security_credential_protection_enable(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let reset = params.get("reset").and_then(Value::as_bool) == Some(true);
        match on_blocking(self.home_dir.clone(), move |cp| cp.enable_detailed(reset)).await {
            Ok(Ok(out)) => {
                if out.changed {
                    let severity = if out.previous == ProtectionState::NotEnabled {
                        Severity::Warning
                    } else {
                        // A reset voids every credential sealed with the old key.
                        Severity::Critical
                    };
                    audit(&self.home_dir, "credential_protection_enabled", severity, Some(ctx), Some(&out.status), Some(out.previous));
                }
                WsFrame::ok_response("", status_json(&out.status))
            }
            Ok(Err(e)) => {
                audit(&self.home_dir, "credential_protection_enable_failed", Severity::Warning, Some(ctx), None, None);
                WsFrame::error_response("", &e.message_zh)
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    /// `security.credential_protection_disable {confirm: true}` — remove the
    /// protected key. Everything sealed with it becomes unreadable, so the
    /// explicit confirmation is required (no default-yes).
    pub(crate) async fn handle_security_credential_protection_disable(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if params.get("confirm").and_then(Value::as_bool) != Some(true) {
            return WsFrame::error_response(
                "",
                "停用憑證保護會讓已保存的個人帳號憑證全部失效，需要明確確認（confirm: true）",
            );
        }
        match on_blocking(self.home_dir.clone(), |cp| cp.disable().map(|()| cp.status())).await {
            Ok(Ok(s)) => {
                audit(&self.home_dir, "credential_protection_disabled", Severity::Critical, Some(ctx), None, None);
                WsFrame::ok_response("", status_json(&s))
            }
            Ok(Err(e)) => WsFrame::error_response("", &e.message_zh),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}

#[cfg(any(test, feature = "live-test-mocks"))]
pub(crate) mod test_store {
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock};

    use duduclaw_security::credential_protection::MemoryKeyStore;

    /// One app-bound in-memory store per home, shared across calls.
    pub(crate) fn for_home(home: &Path) -> Arc<MemoryKeyStore> {
        static STORES: OnceLock<Mutex<HashMap<PathBuf, Arc<MemoryKeyStore>>>> = OnceLock::new();
        STORES
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .entry(home.to_path_buf())
            .or_insert_with(|| Arc::new(MemoryKeyStore::app_bound()))
            .clone()
    }
}
