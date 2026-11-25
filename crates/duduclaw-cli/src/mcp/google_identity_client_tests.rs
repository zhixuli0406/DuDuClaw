use super::*;

    #[test]
    fn external_identity_hides_native_tools_and_refuses_dispatch() {
        let home = home_with("bot", "[google]\nidentity='external'\nexternal_server='Partner MCP'\n");
        let caller = GoogleCaller { agent: Some("bot".into()), ..Default::default() };
        assert!(!caller.tools_listed(home.path()));
        assert!(caller.disabled_message(home.path()).contains("由外部工具提供"));
        assert!(matches!(plan_of(&Effective::External, "bot"), Plan::Refuse(message) if message.contains("由外部工具提供")));
    }

    #[tokio::test]
    async fn dedicated_receipt_keeps_its_lane_when_employee_config_becomes_invalid() {
        use crate::mcp_auth::{authenticate_with_key_call, with_authenticated_call};
        for removed in [true, false] {
            let home = home_with("bot", "[google]\nidentity='none'\n");
            let key = "ddc_dev_0123456789abcdef0123456789abcdef";
            std::fs::write(home.path().join("config.toml"), format!(
                "[mcp_keys.{key}]\nclient_id='bot'\nscopes=['google:read']\nis_external=true\ncreated_at='{}'\n",
                chrono::Utc::now().to_rfc3339(),
            )).unwrap();
            let call = authenticate_with_key_call(key, home.path()).unwrap();
            with_authenticated_call(call, async {
                let args = json!({"query":"unit-fixed"});
                let current = crate::mcp_auth::current_authenticated_call().unwrap();
                let accepted = crate::mcp_auth::accept_google_lane(
                    home.path(),
                    current.principal(),
                    "gmail_search",
                )
                .unwrap()
                .bind(args.clone());
                crate::mcp_auth::with_accepted_google_call(Some(accepted), async {
                    let initial = GoogleCaller::for_process(home.path(), "bot");
                    assert!(initial.is_dedicated());
                    assert!(
                        initial
                            .dedicated_authority(home.path(), "gmail_search", &args)
                            .is_ok()
                    );
                    assert!(
                        initial
                            .dedicated_authority(home.path(), "gmail_create_draft", &args)
                            .is_err()
                    );
                    assert!(
                        initial
                            .dedicated_authority(
                                home.path(),
                                "gmail_search",
                                &json!({"query":"changed"})
                            )
                            .is_err()
                    );
                    let path = home.path().join("agents/bot/agent.toml");
                    if removed {
                        std::fs::remove_file(path).unwrap();
                    } else {
                        std::fs::write(path, "[capabilities]\nallowed_tools=[").unwrap();
                    }
                    let caller = GoogleCaller::for_process(home.path(), "bot");
                    assert!(
                        caller.is_dedicated(),
                        "an invalid employee must not change the receipt's lane"
                    );
                    assert_eq!(caller.agent.as_deref(), Some("bot"));
                    assert!(
                        caller
                            .dedicated_authority(home.path(), "gmail_search", &args)
                            .is_err()
                    );
                    // These methods must refuse before any vault, network or
                    // ambient identity lookup; no account credentials exist here.
                    assert!(caller.token(home.path(), "gmail_search").await.is_err());
                    assert!(caller.backend(home.path(), "gmail_search").await.is_err());
                })
                .await;
            })
            .await;
        }
    }

    #[tokio::test]
    async fn internal_custom_key_without_employee_keeps_original_home_path() {
        use crate::mcp_auth::{authenticate_with_key_call, with_authenticated_call};
        let home = tempfile::tempdir().unwrap();
        let key = "ddc_dev_0123456789abcdef0123456789abcdef";
        std::fs::write(home.path().join("config.toml"), format!(
            "[mcp_keys.{key}]\nclient_id='integration-client'\nscopes=['google:read']\nis_external=false\ncreated_at='{}'\n",
            chrono::Utc::now().to_rfc3339(),
        )).unwrap();
        let call = authenticate_with_key_call(key, home.path()).unwrap();
        with_authenticated_call(call, async {
            let current = crate::mcp_auth::current_authenticated_call().unwrap();
            assert!(
                crate::mcp_auth::accept_google_lane(
                    home.path(),
                    current.principal(),
                    "gmail_search"
                )
                .is_none()
            );
            let caller = GoogleCaller::for_process(home.path(), "integration-client");
            assert!(!caller.is_dedicated());
            assert_eq!(caller.plan(home.path()), Plan::Home);
        })
        .await;
        let scope = crate::mcp_auth::with_accepted_google_call(None, std::future::pending::<()>());
        assert!(std::mem::size_of_val(&scope) <= 512);
    }

    fn home_with(agent: &str, google: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname='{agent}'\n{google}"),
        )
        .unwrap();
        home
    }

    #[test]
    fn plans_follow_the_verified_caller_only() {
        let home = home_with("bot", "[google]\nidentity='delegate:a@c.test'\n");
        let p = home.path();
        assert_eq!(
            GoogleCaller::test(Some("bot"), None).plan(p),
            Plan::Gateway {
                agent: "bot".into()
            }
        );
        // Anonymous: the home default.
        assert_eq!(GoogleCaller::test(None, None).plan(p), Plan::Home);
        // Claiming the delegate without proof: refused, not home.
        assert!(matches!(
            GoogleCaller::test(None, Some("bot")).plan(p),
            Plan::Refuse(_)
        ));
        // Claiming an employee on the home account: the home default.
        std::fs::create_dir_all(p.join("agents/plain")).unwrap();
        std::fs::write(p.join("agents/plain/agent.toml"), "[agent]\nname='plain'\n").unwrap();
        assert_eq!(GoogleCaller::test(None, Some("plain")).plan(p), Plan::Home);
    }

    #[test]
    fn none_hides_and_refuses_and_invalid_refuses() {
        let home = home_with("off", "[google]\nidentity='none'\n");
        let c = GoogleCaller::test(Some("off"), None);
        assert!(!c.tools_listed(home.path()));
        assert!(matches!(c.plan(home.path()), Plan::Refuse(_)));
        std::fs::write(
            home.path().join("agents/off/agent.toml"),
            "[agent]\nname='off'\n[google]\nidentity='bogus'\n",
        )
        .unwrap();
        assert!(
            c.tools_listed(home.path()),
            "a broken setting stays listed so the call can explain"
        );
        assert!(matches!(c.plan(home.path()), Plan::Refuse(m) if m.contains("設定有誤")));
    }

    #[tokio::test]
    async fn person_identity_without_the_gateway_is_refused_not_home() {
        let home = home_with("bot", "[google]\nidentity='delegate:a@c.test'\n");
        let c = GoogleCaller::test(Some("bot"), None);
        // No internal key in this test process: refused before any network.
        let Err(err) = c.backend(home.path(), "gmail_search").await else {
            panic!("a person identity must not resolve without the gateway");
        };
        assert!(err.contains("DuDuClaw"), "{err}");
        let out = c.finish(
            home.path(),
            "bot",
            "gmail_search",
            json!({"content":[{"type":"text","text":err}],"isError":true}),
        ).await;
        assert_eq!(out["isError"], true);
        let audit = std::fs::read_to_string(home.path().join("tool_calls.jsonl")).unwrap();
        assert!(audit.contains("\"google_identity\":\"refused\""), "{audit}");
    }

    #[tokio::test]
    async fn fallback_notice_is_prepended() {
        let home = tempfile::tempdir().unwrap();
        let c = GoogleCaller::test(Some("bot"), None);
        c.record(CallRecord {
            identity: "home",
            account: String::new(),
            user_id: None,
            fallback: true,
            notice: Some("已改用公司共用帳號處理".into()),
        });
        let out = c.finish(
            home.path(),
            "bot",
            "gmail_search",
            json!({"content":[{"type":"text","text":"結果"}]}),
        ).await;
        assert_eq!(
            out["content"][0]["text"],
            "（已改用公司共用帳號處理）\n結果"
        );
    }

    #[test]
    fn gateway_answers_are_interpreted() {
        assert!(interpret(200, br#"{"ok":true,"mode":"home"}"#).is_ok());
        let e = interpret(
            403,
            r#"{"ok":false,"code":"requester_refused","message":"這個對話的發問者沒有權限"}"#
                .as_bytes(),
        )
        .unwrap_err();
        assert!(e.contains("沒有權限"));
        assert!(interpret(502, b"<html>").is_err());
    }
