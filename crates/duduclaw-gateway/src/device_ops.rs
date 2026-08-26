//! Shell-out abstraction for the appliance `device.*` dashboard RPCs
//! (registered in `handlers.rs`).
//!
//! Every process spawn behind the `device.*` RPC surface goes through
//! [`DeviceOps`] — RPC handlers never spawn a subprocess directly. Two
//! consequences fall out of that:
//!
//! 1. There is exactly ONE place command argv is decided, and every argv is
//!    a hardcoded literal (`Command::new("systemctl").arg("reboot")`, never
//!    built by concatenating caller-supplied strings). RPC params are used
//!    only as path/version *arguments* passed through `Command::arg()` —
//!    never interpreted by a shell, so there is no injection surface
//!    regardless of their content.
//! 2. Tests can swap in a mock implementing the same trait instead of
//!    touching a real systemd — see `MockDeviceOps` below.
//!
//! `SystemDeviceOps` targets the appliance image specifically (Debian +
//! systemd): `systemctl`, `systemd-sysupdate`, `tar`. It compiles on every
//! platform (so the crate still builds for the desktop/Windows targets) but
//! is only ever reached in production behind the `is_appliance()` gate in
//! `handlers.rs`, which the real appliance image is Linux-only.

use std::path::Path;
use std::process::Output;

use async_trait::async_trait;

/// Outcome of a single whitelisted shell-out that ran to completion (spawned
/// successfully; may still have exited non-zero — that's a normal `Ok` with
/// `success: false`, not an `Err`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl OpOutput {
    fn from_process_output(out: Output) -> Self {
        OpOutput {
            success: out.status.success(),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        }
    }
}

/// A structured, caller-facing reason a `device.*` op could not run — never
/// a raw process error string forwarded verbatim, so the dashboard can
/// render zh-TW copy and branch on a stable shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceOpError {
    /// The underlying command (systemctl / systemd-sysupdate / tar / ...)
    /// could not even be spawned — not installed on this host (or, for
    /// `update_rollback`, deliberately not implemented this round — see
    /// that method's doc comment).
    Unsupported(String),
    /// Filesystem error building the op (e.g. couldn't wipe/recreate the
    /// home directory for a factory reset, or stage a backup archive).
    Io(String),
}

impl std::fmt::Display for DeviceOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceOpError::Unsupported(msg) => write!(f, "unsupported: {msg}"),
            DeviceOpError::Io(msg) => write!(f, "io error: {msg}"),
        }
    }
}

pub type OpResult = Result<OpOutput, DeviceOpError>;

/// Device management operations behind the `device.*` RPC surface. Every
/// method is a single whitelisted shell-out (or, for `factory_reset`, a
/// filesystem wipe followed by two whitelisted shell-outs) — never a
/// caller-shaped command line.
#[async_trait]
pub trait DeviceOps: Send + Sync {
    /// `systemctl reboot`.
    async fn reboot(&self) -> OpResult;
    /// `systemctl poweroff`.
    async fn poweroff(&self) -> OpResult;
    /// `systemd-sysupdate list --json=short` — raw JSON forwarded verbatim
    /// in `stdout` (never re-interpreted server-side; see the module doc on
    /// why parsing its schema is deliberately out of scope this round).
    async fn update_status(&self) -> OpResult;
    /// `systemd-sysupdate update` — installs the newest available version.
    async fn update_apply(&self) -> OpResult;
    /// Roll back to the previously-installed A/B slot, then reboot.
    ///
    /// `systemd-sysupdate` has no `rollback` verb, so this is NOT built on
    /// it. It is a *relative* operation — "do not boot the entry I am
    /// running" — carried out on the appliance by `duduclaw-sysd`'s
    /// `UpdateRollback` verb (H3f, 2026-08-24), which never computes a slot
    /// number and therefore cannot pick the wrong one. See that verb's doc
    /// comment for the two tiers (`systemd-bless-boot bad` while a boot
    /// counter is in flight; renaming the entry named by
    /// `LoaderEntrySelected` to the exhausted shape once it is not).
    ///
    /// Off the appliance ([`SystemDeviceOps`]) this still answers
    /// `Unsupported`: a dev box or container has no ESP, no boot counting
    /// and no second slot, and fabricating a success there would be a lie.
    async fn update_rollback(&self) -> OpResult;
    /// `systemd-bless-boot status` — `good` / `bad` / `indeterminate` /
    /// `clean`, forwarded verbatim in `stdout`. Read-only: the dashboard
    /// uses it to say whether the current boot is still under assessment,
    /// without any side effect.
    async fn boot_assessment_status(&self) -> OpResult;
    /// Wipe `home_dir`'s contents, best-effort re-arm the first-boot
    /// provisioning unit, then `systemctl reboot`.
    ///
    /// Also wipes the `duduclaw-kiosk` service user's home directory
    /// (`/data/duduclaw-kiosk`) — which is where the shell's OOBE-completion
    /// flag lives (`shell/oobe_state.json`) — so a factory reset actually
    /// re-runs first-time setup on next boot instead of silently returning
    /// to Home. That half needs root ([`SysdDeviceOps`]'s `FactoryReset`
    /// verb does it) since `home_dir` and the kiosk home are owned by two
    /// different unprivileged users; [`SystemDeviceOps`] (no sysd, no
    /// appliance) has no such directory to wipe at all.
    ///
    /// `clear_network`: when `true`, ALSO clears saved Wi-Fi credentials
    /// under `/data/network/iwd` (D4a-8) — always requires root
    /// ([`SysdDeviceOps`] only; see `SysdRequest::ClearNetworkCredentials`).
    /// Defaults to keeping them: losing Wi-Fi on a headless LAN appliance
    /// can mean permanent physical-access-only recovery, so this is an
    /// explicit opt-in the dashboard confirmation surfaces as a checkbox,
    /// never an implicit side effect of "wipe everything".
    async fn factory_reset(&self, home_dir: &Path, clear_network: bool) -> OpResult;
    /// `tar -czf <dest_path> -C <source_dir> .` — archive everything under
    /// `source_dir` into `dest_path`. Caller is responsible for choosing a
    /// `dest_path` outside `source_dir` (otherwise tar would try to include
    /// the archive it is still writing).
    async fn backup_create(&self, source_dir: &Path, dest_path: &Path) -> OpResult;
}

/// Real implementation. Every `Command::new(...)` argument is a literal;
/// nothing here is ever built from RPC params.
///
/// **Known limitation (N-0, 2026-08)**: the gateway process runs as the
/// unprivileged `duduclaw` service user (see
/// `appliance/mkosi.extra/etc/systemd/system/duduclaw-gateway.service`),
/// not root. `systemctl reboot` / `systemctl poweroff` / `systemd-sysupdate
/// update` / re-enabling the first-boot unit are all privileged operations
/// — on the real appliance image, with no polkit rule granting `duduclaw`
/// an exception, every one of these calls is rejected by systemd/polkit at
/// the OS level (`Interactive authentication required.` or an outright
/// `Access denied`), regardless of what this Rust code does. This was a
/// real gap, not a hypothetical one: WP-B's own residual-risk list did not
/// call it out.
///
/// The fix is privilege separation, not a polkit carve-out: [`SysdDeviceOps`]
/// below talks over a peer-credential-verified Unix domain socket to a
/// separate root daemon (`duduclaw-sysd`, `crates/duduclaw-sysd`) that
/// exposes exactly six fixed verbs. [`select_device_ops`] picks it
/// automatically on an appliance host once the sysd socket exists;
/// `SystemDeviceOps` remains exactly as it was for every non-appliance
/// install (dev machines, desktop, container) where this constructor is
/// still reached directly and none of these methods are expected to need
/// root in the first place.
pub struct SystemDeviceOps;

async fn run(mut cmd: tokio::process::Command) -> OpResult {
    match cmd.output().await {
        Ok(out) => Ok(OpOutput::from_process_output(out)),
        Err(e) => Err(DeviceOpError::Unsupported(e.to_string())),
    }
}

#[async_trait]
impl DeviceOps for SystemDeviceOps {
    async fn reboot(&self) -> OpResult {
        let mut cmd = tokio::process::Command::new("systemctl");
        cmd.arg("reboot");
        run(cmd).await
    }

    async fn poweroff(&self) -> OpResult {
        let mut cmd = tokio::process::Command::new("systemctl");
        cmd.arg("poweroff");
        run(cmd).await
    }

    async fn update_status(&self) -> OpResult {
        let mut cmd = tokio::process::Command::new("systemd-sysupdate");
        cmd.args(["list", "--json=short"]);
        run(cmd).await
    }

    async fn update_apply(&self) -> OpResult {
        let mut cmd = tokio::process::Command::new("systemd-sysupdate");
        cmd.arg("update");
        run(cmd).await
    }

    async fn update_rollback(&self) -> OpResult {
        // Deliberately not "try it and see": this implementation is the
        // non-appliance one (dev machine, container, desktop install).
        // There is no ESP, no A/B slot pair and no boot assessment here, so
        // there is nothing a rollback could mean. The appliance path is
        // `SysdDeviceOps::update_rollback`.
        Err(DeviceOpError::Unsupported(
            "這台裝置不是 DuDuClaw 值班機，沒有可回退的上一個系統版本。".to_string(),
        ))
    }

    async fn boot_assessment_status(&self) -> OpResult {
        Err(DeviceOpError::Unsupported(
            "這台裝置不是 DuDuClaw 值班機，沒有開機評估狀態可查詢。".to_string(),
        ))
    }

    async fn factory_reset(&self, home_dir: &Path, clear_network: bool) -> OpResult {
        if let Err(e) = wipe_dir_contents(home_dir) {
            return Err(DeviceOpError::Io(format!(
                "wiping {} failed: {e}",
                home_dir.display()
            )));
        }
        // `clear_network` and the `/data/duduclaw-kiosk` OOBE-flag wipe are
        // both no-ops here on purpose: this impl runs off-appliance (dev
        // machine, desktop, container) where there is no iwd credential
        // store and no kiosk shell home to speak of — the D4a Wi-Fi feature
        // and the OOBE flow it's asked about are appliance-only concepts.
        // The appliance path is `SysdDeviceOps::factory_reset`, which does
        // both over the privileged socket.
        let _ = clear_network;
        // Best-effort: re-arm first-boot provisioning so the next boot
        // re-seeds device identity + minimal config onto the now-empty home
        // dir. The unit self-disables after a successful first run (see
        // appliance/mkosi.extra/etc/systemd/system/duduclaw-firstboot-provision.service),
        // so without this the wipe alone would leave the box with no config
        // and nothing left to write a new one. Non-fatal: an image without
        // this unit (or a non-appliance test host) should still complete
        // the wipe + reboot rather than abort the whole reset.
        let enable = tokio::process::Command::new("systemctl")
            .args(["enable", "duduclaw-firstboot-provision.service"])
            .output()
            .await;
        let enable_note = match &enable {
            Ok(out) if out.status.success() => String::new(),
            Ok(out) => format!(
                "\n[warn] re-arming first-boot provisioning failed: {}",
                String::from_utf8_lossy(&out.stderr)
            ),
            Err(e) => format!("\n[warn] re-arming first-boot provisioning failed: {e}"),
        };
        let mut reboot_cmd = tokio::process::Command::new("systemctl");
        reboot_cmd.arg("reboot");
        let reboot = run(reboot_cmd).await?;
        Ok(OpOutput {
            stdout: format!("{}{enable_note}", reboot.stdout),
            ..reboot
        })
    }

    async fn backup_create(&self, source_dir: &Path, dest_path: &Path) -> OpResult {
        let mut cmd = tokio::process::Command::new("tar");
        cmd.arg("-czf")
            .arg(dest_path)
            .arg("-C")
            .arg(source_dir)
            .arg(".");
        run(cmd).await
    }
}

/// Privilege-separated implementation — talks to the root `duduclaw-sysd`
/// daemon over its peer-credential-verified Unix domain socket
/// (`crates/duduclaw-sysd`) instead of shelling out directly. Selected
/// automatically by [`select_device_ops`] on an appliance host once the
/// sysd socket exists; see `SystemDeviceOps`'s doc comment for why this
/// exists at all.
///
/// Only the genuinely privileged verbs cross the socket — reboot /
/// poweroff / sysupdate status+apply / boot assessment + rollback / the
/// unit-re-arm-and-reboot half of factory reset. `backup_create` (`tar`)
/// is the one `DeviceOps` method with no sysd verb, because it needs no
/// root: it only reads and writes paths the unprivileged `duduclaw` user
/// already owns, so it is delegated straight to `SystemDeviceOps`'s local
/// shell-out rather than duplicated here.
pub struct SysdDeviceOps {
    client: duduclaw_sysd::SysdClient,
}

impl Default for SysdDeviceOps {
    fn default() -> Self {
        Self { client: duduclaw_sysd::SysdClient::from_env() }
    }
}

impl SysdDeviceOps {
    pub fn new(client: duduclaw_sysd::SysdClient) -> Self {
        Self { client }
    }

    /// Not part of the `DeviceOps` trait — there is no `device.*`
    /// dashboard RPC wired to hostname changes yet (deferred to the L3
    /// "系統設定" app in the design doc's N-3 work package). Exposed as an
    /// inherent method so `duduclaw_sysd::SysdRequest::Hostname` has a
    /// reachable, testable caller in this crate today rather than sitting
    /// completely unused.
    pub async fn set_hostname(&self, name: &str) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::Hostname { set: name.to_string() }).await
    }

    /// System-settings app: `device.timedate_set`'s `timezone` half. Not
    /// part of the `DeviceOps` trait — same reasoning as `set_hostname`
    /// above (a verb specific to one narrow RPC surface, not a shape every
    /// `DeviceOps` implementor needs to answer for).
    pub async fn set_timezone(&self, timezone: &str) -> OpResult {
        sysd_call(
            &self.client,
            duduclaw_sysd::SysdRequest::SetTimezone { timezone: timezone.to_string() },
        )
        .await
    }

    /// System-settings app: `device.timedate_set`'s `ntp` half.
    pub async fn set_ntp(&self, enabled: bool) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::SetNtp { enabled }).await
    }

    /// H3d §11.7 fix: clear a stale, exhausted ESP boot entry for `version`
    /// before installing it — see
    /// `duduclaw_sysd::protocol::SysdRequest::ClearExhaustedUpdateTarget`
    /// for the mechanism and the bug this closes (a manually rolled-back
    /// version could never be reinstalled: its exhausted ESP entry and its
    /// unchanged partition label both already satisfied
    /// `systemd-sysupdate`'s `InstancesMax` accounting, so `update apply`
    /// silently did nothing and still reported success). Not part of the
    /// `DeviceOps` trait — same reasoning as `set_hostname` above: this is a
    /// step specific to the `device.update_apply` flow (see
    /// `handlers.rs::handle_device_update_apply`), not a shape every
    /// `DeviceOps` implementor needs to answer for. Idempotent: a no-op
    /// (`Ok`) when there is nothing stale to clear.
    pub async fn clear_exhausted_update_target(&self, version: &str) -> OpResult {
        sysd_call(
            &self.client,
            duduclaw_sysd::SysdRequest::ClearExhaustedUpdateTarget {
                version: version.to_string(),
            },
        )
        .await
    }

    /// Maintenance-mode Entry A (`commercial/docs/DESIGN-maintenance-mode-2026-08.md`
    /// §2.6): `systemctl start ssh.service`. Not part of the `DeviceOps`
    /// trait — same reasoning as `set_hostname` above: this is a verb
    /// specific to the `maintenance.*` RPC surface (see `maintenance.rs`),
    /// not a shape every `DeviceOps` implementor needs to answer for
    /// (`SystemDeviceOps` has no privileged systemctl path at all).
    pub async fn ssh_start(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::SshServiceStart).await
    }

    /// The close side of [`Self::ssh_start`] — `systemctl stop ssh.service`.
    /// Idempotent (stopping an already-stopped unit is still `success:
    /// true`), which the TTL sweep / gateway-restart reassert-closed path in
    /// `maintenance.rs` relies on: it always calls this, whether or not SSH
    /// was actually running.
    pub async fn ssh_stop(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::SshServiceStop).await
    }

    /// System-settings app: `network.wired_config`. `dns` is cloned into the
    /// request as owned `String`s — the wire shape (`SysdRequest::
    /// NetworkWiredConfig`) takes `Vec<String>`, not a borrowed slice.
    pub async fn network_wired_config(
        &self,
        interface: &str,
        mode: &str,
        address: Option<&str>,
        gateway: Option<&str>,
        dns: &[String],
    ) -> OpResult {
        sysd_call(
            &self.client,
            duduclaw_sysd::SysdRequest::NetworkWiredConfig {
                interface: interface.to_string(),
                mode: mode.to_string(),
                address: address.map(str::to_string),
                gateway: gateway.map(str::to_string),
                dns: dns.to_vec(),
            },
        )
        .await
    }
}

/// Translate a `duduclaw-sysd` call into the same [`OpResult`] shape every
/// other `DeviceOps` impl returns, so callers (handlers.rs) don't need to
/// know which backend answered.
async fn sysd_call(client: &duduclaw_sysd::SysdClient, req: duduclaw_sysd::SysdRequest) -> OpResult {
    match client.call(&req).await {
        Ok(out) => Ok(OpOutput { success: out.success, stdout: out.stdout, stderr: out.stderr }),
        Err(e) => Err(DeviceOpError::Unsupported(e.to_string())),
    }
}

#[async_trait]
impl DeviceOps for SysdDeviceOps {
    async fn reboot(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::Reboot).await
    }

    async fn poweroff(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::Poweroff).await
    }

    async fn update_status(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::SysupdateStatus).await
    }

    async fn update_apply(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::SysupdateApply).await
    }

    async fn update_rollback(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::UpdateRollback).await
    }

    async fn boot_assessment_status(&self) -> OpResult {
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::BootAssessmentStatus).await
    }

    async fn factory_reset(&self, home_dir: &Path, clear_network: bool) -> OpResult {
        // The wipe itself is filesystem-only and needs no root (the
        // `duduclaw` user already owns `home_dir`) — only the unit re-arm
        // + kiosk-home wipe + reboot that follow need root, which is
        // exactly what the `FactoryReset` verb does on the sysd side (it
        // now also wipes `/data/duduclaw-kiosk` — see that verb's doc
        // comment for the OOBE-flag bug this closes).
        if let Err(e) = wipe_dir_contents(home_dir) {
            return Err(DeviceOpError::Io(format!(
                "wiping {} failed: {e}",
                home_dir.display()
            )));
        }
        if clear_network {
            // D4a-8, fail-closed: runs BEFORE the `FactoryReset` verb
            // (which ends in `systemctl reboot`) so there is no race
            // against the box going down mid-request, and a failure here
            // aborts the whole call (`?`) rather than silently proceeding
            // to reboot while leaving saved Wi-Fi credentials in place —
            // an operator who explicitly asked for them to be cleared must
            // never be told "done" when they were not.
            sysd_call(&self.client, duduclaw_sysd::SysdRequest::ClearNetworkCredentials).await?;
        }
        sysd_call(&self.client, duduclaw_sysd::SysdRequest::FactoryReset).await
    }

    async fn backup_create(&self, source_dir: &Path, dest_path: &Path) -> OpResult {
        // `tar` needs no root — delegate to the same local shell-out
        // `SystemDeviceOps` uses rather than duplicate it (or invent a
        // sysd verb for a non-privileged operation).
        SystemDeviceOps.backup_create(source_dir, dest_path).await
    }
}

/// Selects the `DeviceOps` implementation `device.*` RPC handlers should
/// use. Appliance mode + a reachable sysd socket ⇒ [`SysdDeviceOps`] (root
/// operations go through the privilege-separated daemon); everything else
/// — dev machines, desktop/container installs, or an appliance boot where
/// `duduclaw-sysd` hasn't started yet — ⇒ [`SystemDeviceOps`], byte-identical
/// to the behavior before this selector existed.
///
/// The socket-existence check is a cheap, non-blocking `Path::exists()`,
/// not a live connect — a stale socket file left by a dead sysd process
/// would still route here and then fail per-call with a `DeviceOpError`
/// (never a panic), which is an acceptable failure mode for a rare,
/// human-triggered operation and avoids making every `device.*` RPC pay
/// for a connect probe just to decide which backend to use.
pub fn select_device_ops() -> Box<dyn DeviceOps> {
    if duduclaw_core::is_appliance() && duduclaw_sysd::resolve_socket_path().exists() {
        Box::new(SysdDeviceOps::default())
    } else {
        Box::new(SystemDeviceOps)
    }
}

/// Same condition [`select_device_ops`] uses, but for the system-settings
/// app's timedate/network-config verbs — those only exist as
/// `SysdDeviceOps` inherent methods (see `set_hostname`'s doc comment for
/// why they're not on the `DeviceOps` trait), so there is no `SystemDeviceOps`
/// fallback to erase into a trait object: `None` means "no privileged
/// backend reachable", which callers map to a `backend_unavailable` refusal
/// rather than attempting a local shell-out that would need root anyway.
pub fn select_sysd_ops() -> Option<SysdDeviceOps> {
    if duduclaw_core::is_appliance() && duduclaw_sysd::resolve_socket_path().exists() {
        Some(SysdDeviceOps::default())
    } else {
        None
    }
}

/// Remove every entry directly under `dir` (not `dir` itself — preserves
/// ownership/mount-point semantics of the directory entry itself). Missing
/// `dir` is not an error (nothing to wipe).
fn wipe_dir_contents(dir: &Path) -> std::io::Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
pub mod mock {
    //! Test-only mock — lets `device.rs`'s orchestration tests (and any
    //! future handler-level test) exercise every `device.*` code path
    //! without touching a real systemd/tar. Exported (not `#[cfg(test)]`
    //! on the type itself) so it stays usable from `device.rs`'s own test
    //! module, which lives in a different compilation unit's `#[cfg(test)]`
    //! but the same crate.
    use super::*;
    use std::sync::Mutex;

    /// Canned responses + a call log, keyed by method name. Every method
    /// defaults to `Unsupported("not mocked")` unless a response was
    /// queued — fail-closed by default, so a test that forgets to arm a
    /// response gets an obvious failure rather than a silent success.
    #[derive(Default)]
    pub struct MockDeviceOps {
        pub calls: Mutex<Vec<String>>,
        pub reboot_result: Mutex<Option<OpResult>>,
        pub poweroff_result: Mutex<Option<OpResult>>,
        pub update_status_result: Mutex<Option<OpResult>>,
        pub update_apply_result: Mutex<Option<OpResult>>,
        pub update_rollback_result: Mutex<Option<OpResult>>,
        pub boot_assessment_status_result: Mutex<Option<OpResult>>,
        pub factory_reset_result: Mutex<Option<OpResult>>,
        pub backup_create_result: Mutex<Option<OpResult>>,
    }

    fn take_or_default(slot: &Mutex<Option<OpResult>>, label: &str) -> OpResult {
        slot.lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| Err(DeviceOpError::Unsupported(format!("not mocked: {label}"))))
    }

    #[async_trait]
    impl DeviceOps for MockDeviceOps {
        async fn reboot(&self) -> OpResult {
            self.calls.lock().unwrap().push("reboot".to_string());
            take_or_default(&self.reboot_result, "reboot")
        }
        async fn poweroff(&self) -> OpResult {
            self.calls.lock().unwrap().push("poweroff".to_string());
            take_or_default(&self.poweroff_result, "poweroff")
        }
        async fn update_status(&self) -> OpResult {
            self.calls.lock().unwrap().push("update_status".to_string());
            take_or_default(&self.update_status_result, "update_status")
        }
        async fn update_apply(&self) -> OpResult {
            self.calls.lock().unwrap().push("update_apply".to_string());
            take_or_default(&self.update_apply_result, "update_apply")
        }
        async fn update_rollback(&self) -> OpResult {
            self.calls.lock().unwrap().push("update_rollback".to_string());
            take_or_default(&self.update_rollback_result, "update_rollback")
        }
        async fn boot_assessment_status(&self) -> OpResult {
            self.calls
                .lock()
                .unwrap()
                .push("boot_assessment_status".to_string());
            take_or_default(&self.boot_assessment_status_result, "boot_assessment_status")
        }
        async fn factory_reset(&self, _home_dir: &Path, clear_network: bool) -> OpResult {
            self.calls
                .lock()
                .unwrap()
                .push(format!("factory_reset(clear_network={clear_network})"));
            take_or_default(&self.factory_reset_result, "factory_reset")
        }
        async fn backup_create(&self, _source_dir: &Path, _dest_path: &Path) -> OpResult {
            self.calls.lock().unwrap().push("backup_create".to_string());
            take_or_default(&self.backup_create_result, "backup_create")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_output_from_process_output_captures_status_and_streams() {
        // Build a fake `Output` without actually spawning a process.
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let status = std::process::ExitStatus::from_raw(0);
            let out = Output { status, stdout: b"hello".to_vec(), stderr: b"".to_vec() };
            let op = OpOutput::from_process_output(out);
            assert!(op.success);
            assert_eq!(op.stdout, "hello");
            assert_eq!(op.stderr, "");
        }
    }

    #[test]
    fn op_output_from_process_output_nonzero_is_not_success() {
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            let status = std::process::ExitStatus::from_raw(256); // exit code 1
            let out = Output { status, stdout: b"".to_vec(), stderr: b"boom".to_vec() };
            let op = OpOutput::from_process_output(out);
            assert!(!op.success);
            assert_eq!(op.stderr, "boom");
        }
    }

    #[test]
    fn wipe_dir_contents_removes_files_and_subdirs_but_not_dir_itself() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"x").unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), b"y").unwrap();

        wipe_dir_contents(dir.path()).unwrap();

        assert!(dir.path().exists(), "the directory itself must survive");
        let remaining: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(remaining.is_empty(), "contents must be gone: {remaining:?}");
    }

    #[test]
    fn wipe_dir_contents_missing_dir_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        assert!(wipe_dir_contents(&missing).is_ok());
    }

    #[tokio::test]
    async fn update_rollback_never_shells_out_off_the_appliance() {
        // The non-appliance implementation must be a pure Err — no
        // subprocess spawn at all. A dev box has no ESP and no A/B slot
        // pair, so "roll back" has no meaning here and must not be faked.
        // The appliance path is SysdDeviceOps::update_rollback, which
        // crosses the socket to duduclaw-sysd's UpdateRollback verb.
        let ops = SystemDeviceOps;
        assert!(matches!(
            ops.update_rollback().await,
            Err(DeviceOpError::Unsupported(_))
        ));
        assert!(matches!(
            ops.boot_assessment_status().await,
            Err(DeviceOpError::Unsupported(_))
        ));
    }

    #[test]
    fn device_op_error_display_is_stable_shape() {
        let e = DeviceOpError::Unsupported("nope".to_string());
        assert_eq!(e.to_string(), "unsupported: nope");
        let e = DeviceOpError::Io("disk full".to_string());
        assert_eq!(e.to_string(), "io error: disk full");
    }

    // ── N-0: SysdDeviceOps against a real (not mocked) duduclaw-sysd
    // server — spins up the actual `duduclaw_sysd::{bind, serve}` on a
    // tempdir-backed socket path and drives `SysdDeviceOps` at it exactly
    // as the gateway would in production, just pointed at a throwaway
    // socket instead of `/run/duduclaw/sysd.sock`. ─────────────────────
    mod sysd_integration {
        use super::*;
        use std::time::Duration;

        fn current_uid() -> u32 {
            nix::unistd::Uid::current().as_raw()
        }

        struct TestServer {
            socket_path: std::path::PathBuf,
            shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
            handle: Option<tokio::task::JoinHandle<()>>,
        }

        impl TestServer {
            async fn spawn(allowed_uid: Option<u32>) -> Self {
                let dir = tempfile::tempdir().unwrap();
                let socket_path = dir.path().join("sysd.sock");
                std::mem::forget(dir); // keep alive for the socket's lifetime

                let listener = duduclaw_sysd::bind(&socket_path, allowed_uid).unwrap();
                let config = duduclaw_sysd::SysdServerConfig {
                    socket_path: socket_path.clone(),
                    allowed_uid,
                };
                let (tx, rx) = tokio::sync::oneshot::channel::<()>();
                let handle = tokio::spawn(async move {
                    let _ = duduclaw_sysd::serve(listener, config, async {
                        let _ = rx.await;
                    })
                    .await;
                });
                TestServer { socket_path, shutdown_tx: Some(tx), handle: Some(handle) }
            }

            fn device_ops(&self) -> SysdDeviceOps {
                let client = duduclaw_sysd::SysdClient::new(self.socket_path.clone())
                    .with_timeout(Duration::from_secs(5));
                SysdDeviceOps::new(client)
            }

            async fn stop(mut self) {
                if let Some(tx) = self.shutdown_tx.take() {
                    let _ = tx.send(());
                }
                if let Some(h) = self.handle.take() {
                    let _ = h.await;
                }
            }
        }

        /// `reboot`/`poweroff`/`update_status`/`update_apply` all cross the
        /// socket and reach real dispatch when authorized. `systemctl` /
        /// `systemd-sysupdate` do not exist on every CI/dev host (this
        /// crate builds cross-platform), so the assertion is on the
        /// AUTHORIZATION outcome — the op result must never be an
        /// `unauthorized`-flavored rejection for an authorized caller.
        #[tokio::test]
        async fn authorized_calls_reach_real_dispatch_not_auth_rejection() {
            let server = TestServer::spawn(Some(current_uid())).await;
            let ops = server.device_ops();

            for result in [
                ops.reboot().await,
                ops.poweroff().await,
                ops.update_status().await,
                ops.update_apply().await,
            ] {
                match result {
                    Ok(_) => {}
                    Err(DeviceOpError::Unsupported(msg)) => {
                        assert!(
                            !msg.contains("unauthorized"),
                            "authorized caller must not be rejected: {msg}"
                        );
                    }
                    Err(e) => panic!("unexpected error variant: {e}"),
                }
            }

            server.stop().await;
        }

        /// `factory_reset` wipes `home_dir` locally (no root needed) even
        /// though the follow-up unit-re-arm + reboot go over the socket —
        /// this is the split documented on `SysdDeviceOps::factory_reset`.
        #[tokio::test]
        async fn factory_reset_wipes_locally_then_calls_sysd() {
            let server = TestServer::spawn(Some(current_uid())).await;
            let ops = server.device_ops();

            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("leftover.txt"), b"x").unwrap();

            let result = ops.factory_reset(home.path(), false).await;
            // The wipe must have happened regardless of whether the
            // downstream `systemctl` calls exist on this host.
            let remaining: Vec<_> = std::fs::read_dir(home.path()).unwrap().collect();
            assert!(remaining.is_empty(), "home dir must be wiped: {remaining:?}");
            match result {
                Ok(_) => {}
                Err(DeviceOpError::Unsupported(msg)) => {
                    assert!(!msg.contains("unauthorized"), "unexpected rejection: {msg}");
                }
                Err(e) => panic!("unexpected error variant: {e}"),
            }

            server.stop().await;
        }

        /// D4a-8: `clear_network: true` must ALSO reach the
        /// `ClearNetworkCredentials` verb (not just `FactoryReset`) —
        /// asserted the same "authorization outcome, not underlying
        /// filesystem state" way as `authorized_calls_reach_real_dispatch_
        /// not_auth_rejection` above (the real `/data/network/iwd` does not
        /// exist on this dev/CI host, but `wipe_dir_contents` treats a
        /// missing directory as a no-op success, so this reaches real
        /// dispatch and returns `Ok` rather than erroring for an unrelated
        /// filesystem reason).
        #[tokio::test]
        async fn factory_reset_with_clear_network_reaches_real_dispatch() {
            let server = TestServer::spawn(Some(current_uid())).await;
            let ops = server.device_ops();

            let home = tempfile::tempdir().unwrap();
            let result = ops.factory_reset(home.path(), true).await;
            match result {
                Ok(_) => {}
                Err(DeviceOpError::Unsupported(msg)) => {
                    assert!(!msg.contains("unauthorized"), "unexpected rejection: {msg}");
                }
                Err(e) => panic!("unexpected error variant: {e}"),
            }

            server.stop().await;
        }

        /// D4a-8 fail-closed contract: when `clear_network: true` and the
        /// sysd socket is unreachable, `factory_reset` must surface an
        /// `Err` — never silently proceed to a fabricated "Ok" while saved
        /// Wi-Fi credentials were never actually cleared. `home_dir`'s own
        /// (unprivileged, local) wipe still happens first and must succeed
        /// regardless — only the privileged half is unreachable here.
        #[tokio::test]
        async fn factory_reset_with_clear_network_fails_closed_when_socket_unreachable() {
            let ops = SysdDeviceOps::new(duduclaw_sysd::SysdClient::new(
                std::path::PathBuf::from("/tmp/duduclaw-sysd-test-no-such-socket.sock"),
            ));

            let home = tempfile::tempdir().unwrap();
            std::fs::write(home.path().join("leftover.txt"), b"x").unwrap();

            let result = ops.factory_reset(home.path(), true).await;

            let remaining: Vec<_> = std::fs::read_dir(home.path()).unwrap().collect();
            assert!(
                remaining.is_empty(),
                "the local home_dir wipe must still have happened: {remaining:?}"
            );
            assert!(
                matches!(result, Err(DeviceOpError::Unsupported(_))),
                "an unreachable socket must surface as Err, never a fabricated success: {result:?}"
            );
        }

        /// `set_hostname` (the `Hostname { set }` verb) round-trips a
        /// caller-supplied value across the socket.
        #[tokio::test]
        async fn set_hostname_reaches_real_dispatch() {
            let server = TestServer::spawn(Some(current_uid())).await;
            let ops = server.device_ops();

            let result = ops.set_hostname("duty-box-test").await;
            match result {
                Ok(_) => {}
                Err(DeviceOpError::Unsupported(msg)) => {
                    assert!(!msg.contains("unauthorized"), "unexpected rejection: {msg}");
                }
                Err(e) => panic!("unexpected error variant: {e}"),
            }

            server.stop().await;
        }

        /// H3d §11.7: `clear_exhausted_update_target` (the
        /// `ClearExhaustedUpdateTarget { version }` verb) reaches real
        /// dispatch when authorized — same "authorization outcome, not
        /// underlying-ESP-availability" assertion as the other verbs in
        /// this module (this dev/CI host has no EFI System Partition at
        /// all, so sysd's own `Unsupported("cannot locate the ESP")` is an
        /// expected, non-authorization failure here).
        #[tokio::test]
        async fn clear_exhausted_update_target_reaches_real_dispatch() {
            let server = TestServer::spawn(Some(current_uid())).await;
            let ops = server.device_ops();

            let result = ops.clear_exhausted_update_target("0.2.0").await;
            match result {
                Ok(_) => {}
                Err(DeviceOpError::Unsupported(msg)) => {
                    assert!(
                        !msg.contains("unauthorized"),
                        "authorized caller must not be rejected: {msg}"
                    );
                }
                Err(e) => panic!("unexpected error variant: {e}"),
            }

            server.stop().await;
        }

        /// A malformed version value is rejected as `bad_request` by sysd
        /// itself, surfaced here as a structured `Unsupported` (never a
        /// panic, never silently accepted) — mirrors the sysd-crate-level
        /// `clear_exhausted_update_target_rejects_bad_version_without_touching_the_esp`
        /// test, but through the gateway's own client call path.
        #[tokio::test]
        async fn clear_exhausted_update_target_rejects_bad_version() {
            let server = TestServer::spawn(Some(current_uid())).await;
            let ops = server.device_ops();

            let result = ops.clear_exhausted_update_target("../../etc/passwd").await;
            match result {
                Err(DeviceOpError::Unsupported(msg)) => {
                    assert!(
                        msg.contains("bad_request"),
                        "expected a bad_request rejection, got: {msg}"
                    );
                }
                other => panic!("expected a bad_request rejection, got {other:?}"),
            }

            server.stop().await;
        }

        /// System-settings app: `set_timezone`/`set_ntp`/`network_wired_config`
        /// (the three new inherent `SysdDeviceOps` methods) all reach real
        /// dispatch when authorized — same "authorization outcome, not
        /// underlying-command success" assertion as
        /// `authorized_calls_reach_real_dispatch_not_auth_rejection` above
        /// (sysd itself may refuse a bogus timezone with `bad_request`, or
        /// this host may not run `systemd-networkd` at all; what must never
        /// happen for an authorized caller is an `unauthorized` rejection).
        #[tokio::test]
        async fn timedate_and_wired_config_verbs_reach_real_dispatch() {
            let server = TestServer::spawn(Some(current_uid())).await;
            let ops = server.device_ops();

            for result in [
                ops.set_timezone("Asia/Taipei").await,
                ops.set_ntp(true).await,
                ops.network_wired_config(
                    "enp1s0",
                    "static",
                    Some("192.168.1.50/24"),
                    Some("192.168.1.1"),
                    &["1.1.1.1".to_string()],
                )
                .await,
            ] {
                match result {
                    Ok(_) => {}
                    Err(DeviceOpError::Unsupported(msg)) => {
                        assert!(
                            !msg.contains("unauthorized"),
                            "authorized caller must not be rejected: {msg}"
                        );
                    }
                    Err(e) => panic!("unexpected error variant: {e}"),
                }
            }

            server.stop().await;
        }

        /// `select_sysd_ops()` mirrors `select_device_ops()`'s condition —
        /// off-appliance (this test process's default state) it must be
        /// `None` regardless of whether a real socket happens to exist.
        #[test]
        fn select_sysd_ops_is_none_off_appliance() {
            assert!(!duduclaw_core::is_appliance());
            assert!(select_sysd_ops().is_none());
        }

        /// The security property that matters most: a peer whose real uid
        /// doesn't match the server's configured `allowed_uid` is denied,
        /// surfaced through `SysdDeviceOps` as a normal `DeviceOpError`
        /// (never a panic). `current_uid() + 1` stands in for "some other
        /// uid" — see `duduclaw-sysd`'s own tests for why a real different
        /// uid can't be used here without root.
        #[tokio::test]
        async fn uid_mismatch_surfaces_as_device_op_error() {
            let wrong_uid = current_uid().wrapping_add(1);
            let server = TestServer::spawn(Some(wrong_uid)).await;
            let ops = server.device_ops();

            let result = ops.reboot().await;
            match result {
                Err(DeviceOpError::Unsupported(msg)) => {
                    assert!(msg.contains("unauthorized"), "expected unauthorized, got: {msg}");
                }
                other => panic!("expected an Unsupported(unauthorized) error, got {other:?}"),
            }

            server.stop().await;
        }

        /// `update_rollback` and `backup_create` never touch the socket at
        /// all (no sysd verb exists for either — see the impl's doc
        /// comment) — confirm both still work even with NO server running.
        #[tokio::test]
        async fn backup_create_never_needs_the_socket() {
            // `backup_create` is the ONE DeviceOps method with no sysd verb,
            // because tar needs no root — it only touches paths the
            // unprivileged `duduclaw` user already owns. It must therefore
            // work with no sysd running at all.
            //
            // `update_rollback` used to be asserted here too, back when it
            // was a pure `Err` that never opened the socket. Since H3f it
            // crosses to the `UpdateRollback` verb, so that half moved to
            // `update_rollback_surfaces_a_missing_socket_honestly` below —
            // leaving it here would have kept passing while asserting the
            // opposite of what the code does.
            let ops = SysdDeviceOps::new(duduclaw_sysd::SysdClient::new(
                std::path::PathBuf::from("/tmp/duduclaw-sysd-test-no-such-socket.sock"),
            ));

            let src = tempfile::tempdir().unwrap();
            std::fs::write(src.path().join("a.txt"), b"hi").unwrap();
            let dest = tempfile::tempdir().unwrap();
            let dest_path = dest.path().join("out.tar.gz");
            let backup = ops.backup_create(src.path(), &dest_path).await;
            assert!(backup.is_ok(), "backup_create must not require the sysd socket: {backup:?}");
        }

        #[tokio::test]
        async fn update_rollback_surfaces_a_missing_socket_honestly() {
            // With no sysd reachable, the appliance path must report a
            // structured `Unsupported` — never panic, and never fall back to
            // pretending a rollback happened.
            let ops = SysdDeviceOps::new(duduclaw_sysd::SysdClient::new(
                std::path::PathBuf::from("/tmp/duduclaw-sysd-test-no-such-socket.sock"),
            ));
            assert!(matches!(
                ops.update_rollback().await,
                Err(DeviceOpError::Unsupported(_))
            ));
            assert!(matches!(
                ops.boot_assessment_status().await,
                Err(DeviceOpError::Unsupported(_))
            ));
        }
    }
}
