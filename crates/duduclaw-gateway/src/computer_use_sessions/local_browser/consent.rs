//! Native page-read consent. Never serialized, persisted, or derived from an
//! operator JWT, a person id, or a turn string alone.
use super::super::attended_call::AttendedCall;
use std::{
    path::Path,
    sync::Mutex,
    time::{Duration, Instant},
};

struct Grant {
    call: AttendedCall,
    binding: String,
    expires: Instant,
}
#[derive(Default)]
pub(crate) struct ReadConsent {
    grant: Mutex<Option<Grant>>,
}
impl ReadConsent {
    pub(crate) fn allows(&self, call: &AttendedCall, binding: &str) -> bool {
        self.grant
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|grant| {
                Instant::now() < grant.expires
                    && grant.binding == binding
                    && grant.call.same_live_call(call)
            })
    }
    pub(crate) fn record(&self, call: AttendedCall, binding: String) {
        *self.grant.lock().unwrap_or_else(|p| p.into_inner()) = Some(Grant {
            call,
            binding,
            expires: Instant::now() + Duration::from_secs(60),
        });
    }
}
pub(crate) fn binding(home: &Path, agent: &str) -> Result<String, String> {
    #[cfg(unix)]
    {
        super::profile::read_binding(home, agent)
    }
    #[cfg(not(unix))]
    {
        let _ = (home, agent);
        Err("local_browser_platform_unsupported".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::computer_use_sessions::{attended, attended_call};
    #[tokio::test]
    async fn consent_is_exact_runtime_scoped_expiring_and_revoked_with_the_ingress() {
        let consent = ReadConsent::default();
        let call = attended::verified_channel_message(
            "line:consent-unit",
            "synthetic-human",
            false,
            async {
                assert!(attended::bind_final_route(
                    "line:consent-unit",
                    "line:consent-unit",
                    "alice",
                    "consent-turn"
                ));
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(
                        Some("line:consent-unit".into()),
                        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                            Some("consent-turn".into()),
                            async {
                                let call = attended_call::capture("alice").unwrap();
                                consent.record(call.clone(), "owner-policy-a".into());
                                assert!(consent.allows(&call, "owner-policy-a"));
                                assert!(!consent.allows(&call, "owner-policy-b"));
                                let reminted = attended_call::capture("alice").unwrap();
                                assert!(!consent.allows(&reminted, "owner-policy-a"));
                                consent.grant.lock().unwrap().as_mut().unwrap().expires =
                                    Instant::now();
                                assert!(!consent.allows(&call, "owner-policy-a"));
                                consent.record(call.clone(), "owner-policy-a".into());
                                let invalidated = call.clone();
                                invalidated.invalidate();
                                assert!(!consent.allows(&call, "owner-policy-a"));
                                consent.record(reminted.clone(), "owner-policy-a".into());
                                reminted
                            },
                        ),
                    )
                    .await
            },
        )
        .await;
        assert!(
            !consent.allows(&call, "owner-policy-a"),
            "ended ingress revokes stored clones"
        );
    }
}
