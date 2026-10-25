//! Owned fixtures only; these declarations have not yet been compiled or run.
use super::*;
use crate::google_identity::requester_gate::UserDbPeople;
use crate::google_personal::{access, authority::PersonalSelection};
use crate::person_credentials::{self as vault, Metadata, Secret, Status};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use std::sync::Arc;

struct Fixture {
    home: tempfile::TempDir,
    db: Arc<UserDb>,
    user: String,
}
impl Fixture {
    fn new(scopes: Vec<String>, expires_at: i64) -> Self {
        let home = tempfile::tempdir().unwrap();
        duduclaw_core::gateway_instance::acquire(home.path()).unwrap();
        crate::handlers::credential_protection_rpc::protection_for(home.path())
            .enable(false)
            .unwrap();
        crate::team_edition::record(home.path(), duduclaw_core::EditionProfile::Enterprise);
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("config.toml"), format!("[integrations]\ngoogle_workspace=true\n[mcp_keys.ddc_prod_0123456789abcdef0123456789abcdef]\nclient_id='bot'\ncreated_at='{}'\nscopes=['google:read']\n", chrono::Utc::now().to_rfc3339())).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='anyone'\nfallback='home'\n").unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let user = db
            .create_user(
                "login@unit.test",
                "Alice",
                "fixture-password-only",
                UserRole::Employee,
            )
            .unwrap();
        db.bind_agent(&user.id, "bot", AccessLevel::Owner).unwrap();
        db.set_google_consent(&user.id, "bot", true, &user.id)
            .unwrap();
        crate::mcp_oauth::upsert_client_config(
            home.path(),
            crate::mcp_oauth::McpOAuthClientConfig {
                provider_id: "google".into(),
                client_id: "unit-client".into(),
                client_secret: "unit-client-secret".into(),
                auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
                token_url: "https://oauth2.googleapis.com/token".into(),
                scopes: vec![],
                redirect_uri: "http://127.0.0.1:18789/api/mcp/oauth/callback".into(),
            },
        )
        .unwrap();
        let metadata = Metadata {
            owner_kind: "user".into(),
            owner_id: user.id.clone(),
            provider: "google".into(),
            credential_id: uuid::Uuid::new_v4().to_string(),
            revision: 1,
            account_email: "alice@unit.test".into(),
            google_sub: "unit-sub".into(),
            hosted_domain: None,
            granted_scopes: scopes,
            status: Status::Connected,
            status_before_suspension: None,
            connected_at: chrono::Utc::now().to_rfc3339(),
            last_refresh_at: None,
            oauth_client_id: "unit-client".into(),
        };
        let epoch = vault::begin_connection(home.path(), &user.id).unwrap();
        let key = crate::google_identity::protection_gate(home.path()).unwrap();
        vault::connect(
            home.path(),
            &key,
            metadata,
            &Secret {
                access_token: "fixture-original-never-output".into(),
                refresh_token: Some("fixture-refresh-never-output".into()),
                expires_at: Some(expires_at),
            },
            epoch,
        )
        .unwrap();
        access::pinned_test::install(home.path());
        crate::google_personal::authority::pinned_client_test::install(home.path());
        Self {
            home,
            db,
            user: user.id,
        }
    }
    fn gmail(expires_at: i64) -> Self {
        Self::new(
            vec!["https://www.googleapis.com/auth/gmail.readonly".into()],
            expires_at,
        )
    }
    fn selected(&self, tool: &str) -> PersonalSelection {
        PersonalSelection::capture(
            self.home.path(),
            "bot",
            &RequesterIdentity::Unknown,
            &UserDbPeople(self.db.clone()),
            tool,
        )
        .unwrap()
    }
    fn dedicated(&self, tool: &str) -> GoogleCallContext {
        GoogleCallContext::dedicated(
            self.home.path(),
            "bot",
            Box::new(UserDbPeople(self.db.clone())),
            tool,
            duduclaw_core::google_identity::google_arguments_digest(
                &serde_json::json!({"fixture":true}),
            ),
            duduclaw_core::google_identity::CredentialRef {
                kind: duduclaw_core::google_identity::CredentialKind::Legacy,
                sha256: {
                    use sha2::Digest;
                    format!(
                        "{:x}",
                        sha2::Sha256::digest(b"ddc_prod_0123456789abcdef0123456789abcdef")
                    )
                },
            },
        )
        .unwrap()
    }
    fn refresh(&self) {
        let key = crate::google_identity::protection_gate(self.home.path()).unwrap();
        let old = vault::get(self.home.path(), &self.user).unwrap().unwrap();
        assert!(
            vault::finish_refresh(
                self.home.path(),
                &key,
                &old.metadata,
                &Secret {
                    access_token: "fixture-other-revision-never-output".into(),
                    refresh_token: Some("fixture-refresh-never-output".into()),
                    expires_at: Some(4_000_000_000),
                }
            )
            .unwrap()
        );
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        access::pinned_test::remove(self.home.path());
        crate::google_personal::authority::pinned_client_test::remove(self.home.path());
    }
}

#[tokio::test]
async fn healthy_pinned_resolve_has_no_refresh_and_effect_validation_is_read_only() {
    let f = Fixture::gmail(4_000_000_000);
    let material = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
        .await
        .unwrap();
    assert!(material.fixture_token_matches("fixture-original-never-output"));
    let before = std::fs::read(vault::path(f.home.path())).unwrap();
    let effect = material.acquire_effect().await.unwrap();
    for _ in 0..3 {
        effect.validate().unwrap();
    }
    assert_eq!(before, std::fs::read(vault::path(f.home.path())).unwrap());
    assert_eq!(
        access::pinned_test::events(f.home.path()),
        ["protection", "unseal", "resolve_locks_released"]
    );
    drop(effect);
    drop(
        crate::google_personal::grants::acquire(f.home.path())
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn pinned_selection_consent_aba_refuses_before_key_unseal_or_provider() {
    let f = Fixture::gmail(1);
    let selected = f.selected("gmail_read");
    f.db.set_google_consent(&f.user, "bot", false, &f.user)
        .unwrap();
    f.db.set_google_consent(&f.user, "bot", true, &f.user)
        .unwrap();
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), selected)
            .await
            .is_err()
    );
    assert!(access::pinned_test::events(f.home.path()).is_empty());
}

#[tokio::test]
async fn context_tool_mismatch_is_not_reselected_or_changed_to_home() {
    let f = Fixture::gmail(1);
    let selected = f.selected("gmail_read");
    assert!(
        resolve_pinned(f.dedicated("gmail_search"), selected)
            .await
            .is_err()
    );
    assert!(access::pinned_test::events(f.home.path()).is_empty());
}

#[tokio::test]
async fn revoke_uncertainty_refuses_before_protection_or_provider() {
    let f = Fixture::gmail(1);
    let selected = f.selected("gmail_read");
    std::fs::create_dir_all(f.home.path().join("locks")).unwrap();
    std::fs::write(
        f.home
            .path()
            .join("locks/google-grant-revoke-uncertain.json"),
        "{}",
    )
    .unwrap();
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), selected)
            .await
            .is_err()
    );
    assert!(access::pinned_test::events(f.home.path()).is_empty());
}

#[tokio::test]
async fn exact_refresh_revision_is_published_once_and_lease_order_is_separate() {
    let f = Fixture::gmail(1);
    access::pinned_test::refresh_result(
        f.home.path(),
        Ok(crate::google_personal::oauth::Tokens {
            secret: Secret {
                access_token: "fixture-fresh-never-output".into(),
                refresh_token: None,
                expires_at: Some(4_000_000_000),
            },
            granted_scopes: None,
        }),
    );
    let material = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
        .await
        .unwrap();
    assert!(material.fixture_token_matches("fixture-fresh-never-output"));
    assert_eq!(
        vault::get(f.home.path(), &f.user)
            .unwrap()
            .unwrap()
            .metadata
            .revision,
        2
    );
    let lease = crate::google_personal::grants::acquire(f.home.path())
        .await
        .unwrap();
    drop(lease); // Resolve returned without retaining its temporary refresh lease.
    let effect = material.acquire_effect().await.unwrap();
    effect.validate().unwrap();
    assert_eq!(
        access::pinned_test::events(f.home.path()),
        [
            "protection",
            "unseal",
            "refresh_grant",
            "provider",
            "refresh_published",
            "resolve_locks_released"
        ]
    );
    drop(effect);
}

#[tokio::test]
async fn refresh_error_is_final_and_never_uses_configured_home_fallback() {
    let f = Fixture::gmail(1);
    access::pinned_test::refresh_result(
        f.home.path(),
        Err(crate::google_personal::oauth::Error::InvalidGrant),
    );
    let result = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read")).await;
    assert!(result.is_err_and(|e| e.code == "needs_reconnect" && !e.fallback_eligible));
    assert_eq!(
        access::pinned_test::events(f.home.path())
            .iter()
            .filter(|&&e| e == "provider")
            .count(),
        1
    );
    assert_eq!(
        vault::get(f.home.path(), &f.user)
            .unwrap()
            .unwrap()
            .metadata
            .revision,
        1
    );
}

#[tokio::test]
async fn refreshed_secret_cannot_be_labelled_with_a_later_reload_revision() {
    let f = Fixture::gmail(1);
    access::pinned_test::refresh_result(
        f.home.path(),
        Ok(crate::google_personal::oauth::Tokens {
            secret: Secret {
                access_token: "fixture-fresh-never-output".into(),
                refresh_token: None,
                expires_at: Some(4_000_000_000),
            },
            granted_scopes: None,
        }),
    );
    let home = f.home.path().to_path_buf();
    let user = f.user.clone();
    access::pinned_test::after_publish(
        f.home.path(),
        Box::new(move || {
            let key = crate::google_identity::protection_gate(&home).unwrap();
            let old = vault::get(&home, &user).unwrap().unwrap();
            assert!(
                vault::finish_refresh(
                    &home,
                    &key,
                    &old.metadata,
                    &Secret {
                        access_token: "fixture-later-never-output".into(),
                        refresh_token: None,
                        expires_at: Some(4_000_000_000)
                    }
                )
                .unwrap()
            );
        }),
    );
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
            .await
            .is_err()
    );
    assert_eq!(
        vault::get(f.home.path(), &f.user)
            .unwrap()
            .unwrap()
            .metadata
            .revision,
        3
    );
    assert_eq!(
        access::pinned_test::events(f.home.path())
            .iter()
            .filter(|&&e| e == "provider")
            .count(),
        1
    );
}

#[tokio::test]
async fn a_material_waiting_for_effect_lease_does_not_follow_a_new_token_revision() {
    let f = Fixture::gmail(4_000_000_000);
    let material = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
        .await
        .unwrap();
    let blocking = crate::google_personal::grants::acquire(f.home.path())
        .await
        .unwrap();
    let waiting = material.acquire_effect();
    tokio::pin!(waiting);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );
    f.refresh();
    drop(blocking);
    assert!(waiting.await.is_err());
    drop(
        crate::google_personal::grants::acquire(f.home.path())
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn cancelled_effect_wait_releases_ownership_without_detached_work() {
    let f = Fixture::gmail(4_000_000_000);
    let material = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
        .await
        .unwrap();
    let blocking = crate::google_personal::grants::acquire(f.home.path())
        .await
        .unwrap();
    let mut waiting = Box::pin(material.acquire_effect());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );
    drop(waiting);
    drop(blocking);
    drop(
        crate::google_personal::grants::acquire(f.home.path())
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn remote_context_requires_full_scope_set_and_canonical_employee_directory() {
    let f = Fixture::gmail(4_000_000_000);
    let context = GoogleCallContext::remote_mcp(
        f.home.path(),
        &f.home.path().join("agents/bot"),
        RequesterIdentity::Unknown,
        Box::new(UserDbPeople(f.db.clone())),
        "mount-fixture".into(),
        "initialize-fixture".into(),
    )
    .unwrap();
    let selected = f.selected("gmail_read");
    assert!(resolve_pinned(context, selected).await.is_err());
    assert!(access::pinned_test::events(f.home.path()).is_empty());
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("agent.toml"), "[agent]\nname='bot'\n").unwrap();
    assert!(
        GoogleCallContext::remote_mcp(
            f.home.path(),
            elsewhere.path(),
            RequesterIdentity::Unknown,
            Box::new(UserDbPeople(f.db.clone())),
            "mount-fixture".into(),
            "initialize-fixture".into()
        )
        .is_err()
    );
}

#[tokio::test]
async fn each_missing_remote_scope_is_refused_without_unseal_or_refresh() {
    for missing in crate::google_workspace::REQUIRED_SCOPES {
        let scopes: Vec<String> = crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .filter(|s| *s != missing)
            .map(|s| (*s).to_owned())
            .collect();
        // Initial metadata capture must enforce each scope, rather than
        // obtaining a google_status receipt and merely failing a tool mismatch.
        let missing_fixture = Fixture::new(scopes.clone(), 1);
        let error = PersonalSelection::capture(
            missing_fixture.home.path(),
            "bot",
            &RequesterIdentity::Unknown,
            &UserDbPeople(missing_fixture.db.clone()),
            "google_remote_mcp",
        )
        .unwrap_err();
        assert_eq!(error.code, "scope_missing");
        assert!(access::pinned_test::events(missing_fixture.home.path()).is_empty());
        // A previously valid same-purpose selection must also fail after
        // a real metadata publication removes the individual scope.
        let f = Fixture::new(
            crate::google_workspace::REQUIRED_SCOPES
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            1,
        );
        let selected = f.selected("google_remote_mcp");
        let old = vault::get(f.home.path(), &f.user).unwrap().unwrap();
        let key = crate::google_identity::protection_gate(f.home.path()).unwrap();
        assert!(
            vault::finish_refresh_with_scopes(
                f.home.path(),
                &key,
                &old.metadata,
                &Secret {
                    access_token: "fixture-reduced-scope-never-output".into(),
                    refresh_token: None,
                    expires_at: Some(4_000_000_000)
                },
                Some(&scopes)
            )
            .unwrap()
        );
        let context = GoogleCallContext::remote_mcp(
            f.home.path(),
            &f.home.path().join("agents/bot"),
            RequesterIdentity::Unknown,
            Box::new(UserDbPeople(f.db.clone())),
            "mount-fixture".into(),
            "initialize-fixture".into(),
        )
        .unwrap();
        assert!(resolve_pinned(context, selected).await.is_err());
        assert!(access::pinned_test::events(f.home.path()).is_empty());
    }
}

#[test]
fn scoped_refresh_transport_explicitly_disables_protocol_retry_without_rewriting_legacy() {
    let source = include_str!("../google_personal/oauth.rs");
    let scoped = source
        .split("fn scoped_refresh_client(")
        .nth(1)
        .unwrap()
        .split("async fn response_json")
        .next()
        .unwrap();
    assert!(scoped.contains(".retry(reqwest::retry::never())"));
    assert!(scoped.contains("Policy::none()"));
    let legacy = source
        .split("fn client()")
        .nth(1)
        .unwrap()
        .split("fn scoped_refresh_client(")
        .next()
        .unwrap();
    assert!(!legacy.contains(".retry("));
    // This checks configuration and source wiring, not an HTTP/2 NACK wire fixture.
}

#[tokio::test]
async fn scope_change_during_refresh_invalidates_the_original_selection_even_if_still_permitted() {
    let f = Fixture::gmail(1);
    access::pinned_test::refresh_result(
        f.home.path(),
        Ok(crate::google_personal::oauth::Tokens {
            secret: Secret {
                access_token: "fixture-fresh-never-output".into(),
                refresh_token: None,
                expires_at: Some(4_000_000_000),
            },
            granted_scopes: Some(vec![
                "https://www.googleapis.com/auth/gmail.readonly".into(),
                "openid".into(),
            ]),
        }),
    );
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
            .await
            .is_err()
    );
    assert_eq!(
        access::pinned_test::events(f.home.path())
            .iter()
            .filter(|&&e| e == "provider")
            .count(),
        1
    );
    drop(
        crate::google_personal::grants::acquire(f.home.path())
            .await
            .unwrap(),
    );
}

#[tokio::test]
async fn full_remote_scope_authority_binds_the_exact_mount_and_operation() {
    let f = Fixture::new(
        crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        4_000_000_000,
    );
    let selected = f.selected("google_remote_mcp");
    let context = GoogleCallContext::remote_mcp(
        f.home.path(),
        &f.home.path().join("agents/bot"),
        RequesterIdentity::Unknown,
        Box::new(UserDbPeople(f.db.clone())),
        "mount-fixture".into(),
        "initialize-fixture".into(),
    )
    .unwrap();
    let effect = resolve_pinned(context, selected)
        .await
        .unwrap()
        .acquire_effect()
        .await
        .unwrap();
    effect
        .validate_remote("mount-fixture", "initialize-fixture")
        .unwrap();
    assert!(
        effect
            .validate_remote("other-mount", "initialize-fixture")
            .is_err()
    );
    assert!(
        effect
            .validate_remote("mount-fixture", "different-tool-call")
            .is_err()
    );
    assert!(
        effect
            .validate_dedicated(
                "google_status",
                &"a".repeat(64),
                &duduclaw_core::google_identity::CredentialRef {
                    kind: duduclaw_core::google_identity::CredentialKind::Legacy,
                    sha256: "a".repeat(64),
                }
            )
            .is_err()
    );
}

#[tokio::test]
async fn live_requester_is_not_promoted_to_owner_and_is_rechecked_without_unsealing() {
    let f = Fixture::new(
        crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        4_000_000_000,
    );
    std::fs::write(
        f.home.path().join("agents/bot/agent.toml"),
        "[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='owner_only'\n",
    )
    .unwrap();
    let who = RequesterIdentity::DashboardUser {
        user_id: f.user.clone(),
    };
    let selected = PersonalSelection::capture(
        f.home.path(),
        "bot",
        &who,
        &UserDbPeople(f.db.clone()),
        "google_remote_mcp",
    )
    .unwrap();
    let unknown = GoogleCallContext::remote_mcp(
        f.home.path(),
        &f.home.path().join("agents/bot"),
        RequesterIdentity::Unknown,
        Box::new(UserDbPeople(f.db.clone())),
        "mount-fixture".into(),
        "initialize-fixture".into(),
    )
    .unwrap();
    assert!(resolve_pinned(unknown, selected.clone()).await.is_err());
    assert!(access::pinned_test::events(f.home.path()).is_empty());
    let context = GoogleCallContext::remote_mcp(
        f.home.path(),
        &f.home.path().join("agents/bot"),
        who,
        Box::new(UserDbPeople(f.db.clone())),
        "mount-fixture".into(),
        "initialize-fixture".into(),
    )
    .unwrap();
    let effect = resolve_pinned(context, selected)
        .await
        .unwrap()
        .acquire_effect()
        .await
        .unwrap();
    let events = access::pinned_test::events(f.home.path());
    f.db.set_user_status(&f.user, duduclaw_auth::UserStatus::Suspended)
        .unwrap();
    assert!(effect.validate().is_err());
    assert_eq!(access::pinned_test::events(f.home.path()), events);
}

#[tokio::test]
async fn primary_owner_aba_does_not_replace_the_original_selected_person() {
    let f = Fixture::gmail(1);
    let bob =
        f.db.create_user(
            "bob@unit.test",
            "Bob",
            "fixture-password-only",
            UserRole::Employee,
        )
        .unwrap();
    let admin =
        f.db.create_user(
            "admin@unit.test",
            "Admin",
            "fixture-password-only",
            UserRole::Admin,
        )
        .unwrap();
    f.db.bind_agent(&bob.id, "bot", AccessLevel::Owner).unwrap();
    f.db.set_google_primary_owner("bot", &f.user, &admin.id)
        .unwrap();
    let selected = f.selected("gmail_read");
    f.db.set_google_primary_owner("bot", &bob.id, &admin.id)
        .unwrap();
    f.db.set_google_primary_owner("bot", &f.user, &admin.id)
        .unwrap();
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), selected)
            .await
            .is_err()
    );
    assert!(access::pinned_test::events(f.home.path()).is_empty());
}

#[tokio::test]
async fn public_visitor_requester_is_never_replaced_with_a_cached_owner() {
    let f = Fixture::new(
        crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        1,
    );
    let selected = f.selected("google_remote_mcp");
    for channel in ["widget", "odoolivechat"] {
        let who = RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: "fixture".into(),
            sender: "fixture".into(),
            user_id: Some(f.user.clone()),
        };
        let context = GoogleCallContext::remote_mcp(
            f.home.path(),
            &f.home.path().join("agents/bot"),
            who,
            Box::new(UserDbPeople(f.db.clone())),
            "mount-fixture".into(),
            "initialize-fixture".into(),
        )
        .unwrap();
        assert!(resolve_pinned(context, selected.clone()).await.is_err());
    }
    assert!(access::pinned_test::events(f.home.path()).is_empty());
}

#[tokio::test]
async fn cancelled_refresh_mutex_wait_never_unseals_and_the_next_call_can_proceed() {
    let f = Fixture::gmail(4_000_000_000);
    let blocked = access::pinned_test::hold_refresh(f.home.path(), &f.user).await;
    let mut waiting = Box::pin(resolve_pinned(
        f.dedicated("gmail_read"),
        f.selected("gmail_read"),
    ));
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );
    assert!(access::pinned_test::events(f.home.path()).is_empty());
    drop(waiting);
    drop(blocked);
    resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
        .await
        .unwrap();
}

#[tokio::test]
async fn late_refresh_cas_loss_returns_no_material_and_does_not_retry_or_overwrite() {
    let f = Fixture::gmail(1);
    access::pinned_test::refresh_result(
        f.home.path(),
        Ok(crate::google_personal::oauth::Tokens {
            secret: Secret {
                access_token: "fixture-fresh-never-output".into(),
                refresh_token: None,
                expires_at: Some(4_000_000_000),
            },
            granted_scopes: None,
        }),
    );
    let home = f.home.path().to_path_buf();
    let user = f.user.clone();
    access::pinned_test::set_before_publish(
        f.home.path(),
        Box::new(move || {
            let key = crate::google_identity::protection_gate(&home).unwrap();
            let old = vault::get(&home, &user).unwrap().unwrap();
            assert!(
                vault::finish_refresh(
                    &home,
                    &key,
                    &old.metadata,
                    &Secret {
                        access_token: "fixture-winning-cas-never-output".into(),
                        refresh_token: None,
                        expires_at: Some(4_000_000_000),
                    }
                )
                .unwrap()
            );
        }),
    );
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
            .await
            .is_err()
    );
    assert_eq!(
        access::pinned_test::events(f.home.path())
            .iter()
            .filter(|&&e| e == "provider")
            .count(),
        1
    );
    let actual = vault::get(f.home.path(), &f.user).unwrap().unwrap();
    assert_eq!(actual.metadata.revision, 2);
    let key = crate::google_identity::protection_gate(f.home.path()).unwrap();
    assert!(
        vault::unseal(&key, &actual).unwrap().access_token == "fixture-winning-cas-never-output"
    );
}

#[tokio::test]
async fn dedicated_effect_validates_final_digest_tool_and_credential_without_exposing_a_token() {
    let f = Fixture::gmail(4_000_000_000);
    let context = f.dedicated("gmail_read");
    let GooglePurpose::Dedicated {
        final_arguments_digest,
        credential_ref,
        ..
    } = &context.purpose
    else {
        panic!("fixture purpose");
    };
    let digest = final_arguments_digest.clone();
    let reference = credential_ref.clone();
    let effect = resolve_pinned(context, f.selected("gmail_read"))
        .await
        .unwrap()
        .acquire_effect()
        .await
        .unwrap();
    effect
        .validate_dedicated("gmail_read", &digest, &reference)
        .unwrap();
    assert!(
        effect
            .validate_dedicated("gmail_search", &digest, &reference)
            .is_err()
    );
    assert!(
        effect
            .validate_dedicated("gmail_read", &"b".repeat(64), &reference)
            .is_err()
    );
    let mut changed = reference.clone();
    changed.sha256 = "c".repeat(64);
    assert!(
        effect
            .validate_dedicated("gmail_read", &digest, &changed)
            .is_err()
    );
    assert!(
        effect
            .validate_remote("mount-fixture", "initialize-fixture")
            .is_err()
    );
}

fn client_rows(f: &Fixture) -> Vec<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(f.home.path().join("mcp-oauth-configs.json")).unwrap())
        .unwrap()
}
fn write_client_rows(f: &Fixture, rows: &[serde_json::Value]) {
    std::fs::write(
        f.home.path().join("mcp-oauth-configs.json"),
        serde_json::to_vec(rows).unwrap(),
    )
    .unwrap();
}
fn client_digest(value: &str) -> String {
    use sha2::Digest;
    format!("{:x}", sha2::Sha256::digest(value.as_bytes()))
}
fn add_unrelated_encrypted_client(f: &Fixture) -> String {
    // A real fixture key encrypts Google A; invalid unrelated ciphertext must
    // never be passed to decrypt_value by this scoped call.
    let mut rows = client_rows(f);
    let google = rows
        .iter_mut()
        .find(|c| c["provider_id"] == "google")
        .unwrap();
    let cipher = format!(
        "enc:v1:{}",
        crate::config_crypto::encrypt_value("fixture-client-A", f.home.path()).unwrap()
    );
    google["client_secret"] = serde_json::json!(cipher);
    let mut other = google.clone();
    other["provider_id"] = serde_json::json!("unrelated-provider");
    other["client_id"] = serde_json::json!("unrelated-client");
    other["client_secret"] = serde_json::json!("enc:v1:invalid-unrelated-fixture");
    rows.push(other);
    write_client_rows(f, &rows);
    cipher
}
fn fake_successful_refresh(f: &Fixture) {
    access::pinned_test::refresh_result(
        f.home.path(),
        Ok(crate::google_personal::oauth::Tokens {
            secret: Secret {
                access_token: "fixture-fresh-never-output".into(),
                refresh_token: None,
                expires_at: Some(4_000_000_000),
            },
            granted_scopes: None,
        }),
    );
}
fn alternate_client_bytes(f: &Fixture) -> Vec<u8> {
    let mut rows = client_rows(f);
    let cipher = format!(
        "enc:v1:{}",
        crate::config_crypto::encrypt_value("fixture-client-B", f.home.path()).unwrap()
    );
    rows.iter_mut()
        .find(|c| c["provider_id"] == "google")
        .unwrap()["client_secret"] = serde_json::json!(cipher);
    serde_json::to_vec(&rows).unwrap()
}

#[tokio::test]
async fn fresh_pinned_client_metadata_reads_once_without_decrypting_any_provider() {
    use crate::google_personal::authority::pinned_client_test as probe;
    let f = Fixture::gmail(4_000_000_000);
    add_unrelated_encrypted_client(&f);
    let material = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
        .await
        .unwrap();
    assert!(material.fixture_token_matches("fixture-original-never-output"));
    let (reads, decrypts) = probe::counts(f.home.path());
    assert_eq!(reads, 1);
    assert!(decrypts.is_empty());
    assert_eq!(
        access::pinned_test::events(f.home.path())
            .iter()
            .filter(|&&e| e == "unseal")
            .count(),
        1
    );
}

#[tokio::test]
async fn refreshing_pinned_client_decrypts_only_owned_google_ciphertext_once() {
    use crate::google_personal::authority::pinned_client_test as probe;
    let f = Fixture::gmail(1);
    let stored_a = add_unrelated_encrypted_client(&f);
    fake_successful_refresh(&f);
    let material = resolve_pinned(f.dedicated("gmail_read"), f.selected("gmail_read"))
        .await
        .unwrap();
    assert!(material.fixture_token_matches("fixture-fresh-never-output"));
    assert_eq!(
        vault::get(f.home.path(), &f.user)
            .unwrap()
            .unwrap()
            .metadata
            .revision,
        2
    );
    let (reads, decrypts) = probe::counts(f.home.path());
    assert_eq!(reads, 1);
    assert_eq!(decrypts.len(), 1);
    assert_eq!(decrypts[0].provider, "google");
    assert_eq!(decrypts[0].stored_digest, client_digest(&stored_a));
    assert_eq!(
        decrypts[0].clear_digest,
        Some(client_digest("fixture-client-A"))
    );
}

#[tokio::test]
async fn replacement_before_owned_client_read_refuses_before_any_client_decrypt() {
    use crate::google_personal::authority::pinned_client_test as probe;
    let f = Fixture::gmail(1);
    add_unrelated_encrypted_client(&f);
    let selected = f.selected("gmail_read");
    let b = alternate_client_bytes(&f);
    let path = f.home.path().join("mcp-oauth-configs.json");
    probe::hook(
        f.home.path(),
        "before_read",
        Box::new(move || std::fs::write(path, b).unwrap()),
    );
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), selected)
            .await
            .is_err()
    );
    let (reads, decrypts) = probe::counts(f.home.path());
    assert_eq!(reads, 1);
    assert!(decrypts.is_empty());
    assert!(access::pinned_test::events(f.home.path()).is_empty());
}

#[tokio::test]
async fn replacement_after_owned_client_read_does_not_reopen_or_decrypt_the_replacement() {
    use crate::google_personal::authority::pinned_client_test as probe;
    let f = Fixture::gmail(1);
    add_unrelated_encrypted_client(&f);
    let selected = f.selected("gmail_read");
    let b = alternate_client_bytes(&f);
    let path = f.home.path().join("mcp-oauth-configs.json");
    probe::hook(
        f.home.path(),
        "after_read",
        Box::new(move || std::fs::write(path, b).unwrap()),
    );
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), selected)
            .await
            .is_err()
    );
    let (reads, decrypts) = probe::counts(f.home.path());
    assert_eq!(reads, 1);
    assert!(decrypts.is_empty());
    assert!(access::pinned_test::events(f.home.path()).is_empty());
}

#[tokio::test]
async fn replacement_at_decrypt_boundary_still_decrypts_owned_a_and_refuses_before_provider() {
    use crate::google_personal::authority::pinned_client_test as probe;
    let f = Fixture::gmail(1);
    let stored_a = add_unrelated_encrypted_client(&f);
    let selected = f.selected("gmail_read");
    let b = alternate_client_bytes(&f);
    let path = f.home.path().join("mcp-oauth-configs.json");
    probe::hook(
        f.home.path(),
        "before_decrypt",
        Box::new(move || std::fs::write(path, b).unwrap()),
    );
    assert!(
        resolve_pinned(f.dedicated("gmail_read"), selected)
            .await
            .is_err()
    );
    let (reads, decrypts) = probe::counts(f.home.path());
    assert_eq!(reads, 1);
    assert_eq!(decrypts.len(), 1);
    assert_eq!(decrypts[0].stored_digest, client_digest(&stored_a));
    assert_eq!(
        decrypts[0].clear_digest,
        Some(client_digest("fixture-client-A"))
    );
    assert!(!access::pinned_test::events(f.home.path()).contains(&"provider"));
}

#[tokio::test]
async fn malformed_or_duplicate_google_metadata_refuses_even_without_refresh() {
    use crate::google_personal::authority::pinned_client_test as probe;
    for duplicate in [false, true] {
        let f = Fixture::gmail(4_000_000_000);
        let mut rows = client_rows(&f);
        if duplicate {
            rows.push(rows[0].clone());
        } else {
            rows[0]["client_secret"] = serde_json::json!(7);
        }
        write_client_rows(&f, &rows);
        let selected = f.selected("gmail_read"); // Legacy metadata capture only checks client id.
        assert!(
            resolve_pinned(f.dedicated("gmail_read"), selected)
                .await
                .is_err()
        );
        let (reads, decrypts) = probe::counts(f.home.path());
        assert_eq!(reads, 1);
        assert!(decrypts.is_empty());
        assert!(access::pinned_test::events(f.home.path()).is_empty());
    }
}

#[tokio::test]
async fn unreadable_malformed_and_oversized_owned_client_reads_fail_closed() {
    use crate::google_personal::authority::pinned_client_test as probe;
    for mode in ["missing", "malformed", "oversize"] {
        let f = Fixture::gmail(1);
        let selected = f.selected("gmail_read");
        let path = f.home.path().join("mcp-oauth-configs.json");
        probe::hook(
            f.home.path(),
            "before_read",
            Box::new(move || match mode {
                "missing" => std::fs::remove_file(path).unwrap(),
                "malformed" => std::fs::write(path, b"not-json").unwrap(),
                "oversize" => std::fs::write(path, vec![b' '; 4 * 1024 * 1024 + 1]).unwrap(),
                _ => unreachable!(),
            }),
        );
        assert!(
            resolve_pinned(f.dedicated("gmail_read"), selected)
                .await
                .is_err()
        );
        let (reads, decrypts) = probe::counts(f.home.path());
        assert_eq!(reads, 1);
        assert!(decrypts.is_empty());
        assert!(access::pinned_test::events(f.home.path()).is_empty());
    }
}

#[tokio::test]
async fn a_failed_google_ciphertext_decrypt_never_uses_the_ciphertext_as_a_secret() {
    use crate::google_personal::authority::pinned_client_test as probe;
    for (stored, expected_decrypts) in [
        ("enc:v1:invalid-selected-fixture", 1),
        ("enc:v2:unsupported-fixture", 0),
    ] {
        let f = Fixture::gmail(1);
        let mut rows = client_rows(&f);
        rows[0]["client_secret"] = serde_json::json!(stored);
        write_client_rows(&f, &rows);
        let selected = f.selected("gmail_read");
        assert!(
            resolve_pinned(f.dedicated("gmail_read"), selected)
                .await
                .is_err()
        );
        let (reads, decrypts) = probe::counts(f.home.path());
        assert_eq!(reads, 1);
        assert_eq!(decrypts.len(), expected_decrypts);
        assert!(decrypts.iter().all(|d| d.clear_digest.is_none()));
        assert!(!access::pinned_test::events(f.home.path()).contains(&"provider"));
    }
}

fn host_home_pending(
    f: &Fixture,
) -> (
    super::PendingGoogleCall,
    duduclaw_core::google_identity::CredentialRef,
    String,
) {
    vault::disconnect(f.home.path(), &f.user).unwrap();
    std::fs::write(f.home.path().join("mcp-oauth-tokens.json"), serde_json::to_vec(&serde_json::json!([{"provider_id":"google","access_token":"owned-c2-home-access","refresh_token":null,"expires_at":null,"scopes":["https://www.googleapis.com/auth/gmail.readonly"]}])).unwrap()).unwrap();
    host_home_pending_same_lineage(f)
}
fn host_home_pending_same_lineage(
    f: &Fixture,
) -> (
    super::PendingGoogleCall,
    duduclaw_core::google_identity::CredentialRef,
    String,
) {
    use crate::google_identity::effect_authority::host_claim::{Clock, SelectionStore};
    let reference = duduclaw_core::google_identity::CredentialRef {
        kind: duduclaw_core::google_identity::CredentialKind::Legacy,
        sha256: client_digest("ddc_prod_0123456789abcdef0123456789abcdef"),
    };
    let args = serde_json::json!({"message_id":"owned"});
    let store = SelectionStore::default();
    let stamp = store
        .select_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            "gmail_read",
            &args,
            Clock::now(),
        )
        .unwrap()
        .unwrap();
    store
        .bind_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            "gmail_read",
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            &args,
            Clock::now(),
        )
        .unwrap();
    let pending = store
        .claim_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            "gmail_read",
            &stamp.opaque_id,
            &args,
            Clock::now(),
        )
        .unwrap();
    (
        pending,
        reference,
        duduclaw_core::google_identity::google_arguments_digest(&args),
    )
}
#[tokio::test]
async fn c2_real_claim_producer_borrows_only_the_same_owned_material_and_drops_lease() {
    let f = Fixture::gmail(4_000_000_000);
    let (pending, reference, digest) = host_home_pending(&f);
    let first = pending
        .resolve()
        .await
        .unwrap()
        .acquire_effect()
        .await
        .unwrap();
    let (pending, _, _) = host_home_pending_same_lineage(&f);
    let second = pending.resolve().await.unwrap();
    let Material::HomeOAuth(second_material) = &second.selected else {
        panic!("Home material");
    };
    let permit = first
        .home_permit_for_native("gmail_read", &digest, &reference)
        .unwrap();
    assert!(second_material.borrow_for_effect(permit).is_err());
    {
        let view = first
            .borrow_home_native("gmail_read", &digest, &reference)
            .unwrap();
        view.revalidate().unwrap();
    }
    assert!(
        first
            .borrow_home_native("gmail_search", &digest, &reference)
            .is_err()
    );
    drop(first);
    drop(
        crate::google_personal::grants::acquire(f.home.path())
            .await
            .unwrap(),
    );
}
#[tokio::test]
async fn c2_effect_validation_and_borrow_do_not_refresh_unseal_or_take_new_grants() {
    let f = Fixture::gmail(4_000_000_000);
    let (pending, reference, digest) = host_home_pending(&f);
    let effect = pending
        .resolve()
        .await
        .unwrap()
        .acquire_effect()
        .await
        .unwrap();
    let probe = crate::mcp_oauth::home_store::test_probe::Guard::install(f.home.path());
    let before = std::fs::read(f.home.path().join("mcp-oauth-tokens.json")).unwrap();
    for _ in 0..3 {
        effect.validate().unwrap();
        effect
            .borrow_home_native("gmail_read", &digest, &reference)
            .unwrap()
            .revalidate()
            .unwrap();
    }
    assert_eq!(probe.counts(), [0, 0]);
    assert_eq!(
        before,
        std::fs::read(f.home.path().join("mcp-oauth-tokens.json")).unwrap()
    );
    let mut raw: serde_json::Value = serde_json::from_slice(&before).unwrap();
    raw[0]["google_home"]["credential"]["token_revision"] = serde_json::json!(2);
    std::fs::write(
        f.home.path().join("mcp-oauth-tokens.json"),
        serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    assert!(effect.validate().is_err());
    assert!(
        effect
            .borrow_home_native("gmail_read", &digest, &reference)
            .is_err()
    );
}
#[test]
fn c2_owned_authority_and_borrowed_view_types_require_real_send_sync() {
    fn send_sync<T: Send + Sync>() {}
    fn send_static<T: Send + 'static>() {}
    send_sync::<GoogleEffectAuthority>();
    send_sync::<super::PendingGoogleCall>();
    send_static::<super::PendingGoogleCall>();
    send_sync::<crate::google_identity::home_source::HomeOAuthResolved>();
    send_sync::<crate::google_identity::home_source::HomeOAuthMaterialProof>();
    send_sync::<crate::google_identity::home_source::HomeOAuthBearer<'static>>();
    fn future_send<F: std::future::Future + Send>(_: F) {}
    fn pending_resolve_is_send(call: super::PendingGoogleCall) {
        future_send(call.resolve());
    }
    fn borrowed_view_across_await_is_send(
        authority: GoogleEffectAuthority,
        reference: duduclaw_core::google_identity::CredentialRef,
        digest: String,
    ) {
        future_send(async move {
            let view = authority
                .borrow_home_native("gmail_read", &digest, &reference)
                .unwrap();
            std::future::ready(()).await;
            view.revalidate().unwrap();
        });
    }
    let _: fn(super::PendingGoogleCall) = pending_resolve_is_send;
    let _: fn(GoogleEffectAuthority, duduclaw_core::google_identity::CredentialRef, String) =
        borrowed_view_across_await_is_send;
}

#[tokio::test]
async fn true_healthy_claim_resolution_failure_never_prepares_or_resolves_home() {
    use crate::google_identity::effect_authority::host_claim::{Clock, SelectionStore};
    let f = Fixture::gmail(4_000_000_000);
    let reference = duduclaw_core::google_identity::CredentialRef {
        kind: duduclaw_core::google_identity::CredentialKind::Legacy,
        sha256: client_digest("ddc_prod_0123456789abcdef0123456789abcdef"),
    };
    let store = SelectionStore::default();
    let args = serde_json::json!({"message_id":"owned"});
    let stamp = store
        .select_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            "gmail_read",
            &args,
            Clock::now(),
        )
        .unwrap()
        .unwrap();
    store
        .bind_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            "gmail_read",
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            &args,
            Clock::now(),
        )
        .unwrap();
    let pending = store
        .claim_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            "gmail_read",
            &stamp.opaque_id,
            &args,
            Clock::now(),
        )
        .unwrap();
    let before = std::fs::read(f.home.path().join("mcp-oauth-tokens.json")).ok();
    let path = vault::path(f.home.path());
    let mut personal: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    personal["records"][0]["secret"] = serde_json::json!("pk:v1:invalid-owned-cipher");
    std::fs::write(path, serde_json::to_vec(&personal).unwrap()).unwrap();
    assert!(pending.resolve().await.is_err_and(|e| !e.fallback_eligible));
    assert_eq!(
        before,
        std::fs::read(f.home.path().join("mcp-oauth-tokens.json")).ok()
    );
}
#[tokio::test]
async fn true_home_material_waiting_for_effect_grant_rejects_new_revision_and_releases_waiter() {
    let f = Fixture::gmail(4_000_000_000);
    let (pending, _, _) = host_home_pending(&f);
    let material = pending.resolve().await.unwrap();
    let held = crate::google_personal::grants::acquire(f.home.path())
        .await
        .unwrap();
    let mut waiting = Box::pin(material.acquire_effect());
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut waiting)
            .await
            .is_err()
    );
    let path = f.home.path().join("mcp-oauth-tokens.json");
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw[0]["google_home"]["credential"]["token_revision"] = serde_json::json!(2);
    std::fs::write(path, serde_json::to_vec(&raw).unwrap()).unwrap();
    drop(held);
    assert!(waiting.await.is_err());
    drop(
        crate::google_personal::grants::acquire(f.home.path())
            .await
            .unwrap(),
    );
}

// D1 owned-wire tests. Source only; not yet compiled or executed.
const D1_SECRET: &str = "fixture-original-never-output";
struct D1Reply {
    status: u16,
    body: String,
    location: Option<String>,
    delay_ms: u64,
    disconnect: bool,
}
impl D1Reply {
    fn json(value: serde_json::Value) -> Self {
        Self {
            status: 200,
            body: value.to_string(),
            location: None,
            delay_ms: 0,
            disconnect: false,
        }
    }
}
struct D1Wire {
    headers: Arc<std::sync::Mutex<Vec<String>>>,
    endpoint: String,
    hits: Arc<std::sync::atomic::AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<(String, String, String)>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for D1Wire {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn d1_wire(respond: impl Fn(usize, &str) -> D1Reply + Send + Sync + 'static) -> D1Wire {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = hits.clone();
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let saved = requests.clone();
    let headers = Arc::new(std::sync::Mutex::new(Vec::new()));
    let saved_headers = headers.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut bytes = Vec::new();
            let mut block = [0; 4096];
            let header_end;
            loop {
                let Ok(n) = stream.read(&mut block).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&block[..n]);
                assert!(bytes.len() <= 1024 * 1024 + 16384);
                if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = i + 4;
                    break;
                }
            }
            let header = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
            let length = header
                .lines()
                .find_map(|l| {
                    let (k, v) = l.split_once(':')?;
                    k.eq_ignore_ascii_case("content-length")
                        .then(|| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(length <= 1024 * 1024);
            while bytes.len() - header_end < length {
                let Ok(n) = stream.read(&mut block).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&block[..n]);
            }
            let first = header.lines().next().unwrap();
            let mut fields = first.split_whitespace();
            let method = fields.next().unwrap().to_owned();
            let target = fields.next().unwrap().to_owned();
            saved_headers.lock().unwrap().push(header.to_owned());
            saved.lock().unwrap().push((
                method,
                target.clone(),
                String::from_utf8_lossy(&bytes[header_end..]).into_owned(),
            ));
            let index = count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let reply = respond(index, &target);
            if reply.delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(reply.delay_ms)).await;
            }
            if reply.disconnect {
                drop(stream);
                continue;
            }
            let extra = reply
                .location
                .as_ref()
                .map(|v| format!("Location: {v}\r\n"))
                .unwrap_or_default();
            let head = format!(
                "HTTP/1.1 {} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
                reply.status,
                reply.body.len(),
                extra
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(reply.body.as_bytes()).await;
        }
    });
    D1Wire {
        headers,
        endpoint,
        hits,
        requests,
        task,
    }
}
fn d1_fixture(home: bool) -> Fixture {
    let f = Fixture::new(
        crate::google_workspace::REQUIRED_SCOPES
            .iter()
            .map(|s| s.to_string())
            .collect(),
        4_000_000_000,
    );
    let config = std::fs::read_to_string(f.home.path().join("config.toml"))
        .unwrap()
        .replace(
            "scopes=['google:read']",
            "scopes=['google:read','google:write']",
        );
    std::fs::write(f.home.path().join("config.toml"), config).unwrap();
    if home {
        vault::disconnect(f.home.path(), &f.user).unwrap();
        std::fs::write(f.home.path().join("mcp-oauth-tokens.json"),serde_json::to_vec(&serde_json::json!([{"provider_id":"google","access_token":D1_SECRET,"refresh_token":null,"expires_at":null,"scopes":crate::google_workspace::REQUIRED_SCOPES}])).unwrap()).unwrap();
    }
    f
}
fn d1_pending(f: &Fixture, tool: &str, args: &serde_json::Value) -> super::PendingGoogleCall {
    let store = SelectionStore::default();
    let reference = duduclaw_core::google_identity::CredentialRef {
        kind: duduclaw_core::google_identity::CredentialKind::Legacy,
        sha256: client_digest("ddc_prod_0123456789abcdef0123456789abcdef"),
    };
    let stamp = store
        .select_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            tool,
            args,
            Clock::now(),
        )
        .unwrap()
        .unwrap();
    store
        .bind_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            tool,
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            args,
            Clock::now(),
        )
        .unwrap();
    store
        .claim_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            tool,
            &stamp.opaque_id,
            args,
            Clock::now(),
        )
        .unwrap()
}
fn d1_cases() -> Vec<(&'static str, serde_json::Value)> {
    use serde_json::json;
    vec![
        ("gmail_search", json!({"query":"q"})),
        ("gmail_read", json!({"message_id":"abc"})),
        (
            "gmail_create_draft",
            json!({"to":"a@b.test","subject":"subject","body":"body"}),
        ),
        ("calendar_list_events", json!({})),
        (
            "calendar_create_event",
            json!({"summary":"s","start":"2026-08-01T00:00:00Z","end":"2026-08-01T01:00:00Z"}),
        ),
        ("sheets_read", json!({"spreadsheet_id":"s","range":"A1"})),
        (
            "sheets_append",
            json!({"spreadsheet_id":"s","range":"A1","values":["x"]}),
        ),
        ("forms_get", json!({"form_id":"f"})),
        ("forms_list_responses", json!({"form_id":"f"})),
        ("gtasks_lists", json!({})),
        ("gtasks_list", json!({})),
        ("gtasks_create", json!({"title":"t"})),
        ("gtasks_complete", json!({"task_id":"t"})),
        ("drive_search", json!({"query":"q"})),
        ("drive_read", json!({"file_id":"d"})),
        ("docs_read", json!({"document_id":"d"})),
        ("docs_append", json!({"document_id":"d","text":"x"})),
        ("slides_read", json!({"presentation_id":"p"})),
    ]
}

#[tokio::test]
async fn d1_both_real_claim_owners_execute_all_eighteen_native_tools() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = d1_wire(|_, _| D1Reply::json(serde_json::json!({}))).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        for (tool, args) in d1_cases() {
            let value = d1_pending(&f, tool, &args)
                .execute_native(args)
                .await
                .unwrap();
            assert_ne!(
                value.get("isError"),
                Some(&serde_json::json!(true)),
                "{tool}"
            );
            assert!(!value.to_string().contains(D1_SECRET));
        }
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 18);
        assert_eq!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).len(),
            18
        );
    }
}

#[tokio::test]
async fn d1_decoded_reflections_are_clean_before_the_only_owner_output() {
    use base64::Engine;
    for home in [false, true] {
        let f = d1_fixture(home);
        for (tool, args, reply) in [
            (
                "gmail_read",
                serde_json::json!({"message_id":"abc"}),
                serde_json::json!({"snippet":D1_SECRET,"ignored":D1_SECRET,"payload":{"mimeType":"text/plain","body":{"data":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("prefix {D1_SECRET} suffix"))}}}),
            ),
            (
                "gmail_read",
                serde_json::json!({"message_id":"abc"}),
                serde_json::json!({"payload":{"mimeType":"text/html","body":{"data":base64::engine::general_purpose::URL_SAFE.encode("fixture-original-<b>never</b>-output")}}}),
            ),
            (
                "docs_read",
                serde_json::json!({"document_id":"d"}),
                serde_json::json!({"body":{"content":[{"paragraph":{"elements":[{"textRun":{"content":"fixture-original-"}},{"textRun":{"content":"never-output"}}]}}]}}),
            ),
            (
                "slides_read",
                serde_json::json!({"presentation_id":"p"}),
                serde_json::json!({"slides":[{"pageElements":[{"shape":{"text":{"textElements":[{"textRun":{"content":"fixture-original-"}},{"textRun":{"content":"never-output"}}]}}}]}]}),
            ),
            (
                "gmail_read",
                serde_json::json!({"message_id":"abc"}),
                serde_json::json!({"payload":{"mimeType":"text/plain","body":{"data":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("{}{D1_SECRET}","x".repeat(7990)))}}}),
            ),
        ] {
            let wire = d1_wire(move |_, _| D1Reply::json(reply.clone())).await;
            let fixture = crate::google_workspace::native_call::testing::install(
                f.home.path(),
                &wire.endpoint,
            );
            let value = d1_pending(&f, tool, &args)
                .execute_native(args)
                .await
                .unwrap();
            assert!(!value.to_string().contains(D1_SECRET));
            assert!(!value.to_string().contains("fixture-original-never"));
            let seen = crate::google_workspace::native_call::testing::observations(f.home.path());
            assert_eq!(seen.len(), 1);
            assert!(!seen[0].to_string().contains(D1_SECRET));
            drop(fixture);
        }
    }
}

#[tokio::test]
async fn d1_json_escaped_error_reflections_are_scrubbed_before_error_truncation() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let message = format!("{}{}", "x".repeat(195), D1_SECRET);
        let raw = serde_json::json!({"error":{"message":message},"ignored":D1_SECRET})
            .to_string()
            .replace("fixture", "\\u0066ixture");
        let wire = d1_wire(move |_, _| D1Reply {
            status: 403,
            body: raw.clone(),
            location: None,
            delay_ms: 0,
            disconnect: false,
        })
        .await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"message_id":"abc"});
        let value = d1_pending(&f, "gmail_read", &args)
            .execute_native(args)
            .await
            .unwrap();
        assert_eq!(value["isError"], true);
        assert!(!value.to_string().contains(D1_SECRET));
        assert!(!value.to_string().contains("fixture-original"));
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn d1_secret_derived_identifier_never_creates_the_second_request() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire =
            d1_wire(|_, _| D1Reply::json(serde_json::json!({"messages":[{"id":D1_SECRET}]}))).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"query":"q"});
        let error = d1_pending(&f, "gmail_search", &args)
            .execute_native(args)
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "result_unconfirmed");
        assert!(!error.message_zh.contains(D1_SECRET));
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).is_empty()
        );
    }
}

#[tokio::test]
async fn d1_every_wire_and_codec_boundary_rechecks_original_authority() {
    for home in [false, true] {
        for stage in [
            "before_builder",
            "before_send",
            "after_send",
            "after_chunk",
            "after_eof",
            "after_decode",
            "after_program",
            "after_scrub",
        ] {
            let f = d1_fixture(home);
            let wire =
                d1_wire(|_, _| D1Reply::json(serde_json::json!({"snippet":"should not escape"})))
                    .await;
            let _fixture = crate::google_workspace::native_call::testing::install(
                f.home.path(),
                &wire.endpoint,
            );
            let args = serde_json::json!({"message_id":"abc"});
            let pending = d1_pending(&f, "gmail_read", &args);
            let config = f.home.path().join("config.toml");
            crate::google_workspace::native_call::testing::hook(
                f.home.path(),
                stage,
                Box::new(move || {
                    let text = std::fs::read_to_string(&config)
                        .unwrap()
                        .replace("google_workspace=true", "google_workspace=false");
                    std::fs::write(config, text).unwrap();
                }),
            );
            let error = pending.execute_native(args).await.err().unwrap();
            if stage.starts_with("before_") {
                assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
                assert_eq!(error.code, "disabled");
            } else {
                assert_eq!(error.code, "result_unconfirmed");
            }
            assert!(
                crate::google_workspace::native_call::testing::observations(f.home.path())
                    .is_empty()
            );
        }
    }
}

#[tokio::test]
async fn d1_reads_and_every_write_make_one_attempt_and_do_not_follow_redirects() {
    for home in [false, true] {
        let f = d1_fixture(home);
        for (tool, args) in d1_cases().into_iter().filter(|(tool, _)| {
            matches!(
                *tool,
                "gmail_read"
                    | "gmail_create_draft"
                    | "calendar_create_event"
                    | "sheets_append"
                    | "gtasks_create"
                    | "gtasks_complete"
                    | "docs_append"
            )
        }) {
            for status in [0, 429, 500, 302] {
                let wire = d1_wire(move |_, _| D1Reply {
                    status,
                    body: "{}".into(),
                    location: Some("https://unexpected.invalid/not-allowed".into()),
                    delay_ms: 0,
                    disconnect: status == 0,
                })
                .await;
                let fixture = crate::google_workspace::native_call::testing::install(
                    f.home.path(),
                    &wire.endpoint,
                );
                let result = d1_pending(&f, tool, &args)
                    .execute_native(args.clone())
                    .await;
                if tool == "gmail_read" && status != 0 {
                    assert_eq!(result.unwrap()["isError"], true);
                } else {
                    assert_eq!(result.err().unwrap().code, "result_unconfirmed");
                }
                assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
                drop(fixture);
            }
        }
    }
}

#[tokio::test]
async fn d1_invalid_args_and_missing_owned_fixture_never_resolve_material() {
    let f = d1_fixture(false);
    for args in [
        serde_json::json!({}),
        serde_json::json!({"message_id":"../other"}),
    ] {
        let pending = d1_pending(&f, "gmail_read", &args);
        let before = access::pinned_test::events(f.home.path());
        let value = pending.execute_native(args).await.unwrap();
        assert_eq!(value["isError"], true);
        assert_eq!(before, access::pinned_test::events(f.home.path()));
    }
    let args = serde_json::json!({"message_id":"abc"});
    let before = access::pinned_test::events(f.home.path());
    let error = d1_pending(&f, "gmail_read", &args)
        .execute_native(args)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "personal_external_unsupported");
    assert_eq!(before, access::pinned_test::events(f.home.path()));
}

#[tokio::test]
async fn d1_cancellation_drops_the_only_effect_lease_without_a_detached_sender() {
    let f = d1_fixture(false);
    let wire = d1_wire(|_, _| D1Reply {
        status: 200,
        body: "{}".into(),
        location: None,
        delay_ms: 10000,
        disconnect: false,
    })
    .await;
    let _fixture =
        crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
    let args = serde_json::json!({"message_id":"abc"});
    let pending = d1_pending(&f, "gmail_read", &args);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            pending.execute_native(args)
        )
        .await
        .is_err()
    );
    assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    let lease = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::google_personal::grants::acquire(f.home.path()),
    )
    .await
    .unwrap()
    .unwrap();
    drop(lease);
    assert!(crate::google_workspace::native_call::testing::observations(f.home.path()).is_empty());
}

#[tokio::test]
async fn d1_ignored_secret_keys_never_escape_and_curated_secret_keys_refuse() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let mut ignored = serde_json::json!({"snippet":"safe"});
        ignored[D1_SECRET] = serde_json::json!("ignored value");
        let wire = d1_wire(move |_, _| D1Reply::json(ignored.clone())).await;
        let guard =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"message_id":"abc"});
        let value = d1_pending(&f, "gmail_read", &args)
            .execute_native(args)
            .await
            .unwrap();
        assert!(!value.to_string().contains(D1_SECRET));
        drop(guard);
        let mut answers = serde_json::Map::new();
        answers.insert(
            D1_SECRET.into(),
            serde_json::json!({"textAnswers":{"answers":[{"value":"safe"}]}}),
        );
        let raw = serde_json::json!({"responses":[{"answers":answers}]})
            .to_string()
            .replace("fixture", "\\u0066ixture");
        let wire = d1_wire(move |_, _| D1Reply {
            status: 200,
            body: raw.clone(),
            location: None,
            delay_ms: 0,
            disconnect: false,
        })
        .await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"form_id":"form"});
        let error = d1_pending(&f, "forms_list_responses", &args)
            .execute_native(args)
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "result_unconfirmed");
        assert!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).is_empty()
        );
    }
}

#[tokio::test]
async fn d1_normal_multistep_gmail_and_fixed_drive_export_are_real_counted_wire() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = d1_wire(|index, _| {
            if index == 0 {
                D1Reply::json(serde_json::json!({"messages":[{"id":"a"},{"id":"b"}]}))
            } else {
                D1Reply::json(serde_json::json!({"snippet":"safe"}))
            }
        })
        .await;
        let guard =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"query":"q"});
        let value = d1_pending(&f, "gmail_search", &args)
            .execute_native(args)
            .await
            .unwrap();
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert!(value.to_string().contains("safe"));
        drop(guard);
        let wire=d1_wire(|index,_|if index==0 {D1Reply::json(serde_json::json!({"name":"Title","mimeType":"application/vnd.google-apps.document","exportLinks":{"text/plain":"https://unexpected.invalid/steal"}}))}else{D1Reply{status:200,body:"safe document".into(),location:None,delay_ms:0,disconnect:false}}).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"file_id":"doc"});
        let value = d1_pending(&f, "drive_read", &args)
            .execute_native(args)
            .await
            .unwrap();
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 2);
        assert!(value.to_string().contains("safe document"));
        assert!(!value.to_string().contains("unexpected.invalid"));
        assert!(
            wire.requests.lock().unwrap()[1]
                .1
                .contains("/www.googleapis.com/drive/v3/files/doc/export")
        );
    }
}

#[tokio::test]
async fn d1_response_aggregate_fanout_recipe_and_result_limits_fail_without_partial_output() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = d1_wire(|_, _| D1Reply {
            status: 200,
            body: "x".repeat(4 * 1024 * 1024 + 1),
            location: None,
            delay_ms: 0,
            disconnect: false,
        })
        .await;
        let guard =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"message_id":"abc"});
        assert_eq!(
            d1_pending(&f, "gmail_read", &args)
                .execute_native(args)
                .await
                .err()
                .unwrap()
                .code,
            "result_unconfirmed"
        );
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(guard);
        let ids: Vec<_> = (0..26)
            .map(|n| serde_json::json!({"id":format!("m{n}")}))
            .collect();
        let wire = d1_wire(move |_, _| D1Reply::json(serde_json::json!({"messages":ids}))).await;
        let guard =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"query":"q"});
        assert_eq!(
            d1_pending(&f, "gmail_search", &args)
                .execute_native(args)
                .await
                .err()
                .unwrap()
                .code,
            "result_unconfirmed"
        );
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        drop(guard);
        let pad = "x".repeat(4 * 1024 * 1024 - 2048);
        let wire=d1_wire(move|index,_|if index==0 {D1Reply::json(serde_json::json!({"messages":[{"id":"a"},{"id":"b"},{"id":"c"},{"id":"d"},{"id":"e"}]}))}else{D1Reply::json(serde_json::json!({"ignored":pad}))}).await;
        let guard =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"query":"q"});
        assert_eq!(
            d1_pending(&f, "gmail_search", &args)
                .execute_native(args)
                .await
                .err()
                .unwrap()
                .code,
            "result_unconfirmed"
        );
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
        drop(guard);
        let wire = d1_wire(|_, _| D1Reply::json(serde_json::json!({}))).await;
        let guard =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"query":"x".repeat(1024*1024+1)});
        let before = access::pinned_test::events(f.home.path());
        let value = d1_pending(&f, "gmail_search", &args)
            .execute_native(args)
            .await
            .unwrap();
        assert_eq!(value["isError"], true);
        assert_eq!(before, access::pinned_test::events(f.home.path()));
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        drop(guard);
        let slides:Vec<_>=(0..40).map(|_|serde_json::json!({"pageElements":[{"shape":{"text":{"textElements":[{"textRun":{"content":"x".repeat(14000)}}]}}}]})).collect();
        let wire = d1_wire(move |_, _| D1Reply::json(serde_json::json!({"slides":slides}))).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"presentation_id":"p"});
        assert_eq!(
            d1_pending(&f, "slides_read", &args)
                .execute_native(args)
                .await
                .err()
                .unwrap()
                .code,
            "result_unconfirmed"
        );
        assert!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).is_empty()
        );
    }
}

async fn d1_signed_post(
    app: axum::Router,
    f: &Fixture,
    path: &str,
    value: serde_json::Value,
) -> (u16, serde_json::Value) {
    use tower::ServiceExt;
    const INTERNAL: &str = "ddc_prod_fedcba9876543210fedcba9876543210";
    use crate::computer_use_sessions::auth;
    let body = serde_json::to_vec(&value).unwrap();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let timestamp = chrono::Utc::now().timestamp().to_string();
    let identity = duduclaw_core::mint_identity_token(
        &duduclaw_core::load_identity_key(f.home.path()).unwrap(),
        "bot",
    );
    let signature = duduclaw_core::internal_request_signature(
        INTERNAL.as_bytes(),
        "bot",
        &identity,
        &timestamp,
        &nonce,
        &body,
    );
    let mut req = axum::http::Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json");
    for (name, value) in [
        (auth::AGENT_ID_HEADER, "bot"),
        (auth::TIMESTAMP_HEADER, timestamp.as_str()),
        (auth::NONCE_HEADER, nonce.as_str()),
        (auth::SIGNATURE_HEADER, signature.as_str()),
    ] {
        req = req.header(name, value);
    }
    let mut req = req.body(axum::body::Body::from(body)).unwrap();
    req.extensions_mut()
        .insert(axum::extract::ConnectInfo::<std::net::SocketAddr>(
            "127.0.0.1:12345".parse().unwrap(),
        ));
    let result = app.oneshot(req).await.unwrap();
    let status = result.status().as_u16();
    let bytes = axum::body::to_bytes(result.into_body(), 512 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn d1_signed_select_bind_execute_consumes_the_same_private_claim_and_sends_once() {
    use duduclaw_core::google_identity::{
        GoogleSelectionRequest, GoogleSelectionStamp, GOOGLE_SELECTION_ROUTE,
    };
    for home in [false, true] {
        let f = d1_fixture(home);
        duduclaw_core::ensure_identity_key(f.home.path()).unwrap();
        let config = f.home.path().join("config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(config,format!("{text}\n[mcp_keys.ddc_prod_fedcba9876543210fedcba9876543210]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n",chrono::Utc::now().to_rfc3339())).unwrap();
        let wire =
            d1_wire(|_, _| D1Reply::json(serde_json::json!({"snippet":"signed wire"}))).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let app = crate::google_identity::execute::router(Arc::new(
            crate::google_identity::execute::ExecuteState::new(
                f.home.path().to_owned(),
                Some(f.db.clone()),
            ),
        ));
        let reference = duduclaw_core::google_identity::CredentialRef {
            kind: duduclaw_core::google_identity::CredentialKind::Legacy,
            sha256: client_digest("ddc_prod_0123456789abcdef0123456789abcdef"),
        };
        let args = serde_json::json!({"message_id":"abc"});
        let select = GoogleSelectionRequest::SelectGoogleAccount {
            tool: "gmail_read".into(),
            arguments: args.clone(),
            credential_ref: reference.clone(),
        };
        let (status, response) = d1_signed_post(
            app.clone(),
            &f,
            GOOGLE_SELECTION_ROUTE,
            serde_json::to_value(select).unwrap(),
        )
        .await;
        assert_eq!(status, 200, "{response}");
        let stamp: GoogleSelectionStamp =
            serde_json::from_value(response["selection"].clone()).unwrap();
        let bind = GoogleSelectionRequest::BindGoogleArguments {
            opaque_id: stamp.opaque_id.clone(),
            tool: "gmail_read".into(),
            credential_ref: reference.clone(),
            original_arguments_digest: stamp.original_arguments_digest.clone(),
            account_binding_digest: stamp.approval.account_binding_digest.clone(),
            arguments: args.clone(),
        };
        let (status, response) = d1_signed_post(
            app.clone(),
            &f,
            GOOGLE_SELECTION_ROUTE,
            serde_json::to_value(bind).unwrap(),
        )
        .await;
        assert_eq!(status, 200, "{response}");
        let execute = serde_json::json!({"tool":"gmail_read","arguments":args,"credential_ref":reference,"selection_stamp":stamp.opaque_id});
        let (status, response) = d1_signed_post(
            app.clone(),
            &f,
            crate::google_identity::execute::ROUTE,
            execute.clone(),
        )
        .await;
        assert_eq!(status, 200, "{response}");
        assert!(response.to_string().contains("signed wire"));
        assert!(!response.to_string().contains(D1_SECRET));
        let (status, _) =
            d1_signed_post(app, &f, crate::google_identity::execute::ROUTE, execute).await;
        assert_eq!(status, 403);
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn d1_representationally_equal_but_differently_bound_arguments_never_open_material() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let original = serde_json::json!({"query":"q","max_results":10});
        for changed in [
            serde_json::json!({"query":"q","max_results":"10"}),
            serde_json::json!({"query":"q","max_results":10,"unused":true}),
            serde_json::json!({"query":"different","max_results":10}),
        ] {
            let pending = d1_pending(&f, "gmail_search", &original);
            let before = access::pinned_test::events(f.home.path());
            let error = pending.execute_native(changed).await.err().unwrap();
            assert_eq!(error.code, "identity_changed");
            assert_eq!(before, access::pinned_test::events(f.home.path()));
        }
    }
}

#[tokio::test]
async fn d1_same_scope_equal_digest_foreign_template_step_never_reaches_wire() {
    use crate::google_workspace::native_call::NativeCallTemplate;
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = d1_wire(|_, _| D1Reply::json(serde_json::json!({}))).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"message_id":"abc"});
        let expected = NativeCallTemplate::prepare("gmail_read", &args).unwrap();
        let other = NativeCallTemplate::prepare("gmail_read", &args).unwrap();
        assert_eq!(expected.digest(), other.digest());
        let authority = d1_pending(&f, "gmail_read", &args)
            .resolve()
            .await
            .unwrap()
            .acquire_effect()
            .await
            .unwrap();
        authority.validate_native_template(&other).unwrap();
        let refused = authority.test_foreign_native_step(&expected, &other).await;
        assert!(refused);
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).is_empty()
        );
    }
}

#[tokio::test]
async fn d1_maximum_gmail_fanout_is_twenty_six_requests_and_one_result() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire=d1_wire(|index,_|if index==0 {D1Reply::json(serde_json::json!({"messages":(0..25).map(|n|serde_json::json!({"id":format!("m{n}")})).collect::<Vec<_>>()}))}else{D1Reply::json(serde_json::json!({"snippet":"safe"}))}).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"query":"q","max_results":25});
        let value = d1_pending(&f, "gmail_search", &args)
            .execute_native(args)
            .await
            .unwrap();
        assert_ne!(value.get("isError"), Some(&serde_json::json!(true)));
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 26);
        assert_eq!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).len(),
            1
        );
    }
}

#[tokio::test]
async fn d1_e1_keeps_original_authority_after_the_completed_owner_output() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = d1_wire(|_, _| D1Reply::json(serde_json::json!({"snippet":"safe"}))).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        let args = serde_json::json!({"message_id":"abc"});
        let pending = d1_pending(&f, "gmail_read", &args);
        let config = f.home.path().join("config.toml");
        crate::google_workspace::native_call::testing::hook(
            f.home.path(),
            "e1_after_owner",
            Box::new(move || {
                let text = std::fs::read_to_string(&config)
                    .unwrap()
                    .replace("google_workspace=true", "google_workspace=false");
                std::fs::write(config, text).unwrap();
            }),
        );
        let error = pending.execute_native(args).await.err().unwrap();
        assert_eq!(error.code, "result_unconfirmed");
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
        // This observer is inside the owner before its completed value; the host
        // still refuses the sanitized completed result after authority changes.
        assert_eq!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).len(),
            1
        );
        let lease = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            crate::google_personal::grants::acquire(f.home.path()),
        )
        .await
        .unwrap()
        .unwrap();
        drop(lease);
    }
}

fn d1_r2_execute_audit_rows(f: &Fixture) -> Vec<serde_json::Value> {
    std::fs::read_to_string(f.home.path().join("tool_calls.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
        .filter(|row| row["tool_name"] == "google_execute")
        .collect()
}

#[tokio::test]
async fn d1_r2_preflight_failures_audit_once_without_resolving_or_sending() {
    use duduclaw_core::google_identity::{
        GoogleSelectionRequest, GoogleSelectionStamp, GOOGLE_SELECTION_ROUTE,
    };
    for home in [false, true] {
        let f = d1_fixture(home);
        duduclaw_core::ensure_identity_key(f.home.path()).unwrap();
        let config = f.home.path().join("config.toml");
        let text = std::fs::read_to_string(&config).unwrap();
        std::fs::write(config,format!("{text}\n[mcp_keys.ddc_prod_fedcba9876543210fedcba9876543210]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n",chrono::Utc::now().to_rfc3339())).unwrap();
        let app = crate::google_identity::execute::router(Arc::new(
            crate::google_identity::execute::ExecuteState::new(
                f.home.path().to_owned(),
                Some(f.db.clone()),
            ),
        ));
        let wire = d1_wire(|_, _| D1Reply::json(serde_json::json!({}))).await;
        let _fixture =
            crate::google_workspace::native_call::testing::install(f.home.path(), &wire.endpoint);
        for (index, tool, args, reason) in [
            (
                0,
                "gmail_read",
                serde_json::json!({"unused":"private_preflight_payload"}),
                "native_arguments_invalid",
            ),
            (
                1,
                "gmail_read",
                serde_json::json!({"message_id":"../other"}),
                "native_arguments_invalid",
            ),
            (
                2,
                "gmail_search",
                serde_json::json!({"query":"X".repeat(1024*1024+1)}),
                "native_recipe_too_large",
            ),
        ] {
            let before = d1_r2_execute_audit_rows(&f);
            let events = access::pinned_test::events(f.home.path());
            let value = if index == 0 {
                let reference = duduclaw_core::google_identity::CredentialRef {
                    kind: duduclaw_core::google_identity::CredentialKind::Legacy,
                    sha256: client_digest("ddc_prod_0123456789abcdef0123456789abcdef"),
                };
                let select = GoogleSelectionRequest::SelectGoogleAccount {
                    tool: tool.into(),
                    // Selection validates the original native request. Final
                    // approved arguments deliberately exercise execute's error.
                    arguments: serde_json::json!({"message_id":"owned-original"}),
                    credential_ref: reference.clone(),
                };
                let (status, response) = d1_signed_post(
                    app.clone(),
                    &f,
                    GOOGLE_SELECTION_ROUTE,
                    serde_json::to_value(select).unwrap(),
                )
                .await;
                assert_eq!(status, 200);
                let stamp: GoogleSelectionStamp =
                    serde_json::from_value(response["selection"].clone()).unwrap();
                let bind = GoogleSelectionRequest::BindGoogleArguments {
                    opaque_id: stamp.opaque_id.clone(),
                    tool: tool.into(),
                    credential_ref: reference.clone(),
                    original_arguments_digest: stamp.original_arguments_digest.clone(),
                    account_binding_digest: stamp.approval.account_binding_digest.clone(),
                    arguments: args.clone(),
                };
                let (status, _) = d1_signed_post(
                    app.clone(),
                    &f,
                    GOOGLE_SELECTION_ROUTE,
                    serde_json::to_value(bind).unwrap(),
                )
                .await;
                assert_eq!(status, 200);
                let held = crate::google_personal::grants::acquire(f.home.path())
                    .await
                    .unwrap();
                let request = serde_json::json!({"tool":tool,"arguments":args,"credential_ref":reference,"selection_stamp":stamp.opaque_id});
                let (status, response) = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    d1_signed_post(
                        app.clone(),
                        &f,
                        crate::google_identity::execute::ROUTE,
                        request,
                    ),
                )
                .await
                .unwrap();
                drop(held);
                assert_eq!(status, 200);
                response["result"].clone()
            } else {
                // The route has its existing 256 KiB body cap; a larger first
                // recipe is checked through the real consumed host claim.
                let pending = d1_pending(&f, tool, &args);
                let held = crate::google_personal::grants::acquire(f.home.path())
                    .await
                    .unwrap();
                let value = tokio::time::timeout(
                    std::time::Duration::from_secs(1),
                    pending.execute_native(args),
                )
                .await
                .unwrap()
                .unwrap();
                drop(held);
                value
            };
            assert_eq!(value["isError"], true);
            assert_eq!(events, access::pinned_test::events(f.home.path()));
            assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
            let rows = d1_r2_execute_audit_rows(&f);
            assert_eq!(rows.len(), before.len() + 1);
            let row = rows.last().unwrap();
            assert_eq!(row["agent_id"], "bot");
            assert_eq!(row["google_tool"], tool);
            assert_eq!(row["params_summary"], tool);
            assert_eq!(row["success"], false);
            assert_eq!(row["fallback"], false);
            assert_eq!(row["google_account"], "");
            assert_eq!(row["reason"], reason);
            assert!(!row.to_string().contains("private_preflight_payload"));
            assert!(!row.to_string().contains(D1_SECRET));
            assert!(!row.to_string().contains(&"X".repeat(80)));
        }
    }
}

fn d1_r2_reflection_fixture(home: bool, token: &str) -> Fixture {
    let f = d1_fixture(home);
    if home {
        std::fs::write(f.home.path().join("mcp-oauth-tokens.json"),serde_json::to_vec(&serde_json::json!([{"provider_id":"google","access_token":token,"refresh_token":null,"expires_at":null,"scopes":crate::google_workspace::REQUIRED_SCOPES}])).unwrap()).unwrap();
    } else {
        let old = vault::get(f.home.path(), &f.user).unwrap().unwrap();
        let key = crate::google_identity::protection_gate(f.home.path()).unwrap();
        assert!(
            vault::finish_refresh(
                f.home.path(),
                &key,
                &old.metadata,
                &Secret {
                    access_token: token.into(),
                    refresh_token: Some("fixture-refresh-never-output".into()),
                    expires_at: Some(4_000_000_000)
                }
            )
            .unwrap()
        );
    }
    f
}

#[tokio::test]
async fn d1_r2_both_owners_scrub_redacted_and_one_character_reflections() {
    use base64::Engine;
    for home in [false, true] {
        for token in ["redacted", "-"] {
            let f = d1_r2_reflection_fixture(home, token);
            let reflected = format!("{token}{token}");
            let payload = serde_json::json!({"snippet":format!("before:{reflected}:after"),"payload":{"mimeType":"text/plain","body":{"data":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&reflected)}}});
            let wire = d1_wire(move |_, _| D1Reply::json(payload.clone())).await;
            let _fixture = crate::google_workspace::native_call::testing::install(
                f.home.path(),
                &wire.endpoint,
            );
            let args = serde_json::json!({"message_id":"abc"});
            let value = d1_pending(&f, "gmail_read", &args)
                .execute_native(args)
                .await
                .unwrap();
            assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 1);
            let seen = crate::google_workspace::native_call::testing::observations(f.home.path());
            assert_eq!(seen.len(), 1);
            for output in [&value, &seen[0]] {
                let decoded: serde_json::Value = output["content"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find_map(|item| serde_json::from_str(item["text"].as_str()?).ok())
                    .unwrap();
                assert_eq!(decoded["body"], "••••••");
                assert_eq!(decoded["snippet"], "before:••••••:after");
                assert!(!decoded["body"].as_str().unwrap().contains(token));
                assert!(!decoded["snippet"].as_str().unwrap().contains(token));
            }
        }
    }
}

// M1 source-first declarations; helper bodies follow. NOTCOMPILED / NOTRUN.
#[tokio::test]
async fn m1_both_original_owners_use_three_posts_and_scrub_split_bearer_session() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = m1_wire(false).await;
        let registry = m1_registry(&f, &wire).await;
        let outcome =
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({"x":1}))
                .await
                .unwrap();
        assert!(!outcome.is_error);
        assert_eq!(outcome.content, "••• •••");
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
        let requests = wire.requests.lock().unwrap();
        for group in requests.chunks_exact(3) {
            assert!(!group[0].contains("mcp-session-id:"));
            assert!(group[1].contains("mcp-session-id: fixture-session-never-output"));
            assert!(group[2].contains("mcp-session-id: fixture-session-never-output"));
        }
        for observed in remote_testing::observations(f.home.path()) {
            assert!(!observed.to_string().contains(D1_SECRET));
            assert!(
                !observed
                    .to_string()
                    .contains("fixture-session-never-output")
            );
        }
    }
}

#[tokio::test]
async fn m1_changed_parsed_config_never_executes_or_switches_source() {
    let f = d1_fixture(false);
    let wire = m1_wire(false).await;
    let registry = m1_registry(&f, &wire).await;
    let path = f.home.path().join("agents/bot/agent.toml");
    let old = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, format!("{old}\n# changed after discovery\n")).unwrap();
    let outcome = duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
        .await
        .unwrap();
    assert!(outcome.is_error);
    assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 3);
}

struct M1Wire {
    endpoint: String,
    hits: Arc<std::sync::atomic::AtomicUsize>,
    requests: Arc<std::sync::Mutex<Vec<String>>>,
    mode: Arc<std::sync::atomic::AtomicUsize>,
    redirect: Arc<std::sync::Mutex<String>>,
    task: tokio::task::JoinHandle<()>,
    fixtures: std::sync::Mutex<Vec<remote_testing::Guard>>,
}
impl Drop for M1Wire {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn m1_wire(sse: bool) -> M1Wire {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let mode = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let redirect = Arc::new(std::sync::Mutex::new(String::new()));
    let count = hits.clone();
    let saved = requests.clone();
    let behavior = mode.clone();
    let location = redirect.clone();
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut bytes = Vec::new();
            let mut buffer = [0; 8192];
            let end;
            loop {
                let Ok(n) = stream.read(&mut buffer).await else {
                    return;
                };
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&buffer[..n]);
                if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    end = i + 4;
                    break;
                }
                assert!(bytes.len() < 32768);
            }
            let header = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let len = header
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            assert!(len <= 16 * 1024 * 1024);
            while bytes.len() - end < len {
                let n = stream.read(&mut buffer).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&buffer[..n]);
            }
            saved.lock().unwrap().push(header);
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let frame: serde_json::Value = serde_json::from_slice(&bytes[end..end + len]).unwrap();
            let method = frame["method"].as_str().unwrap();
            let mode = behavior.load(std::sync::atomic::Ordering::SeqCst);
            if mode == 3 {
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
            let (status, body) = if mode == 1 {
                (503, "{}".to_owned())
            } else if mode == 2 {
                (302, "{}".to_owned())
            } else if method == "notifications/initialized" {
                (204, String::new())
            } else {
                let result = if method == "initialize" {
                    serde_json::json!({"id":1,"result":{}})
                } else if method == "tools/list" {
                    serde_json::json!({"id":2,"result":{"tools":[{"name":"remote_tool","description":D1_SECRET,"inputSchema":{"type":"object"}}]}})
                } else {
                    serde_json::json!({"id":2,"result":{"content":[{"type":"text","text":"fixture-original-"},{"type":"text","text":"never-output fixture-session-"},{"type":"text","text":"never-output"}]}})
                };
                let result = if method == "tools/call" {
                    match mode {
                        4 => {
                            serde_json::json!({"id":2,"error":{"code":-1,"message":format!("{D1_SECRET} fixture-session-never-output")}})
                        }
                        5 => {
                            let mut object = serde_json::Map::new();
                            object.insert(D1_SECRET.into(), serde_json::json!("value"));
                            serde_json::json!({"id":2,"result":{"content":[{"type":"text","text":serde_json::Value::Object(object).to_string()}]}})
                        }
                        6 => {
                            let mut ignored = serde_json::Map::new();
                            ignored.insert(
                                D1_SECRET.into(),
                                serde_json::json!("fixture-session-never-output"),
                            );
                            serde_json::json!({"id":2,"result":{"ignored":ignored,"content":[{"type":"text","text":"safe"}]}})
                        }
                        7 => {
                            serde_json::json!({"id":2,"result":{"content":[{"type":"text","text":serde_json::json!({"message":D1_SECRET}).to_string()}]}})
                        }
                        9 => {
                            serde_json::json!({"id":2,"result":{"content":[{"type":"text","text":"{\"x\":\"\\u0066ixture-original-"},{"type":"text","text":"never-output \\u0066ixture-session-"},{"type":"text","text":"never-output\"}"}]}})
                        }
                        10 => {
                            serde_json::json!({"id":2,"result":{"content":[{"type":"text","text":format!("{D1_SECRET} fixture-session-old fixture-session-middle fixture-session-new")}]}})
                        }
                        _ => result,
                    }
                } else {
                    result
                };
                let raw = result
                    .to_string()
                    .replace("fixture-original", "\\u0066ixture-original");
                (
                    200,
                    if mode == 8 && method == "tools/call" {
                        "x".repeat(16 * 1024 * 1024 + 1)
                    } else if sse {
                        format!("event: message\ndata: {raw}\n\n")
                    } else {
                        raw
                    },
                )
            };
            let session = if mode == 10 {
                match method {
                    "initialize" => "Mcp-Session-Id: fixture-session-old\r\n",
                    "notifications/initialized" => "Mcp-Session-Id: fixture-session-middle\r\n",
                    _ => "Mcp-Session-Id: fixture-session-new\r\n",
                }
            } else if method == "initialize" {
                "Mcp-Session-Id: fixture-session-never-output\r\n"
            } else {
                ""
            };
            let redirect = if mode == 2 {
                format!("Location: {}\r\n", location.lock().unwrap())
            } else {
                String::new()
            };
            let kind = if sse {
                "text/event-stream"
            } else {
                "application/json"
            };
            let head = format!(
                "HTTP/1.1 {status} Fixture\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n{session}{redirect}\r\n",
                body.len()
            );
            let _ = stream.write_all(head.as_bytes()).await;
            let _ = stream.write_all(body.as_bytes()).await;
        }
    });
    M1Wire {
        endpoint,
        hits,
        requests,
        mode,
        redirect,
        task,
        fixtures: Default::default(),
    }
}
async fn m1_registry(f: &Fixture, wire: &M1Wire) -> duduclaw_llm::ToolRegistry {
    let path = f.home.path().join("agents/bot/agent.toml");
    let config = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path,format!("{config}\n[[mcp.external]]\nenabled=false\nname='skipped'\nurl='https://skipped.invalid/'\n[[mcp.external]]\nname='remote'\nurl='{}'\nbearer_token='oauth://google'\n",wire.endpoint)).unwrap();
    wire.fixtures
        .lock()
        .unwrap()
        .push(remote_testing::install(f.home.path(), &wire.endpoint));
    let mut servers = crate::mcp_external::load_external_mcp_servers_resolved(
        &f.home.path().join("agents/bot"),
        f.home.path(),
    )
    .await;
    assert_eq!(servers.len(), 1);
    let server = servers.remove(0);
    assert_eq!(server.source_ordinal, 1);
    assert!(server.bearer_token.is_none());
    assert!(server.google_guard.is_none());
    let mount = server.google_closed.unwrap();
    duduclaw_llm::ToolRegistry::from_client_slots_named(
        vec![(server.name, mount.registry_slot())],
        vec![server.filter],
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn m1_sse_both_owners_finish_codecs_before_completed_output() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = m1_wire(true).await;
        let registry = m1_registry(&f, &wire).await;
        let outcome =
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                .await
                .unwrap();
        assert_eq!(outcome.content, "••• •••");
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
    }
}
#[tokio::test]
async fn m1_no_retry_and_no_redirect_use_only_the_bound_owned_endpoint() {
    for home in [false, true] {
        for mode in [1, 2] {
            let f = d1_fixture(home);
            let wire = m1_wire(false).await;
            let other = m1_wire(false).await;
            let registry = m1_registry(&f, &wire).await;
            *wire.redirect.lock().unwrap() = other.endpoint.clone();
            wire.mode.store(mode, std::sync::atomic::Ordering::SeqCst);
            let outcome =
                duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                    .await
                    .unwrap();
            assert!(outcome.is_error);
            assert!(outcome.content.contains("unconfirmed"));
            assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 4);
            assert_eq!(other.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
        }
    }
}
#[tokio::test]
async fn m1_each_send_chunk_codec_and_completion_boundary_rechecks_original_authority() {
    for home in [false, true] {
        for stage in [
            "before_builder",
            "before_send",
            "after_send",
            "after_chunk",
            "after_eof",
            "after_codec",
            "after_program",
            "before_completed",
        ] {
            let f = d1_fixture(home);
            let wire = m1_wire(false).await;
            let registry = m1_registry(&f, &wire).await;
            let hits = wire.hits.load(std::sync::atomic::Ordering::SeqCst);
            assert_eq!(hits, 3, "home={home}, stage={stage}, hits={hits}");
            let config = f.home.path().join("config.toml");
            remote_testing::hook(
                f.home.path(),
                stage,
                Box::new(move || {
                    let old = std::fs::read_to_string(&config).unwrap();
                    std::fs::write(
                        config,
                        old.replace("google_workspace=true", "google_workspace=false"),
                    )
                    .unwrap();
                }),
            );
            let outcome =
                duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                    .await
                    .unwrap();
            let hits = wire.hits.load(std::sync::atomic::Ordering::SeqCst);
            assert!(outcome.is_error, "home={home}, stage={stage}, hits={hits}");
            assert!(
                !outcome.content.contains(D1_SECRET),
                "home={home}, stage={stage}, hits={hits}"
            );
            if stage == "before_builder" || stage == "before_send" {
                assert_eq!(hits, 3, "home={home}, stage={stage}, hits={hits}");
            } else {
                assert!(
                    outcome.content.contains("unconfirmed"),
                    "home={home}, stage={stage}, hits={hits}"
                );
                let expected_hits = if stage == "after_program" || stage == "before_completed" {
                    6
                } else {
                    4
                };
                assert_eq!(hits, expected_hits, "home={home}, stage={stage}, hits={hits}");
            }
        }
    }
}
#[tokio::test]
async fn m1_cancelled_sender_drops_effect_lease_without_detached_http() {
    let f = d1_fixture(false);
    let wire = m1_wire(false).await;
    let registry = m1_registry(&f, &wire).await;
    wire.mode.store(3, std::sync::atomic::Ordering::SeqCst);
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(200),
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
        )
        .await
        .is_err()
    );
    assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 4);
    let lease = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        crate::google_personal::grants::acquire(f.home.path()),
    )
    .await
    .unwrap()
    .unwrap();
    drop(lease);
}
#[tokio::test]
async fn m1_owned_proxy_child() {
    // Also a normal owned positive case when run outside the isolated child.
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = m1_wire(false).await;
        let registry = m1_registry(&f, &wire).await;
        let outcome =
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                .await
                .unwrap();
        assert_eq!(outcome.content, "••• •••");
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
    }
    assert_eq!(m1_refreshing_owners().await, 2);
    if std::env::var_os("DUDUCLAW_M1_OWNED_PROXY_CHILD").is_some() {
        println!("M1_OWNED_PROXY_CHILD_POSTS=24 M1_OWNED_PROXY_CHILD_REFRESHES=2");
    }
}
#[tokio::test]
async fn m1_ambient_proxy_owned_process_receives_zero_closed_posts() {
    let proxy = m1_wire(false).await;
    let exe = std::env::current_exe().unwrap();
    let mut command = tokio::process::Command::new(exe);
    command
        .args([
            "--exact",
            "google_identity::effect_authority::tests::m1_owned_proxy_child",
            "--nocapture",
        ])
        .env("DUDUCLAW_M1_OWNED_PROXY_CHILD", "1")
        .env("HTTP_PROXY", &proxy.endpoint)
        .env("http_proxy", &proxy.endpoint)
        .env("HTTPS_PROXY", &proxy.endpoint)
        .env("https_proxy", &proxy.endpoint)
        .env("ALL_PROXY", &proxy.endpoint)
        .env("all_proxy", &proxy.endpoint)
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(60), command.output())
        .await
        .unwrap()
        .unwrap();
    assert!(output.status.success(), "owned proxy child failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("running 1 test"));
    assert!(stdout.contains("1 passed; 0 failed"));
    assert!(stdout.contains("M1_OWNED_PROXY_CHILD_POSTS=24 M1_OWNED_PROXY_CHILD_REFRESHES=2"));
    assert_eq!(proxy.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn m1_new_requester_is_captured_before_metadata_offload() {
    let f = d1_fixture(false);
    let wire = m1_wire(false).await;
    let registry = m1_registry(&f, &wire).await;
    let visitor = RequesterIdentity::ChannelUser {
        channel: "widget".into(),
        chat: "owned-chat".into(),
        sender: "owned-visitor".into(),
        user_id: None,
    };
    let outcome = crate::knowledge::turn_requesters::CURRENT_REQUESTER
        .scope(
            visitor,
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({})),
        )
        .await
        .unwrap();
    assert!(outcome.is_error);
    assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 3);
}
#[tokio::test]
async fn m1_missing_owned_fixture_refuses_before_resolution_and_provider() {
    let f = d1_fixture(false);
    let wire = m1_wire(false).await;
    let path = f.home.path().join("agents/bot/agent.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path,format!("{original}\n[[mcp.external]]\nname='remote'\nurl='{}'\nbearer_token='oauth://google'\n",wire.endpoint)).unwrap();
    let mut servers = crate::mcp_external::load_external_mcp_servers_resolved(
        &f.home.path().join("agents/bot"),
        f.home.path(),
    )
    .await;
    let server = servers.remove(0);
    let before = access::pinned_test::events(f.home.path());
    assert!(
        duduclaw_llm::ToolRegistry::from_client_slots_named(
            vec![(server.name, server.google_closed.unwrap().registry_slot())],
            vec![]
        )
        .await
        .is_err()
    );
    assert_eq!(before, access::pinned_test::events(f.home.path()));
    assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
}
#[tokio::test]
async fn m1_changed_final_frame_is_rejected_before_resolving_material() {
    let f = d1_fixture(false);
    let wire = m1_wire(false).await;
    let registry = m1_registry(&f, &wire).await;
    let before = access::pinned_test::events(f.home.path());
    remote_testing::corrupt_next_final_frame(f.home.path());
    let outcome = duduclaw_llm::ToolExecutor::call(
        &registry,
        "remote_tool",
        serde_json::json!({"number":1,"unused":true}),
    )
    .await
    .unwrap();
    assert!(outcome.is_error);
    assert_eq!(before, access::pinned_test::events(f.home.path()));
    assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 3);
}
#[test]
fn m1_authority_and_owned_operation_have_send_sync_contract() {
    fn send_sync<T: Send + Sync>() {}
    send_sync::<GoogleEffectAuthority>();
    fn send_static<T: Send + 'static>() {}
    send_static::<Box<dyn duduclaw_llm::OwnedScopedOperation + Send + 'static>>();
}

#[tokio::test]
async fn m1_rpc_error_escaped_key_ignored_field_and_nested_json_finish_inside_both_owners() {
    for home in [false, true] {
        for mode in [4, 5, 6, 7] {
            let f = d1_fixture(home);
            let wire = m1_wire(false).await;
            let registry = m1_registry(&f, &wire).await;
            wire.mode.store(mode, std::sync::atomic::Ordering::SeqCst);
            let outcome =
                duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                    .await
                    .unwrap();
            assert!(!outcome.content.contains(D1_SECRET));
            assert!(!outcome.content.contains("fixture-session-never-output"));
            assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
            if mode == 4 || mode == 5 {
                assert!(outcome.is_error);
                assert!(outcome.content.contains("unconfirmed"));
                assert_eq!(remote_testing::observations(f.home.path()).len(), 1);
            } else if mode == 6 {
                assert_eq!(outcome.content, "safe");
            } else {
                let content: serde_json::Value = serde_json::from_str(&outcome.content).unwrap();
                assert_eq!(content["message"], "•••");
            }
        }
    }
}
#[tokio::test]
async fn m1_input_and_each_post_response_are_bounded_without_partial_success() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = m1_wire(false).await;
        let registry = m1_registry(&f, &wire).await;
        let before = access::pinned_test::events(f.home.path());
        let outcome = duduclaw_llm::ToolExecutor::call(
            &registry,
            "remote_tool",
            serde_json::json!({"body":"x".repeat(16*1024*1024+1)}),
        )
        .await
        .unwrap();
        assert!(outcome.is_error);
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 3);
        assert_eq!(before, access::pinned_test::events(f.home.path()));
        wire.mode.store(8, std::sync::atomic::Ordering::SeqCst);
        let outcome =
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                .await
                .unwrap();
        assert!(outcome.is_error);
        assert!(outcome.content.contains("unconfirmed"));
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
    }
}

struct M1LoopProvider {
    seen: std::sync::Mutex<Vec<duduclaw_llm::ChatRequest>>,
}
#[async_trait::async_trait]
impl duduclaw_llm::ChatProvider for M1LoopProvider {
    fn id(&self) -> &str {
        "m1-owned-model-fixture"
    }
    async fn complete(
        &self,
        request: &duduclaw_llm::ChatRequest,
    ) -> Result<duduclaw_llm::ChatResponse, duduclaw_llm::LlmError> {
        let mut seen = self.seen.lock().unwrap();
        seen.push(request.clone());
        let first = seen.len() == 1;
        Ok(duduclaw_llm::ChatResponse {
            parts: if first {
                vec![duduclaw_llm::ContentPart::ToolCall {
                    id: "m1-inner-json-call".into(),
                    name: "remote_tool".into(),
                    args: serde_json::json!({}),
                }]
            } else {
                vec![duduclaw_llm::ContentPart::Text("done".into())]
            },
            stop: if first {
                duduclaw_llm::StopReason::ToolUse
            } else {
                duduclaw_llm::StopReason::EndTurn
            },
            usage: Default::default(),
            model_used: "owned-fixture".into(),
            provider: "owned-fixture".into(),
        })
    }
    async fn stream(
        &self,
        _: &duduclaw_llm::ChatRequest,
    ) -> Result<
        futures_util::stream::BoxStream<
            'static,
            Result<duduclaw_llm::StreamEvent, duduclaw_llm::LlmError>,
        >,
        duduclaw_llm::LlmError,
    > {
        Err(duduclaw_llm::LlmError::InvalidRequest(
            "unused owned stream".into(),
        ))
    }
}
struct M1GlobalSpy {
    values: std::sync::Mutex<Vec<serde_json::Value>>,
}
#[async_trait::async_trait]
impl duduclaw_llm::ToolInterceptor for M1GlobalSpy {
    fn before_call(
        &self,
        _: &str,
        _: &str,
        args: serde_json::Value,
    ) -> duduclaw_llm::InterceptDecision {
        duduclaw_llm::InterceptDecision::Allow(args)
    }
    fn after_call(&self, _: &str, _: &str, _: &serde_json::Value, value: &mut serde_json::Value) {
        assert!(!value.to_string().contains(D1_SECRET));
        assert!(!value.to_string().contains("fixture-session-never-output"));
        self.values.lock().unwrap().push(value.clone());
    }
}
#[tokio::test]
async fn m1_inner_json_unicode_escapes_split_across_blocks_are_safe_before_true_global_hook() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = m1_wire(false).await;
        let registry = m1_registry(&f, &wire).await;
        wire.mode.store(9, std::sync::atomic::Ordering::SeqCst);
        let provider = M1LoopProvider {
            seen: Default::default(),
        };
        let spy = Arc::new(M1GlobalSpy {
            values: Default::default(),
        });
        let outcome = duduclaw_llm::run_tool_loop_with_provenance_and_ccr(
            &provider,
            duduclaw_llm::ChatRequest::new("owned-model"),
            &registry,
            2,
            Default::default(),
            Some(spy.clone()),
            None,
        )
        .await
        .unwrap();
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
        assert!(outcome.ccr_saved_results.is_empty());
        assert!(
            spy.values
                .lock()
                .unwrap()
                .iter()
                .any(|value| value.get("x") == Some(&serde_json::json!("••• •••")))
        );
        let seen = provider.seen.lock().unwrap();
        let text = seen
            .last()
            .unwrap()
            .messages
            .iter()
            .flat_map(|m| &m.parts)
            .find_map(|p| match p {
                duduclaw_llm::ContentPart::ToolResult { content, .. } => Some(content),
                _ => None,
            })
            .unwrap();
        assert!(!text.contains(D1_SECRET));
        assert!(!text.contains("fixture-session-never-output"));
        assert!(text.contains("•••"));
        assert_eq!(remote_testing::observations(f.home.path()).len(), 2);
    }
}

#[tokio::test]
async fn m1_remote_personal_sync_decrypt_expiry_refuses_but_legacy_keeps_original_strategy() {
    for remote in [false, true] {
        let f = Fixture::new(
            crate::google_workspace::REQUIRED_SCOPES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            1,
        );
        let selected = f.selected("google_remote_mcp");
        crate::google_personal::authority::pinned_client_test::hook(
            f.home.path(),
            "before_decrypt",
            Box::new(|| std::thread::sleep(std::time::Duration::from_millis(80))),
        );
        access::pinned_test::refresh_result(
            f.home.path(),
            Ok(crate::google_personal::oauth::Tokens {
                secret: Secret {
                    access_token: D1_SECRET.into(),
                    refresh_token: Some("owned-refresh".into()),
                    expires_at: Some(4_000_000_000),
                },
                granted_scopes: None,
            }),
        );
        let result = if remote {
            access::resolve_pinned_mcp(
                f.home.path(),
                "bot",
                &RequesterIdentity::Unknown,
                &UserDbPeople(f.db.clone()),
                &selected,
                tokio::time::Instant::now() + std::time::Duration::from_millis(30),
            )
            .await
        } else {
            access::resolve_pinned(
                f.home.path(),
                "bot",
                &RequesterIdentity::Unknown,
                &UserDbPeople(f.db.clone()),
                &selected,
                "google_remote_mcp",
            )
            .await
        };
        if remote {
            assert!(result.is_err());
            assert!(!access::pinned_test::events(f.home.path()).contains(&"provider"));
        } else {
            assert!(result.is_ok());
            assert!(access::pinned_test::events(f.home.path()).contains(&"refresh_published"));
        }
    }
}

async fn m1_refreshing_owners() -> usize {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut refreshed = 0;
    for home in [false, true] {
        let f = d1_fixture(home);
        if home {
            std::fs::write(f.home.path().join("mcp-oauth-tokens.json"),serde_json::to_vec(&serde_json::json!([{"provider_id":"google","access_token":D1_SECRET,"refresh_token":"owned-refresh","expires_at":"2000-01-01T00:00:00Z","scopes":crate::google_workspace::REQUIRED_SCOPES}])).unwrap()).unwrap();
        } else {
            let old = vault::get(f.home.path(), &f.user).unwrap().unwrap();
            let key = crate::google_identity::protection_gate(f.home.path()).unwrap();
            assert!(
                vault::finish_refresh(
                    f.home.path(),
                    &key,
                    &old.metadata,
                    &Secret {
                        access_token: D1_SECRET.into(),
                        refresh_token: Some("owned-refresh".into()),
                        expires_at: Some(1)
                    }
                )
                .unwrap()
            );
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/token", listener.local_addr().unwrap());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed = count.clone();
        let task = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                let mut buffer = [0; 4096];
                loop {
                    let n = stream.read(&mut buffer).await.unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&buffer[..n]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let len = header
                            .lines()
                            .find_map(|line| {
                                line.split_once(':')
                                    .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                    .map(|(_, v)| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap();
                        if bytes.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let body=serde_json::json!({"access_token":D1_SECRET,"token_type":"Bearer","expires_in":3600}).to_string();
                let response = format!(
                    "HTTP/1.1 200 Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).await.unwrap();
            }
        });
        let _personal = (!home).then(|| {
            crate::google_personal::oauth::mcp_refresh_test::install(f.home.path(), &endpoint)
        });
        let _home = home.then(|| {
            crate::google_identity::home_source::mcp_budget_testing::install(f.home.path(), &endpoint)
        });
        let wire = m1_wire(false).await;
        let registry = m1_registry(&f, &wire).await;
        let result =
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                .await
                .unwrap();
        assert_eq!(result.content, "••• •••");
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
        refreshed += 1;
        task.abort();
    }
    refreshed
}
#[tokio::test]
async fn m1_both_original_owners_refresh_once_through_owned_no_proxy_transport() {
    assert_eq!(m1_refreshing_owners().await, 2);
}

#[tokio::test]
async fn m1_remote_held_refresh_mutex_and_client_rotation_refuse_without_provider_or_fallback() {
    for rotate in [false, true] {
        let f = Fixture::new(
            crate::google_workspace::REQUIRED_SCOPES
                .iter()
                .map(|s| s.to_string())
                .collect(),
            1,
        );
        let home_tokens = f.home.path().join(crate::mcp_oauth::TOKEN_FILE);
        let home_before = std::fs::read(&home_tokens).unwrap();
        let home_rows: Vec<serde_json::Value> = serde_json::from_slice(&home_before).unwrap();
        assert!(
            home_rows.iter().all(|row| {
                row["provider_id"] != "google" || row.get("access_token").is_none()
            }),
            "rotate={rotate}: fixture must have no Home access token"
        );
        let selected = f.selected("google_remote_mcp");
        let held = if rotate {
            let path = f.home.path().join("mcp-oauth-configs.json");
            crate::google_personal::authority::pinned_client_test::hook(
                f.home.path(),
                "before_decrypt",
                Box::new(move || {
                    let text = std::fs::read_to_string(&path).unwrap();
                    std::fs::write(path, text.replace("unit-client", "rotated-client")).unwrap();
                }),
            );
            None
        } else {
            Some(access::pinned_test::hold_refresh(f.home.path(), &f.user).await)
        };
        let result = access::resolve_pinned_mcp(
            f.home.path(),
            "bot",
            &RequesterIdentity::Unknown,
            &UserDbPeople(f.db.clone()),
            &selected,
            tokio::time::Instant::now()
                + std::time::Duration::from_millis(if rotate { 500 } else { 30 }),
        )
        .await;
        drop(held);
        assert!(result.is_err(), "rotate={rotate}");
        let error = result.err().unwrap();
        assert_eq!(error.code, "identity_changed", "rotate={rotate}");
        assert!(!error.fallback_eligible, "rotate={rotate}");
        assert!(
            !access::pinned_test::events(f.home.path()).contains(&"provider"),
            "rotate={rotate}"
        );
        assert!(
            std::fs::read(&home_tokens).unwrap() == home_before,
            "rotate={rotate}: Home token/control bytes changed"
        );
    }
}

#[tokio::test]
async fn m1_rotated_session_headers_keep_all_original_values_inside_both_owners() {
    for home in [false, true] {
        let f = d1_fixture(home);
        let wire = m1_wire(false).await;
        let registry = m1_registry(&f, &wire).await;
        wire.mode.store(10, std::sync::atomic::Ordering::SeqCst);
        let outcome =
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", serde_json::json!({}))
                .await
                .unwrap();
        assert!(!outcome.is_error);
        assert_eq!(outcome.content, "••• ••• ••• •••");
        assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 6);
        let requests = wire.requests.lock().unwrap();
        assert!(!requests[3].contains("mcp-session-id:"));
        assert!(requests[4].contains("mcp-session-id: fixture-session-old"));
        assert!(requests[5].contains("mcp-session-id: fixture-session-middle"));
        for observed in remote_testing::observations(f.home.path()) {
            for secret in [
                D1_SECRET,
                "fixture-session-old",
                "fixture-session-middle",
                "fixture-session-new",
            ] {
                assert!(!observed.to_string().contains(secret));
            }
        }
    }
}

#[tokio::test]
async fn m1_r2_missing_home_refresh_fixture_refuses_true_closed_mount_with_zero_sends() {
    let f = d1_fixture(true);
    std::fs::write(f.home.path().join("mcp-oauth-tokens.json"),serde_json::to_vec(&serde_json::json!([{"provider_id":"google","access_token":D1_SECRET,"refresh_token":"owned-refresh","expires_at":"2000-01-01T00:00:00Z","scopes":crate::google_workspace::REQUIRED_SCOPES}])).unwrap()).unwrap();
    let wire = m1_wire(false).await;
    let path = f.home.path().join("agents/bot/agent.toml");
    let original = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path,format!("{original}\n[[mcp.external]]\nname='remote'\nurl='{}'\nbearer_token='oauth://google'\n",wire.endpoint)).unwrap();
    let _mcp_fixture = remote_testing::install(f.home.path(), &wire.endpoint);
    let probe = crate::mcp_oauth::home_store::test_probe::Guard::install(f.home.path());
    let mut servers = crate::mcp_external::load_external_mcp_servers_resolved(
        &f.home.path().join("agents/bot"),
        f.home.path(),
    )
    .await;
    let server = servers.remove(0);
    assert!(
        duduclaw_llm::ToolRegistry::from_client_slots_named(
            vec![(server.name, server.google_closed.unwrap().registry_slot())],
            vec![server.filter]
        )
        .await
        .is_err()
    );
    assert_eq!(wire.hits.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(probe.counts(), [0, 0]);
    assert_eq!(probe.google_crypto_count(), 0);
}

#[path = "effect_authority_cli_tests.rs"]
mod cli_composed;

#[path = "effect_authority_w3_tests.rs"]
mod w3;
