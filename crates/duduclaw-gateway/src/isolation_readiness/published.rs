//! R8m 30b: the gateway's own readiness view, published for the root CLI
//! (`duduclaw service readiness --protected-linux`), so the command line and
//! the gateway agree on whether employees can run.
//!
//! The gateway writes `/run/duduclaw-gateway-update/isolation-readiness.json`:
//! that directory is the isolated gateway unit's existing `RuntimeDirectory=`
//! (owned by the gateway account, 0750, under root-owned `/run`; systemd
//! removes it when the unit stops), so no new systemd directive is needed and
//! root can read it. The document holds fixed identifiers only (component
//! names, state names, reason names, assembly producer names, the kernel boot
//! id and a timestamp): no employee id, label, path or secret. It is rewritten
//! atomically (temp file + fsync + rename, `fs_safe::SafeDir`) when its
//! content changes and at least every [`REWRITE_SECS`]; nothing is removed on
//! exit (the reader rejects a stale or other-boot document).
//!
//! [`parse`] is the reader's strict check, shared so the schema cannot drift;
//! the CLI adds the file checks (no-follow open, owner, size).
// The writer runs on Linux only; elsewhere its helpers are exercised by tests.
#![cfg_attr(not(target_os = "linux"), allow(dead_code))]
use serde::{Deserialize, Serialize};
use super::{ComponentReasons, ComponentState, Components, Readiness};

pub const SCHEMA: u32 = 1;
/// The isolated gateway unit's `RuntimeDirectory=` under `/run`.
pub const DIRECTORY: &str = "duduclaw-gateway-update";
pub const FILE: &str = "isolation-readiness.json";
/// The reader refuses anything larger.
pub const MAX_BYTES: u64 = 64 * 1024;
/// The reader refuses a document older than this.
pub const MAX_AGE_SECS: u64 = 90;
/// The writer rewrites an unchanged document at least this often.
pub const REWRITE_SECS: u64 = 30;
/// Tolerated clock skew for a timestamp in the future.
const FUTURE_SKEW_SECS: u64 = 5;

/// Component names (the `Components` field names, in this order).
pub const COMPONENTS: [&str; 8] = ["launch_service", "sandbox", "network", "storage", "parse_worker",
    "update_service", "mcp_boundary", "runtime_adapters"];
pub const STATES: [&str; 3] = ["ready", "not_ready", "pending"];
/// A not-ready component's reason: its `ComponentState` serde name.
pub const COMPONENT_REASONS: [&str; 2] = ["unavailable", "pending_integration"];
/// R8n-1: the ParseWorker probe's fixed refusal reasons (a not-ready
/// `parse_worker` may carry one instead of `unavailable`).
/// R8n-1 review F2: `observation_pending` (the background observer has not
/// stored a readback yet) and `observation_stale` (its last one is older
/// than the 2 s window) in both tables.
pub const PARSE_WORKER_REASONS: [&str; 9] = ["assembly_incomplete", "launcher_not_installed",
    "generation_mismatch", "socket_unavailable", "socket_unsafe", "socket_not_listening",
    "retirement_lease_unavailable", "observation_pending", "observation_stale"];
/// R8n-1: the RuntimeAdapters probe's fixed refusal reasons (`runtime_adapters`).
pub const RUNTIME_ADAPTER_REASONS: [&str; 9] = ["assembly_incomplete", "helper_producer_missing",
    "fork_reader_missing", "generation_mismatch", "publication_unavailable", "no_main_runtime",
    "main_adapter_unavailable", "observation_pending", "observation_stale"];
/// R8n-1: the helper tool family names (`isolation_execution::HelperFamily::name`).
/// A listed family is not offered on this host: its tools are neither listed
/// nor callable for isolated employees. Informational only.
pub const TOOL_FAMILIES: [&str; 4] = ["program", "office", "code_map", "browser_record"];

/// The probe reasons a not-ready component may carry, by component name.
fn probe_reasons(component: &str) -> &'static [&'static str] {
    match component {
        "parse_worker" => &PARSE_WORKER_REASONS,
        "runtime_adapters" => &RUNTIME_ADAPTER_REASONS,
        _ => &[],
    }
}
/// The boot assembly's producer names (`isolation_mcp::MissingProducer::name`).
pub const ASSEMBLY: [&str; 13] = ["backend", "live_authority", "durable_bindings", "runtime_publication",
    "execution_plans", "worker_retirement", "worker_plans", "turn_authority", "dispatcher",
    "worker_integration", "main_plans", "connector_providers", "initialization"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct View {
    pub schema: u32,
    pub boot_id: String,
    pub written_unix: u64,
    pub ready: bool,
    pub components: Vec<ComponentView>,
    pub assembly_missing: Vec<String>,
    /// R8n-1: [`TOOL_FAMILIES`] not offered on this host (empty until the
    /// boot assembly completed). Required: a document without it is rejected
    /// (reads as "gateway view unavailable"); gateway and CLI ship together.
    pub tools_unavailable: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentView {
    pub name: String,
    pub state: String,
    pub reason: Option<String>,
}

/// The content that decides a rewrite (everything but the timestamp).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Content {
    ready: bool,
    components: [(&'static str, ComponentState, Option<&'static str>); 8],
    assembly_missing: Vec<&'static str>,
    tools_unavailable: Vec<&'static str>,
}

impl Content {
    pub(crate) fn of(readiness: &Readiness, reasons: ComponentReasons, assembly_missing: Vec<&'static str>,
        tools_unavailable: Vec<&'static str>) -> Self {
        let Components { launch_service, sandbox, network, storage, parse_worker, update_service,
            mcp_boundary, runtime_adapters } = readiness.components;
        let states = [launch_service, sandbox, network, storage, parse_worker, update_service, mcp_boundary,
            runtime_adapters];
        let mut components = [("", ComponentState::Unavailable, None); 8];
        for (slot, (name, state)) in components.iter_mut().zip(COMPONENTS.into_iter().zip(states)) {
            // Only a fixed probe reason of this very component, and only while unavailable.
            let reason = match name { "parse_worker" => reasons.parse_worker, "runtime_adapters" => reasons.runtime_adapters,
                _ => None }.filter(|reason| state == ComponentState::Unavailable && probe_reasons(name).contains(reason));
            *slot = (name, state, reason);
        }
        // Fixed family names only, each once, in table order.
        let tools_unavailable = TOOL_FAMILIES.into_iter().filter(|family| tools_unavailable.contains(family)).collect();
        Self { ready: readiness.ready, components, assembly_missing, tools_unavailable }
    }

    pub(crate) fn view(&self, boot_id: &str, written_unix: u64) -> View {
        View { schema: SCHEMA, boot_id: boot_id.to_owned(), written_unix, ready: self.ready,
            components: self.components.iter().map(|(name, state, probe_reason)| {
                let (state, reason) = state_and_reason(*state, *probe_reason);
                ComponentView { name: (*name).to_owned(), state: state.to_owned(), reason: reason.map(str::to_owned) }
            }).collect(),
            assembly_missing: self.assembly_missing.iter().map(|name| (*name).to_owned()).collect(),
            tools_unavailable: self.tools_unavailable.iter().map(|name| (*name).to_owned()).collect() }
    }
}

/// A component's published state name and fixed reason name.
fn state_and_reason(state: ComponentState, probe_reason: Option<&'static str>) -> (&'static str, Option<&'static str>) {
    match state {
        ComponentState::Ready => ("ready", None),
        ComponentState::Unavailable => ("not_ready", Some(probe_reason.unwrap_or("unavailable"))),
        ComponentState::PendingIntegration => ("pending", Some("pending_integration")),
    }
}

/// R8n-1 review F3: each not-ready component's fixed reason name, exactly as
/// [`Content::view`] publishes it (same filtering of probe reasons, same
/// tables), for `system.status`'s `isolation_readiness.component_reasons`.
pub(crate) fn component_reasons(readiness: &Readiness, reasons: ComponentReasons)
    -> std::collections::BTreeMap<&'static str, &'static str> {
    Content::of(readiness, reasons, Vec::new(), Vec::new()).components.iter()
        .filter_map(|(name, state, probe_reason)| state_and_reason(*state, *probe_reason).1.map(|reason| (*name, reason)))
        .collect()
}

/// `true` when the document must be (re)written: never written, content
/// changed, or the last write is [`REWRITE_SECS`] old.
pub(crate) fn due(last: Option<&(Content, std::time::Instant)>, content: &Content, now: std::time::Instant) -> bool {
    last.is_none_or(|(written, at)| written != content
        || now.checked_duration_since(*at).is_none_or(|age| age >= std::time::Duration::from_secs(REWRITE_SECS)))
}

/// A boot id as `/proc/sys/kernel/random/boot_id` gives it.
pub fn valid_boot_id(text: &str) -> bool {
    text.len() == 36 && text.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte),
    })
}

/// This boot's id, or `None` when it cannot be read (never published then).
pub fn current_boot_id() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let mut text = String::new();
        std::fs::File::open("/proc/sys/kernel/random/boot_id").ok()?.take(64).read_to_string(&mut text).ok()?;
        let text = text.trim();
        valid_boot_id(text).then(|| text.to_owned())
    }
    #[cfg(not(target_os = "linux"))]
    { None }
}

/// Why a published document was not accepted. Every variant reads as
/// "gateway view unavailable".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejected {
    /// Oversized, not the strict schema, unknown field, unknown identifier,
    /// duplicate or contradictory content.
    Malformed,
    /// Written during another boot.
    OtherBoot,
    /// Older than [`MAX_AGE_SECS`], or too far in the future.
    Stale,
}

/// The reader's strict check of an untrusted document: size, strict schema
/// (`deny_unknown_fields`), fixed identifiers only (which bounds every
/// string), each component exactly once, consistent states, this boot, fresh.
pub fn parse(bytes: &[u8], boot_id: &str, now_unix: u64) -> Result<View, Rejected> {
    if bytes.len() as u64 > MAX_BYTES { return Err(Rejected::Malformed); }
    let view: View = serde_json::from_slice(bytes).map_err(|_| Rejected::Malformed)?;
    if view.schema != SCHEMA || !valid_boot_id(&view.boot_id) { return Err(Rejected::Malformed); }
    if view.components.len() != COMPONENTS.len() || view.assembly_missing.len() > ASSEMBLY.len()
        || view.tools_unavailable.len() > TOOL_FAMILIES.len() {
        return Err(Rejected::Malformed);
    }
    for (expected, component) in COMPONENTS.iter().zip(&view.components) {
        let consistent = match (component.state.as_str(), component.reason.as_deref()) {
            ("ready", None) => true,
            ("not_ready", Some("unavailable")) | ("pending", Some("pending_integration")) => true,
            // R8n-1: a probe-backed component's own fixed reason.
            ("not_ready", Some(reason)) => probe_reasons(expected).contains(&reason),
            _ => false,
        };
        if component.name != *expected || !consistent { return Err(Rejected::Malformed); }
    }
    for (names, table) in [(&view.assembly_missing, &ASSEMBLY[..]), (&view.tools_unavailable, &TOOL_FAMILIES[..])] {
        let mut seen = Vec::new();
        for name in names {
            if !table.contains(&name.as_str()) || seen.contains(&name) { return Err(Rejected::Malformed); }
            seen.push(name);
        }
    }
    let all_ready = view.components.iter().all(|component| component.state == "ready");
    if view.ready && (!all_ready || !view.assembly_missing.is_empty()) { return Err(Rejected::Malformed); }
    if view.boot_id != boot_id { return Err(Rejected::OtherBoot); }
    if view.written_unix > now_unix.saturating_add(FUTURE_SKEW_SECS)
        || now_unix.saturating_sub(view.written_unix) > MAX_AGE_SECS {
        return Err(Rejected::Stale);
    }
    Ok(view)
}

/// The boot assembly's missing producers by fixed name (Linux only; empty
/// elsewhere, where there is no assembly).
pub(crate) fn assembly_missing() -> Vec<&'static str> {
    #[cfg(target_os = "linux")]
    { crate::isolation_mcp::boot_status().missing.into_iter().map(|producer| producer.name()).collect() }
    #[cfg(not(target_os = "linux"))]
    { Vec::new() }
}

/// R8n-1: the helper tool families the assembled tool service does not offer
/// (Linux only; empty until assembled and elsewhere).
pub(crate) fn tools_unavailable() -> Vec<&'static str> {
    #[cfg(target_os = "linux")]
    { crate::isolation_mcp::tools_unavailable() }
    #[cfg(not(target_os = "linux"))]
    { Vec::new() }
}

/// Replace [`FILE`] in `directory` atomically (temp + fsync + rename).
#[cfg(unix)]
pub(crate) fn write_into(directory: &crate::fs_safe::SafeDir, view: &View) -> std::io::Result<()> {
    let text = serde_json::to_string(view).map_err(std::io::Error::other)?;
    directory.write_file_atomic(FILE, &text)
}

/// R8m 30b: the publisher. Only while isolation is required (protected
/// Linux TEAM) and only on Linux. Starts ONE tokio task
/// (`duduclaw_gateway::isolation_readiness::published::spawn`), which
/// computes the view every 5 s on the blocking pool, writes it when it
/// changed or every [`REWRITE_SECS`], and ends once the gateway's shutdown
/// gate (`handlers::migration_shutdown_gate`) has closed.
pub fn spawn() {
    #[cfg(target_os = "linux")]
    linux::spawn();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{path::Path, sync::atomic::{AtomicBool, Ordering}, time::{Duration, Instant}};
    use super::{Content, DIRECTORY};

    const TICK: Duration = Duration::from_secs(5);
    const POLL: Duration = Duration::from_millis(250);
    static FAILING: AtomicBool = AtomicBool::new(false);

    fn closed() -> bool {
        use duduclaw_migration::confirmation::EffectGate;
        crate::handlers::migration_shutdown_gate().is_closed()
    }

    pub(super) fn spawn() {
        if super::super::status().is_none() { return; }
        drop(tokio::spawn(async move {
            let mut last: Option<(Content, Instant)> = None;
            loop {
                if closed() { return; }
                let previous = last.take();
                last = tokio::task::spawn_blocking(move || publish_once(previous)).await.unwrap_or(None);
                let until = Instant::now() + TICK;
                while Instant::now() < until {
                    if closed() { return; }
                    tokio::time::sleep(POLL).await;
                }
            }
        }));
    }

    /// One tick: the last written content, updated when a write landed.
    fn publish_once(last: Option<(Content, Instant)>) -> Option<(Content, Instant)> {
        let Some((readiness, reasons)) = super::super::status_with_reasons() else { return last };
        let content = Content::of(&readiness, reasons, super::assembly_missing(), super::tools_unavailable());
        let now = Instant::now();
        if !super::due(last.as_ref(), &content, now) { return last; }
        let Some(boot_id) = super::current_boot_id() else { return last };
        let written_unix = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs()).unwrap_or(0);
        match runtime_directory().and_then(|directory| super::write_into(&directory, &content.view(&boot_id, written_unix))) {
            Ok(()) => {
                FAILING.store(false, Ordering::Release);
                Some((content, now))
            }
            Err(_) => {
                if !FAILING.swap(true, Ordering::AcqRel) {
                    tracing::warn!("Gateway 的隔離就緒狀態檔無法寫入；命令列的就緒檢查將回報無法取得 Gateway 狀態。");
                }
                last
            }
        }
    }

    /// `/run` root-owned and not group/other-writable; the leaf the unit's
    /// `RuntimeDirectory=`: owned by this (non-root) account, no group write,
    /// no other access (the same rule as the update read-back socket).
    fn runtime_directory() -> std::io::Result<crate::fs_safe::SafeDir> {
        use std::os::unix::fs::MetadataExt;
        let refused = || crate::fs_safe::refused("gateway runtime directory refused");
        let run = crate::fs_safe::SafeDir::open_root(Path::new("/run"))?;
        let meta = run.metadata()?;
        if meta.uid() != 0 || meta.mode() & 0o022 != 0 { return Err(refused()); }
        let leaf = run.child_dir(DIRECTORY, false)?.ok_or_else(refused)?;
        let meta = leaf.metadata()?;
        let (uid, gid) = (nix::unistd::geteuid().as_raw(), nix::unistd::getegid().as_raw());
        if uid == 0 || meta.uid() != uid || meta.gid() != gid || meta.mode() & 0o027 != 0 { return Err(refused()); }
        Ok(leaf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    const BOOT: &str = "0123abcd-4567-89ef-0123-456789abcdef";
    const NOW: u64 = 1_800_000_000;

    fn readiness(state: ComponentState) -> Readiness {
        let components = Components { launch_service: ComponentState::Ready, sandbox: ComponentState::Ready,
            network: ComponentState::Ready, storage: ComponentState::Ready, parse_worker: ComponentState::Ready,
            update_service: ComponentState::Ready, mcp_boundary: state, runtime_adapters: ComponentState::Ready };
        Readiness { required: true, ready: components.all_ready(), components, status: None, reasons: None,
            employees: None, notices: None, assembly_missing: None, tools_unavailable: None, component_reasons: None }
    }
    fn bytes(view: &View) -> Vec<u8> { serde_json::to_vec(view).unwrap() }
    fn content(readiness: &Readiness, assembly_missing: Vec<&'static str>) -> Content {
        Content::of(readiness, ComponentReasons::default(), assembly_missing, Vec::new())
    }

    /// R8n-1 1.6/2.3: probe reasons and the unavailable tool families round-trip
    /// through the strict reader; only a component's own fixed reason, only
    /// while it is not ready, and only fixed family names are accepted.
    #[test]
    fn r8n_published_view_round_trips_probe_reasons_and_unavailable_tools() {
        let mut readiness = readiness(ComponentState::Ready);
        readiness.components.parse_worker = ComponentState::Unavailable;
        readiness.components.runtime_adapters = ComponentState::Unavailable;
        readiness.ready = false;
        let reasons = ComponentReasons { parse_worker: Some("socket_unavailable"), runtime_adapters: Some("no_main_runtime") };
        let view = Content::of(&readiness, reasons, Vec::new(), vec!["office", "program"]).view(BOOT, NOW);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["components"][4], serde_json::json!({"name": "parse_worker", "state": "not_ready",
            "reason": "socket_unavailable"}));
        assert_eq!(value["components"][7], serde_json::json!({"name": "runtime_adapters", "state": "not_ready",
            "reason": "no_main_runtime"}));
        // Table order, whatever order they were given in.
        assert_eq!(value["tools_unavailable"], serde_json::json!(["program", "office"]));
        assert_eq!(parse(&bytes(&view), BOOT, NOW).unwrap(), view);
        // A ready host can still not offer every family: informational only.
        let ready = Content::of(&readiness_all_ready(), ComponentReasons::default(), Vec::new(), TOOL_FAMILIES.to_vec())
            .view(BOOT, NOW);
        assert!(ready.ready);
        assert_eq!(parse(&bytes(&ready), BOOT, NOW).unwrap().tools_unavailable, TOOL_FAMILIES);
        // A reason of another component, an unknown name, or a reason on a
        // ready component is never emitted.
        let crossed = ComponentReasons { parse_worker: Some("no_main_runtime"), runtime_adapters: Some("/etc/passwd") };
        let view = Content::of(&readiness, crossed, Vec::new(), vec!["shell"]).view(BOOT, NOW);
        assert_eq!(view.components[4].reason.as_deref(), Some("unavailable"));
        assert_eq!(view.components[7].reason.as_deref(), Some("unavailable"));
        assert!(view.tools_unavailable.is_empty());
        let view = Content::of(&readiness_all_ready(), reasons, Vec::new(), Vec::new()).view(BOOT, NOW);
        assert!(view.components.iter().all(|component| component.reason.is_none()));
        // The reader refuses the same contradictions in a forged document.
        let good = Content::of(&readiness, reasons, Vec::new(), vec!["code_map"]).view(BOOT, NOW);
        let cases: [fn(&mut View); 6] = [
            |view| view.components[4].reason = Some("no_main_runtime".into()),
            |view| view.components[7].reason = Some("socket_unavailable".into()),
            |view| view.components[0].reason = Some("socket_unavailable".into()),
            |view| view.tools_unavailable.push("shell".into()),
            |view| view.tools_unavailable.push("code_map".into()),
            |view| view.components[4].state = "pending".into(),
        ];
        for (index, case) in cases.into_iter().enumerate() {
            let mut view = good.clone();
            case(&mut view);
            assert_eq!(parse(&bytes(&view), BOOT, NOW), Err(Rejected::Malformed), "case {index}");
        }
        // A document without the field (an older writer) reads as unavailable.
        let mut value = serde_json::to_value(&good).unwrap();
        value.as_object_mut().unwrap().remove("tools_unavailable");
        assert_eq!(parse(value.to_string().as_bytes(), BOOT, NOW), Err(Rejected::Malformed));
        for reason in PARSE_WORKER_REASONS.into_iter().chain(RUNTIME_ADAPTER_REASONS) {
            assert!(!reason.is_empty() && reason.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_'), "{reason}");
        }
    }
    fn readiness_all_ready() -> Readiness { readiness(ComponentState::Ready) }

    /// R8n-1 review F3: `system.status`'s `component_reasons` is the published
    /// view's reason of every not-ready component (probe reason only from its
    /// own table), and nothing for a ready one.
    #[test]
    fn r8n_status_component_reasons_match_the_published_view() {
        let mut readiness = readiness(ComponentState::PendingIntegration);
        readiness.components.parse_worker = ComponentState::Unavailable;
        readiness.components.runtime_adapters = ComponentState::Unavailable;
        readiness.components.storage = ComponentState::Unavailable;
        readiness.ready = false;
        for reasons in [ComponentReasons { parse_worker: Some("observation_stale"), runtime_adapters: Some("no_main_runtime") },
            ComponentReasons { parse_worker: Some("no_main_runtime"), runtime_adapters: None }] {
            let map = component_reasons(&readiness, reasons);
            let view = Content::of(&readiness, reasons, Vec::new(), Vec::new()).view(BOOT, NOW);
            let published: std::collections::BTreeMap<&str, &str> = view.components.iter()
                .filter_map(|component| component.reason.as_deref().map(|reason| (component.name.as_str(), reason))).collect();
            assert_eq!(map, published);
            assert_eq!(map.len(), 4, "storage, parse_worker, mcp_boundary, runtime_adapters");
            assert_eq!(map["mcp_boundary"], "pending_integration");
            assert_eq!(map["storage"], "unavailable");
        }
        let map = component_reasons(&readiness, ComponentReasons { parse_worker: Some("observation_stale"), runtime_adapters: None });
        assert_eq!((map["parse_worker"], map["runtime_adapters"]), ("observation_stale", "unavailable"));
        assert!(component_reasons(&readiness_all_ready(), ComponentReasons::default()).is_empty());
        let mut status = readiness.clone();
        status.component_reasons = Some(map);
        let value = serde_json::to_value(&status).unwrap();
        assert_eq!(value["component_reasons"]["parse_worker"], "observation_stale");
        assert_eq!(value["components"].as_object().unwrap().len(), 8, "the component map's shape is unchanged");
    }

    /// The family names are exactly `HelperFamily::name`, in order.
    #[cfg(target_os = "linux")]
    #[test]
    fn published_tool_families_match_the_helper_families() {
        let names: Vec<_> = crate::isolation_execution::HelperFamily::ALL.iter().map(|family| family.name()).collect();
        assert_eq!(names, TOOL_FAMILIES);
    }

    /// Every emitted name/state/reason is in the fixed tables, and the
    /// document has exactly the schema's keys.
    #[test]
    fn published_view_has_fixed_identifiers_only() {
        for state in [ComponentState::Ready, ComponentState::Unavailable, ComponentState::PendingIntegration] {
            let view = content(&readiness(state), ASSEMBLY.to_vec()).view(BOOT, NOW);
            for component in &view.components {
                assert!(COMPONENTS.contains(&component.name.as_str()));
                assert!(STATES.contains(&component.state.as_str()));
                assert!(component.reason.as_deref().is_none_or(|reason| COMPONENT_REASONS.contains(&reason)));
            }
            assert!(view.assembly_missing.iter().all(|name| ASSEMBLY.contains(&name.as_str())));
            let value: serde_json::Value = serde_json::from_slice(&bytes(&view)).unwrap();
            let keys: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
            assert_eq!(keys.len(), 7, "{keys:?}");
            for key in ["schema", "boot_id", "written_unix", "ready", "components", "assembly_missing", "tools_unavailable"] {
                assert!(value.get(key).is_some(), "{key}");
            }
        }
        // The component reasons are exactly `ComponentState`'s not-ready serde names.
        assert_eq!([ComponentState::Unavailable, ComponentState::PendingIntegration]
            .map(|state| serde_json::to_value(state).unwrap()), COMPONENT_REASONS.map(|reason| serde_json::json!(reason)));
        let view = content(&readiness(ComponentState::PendingIntegration), vec!["connector_providers"]).view(BOOT, NOW);
        let value = serde_json::to_value(&view).unwrap();
        assert_eq!(value["ready"], false);
        assert_eq!(value["components"][6], serde_json::json!({"name": "mcp_boundary", "state": "pending",
            "reason": "pending_integration"}));
        assert_eq!(value["components"][0], serde_json::json!({"name": "launch_service", "state": "ready", "reason": null}));
        assert_eq!(value["assembly_missing"], serde_json::json!(["connector_providers"]));
    }

    /// The assembly's producer names are exactly `MissingProducer::name`.
    #[cfg(target_os = "linux")]
    #[test]
    fn published_assembly_names_match_the_boot_producers() {
        let names: Vec<_> = crate::isolation_mcp::MissingProducer::ALL.iter().map(|producer| producer.name()).collect();
        assert_eq!(names, ASSEMBLY);
        for producer in crate::isolation_mcp::MissingProducer::ALL {
            assert_eq!(serde_json::to_value(producer).unwrap(), serde_json::json!(producer.name()));
        }
    }

    #[test]
    fn published_view_is_rewritten_on_change_and_every_thirty_seconds() {
        let start = Instant::now();
        let ready = content(&readiness(ComponentState::Ready), Vec::new());
        let pending = content(&readiness(ComponentState::PendingIntegration), vec!["connector_providers"]);
        assert!(due(None, &ready, start), "never written");
        let last = (ready.clone(), start);
        assert!(!due(Some(&last), &ready, start + Duration::from_secs(29)));
        assert!(due(Some(&last), &ready, start + Duration::from_secs(30)), "periodic rewrite");
        assert!(due(Some(&last), &pending, start + Duration::from_secs(1)), "content changed");
        let other = content(&readiness(ComponentState::PendingIntegration), vec!["dispatcher"]);
        assert!(due(Some(&(pending, start)), &other, start), "assembly changed");
    }

    #[cfg(unix)]
    #[test]
    fn published_view_write_replaces_atomically_and_reads_back() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let directory = crate::fs_safe::SafeDir::open_nofollow(home.path(), false).unwrap();
        let first = content(&readiness(ComponentState::Unavailable), vec!["backend"]).view(BOOT, NOW);
        write_into(&directory, &first).unwrap();
        let second = content(&readiness(ComponentState::Ready), Vec::new()).view(BOOT, NOW + 30);
        write_into(&directory, &second).unwrap();
        let read = std::fs::read(home.path().join(FILE)).unwrap();
        assert_eq!(parse(&read, BOOT, NOW + 31).unwrap(), second);
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 1);
    }

    #[test]
    fn published_view_reader_rejects_everything_but_a_fresh_strict_document() {
        let good = content(&readiness(ComponentState::PendingIntegration), vec!["connector_providers"]).view(BOOT, NOW);
        assert_eq!(parse(&bytes(&good), BOOT, NOW + 10).unwrap(), good);
        // Other boot, stale, future.
        assert_eq!(parse(&bytes(&good), "fedcba98-7654-3210-fedc-ba9876543210", NOW), Err(Rejected::OtherBoot));
        assert_eq!(parse(&bytes(&good), BOOT, NOW + MAX_AGE_SECS + 1), Err(Rejected::Stale));
        assert!(parse(&bytes(&good), BOOT, NOW + MAX_AGE_SECS).is_ok());
        assert_eq!(parse(&bytes(&good), BOOT, NOW - 60), Err(Rejected::Stale));
        // Unknown fields, at the top and in a component.
        let mut value = serde_json::to_value(&good).unwrap();
        value["extra"] = serde_json::json!(1);
        assert_eq!(parse(value.to_string().as_bytes(), BOOT, NOW), Err(Rejected::Malformed));
        let mut value = serde_json::to_value(&good).unwrap();
        value["components"][0]["label"] = serde_json::json!("alice");
        assert_eq!(parse(value.to_string().as_bytes(), BOOT, NOW), Err(Rejected::Malformed));
        // Oversized.
        let mut padded = bytes(&good);
        padded.extend(vec![b' '; MAX_BYTES as usize]);
        assert_eq!(parse(&padded, BOOT, NOW), Err(Rejected::Malformed));
        // Unknown identifiers, long strings, duplicates, wrong schema, contradictions.
        let cases: [fn(&mut View); 9] = [
            |view| view.schema = 2,
            |view| view.components[0].name = "x".repeat(10_000),
            |view| view.components[1].state = "fine".into(),
            |view| view.components[2].reason = Some("/var/lib/duduclaw".into()),
            |view| view.assembly_missing.push("employee-7".into()),
            |view| view.assembly_missing.push("connector_providers".into()),
            |view| { view.components.pop(); },
            |view| view.ready = true,
            |view| view.boot_id = "not-a-boot-id".into(),
        ];
        for (index, case) in cases.into_iter().enumerate() {
            let mut view = good.clone();
            case(&mut view);
            assert_eq!(parse(&bytes(&view), BOOT, NOW), Err(Rejected::Malformed), "case {index}");
        }
        assert_eq!(parse(b"not json", BOOT, NOW), Err(Rejected::Malformed));
        assert!(valid_boot_id(BOOT) && !valid_boot_id(&BOOT.to_uppercase()) && !valid_boot_id(""));
    }
}
