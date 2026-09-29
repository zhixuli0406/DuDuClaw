//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Outcome of [`create_device_backup_archive`] — every branch the original
/// `device.backup_create` RPC handler could reach, factored out of
/// `MethodHandler::handle_device_backup_create` (now a thin match over this)
/// so the O-0 `os_backup_create` MCP tool (`duduclaw-cli::mcp`) reuses the
/// EXACT same archive-then-stage logic instead of re-deriving it — avoiding
/// the two-implementation drift the O-0 design explicitly warns against.
pub enum DeviceBackupOutcome {
    /// Archive built and moved into the attachments dir.
    Created {
        filename: String,
        stdout: String,
        stderr: String,
    },
    /// The op ran (spawned) but reported failure (`OpOutput::success == false`).
    OpFailed(crate::device_ops::OpOutput),
    /// The op itself could not run (see [`crate::device_ops::DeviceOpError`]).
    OpError(crate::device_ops::DeviceOpError),
    /// Archive succeeded but the staging→attachments move failed. Carries a
    /// ready-to-display zh-TW message, byte-identical to the original inline
    /// error strings.
    MoveFailed(String),
}

/// `device.backup_create`'s data-gathering + orchestration half (dispatch
/// gates — admin / appliance — live in `dispatch()`'s method match; this is
/// the part both the dashboard RPC and the agent-facing `os_backup_create`
/// MCP tool share). See the original handler's doc comment (now on
/// [`MethodHandler::handle_device_backup_create`]) for the staging-path
/// rationale: the archive is built OUTSIDE the source tree first so tar-ing
/// the writable data partition never tries to include the archive it is
/// still writing.
pub async fn create_device_backup_archive(home_dir: &Path) -> DeviceBackupOutcome {
    let home = home_dir.to_path_buf();
    let source_dir = home
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| home.clone());
    let filename = format!(
        "device-backup-{}.tar.gz",
        Utc::now().format("%Y%m%dT%H%M%SZ")
    );
    let staging = std::env::temp_dir().join(format!("duduclaw-{filename}"));

    let result = crate::device_ops::select_device_ops()
        .backup_create(&source_dir, &staging)
        .await;
    let out = match result {
        Ok(out) if out.success => out,
        Ok(out) => {
            let _ = std::fs::remove_file(&staging);
            return DeviceBackupOutcome::OpFailed(out);
        }
        Err(e) => return DeviceBackupOutcome::OpError(e),
    };

    let Some(dest_dir) = crate::files_api::attachments_dir(&home, None) else {
        let _ = std::fs::remove_file(&staging);
        return DeviceBackupOutcome::MoveFailed("無法解析備份存放目錄".to_string());
    };
    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
        let _ = std::fs::remove_file(&staging);
        return DeviceBackupOutcome::MoveFailed(format!("建立備份目錄失敗: {e}"));
    }
    let dest_path = dest_dir.join(&filename);
    if let Err(e) = std::fs::rename(&staging, &dest_path) {
        // Cross-device rename (e.g. /tmp on a different filesystem than
        // the attachments dir) falls back to copy+remove.
        if let Err(e2) = std::fs::copy(&staging, &dest_path) {
            let _ = std::fs::remove_file(&staging);
            return DeviceBackupOutcome::MoveFailed(format!(
                "搬移備份檔失敗: rename={e} copy={e2}"
            ));
        }
        let _ = std::fs::remove_file(&staging);
    }

    DeviceBackupOutcome::Created {
        filename,
        stdout: out.stdout,
        stderr: out.stderr,
    }
}

/// Outcome of [`stage_and_apply_device_update`] — every branch the original
/// `device.update_apply` RPC handler could reach, factored out of
/// `MethodHandler::handle_device_update_apply` (now a thin match over this,
/// same pattern as [`DeviceBackupOutcome`]/[`create_device_backup_archive`]
/// above) so the O-0 `os_apply_update` MCP tool
/// (`duduclaw-cli::mcp_os_ops::handle_os_apply_update`) reuses the EXACT same
/// verify→stage→backup→ESP-clear→install→confirm-slot→cleanup pipeline
/// instead of calling the bare `device_ops::update_apply()` sysupdate wrapper
/// directly.
///
/// Y5-3 (agent-body update vertical slice) found and fixed a real gap here:
/// before this extraction, `os_apply_update(target="device")` called ONLY
/// `device_ops::update_apply()` — skipping the H3d manifest signature
/// verification, the pre-update `/data` snapshot, the stale-ESP-entry clear,
/// AND the post-install slot-mismatch confirmation entirely. Per this
/// module's own doc comment on `handle_device_update_apply` (below), staging
/// is "the only thing standing between a payload and the boot chain" on this
/// appliance's `Type=regular-file` sysupdate source — an agent-triggered
/// device update had strictly weaker safety properties than a
/// dashboard-triggered one, violating the O-0 design's core invariant
/// ("同一套能力，兩種前門，同一組閘", `DESIGN-agent-os-native-apps-2026-08.md`
/// §6.1). This function is the fix: one implementation, two callers.
pub enum DeviceUpdateApplyOutcome {
    /// Verification/staging failed before touching the boot chain at all.
    StageFailed(crate::os_update::StageError),
    /// The ESP could not be prepared for the staged version — install
    /// aborted, device untouched. Carries a ready-to-display zh-TW message.
    EspPrepareFailed(String),
    /// `sysupdate` ran; success or failure, exactly the shape
    /// `crate::os_ops`'s `{success, stdout, stderr}` payload renders (and,
    /// on this surface, `device_op_result_frame`).
    Applied(crate::device_ops::OpResult),
    /// `sysupdate` reported success, but the installed slot didn't match
    /// what was staged — a failure even though the op itself "succeeded".
    /// Carries a ready-to-display zh-TW message.
    SlotMismatch(String),
}

/// `device.update_apply`'s verify→stage→backup→ESP-clear→install→
/// confirm-slot→cleanup pipeline — the part both the dashboard RPC and the
/// agent-facing `os_apply_update` MCP tool share. See
/// [`DeviceUpdateApplyOutcome`]'s doc comment for why this was extracted.
/// Every step here is a free function taking only `home_dir`/plain args —
/// none of it touches `MethodHandler`'s in-memory state — so it is exactly as
/// reachable from the separate `duduclaw mcp-server` process as
/// [`create_device_backup_archive`] already is.
pub async fn stage_and_apply_device_update(home_dir: &Path) -> DeviceUpdateApplyOutcome {
    let staged = match crate::os_update::stage_update(home_dir).await {
        Ok(report) => {
            tracing::info!(
                "[stage_and_apply_device_update] staged {} for {} ({} bytes)",
                report.version,
                report.destination_partuuid,
                report.bytes_downloaded
            );
            report
        }
        Err(e) => return DeviceUpdateApplyOutcome::StageFailed(e),
    };

    // H3d §11.5 item 2: best-effort pre-update /data snapshot — never blocks
    // the update itself (see `pre_update_backup` module doc for the
    // "automatic, narrow, best-effort" reasoning). A failure here is logged
    // and the flow continues exactly as before this step existed.
    match crate::pre_update_backup::snapshot_before_update(home_dir, &staged.version) {
        Ok(report) => tracing::info!(
            "[stage_and_apply_device_update] pre-update snapshot: {} file(s), {} bytes, in {}",
            report.files_copied,
            report.bytes_copied,
            report.dir.display()
        ),
        Err(e) => tracing::warn!(
            "[stage_and_apply_device_update] pre-update snapshot failed (continuing with the update): {e}"
        ),
    }

    // H3d §11.7: clear a stale exhausted ESP entry for the version we are
    // about to install, BEFORE calling sysupdate. Without this, a version
    // that was manually rolled back (device.update_rollback's tier 2) can
    // never be reinstalled: its exhausted ESP entry and its unchanged
    // partition label both already satisfy systemd-sysupdate's InstancesMax
    // accounting, so `update apply` silently writes nothing and still
    // reports success — "rolled back once, uninstallable forever, and lies
    // about it." Idempotent (no-op when there is nothing stale), so this
    // runs unconditionally rather than only after a detected rollback.
    //
    // Off-appliance (no sysd reachable) there is no ESP at all —
    // `select_sysd_ops()` is `None` and this step is skipped entirely;
    // `update_apply()` below degrades the same way it always has there.
    // On-appliance, a genuine failure here is treated as fatal to the whole
    // apply rather than best-effort: proceeding anyway risks reproducing the
    // exact "reports success but did nothing" bug this step exists to close.
    if let Some(sysd) = crate::device_ops::select_sysd_ops() {
        if let Err(e) = sysd.clear_exhausted_update_target(&staged.version).await {
            tracing::error!(
                "[stage_and_apply_device_update] could not prepare the ESP for {}: {e}",
                staged.version
            );
            return DeviceUpdateApplyOutcome::EspPrepareFailed(format!(
                "更新檔已驗證，但清理舊開機項目失敗，安裝已中止（裝置未被更動）：{e}"
            ));
        }
    }

    let applied = crate::device_ops::select_device_ops().update_apply().await;
    // Only when sysupdate actually succeeded: confirm from the live GPT that
    // it wrote the slot the kernel image was bound to, and reclaim the
    // ~4 GiB of payload now that the partition label is the ledger. A
    // mismatch is surfaced as a failure even though the install "worked" —
    // rebooting into a kernel/root pair from two different versions is the
    // failure this whole package exists to prevent.
    if matches!(&applied, Ok(out) if out.success) {
        if let Err(why) = crate::os_update::confirm_installed_slot(&staged).await {
            tracing::error!("[stage_and_apply_device_update] slot mismatch after install: {why}");
            return DeviceUpdateApplyOutcome::SlotMismatch(format!(
                "更新已寫入，但寫入的位置與預期不符，請勿重新開機並聯絡技術支援：{why}"
            ));
        }
        crate::os_update::cleanup_staged(&staged);
        // B5 (OS security line P0): a device-target apply previously left NO
        // trace in `security_audit.jsonl` at all — this is the fix. `"device"`
        // is a fixed actor sentinel: this shared pipeline function (dashboard
        // RPC + the `os_apply_update` MCP tool both call it, see the doc
        // comment above) intentionally has no agent-identity parameter, so
        // there is no caller identity to attribute this to more precisely.
        // Fired only here — i.e. sysupdate succeeded AND the installed slot
        // was confirmed to match what was staged — NOT the same moment as
        // "the new slot booted successfully" (that is `log_os_update_blessed`/
        // `log_os_rollback_detected`, recorded on a LATER boot by
        // `update_report_reconcile.rs`).
        duduclaw_security::audit::log_os_update_applied(
            home_dir,
            "device",
            "device",
            &staged.version,
        );
        // C1 producer 甲 companion — see `security_autopilot.rs`.
        crate::security_autopilot::emit_os_update_applied("device");
    }
    DeviceUpdateApplyOutcome::Applied(applied)
}
