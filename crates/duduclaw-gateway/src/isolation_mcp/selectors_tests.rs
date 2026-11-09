//! H9 selector / H10 issuer tests. Real profile (`agent.toml`), task store
//! (`tasks.db`) and authority stores; only the root ReadBack and the root work
//! broker are fixtures (the broker is never called while issuing).
use super::*;
use std::{cell::Cell, os::unix::fs::PermissionsExt, sync::atomic::{AtomicU32, Ordering}};
use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord, Lane, PublicOrigin,
    ResourceGrant}, ids::{RunId, Sha256Digest}, ipc::SpawndFeatures};
use duduclaw_mcp_boundary::{resources::{BrokeredWorkFile, WorkBroker, WorkBrokerError, WorkFileRequest,
    EMPLOYEE_WORK_ROOT}, tool_policy::{eligible, IN_RUN_HELPER_TOOLS},
    workspace_selection::PENDING_WORKSPACE_SELECTOR_TOOLS, RequestContext};
use duduclaw_migration::{allocation::Receipt, contract::ConfirmationItem};
use crate::isolation_lifecycle::LifecycleError;

const LABEL: &str = "alice";
const EMPLOYEE: EmployeeId = EmployeeId([2; 16]);
type Authorization = super::super::RuntimeAuthorization;

/// Stands in for H5's spawnd bridge. Issuance must never open a file.
struct NoBroker;
impl WorkBroker for NoBroker {
    fn open_work_file(&self, _: &WorkFileRequest<'_>) -> Result<BrokeredWorkFile, WorkBrokerError> {
        Err(WorkBrokerError::Unavailable)
    }
}

struct Fixture { dir: tempfile::TempDir, employee: test_support::SharedEmployee }
impl Fixture {
    fn home(&self) -> &Path { self.dir.path() }
    fn profile_path(&self) -> std::path::PathBuf { self.home().join("agents").join(LABEL).join("agent.toml") }
    /// `public_web` is the admin-only `[isolation]` switch; the task sandbox's
    /// `[container] network_access` stays off.
    fn profile(&self, public_web: bool, capabilities: &str) { self.profile_with(false, public_web, capabilities); }
    fn profile_with(&self, network_access: bool, public_web: bool, capabilities: &str) {
        let path = self.profile_path();
        let _ = std::fs::remove_file(&path);
        std::fs::write(path, test_support::agent_toml_with(LABEL, network_access, public_web, capabilities)).unwrap();
    }
    /// `security_audit.jsonl` rows of `event_type`.
    fn audit(&self, event_type: &str) -> Vec<serde_json::Value> {
        std::fs::read_to_string(self.home().join("security_audit.jsonl")).unwrap_or_default().lines()
            .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
            .filter(|row| row["event_type"] == event_type).collect()
    }
    fn tasks(&self, rows: &[(&str, &str)]) {
        let path = self.home().join("tasks.db");
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE IF NOT EXISTS tasks(id TEXT PRIMARY KEY, assigned_to TEXT NOT NULL)").unwrap();
        for (id, agent) in rows {
            db.execute("INSERT OR REPLACE INTO tasks(id, assigned_to) VALUES(?1, ?2)", rusqlite::params![id, agent]).unwrap();
        }
        drop(db);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    fn background(&self, task: &str) -> crate::isolation_background::RecordAuthority {
        let current = self.employee.snapshot();
        serde_json::from_value(serde_json::json!({
            "home": self.home(), "kind": "task", "id": task, "agent": LABEL,
            "employee": EMPLOYEE, "owner": self.employee.user(), "epoch": self.employee.epoch(),
            "acl_generation": current.acl_generation, "policy_epoch": current.policy_epoch,
            "policy": "policy", "material": "material", "approval": null, "expires_at": u64::MAX,
            "lane": Lane::Standard, "caller": null, "parent_run": null, "parent_agent": null,
            "parent_policy": null, "parent": null, "detached_bus": false,
        })).unwrap()
    }
    fn issue(&self, names: &[&str]) -> Result<Authorization, VerificationError> {
        self.employee.authorize(&self.employee.snapshot(), Lane::Standard, tools(names), turn(None), None)
    }
    fn issue_task(&self, names: &[&str], task: &str) -> Result<Authorization, VerificationError> {
        self.employee.authorize_record(&self.employee.snapshot(), Lane::Standard, tools(names),
            turn(Some(self.background(task))), None, Some(task))
    }
    /// The production constructor with a wrapped root ReadBack.
    fn issue_with(&self, root: &dyn RootAllocation, names: &[&str], task: Option<&str>)
        -> Result<Authorization, VerificationError> {
        let lifecycle = lifecycle::read_lifecycle_with(self.home(), LABEL, root).map_err(lifecycle_refusal)?;
        human_authorization_with(self.home().to_owned(), LABEL.into(), EMPLOYEE, self.employee.user().into(),
            self.employee.epoch(), Lane::Standard, &self.employee.snapshot(), tools(names),
            turn(task.map(|task| self.background(task))), None, &lifecycle, root, self.employee.sources(), task)
    }
}

fn fixture(broker: bool) -> Fixture { fixture_with(broker, None) }

/// spawnd's readiness view as the selector reads it; the bits can change
/// between reads (spawnd restarted as another build).
fn feature_view(bits: &Arc<AtomicU32>) -> selectors::SpawndFeatureView {
    let bits = bits.clone();
    Arc::new(move || SpawndFeatures(bits.load(Ordering::SeqCst)))
}
fn bits(features: SpawndFeatures) -> Arc<AtomicU32> { Arc::new(AtomicU32::new(features.0)) }
/// Broker installed and spawnd advertising `features`.
fn helper_fixture(features: SpawndFeatures) -> (Fixture, Arc<AtomicU32>) {
    let bits = bits(features);
    (fixture_with(true, Some(feature_view(&bits))), bits)
}

fn fixture_with(broker: bool, features: Option<selectors::SpawndFeatureView>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("agents").join(LABEL)).unwrap();
    let mut employee = test_support::SharedEmployee::new(dir.path(), LABEL, EMPLOYEE);
    if broker { employee = employee.with_work_broker(Arc::new(NoBroker)); }
    if let Some(view) = features { employee = employee.with_spawnd_features(view); }
    let fixture = Fixture { dir, employee };
    fixture.profile(false, "");
    fixture
}

fn tools(names: &[&str]) -> List<Text<128>, 256> {
    List::new(names.iter().map(|name| Text::new((*name).to_owned()).unwrap()).collect()).unwrap()
}

fn turn(background: Option<crate::isolation_background::RecordAuthority>) -> super::super::HostTurn {
    super::super::HostTurn { identity: crate::isolation_egress::RunTurnIdentity {
        turn: "h9-turn".into(), approval_session: None }, egress_turn: None,
        metadata: None, principal_context: None, background }
}

fn capability(auth: &Authorization) -> CapabilityRecord {
    CapabilityRecord { id: CapabilityId(Sha256Digest([1; 32])), employee: EMPLOYEE, run: RunId([3; 16]),
        task: auth.grant.task.clone(), principal: auth.grant.principal.clone(),
        employee_binding: auth.grant.binding.clone(), lane: auth.grant.lane, tools: auth.grant.tools.clone(),
        resources: auth.grant.resources.clone(), policy_revision: auth.grant.policy_revision,
        gateway_epoch: [4; 16], issued_at: auth.grant.not_before, not_before: auth.grant.not_before,
        expires_at: auth.grant.expires_at, nonce: [5; 32], jti: [6; 16] }
}

fn context(auth: &Authorization) -> RequestContext { RequestContext::from_verified_record(capability(auth)) }

fn has(auth: &Authorization, grant: &ResourceGrant) -> bool { auth.grant.resources.as_slice().contains(grant) }

fn workspace() -> ResourceGrant { ResourceGrant::Workspace { handle: Text::new(EMPLOYEE_WORK_ROOT.into()).unwrap() } }
fn public_any() -> ResourceGrant { ResourceGrant::PublicOrigins { origins: List::new(vec![PublicOrigin::any()]).unwrap() } }
fn task_grant(id: &str) -> ResourceGrant { ResourceGrant::Task { id: Text::new(id.into()).unwrap() } }
fn any_public(auth: &Authorization) -> bool {
    auth.grant.resources.as_slice().iter().any(|grant| matches!(grant, ResourceGrant::PublicOrigins { .. }))
}
fn any_task(auth: &Authorization) -> bool {
    auth.grant.resources.as_slice().iter().any(|grant| matches!(grant, ResourceGrant::Task { .. }))
}
fn new_task(requester: &str) -> ResourceGrant { ResourceGrant::NewTask { requester: Text::new(requester.into()).unwrap() } }
fn any_new_task(auth: &Authorization) -> bool {
    auth.grant.resources.as_slice().iter().any(|grant| matches!(grant, ResourceGrant::NewTask { .. }))
}

#[test]
fn h9_workspace_grant_only_when_profile_allows() {
    let f = fixture(true);
    let auth = f.issue(&["file_read", "csv_read"]).unwrap();
    assert!(has(&auth, &workspace()));
    assert!(auth.roots.is_brokered(EMPLOYEE_WORK_ROOT), "the grant comes with the brokered root, never a dirfd");
    let ctx = context(&auth);
    for tool in ["file_read", "csv_read"] { assert!(eligible(&ctx, tool), "{tool}"); }
    assert!(!eligible(&ctx, "xlsx_read"), "a tool outside the run's snapshot stays hidden");

    // The profile is re-read at issuance: a denial or an allowlist without
    // any listed file tool withholds the grant and the root.
    for capabilities in [r#"denied_tools = ["file_read", "csv_read", "xlsx_read"]"#, r#"allowed_tools = ["tasks_update"]"#] {
        f.profile(false, capabilities);
        let auth = f.issue(&["file_read", "csv_read"]).unwrap();
        assert!(!has(&auth, &workspace()), "{capabilities}");
        assert!(!auth.roots.is_brokered(EMPLOYEE_WORK_ROOT));
        assert!(!eligible(&context(&auth), "file_read"));
    }
    f.profile(false, r#"allowed_tools = ["file_read"]"#);
    assert!(has(&f.issue(&["file_read"]).unwrap(), &workspace()));
    // No file tool in the run's snapshot: nothing to select.
    assert!(!has(&f.issue(&["tasks_update"]).unwrap(), &workspace()));

    // Without an installed root work broker there is no Workspace grant at all.
    let g = fixture(false);
    let auth = g.issue(&["file_read", "csv_read", "xlsx_read"]).unwrap();
    assert!(!auth.grant.resources.as_slice().iter().any(|grant| matches!(grant, ResourceGrant::Workspace { .. })));
    assert!(!auth.roots.is_brokered(EMPLOYEE_WORK_ROOT));
    assert!(!eligible(&context(&auth), "file_read"));
}

fn signed(auth: &Authorization) -> Vec<&str> { auth.grant.tools.as_slice().iter().map(|tool| tool.as_str()).collect() }

#[test]
fn h10_file_workspace_never_signs_execution_tools() {
    // `employee-work` is also the A workspace handle. A grant selected for
    // the file tools alone never signs the A/E tools it would enable: the
    // pending `skill_from_recording`, the HX6 recording tools (no recording
    // gate here) and — without spawnd's `helper_calls` — the HX5 in-run
    // helpers too. (H12b
    // hub install is no longer pending: it rests on PublicOrigins, see h9.)
    let names = ["file_read", "browser_record_start", "browser_record_stop", "execute_program", "code_map",
        "skill_from_recording", "tasks_update"];
    for features in [None, Some(SpawndFeatures::NONE), Some(SpawndFeatures::WORK_BROKER)] {
        let f = fixture_with(true, features.map(|features| feature_view(&bits(features))));
        let auth = f.issue(&names).unwrap();
        assert!(has(&auth, &workspace()));
        assert_eq!(signed(&auth), ["file_read", "tasks_update"], "{features:?}");
        let ctx = context(&auth);
        assert!(eligible(&ctx, "file_read"));
        for tool in &names[1..6] { assert!(!eligible(&ctx, tool), "{features:?}: {tool}"); }
    }
    // HX5 truth: with `helper_calls` advertised the same grant also signs the
    // in-run helpers, and still never the pending tools.
    let (f, _) = helper_fixture(SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::WORK_BROKER));
    let auth = f.issue(&names).unwrap();
    assert_eq!(signed(&auth), ["file_read", "execute_program", "code_map", "tasks_update"]);
    let ctx = context(&auth);
    for tool in ["file_read", "execute_program", "code_map"] { assert!(eligible(&ctx, tool), "{tool}"); }
    for tool in ["browser_record_start", "browser_record_stop", "skill_from_recording"] { assert!(!eligible(&ctx, tool), "{tool}"); }
    // No Workspace selected: the snapshot is signed unchanged.
    let auth = fixture(false).issue(&names).unwrap();
    assert_eq!(auth.grant.tools.as_slice().len(), names.len());
}

#[test]
fn hx5_helper_tools_get_workspace_without_file_tools() {
    let names = ["execute_program", "code_map", "office_script", "tasks_update"];
    let (f, view) = helper_fixture(SpawndFeatures::HELPER_CALLS);
    let auth = f.issue(&names).unwrap();
    assert!(has(&auth, &workspace()), "an employee without file tools still gets the helper workspace");
    assert!(auth.roots.is_brokered(EMPLOYEE_WORK_ROOT), "the brokered root, never a dirfd");
    assert_eq!(signed(&auth), names);
    let ctx = context(&auth);
    for tool in IN_RUN_HELPER_TOOLS { assert!(eligible(&ctx, tool), "{tool}"); }
    for tool in ["file_read", "csv_read"] { assert!(!eligible(&ctx, tool), "{tool}: not in the run's snapshot"); }
    // A task-limited run keeps the helper workspace and the helpers.
    f.tasks(&[("task-a", LABEL)]);
    let auth = f.issue_task(&names, "task-a").unwrap();
    assert!(has(&auth, &workspace()) && has(&auth, &task_grant("task-a")));
    assert_eq!(signed(&auth), names);
    capability(&auth).canonical_bytes().unwrap();

    // Fail closed: no `helper_calls` bit, no view, or no broker ⇒ no grant,
    // no root and the helpers stay hidden (signed unchanged, never eligible).
    let without = |f: &Fixture| {
        let auth = f.issue(&names).unwrap();
        assert!(!auth.grant.resources.as_slice().iter().any(|grant| matches!(grant, ResourceGrant::Workspace { .. })));
        assert!(!auth.roots.is_brokered(EMPLOYEE_WORK_ROOT));
        for tool in IN_RUN_HELPER_TOOLS { assert!(!eligible(&context(&auth), tool), "{tool}"); }
    };
    view.store(SpawndFeatures::WORK_BROKER.0, Ordering::SeqCst);
    without(&f);
    view.store(SpawndFeatures::NONE.0, Ordering::SeqCst);
    without(&f);
    without(&fixture(true));
    without(&fixture_with(false, Some(feature_view(&bits(SpawndFeatures::HELPER_CALLS)))));
    // The profile is re-read: denying every listed helper withholds the grant.
    view.store(SpawndFeatures::HELPER_CALLS.0, Ordering::SeqCst);
    f.profile(false, r#"denied_tools = ["execute_program", "code_map", "office_script"]"#);
    without(&f);
    f.profile(false, r#"allowed_tools = ["code_map"]"#);
    assert!(has(&f.issue(&["code_map"]).unwrap(), &workspace()));
    f.profile(false, "");
    // An unreadable profile fails the issuance closed, as for the file tools.
    std::fs::remove_file(f.profile_path()).unwrap();
    assert_eq!(f.issue(&["execute_program"]).err(), Some(VerificationError::Unavailable));
    f.profile(false, "");
    // spawnd losing `helper_calls` while the issuer does its I/O is a changed
    // selector input: the post-I/O re-read refuses the run.
    let lost = during(&f, Box::new(|| view.store(SpawndFeatures::WORK_BROKER.0, Ordering::SeqCst)));
    assert_eq!(f.issue_with(&lost, &names, None).err(), Some(VerificationError::Revoked));
}

#[test]
fn hx5_pending_tools_still_never_signed() {
    let (f, _) = helper_fixture(SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::WORK_BROKER));
    f.tasks(&[("task-a", LABEL)]);
    for with_files in [false, true] {
        let mut names: Vec<&str> = PENDING_WORKSPACE_SELECTOR_TOOLS.to_vec();
        names.extend(IN_RUN_HELPER_TOOLS);
        if with_files { names.push("file_read"); }
        for task in [None, Some("task-a")] {
            let auth = match task { None => f.issue(&names), Some(task) => f.issue_task(&names, task) }.unwrap();
            assert!(has(&auth, &workspace()), "{with_files} {task:?}");
            let ctx = context(&auth);
            for tool in PENDING_WORKSPACE_SELECTOR_TOOLS {
                assert!(!signed(&auth).contains(tool), "{tool} signed ({with_files} {task:?})");
                assert!(!eligible(&ctx, tool), "{tool} ({with_files} {task:?})");
            }
            // HX6: routed (own selector), so no longer on the pending list.
            for tool in ["browser_record_start", "browser_record_stop"] {
                assert!(!PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&tool), "{tool}");
            }
            for tool in IN_RUN_HELPER_TOOLS {
                assert!(!PENDING_WORKSPACE_SELECTOR_TOOLS.contains(&tool), "{tool}");
                assert!(eligible(&ctx, tool), "{tool} ({with_files} {task:?})");
            }
        }
    }
}

/// HX6: the recording is signed only with spawnd's `helper_calls` AND
/// `helper_recording` (Chromium proven under the helper sandbox), the
/// admin-only public-web egress switch (Q3 = A) and an interactive run.
#[test]
fn hx6_recording_selector_requires_host_gate_and_public_web() {
    let names = ["browser_record_start", "browser_record_stop", "tasks_update"];
    let recording = SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::HELPER_RECORDING);
    let (f, view) = helper_fixture(recording);
    // No public-web switch: no workspace, nothing signed for recording.
    let auth = f.issue(&names).unwrap();
    assert!(!has(&auth, &workspace()));
    for tool in &names[..2] { assert!(!eligible(&context(&auth), tool), "{tool}"); }
    f.profile(true, "");
    let auth = f.issue(&names).unwrap();
    assert!(has(&auth, &workspace()) && auth.roots.is_brokered(EMPLOYEE_WORK_ROOT));
    assert_eq!(signed(&auth), names);
    for tool in &names[..2] { assert!(eligible(&context(&auth), tool), "{tool}"); }
    // Fail closed: helper calls without the recording gate, no bits, or a task run.
    for bits in [SpawndFeatures::HELPER_CALLS, SpawndFeatures::HELPER_RECORDING, SpawndFeatures::NONE] {
        view.store(bits.0, Ordering::SeqCst);
        let auth = f.issue(&names).unwrap();
        for tool in &names[..2] { assert!(!eligible(&context(&auth), tool), "{bits:?} {tool}"); }
    }
    view.store(recording.0, Ordering::SeqCst);
    f.tasks(&[("task-a", LABEL)]);
    let auth = f.issue_task(&names, "task-a").unwrap();
    assert!(!has(&auth, &workspace()));
    for tool in &names[..2] { assert!(!eligible(&context(&auth), tool), "task runs never record: {tool}"); }
    // The foreground helper bit alone never signs the recording.
    let auth = f.issue(&["browser_record_start", "execute_program"]).unwrap();
    assert_eq!(signed(&auth), ["browser_record_start", "execute_program"]);
    f.profile(false, "");
    let auth = f.issue(&["browser_record_start", "execute_program"]).unwrap();
    assert_eq!(signed(&auth), ["execute_program"]);
}

/// HX6: the boundary-side resolver answer for a recording call (Employee +
/// Workspace; the URL authority is the record's own PublicOrigins).
struct RecordingResolver;
impl duduclaw_mcp_boundary::tool_policy::ResourceResolver for RecordingResolver {
    fn resolve(&self, context: &RequestContext, _: &duduclaw_mcp_boundary::tool_policy::ToolPolicy, _: &serde_json::Value)
        -> Result<Vec<ResourceGrant>, VerificationError> {
        Ok(vec![ResourceGrant::Employee { id: context.record().employee }, workspace()])
    }
}

/// HX6 url authority: a selected recording signs `PublicOrigins { * }` (never
/// a UrlOrigin), so a public start is authorized; without `[isolation]
/// public_web` nothing is signed and the start is refused. A grant signed for
/// the recording alone never makes the web tools eligible.
#[test]
fn hx6url_recording_signs_public_origins_only_with_public_web() {
    use duduclaw_mcp_boundary::tool_policy::authorize;
    let names = ["browser_record_start", "browser_record_stop", "tasks_update"];
    let start = serde_json::json!({"url":"https://news.example.org/a","headless":true});
    let recording = SpawndFeatures::HELPER_CALLS.with(SpawndFeatures::HELPER_RECORDING);
    let (f, view) = helper_fixture(recording);
    // public_web off: no PublicOrigins, and the start is refused.
    let auth = f.issue(&names).unwrap();
    assert!(!any_public(&auth));
    assert!(authorize(&context(&auth), "browser_record_start", &start, &RecordingResolver).is_err());

    f.profile(true, "");
    let auth = f.issue(&names).unwrap();
    assert!(has(&auth, &public_any()) && has(&auth, &ResourceGrant::EmployeeScope { id: EMPLOYEE }));
    assert!(!auth.grant.resources.as_slice().iter().any(|grant| matches!(grant, ResourceGrant::UrlOrigin { .. })));
    capability(&auth).canonical_bytes().expect("recording record stays signable");
    let ctx = context(&auth);
    let call = authorize(&ctx, "browser_record_start", &start, &RecordingResolver).unwrap();
    assert!(call.resources().contains(&public_any()));
    assert!(!call.resources().iter().any(|grant| matches!(grant, ResourceGrant::UrlOrigin { .. })));
    for url in ["https://127.0.0.1/", "https://localhost/", "https://user:pw@news.example.org/"] {
        let args = serde_json::json!({"url":url,"headless":true});
        assert!(authorize(&ctx, "browser_record_start", &args, &RecordingResolver).is_err(), "{url}");
    }

    // Recording selected, web tools denied by the profile: the grant is signed
    // for the recording only and the web tools leave the snapshot.
    let with_web = ["browser_record_start", "browser_record_stop", "web_fetch_cached", "web_extract"];
    f.profile(true, r#"denied_tools = ["web_fetch_cached", "web_extract"]"#);
    let auth = f.issue(&with_web).unwrap();
    assert!(has(&auth, &public_any()));
    assert_eq!(signed(&auth), ["browser_record_start", "browser_record_stop"]);
    for tool in ["web_fetch_cached", "web_extract"] { assert!(!eligible(&context(&auth), tool), "{tool}"); }
    // Both selected: one grant, the web tools stay signed.
    f.profile(true, "");
    let auth = f.issue(&with_web).unwrap();
    assert_eq!(auth.grant.resources.as_slice().iter().filter(|grant| matches!(grant, ResourceGrant::PublicOrigins { .. })).count(), 1);
    assert_eq!(signed(&auth), with_web);

    // The recording gate closed: no recording, so no PublicOrigins for it.
    view.store(SpawndFeatures::HELPER_CALLS.0, Ordering::SeqCst);
    let auth = f.issue(&names).unwrap();
    assert!(!any_public(&auth));
    assert!(authorize(&context(&auth), "browser_record_start", &start, &RecordingResolver).is_err());
}

#[test]
fn h9_public_origins_only_when_admin_enabled() {
    let f = fixture(false);
    let web = ["web_fetch_cached", "web_extract"];
    // Default ([isolation] public_web = false): no web authority.
    let auth = f.issue(&web).unwrap();
    assert!(!any_public(&auth));
    for tool in web { assert!(!eligible(&context(&auth), tool), "{tool}"); }

    f.profile(true, "");
    let auth = f.issue(&web).unwrap();
    assert!(has(&auth, &public_any()));
    assert!(auth.grant.resources.as_slice().contains(&ResourceGrant::EmployeeScope { id: EMPLOYEE }));
    let ctx = context(&auth);
    for tool in web { assert!(eligible(&ctx, tool), "{tool}"); }

    // The employee's own denial still wins over the admin switch.
    f.profile(true, r#"denied_tools = ["web_fetch_cached", "web_extract"]"#);
    assert!(!any_public(&f.issue(&web).unwrap()));
    // No web tool in the run's snapshot.
    f.profile(true, "");
    assert!(!any_public(&f.issue(&["tasks_update"]).unwrap()));
    // A task-limited run has no EmployeeScope, so never PublicOrigins (D-H2).
    f.tasks(&[("task-a", LABEL)]);
    let auth = f.issue_task(&web, "task-a").unwrap();
    assert!(!any_public(&auth));
    capability(&auth).canonical_bytes().expect("task-limited record stays signable");
}

#[test]
fn h12bc_hub_install_tied_to_public_web_synthesis_needs_own_employee_only() {
    // Owner decisions: hub install rests on the admin `[isolation] public_web`
    // switch (its PublicOrigins grant); synthesis needs no admin card and no
    // grant beyond the own Employee. Neither is a pending workspace tool.
    let f = fixture(true);
    let names = ["file_read", "web_fetch_cached", "skill_hub_install", "skill_synthesis_run"];
    let auth = f.issue(&names).unwrap();
    assert!(has(&auth, &workspace()) && !any_public(&auth));
    let ctx = context(&auth);
    assert!(!eligible(&ctx, "skill_hub_install"), "no PublicOrigins without public_web");
    assert!(eligible(&ctx, "skill_synthesis_run"));
    f.profile(true, "");
    let auth = f.issue(&names).unwrap();
    assert!(has(&auth, &public_any()));
    for tool in ["skill_hub_install", "skill_synthesis_run"] {
        assert!(signed(&auth).contains(&tool), "{tool}");
        assert!(eligible(&context(&auth), tool), "{tool}");
    }
}

#[test]
fn hub_install_alone_gets_public_origins_when_public_web_on() {
    let f = fixture(false);
    f.profile(true, "");
    let auth = f.issue(&["skill_hub_install"]).unwrap();
    assert!(has(&auth, &public_any()));
    assert!(auth.grant.resources.as_slice().contains(&ResourceGrant::EmployeeScope { id: EMPLOYEE }));
    assert!(eligible(&context(&auth), "skill_hub_install"));
    // The employee's own denial still wins.
    f.profile(true, r#"denied_tools = ["skill_hub_install"]"#);
    assert!(!any_public(&f.issue(&["skill_hub_install"]).unwrap()));
    // A task-limited run never carries PublicOrigins.
    f.profile(true, "");
    f.tasks(&[("task-a", LABEL)]);
    assert!(!any_public(&f.issue_task(&["skill_hub_install"], "task-a").unwrap()));
}

/// Merge HX6 × H12b: a PublicOrigins grant signed only for the hub install
/// (web tools denied by the profile) never signs the web tools; with both
/// selected there is still exactly one grant.
#[test]
fn hub_install_grant_never_signs_denied_web_tools() {
    let f = fixture(false);
    let names = ["skill_hub_install", "web_fetch_cached", "web_extract"];
    f.profile(true, r#"denied_tools = ["web_fetch_cached", "web_extract"]"#);
    let auth = f.issue(&names).unwrap();
    assert!(has(&auth, &public_any()));
    assert_eq!(signed(&auth), ["skill_hub_install"]);
    for tool in ["web_fetch_cached", "web_extract"] { assert!(!eligible(&context(&auth), tool), "{tool}"); }
    assert!(eligible(&context(&auth), "skill_hub_install"));
    f.profile(true, "");
    let auth = f.issue(&names).unwrap();
    assert_eq!(auth.grant.resources.as_slice().iter().filter(|grant| matches!(grant, ResourceGrant::PublicOrigins { .. })).count(), 1);
    assert_eq!(signed(&auth), names);
}

#[test]
fn hub_install_alone_no_grant_when_public_web_off() {
    let f = fixture(false);
    let auth = f.issue(&["skill_hub_install"]).unwrap();
    assert!(!any_public(&auth));
    assert!(!eligible(&context(&auth), "skill_hub_install"));
    f.profile(false, "");
    assert!(!any_public(&f.issue(&["skill_hub_install"]).unwrap()));
}

#[test]
fn h9_task_grant_only_for_owned_record_task() {
    let f = fixture(false);
    f.tasks(&[("task-own", LABEL), ("task-other", "bob")]);
    let auth = f.issue_task(&["create_task", "task_status"], "task-own").unwrap();
    assert_eq!(auth.grant.task.as_ref().map(|task| task.as_str()), Some("task-own"));
    assert!(has(&auth, &task_grant("task-own")));
    assert!(!auth.grant.resources.as_slice().contains(&ResourceGrant::EmployeeScope { id: EMPLOYEE }));
    capability(&auth).canonical_bytes().unwrap();
    assert!(eligible(&context(&auth), "create_task"));
    assert!(!any_new_task(&auth), "a task run plans under its record task, never NewTask");
    // Another employee's task, a missing one or a reassignment before
    // issuance refuse the run outright (`foreign_task_run_refused`).
    f.tasks(&[("task-own", "bob")]);
    assert_eq!(f.issue_task(&["create_task"], "task-own").err(), Some(VerificationError::Revoked));

    // Interactive ingress cannot name a record task.
    assert_eq!(f.employee.authorize_record(&f.employee.snapshot(), Lane::Standard, tools(&["create_task"]),
        turn(None), None, Some("task-own")).err(), Some(VerificationError::Invalid));
    // An interactive run has no task: no Task grant and no task-store read
    // (it may carry the NewTask grant instead, `create_task_interactive_new_task_only`).
    std::fs::remove_file(f.home().join("tasks.db")).unwrap();
    let auth = f.issue(&["create_task"]).unwrap();
    assert!(auth.grant.task.is_none() && !any_task(&auth));
}

#[test]
fn foreign_task_run_refused() {
    let f = fixture(false);
    f.tasks(&[("task-own", LABEL), ("task-other", "bob")]);
    // Reassigned to another employee, or deleted: the issuer refuses the run
    // outright (no task-limited run without the grant) and records why.
    for task in ["task-other", "task-missing"] {
        assert_eq!(f.issue_task(&["create_task", "task_status"], task).err(), Some(VerificationError::Revoked), "{task}");
        let rows = f.audit("isolation_task_run_refused");
        assert!(rows.iter().any(|row| row["details"]["task"] == task
            && row["details"]["reason"] == crate::isolation_background::FOREIGN_TASK_REASON
            && row["details"]["stage"] == "issuance" && row["agent_id"] == LABEL), "{task}: {rows:?}");
    }
    // The owned task still runs, with its grant.
    assert!(has(&f.issue_task(&["create_task"], "task-own").unwrap(), &task_grant("task-own")));
    // The background scheduler refuses the same run before any approval card,
    // with the zh-TW reason the owning scheduler records.
    use crate::isolation_background::{task_run_precheck, FOREIGN_TASK_REFUSAL};
    assert_eq!(task_run_precheck(f.home(), "task-own", LABEL), Ok(()));
    assert_eq!(task_run_precheck(f.home(), "task-other", LABEL), Err(Some(FOREIGN_TASK_REFUSAL)));
    assert!(f.audit("isolation_task_run_refused").iter().any(|row|
        row["details"]["task"] == "task-other" && row["details"]["stage"] == "schedule"));
    // Unreadable ownership fails closed without claiming a reassignment.
    std::fs::remove_file(f.home().join("tasks.db")).unwrap();
    assert_eq!(task_run_precheck(f.home(), "task-own", LABEL), Err(None));
    assert_eq!(f.issue_task(&["create_task"], "task-own").err(), Some(VerificationError::Unavailable));
}

#[test]
fn create_task_interactive_new_task_only() {
    let f = fixture(false);
    let user = f.employee.user().to_owned();
    // An interactive run listing create_task: the issuer signs NewTask for the
    // authenticated human, never a task, and never reads tasks.db.
    let auth = f.issue(&["create_task"]).unwrap();
    assert!(has(&auth, &new_task(&user)), "requester is the run's human principal");
    assert!(auth.grant.task.is_none() && !any_task(&auth));
    assert!(matches!(&auth.grant.principal, duduclaw_isolation::capability::Principal::Human { user_id, .. }
        if user_id.as_str() == user));
    let record = capability(&auth);
    record.canonical_bytes().expect("signable with EmployeeScope");
    let ctx = context(&auth);
    assert!(eligible(&ctx, "create_task"));
    assert_eq!(duduclaw_mcp_boundary::tool_policy::new_task_grant(&ctx).unwrap(), new_task(&user));
    assert!(!f.home().join("tasks.db").exists());
    // Not listed ⇒ not selected; the profile's own denial still wins.
    assert!(!any_new_task(&f.issue(&["tasks_update"]).unwrap()));
    for capabilities in [r#"denied_tools = ["create_task"]"#, r#"allowed_tools = ["tasks_update"]"#] {
        f.profile(false, capabilities);
        let auth = f.issue(&["create_task"]).unwrap();
        assert!(!any_new_task(&auth), "{capabilities}");
        assert!(!eligible(&context(&auth), "create_task"), "{capabilities}");
    }
    f.profile(false, "");
    // A record-backed (task/schedule) run never gets NewTask: it is task
    // limited (no EmployeeScope) and plans under its own record task (H8).
    f.tasks(&[("task-a", LABEL)]);
    let auth = f.issue_task(&["create_task"], "task-a").unwrap();
    assert!(!any_new_task(&auth) && has(&auth, &task_grant("task-a")));
    // An unreadable profile fails the issuance closed, as for the other selectors.
    std::fs::remove_file(f.profile_path()).unwrap();
    assert_eq!(f.issue(&["create_task"]).err(), Some(VerificationError::Unavailable));
}

#[test]
fn public_web_switch_independent_of_network_access() {
    let f = fixture(false);
    let web = ["web_fetch_cached", "web_extract"];
    // The task-sandbox network switch alone grants no protected public web…
    f.profile_with(true, false, "");
    let auth = f.issue(&web).unwrap();
    assert!(!any_public(&auth));
    for tool in web { assert!(!eligible(&context(&auth), tool), "{tool}"); }
    // …and the protected switch works with the sandbox network off.
    f.profile_with(false, true, "");
    let auth = f.issue(&web).unwrap();
    assert!(has(&auth, &public_any()));
    for tool in web { assert!(eligible(&context(&auth), tool), "{tool}"); }
    f.profile_with(true, true, "");
    assert!(has(&f.issue(&web).unwrap(), &public_any()));
    // Turning only network_access off mid-issuance changes no selector input;
    // turning public_web off does (the B2 re-read refuses the run).
    let sandbox_only = during(&f, Box::new(|| f.profile_with(false, true, "")));
    assert!(has(&f.issue_with(&sandbox_only, &web, None).unwrap(), &public_any()));
    let switch_off = during(&f, Box::new(|| f.profile_with(false, false, "")));
    assert_eq!(f.issue_with(&switch_off, &web, None).err(), Some(VerificationError::Revoked));
}

#[test]
fn h9_selector_store_unreadable_fails_closed() {
    let f = fixture(true);
    let unavailable = Some(VerificationError::Unavailable);
    let elsewhere = f.home().join("elsewhere.toml");
    std::fs::write(&elsewhere, test_support::agent_toml(LABEL, true, "")).unwrap();
    for case in ["malformed", "symlink", "hard link", "missing"] {
        let path = f.profile_path();
        std::fs::remove_file(&path).unwrap();
        match case {
            "malformed" => std::fs::write(&path, "[agent\nname = ").unwrap(),
            "symlink" => std::os::unix::fs::symlink(&elsewhere, &path).unwrap(),
            "hard link" => std::fs::hard_link(&elsewhere, &path).unwrap(),
            _ => {}
        }
        for names in [&["web_fetch_cached"][..], &["file_read"][..]] {
            assert_eq!(f.issue(names).err(), unavailable, "{case}: {names:?}");
        }
        // A run that needs no selector input reads nothing and is unaffected.
        assert!(f.issue(&["tasks_update"]).is_ok(), "{case}");
        let _ = std::fs::remove_file(&path);
        f.profile(false, "");
    }

    // Task store: missing, writable by others, or without the tasks table.
    assert_eq!(f.issue_task(&["create_task"], "task-a").err(), unavailable, "missing tasks.db");
    f.tasks(&[("task-a", LABEL)]);
    let store = f.home().join("tasks.db");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o666)).unwrap();
    assert_eq!(f.issue_task(&["create_task"], "task-a").err(), unavailable, "unsafe tasks.db");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(f.issue_task(&["create_task"], "task-a").is_ok());
    rusqlite::Connection::open(&store).unwrap().execute_batch("DROP TABLE tasks").unwrap();
    assert_eq!(f.issue_task(&["create_task"], "task-a").err(), unavailable, "tasks table missing");
}

#[test]
fn h10_issuer_mac_covers_selected_grants() {
    let f = fixture(true);
    f.profile(true, "");
    let auth = f.issue(&["file_read", "web_fetch_cached"]).unwrap();
    let record = capability(&auth);
    let signed = record.canonical_bytes().unwrap();
    let with = |resources: Vec<ResourceGrant>| {
        let mut changed = record.clone();
        changed.resources = List::new(resources).unwrap();
        changed.canonical_bytes().unwrap()
    };
    for grant in [workspace(), public_any()] {
        // A plain member of the signed resource set, not a Confined wrapper.
        assert!(record.resources.as_slice().contains(&grant), "{grant:?}");
        duduclaw_mcp_boundary::tool_policy::validate_resource(&grant).unwrap();
        let without: Vec<_> = record.resources.as_slice().iter().filter(|other| *other != &grant).cloned().collect();
        assert_ne!(with(without), signed, "{grant:?} must be MAC-covered");
    }
    // Neither another workspace handle nor a different origin set verifies
    // under the MAC of the issued bytes.
    let swap = |from: &ResourceGrant, to: ResourceGrant| record.resources.as_slice().iter()
        .map(|grant| if grant == from { to.clone() } else { grant.clone() }).collect::<Vec<_>>();
    assert_ne!(with(swap(&workspace(), ResourceGrant::Workspace { handle: Text::new("other-root".into()).unwrap() })), signed);
    assert_ne!(with(swap(&public_any(), ResourceGrant::PublicOrigins {
        origins: List::new(vec![PublicOrigin::parse("https://example.org").unwrap()]).unwrap() })), signed);

    f.tasks(&[("task-a", LABEL)]);
    let auth = f.issue_task(&["create_task", "file_read"], "task-a").unwrap();
    let record = capability(&auth);
    let signed = record.canonical_bytes().unwrap();
    assert!(record.resources.as_slice().contains(&task_grant("task-a")));
    assert!(record.resources.as_slice().contains(&workspace()), "Workspace survives task narrowing");
    let mut without = record.clone();
    without.resources = List::new(record.resources.as_slice().iter()
        .filter(|grant| **grant != task_grant("task-a")).cloned().collect()).unwrap();
    assert_ne!(without.canonical_bytes().unwrap(), signed, "the record Task grant must be MAC-covered");
    let mut other = record.clone();
    other.task = Some(Text::new("task-b".into()).unwrap());
    assert_ne!(other.canonical_bytes().unwrap(), signed);
}

/// Deterministically change selector inputs at the post-I/O lifecycle recheck
/// (the issuer's second root ReadBack).
struct DuringIssue<'a> {
    root: &'a dyn RootAllocation,
    reads: Cell<usize>,
    change: Box<dyn Fn() + 'a>,
}
impl RootAllocation for DuringIssue<'_> {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError> {
        self.reads.set(self.reads.get() + 1);
        if self.reads.get() == 2 { (self.change)(); }
        self.root.read_back(item)
    }
}
fn during<'a>(f: &'a Fixture, change: Box<dyn Fn() + 'a>) -> DuringIssue<'a> {
    DuringIssue { root: f.employee.root(), reads: Cell::new(0), change }
}

#[test]
fn h10_reread_after_io() {
    let f = fixture(true);
    f.profile(true, "");
    f.tasks(&[("task-a", LABEL)]);
    let web = ["web_fetch_cached"];

    // Unchanged inputs: issued, and the recheck really ran in between.
    let quiet = during(&f, Box::new(|| {}));
    assert!(has(&f.issue_with(&quiet, &web, None).unwrap(), &public_any()));
    assert_eq!(quiet.reads.get(), 2);

    // The admin turns public web off while the issuer is doing its I/O.
    let off = during(&f, Box::new(|| f.profile(false, "")));
    assert_eq!(f.issue_with(&off, &web, None).err(), Some(VerificationError::Revoked));
    assert_eq!(off.reads.get(), 2);
    // ...or turns it on: a grant that was not selected is not silently added.
    let on = during(&f, Box::new(|| f.profile(true, "")));
    assert_eq!(f.issue_with(&on, &web, None).err(), Some(VerificationError::Revoked));

    // The profile stops allowing the file tools.
    f.profile(true, "");
    let denied = during(&f, Box::new(|| f.profile(true, r#"denied_tools = ["file_read"]"#)));
    assert_eq!(f.issue_with(&denied, &["file_read"], None).err(), Some(VerificationError::Revoked));

    // The task is reassigned to another agent mid-issuance.
    f.profile(true, "");
    let reassigned = during(&f, Box::new(|| f.tasks(&[("task-a", "bob")])));
    assert_eq!(f.issue_with(&reassigned, &["create_task"], Some("task-a")).err(), Some(VerificationError::Revoked));
    f.tasks(&[("task-a", LABEL)]);

    // The profile becomes unreadable mid-issuance: fail closed, not Revoked
    // (the lifecycle itself is still live).
    let removed = during(&f, Box::new(|| std::fs::remove_file(f.profile_path()).unwrap()));
    assert_eq!(f.issue_with(&removed, &web, None).err(), Some(VerificationError::Unavailable));
}
