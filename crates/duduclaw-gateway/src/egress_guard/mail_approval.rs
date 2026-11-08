//! Host-attested requester binding for the native mail send approval only.
use super::*;
use crate::approval::{ApprovalBroker, ApprovalId, ExecutionBinding};
use duduclaw_knowledge::requester::RequesterIdentity;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MailRequest {
    turn_id: String,
    summary: String,
    payload: Value,
    ttl_seconds: i64,
    simulation: Option<Value>,
}

pub(super) async fn request(
    State(state): State<Arc<RouteState>>, ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap, body: Bytes,
) -> Response {
    if check_peer(peer).is_err() { return StatusCode::UNAUTHORIZED.into_response(); }
    let runtime = tokio::runtime::Handle::current();
    blocking_response(state.blocking.clone(), move || {
        runtime.block_on(handle(&state, peer, &headers, &body))
    }).await
}

async fn handle(state: &RouteState, peer: SocketAddr, headers: &HeaderMap, body: &[u8]) -> Response {
    let unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .map(|v| v.as_secs()).unwrap_or(0);
    let Ok(caller) = authenticate(&state.home, peer, headers, body, unix) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let now = Instant::now();
    if !state.rate.allow(&caller.agent_id, now) || !state.replay.record(&caller.nonce, now) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }
    let Ok(request) = serde_json::from_slice::<MailRequest>(body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(identity) = duduclaw_core::load_identity_key(&state.home) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(token) = duduclaw_core::current_agent_token(&state.home, &identity, &caller.agent_id) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let timestamp = headers.get(TIMESTAMP_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    let signature = headers.get(SIGNATURE_HEADER).and_then(|v| v.to_str().ok()).unwrap_or("");
    let Some(key) = crate::mcp_internal_key::valid_internal_keys(&state.home).into_iter().find(|key| {
        duduclaw_core::verify_internal_request_signature(key.as_bytes(), &caller.agent_id, &token,
            timestamp, &caller.nonce, body, signature)
    }) else { return StatusCode::UNAUTHORIZED.into_response(); };
    let value = match create(&state.home, &caller.agent_id, request).await {
        Ok(id) => json!({"approval_id": id.map(|id| id.to_string())}),
        Err(error) => json!({"error": error}),
    };
    let bytes = serde_json::to_vec(&value).expect("mail approval response serializable");
    let signature = duduclaw_core::internal_request_signature(key.as_bytes(), &caller.agent_id,
        &token, duduclaw_core::egress_guard::RESPONSE_DOMAIN, &caller.nonce, &bytes);
    ([("content-type", "application/json"),
        (duduclaw_core::egress_guard::RESPONSE_SIGNATURE_HEADER, signature.as_str())], bytes).into_response()
}

async fn create(home: &Path, agent: &str, request: MailRequest) -> Result<Option<ApprovalId>, String> {
    if !crate::turn_context::valid_turn_id(&request.turn_id)
        || request.payload["name"] != "mail_send"
        || !request.payload["arguments"].is_object()
        || !request.payload["egress_guard"].is_object()
        || !(1..=crate::approval::DEFAULT_TTL_SECONDS).contains(&request.ttl_seconds)
    {
        return Err("invalid mail approval request".into());
    }
    let grant = crate::approval::action_grant::ActionGrant::build(agent, "mail_send", &request.payload);
    if !grant.matches_stored(&request.payload[crate::approval::action_grant::ACTION_GRANT_KEY]) {
        return Err("mail approval requires the exact single-use action grant".into());
    }
    if !crate::approval::mail_requester::enabled(home) { return Ok(None); }
    let Some(context) = crate::turn_context::decision_context_for(agent, &request.turn_id) else {
        return Ok(None);
    };
    let Some(user) = crate::approval::mail_requester::requester_user(home, &context, agent) else {
        return Ok(None);
    };
    let requester = crate::knowledge::turn_requesters::lookup(agent, &request.turn_id);
    let registered_user = match requester.clone() {
        Some(RequesterIdentity::DashboardUser { user_id }) if context.channel == "webchat" => Some(user_id),
        Some(RequesterIdentity::ChannelUser { user_id, channel, sender, .. })
            if channel == context.channel && sender == context.principal_id => user_id,
        _ => None,
    };
    if registered_user.as_deref() != Some(user.as_str()) { return Ok(None); }
    // WebChat authority comes only from the authenticated dashboard identity.
    // Public widget ChannelUser identities never qualify, even if linked.
    let audience = if context.channel == "webchat" {
        if !matches!(requester, Some(RequesterIdentity::DashboardUser { .. })) { return Ok(None); }
        crate::approval::ConversationAudience::Private
    } else {
        let Some(target) = crate::turn_context::target_for(agent, &request.turn_id) else { return Ok(None); };
        if target.context != context { return Ok(None); }
        target.conversation_audience
    };
    let Some(session) = crate::turn_context::session_for(agent, &request.turn_id) else {
        return Ok(None);
    };
    let principal = duduclaw_core::HumanPrincipal::user(&user)
        .ok_or("invalid requester identity")?.to_string();
    let mut payload = request.payload;
    // The card's private-data warning is attested by the host too. Preserve
    // sources already observed by another MCP process and add current taint.
    let taint = query_turn(agent, Some(&request.turn_id));
    if taint.tainted {
        let mut sources = payload["egress_guard"]["sources"].as_array().cloned().unwrap_or_default();
        for source in taint.sources {
            let source = Value::String(source);
            if !sources.contains(&source) { sources.push(source); }
        }
        payload["egress_guard"]["sources"] = Value::Array(sources);
    }
    // A task id only adds restrictions. Read one current task row so the
    // revision, lifecycle and contract hash describe the same artifact.
    let task = match payload.get("task_id") {
        None => None,
        Some(value) => {
            let id = value.as_str().filter(|id| duduclaw_core::is_valid_agent_id(id))
                .ok_or("invalid mail approval task")?;
            let task = crate::task_store::TaskStore::open(home)?.get_task(id).await?
                .ok_or("mail approval task missing")?;
            if task.assigned_to != agent || !task.approval_eligible() {
                return Err("mail approval task is closed or belongs to another employee".into());
            }
            Some(task)
        }
    };
    let binding = ExecutionBinding {
        conversation_audience: audience,
        schema_version: 1,
        run_id: request.turn_id.clone(),
        run_origin_kind: "ingress".into(),
        actor_principal: agent.into(),
        decision_context: context,
        task_id: task.as_ref().map(|task| task.id.clone()),
        task_revision: task.as_ref().map(|task| task.authority_revision),
        task_snapshot_hash: task.as_ref().map(|task| task.authority_snapshot_hash()),
        payload_hash: crate::approval::payload_hash(&payload),
        policy_revision: crate::approval::policy_revision(home, agent)?,
        cwd: None,
        environment_hash: crate::approval::mail_requester::environment_hash(home, agent, &request.turn_id)?,
        file_hashes: Default::default(),
        expires_at: (chrono::Utc::now() + chrono::Duration::seconds(request.ttl_seconds)).to_rfc3339(),
        resume_handler: "live_only_v1".into(), resume_version: 1,
    };
    let broker = ApprovalBroker::open(home)?;
    let id = crate::on_behalf_of::ON_BEHALF_OF.scope(Some(principal),
        crate::approval::scope_turn(&session, &request.turn_id,
            broker.request_bound_mail(agent, &request.summary, payload, binding, request.simulation)),
    ).await?;
    // Preserve the exact row before notifying. Notification never changes who
    // may decide, and a transport outage never grants authority.
    let record = broker.get(&id).await?.ok_or("mail approval disappeared")?;
    tokio::spawn(async move { broker.push_new_request(&record).await; });
    Ok(Some(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request_for(agent: &str, turn: &str) -> MailRequest {
        let mut payload = json!({"name":"mail_send", "arguments":{
            "to":"recipient@example.com", "subject":"Status", "body":"Approved report"},
            "egress_guard":{"reason":"recipient_guard","sources":[],
                "recipient_guard":{"reason":"outside_allowed_recipients"}}});
        payload["action_grant"] = crate::approval::action_grant::ActionGrant::build(agent, "mail_send", &payload).to_json();
        MailRequest { turn_id:turn.into(), summary:"Mail confirmation".into(), payload,
            ttl_seconds:300, simulation:None }
    }

    fn fixture() -> (tempfile::TempDir, String, String) {
        let home = tempfile::tempdir().unwrap();
        let agent = format!("mail-{}", uuid::Uuid::new_v4().simple());
        let dir = home.path().join("agents").join(&agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), format!("[agent]\nid='{agent}'\n")).unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let user = db.create_user("requester@example.com", "Requester", "test-password", duduclaw_auth::UserRole::Employee).unwrap();
        db.bind_agent(&user.id, &agent, duduclaw_auth::AccessLevel::Operator).unwrap();
        (home, agent, user.id)
    }

    async fn register(agent: &str, turn: &str, user: &str) -> crate::turn_context::TurnGuard {
        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(user.into(),
            crate::knowledge::turn_requesters::CURRENT_REQUESTER.scope(
                RequesterIdentity::DashboardUser { user_id:user.into() },
                duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("requester-session".into()), async {
                    crate::turn_context::register(agent, turn, "webchat:requester-session").unwrap()
                }),
            ),
        ).await
    }

    #[tokio::test]
    async fn mail_approval_route_binds_only_the_registered_human_and_exact_arguments() {
        let (home, agent, user) = fixture();
        let _turn = register(&agent, "live", &user).await;
        let _requester = crate::knowledge::turn_requesters::register(&agent, "live",
            RequesterIdentity::DashboardUser { user_id:user.clone() }).unwrap();
        let request = request_for(&agent, "live");
        let expected = request.payload["arguments"].clone();
        let id = create(home.path(), &agent, request).await.unwrap().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let record = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(record.payload["arguments"], expected);
        assert_eq!(record.on_behalf_of, Some(format!("user:{user}")));
        assert_eq!(record.binding.as_ref().unwrap().run_id, "live");
        assert_eq!(record.binding.as_ref().unwrap().decision_context.principal_id, user);
        assert!(crate::approval::mail_requester::eligible(home.path(), &record));
        assert!(create(home.path(), "different-agent", request_for("different-agent", "live")).await.unwrap().is_none());
        assert!(create(home.path(), &agent, request_for(&agent, "invented-turn")).await.unwrap().is_none());
        let mut changed = request_for(&agent, "live");
        changed.payload["arguments"]["to"] = json!("other@example.com");
        assert!(create(home.path(), &agent, changed).await.is_err());
    }

    #[tokio::test]
    async fn mail_approval_keeps_simulation_and_task_contract_in_the_persisted_record() {
        let (home, agent, user) = fixture();
        let _turn = register(&agent, "task-turn", &user).await;
        let _requester = crate::knowledge::turn_requesters::register(&agent, "task-turn",
            RequesterIdentity::DashboardUser { user_id:user }).unwrap();
        let store = crate::task_store::TaskStore::open(home.path()).unwrap();
        let task = crate::task_store::TaskRow::new("mail-task".into(), "Send report".into(),
            String::new(), "medium".into(), agent.clone(), "human".into());
        store.insert_task(&task).await.unwrap();
        let mut request = request_for(&agent, "task-turn");
        request.payload["task_id"] = json!("mail-task");
        request.simulation = Some(json!({"world_state_change":"The report is delivered", "risk_points":["External recipient"]}));
        let id = create(home.path(), &agent, request).await.unwrap().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let row = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(row.simulation.unwrap()["world_state_change"], "The report is delivered");
        let binding = row.binding.unwrap();
        assert_eq!(binding.task_id.as_deref(), Some("mail-task"));
        assert_eq!(binding.task_snapshot_hash.as_deref(), Some(task.authority_snapshot_hash().as_str()));
        store.update_task("mail-task", &json!({"title":"Changed contract"})).await.unwrap();
        assert!(broker.decide_bound(&id, &binding.decision_context, true).await.is_err());
    }

    #[tokio::test]
    async fn mail_approval_dispatch_refuses_a_task_contract_changed_after_approval() {
        use crate::approval::action_grant::{ActionGrant, verify_and_consume};
        let (home, agent, user) = fixture();
        let _turn = register(&agent, "dispatch-turn", &user).await;
        let _requester = crate::knowledge::turn_requesters::register(&agent, "dispatch-turn",
            RequesterIdentity::DashboardUser { user_id:user }).unwrap();
        let store = crate::task_store::TaskStore::open(home.path()).unwrap();
        let task = crate::task_store::TaskRow::new("dispatch-task".into(), "Send report".into(),
            String::new(), "medium".into(), agent.clone(), "human".into());
        store.insert_task(&task).await.unwrap();
        let mut request = request_for(&agent, "dispatch-turn");
        request.payload["task_id"] = json!("dispatch-task");
        let grant = ActionGrant::build(&agent, "mail_send", &request.payload);
        let id = create(home.path(), &agent, request).await.unwrap().unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let binding = broker.get(&id).await.unwrap().unwrap().binding.unwrap();
        broker.decide_bound(&id, &binding.decision_context, true).await.unwrap();
        store.update_task("dispatch-task", &json!({"title":"Different approved work"})).await.unwrap();
        assert!(verify_and_consume(&broker, &id, &grant).await.is_err());
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Approved,
            "a changed task contract never consumes a dispatch grant");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn mail_approval_route_uses_the_verified_line_company_user_and_exact_target() {
        let (home, agent, user) = fixture();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        db.bind_channel_identity(&user, "line", "line-human", true).unwrap();
        let context = crate::approval::DecisionContext { channel:"line".into(), account_id:"company-line-oa".into(),
            conversation_id:"line-human".into(), principal_id:"line-human".into() };
        let target = crate::approval::TrustedReplyTarget::new(context.clone(), "synthetic-line-token".into(),
            "line-human".into(), None).map(|target| target.with_chat_type(Some("user")));
        let turn = crate::approval::scope_trusted_reply(target,
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("line:line-human".into()), async {
                crate::turn_context::register(&agent, "line-turn", "line:line-human").unwrap()
            }),
        ).await;
        let _requester = crate::knowledge::turn_requesters::register(&agent, "line-turn",
            RequesterIdentity::ChannelUser { channel:"line".into(), chat:"line-human".into(),
                sender:"line-human".into(), user_id:Some(user.clone()) }).unwrap();
        assert_eq!(crate::turn_context::target_for(&agent, "line-turn").unwrap().context, context);
        let id = create(home.path(), &agent, request_for(&agent, "line-turn")).await.unwrap().unwrap();
        // End the registered transport before yielding to the detached notifier:
        // this current-thread test exercises authority without contacting LINE.
        drop(turn);
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let row = broker.get(&id).await.unwrap().unwrap();
        assert_eq!(row.binding.as_ref().unwrap().decision_context, context);
        assert_eq!(row.on_behalf_of, Some(format!("user:{user}")));
        assert!(crate::approval::mail_requester::eligible(home.path(), &row));
        let mut other_person = context.clone();
        other_person.principal_id = "different-line-human".into();
        assert!(broker.decide_bound(&id, &other_person, true).await.is_err());
        broker.decide_bound(&id, &context, true).await.unwrap();
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, crate::approval::ApprovalStatus::Approved);
    }

    #[tokio::test]
    async fn mail_approval_route_never_binds_system_visitors_unverified_or_disabled_turns() {
        let (home, agent, user) = fixture();
        let _turn = register(&agent, "live", &user).await;
        for identity in [RequesterIdentity::Unknown,
            RequesterIdentity::System { kind:"cron".into(), on_behalf_of:Some(user.clone()) },
            RequesterIdentity::ChannelUser { channel:"odoolivechat".into(), chat:"visitor".into(),
                sender:user.clone(), user_id:Some(user.clone()) },
            RequesterIdentity::ChannelUser { channel:"webchat".into(), chat:"visitor".into(),
                sender:user.clone(), user_id:None },
            RequesterIdentity::DashboardUser { user_id:"another-user".into() },
        ] {
            let _requester = crate::knowledge::turn_requesters::register(&agent, "live", identity).unwrap();
            assert!(create(home.path(), &agent, request_for(&agent, "live")).await.unwrap().is_none());
        }
        let _requester = crate::knowledge::turn_requesters::register(&agent, "live",
            RequesterIdentity::DashboardUser { user_id:user }).unwrap();
        for config in ["[security]\nmail_requester_approval=false", "[security", "[security]\nmail_requester_approval='true'"] {
            std::fs::write(home.path().join("config.toml"), config).unwrap();
            assert!(create(home.path(), &agent, request_for(&agent, "live")).await.unwrap().is_none());
        }
        assert!(!home.path().join("approvals.db").exists());
    }

    #[tokio::test]
    async fn mail_approval_public_webchat_even_linked_never_gets_requester_authority() {
        let (home, agent, user) = fixture();
        let _turn = register(&agent, "public", &user).await;
        let _requester = crate::knowledge::turn_requesters::register(&agent, "public",
            RequesterIdentity::ChannelUser { channel:"webchat".into(), chat:"visitor".into(),
                sender:user.clone(), user_id:Some(user) }).unwrap();
        assert!(create(home.path(), &agent, request_for(&agent, "public")).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn mail_approval_group_transport_never_receives_private_card() {
        for (channel, kind) in [("line", Some("group")), ("telegram", Some("supergroup")), ("line", None)] {
            let (home, agent, user) = fixture();
            let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
            db.bind_channel_identity(&user, channel, "human", true).unwrap();
            let provider = crate::test_channel_provider::TestChannelProvider::start().await;
            let context = crate::approval::DecisionContext { channel:channel.into(), account_id:"account".into(),
                conversation_id:"group".into(), principal_id:"human".into() };
            let target = crate::approval::TrustedReplyTarget::new(context.clone(), provider.token.clone(), "group".into(), None)
                .map(|target| target.with_chat_type(kind));
            let _turn = crate::approval::scope_trusted_reply(target,
                duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("group-session".into()), async {
                    crate::turn_context::register(&agent, "group-turn", &format!("{channel}:group")).unwrap()
                }),
            ).await;
            let _requester = crate::knowledge::turn_requesters::register(&agent, "group-turn",
                RequesterIdentity::ChannelUser { channel:channel.into(), chat:"group".into(), sender:"human".into(), user_id:Some(user) }).unwrap();
            let id = create(home.path(), &agent, request_for(&agent, "group-turn")).await.unwrap().unwrap();
            let broker = ApprovalBroker::open(home.path()).unwrap();
            let row = broker.get(&id).await.unwrap().unwrap();
            assert!(!crate::approval::mail_requester::eligible(home.path(), &row));
            assert!(broker.decide_bound(&id, &context, true).await.is_err());
            broker.push_new_request(&row).await;
            assert!(provider.requests().is_empty(), "the origin must not receive the mail card");
        }
    }

    #[test]
    fn mail_approval_caller_cannot_supply_audience_attestation() {
        let request = request_for("alice", "turn");
        let mut value = json!({"turn_id":request.turn_id, "summary":request.summary,
            "payload":request.payload, "ttl_seconds":300, "simulation":null});
        value["conversation_audience"] = json!("private");
        assert!(serde_json::from_value::<MailRequest>(value).is_err());
    }

    #[tokio::test]
    async fn mail_approval_transport_requires_signature_and_rejects_caller_identity_fields() {
        use crate::computer_use_sessions::auth::{AGENT_ID_HEADER, NONCE_HEADER};
        let (home, agent, user) = fixture();
        let identity = duduclaw_core::ensure_identity_key(home.path()).unwrap();
        let key = crate::mcp_internal_key::ensure_internal_mcp_key_for_gateway(home.path(), "127.0.0.1:18789".parse().unwrap(), true, None).unwrap();
        let state = RouteState::new(home.path().into());
        let _turn = register(&agent, "live", &user).await;
        let _requester = crate::knowledge::turn_requesters::register(&agent, "live",
            RequesterIdentity::DashboardUser { user_id:user.clone() }).unwrap();
        let token = duduclaw_core::mint_identity_token(&identity, &agent);
        for (signed, forged_context, expected) in [(false,false,StatusCode::UNAUTHORIZED),
            (true,true,StatusCode::BAD_REQUEST), (true,false,StatusCode::OK)] {
            let req = request_for(&agent, "live");
            let mut data = json!({"turn_id":req.turn_id,"summary":req.summary,"payload":req.payload,"ttl_seconds":300,"simulation":null});
            if forged_context { data["decision_context"] = json!({"principal_id":user}); }
            let body = serde_json::to_vec(&data).unwrap();
            let timestamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs().to_string();
            let nonce = uuid::Uuid::new_v4().simple().to_string();
            let signature = duduclaw_core::internal_request_signature(key.as_bytes(), &agent, &token, &timestamp, &nonce, &body);
            let mut headers = HeaderMap::new();
            if signed {
                for (name,value) in [(AGENT_ID_HEADER,agent.as_str()),(TIMESTAMP_HEADER,timestamp.as_str()),
                    (NONCE_HEADER,nonce.as_str()),(SIGNATURE_HEADER,signature.as_str())] {
                    headers.insert(name,value.parse().unwrap());
                }
            }
            let response = handle(&state, "127.0.0.1:50001".parse().unwrap(), &headers, &body).await;
            assert_eq!(response.status(),expected);
            if expected == StatusCode::OK {
                let sig = response.headers()[duduclaw_core::egress_guard::RESPONSE_SIGNATURE_HEADER].to_str().unwrap().to_owned();
                let bytes = axum::body::to_bytes(response.into_body(),4096).await.unwrap();
                assert!(duduclaw_core::verify_internal_request_signature(key.as_bytes(), &agent, &token,
                    duduclaw_core::egress_guard::RESPONSE_DOMAIN, &nonce, &bytes, &sig));
                let value: Value = serde_json::from_slice(&bytes).unwrap();
                assert!(value["approval_id"].as_str().is_some());
                assert_eq!(handle(&state,"127.0.0.1:50001".parse().unwrap(),&headers,&body).await.status(),StatusCode::TOO_MANY_REQUESTS);
            }
        }
    }
}
