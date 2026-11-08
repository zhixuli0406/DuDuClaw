//! H13 boundary tests for the `spawn_agent` / `spawn_ephemeral` selectors.
use super::*;
use crate::tool_policy::{authorize, eligible};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane},
    ids::{EmployeeId, RunId, Sha256Digest}};
use serde_json::json;

fn text<const N: usize>(value: &str) -> Text<N> { Text::new(value.into()).unwrap() }
fn own() -> ResourceGrant { ResourceGrant::Employee { id: EmployeeId([2; 16]) } }
fn record(tools: &[&str], resources: Vec<ResourceGrant>, binding: EmployeeBinding, principal: Principal) -> RequestContext {
    RequestContext::from_verified_record(CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal, employee_binding: binding, lane: Lane::Standard,
        tools: List::new(tools.iter().map(|tool| text(tool)).collect()).unwrap(),
        resources: List::new(resources).unwrap(),
        policy_revision: 1, gateway_epoch: [0; 16], issued_at: 1, not_before: 1, expires_at: 2,
        nonce: [0; 32], jti: [0; 16],
    })
}
fn human(user: &str) -> Principal { Principal::Human { user_id: text(user), generation: 1 } }
fn shared() -> EmployeeBinding { EmployeeBinding::Shared(text("tenant")) }
fn personal(owner: &str) -> EmployeeBinding { EmployeeBinding::Personal(text(owner)) }

struct Fixed(Vec<ResourceGrant>);
impl ResourceResolver for Fixed {
    fn resolve(&self, _: &RequestContext, _: &ToolPolicy, _: &Value) -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(self.0.clone())
    }
}

fn ephemeral_args(tools: Value) -> Value {
    json!({"instruction":"You review invoices.", "context":"Check the March batch.", "tools": tools, "tier":"cheap"})
}

#[test]
fn spawn_ephemeral_tools_are_intersection() {
    let context = record(&["spawn_ephemeral", "wiki_read", "memory_search"], vec![own()], shared(), human("alice"));
    // Request order, prefix-normalized, deduplicated; tools the parent run does
    // not hold (native or MCP) are dropped, never granted.
    let tools = ephemeral_tools(&context, &ephemeral_args(json!(["mcp__duduclaw__wiki_read", "Read", "web_search",
        "wiki_read", "memory_search"]))).unwrap();
    assert_eq!(tools, vec!["wiki_read".to_owned(), "memory_search".to_owned()]);
    assert_eq!(ephemeral_tools(&context, &ephemeral_args(json!("memory_search, Bash"))).unwrap(),
        vec!["memory_search".to_owned()]);
    // Empty intersection, wildcards and malformed names refuse.
    for tools in [json!(["Read", "web_search"]), json!(["mcp__duduclaw__*"]), json!([]), json!(""),
        json!([1]), json!(["wiki\u{7}read"])] {
        assert!(ephemeral_tools(&context, &ephemeral_args(tools.clone())).is_err(), "{tools}");
    }
    let too_many: Vec<String> = (0..=EPHEMERAL_MAX_TOOLS).map(|index| format!("tool_{index}")).collect();
    assert!(requested_tools(&ephemeral_args(json!(too_many))).is_err());
    // The call holds exactly the own Employee grant: no grant follows the tools.
    let call = authorize(&context, "spawn_ephemeral", &ephemeral_args(json!(["wiki_read", "Read"])),
        &Fixed(vec![own()])).unwrap();
    assert_eq!(call.resources(), &[own()]);
    assert!(authorize(&context, "spawn_ephemeral", &ephemeral_args(json!(["Read"])), &Fixed(vec![own()])).is_err());
}

#[test]
fn spawn_ephemeral_personal_parent_owner_only() {
    let args = ephemeral_args(json!(["wiki_read"]));
    let tools = ["spawn_ephemeral", "wiki_read"];
    let owner = record(&tools, vec![own()], personal("owner"), human("owner"));
    assert!(eligible(&owner, "spawn_ephemeral"));
    assert!(authorize(&owner, "spawn_ephemeral", &args, &Fixed(vec![own()])).is_ok());
    // Another person, or a background principal, can never cause a Personal
    // employee's child run.
    let background = Principal::Background { tenant_id: text("tenant"), generation: 1 };
    for principal in [human("intruder"), background] {
        let context = record(&tools, vec![own()], personal("owner"), principal);
        assert!(!eligible(&context, "spawn_ephemeral"));
        assert_eq!(ephemeral_parent_allowed(&context), Err(VerificationError::BindingMismatch));
        assert!(authorize(&context, "spawn_ephemeral", &args, &Fixed(vec![own()])).is_err());
    }
    // A Shared parent has no owner restriction at the record (membership is
    // re-validated by the host resolver).
    let member = record(&tools, vec![own()], shared(), human("member"));
    assert!(eligible(&member, "spawn_ephemeral"));
}

#[test]
fn spawn_selectors_pin_own_employee_and_record_task() {
    let args = json!({"agent_id":"target", "task":"Prepare the report."});
    let task = ResourceGrant::Task { id: text("task-1") };
    let interactive = record(&["spawn_agent"], vec![own()], shared(), human("alice"));
    assert!(eligible(&interactive, "spawn_agent"));
    assert_eq!(authorize(&interactive, "spawn_agent", &args, &Fixed(vec![own()])).unwrap().resources(), &[own()]);
    // The resolver can neither substitute another employee nor add a grant.
    let other = ResourceGrant::Employee { id: EmployeeId([9; 16]) };
    for resources in [vec![], vec![other.clone()], vec![own(), other], vec![own(), task.clone()]] {
        assert!(authorize(&interactive, "spawn_agent", &args, &Fixed(resources)).is_err());
    }
    // Without its own Employee grant nothing lists or authorizes.
    let bare = record(&["spawn_agent", "spawn_ephemeral"], vec![], shared(), human("alice"));
    assert!(!eligible(&bare, "spawn_agent") && !eligible(&bare, "spawn_ephemeral"));
    // A task run selects exactly its own record task as well; a record task
    // without its signed grant is not eligible.
    let mut with_task = interactive.record().clone();
    with_task.task = Some(text("task-1"));
    let ungranted = RequestContext::from_verified_record(with_task.clone());
    assert!(!eligible(&ungranted, "spawn_agent"));
    with_task.resources = List::new(vec![own(), task.clone()]).unwrap();
    let task_run = RequestContext::from_verified_record(with_task);
    assert_eq!(spawn_selector(&task_run, "spawn_agent").unwrap(), vec![own(), task.clone()]);
    assert!(authorize(&task_run, "spawn_agent", &args, &Fixed(vec![task.clone(), own()])).is_ok());
    assert!(authorize(&task_run, "spawn_agent", &args, &Fixed(vec![own()])).is_err());
    assert!(spawn_selector(&task_run, "create_task").is_err());
}

#[test]
fn spawn_arguments_are_exact() {
    assert!(check_spawn_arguments("spawn_agent", &json!({"agent_id":"target", "task":"work", "session_key":"s"})).is_ok());
    for forged in ["sender_agent", "origin_agent", "delegation_depth", "hop_depth", "turn_id", "on_behalf_of",
        "principal", "employee_id", "run_id", "lane"] {
        let mut args = json!({"agent_id":"target", "task":"work"});
        args[forged] = json!("forged");
        assert!(check_spawn_arguments("spawn_agent", &args).is_err(), "{forged}");
        let mut args = ephemeral_args(json!(["wiki_read"]));
        args[forged] = json!("forged");
        assert!(check_spawn_arguments("spawn_ephemeral", &args).is_err(), "{forged}");
    }
    for args in [json!({"agent_id":"../target", "task":"work"}), json!({"agent_id":"target", "task":""}),
        json!({"agent_id":"target"}), json!({"agent_id":"target", "task":"work", "session_key":7}),
        json!({"agent_id":"target", "task":"x".repeat(SPAWN_TEXT_MAX_BYTES + 1)})] {
        assert!(check_spawn_arguments("spawn_agent", &args).is_err(), "{args}");
    }
    let mut raw_model = ephemeral_args(json!(["wiki_read"]));
    raw_model["tier"] = json!("claude-opus-4");
    assert!(check_spawn_arguments("spawn_ephemeral", &raw_model).is_err());
    let mut no_context = ephemeral_args(json!(["wiki_read"]));
    no_context.as_object_mut().unwrap().remove("context");
    assert!(check_spawn_arguments("spawn_ephemeral", &no_context).is_err());
    assert!(check_spawn_arguments("create_task", &json!({})).is_err());
}
