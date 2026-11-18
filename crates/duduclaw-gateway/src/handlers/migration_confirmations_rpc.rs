//! Live dashboard authorization, atomic decisions, and bounded durable apply.
use super::*;
use duduclaw_migration::{confirmation::{ApprovalSession, ConfirmationStore},
    contract::{ProposalBinding, ConfirmationStatus}, ids::RequestId};
use super::migration_authority::{GatewayAuthority, GatewayAccountReadback, LiveAuthority, MigrationAuthorityBackend};
use super::migration_authority::employee::EmployeeAuthority;

/// R7d: an employee allocation whose SSH refusal spawnd has not verified
/// within the bounded wait. The item stays `applying` (nothing compensated);
/// Retry resumes the same reservation (dashboard key
/// `migrationConfirm.allocationPending`).
pub(super) const ALLOCATION_PENDING: &str = "帳號安全驗證尚未完成，請稍後重試";
/// R7d round 3: a `reject` whose compensation (Disable) did not start
/// because gateway shutdown has begun; the item keeps its state.
pub(super) const SHUTTING_DOWN_RETRY: &str = "服務正在關閉，請稍後重試";

/// R7d round 1: confirmation apply loops (approve / retry / reject) that may run at
/// once in this process. Equal to spawnd's pending-reservation cap
/// (`registry::MAX_PENDING_RESERVATIONS`): more concurrent loops cannot make
/// more allocations progress, they would only hold blocking threads.
const APPLY_LOOPS_MAX: usize = 4;
static APPLY_LOOPS: ApplyAdmission = ApplyAdmission::new(APPLY_LOOPS_MAX);

/// A counting semaphore that never queues: a full one refuses at once.
pub(super) struct ApplyAdmission { running: std::sync::atomic::AtomicUsize, capacity: usize }
/// One admitted apply loop; dropping it (every path out of the loop, an
/// unwinding panic and a task that never ran included) releases the slot.
pub(super) struct ApplyPermit<'a>(&'a ApplyAdmission);
impl ApplyAdmission {
    pub(super) const fn new(capacity: usize) -> Self { Self { running: std::sync::atomic::AtomicUsize::new(0), capacity } }
    pub(super) fn try_enter(&self) -> Option<ApplyPermit<'_>> {
        use std::sync::atomic::Ordering::SeqCst;
        self.running.fetch_update(SeqCst, SeqCst, |n| (n < self.capacity).then_some(n + 1)).ok().map(|_| ApplyPermit(self))
    }
    /// Read only: the number of permits held now.
    pub(super) fn running(&self) -> usize { self.running.load(std::sync::atomic::Ordering::SeqCst) }
}

/// R7d round 2: how long gateway shutdown waits (best effort) for permits
/// still held, so an effect already in progress can finish cleanly. It does
/// not enforce anything: see the shutdown flag (`bridge::SHUTTING_DOWN`).
/// On expiry shutdown continues.
pub(crate) const APPLY_SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(40);
const APPLY_SHUTDOWN_POLL: std::time::Duration = std::time::Duration::from_millis(100);
/// Called after `begin_migration_apply_shutdown`: no new effect starts
/// (effect gate), so this only waits for one already in progress. Async
/// sleeps, never a blocked runtime worker.
pub(crate) async fn wait_for_apply_loops() {
    if !wait_idle(&APPLY_LOOPS, APPLY_SHUTDOWN_WAIT, APPLY_SHUTDOWN_POLL).await {
        warn!(running = APPLY_LOOPS.running(), "migration apply loops still running after {}s — continuing shutdown",
            APPLY_SHUTDOWN_WAIT.as_secs());
    }
}
/// `true` once no permit is held; `false` when `bound` elapsed first.
async fn wait_idle(admission: &ApplyAdmission, bound: std::time::Duration, poll: std::time::Duration) -> bool {
    let until = tokio::time::Instant::now() + bound;
    loop {
        if admission.running() == 0 { return true; }
        let now = tokio::time::Instant::now();
        if now >= until { return false; }
        tokio::time::sleep((until - now).min(poll)).await;
    }
}

fn apply_outcome(failed: usize, pending: usize) -> Value {
    if pending > 0 {
        return json!({ "readback_required": true, "failed": failed, "allocation_pending": pending,
            "message": ALLOCATION_PENDING });
    }
    json!({ "readback_required": true, "failed": failed })
}

/// The approve / retry apply loop. R7d round 2 step gate: `stopping`
/// (gateway shutdown) is checked before every `apply_next_until` call and,
/// inside it, before the step and before compensation. Once it answers
/// `true` no new effect starts: the current and every remaining binding
/// count as `allocation_pending` and keep the state the journal already has.
/// The stop path never reaches the `Failed` transition (only a step failure
/// seen while not stopping does).
pub(super) fn apply_bindings(store: &mut ConfirmationStore, bindings: &[ProposalBinding],
    applier: &mut dyn duduclaw_migration::confirmation::ProposalApplier, stopping: &dyn Fn() -> bool,
    session_live: &mut dyn FnMut() -> bool,
    on_applied: &mut dyn FnMut(&duduclaw_migration::contract::ConfirmationItem)) -> Result<Value, &'static str> {
    let (mut failed, mut pending) = (0, 0);
    'items: for binding in bindings {
        for _ in 0..3 {
            if stopping() { pending += 1; continue 'items; }
            // A role/epoch change during a long bulk operation stops the
            // remaining effects; their approved states remain resumable.
            if !session_live() { return Err("身分驗證已失效，請重新驗證後繼續。"); }
            match store.apply_next_until(binding, applier, stopping) {
                Ok(item) if item.status == ConfirmationStatus::Applied => { on_applied(&item); break }
                Ok(_) => {},
                // R7d: still Applying and uncompensated; Retry resumes it.
                Err(duduclaw_migration::confirmation::ConfirmationError::Pending) => { pending += 1; break; },
                Err(_) => { failed += 1; break; },
            }
        }
    }
    Ok(apply_outcome(failed, pending))
}

/// Retry of a `Failed` item (R7d round 3): its compensation is an effect
/// behind the effect gate (`stopping` checked right before it). Stopped:
/// the retry-safe pending answer, the item stays `Failed`. `Ok(Some)` ends
/// the call with that answer; `Ok(None)`: the item is `Applying` again and
/// the apply loop runs. Called while the RPC holds its apply permit.
#[allow(clippy::too_many_arguments)]
pub(super) fn retry_failed(store: &mut ConfirmationStore, item: &duduclaw_migration::contract::ConfirmationItem,
    approval: &ApprovalSession, csrf: [u8; 32], now: u64,
    applier: &mut dyn duduclaw_migration::confirmation::ProposalApplier,
    validator: &dyn duduclaw_migration::confirmation::ProposalValidator,
    stopping: &dyn Fn() -> bool, error: &'static str) -> Result<Option<Value>, &'static str> {
    if item.category == duduclaw_migration::contract::ConfirmationCategory::Employee {
        // Reconcile any interrupted compensation, but never
        // revive a permanently tombstoned UID on Retry.
        approval.authorize(&csrf, now).map_err(|_| error)?;
        // Effect gate: nothing that can block between this check and the call.
        if stopping() { return Ok(Some(apply_outcome(0, 1))); }
        duduclaw_migration::confirmation::ProposalApplier::disable_and_read_back(applier, item).map_err(|_| error)?;
        return Err("此員工配置已撤銷，舊身分不會重用。請重新建立員工配置提案，再逐筆確認。");
    }
    match store.retry_until(&item.binding, approval, csrf, now, applier, validator, stopping) {
        Ok(()) => Ok(None),
        Err(duduclaw_migration::confirmation::ConfirmationError::Pending) => Ok(Some(apply_outcome(0, 1))),
        Err(_) => Err(error),
    }
}

/// One `reject` (R7d round 3): the compensation (Disable) is behind the
/// effect gate. Stopped: [`SHUTTING_DOWN_RETRY`], the item keeps its state.
/// Called while the RPC holds its apply permit.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_one(store: &mut ConfirmationStore, binding: &ProposalBinding, approval: &ApprovalSession,
    csrf: [u8; 32], now: u64, applier: &mut dyn duduclaw_migration::confirmation::ProposalApplier,
    stopping: &dyn Fn() -> bool, error: &'static str) -> Result<Value, &'static str> {
    match store.reject_until(binding, approval, csrf, now, applier, stopping) {
        Ok(()) => Ok(json!({ "readback_required": true })),
        Err(duduclaw_migration::confirmation::ConfirmationError::Pending) => Err(SHUTTING_DOWN_RETRY),
        Err(_) => Err(error),
    }
}
impl Drop for ApplyPermit<'_> {
    fn drop(&mut self) { self.0.running.fetch_sub(1, std::sync::atomic::Ordering::SeqCst); }
}
/// Admission of one RPC. approve / retry / reject (each may run an effect,
/// reject a Disable) need a permit (`Ok(Some)`), so the shutdown wait also
/// covers an effect already in progress there; every other method needs
/// none (`Ok(None)`). Full: `Err` with the retry-safe
/// pending answer. Called before the blocking task starts, so a refused call
/// has opened no store, checked no CSRF, approved nothing, written no intent
/// and consumed nothing: every item stays exactly as it was.
pub(super) fn admit<'a>(admission: &'a ApplyAdmission, method: &str, params: &Value) -> Result<Option<ApplyPermit<'a>>, Value> {
    if !matches!(method, "migration.confirmations.approve" | "migration.confirmations.retry"
        | "migration.confirmations.reject") { return Ok(None); }
    admission.try_enter().map(Some).ok_or_else(|| {
        let requested = params.get("bindings").and_then(Value::as_array).map_or(1, |b| b.len().clamp(1, 200));
        json!({ "readback_required": true, "failed": 0, "allocation_pending": requested, "message": ALLOCATION_PENDING })
    })
}

pub(super) fn known_method(method: &str) -> bool {
    matches!(method, "migration.confirmations.list" | "migration.confirmations.step_up"
        | "migration.confirmations.approve" | "migration.confirmations.retry"
        | "migration.confirmations.reject" | "migration.confirmations.rotate_secret")
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    bindings: Vec<ProposalBinding>,
    csrf_token: [u8; 32],
}

impl MethodHandler {
    pub(crate) async fn handle_migration_confirmations(&self, method: &str, params: Value,
        ctx: &UserContext) -> WsFrame {
        if !known_method(method) || ctx.user_id == "system" {
            return WsFrame::error_response("", "不允許此遷移操作。");
        }
        let session = match crate::google_personal::session_for(&ctx.user_id) {
            Ok(session) => session,
            Err(_) => return WsFrame::error_response("", "請以管理員帳號重新登入後再操作。"),
        };
        if self.mfa_require_current_admin(ctx).await.is_err() {
            return WsFrame::error_response("", "請以管理員帳號重新登入後再操作。");
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return WsFrame::error_response("", "無法讀取帳號資料。");
        };
        if method == "migration.confirmations.step_up" && !crate::server::check_login_rate_limit(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), &ctx.email) {
            return WsFrame::error_response("", "嘗試次數過多，請稍後再試。");
        }
        let home = self.home_dir.clone();
        let user = ctx.user_id.clone();
        let epoch = ctx.credential_epoch;
        let method = method.to_owned();
        let runtime = tokio::runtime::Handle::current();
        let live=Arc::new(LiveAuthority {registry:self.registry.clone(),context:ctx.clone(),handle:runtime.clone(),scheduler:self.cron_scheduler.read().await.clone()});
        // F6fb item 8: an adoption card enforces the licence cap at approval and apply.
        let agent_cap = self.effective_agent_cap().await;
        // F6g: set when an apply committed an adoption (its tree is now under agents/).
        let adopted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let adopted_flag = adopted.clone();
        // R7d round 1: the process-wide apply-loop cap, taken before the
        // blocking task (a full cap never queues, nothing has run yet).
        let permit = match admit(&APPLY_LOOPS, &method, &params) {
            Ok(permit) => permit,
            Err(pending) => return WsFrame::ok_response("", pending),
        };
        let result = tokio::task::spawn_blocking(move || -> Result<Value, &'static str> {
            // Held until this closure ends (or is dropped unrun).
            let _permit = permit;
            let now = chrono::Utc::now().timestamp();
            if now < 0 { return Err("無法確認目前時間。"); }
            let path = home.join("migration-confirmations.db");
            match std::fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.file_type().is_file() && metadata.len() <= 64 * 1024 * 1024 => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
                    && method == "migration.confirmations.list" => {
                    return Ok(json!({ "active": false, "incomplete": false, "items": [],
                        "mutations_available": false, "csrf_token": null,
                        "mfa_enrolled": false, "step_up_valid": false }));
                }
                _ => return Err("無法讀取遷移狀態。"),
            }
            if method == "migration.confirmations.step_up" {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct StepUp { password: String, code: String }
                let input: StepUp = serde_json::from_value(params).map_err(|_| "請輸入密碼與驗證碼。")?;
                db.migration_step_up(&user, &session, epoch, &input.password, &input.code,
                    &crate::mfa::Cipher(&home), now).map_err(|_| "驗證失敗，請確認密碼與新的驗證碼後重試。")?;
                return Ok(json!({ "verified": true }));
            }
            let proof = db.migration_confirmation_session(&user, &session, epoch, now)
                .map_err(|_| "登入憑證已失效，請重新登入。")?;
            if method == "migration.confirmations.list" {
                let store = ConfirmationStore::open_read_only(&path).map_err(|_| "無法讀取遷移狀態。")?;
                let items = store.list().map_err(|_| "無法讀取遷移狀態。")?;
                // F6d: Employee and EmployeeRetire items behind one backend.
                // F6fa: the listing's step-up freshness is the real one (an
                // early-purged retire card without a fresh proof lists as not
                // applicable until the Admin steps up).
                let employee_backend = super::migration_authority::retire::production_backend(home.clone(),
                    Arc::new(EmployeeAuthority::new(home.clone(), live.clone()).with_agent_cap(agent_cap)
                        .with_step_up(proof.as_ref().map(|p| p.verified_at))), &user, proof.as_ref().map(|p| p.verified_at));
                let items: Vec<Value> = items.into_iter().map(|item| {
                    let supported = employee_backend.validate(&item).is_ok() || GatewayAuthority::built_in_item_supported(&item)
                        || GatewayAccountReadback::available(&home, &item)
                        || (matches!(item.category,duduclaw_migration::contract::ConfirmationCategory::EmployeeInstructions | duduclaw_migration::contract::ConfirmationCategory::Skill | duduclaw_migration::contract::ConfirmationCategory::Hook) && !item.fields.iter().any(|f|f.name=="requires_reentry"));
                    let rotation_pending = store.rotation_pending(&item.binding).unwrap_or(true);
                    let content_status = if item.category == duduclaw_migration::contract::ConfirmationCategory::Employee {
                        match item.status {
                            ConfirmationStatus::Rejected | ConfirmationStatus::ExpiredRejected => Some("not_migrated_rejected"),
                            ConfirmationStatus::Applied => {
                                let migrated=crate::migration_startup::completed_manifest(&home).ok().flatten().is_some_and(|manifest| {
                                    let grants:Vec<_>=manifest.grants.iter().filter(|entry| matches!(&entry.materialization,
                                        Some(duduclaw_migration::materialization::MaterializationDecision::Confirmed{allocation}) if allocation.proposal==item.binding))
                                        .map(|entry|entry.grant.clone()).collect();
                                    !grants.is_empty() && duduclaw_migration::materialization::MaterializationStore::open_read_only(&home.join("migration-materialization.db"))
                                        .and_then(|ledger|ledger.all_terminal(&grants)).unwrap_or(false)
                                });
                                Some(if migrated {"migrated"}else{"awaiting_materialization"})
                            },
                            _ => None,
                        }
                    } else { None };
                    // v1.81 C5: provider, label, proposed binding/owner, detail references.
                    let connector = super::migration_authority::connectors::card(&item);
                    let mut value = serde_json::to_value(item).expect("bounded confirmation schema");
                    value["connector"] = json!(connector);
                    value["content_status"] = json!(content_status);
                    value["apply_available"] = json!(supported);
                    value["rotation_pending"] = json!(rotation_pending);
                    value
                }).collect();
                return Ok(json!({
                    // Only the root completion readback can declare protection.
                    "active": true, "incomplete": true, "items": items,
                    "confirmations_incomplete": store.incomplete().map_err(|_| "無法讀取遷移狀態。")?,
                    "mutations_available": true, "csrf_token": proof.as_ref().map(|p| p.csrf),
                    "mfa_enrolled": db.mfa_active(&user).map_err(|_| "無法讀取帳號資料。")?,
                    "step_up_valid": proof.is_some(),
                }));
            }
            let proof = proof.ok_or("請先以密碼與雙因素驗證確認身分。")?;
            if method == "migration.confirmations.rotate_secret" {
                #[derive(serde::Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Rotation { binding: ProposalBinding, csrf_token: [u8; 32], new_value: String }
                let input: Rotation = serde_json::from_value(params).map_err(|_| "請輸入新的憑證。")?;
                let actor = RequestId(*uuid::Uuid::parse_str(&user).map_err(|_| "無法確認帳號。")?.as_bytes());
                let approval = ApprovalSession::from_verified_session(true, true,
                    Some(proof.verified_at), proof.csrf, actor);
                approval.authorize(&input.csrf_token, now as u64).map_err(|_| "身分驗證已失效。")?;
                let mut store = ConfirmationStore::open_existing(&path).map_err(|_| "無法讀取遷移狀態。")?;
                let item = store.list().map_err(|_| "無法讀取遷移狀態。")?.into_iter()
                    .find(|item| item.binding == input.binding && item.status == ConfirmationStatus::Applied)
                    .ok_or("請先完成此項目的核准與套用。")?;
                let readback = Arc::new(GatewayAccountReadback::new(home.clone(), runtime));
                let employee_backend = Arc::new(EmployeeAuthority::new(home.clone(), live.clone()).with_agent_cap(agent_cap)
                    .with_step_up(Some(proof.verified_at)));
                let authority = GatewayAuthority::new(home).map_err(|_| "套用服務尚未就緒。")?.with_runtime(readback).with_live(live.clone()).with_backend(employee_backend);
                let fingerprint = authority.rotate_secret(&item, &input.new_value).map_err(|_| "憑證更新尚未確認，請重新載入後用相同新值重試。")?;
                store.record_rotation(&input.binding, "credential", fingerprint, &authority)
                    .map_err(|_| "憑證更新尚未確認，請重新載入後重試。")?;
                return Ok(json!({ "readback_required": true }));
            }
            let input: Decision = serde_json::from_value(params).map_err(|_| "確認內容不正確，請重新載入。")?;
            if input.bindings.is_empty() || input.bindings.len() > 200 {
                return Err("每次請選擇一至兩百個項目。");
            }
            let actor = RequestId(*uuid::Uuid::parse_str(&user).map_err(|_| "無法確認帳號。")?.as_bytes());
            let approval = ApprovalSession::from_verified_session(true, true,
                Some(proof.verified_at), proof.csrf, actor);
            let mut store = ConfirmationStore::open_existing(&path).map_err(|_| "無法更新遷移狀態。")?;
            // Separate handles share the durable binding ledger. No cloned payload
            // can bypass the normal fresh-input validator at approval or staging.
            let readback = Arc::new(GatewayAccountReadback::new(home.clone(), runtime));
            // F6d: one slot, two backends (Employee + EmployeeRetire).
            let employee_backend = super::migration_authority::retire::production_backend(home.clone(),
                Arc::new(EmployeeAuthority::new(home.clone(), live.clone()).with_agent_cap(agent_cap)
                    .with_step_up(Some(proof.verified_at))), &user, Some(proof.verified_at));
            let validator = GatewayAuthority::new(home.clone()).map_err(|_| "套用服務尚未就緒。")?.with_runtime(readback.clone()).with_live(live.clone()).with_backend(employee_backend.clone());
            let adopt_home = home.clone();
            let mut applier = GatewayAuthority::new(home).map_err(|_| "套用服務尚未就緒。")?.with_runtime(readback).with_live(live.clone()).with_backend(employee_backend);
            let error = "項目已變更、尚未支援或套用失敗，請重新載入確認目前狀態。";
            match method.as_str() {
                "migration.confirmations.approve" => {
                    store.approve(&input.bindings, &approval, input.csrf_token, now as u64, &validator).map_err(|_| error)?;
                }
                "migration.confirmations.retry" if input.bindings.len() == 1 => {
                    // The same approval binding may resume after process death.
                    let item = store.list().map_err(|_| error)?.into_iter()
                        .find(|item| item.binding == input.bindings[0]).ok_or(error)?;
                    if item.status == ConfirmationStatus::Failed {
                        // R7d round 3: compensation is an effect; none starts
                        // once shutdown has begun (the item stays Failed).
                        if let Some(answer) = retry_failed(&mut store, &item, &approval, input.csrf_token, now as u64,
                            &mut applier, &validator, &super::migration_authority::shutting_down, error)? {
                            return Ok(answer);
                        }
                    } else {
                        approval.authorize(&input.csrf_token, now as u64).map_err(|_| error)?;
                        if !matches!(item.status, ConfirmationStatus::Approved | ConfirmationStatus::Applying) {
                            return Err(error);
                        }
                    }
                }
                "migration.confirmations.reject" => {
                    if input.bindings.len() > 1 {
                        // Pending items only: no external effect.
                        store.reject_pending(&input.bindings, &approval, input.csrf_token, now as u64).map_err(|_| error)?;
                        return Ok(json!({ "readback_required": true }));
                    }
                    return reject_one(&mut store, &input.bindings[0], &approval, input.csrf_token, now as u64,
                        &mut applier, &super::migration_authority::shutting_down, error);
                }
                _ => return Err("不允許此遷移操作。"),
            }
            apply_bindings(&mut store, &input.bindings, &mut applier, &super::migration_authority::shutting_down,
                &mut || db.migration_confirmation_session(&user, &session, epoch, chrono::Utc::now().timestamp())
                    .ok().flatten().is_some(),
                // F6g: an adoption committed inside this call — notice now, rescan below.
                &mut |item: &duduclaw_migration::contract::ConfirmationItem| {
                    if crate::isolation_restore_rpc::adoption_committed(&adopt_home, item, &user, chrono::Utc::now().timestamp()) {
                        adopted_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                })
        }).await;
        if adopted.load(std::sync::atomic::Ordering::SeqCst) {
            // Same as a restore: the adopted tree is back under agents/ (F6d).
            if let Err(error) = self.registry.write().await.scan().await {
                warn!(error = %error, "employee adoption: registry rescan failed");
            }
        }
        match result {
            Ok(Ok(value)) => WsFrame::ok_response("", value),
            Ok(Err(message)) => WsFrame::error_response("", message),
            Err(_) => WsFrame::error_response("", "遷移服務無法使用，請重新載入。"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn migration_confirmation_methods_are_exact() {
        for method in ["migration.confirmations.list", "migration.confirmations.step_up",
            "migration.confirmations.approve", "migration.confirmations.retry", "migration.confirmations.reject",
            "migration.confirmations.rotate_secret"] {
            assert!(known_method(method));
        }
        for method in ["migration.confirmations", "migration.confirmations.approve_all",
            "migration.confirmations.list.extra", "evil.migration.confirmations.list"] {
            assert!(!known_method(method));
        }
    }
    #[test]
    fn migration_decisions_refuse_client_authority_and_malformed_bindings() {
        assert!(serde_json::from_value::<Decision>(json!({"bindings":[], "csrf_token":[0], "admin":true})).is_err());
        assert!(serde_json::from_value::<Decision>(json!({"bindings":[], "csrf_token":vec![0;32], "mfa_enrolled":true})).is_err());
    }

    struct Valid;
    impl duduclaw_migration::confirmation::ProposalValidator for Valid {
        fn validate_fresh(&self, _: &duduclaw_migration::contract::ConfirmationItem)
            -> std::result::Result<(), duduclaw_migration::confirmation::ConfirmationError> { Ok(()) }
    }
    /// A journal with one Pending Employee proposal.
    fn journal() -> (ConfirmationStore, ProposalBinding) {
        use duduclaw_migration::contract::{ConfirmationCategory, ProposalField, ProposalOperationId, ProposalValue};
        let mut store = ConfirmationStore::from_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let field = |name: &str, value: &str| ProposalField { name: name.into(), value: ProposalValue::Text { value: value.into() } };
        let item = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([5; 16]), revision: 1 }, ConfirmationCategory::Employee,
            vec![field("disk_bytes", "67108864"), field("employee_id", "sales")], &Valid).unwrap();
        (store, item.binding)
    }
    fn snapshot(store: &ConfirmationStore) -> Value { serde_json::to_value(store.list().unwrap()).unwrap() }

    /// R7d round 1: a full cap answers at once with the retry-safe pending
    /// outcome; the approve behind it never runs and the journal is unchanged.
    #[test]
    fn apply_admission_full_answers_pending_at_once_and_leaves_journal_unchanged() {
        assert_eq!(APPLY_LOOPS_MAX, 4);
        let admission = ApplyAdmission::new(APPLY_LOOPS_MAX);
        let held: Vec<_> = (0..APPLY_LOOPS_MAX).map(|_| admission.try_enter().unwrap()).collect();
        let (mut store, binding) = journal();
        let before = snapshot(&store);
        let session = ApprovalSession::from_verified_session(true, true, Some(100), [7; 32], RequestId([8; 16]));
        for method in ["migration.confirmations.approve", "migration.confirmations.retry",
            "migration.confirmations.reject"] {
            let started = std::time::Instant::now();
            let params = json!({ "bindings": [binding], "csrf_token": vec![7u8; 32] });
            let answer = match admit(&admission, method, &params) {
                Ok(_) => { store.approve(&[binding], &session, [7; 32], 101, &Valid).unwrap(); panic!("admitted past a full cap") }
                Err(answer) => answer,
            };
            assert!(started.elapsed() < std::time::Duration::from_secs(1));
            assert_eq!(answer["allocation_pending"], json!(1));
            assert_eq!(answer["failed"], json!(0));
            assert_eq!(answer["message"], json!(ALLOCATION_PENDING));
            assert_eq!(snapshot(&store), before, "{method}");
            assert_eq!(admission.running(), APPLY_LOOPS_MAX);
        }
        // Methods without an apply effect take no permit, even when full.
        for method in ["migration.confirmations.list", "migration.confirmations.step_up",
            "migration.confirmations.rotate_secret"] {
            assert!(matches!(admit(&admission, method, &json!({})), Ok(None)), "{method}");
        }
        drop(held);
        assert_eq!(admission.running(), 0);
        // With a slot free the same approve is admitted and runs.
        let permit = admit(&admission, "migration.confirmations.approve", &json!({ "bindings": [binding] })).unwrap();
        assert!(permit.is_some() && admission.running() == 1);
        store.approve(&[binding], &session, [7; 32], 101, &Valid).unwrap();
        drop(permit);
        assert_eq!(admission.running(), 0);
    }

    /// R7d round 2: the shutdown wait returns at once when idle, as soon as
    /// the last permit is dropped, and at the (injected) bound while one is
    /// still held.
    #[tokio::test]
    async fn apply_shutdown_wait_ends_at_last_permit_or_bound() {
        use std::time::{Duration, Instant};
        assert_eq!((APPLY_SHUTDOWN_WAIT, APPLY_SHUTDOWN_POLL), (Duration::from_secs(40), Duration::from_millis(100)));
        static ADMISSION: ApplyAdmission = ApplyAdmission::new(2);
        let started = Instant::now();
        assert!(wait_idle(&ADMISSION, Duration::from_secs(5), APPLY_SHUTDOWN_POLL).await);
        assert!(started.elapsed() < Duration::from_millis(500));
        // Held: times out at the bound.
        let first = ADMISSION.try_enter().unwrap();
        let started = Instant::now();
        assert!(!wait_idle(&ADMISSION, Duration::from_millis(150), Duration::from_millis(10)).await);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(150) && elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert_eq!(ADMISSION.running(), 1);
        // Two held, dropped at ~50 ms and ~150 ms: the wait ends with the last.
        let second = ADMISSION.try_enter().unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(first);
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(second);
        });
        let started = Instant::now();
        assert!(wait_idle(&ADMISSION, Duration::from_secs(10), Duration::from_millis(10)).await);
        let elapsed = started.elapsed();
        assert!(elapsed >= Duration::from_millis(150) && elapsed < Duration::from_secs(2), "{elapsed:?}");
        assert_eq!(ADMISSION.running(), 0);
        release.await.unwrap();
    }

    /// Records each Disable with the number of permits held while it ran.
    struct Probe<'a> { admission: &'a ApplyAdmission, fail_stage: bool, disables: Vec<usize> }
    impl duduclaw_migration::confirmation::ProposalApplier for Probe<'_> {
        fn stage(&mut self, _: &duduclaw_migration::contract::ConfirmationItem)
            -> std::result::Result<(), duduclaw_migration::confirmation::ConfirmationError> {
            if self.fail_stage { Err(duduclaw_migration::confirmation::ConfirmationError::Apply) } else { Ok(()) }
        }
        fn publish(&mut self, _: &duduclaw_migration::contract::ConfirmationItem)
            -> std::result::Result<(), duduclaw_migration::confirmation::ConfirmationError> { Ok(()) }
        fn read_back(&mut self, _: &duduclaw_migration::contract::ConfirmationItem)
            -> std::result::Result<(), duduclaw_migration::confirmation::ConfirmationError> { Ok(()) }
        fn disable_and_read_back(&mut self, _: &duduclaw_migration::contract::ConfirmationItem)
            -> std::result::Result<(), duduclaw_migration::confirmation::ConfirmationError> {
            self.disables.push(self.admission.running()); Ok(())
        }
    }
    /// A journal with one approved Department proposal.
    fn department() -> (ConfirmationStore, ProposalBinding) {
        use duduclaw_migration::contract::{ConfirmationCategory, ProposalField, ProposalOperationId, ProposalValue};
        let mut store = ConfirmationStore::from_connection(rusqlite::Connection::open_in_memory().unwrap()).unwrap();
        let item = store.stage_proposal(ProposalOperationId { proposal_id: RequestId([6; 16]), revision: 1 },
            ConfirmationCategory::Department,
            vec![ProposalField { name: "name".into(), value: ProposalValue::Text { value: "sales".into() } }], &Valid).unwrap();
        let session = ApprovalSession::from_verified_session(true, true, Some(100), [7; 32], RequestId([8; 16]));
        store.approve(&[item.binding], &session, [7; 32], 101, &Valid).unwrap();
        (store, item.binding)
    }
    fn status_of(store: &ConfirmationStore, binding: &ProposalBinding) -> ConfirmationStatus {
        store.list().unwrap().into_iter().find(|item| item.binding == *binding).unwrap().status
    }

    /// R7d round 3: reject and the retry of a Failed item hold an apply
    /// permit while they run (their Disable runs under it); during shutdown
    /// neither sends a Disable — reject answers `SHUTTING_DOWN_RETRY`, the
    /// retry the pending answer — and the journal is unchanged; the same
    /// call afterwards completes.
    #[test]
    fn reject_and_failed_retry_hold_a_permit_and_start_no_disable_during_shutdown() {
        let admission = ApplyAdmission::new(APPLY_LOOPS_MAX);
        let session = ApprovalSession::from_verified_session(true, true, Some(100), [7; 32], RequestId([8; 16]));
        let error = "項目已變更";
        // Reject of an Applying (staged) item.
        let (mut store, binding) = department();
        let mut probe = Probe { admission: &admission, fail_stage: false, disables: Vec::new() };
        store.apply_next(&binding, &mut probe).unwrap();
        let before = snapshot(&store);
        let permit = admit(&admission, "migration.confirmations.reject", &json!({ "bindings": [binding] })).unwrap();
        assert!(permit.is_some() && admission.running() == 1);
        assert_eq!(reject_one(&mut store, &binding, &session, [7; 32], 101, &mut probe, &|| true, error),
            Err(SHUTTING_DOWN_RETRY));
        assert!(probe.disables.is_empty(), "no Disable during shutdown");
        assert_eq!(snapshot(&store), before);
        assert_eq!(status_of(&store, &binding), ConfirmationStatus::Applying);
        assert_eq!(reject_one(&mut store, &binding, &session, [7; 32], 101, &mut probe, &|| false, error).unwrap(),
            json!({ "readback_required": true }));
        assert_eq!(probe.disables, vec![1], "the Disable ran under the reject's permit");
        assert_eq!(status_of(&store, &binding), ConfirmationStatus::Rejected);
        drop(permit);
        assert_eq!(admission.running(), 0);
        // Retry of a Failed item (store path, then the Employee branch).
        for employee in [false, true] {
            let (mut store, binding) = if employee {
                let (mut store, binding) = journal();
                store.approve(&[binding], &session, [7; 32], 101, &Valid).unwrap();
                (store, binding)
            } else { department() };
            let mut probe = Probe { admission: &admission, fail_stage: true, disables: Vec::new() };
            assert!(store.apply_next(&binding, &mut probe).is_err());
            assert_eq!(probe.disables, vec![0]);
            probe.fail_stage = false;
            let item = store.list().unwrap().into_iter().find(|item| item.binding == binding).unwrap();
            assert_eq!(item.status, ConfirmationStatus::Failed);
            let before = snapshot(&store);
            let permit = admit(&admission, "migration.confirmations.retry", &json!({ "bindings": [binding] })).unwrap();
            assert!(permit.is_some() && admission.running() == 1);
            let answer = retry_failed(&mut store, &item, &session, [7; 32], 101, &mut probe, &Valid, &|| true, error)
                .unwrap().expect("ends the call");
            assert_eq!((answer["allocation_pending"].clone(), answer["message"].clone()), (json!(1), json!(ALLOCATION_PENDING)));
            assert_eq!(probe.disables, vec![0], "no Disable during shutdown (employee: {employee})");
            assert_eq!(snapshot(&store), before);
            let resumed = retry_failed(&mut store, &item, &session, [7; 32], 101, &mut probe, &Valid, &|| false, error);
            assert_eq!(probe.disables, vec![0, 1], "the Disable ran under the retry's permit");
            if employee {
                assert!(resumed.is_err(), "a tombstoned employee is never revived");
            } else {
                assert!(matches!(resumed, Ok(None)));
                assert_eq!(status_of(&store, &binding), ConfirmationStatus::Applying);
            }
            drop(permit);
            assert_eq!(admission.running(), 0);
        }
    }

    /// The permit is released however the apply loop ends: success, failure,
    /// and an early return.
    #[test]
    fn apply_admission_permit_released_on_every_path() {
        fn apply_loop(permit: Option<ApplyPermit<'_>>, outcome: u8) -> std::result::Result<Value, &'static str> {
            let _permit = permit;
            if outcome == 2 { return Err("early"); }
            let failed = outcome == 1;
            if failed { Err("failed") } else { Ok(json!({ "readback_required": true })) }
        }
        let admission = ApplyAdmission::new(1);
        for outcome in [0u8, 1, 2] {
            let permit = admit(&admission, "migration.confirmations.approve", &json!({})).unwrap();
            assert_eq!(admission.running(), 1);
            assert!(admit(&admission, "migration.confirmations.retry", &json!({})).is_err(), "full at capacity 1");
            assert_eq!(apply_loop(permit, outcome).is_ok(), outcome == 0);
            assert_eq!(admission.running(), 0, "released after outcome {outcome}");
        }
    }
}
