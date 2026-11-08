//! Root precheck with the R5/S4/S5 result classes (§3.4) and the
//! collaborator seams of the restore engine (child module of
//! `isolation_restore`, re-exported there).
use super::*;

/// Result classes of [`restore_precheck`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootClass {
    Ok,
    /// Transport error, no reply, timeout or `IsolationUnavailable` (S5d):
    /// retry later, never adoption.
    Unavailable,
    /// Only an authoritative `NotFound` (S5b): adoption may be offered.
    Gone,
    /// spawnd `Inconsistent` or any ReadBack mismatch (S4): alert, nothing.
    Inconsistent,
    /// `root_state` retiring/retired, or phase Retiring/Retired.
    Retired,
    /// ReadBack ok but the employee is not idle (M8).
    Busy,
}

impl RootClass {
    pub(crate) fn code(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unavailable => "ROOT_UNAVAILABLE",
            Self::Gone => "ROOT_GONE",
            Self::Inconsistent => "ROOT_INCONSISTENT",
            Self::Retired => "ROOT_RETIRED",
            Self::Busy => "ROOT_BUSY",
        }
    }
}

/// spawnd allocation phase as reported by a ReadBack (F6l schema 2, S5c).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootPhase { Planned, Ready, Disabling, Disabled, Retiring, Retired }

/// One ReadBack answer. `idle` is the schema-2 field (S17); `None` makes the
/// precheck ask [`RootProbe::employee_idle`].
#[derive(Debug, Clone)]
pub(crate) struct RootReadBack {
    pub(crate) receipt: Receipt,
    pub(crate) phase: RootPhase,
    pub(crate) idle: Option<bool>,
}

/// Root refusals, kept apart so a transport failure can never read as gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootError { Transport, NoReply, Timeout, IsolationUnavailable, NotReady, InvalidRequest, NotFound, Inconsistent }

/// The root allocator ReadBack and the H4 `EmployeeIdle` liveness readback.
pub(crate) trait RootProbe {
    fn read_back(&self, item: &ConfirmationItem) -> Result<RootReadBack, RootError>;
    fn employee_idle(&self, employee: EmployeeId) -> Result<bool, RootError>;
}

/// Production probe. ReadBack goes through the schema-1 allocation socket,
/// which answers only Ready entries and replies `None` for every refusal,
/// so no answer can be an authoritative `NotFound` (`ROOT_GONE` needs F6l's
/// schema 2); a receipt the transport rejects as mismatched
/// (`Authentication`) is `Inconsistent`. The gateway has no client for the
/// spawnd control socket's `EmployeeIdle` (H4) yet, so idleness is
/// unavailable and every restore refuses `ROOT_UNAVAILABLE` until F6l adds
/// `idle` to the ReadBack or F6d wires that client (fail closed).
pub(crate) struct SpawndRootProbe;

impl RootProbe for SpawndRootProbe {
    fn read_back(&self, item: &ConfirmationItem) -> Result<RootReadBack, RootError> {
        #[cfg(target_os = "linux")]
        {
            use duduclaw_migration::allocation::{Action, Request};
            let nonce = duduclaw_migration::ids::RequestId(*uuid::Uuid::new_v4().as_bytes());
            match allocation::transport::request(&Request { schema: 1, nonce, action: Action::ReadBack, item: item.clone() }) {
                Ok(receipt) => Ok(RootReadBack { receipt, phase: RootPhase::Ready, idle: None }),
                Err(duduclaw_migration::Error::Authentication) => Err(RootError::Inconsistent),
                Err(_) => Err(RootError::Transport),
            }
        }
        #[cfg(not(target_os = "linux"))]
        { let _ = item; Err(RootError::Transport) }
    }
    /// F6d: the spawnd `EmployeeIdle` request (transport error ⇒ unavailable).
    fn employee_idle(&self, employee: EmployeeId) -> Result<bool, RootError> {
        crate::isolation_restore_wiring::spawnd_employee_idle(employee)
    }
}

/// `lifecycle::resolve` without the `prior.disabled` gate (§3.4 root
/// precheck): the Applied proposal must be found with `spec.binding ==
/// prior.principal_binding`, and the ReadBack must give a non-disabled
/// receipt with the same employee, UID, label, quota and principal, and the
/// employee must be idle (M8). Workspace contents are not covered. `Err` is a
/// gateway-side refusal (unreadable store, unknown or replaced employee).
pub(crate) fn restore_precheck(home: &Path, label: &str, employee: EmployeeId, root: &dyn RootProbe)
    -> Result<RootClass, LifecycleError> {
    use LifecycleError::*;
    if !lifecycle::valid_label(label) { return Err(Denied); }
    let (prior, pinned, _) = lifecycle::confirmed_row(home, label)?.ok_or(Denied)?;
    if prior.label != label || prior.employee != employee { return Err(Denied); }
    match root_state(home, employee)?.as_deref() {
        Some("allocated") => {}
        Some("retiring" | "retired") => return Ok(RootClass::Retired),
        Some("root_absent") => return Ok(RootClass::Gone),
        _ => return Err(Unavailable),
    }
    let pinned = pinned.ok_or(Denied)?;
    lifecycle::principal(&prior, &pinned)?;
    let item = lifecycle::applied_item(home, &prior.binding)?;
    let spec = allocation::specification(&item).map_err(|_| Denied)?;
    if spec.label != label { return Err(Denied); }
    if spec.binding != prior.principal_binding { return Err(Stale); }
    let answer = match root.read_back(&item) {
        Ok(answer) => answer,
        Err(RootError::NotFound) => return Ok(RootClass::Gone),
        Err(RootError::Inconsistent) => return Ok(RootClass::Inconsistent),
        Err(_) => return Ok(RootClass::Unavailable),
    };
    match answer.phase {
        RootPhase::Ready => {}
        RootPhase::Retiring | RootPhase::Retired => return Ok(RootClass::Retired),
        RootPhase::Planned => return Ok(RootClass::Unavailable),
        RootPhase::Disabling | RootPhase::Disabled => return Ok(RootClass::Inconsistent),
    }
    let current = &answer.receipt;
    if current.disabled || current.employee != prior.employee || current.uid != prior.uid
        || current.binding != prior.binding || current.label != prior.label || current.bytes != prior.bytes
        || current.inodes != prior.inodes || current.principal_binding != prior.principal_binding {
        return Ok(RootClass::Inconsistent);
    }
    let idle = match answer.idle { Some(idle) => Ok(idle), None => root.employee_idle(employee) };
    Ok(match idle {
        Ok(true) => RootClass::Ok,
        Ok(false) => RootClass::Busy,
        Err(RootError::NotFound | RootError::Inconsistent) => RootClass::Inconsistent,
        Err(_) => RootClass::Unavailable,
    })
}

fn root_state(home: &Path, employee: EmployeeId) -> Result<Option<String>, LifecycleError> {
    let db = lifecycle::open_store(home, APPLY_DB, Access::ReadOnly)?;
    quarantine::check_readable(&db).map_err(|_| LifecycleError::Unavailable)?;
    db.query_row("SELECT root_state FROM root_allocations WHERE employee_hex=?1", [hex::encode(employee.0)], |r| r.get(0))
        .optional().map_err(|_| LifecycleError::Unavailable)
}

/// Adapter for the post-commit lifecycle read (§3.5 step 4).
pub(crate) struct ProbeRoot<'a>(pub(crate) &'a dyn RootProbe);
impl lifecycle::RootAllocation for ProbeRoot<'_> {
    fn read_back(&self, item: &ConfirmationItem) -> Result<Receipt, LifecycleError> {
        self.0.read_back(item).map(|answer| answer.receipt).map_err(|_| LifecycleError::Unavailable)
    }
}

// ── Collaborators ───────────────────────────────────────────────────────

/// Catalog routes the restore lists and fences.
pub(crate) trait RouteSource {
    /// Every active route the employee would use again (Q4), sorted
    /// `(account, generation)`; the request digest covers the full list.
    fn routes_for(&self, binding: &EmployeeBinding, tenant: &str) -> Result<Vec<(String, u64)>, ()>;
    /// Active routes whose C6 detail row names `employee` (must be none, H2).
    fn detail_routes_naming(&self, employee: EmployeeId) -> Result<Vec<String>, ()>;
}

/// Production routes: the connector catalog. No catalog file means nothing
/// was ever provisioned (empty lists); anything unreadable is `Err`.
pub(crate) struct CatalogRoutes<'a>(pub(crate) &'a Path);

impl CatalogRoutes<'_> {
    fn catalog(&self) -> Result<Option<crate::isolation_connectors::ConnectorCatalog>, ()> {
        use crate::isolation_connectors::{ConnectorCatalog, CATALOG_FILE};
        match std::fs::symlink_metadata(self.0.join(CATALOG_FILE)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(()),
            Ok(_) => ConnectorCatalog::open(self.0).map(Some).map_err(|_| ()),
        }
    }
}

impl RouteSource for CatalogRoutes<'_> {
    fn routes_for(&self, binding: &EmployeeBinding, tenant: &str) -> Result<Vec<(String, u64)>, ()> {
        let Some(catalog) = self.catalog()? else { return Ok(Vec::new()) };
        let mut bindings = vec![binding.clone()];
        // A Personal employee also uses the installation's SharedHome routes.
        if matches!(binding, EmployeeBinding::Personal(_)) {
            bindings.push(EmployeeBinding::Shared(duduclaw_isolation::bounded::Text::new(tenant.to_owned()).map_err(|_| ())?));
        }
        let mut routes = Vec::new();
        for binding in &bindings {
            for route in catalog.active_routes_for(binding).map_err(|_| ())? {
                if route.active { routes.push((route.account.as_str().to_owned(), route.generation)); }
            }
        }
        routes.sort();
        routes.dedup();
        Ok(routes)
    }
    fn detail_routes_naming(&self, employee: EmployeeId) -> Result<Vec<String>, ()> {
        let Some(catalog) = self.catalog()? else { return Ok(Vec::new()) };
        let mut named = Vec::new();
        let mut after = None;
        loop {
            let page = catalog.list(after.as_ref(), 500).map_err(|_| ())?;
            let Some(last) = page.last().map(|route| route.account.clone()) else { break };
            for route in page {
                if !route.active { continue; }
                if let Some((_, Some(record))) = catalog.route_details(&route.account).map_err(|_| ())? {
                    if record.employee == employee { named.push(route.account.as_str().to_owned()); }
                }
            }
            after = Some(last);
        }
        Ok(named)
    }
}

/// F6h store marks of the label-keyed stores (§3.3).
pub(crate) trait LabelStores {
    /// The marks now; `Err` = unreadable (never "unchanged").
    fn marks(&self, home: &Path, label: &str) -> Result<Value, ()>;
}

/// Production until F6h lands the `LABEL_KEYED_STORES` registry: unreadable,
/// so every restore refuses `STORES_UNAVAILABLE` (fail closed: without the
/// writer gate and marks nothing proves the stores were left alone).
pub(crate) struct UnregisteredStores;
impl LabelStores for UnregisteredStores {
    fn marks(&self, _: &Path, _: &str) -> Result<Value, ()> { Err(()) }
}

/// Request kinds of `employee_restores.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RequestKind { PersonalRestore, SharedRestore, PersonalAdopt, SharedAdopt, EarlyPurge }

impl RequestKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PersonalRestore => "personal_restore", Self::SharedRestore => "shared_restore",
            Self::PersonalAdopt => "personal_adopt", Self::SharedAdopt => "shared_adopt", Self::EarlyPurge => "early_purge",
        }
    }
    pub(crate) fn parse(value: &str) -> Option<Self> {
        [Self::PersonalRestore, Self::SharedRestore, Self::PersonalAdopt, Self::SharedAdopt, Self::EarlyPurge]
            .into_iter().find(|kind| kind.as_str() == value)
    }
}

/// What the approval rule allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Approval { pub(crate) self_approved: bool }

/// R1/R2/S1/S2 approval rule (task F6c2). Called inside the users.db
/// transaction of the step that applies (`users` is that transaction), so the
/// Admin count and the eligibility log are read with the writes. `Err` is a
/// refusal code (`APPROVER_NOT_QUALIFIED`, `ADMIN_LOG_UNAVAILABLE`, …).
pub(crate) trait ApprovalRule {
    fn decide(&self, users: &Connection, requester: &str, approver: &str, kind: RequestKind) -> Result<Approval, &'static str>;
}

/// Production rule until F6c2 lands: refuses every approval.
pub(crate) struct ApprovalRuleUnavailable;
impl ApprovalRule for ApprovalRuleUnavailable {
    fn decide(&self, _: &Connection, _: &str, _: &str, _: RequestKind) -> Result<Approval, &'static str> {
        Err("APPROVAL_RULE_UNAVAILABLE")
    }
}

/// The rule's answer must be self-consistent: an approver who is the
/// requester is only ever `self_approved`, and nobody else is.
pub(crate) fn decide(rule: &dyn ApprovalRule, users: &Connection, requester: &str, approver: &str, kind: RequestKind)
    -> Result<Approval, Refusal> {
    let approval = rule.decide(users, requester, approver, kind).map_err(Refusal::Approval)?;
    if approval.self_approved != (approver == requester) { return Err(Refusal::Approval("APPROVAL_RULE_INCONSISTENT")); }
    Ok(approval)
}

/// Collaborators of every restore step.
pub(crate) struct RestoreEnv<'a> {
    pub(crate) root: &'a dyn RootProbe,
    pub(crate) routes: &'a dyn RouteSource,
    pub(crate) stores: &'a dyn LabelStores,
    /// Fence retry of the tombstone's run stop (S13).
    pub(crate) runs: &'a dyn quarantine::RunStopper,
    pub(crate) rule: &'a dyn ApprovalRule,
    /// Agent-count cap in force (0 = unlimited), read by the RPC (L2).
    pub(crate) agent_cap: usize,
    /// The verified Admin acting for fence retries (route revocation audit).
    pub(crate) actor: &'a str,
    /// Unix seconds.
    pub(crate) now: i64,
}
