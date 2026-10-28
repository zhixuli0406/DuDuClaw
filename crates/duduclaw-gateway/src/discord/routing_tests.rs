//! Source-first real Discord handler routing regression cases. UNRUN.
use super::*;
use duduclaw_core::types::RuntimeType;
use std::sync::Mutex;

#[derive(Clone, Debug)]
struct Call {
    agent: String,
    utility: bool,
    model: String,
    dir: Option<std::path::PathBuf>,
}
fn discord_binding(kind: &str) -> toml::Value {
    toml::Value::Table(
        format!("[discord]\n[[discord.bindings]]\nkind='{kind}'\nid='C1'\n")
            .parse()
            .unwrap(),
    )
}
struct CountedRuntime {
    kind: RuntimeType,
    calls: Arc<Mutex<Vec<Call>>>,
    provider: std::sync::Weak<crate::test_channel_provider::TestChannelProvider>,
    require_typing: Arc<std::sync::atomic::AtomicBool>,
    success_reply: Arc<Mutex<Option<String>>>,
}
#[async_trait::async_trait]
impl crate::runtime::AgentRuntime for CountedRuntime {
    fn name(&self) -> &str {
        self.kind.as_str()
    }
    async fn is_available(&self) -> bool {
        true
    }
    async fn execute(
        &self,
        _: &str,
        ctx: &crate::runtime::RuntimeContext,
    ) -> Result<crate::runtime::RuntimeResponse, String> {
        self.calls.lock().unwrap().push(Call {
            agent: ctx.agent_id.clone(),
            utility: crate::runtime_dispatch::in_utility_call(),
            model: ctx.model.clone(),
            dir: ctx.agent_dir.clone(),
        });
        // A normal handler must actually send typing to our owned fixture.
        // An unmapped external attempt cannot silently fail then count as PASS.
        if self.require_typing.load(Ordering::SeqCst) {
            let provider = self
                .provider
                .upgrade()
                .expect("owned transport must remain live during dispatch");
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                while !provider
                    .requests()
                    .iter()
                    .any(|r| r.method == "POST" && r.path.ends_with("/typing"))
                {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("real typing POST must reach the owned Discord fixture");
        }
        let reply = self.success_reply.lock().unwrap().clone();
        if let Some(content) = reply {
            Ok(crate::runtime::RuntimeResponse {
                partial_timeout_secs: None,
                content,
                model_used: ctx.model.clone(),
                runtime_name: self.kind.as_str().into(),
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: 0,
            })
        } else {
            Err("401 invalid bearer token; owned runtime auth failure".into())
        }
    }
}

struct Fixture {
    ctx: Arc<ReplyContext>,
    provider: Arc<crate::test_channel_provider::TestChannelProvider>,
    token: String,
    calls: Arc<Mutex<Vec<Call>>>,
    typing: Arc<std::sync::atomic::AtomicBool>,
    success_reply: Arc<Mutex<Option<String>>>,
    cloud: crate::channel_reply::NoImplicitCloud,
    _registry: crate::runtime_dispatch::testing::RegistryGuard,
    _transport: routing_test_transport::ScopedTransport,
    _home: tempfile::TempDir,
}
impl Fixture {
    async fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().canonicalize().unwrap();
        let provider = Arc::new(crate::test_channel_provider::TestChannelProvider::start().await);
        let (token, transport) = routing_test_transport::install(&home, &provider.token);
        std::fs::write(
            home.join("config.toml"),
            "[general]\ninference_mode='claude'\n[goal_intent]\nenabled=false\nmode='off'\n",
        )
        .unwrap();
        for (name, role, display, model, runtime) in [
            ("main", "main", "Wrong Main", "claude-sonnet-4-6", "claude"),
            ("worker", "specialist", "Actual Worker", "gpt-5.4", "codex"),
        ] {
            let dir = home.join("agents").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let config = format!(
                r#"[agent]
name='{name}'
display_name='{display}'
role='{role}'
status='active'
trigger='@{name}'
reports_to=''
icon='🤖'
[model]
preferred='{model}'
fallback=''
account_pool=[]
[runtime]
provider='{runtime}'
[container]
timeout_ms=1800000
[heartbeat]
enabled=false
interval_seconds=3600
max_concurrent_runs=1
cron=''
[budget]
monthly_limit_cents=5000
warn_threshold_percent=80
hard_stop=true
[permissions]
can_create_agents=false
can_send_cross_agent=false
can_modify_own_skills=false
can_modify_own_soul=false
can_schedule_tasks=false
allowed_channels=[]
[goal_intent]
enabled=false
mode='off'
[evolution]
enabled=false
"#
            );
            toml::from_str::<duduclaw_core::types::AgentConfig>(&config).unwrap();
            std::fs::write(dir.join("agent.toml"), config).unwrap();
        }
        let mut registry = duduclaw_agent::AgentRegistry::new(home.join("agents"));
        registry.scan().await.unwrap();
        assert_eq!(registry.list().len(), 2);
        let sessions =
            Arc::new(crate::session::SessionManager::new(&home.join("sessions.db")).unwrap());
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let ctx = Arc::new(ReplyContext::new(
            Arc::new(tokio::sync::RwLock::new(registry)),
            home.clone(),
            sessions,
            Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
            tx,
        ));
        // Real owned identity store, not a privileged test identity injection.
        let users = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let admin = users
            .create_user(
                "discord-owned@example.test",
                "Owned Admin",
                "FixturePass-123",
                duduclaw_auth::UserRole::Admin,
            )
            .unwrap();
        users
            .bind_channel_identity(&admin.id, "discord", "H1", true)
            .unwrap();
        for (key, value) in [
            (keys::MENTION_ONLY, "false"),
            (keys::AUTO_THREAD, "false"),
            (keys::ADMIN_USERS, "[\"H1\"]"),
        ] {
            ctx.channel_settings
                .set("discord", "global", key, value)
                .await
                .unwrap();
        }
        let calls = Arc::new(Mutex::new(Vec::new()));
        let typing = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let success_reply = Arc::new(Mutex::new(None));
        let mut runtimes: std::collections::HashMap<
            RuntimeType,
            Box<dyn crate::runtime::AgentRuntime>,
        > = std::collections::HashMap::new();
        for kind in [RuntimeType::Codex, RuntimeType::OpenAiCompat] {
            runtimes.insert(
                kind,
                Box::new(CountedRuntime {
                    kind,
                    calls: calls.clone(),
                    provider: Arc::downgrade(&provider),
                    require_typing: typing.clone(),
                    success_reply: success_reply.clone(),
                }),
            );
        }
        let owned_registry = crate::runtime_dispatch::testing::RegistryGuard::install(
            &home,
            crate::runtime::RuntimeRegistry::with_runtimes(runtimes),
        )
        .await;
        let cloud = crate::channel_reply::NoImplicitCloud::at(&home);
        Self {
            ctx,
            provider,
            token,
            calls,
            typing,
            success_reply,
            cloud,
            _registry: owned_registry,
            _transport: transport,
            _home: temp,
        }
    }
    async fn select(&self, scope: &str, name: &str) {
        self.ctx
            .channel_settings
            .set("discord", scope, keys::AGENT_OVERRIDE, name)
            .await
            .unwrap();
    }
    fn bytes(&self, name: &str) -> Vec<u8> {
        std::fs::read(
            self.ctx
                .home_dir
                .join("agents")
                .join(name)
                .join("agent.toml"),
        )
        .unwrap()
    }
    async fn edit(&self, name: &str, edit: impl FnOnce(&mut toml::Table)) {
        let path = self
            .ctx
            .home_dir
            .join("agents")
            .join(name)
            .join("agent.toml");
        let mut table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        edit(&mut table);
        std::fs::write(path, toml::to_string(&table).unwrap()).unwrap();
        self.ctx.registry.write().await.scan().await.unwrap();
        assert_eq!(self.ctx.registry.read().await.list().len(), 2);
    }
    async fn message(&self, text: &str, bound: Option<&str>, guild: bool, thread: bool) {
        self.typing.store(true, Ordering::SeqCst);
        self.provider.enqueue_response(
            "/channels/C1",
            if guild {
                json!({"id":"C1","type":if thread {11} else {0},"guild_id":"G1"})
            } else {
                json!({"id":"C1","type":1})
            },
        );
        let mut data = json!({"id":"owned-message","channel_id":"C1","author":{"id":"H1","username":"Owned Human","bot":false},"content":text});
        if guild {
            data["guild_id"] = json!("G1");
        }
        Box::pin(handle_message_create(
            &data,
            "BOT",
            &reqwest::Client::new(),
            &self.token,
            &self.ctx,
            bound,
            "A1",
        ))
        .await;
    }
    async fn ask(&self, text: &str, bound: Option<&str>, guild: bool) {
        self.typing.store(false, Ordering::SeqCst);
        let mut data = json!({"type":2,"id":"owned-interaction","token":self.token,"channel_id":"C1","data":{"name":"ask","options":[{"name":"prompt","value":text}]}});
        if guild {
            data["guild_id"] = json!("G1");
            data["member"] =
                json!({"user":{"id":"H1","username":"Owned Human"},"permissions":"32"});
        } else {
            data["user"] = json!({"id":"H1","username":"Owned Human"});
        }
        Box::pin(handle_interaction(
            &data,
            "BOT",
            "A1",
            &reqwest::Client::new(),
            &self.token,
            &self.ctx,
            bound,
        ))
        .await;
    }
    fn final_bodies(&self) -> String {
        self.provider
            .requests()
            .into_iter()
            .filter(|r| r.path.ends_with("/messages") || r.path.ends_with("/messages/@original"))
            .map(|r| r.body.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn worker_called_once(&self, webhook: bool) {
        let calls = self.calls.lock().unwrap();
        // Pinning and other background jobs may arrive before or after this
        // snapshot. Exactly one reply is required; every observed call must
        // still belong to the worker, including any utility calls.
        assert_eq!(calls.iter().filter(|call| !call.utility).count(), 1, "{calls:?}");
        let expected_dir = self.ctx.home_dir.join("agents/worker");
        for call in calls.iter() {
            assert_eq!(call.agent, "worker", "{calls:?}");
            assert_eq!(call.model, "gpt-5.4", "{calls:?}");
            assert_eq!(call.dir.as_deref(), Some(expected_dir.as_path()), "{calls:?}");
        }
        assert_eq!(self.cloud.counts(), [0, 0]);
        let body = self.final_bodies();
        assert!(body.contains("Actual Worker"), "{body}");
        assert!(
            !body.contains("Wrong Main") && !body.contains("/login"),
            "{body}"
        );
        if webhook {
            assert!(
                self.provider
                    .requests()
                    .iter()
                    .any(|r| r.method == "PATCH" && r.path.ends_with("/messages/@original"))
            );
        }
    }
    fn no_model_or_cloud(&self) {
        assert!(self.calls.lock().unwrap().is_empty());
        assert_eq!(self.cloud.counts(), [0, 0]);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn real_normal_handler_perbot_priority_keeps_worker_for_chat_and_development() {
    for text in ["普通訊息", "直接開始開發"] {
        let f = Fixture::new().await;
        f.select("G1", "main").await;
        f.select("global", "main").await;
        f.message(text, Some("worker"), true, false).await;
        f.worker_called_once(false);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_normal_handler_guild_empty_scope_and_dm_share_global_fallback() {
    for mode in ["guild", "empty", "dm", "dm_scope"] {
        for text in ["普通訊息", "直接開始開發"] {
            let f = Fixture::new().await;
            f.select(
                "global",
                if mode == "guild" || mode == "dm_scope" {
                    "main"
                } else {
                    "worker"
                },
            )
            .await;
            f.select("G1", if mode == "guild" { "worker" } else { "" })
                .await;
            if mode == "dm_scope" {
                f.select("dm", "worker").await;
            }
            f.message(text, None, !mode.starts_with("dm"), false).await;
            f.worker_called_once(false);
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_text_model_command_uses_perbot_guild_global_and_original_channel_binding() {
    for mode in ["bot", "guild", "global", "dm_global", "binding"] {
        let f = Fixture::new().await;
        if mode == "bot" {
            f.select("G1", "main").await;
            f.select("global", "main").await;
        }
        if mode == "guild" {
            f.select("G1", "worker").await;
            f.select("global", "main").await;
        }
        if mode == "global" || mode == "dm_global" {
            f.select("global", "worker").await;
        }
        f.edit("worker", |t| {
            t["runtime"]["provider"] = toml::Value::String("claude".into());
            if mode == "binding" {
                t.insert("channels".into(), discord_binding("channel"));
            }
        })
        .await;
        let before_main = f.bytes("main");
        let _available = crate::runtime_config::testing::AvailabilityGuard::at("gpt-5.4", true);
        f.message(
            "/model gpt-5.4",
            (mode == "bot").then_some("worker"),
            mode != "dm_global",
            false,
        )
        .await;
        let worker: toml::Table = std::str::from_utf8(&f.bytes("worker"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(worker["runtime"]["provider"].as_str(), Some("codex"));
        assert_eq!(f.bytes("main"), before_main);
        assert!(f.final_bodies().contains("已修正執行環境"));
        f.no_model_or_cloud();
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_interaction_ask_preserves_bot_binding_and_guild_or_dm_global() {
    for mode in ["bot", "guild", "global", "dm", "dm_scope"] {
        let f = Fixture::new().await;
        f.select(
            "global",
            if mode == "bot" || mode == "guild" || mode == "dm_scope" {
                "main"
            } else {
                "worker"
            },
        )
        .await;
        if mode == "guild" {
            f.select("G1", "worker").await;
        }
        if mode == "bot" {
            f.select("G1", "main").await;
        }
        if mode == "dm_scope" {
            f.select("dm", "worker").await;
        }
        f.ask(
            "直接開始開發",
            (mode == "bot").then_some("worker"),
            !mode.starts_with("dm"),
        )
        .await;
        f.worker_called_once(true);
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_handlers_refuse_stale_explicit_targets_without_main_or_global_reselection() {
    for source in ["bot", "guild", "global"] {
        for entry in ["normal", "model", "ask"] {
            let f = Fixture::new().await;
            f.select(
                "global",
                if source == "global" {
                    "missing"
                } else {
                    "worker"
                },
            )
            .await;
            if source == "guild" {
                f.select("G1", "missing").await;
            }
            let bound = (source == "bot").then_some("missing");
            let before = [f.bytes("main"), f.bytes("worker")];
            if entry == "ask" {
                f.ask("hello", bound, true).await;
            } else {
                f.message(
                    if entry == "model" {
                        "/model gpt-5.4"
                    } else {
                        "hello"
                    },
                    bound,
                    true,
                    false,
                )
                .await;
            }
            f.no_model_or_cloud();
            assert_eq!([f.bytes("main"), f.bytes("worker")], before);
            let reply = f.final_bodies();
            assert!(
                reply.contains(crate::channel_reply::SELECTED_AGENT_UNAVAILABLE),
                "{reply}"
            );
            assert!(
                !reply.contains("Wrong Main")
                    && !reply.contains("missing")
                    && !reply.contains("/login")
            );
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn shared_explicit_missing_name_refuses_even_with_main_and_masks_public_visitor() {
    let f = Fixture::new().await;
    let reply =
        build_guarded_reply_for_agent("hello", &f.ctx, "removed-worker", "discord:C1", "H1", None)
            .await;
    assert_eq!(reply.text, crate::channel_reply::SELECTED_AGENT_UNAVAILABLE);
    let visitor = crate::odoo_livechat::session_key("owned", 42);
    let reply =
        build_guarded_reply_for_agent("hello", &f.ctx, "removed-worker", &visitor, "visitor", None)
            .await;
    assert_eq!(reply.text, crate::odoo_livechat::VISITOR_FALLBACK_MESSAGE);
    f.no_model_or_cloud();
}
#[tokio::test(flavor = "current_thread")]
async fn unconfigured_normal_handler_preserves_channel_thread_allowed_main_and_at_trigger_routes() {
    for mode in [
        "channel",
        "thread",
        "allowed",
        "allowed_order",
        "main",
        "configured_default",
        "trigger",
    ] {
        let f = Fixture::new().await;
        if mode == "main" || mode == "trigger" || mode == "allowed_order" {
            f.edit("main", |t| {
                t["runtime"]["provider"] = toml::Value::String("codex".into());
                t["model"]["preferred"] = toml::Value::String("gpt-5.4".into());
            })
            .await;
        }
        if mode == "allowed_order" {
            for name in ["main", "worker"] {
                f.edit(name, |t| {
                    t["permissions"]["allowed_channels"] =
                        toml::Value::Array(vec![toml::Value::String("discord".into())]);
                })
                .await;
            }
        }
        if mode == "configured_default" {
            f.edit("main", |t| {
                t["agent"]["role"] = toml::Value::String("specialist".into());
            })
            .await;
            let path = f.ctx.home_dir.join("config.toml");
            let mut config: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
            let general = config
                .get_mut("general")
                .and_then(toml::Value::as_table_mut)
                .expect("owned fixture has a general table");
            assert!(
                general
                    .insert("default_agent".into(), toml::Value::String("worker".into()))
                    .is_none()
            );
            std::fs::write(path, toml::to_string(&config).unwrap()).unwrap();
        }
        if mode == "channel" || mode == "thread" || mode == "allowed" {
            f.edit("worker", |t| {
                if mode == "allowed" {
                    t["permissions"]["allowed_channels"] =
                        toml::Value::Array(vec![toml::Value::String("discord".into())]);
                } else {
                    let kind = if mode == "thread" {
                        "thread"
                    } else {
                        "channel"
                    };
                    t.insert("channels".into(), discord_binding(kind));
                }
            })
            .await;
        }
        f.message(
            if mode == "trigger" {
                "@worker hello"
            } else {
                "hello"
            },
            None,
            true,
            mode == "thread",
        )
        .await;
        if mode == "main" || mode == "allowed_order" {
            let calls = f.calls.lock().unwrap();
            assert_eq!(calls.len(), 1);
            assert_eq!(calls[0].agent, "main");
            assert_eq!(f.cloud.counts(), [0, 0]);
        } else {
            f.worker_called_once(false);
        }
    }
}
#[tokio::test(flavor = "current_thread")]
async fn stale_binding_can_be_repaired_only_by_manage_guild_and_model_admin_is_preserved() {
    for control in ["agent", "agent_select"] {
        for allowed in [false, true] {
            let f = Fixture::new().await;
            f.select("G1", "missing").await;
            let data = json!({"type":if control == "agent" {2} else {3},"id":"control","token":f.token,"guild_id":"G1","channel_id":"C1","member":{"user":{"id":"H1"},"permissions":if allowed {"32"} else {"0"}},"data":if control == "agent" {json!({"name":"agent","options":[{"name":"name","value":"worker"}]})} else {json!({"custom_id":"duduclaw:agent_select","values":["worker"]})}});
            Box::pin(handle_interaction(
                &data,
                "BOT",
                "A1",
                &reqwest::Client::new(),
                &f.token,
                &f.ctx,
                None,
            ))
            .await;
            assert_eq!(
                f.ctx
                    .channel_settings
                    .get("discord", "G1", keys::AGENT_OVERRIDE)
                    .await
                    .as_deref(),
                Some(if allowed { "worker" } else { "missing" })
            );
            f.no_model_or_cloud();
        }
    }
    let f = Fixture::new().await;
    f.select("G1", "worker").await;
    f.ctx
        .channel_settings
        .set("discord", "global", keys::ADMIN_USERS, "[]")
        .await
        .unwrap();
    let before = [f.bytes("main"), f.bytes("worker")];
    f.message("/model claude-sonnet-4-6", None, true, false)
        .await;
    assert_eq!([f.bytes("main"), f.bytes("worker")], before);
    assert!(f.final_bodies().contains("管理員"));
    f.no_model_or_cloud();
    // The test transport cannot map another home, an unregistered dedicated
    // token or another service to this owned Discord server.
    let another_home = tempfile::tempdir().unwrap();
    for (home, token, url) in [
        (
            another_home.path(),
            f.token.as_str(),
            "https://discord.com/api/v10/channels/C1/typing",
        ),
        (
            f.ctx.home_dir.as_path(),
            "p0-test-issue210-unregistered",
            "https://discord.com/api/v10/channels/C1/typing",
        ),
        (
            f.ctx.home_dir.as_path(),
            f.token.as_str(),
            "https://www.googleapis.com/token",
        ),
    ] {
        assert!(
            std::panic::catch_unwind(|| routing_test_transport::url(home, token, url)).is_err()
        );
    }
    let blocked = Fixture::new().await;
    blocked.select("global", "missing").await;
    blocked
        .ctx
        .channel_settings
        .set("discord", "global", keys::BLOCKED_USERS, "[\"H1\"]")
        .await
        .unwrap();
    blocked.ask("hello", None, true).await;
    blocked.no_model_or_cloud();
    assert!(
        blocked.final_bodies().is_empty(),
        "blocked sender must not receive a stale-target response"
    );
    assert!(
        std::panic::catch_unwind(|| super::test_sender_url(
            &f.token,
            "https://discord.com/api/v10/channels/C1/messages"
        ))
        .is_err()
    );
    let wrong_home_sender_refused =
        routing_test_transport::scope_sender(another_home.path(), async {
            std::panic::catch_unwind(|| {
                super::test_sender_url(&f.token, "https://discord.com/api/v10/channels/C1/messages")
            })
            .is_err()
        })
        .await;
    assert!(wrong_home_sender_refused);
    let unconfigured = Fixture::new().await;
    for name in ["main", "worker"] {
        let dir = unconfigured.ctx.home_dir.join("agents").join(name);
        std::fs::rename(dir.join("agent.toml"), dir.join("agent.unloaded")).unwrap();
    }
    unconfigured
        .ctx
        .registry
        .write()
        .await
        .scan()
        .await
        .unwrap();
    assert!(unconfigured.ctx.registry.read().await.list().is_empty());
    unconfigured.message("/help", None, true, false).await;
    unconfigured.no_model_or_cloud();
    assert!(unconfigured.final_bodies().contains("/model"));
    assert!(
        !unconfigured
            .final_bodies()
            .contains(crate::channel_reply::SELECTED_AGENT_UNAVAILABLE)
    );
}

fn office_container(content: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("word/document.xml", options).unwrap();
    zip.write_all(content).unwrap();
    zip.finish().unwrap().into_inner()
}

#[tokio::test(flavor = "current_thread")]
async fn real_bound_worker_delivery_archives_and_sends_worker_multipart_never_main_same_name() {
    for mode in ["declared", "swept", "forbidden_main", "unresolved_menu"] {
        let f = Fixture::new().await;
        // Short Auto replies are plain text; embed mode exposes the employee footer.
        f.ctx
            .channel_settings
            .set("discord", "global", keys::RESPONSE_MODE, "embed")
            .await
            .unwrap();
        f.edit("worker", |t| {
            t.insert("channels".into(), discord_binding("channel"));
        })
        .await;
        let worker_file = f.ctx.home_dir.join("agents/worker/same-name.docx");
        let main_file = f.ctx.home_dir.join("agents/main/same-name.docx");
        let worker_bytes =
            office_container(b"<w:document>OWNED_WORKER_UNIQUE_PAYLOAD</w:document>");
        let main_bytes =
            office_container(b"<w:document>MAIN_SAME_NAME_MUST_NOT_BE_SENT</w:document>");
        std::fs::write(&worker_file, &worker_bytes).unwrap();
        std::fs::write(&main_file, &main_bytes).unwrap();
        *f.success_reply.lock().unwrap() = Some(match mode {
            "declared" => format!(
                "報告已完成。\n{}{}",
                crate::office_docs::DELIVER_MARKER,
                worker_file.display()
            ),
            "swept" => "報告 same-name.docx 檔案已完成。".to_owned(),
            _ => format!(
                "報告已完成。\n{}{}",
                crate::office_docs::DELIVER_MARKER,
                main_file.display()
            ),
        });
        // No explicit Discord override: the real channel binding selects Worker.
        if mode == "unresolved_menu" {
            f.edit("worker", |t| {
                t["agent"]["display_name"] = toml::Value::String("文件專員".into());
            })
            .await;
            f.message("@", None, true, false).await;
            f.no_model_or_cloud();
            assert!(
                f.final_bodies().contains("文件專員"),
                "real employee menu must have reached the reply path"
            );
        } else {
            f.message("產生一份報告", None, true, false).await;
            f.worker_called_once(false);
        }
        let requests = f.provider.requests();
        let multipart: Vec<_> = requests
            .iter()
            .filter(|r| {
                r.content_type
                    .as_deref()
                    .is_some_and(|s| s.starts_with("multipart/form-data;"))
            })
            .collect();
        if mode == "forbidden_main" || mode == "unresolved_menu" {
            assert!(
                multipart.is_empty(),
                "another employee's path must be refused before send"
            );
        } else {
            assert_eq!(
                multipart.len(),
                1,
                "a real document must be sent, not merely suppressed"
            );
            assert_eq!(multipart[0].method, "POST");
            assert!(multipart[0].path.ends_with("/channels/C1/messages"));
            let expected_authorization = format!("Bot {}", f.token);
            assert_eq!(
                multipart[0].authorization.as_deref(),
                Some(expected_authorization.as_str())
            );
            assert!(
                multipart[0]
                    .raw_body
                    .windows(worker_bytes.len())
                    .any(|b| b == worker_bytes.as_slice())
            );
            assert!(
                !multipart[0]
                    .raw_body
                    .windows(main_bytes.len())
                    .any(|b| b == main_bytes.as_slice())
            );
            let archives: Vec<_> =
                std::fs::read_dir(f.ctx.home_dir.join("agents/worker/attachments"))
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .collect();
            assert_eq!(archives.len(), 1);
            assert_eq!(std::fs::read(&archives[0]).unwrap(), worker_bytes);
            let ledger =
                std::fs::read_to_string(f.ctx.home_dir.join(crate::artifacts::ARTIFACTS_FILE))
                    .unwrap();
            let record: Value = serde_json::from_str(ledger.lines().last().unwrap()).unwrap();
            assert_eq!(record["agent_id"], "worker");
            assert_eq!(record["channel"], "discord");
            assert_eq!(record["source_path"].as_str(), worker_file.to_str());
            assert_eq!(record["origin"], mode);
            // Discord retains its existing unknown-human ownership metadata;
            // this test never forges a WebChat owner or changes an audience.
            assert!(record["owner_user_id"].is_null());
        }
        assert!(requests.iter().all(|r| {
            !r.raw_body
                .windows(main_bytes.len())
                .any(|b| b == main_bytes.as_slice())
        }));
        assert!(
            std::fs::read_dir(f.ctx.home_dir.join("agents/main/attachments"))
                .map(|mut d| d.next().is_none())
                .unwrap_or(true)
        );
        assert_eq!(std::fs::read(&main_file).unwrap(), main_bytes);
        assert_eq!(std::fs::read(&worker_file).unwrap(), worker_bytes);
    }
}
