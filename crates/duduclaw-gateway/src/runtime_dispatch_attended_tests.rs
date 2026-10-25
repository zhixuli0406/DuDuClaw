//! Attendance regressions through the real detailed runtime entry point.
use super::{AgentPrompt, run_agent_prompt_detailed};
use crate::computer_use_sessions::{attended, attended_call::AttendedCall};
use crate::cost_telemetry::RequestType;
use crate::failover::{RuntimeDispatchFailure, RuntimeStepOutcome, RuntimeStepRole};
use crate::on_behalf_of::context;
use crate::runtime::{AgentRuntime, RuntimeContext, RuntimeRegistry, RuntimeResponse, RuntimeType};
use std::{collections::HashMap, future::Future, path::Path, sync::{Arc, Mutex}};
use tokio::sync::oneshot;

const AGENT: &str = "attendance-worker";
const PRINCIPAL: &str = "user:attendance-human";
const MODEL: &str = "gpt-5.4";
const REFUSAL: &str = "401 hermetic attendance fixture";

#[derive(Clone)]
struct Observation {
    prompt: String,
    reference: String,
    attendance: Option<AttendedCall>,
    was_attended: bool,
}

struct ObservingRuntime {
    observations: Arc<Mutex<Vec<Observation>>>,
    entered: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait::async_trait]
impl AgentRuntime for ObservingRuntime {
    fn name(&self) -> &str {
        "codex"
    }

    async fn is_available(&self) -> bool {
        panic!("the owned registry must not probe host runtime availability")
    }

    async fn execute(&self, prompt: &str, ctx: &RuntimeContext) -> Result<RuntimeResponse, String> {
        assert_eq!(ctx.agent_id, AGENT);
        let (_, reference) = context::env_pair().expect("real entry must install a context");
        assert_eq!(context::lookup(&ctx.home_dir, AGENT, &reference), Some(Some(PRINCIPAL.into())));
        assert_eq!(context::lookup(&ctx.home_dir, "other-worker", &reference), None);
        let turn = duduclaw_memory::feedback::CURRENT_TURN_ID
            .try_with(Clone::clone).unwrap().unwrap();
        let attendance = context::attended_call(&ctx.home_dir, AGENT, &reference);
        let was_attended = attendance.as_ref().is_some_and(|call| call.matches_turn(AGENT, &turn));
        self.observations.lock().unwrap().push(Observation {
            prompt: prompt.into(), reference: reference.clone(), attendance, was_attended,
        });

        if prompt == "nested-dispatch" {
            // Re-enter the production choke point, not runtime_scope directly.
            let error = Box::pin(run_agent_prompt_detailed(request(
                &ctx.home_dir, RequestType::Chat, "nested-chat",
            ))).await.unwrap_err();
            assert_typed_refusal(&error);
            let nested = self.observations.lock().unwrap().last().unwrap().clone();
            assert_ne!(nested.reference, reference);
            assert_eq!(context::lookup(&ctx.home_dir, AGENT, &nested.reference), None);
            assert_eq!(context::env_pair().unwrap().1, reference);
            assert_eq!(context::lookup(&ctx.home_dir, AGENT, &reference), Some(Some(PRINCIPAL.into())));
        } else if prompt == "pending" {
            let entered = self.entered.lock().unwrap().take().expect("one pending entry");
            entered.send(()).expect("test is waiting for the runtime");
            std::future::pending::<()>().await;
        }

        // Non-retryable failures neither enter cooldown nor start success telemetry.
        Err(REFUSAL.into())
    }
}

struct Fixture {
    // Remove the injection before deleting its owned home.
    _registry: super::testing::RegistryGuard,
    home: tempfile::TempDir,
    observations: Arc<Mutex<Vec<Observation>>>,
}

impl Fixture {
    async fn new(entered: Option<oneshot::Sender<()>>) -> Self {
        let home = tempfile::tempdir().unwrap();
        let observations = Arc::new(Mutex::new(Vec::new()));
        let mut runtimes: HashMap<RuntimeType, Box<dyn AgentRuntime>> = HashMap::new();
        runtimes.insert(RuntimeType::Codex, Box::new(ObservingRuntime {
            observations: observations.clone(), entered: Mutex::new(entered),
        }));
        let registry = super::testing::RegistryGuard::install(
            home.path(), RuntimeRegistry::with_runtimes(runtimes),
        ).await;
        Self { _registry: registry, home, observations }
    }

    fn observations(&self) -> Vec<Observation> {
        self.observations.lock().unwrap().clone()
    }

    fn assert_revoked(&self) {
        assert!(context::env_pair().is_none(), "entry task-local must not leak to its caller");
        for observation in self.observations() {
            assert_eq!(context::lookup(self.home.path(), AGENT, &observation.reference), None);
            if let Some(call) = observation.attendance {
                assert!(!call.valid_for(AGENT), "runtime clones must die before the live ingress");
            }
        }
    }
}

fn request<'a>(home: &'a Path, request_type: RequestType, prompt: &'a str) -> AgentPrompt<'a> {
    AgentPrompt {
        agent_dir: None, home_dir: home, agent_id: AGENT, prompt, system_prompt: "",
        model: MODEL, max_tokens: 8, provider_override: Some(RuntimeType::Codex),
        conversation_history: &[], request_type, runtime_settings: None, effort: None,
        allow_cross_family_failover: false,
    }
}

fn assert_typed_refusal(error: &RuntimeDispatchFailure) {
    assert_eq!(error.requested, Some(RuntimeType::Codex));
    assert_eq!(error.requested_model, MODEL);
    assert!(error.preflight.is_none());
    assert_eq!(error.steps.len(), 1, "one owned mock call, no fallback");
    assert_eq!(error.steps[0].role, RuntimeStepRole::Primary);
    assert_eq!(error.steps[0].runtime, RuntimeType::Codex);
    assert_eq!(error.steps[0].model.as_deref(), Some(MODEL));
    assert_eq!(error.steps[0].outcome, RuntimeStepOutcome::ExecutionError(REFUSAL.into()));
}

async fn live_human_turn<F: Future>(future: F) -> F::Output {
    let future = Box::pin(future);
    let session = format!("webchat:runtime-attendance-{}", uuid::Uuid::new_v4());
    let turn = uuid::Uuid::new_v4().to_string();
    attended::verified_webchat_message(&session, Some("attendance-human"),
        duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session.clone()),
            duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn.clone()),
                crate::on_behalf_of::scope_first(Some(PRINCIPAL.into()), async {
                    assert!(attended::bind_final_route(&session, &session, AGENT, &turn));
                    future.await
                }),
            ),
        ),
    ).await
}

#[tokio::test]
async fn detailed_entry_only_chat_mints_and_nested_chat_cannot_widen_background() {
    let fixture = Fixture::new(None).await;
    live_human_turn(async {
        for (kind, prompt) in [
            (RequestType::Chat, "chat"),
            (RequestType::Dispatch, "dispatch"),
            (RequestType::Cron, "cron"),
            (RequestType::Evolution, "evolution"),
            (RequestType::Dispatch, "nested-dispatch"),
            (RequestType::Chat, "chat-after-background"),
        ] {
            let error = run_agent_prompt_detailed(request(fixture.home.path(), kind, prompt))
                .await.unwrap_err();
            assert_typed_refusal(&error);
            fixture.assert_revoked();
        }
        let observations = fixture.observations();
        assert_eq!(observations.iter().map(|o| (o.prompt.as_str(), o.was_attended)).collect::<Vec<_>>(), [
            ("chat", true), ("dispatch", false), ("cron", false), ("evolution", false),
            ("nested-dispatch", false), ("nested-chat", false), ("chat-after-background", true),
        ]);
    }).await;
}

#[tokio::test]
async fn dropping_pending_detailed_entry_revokes_context_without_ending_human_turn() {
    let (entered, ready) = oneshot::channel();
    let fixture = Fixture::new(Some(entered)).await;
    live_human_turn(async {
        let mut pending = Box::pin(run_agent_prompt_detailed(request(
            fixture.home.path(), RequestType::Chat, "pending",
        )));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                result = &mut pending => panic!("runtime must stay pending: {result:?}"),
                entered = ready => entered.expect("mock must reach its real execute entry"),
            }
        }).await.expect("owned runtime must enter without a host CLI or provider");
        let active = fixture.observations().pop().unwrap();
        assert!(active.was_attended);
        assert!(active.attendance.as_ref().unwrap().valid_for(AGENT));
        assert_eq!(context::lookup(fixture.home.path(), AGENT, &active.reference), Some(Some(PRINCIPAL.into())));

        drop(pending);
        fixture.assert_revoked();
        let error = run_agent_prompt_detailed(request(
            fixture.home.path(), RequestType::Chat, "chat-after-cancel",
        )).await.unwrap_err();
        assert_typed_refusal(&error);
        let observations = fixture.observations();
        assert_eq!(observations.len(), 2);
        assert!(observations[1].was_attended, "the same human ingress is still live");
        assert_ne!(observations[1].reference, active.reference);
        fixture.assert_revoked();
    }).await;
}
