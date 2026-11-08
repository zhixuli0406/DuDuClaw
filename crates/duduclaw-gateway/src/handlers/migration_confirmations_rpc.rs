//! Live dashboard authorization, atomic decisions, and bounded durable apply.
use super::*;
use duduclaw_migration::{confirmation::{ApprovalSession, ConfirmationStore},
    contract::{ProposalBinding, ConfirmationStatus}, ids::RequestId};
use super::migration_authority::{GatewayAuthority, GatewayAccountReadback, LiveAuthority, MigrationAuthorityBackend};
use super::migration_authority::employee::EmployeeAuthority;

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
        // F6g: set when an apply committed an adoption (its tree is now under agents/).
        let adopted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let adopted_flag = adopted.clone();
        let result = tokio::task::spawn_blocking(move || -> Result<Value, &'static str> {
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
                let employee_backend = super::migration_authority::retire::production_backend(home.clone(),
                    Arc::new(EmployeeAuthority::new(home.clone(), live.clone())), &user);
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
                let employee_backend = Arc::new(EmployeeAuthority::new(home.clone(), live.clone()));
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
                Arc::new(EmployeeAuthority::new(home.clone(), live.clone())), &user);
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
                        if item.category == duduclaw_migration::contract::ConfirmationCategory::Employee {
                            // Reconcile any interrupted compensation, but never
                            // revive a permanently tombstoned UID on Retry.
                            approval.authorize(&input.csrf_token, now as u64).map_err(|_| error)?;
                            duduclaw_migration::confirmation::ProposalApplier::disable_and_read_back(&mut applier, &item).map_err(|_| error)?;
                            return Err("此員工配置已撤銷，舊身分不會重用。請重新建立員工配置提案，再逐筆確認。");
                        }
                        store.retry(&item.binding, &approval, input.csrf_token, now as u64,
                            &mut applier, &validator).map_err(|_| error)?;
                    } else {
                        approval.authorize(&input.csrf_token, now as u64).map_err(|_| error)?;
                        if !matches!(item.status, ConfirmationStatus::Approved | ConfirmationStatus::Applying) {
                            return Err(error);
                        }
                    }
                }
                "migration.confirmations.reject" => {
                    if input.bindings.len() > 1 {
                        store.reject_pending(&input.bindings, &approval, input.csrf_token, now as u64).map_err(|_| error)?;
                    } else {
                        store.reject(&input.bindings[0], &approval, input.csrf_token, now as u64, &mut applier).map_err(|_| error)?;
                    }
                    return Ok(json!({ "readback_required": true }));
                }
                _ => return Err("不允許此遷移操作。"),
            }
            let mut failed = 0;
            for binding in &input.bindings {
                for _ in 0..3 {
                    // A role/epoch change during a long bulk operation stops the
                    // remaining effects; their approved states remain resumable.
                    if db.migration_confirmation_session(&user, &session, epoch, chrono::Utc::now().timestamp())
                        .ok().flatten().is_none() { return Err("身分驗證已失效，請重新驗證後繼續。"); }
                    match store.apply_next(binding, &mut applier) {
                        Ok(item) if item.status == ConfirmationStatus::Applied => {
                            // F6g: an adoption committed inside this call — notice now, rescan below.
                            if crate::isolation_restore_rpc::adoption_committed(&adopt_home, &item, &user, chrono::Utc::now().timestamp()) {
                                adopted_flag.store(true, std::sync::atomic::Ordering::SeqCst);
                            }
                            break
                        }
                        Ok(_) => {},
                        Err(_) => { failed += 1; break; },
                    }
                }
            }
            Ok(json!({ "readback_required": true, "failed": failed }))
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
}
