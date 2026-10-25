//! W3 composed source oracles; NOTCOMPILED and NOTRUN. All HTTP is owned loopback.
use super::*;
use std::sync::atomic::Ordering;
use serde_json::{json, Value};
const BRIDGE_SECRET: &str = "w3&bridge-private-secret-never-output";
const SCRIPT: &str = "https://script.google.com/macros/s/fixtureDeployment/exec";
const ISSUER: &str = "https://oauth2.googleapis.com/token";
struct Harness {
    f: Fixture,
    issuer: D1Wire,
    _issuer: Option<super::super::variants::testing::Guard>,
    _native: crate::google_workspace::native_call::testing::Guard,
}
impl Harness {
    async fn new(kind: &str, native: &D1Wire) -> Self {
        let f = d1_fixture(false);
        let issuer = d1_wire(|_, _| {
            D1Reply::json(json!({"access_token":D1_SECRET,"expires_in":3600,"token_type":"Bearer"}))
        })
        .await;
        let fixture = super::super::variants::testing::install_issuer(
            f.home.path(),
            ISSUER,
            &issuer.endpoint,
        );
        let native =
            crate::google_workspace::native_call::testing::install(f.home.path(), &native.endpoint);
        let old = std::fs::read_to_string(f.home.path().join("config.toml")).unwrap();
        let policy = match kind {
            "sa" => {
                std::fs::write(
                    f.home.path().join("sa.json"),
                    crate::google_identity::tests::key_json(ISSUER),
                )
                .unwrap();
                std::fs::write(f.home.path().join("config.toml"),format!("{old}\n[integrations.google_service_account]\nkey_file='sa.json'\nsubject='declared@unit.test'\n")).unwrap();
                "identity='home'"
            }
            "delegate" => {
                let key = crate::google_identity::protection_gate(f.home.path()).unwrap();
                crate::google_identity::delegation_key::import(
                    f.home.path(),
                    &key,
                    &crate::google_identity::tests::key_json(ISSUER),
                )
                .unwrap();
                std::fs::write(f.home.path().join("config.toml"),format!("{old}\n[integrations.google_identity]\nallowed_subject_domains=['unit.test']\n")).unwrap();
                "identity='delegate:declared@unit.test'\nrequesters='anyone'"
            }
            "bridge" => {
                std::fs::write(f.home.path().join("config.toml"),format!("{old}\n[integrations.google_service_account]\nkey_file='missing-sa.json'\nsubject='declared@unit.test'\n[integrations.google_apps_script]\nurl='{SCRIPT}'\nsecret='{BRIDGE_SECRET}'\n")).unwrap();
                "identity='home'"
            }
            _ => panic!("unknown fixture kind"),
        };
        std::fs::write(
            f.home.path().join("agents/bot/agent.toml"),
            format!("[agent]\nname='bot'\n[google]\n{policy}\n"),
        )
        .unwrap();
        Self {
            f,
            issuer,
            _issuer: Some(fixture),
            _native: native,
        }
    }
    fn reference(&self) -> CredentialRef {
        CredentialRef {
            kind: gi::CredentialKind::Legacy,
            sha256: client_digest("ddc_prod_0123456789abcdef0123456789abcdef"),
        }
    }
    async fn call(&self, tool: &str, args: Value) -> Result<Value, Refusal> {
        crate::google_identity::effect_authority::execute_immediate_native(
            self.f.home.path(),
            Some(&self.f.db),
            "bot",
            &self.reference(),
            tool,
            args,
        )
        .await
    }
    fn personal_unavailable(&self) {
        vault::disconnect(self.f.home.path(), &self.f.user).unwrap();
        std::fs::write(self.f.home.path().join("agents/bot/agent.toml"),"[agent]\nname='bot'\n[google]\nidentity='owner'\nrequesters='anyone'\nfallback='home'\n").unwrap();
    }
    async fn select(
        &self,
        store: &SelectionStore,
        tool: &str,
        args: &Value,
    ) -> Result<gi::GoogleSelectionStamp, Refusal> {
        store
            .select_dedicated_async(
                self.f.home.path(),
                Some(&self.f.db),
                "bot",
                &self.reference(),
                tool,
                args,
                Clock::now(),
            )
            .await
            .map(|v| v.unwrap())
    }
}
fn count(wire: &D1Wire) -> usize {
    wire.hits.load(Ordering::SeqCst)
}
#[tokio::test]
async fn w3_home_sa_and_delegate_execute_all_eighteen_actual_native_recipes() {
    for kind in ["sa", "delegate"] {
        let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new(kind, &wire).await;
        for (tool, args) in d1_cases() {
            let value = h.call(tool, args).await.unwrap();
            assert_ne!(
                value.get("isError"),
                Some(&json!(true)),
                "{kind}/{tool}: {value}"
            );
            assert!(!value.to_string().contains(D1_SECRET));
        }
        assert_eq!(count(&wire), 18);
        assert_eq!(count(&h.issuer), 18);
        assert_eq!(
            crate::google_workspace::native_call::testing::observations(h.f.home.path()).len(),
            18
        );
        for (_, _, body) in h.issuer.requests.lock().unwrap().iter() {
            let form: std::collections::HashMap<_, _> =
                url::form_urlencoded::parse(body.as_bytes())
                    .into_owned()
                    .collect();
            assert_eq!(
                form["grant_type"],
                "urn:ietf:params:oauth:grant-type:jwt-bearer"
            );
            use base64::Engine as _;
            let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .decode(form["assertion"].split('.').nth(1).unwrap())
                .unwrap();
            let claims: Value = serde_json::from_slice(&payload).unwrap();
            assert_eq!(claims["sub"], "declared@unit.test");
            assert_eq!(
                claims["scope"],
                crate::google_workspace::REQUIRED_SCOPES.join(" ")
            );
        }
    }
}
#[tokio::test]
async fn w3_bridge_all_seven_actions_use_independent_request_contracts() {
    let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("bridge", &wire).await;
    for (tool, args) in d1_cases().into_iter().take(7) {
        let value = h.call(tool, args).await.unwrap();
        assert_ne!(value.get("isError"), Some(&json!(true)), "{tool}:{value}");
        assert!(!value.to_string().contains(BRIDGE_SECRET));
    }
    let rows = wire.requests.lock().unwrap();
    assert_eq!(rows.len(), 7);
    assert_eq!(count(&h.issuer), 0);
    let expected = [
        ("gmail_search", json!({"query":"q","limit":10})),
        ("gmail_read", json!({"message_id":"abc"})),
        (
            "gmail_create_draft",
            json!({"to":"a@b.test","subject":"subject","body":"body"}),
        ),
        ("calendar_list_events", json!({"days":7})),
        (
            "calendar_create_event",
            json!({"title":"s","start":"2026-08-01T00:00:00Z","end":"2026-08-01T01:00:00Z","description":""}),
        ),
        ("sheets_read", json!({"spreadsheet":"s","range":"A1"})),
        ("sheets_append", json!({"spreadsheet":"s","values":["x"]})),
    ];
    for ((method, path, body), (action, params)) in rows.iter().zip(expected) {
        assert_eq!(method, "POST");
        assert_eq!(path, "/script.google.com/macros/s/fixtureDeployment/exec");
        assert_eq!(
            serde_json::from_str::<Value>(body).unwrap(),
            json!({"secret":BRIDGE_SECRET,"action":action,"params":params})
        );
    }
}
#[tokio::test]
async fn w3_bridge_direct_only_and_unsupported_fields_never_send() {
    const BRIDGE_TOOLS: [&str; 7] = [
        "gmail_search",
        "gmail_read",
        "gmail_create_draft",
        "calendar_list_events",
        "calendar_create_event",
        "sheets_read",
        "sheets_append",
    ];
    let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("bridge", &wire).await;
    let native_cases = d1_cases();
    assert_eq!(native_cases.len(), 18);
    for tool in BRIDGE_TOOLS {
        assert_eq!(
            native_cases.iter().filter(|(name, _)| *name == tool).count(),
            1,
            "Bridge fixture tool={tool} must occur once"
        );
    }
    let direct_only: Vec<_> = native_cases
        .into_iter()
        .filter(|(tool, _)| !BRIDGE_TOOLS.contains(tool))
        .collect();
    assert_eq!(direct_only.len(), 11);
    let cases: Vec<_> = direct_only
        .into_iter()
        .chain([
            (
                "gmail_create_draft",
                json!({"to":"a@b.test","subject":"s","body":"b","cc":"c@d.test"}),
            ),
            (
                "calendar_create_event",
                json!({"summary":"s","start":"2026-08-01T00:00:00Z","end":"2026-08-01T01:00:00Z","attendees":"a@b.test"}),
            ),
            (
                "calendar_create_event",
                json!({"summary":"s","start":"2026-08-01T00:00:00Z","end":"2026-08-01T01:00:00Z","with_meet":true}),
            ),
        ])
        .collect();
    assert_eq!(cases.len(), 14);
    for (tool, args) in cases {
        let arg_keys: Vec<_> = args.as_object().unwrap().keys().cloned().collect();
        let result = h.call(tool, args).await.unwrap();
        assert_eq!(result["isError"], true, "tool={tool}, arg_keys={arg_keys:?}");
        assert_eq!(count(&wire), 0, "tool={tool}, arg_keys={arg_keys:?}");
        assert_eq!(count(&h.issuer), 0, "tool={tool}, arg_keys={arg_keys:?}");
    }
    assert_eq!(count(&wire), 0);
    assert_eq!(count(&h.issuer), 0);
}
#[tokio::test]
async fn w3_selected_home_sa_preflight_is_once_before_bind_and_execute() {
    let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("sa", &wire).await;
    h.personal_unavailable();
    let store = SelectionStore::default();
    let args = json!({"query":"q"});
    let stamp = h.select(&store, "gmail_search", &args).await.unwrap();
    assert_eq!(
        stamp.approval.selected_identity,
        gi::ApprovedGoogleIdentity::HomeServiceAccountFallback
    );
    assert!(stamp.approval.home_connection.is_none());
    assert!(stamp.approval.home_source.is_some());
    assert_eq!(count(&h.issuer), 1);
    assert_eq!(count(&wire), 0);
    store
        .bind_dedicated(
            h.f.home.path(),
            Some(&h.f.db),
            "bot",
            &h.reference(),
            "gmail_search",
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            &args,
            Clock::now(),
        )
        .unwrap();
    let pending = store
        .claim_dedicated(
            h.f.home.path(),
            Some(&h.f.db),
            "bot",
            &h.reference(),
            "gmail_search",
            &stamp.opaque_id,
            &args,
            Clock::now(),
        )
        .unwrap();
    let value = pending.execute_native(args.clone()).await.unwrap();
    assert_ne!(value["isError"], true);
    assert_eq!(count(&h.issuer), 1);
    assert_eq!(count(&wire), 1);
    assert!(
        store
            .claim_dedicated(
                h.f.home.path(),
                Some(&h.f.db),
                "bot",
                &h.reference(),
                "gmail_search",
                &stamp.opaque_id,
                &args,
                Clock::now()
            )
            .is_err()
    );
}
#[tokio::test]
async fn w3_selected_bridge_keeps_historical_failure_and_refuses_local_recovery() {
    let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("bridge", &wire).await;
    h.personal_unavailable();
    let store = SelectionStore::default();
    let args = json!({"query":"q"});
    let stamp = h.select(&store, "gmail_search", &args).await.unwrap();
    assert_eq!(
        stamp.approval.selected_identity,
        gi::ApprovedGoogleIdentity::HomeAppsScriptFallback
    );
    assert!(stamp.approval.home_connection.is_none());
    assert_eq!(count(&h.issuer), 0);
    // Provider health is not queried again. A newly present local key is observable.
    std::fs::write(
        h.f.home.path().join("missing-sa.json"),
        crate::google_identity::tests::key_json(ISSUER),
    )
    .unwrap();
    let error = store
        .bind_dedicated(
            h.f.home.path(),
            Some(&h.f.db),
            "bot",
            &h.reference(),
            "gmail_search",
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            &args,
            Clock::now(),
        )
        .unwrap_err();
    assert_eq!(error.code, "identity_changed");
    assert_eq!(count(&wire), 0);
    assert_eq!(count(&h.issuer), 0);
}
#[tokio::test]
async fn w3_ask_material_expiry_prunes_without_post_ask_mint() {
    let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("sa", &wire).await;
    h.personal_unavailable();
    let store = SelectionStore::default();
    let args = json!({"query":"q"});
    let stamp = h.select(&store, "gmail_search", &args).await.unwrap();
    store.expire_prepared_for_test(&stamp.opaque_id);
    assert!(
        store
            .bind_dedicated(
                h.f.home.path(),
                Some(&h.f.db),
                "bot",
                &h.reference(),
                "gmail_search",
                &stamp.opaque_id,
                &stamp.original_arguments_digest,
                &stamp.approval.account_binding_digest,
                &args,
                Clock::now()
            )
            .is_err()
    );
    assert_eq!(store.len_for_test(), 0);
    assert_eq!(count(&h.issuer), 1);
    assert_eq!(count(&wire), 0);
}
#[tokio::test]
async fn w3_sa_delegate_reflections_and_all_effect_boundaries_remain_owner_local() {
    for kind in ["sa", "delegate"] {
        for stage in [
            "before_builder",
            "before_send",
            "after_send",
            "after_chunk",
            "after_eof",
            "after_decode",
            "after_program",
            "after_scrub",
            "e1_after_owner",
        ] {
            let wire = d1_wire(|_, _| D1Reply::json(json!({"snippet":D1_SECRET}))).await;
            let h = Harness::new(kind, &wire).await;
            let config = h.f.home.path().join("config.toml");
            crate::google_workspace::native_call::testing::hook(
                h.f.home.path(),
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
            let error = h
                .call("gmail_read", json!({"message_id":"abc"}))
                .await
                .unwrap_err();
            if matches!(stage, "before_builder" | "before_send") {
                assert_eq!(count(&wire), 0);
            } else {
                assert_eq!(error.code, "result_unconfirmed");
                assert_eq!(count(&wire), 1);
            }
            assert!(!error.message_zh.contains(D1_SECRET));
            assert_eq!(count(&h.issuer), 1);
        }
    }
}
#[tokio::test]
async fn w3_sa_delegate_full_codecs_scrub_before_output_and_reject_derived_ids() {
    for kind in ["sa", "delegate"] {
        let wire=d1_wire(|_,_|{use base64::Engine as _;D1Reply::json(json!({"snippet":D1_SECRET,"ignored":D1_SECRET,"payload":{"mimeType":"text/html","body":{"data":base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(format!("<p>{D1_SECRET}</p>"))}}}))}).await;
        let h = Harness::new(kind, &wire).await;
        let value = h
            .call("gmail_read", json!({"message_id":"abc"}))
            .await
            .unwrap();
        assert!(!value.to_string().contains(D1_SECRET));
        for output in crate::google_workspace::native_call::testing::observations(h.f.home.path()) {
            assert!(!output.to_string().contains(D1_SECRET));
        }
        assert_eq!(count(&wire), 1);
        let derived = d1_wire(|_, _| D1Reply::json(json!({"messages":[{"id":D1_SECRET}]}))).await;
        let h = Harness::new(kind, &derived).await;
        let error = h
            .call("gmail_search", json!({"query":"q"}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "result_unconfirmed");
        assert_eq!(count(&derived), 1);
    }
}
#[tokio::test]
async fn w3_sa_delegate_remote_real_closed_slots_use_full_scopes_and_complete_reply() {
    for kind in ["sa", "delegate"] {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new(kind, &native).await;
        let wire = m1_wire(true).await;
        let registry = m1_registry(&h.f, &wire).await;
        let outcome = duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", json!({}))
            .await
            .unwrap();
        assert_eq!(outcome.content, "••• •••");
        assert_eq!(wire.hits.load(Ordering::SeqCst), 6);
        assert_eq!(count(&h.issuer), 2);
        assert_eq!(count(&native), 0);
    }
}
#[tokio::test]
async fn w3_delegate_requester_and_protection_hard_errors_never_fall_back() {
    let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("delegate", &wire).await;
    let path = h.f.home.path().join("agents/bot/agent.toml");
    let old = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        format!(
            "{}\nfallback='home'\n",
            old.replace("requesters='anyone'", "requesters='owner_only'")
        ),
    )
    .unwrap();
    let error = h
        .call("gmail_search", json!({"query":"q"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "requester_refused");
    assert_eq!(count(&h.issuer), 0);
    assert_eq!(count(&wire), 0);
    std::fs::write(&path, format!("{old}\nfallback='home'\n")).unwrap();
    std::fs::write(
        h.f.home.path().join("credential_protection.json"),
        "malformed",
    )
    .unwrap();
    let error = h
        .call("gmail_search", json!({"query":"q"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "credential_protection");
    assert_eq!(count(&h.issuer), 0);
    assert_eq!(count(&wire), 0);
}
#[tokio::test]
async fn w3_metadata_status_and_invalid_arguments_resolve_zero_credentials() {
    for kind in ["sa", "delegate", "bridge"] {
        let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new(kind, &wire).await;
        let status = h.call("google_status", json!({})).await.unwrap();
        assert_ne!(status["isError"], true);
        let invalid = h.call("gmail_read", json!({})).await.unwrap();
        assert_eq!(invalid["isError"], true);
        assert_eq!(count(&wire), 0);
        assert_eq!(count(&h.issuer), 0);
    }
}
#[tokio::test]
async fn w3_oversize_retained_bridge_secret_is_refused_by_actual_selection_capacity() {
    let wire = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("bridge", &wire).await;
    h.personal_unavailable();
    let path = h.f.home.path().join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(path, raw.replace(BRIDGE_SECRET, &"x".repeat(64 * 1024))).unwrap();
    let store = SelectionStore::default();
    let error = h
        .select(&store, "gmail_search", &json!({"query":"q"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "selection_capacity");
    assert_eq!(store.len_for_test(), 0);
    assert_eq!(store.reservations_for_test(), 0);
    assert_eq!(count(&wire), 0);
}
#[tokio::test]
async fn w3_bridge_one_302_or_303_uses_fresh_credential_free_get() {
    for status in [302, 303] {
        let wire=d1_wire(move|index,_|if index==0{D1Reply{status,body:String::new(),location:Some("https://script.googleusercontent.com/macros/echo?user_content_key=privateCapability".into()),delay_ms:0,disconnect:false}}else{D1Reply::json(json!({"messages":[]}))}).await;
        let h = Harness::new("bridge", &wire).await;
        let result = h.call("gmail_search", json!({"query":"q"})).await.unwrap();
        assert_ne!(result["isError"], true);
        assert!(!result.to_string().contains("privateCapability"));
        assert_eq!(count(&wire), 2);
        let rows = wire.requests.lock().unwrap();
        assert_eq!(rows[0].0, "POST");
        assert_eq!(rows[1].0, "GET");
        assert!(
            rows[1]
                .1
                .starts_with("/script.googleusercontent.com/macros/echo?")
        );
        assert!(rows[1].2.is_empty());
        let headers = wire.headers.lock().unwrap();
        let get = headers[1].to_ascii_lowercase();
        for forbidden in [
            "authorization:",
            "cookie:",
            "referer:",
            "x-duduclaw",
            BRIDGE_SECRET,
        ] {
            assert!(!get.contains(forbidden));
        }
        assert_eq!(rows.iter().filter(|(m, _, _)| m == "POST").count(), 1);
    }
}
#[tokio::test]
async fn w3_bridge_bad_handoff_and_get_redirect_never_replay_post() {
    for (status, location) in [
        (307, "https://script.googleusercontent.com/macros/echo?x=1"),
        (308, "https://script.googleusercontent.com/macros/echo?x=1"),
        (
            302,
            "https://script.googleusercontent.com.evil.test/macros/echo?x=1",
        ),
        (302, "/macros/echo?x=1"),
        (
            302,
            "https://script.googleusercontent.com:444/macros/echo?x=1",
        ),
        (302, "https://script.googleusercontent.com/other?x=1"),
    ] {
        let wire = d1_wire(move |_, _| D1Reply {
            status,
            body: "{}".into(),
            location: Some(location.into()),
            delay_ms: 0,
            disconnect: false,
        })
        .await;
        let h = Harness::new("bridge", &wire).await;
        let error = h
            .call("gmail_search", json!({"query":"q"}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "result_unconfirmed");
        assert_eq!(count(&wire), 1);
    }
    let wire = d1_wire(|_, _| D1Reply {
        status: 302,
        body: String::new(),
        location: Some("https://script.googleusercontent.com/macros/echo?x=1".into()),
        delay_ms: 0,
        disconnect: false,
    })
    .await;
    let h = Harness::new("bridge", &wire).await;
    assert_eq!(
        h.call("gmail_search", json!({"query":"q"}))
            .await
            .unwrap_err()
            .code,
        "result_unconfirmed"
    );
    assert_eq!(count(&wire), 2);
}
#[tokio::test]
async fn w3_bridge_after_post_source_revocation_stops_result_get() {
    let wire = d1_wire(|_, _| D1Reply {
        status: 302,
        body: String::new(),
        location: Some("https://script.googleusercontent.com/macros/echo?x=1".into()),
        delay_ms: 0,
        disconnect: false,
    })
    .await;
    let h = Harness::new("bridge", &wire).await;
    let path = h.f.home.path().join("config.toml");
    crate::google_workspace::native_call::testing::hook(
        h.f.home.path(),
        "before_result_get",
        Box::new(move || {
            let old = std::fs::read_to_string(&path).unwrap();
            std::fs::write(
                path,
                old.replace("google_workspace=true", "google_workspace=false"),
            )
            .unwrap();
        }),
    );
    assert_eq!(
        h.call("gmail_search", json!({"query":"q"}))
            .await
            .unwrap_err()
            .code,
        "result_unconfirmed"
    );
    assert_eq!(count(&wire), 1);
}
#[tokio::test]
async fn w3_bridge_reflected_secret_and_old_capability_never_leave_owner() {
    for response in [
        json!({"messages":[{"message_id":"1","subject":BRIDGE_SECRET}]}),
        json!({"ignored":BRIDGE_SECRET}),
        json!({(BRIDGE_SECRET):"value"}),
        json!({"body":"w3&amp;bridge-private-secret-never-output"}),
    ] {
        let wire = d1_wire(move |_, _| D1Reply::json(response.clone())).await;
        let h = Harness::new("bridge", &wire).await;
        let error = h
            .call("gmail_search", json!({"query":"q"}))
            .await
            .unwrap_err();
        assert_eq!(error.code, "result_unconfirmed");
        assert!(!error.message_zh.contains(BRIDGE_SECRET));
        assert!(
            crate::google_workspace::native_call::testing::observations(h.f.home.path()).is_empty()
        );
        assert_eq!(count(&wire), 1);
    }
}
#[tokio::test]
async fn w3_selected_bridge_external_recovery_does_not_reselect_or_remint() {
    let native = d1_wire(|_, _| D1Reply::json(json!({"messages":[]}))).await;
    let mut h = Harness::new("sa", &native).await;
    let recovered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let behavior = recovered.clone();
    let issuer = d1_wire(move |_, _| {
        if behavior.load(Ordering::SeqCst) {
            D1Reply::json(json!({"access_token":D1_SECRET,"expires_in":3600}))
        } else {
            D1Reply {
                status: 503,
                body: "{}".into(),
                location: None,
                delay_ms: 0,
                disconnect: false,
            }
        }
    })
    .await;
    drop(h._issuer.take());
    let _issuer =
        super::super::variants::testing::install_issuer(h.f.home.path(), ISSUER, &issuer.endpoint);
    let path = h.f.home.path().join("config.toml");
    let old = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        format!(
            "{old}\n[integrations.google_apps_script]\nurl='{SCRIPT}'\nsecret='{BRIDGE_SECRET}'\n"
        ),
    )
    .unwrap();
    h.personal_unavailable();
    let store = SelectionStore::default();
    let args = json!({"query":"q"});
    let stamp = h.select(&store, "gmail_search", &args).await.unwrap();
    assert_eq!(
        stamp.approval.selected_identity,
        gi::ApprovedGoogleIdentity::HomeAppsScriptFallback
    );
    assert_eq!(count(&issuer), 1);
    // External issuer health changes without any captured local dependency change.
    recovered.store(true, Ordering::SeqCst);
    store
        .bind_dedicated(
            h.f.home.path(),
            Some(&h.f.db),
            "bot",
            &h.reference(),
            "gmail_search",
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            &args,
            Clock::now(),
        )
        .unwrap();
    let pending = store
        .claim_dedicated(
            h.f.home.path(),
            Some(&h.f.db),
            "bot",
            &h.reference(),
            "gmail_search",
            &stamp.opaque_id,
            &args,
            Clock::now(),
        )
        .unwrap();
    let value = pending.execute_native(args).await.unwrap();
    assert_ne!(value["isError"], true);
    assert_eq!(count(&issuer), 1);
    assert_eq!(count(&native), 1);
}
#[tokio::test]
async fn w3_missing_or_tampered_owned_issuer_refuses_without_network_fallback() {
    for tampered in [false, true] {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new("sa", &native).await;
        let marker = h.f.home.path().join(".w3-issuer-owned-fixture");
        if tampered {
            std::fs::write(marker, "wrong-nonce").unwrap();
        } else {
            std::fs::remove_file(marker).unwrap();
        }
        assert!(h.call("gmail_search", json!({"query":"q"})).await.is_err());
        assert_eq!(count(&h.issuer), 0);
        assert_eq!(count(&native), 0);
    }
}
#[tokio::test]
async fn w3_cancelled_preflight_releases_reservation_and_never_commits() {
    let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let mut h = Harness::new("sa", &native).await;
    h.personal_unavailable();
    let entered = Arc::new(tokio::sync::Notify::new());
    let signal = entered.clone();
    let slow = d1_wire(move |_, _| {
        signal.notify_one();
        D1Reply {
            status: 200,
            body: json!({"access_token":D1_SECRET,"expires_in":3600}).to_string(),
            location: None,
            delay_ms: 10000,
            disconnect: false,
        }
    })
    .await;
    drop(h._issuer.take());
    let _guard =
        super::super::variants::testing::install_issuer(h.f.home.path(), ISSUER, &slow.endpoint);
    let store = SelectionStore::default();
    let args = json!({"query":"q"});
    let mut pending = Box::pin(h.select(&store, "gmail_search", &args));
    tokio::select! {_=entered.notified()=>{},result=&mut pending=>panic!("preflight returned before issuer response: {result:?}"),_=tokio::time::sleep(std::time::Duration::from_secs(2))=>panic!("owned issuer did not observe preflight")};
    assert_eq!(store.reservations_for_test(), 1);
    drop(pending);
    assert_eq!(store.reservations_for_test(), 0);
    assert_eq!(store.len_for_test(), 0);
    assert_eq!(count(&slow), 1);
    assert_eq!(count(&native), 0);
}
#[tokio::test]
async fn w3_eighteen_second_server_preflight_deadline_discards_late_issuer_result() {
    // Intentionally long: the actual 18-second server deadline is exercised.
    let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let mut h = Harness::new("sa", &native).await;
    h.personal_unavailable();
    let slow = d1_wire(|_, _| D1Reply {
        status: 200,
        body: json!({"access_token":D1_SECRET,"expires_in":3600}).to_string(),
        location: None,
        delay_ms: 19000,
        disconnect: false,
    })
    .await;
    drop(h._issuer.take());
    let _guard =
        super::super::variants::testing::install_issuer(h.f.home.path(), ISSUER, &slow.endpoint);
    let store = SelectionStore::default();
    let error = h
        .select(&store, "gmail_search", &json!({"query":"q"}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "selection_expired");
    assert_eq!(store.reservations_for_test(), 0);
    assert_eq!(store.len_for_test(), 0);
    assert_eq!(count(&slow), 1);
    assert_eq!(count(&native), 0);
}
#[tokio::test]
async fn w3_delegate_soft_missing_key_falls_back_only_when_explicit() {
    for enabled in [false, true] {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new("delegate", &native).await;
        std::fs::remove_file(crate::google_identity::delegation_key::path(
            h.f.home.path(),
        ))
        .unwrap();
        let path = h.f.home.path().join("config.toml");
        let old = std::fs::read_to_string(&path).unwrap();
        std::fs::write(
            h.f.home.path().join("sa.json"),
            crate::google_identity::tests::key_json(ISSUER),
        )
        .unwrap();
        std::fs::write(path,format!("{old}\n[integrations.google_service_account]\nkey_file='sa.json'\nsubject='shared@unit.test'\n")).unwrap();
        if enabled {
            let path = h.f.home.path().join("agents/bot/agent.toml");
            let raw = std::fs::read_to_string(&path).unwrap();
            std::fs::write(path, format!("{raw}\nfallback='home'\n")).unwrap();
        }
        let result = h.call("gmail_search", json!({"query":"q"})).await;
        if enabled {
            assert_ne!(result.unwrap()["isError"], true);
            assert_eq!(count(&h.issuer), 1);
            assert_eq!(count(&native), 1);
        } else {
            assert_eq!(result.unwrap_err().code, "delegate_unavailable");
            assert_eq!(count(&h.issuer), 0);
            assert_eq!(count(&native), 0);
        }
    }
}
#[tokio::test]
async fn w3_remote_delegate_captures_true_visitor_and_never_home_falls_back() {
    let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("delegate", &native).await;
    let wire = m1_wire(false).await;
    let registry = m1_registry(&h.f, &wire).await;
    let visitor = RequesterIdentity::ChannelUser {
        channel: "widget".into(),
        chat: "owned-chat".into(),
        sender: "owned-visitor".into(),
        user_id: None,
    };
    let outcome = crate::knowledge::turn_requesters::CURRENT_REQUESTER
        .scope(
            visitor,
            duduclaw_llm::ToolExecutor::call(&registry, "remote_tool", json!({})),
        )
        .await
        .unwrap();
    assert!(outcome.is_error);
    assert_eq!(wire.hits.load(Ordering::SeqCst), 3);
    assert_eq!(count(&h.issuer), 1);
    assert_eq!(count(&native), 0);
}
#[tokio::test]
async fn w3_same_source_different_operation_material_cannot_replace_original() {
    let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("sa", &native).await;
    let args = json!({"query":"q"});
    let context = || {
        GoogleCallContext::dedicated(
            h.f.home.path(),
            "bot",
            Box::new(UserDbPeople(h.f.db.clone())),
            "gmail_search",
            gi::google_arguments_digest(&args),
            h.reference(),
        )
        .unwrap()
    };
    let mut first = super::super::host_claim::prepare_immediate(context())
        .await
        .unwrap()
        .resolve()
        .await
        .unwrap();
    let mut second = super::super::host_claim::prepare_immediate(context())
        .await
        .unwrap()
        .resolve()
        .await
        .unwrap();
    std::mem::swap(&mut first.selected, &mut second.selected);
    assert!(first.acquire_effect().await.is_err());
    assert_eq!(count(&h.issuer), 2);
    assert_eq!(count(&native), 0);
}
#[tokio::test]
async fn w3_bridge_remote_is_not_a_bearer_and_has_zero_bridge_posts() {
    let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("bridge", &native).await;
    let wire = m1_wire(false).await;
    let path = h.f.home.path().join("agents/bot/agent.toml");
    let old = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        format!(
            "{old}\n[[mcp.external]]\nname='remote'\nurl='{}'\nbearer_token='oauth://google'\n",
            wire.endpoint
        ),
    )
    .unwrap();
    let _remote = remote_testing::install(h.f.home.path(), &wire.endpoint);
    let mut servers = crate::mcp_external::load_external_mcp_servers_resolved(
        &h.f.home.path().join("agents/bot"),
        h.f.home.path(),
    )
    .await;
    assert_eq!(servers.len(), 1);
    let server = servers.remove(0);
    assert!(
        duduclaw_llm::ToolRegistry::from_client_slots_named(
            vec![(server.name, server.google_closed.unwrap().registry_slot())],
            vec![]
        )
        .await
        .is_err()
    );
    assert_eq!(wire.hits.load(Ordering::SeqCst), 0);
    assert_eq!(count(&native), 0);
    assert_eq!(count(&h.issuer), 0);
}

#[tokio::test]
async fn w3_signed_route_truthful_tags_bind_and_one_shot_execute_actual_prepared_material() {
    for kind in ["sa", "bridge"] {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new(kind, &native).await;
        h.personal_unavailable();
        duduclaw_core::ensure_identity_key(h.f.home.path()).unwrap();
        let path = h.f.home.path().join("config.toml");
        let old = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path,format!("{old}\n[mcp_keys.ddc_prod_fedcba9876543210fedcba9876543210]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n",chrono::Utc::now().to_rfc3339())).unwrap();
        let app = crate::google_identity::execute::router(Arc::new(
            crate::google_identity::execute::ExecuteState::new(
                h.f.home.path().into(),
                Some(h.f.db.clone()),
            ),
        ));
        let args = json!({"query":"q"});
        let request = gi::GoogleSelectionRequest::SelectGoogleAccount {
            tool: "gmail_search".into(),
            arguments: args.clone(),
            credential_ref: h.reference(),
        };
        let (status, value) = d1_signed_post(
            app.clone(),
            &h.f,
            gi::GOOGLE_SELECTION_ROUTE,
            serde_json::to_value(request).unwrap(),
        )
        .await;
        assert_eq!(status, 200, "{value}");
        let stamp: gi::GoogleSelectionStamp =
            serde_json::from_value(value["selection"].clone()).unwrap();
        assert!(stamp.approval.is_well_formed());
        assert!(stamp.approval.home_connection.is_none());
        assert!(stamp.approval.home_source.is_some());
        let expected = usize::from(kind == "sa");
        assert_eq!(count(&h.issuer), expected);
        assert_eq!(count(&native), 0);
        let bind = gi::GoogleSelectionRequest::BindGoogleArguments {
            opaque_id: stamp.opaque_id.clone(),
            tool: "gmail_search".into(),
            credential_ref: h.reference(),
            original_arguments_digest: stamp.original_arguments_digest.clone(),
            account_binding_digest: stamp.approval.account_binding_digest.clone(),
            arguments: args.clone(),
        };
        assert_eq!(
            d1_signed_post(
                app.clone(),
                &h.f,
                gi::GOOGLE_SELECTION_ROUTE,
                serde_json::to_value(bind).unwrap()
            )
            .await
            .0,
            200
        );
        let execute = json!({"tool":"gmail_search","arguments":args,"credential_ref":h.reference(),"selection_stamp":stamp.opaque_id});
        let (status, result) = d1_signed_post(
            app.clone(),
            &h.f,
            crate::google_identity::execute::ROUTE,
            execute.clone(),
        )
        .await;
        assert_eq!(status, 200, "{result}");
        assert!(!result.to_string().contains(D1_SECRET));
        assert!(!result.to_string().contains(BRIDGE_SECRET));
        assert_eq!(count(&h.issuer), expected);
        assert_eq!(count(&native), 1);
        assert_eq!(
            d1_signed_post(app, &h.f, crate::google_identity::execute::ROUTE, execute)
                .await
                .0,
            403
        );
        assert_eq!(count(&native), 1);
    }
}
#[tokio::test]
async fn w3_mcp_inner_json_concat_is_clean_before_true_global_hook() {
    for kind in ["sa", "delegate"] {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new(kind, &native).await;
        let wire = m1_wire(false).await;
        let registry = m1_registry(&h.f, &wire).await;
        wire.mode.store(9, Ordering::SeqCst);
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
        assert_eq!(wire.hits.load(Ordering::SeqCst), 6);
        assert!(outcome.ccr_saved_results.is_empty());
        assert!(
            spy.values
                .lock()
                .unwrap()
                .iter()
                .any(|v| v.get("x") == Some(&json!("••• •••")))
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
        assert_eq!(count(&h.issuer), 2);
    }
}
#[tokio::test]
async fn w3_file_and_encrypted_pointer_capture_same_bytes_then_refuse_rotation() {
    for encrypted in [false, true] {
        let native = d1_wire(|_, _| D1Reply::json(json!({"messages":[]}))).await;
        let h = Harness::new("bridge", &native).await;
        std::fs::create_dir_all(h.f.home.path().join("secrets")).unwrap();
        let file = h.f.home.path().join("secrets/bridge");
        std::fs::write(&file, format!("{BRIDGE_SECRET}\n")).unwrap();
        let pointer = format!("secret://file/{}", file.display());
        let stored = if encrypted {
            let key = [7u8; 32];
            std::fs::write(h.f.home.path().join(".keyfile"), key).unwrap();
            duduclaw_security::crypto::CryptoEngine::new(&key)
                .unwrap()
                .encrypt_string(&pointer)
                .unwrap()
        } else {
            pointer
        };
        let config = h.f.home.path().join("config.toml");
        let old = std::fs::read_to_string(&config).unwrap();
        std::fs::write(config, old.replace(BRIDGE_SECRET, &stored)).unwrap();
        h.personal_unavailable();
        let store = SelectionStore::default();
        let args = json!({"query":"q"});
        let stamp = h.select(&store, "gmail_search", &args).await.unwrap();
        std::fs::write(file, "rotated-value\n").unwrap();
        assert_eq!(
            store
                .bind_dedicated(
                    h.f.home.path(),
                    Some(&h.f.db),
                    "bot",
                    &h.reference(),
                    "gmail_search",
                    &stamp.opaque_id,
                    &stamp.original_arguments_digest,
                    &stamp.approval.account_binding_digest,
                    &args,
                    Clock::now()
                )
                .unwrap_err()
                .code,
            "identity_changed"
        );
        assert_eq!(count(&native), 0);
        assert_eq!(count(&h.issuer), 0);
    }
}
#[tokio::test]
async fn w3_owned_proxy_and_env_child() {
    for kind in ["sa", "delegate", "bridge"] {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new(kind, &native).await;
        let result = h.call("gmail_search", json!({"query":"q"})).await.unwrap();
        assert_ne!(result["isError"], true);
        assert_eq!(count(&native), 1);
        assert_eq!(count(&h.issuer), usize::from(kind != "bridge"));
    }
    if let Ok(secret) = std::env::var("DUDUCLAW_W3_SECRET_FIXTURE") {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new("bridge", &native).await;
        let config = h.f.home.path().join("config.toml");
        let old = std::fs::read_to_string(&config).unwrap();
        std::fs::write(
            config,
            old.replace(BRIDGE_SECRET, "secret://env/DUDUCLAW_W3_SECRET_FIXTURE"),
        )
        .unwrap();
        let value = h.call("gmail_search", json!({"query":"q"})).await.unwrap();
        assert_ne!(value["isError"], true);
        let body = native.requests.lock().unwrap()[0].2.clone();
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["secret"],
            secret
        );
        println!("W3_OWNED_PROXY_CHILD_COMPLETE=1 ENV_CAPTURE=1");
    }
}
#[tokio::test]
async fn w3_isolated_ambient_proxy_has_zero_hits_for_new_owners() {
    let proxy = m1_wire(false).await;
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            "google_identity::effect_authority::tests::w3::w3_owned_proxy_and_env_child",
            "--nocapture",
        ])
        .env("DUDUCLAW_W3_SECRET_FIXTURE", BRIDGE_SECRET)
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
    assert!(output.status.success(), "owned child failed");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("running 1 test"));
    assert!(stdout.contains("1 passed; 0 failed"));
    assert!(stdout.contains("W3_OWNED_PROXY_CHILD_COMPLETE=1 ENV_CAPTURE=1"));
    assert_eq!(proxy.hits.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn w3_security_projection_preserves_unrelated_config_and_fences_domain_policy_changes() {
    let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
    let h = Harness::new("delegate", &native).await;
    let source = crate::google_identity::home_source::w3_observation::SourceSnapshot::capture(
        h.f.home.path(),
        "bot",
    )
    .unwrap();
    let path = h.f.home.path().join("config.toml");
    let old = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, format!("{old}\n[unrelated]\nname='changed'\n")).unwrap();
    source.validate().unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        text.replace(
            "allowed_subject_domains=['unit.test']",
            "allowed_subject_domains=['unit.test','other.test']",
        ),
    )
    .unwrap();
    assert_eq!(source.validate().unwrap_err().code, "identity_changed");
    assert_eq!(count(&h.issuer), 0);
    assert_eq!(count(&native), 0);
}

#[tokio::test]
async fn w3_immediate_final_claim_validation_rechecks_original_deadline() {
    for expired in [false, true] {
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let h = Harness::new("sa", &native).await;
        let tail =
            super::super::host_claim::immediate_tail_clock::install(h.f.home.path(), expired);
        let result = h.call("gmail_search", json!({"query":"q"})).await;
        assert_eq!(
            tail.observations(),
            1,
            "must cross the true final claim validation return"
        );
        assert_eq!(count(&h.issuer), 1);
        if expired {
            assert_eq!(result.unwrap_err().code, "selection_expired");
            assert_eq!(count(&native), 0);
        } else {
            assert_ne!(result.unwrap()["isError"], true);
            assert_eq!(count(&native), 1);
        }
    }
}

#[tokio::test]
async fn w3_oauth_private_mono_expiry_is_enforced_at_bind_claim_and_effect() {
    for stage in ["bind", "claim", "effect"] {
        let f = d1_fixture(true);
        let path = f.home.path().join(crate::mcp_oauth::TOKEN_FILE);
        let mut rows: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        rows[0]["expires_at"] =
            json!((chrono::Utc::now() + chrono::Duration::seconds(300)).to_rfc3339());
        std::fs::write(path, serde_json::to_vec(&rows).unwrap()).unwrap();
        let native = d1_wire(|_, _| D1Reply::json(json!({}))).await;
        let _native =
            crate::google_workspace::native_call::testing::install(f.home.path(), &native.endpoint);
        let store = SelectionStore::default();
        let reference = CredentialRef {
            kind: gi::CredentialKind::Legacy,
            sha256: client_digest("ddc_prod_0123456789abcdef0123456789abcdef"),
        };
        let args = json!({"query":"q"});
        let selected_clock = Clock::now();
        let stamp = store
            .select_dedicated_async(
                f.home.path(),
                Some(&f.db),
                "bot",
                &reference,
                "gmail_search",
                &args,
                selected_clock,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            stamp.expires_at,
            selected_clock.unix + gi::GOOGLE_SELECTION_TTL_SECS
        );
        if stage == "bind" {
            store.expire_w3_oauth_mono_for_test(&stamp.opaque_id);
        }
        let bound = store.bind_dedicated(
            f.home.path(),
            Some(&f.db),
            "bot",
            &reference,
            "gmail_search",
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            &args,
            Clock::now(),
        );
        if stage == "bind" {
            assert!(bound.is_err());
            assert_eq!(store.len_for_test(), 0);
        } else {
            bound.unwrap();
            if stage == "claim" {
                store.expire_w3_oauth_mono_for_test(&stamp.opaque_id);
            }
            let claimed = store.claim_dedicated(
                f.home.path(),
                Some(&f.db),
                "bot",
                &reference,
                "gmail_search",
                &stamp.opaque_id,
                &args,
                Clock::now(),
            );
            if stage == "claim" {
                assert!(claimed.is_err());
                assert_eq!(store.len_for_test(), 0);
            } else {
                let mut pending = claimed.unwrap();
                pending.claim.expire_w3_oauth_mono_for_test();
                assert_eq!(
                    pending.execute_native(args).await.unwrap_err().code,
                    "identity_changed"
                );
            }
        }
        assert_eq!(count(&native), 0);
        assert_eq!(
            crate::google_workspace::native_call::testing::observations(f.home.path()).len(),
            0
        );
    }
}

#[tokio::test]
async fn w3_bridge_secret_in_result_location_refuses_before_get() {
    use base64::Engine as _;
    let percent: String = BRIDGE_SECRET.bytes().map(|b| format!("%{b:02X}")).collect();
    let reflected = [
        BRIDGE_SECRET.to_owned(),
        percent,
        BRIDGE_SECRET.replace('&', "&amp;"),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(BRIDGE_SECRET),
    ];
    for status in [302, 303] {
        for representation in &reflected {
            let location = format!(
                "https://script.googleusercontent.com/macros/echo?user_content_key=opaqueCapability&x={representation}"
            );
            let wire = d1_wire(move |_, _| D1Reply {
                status,
                body: String::new(),
                location: Some(location.clone()),
                delay_ms: 0,
                disconnect: false,
            })
            .await;
            let h = Harness::new("bridge", &wire).await;
            let error = h
                .call("gmail_search", json!({"query":"q"}))
                .await
                .unwrap_err();
            assert_eq!(error.code, "result_unconfirmed");
            assert!(!error.message_zh.contains(BRIDGE_SECRET));
            assert!(!error.message_zh.contains(representation));
            assert_eq!(count(&wire), 1);
            assert_eq!(count(&h.issuer), 0);
            let rows = wire.requests.lock().unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].0, "POST");
            assert_eq!(
                rows.iter().filter(|(method, _, _)| method == "GET").count(),
                0
            );
        }
    }
}
