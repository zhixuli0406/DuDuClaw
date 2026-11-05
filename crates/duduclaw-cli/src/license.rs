//! `duduclaw license` subcommand — client-side license management.
//!
//! Wraps the OSS `duduclaw-license` crate with a friendly CLI surface:
//!
//!   activate <key>      Install a license key into ~/.duduclaw/license.json
//!   status              Show the active license tier, expiry, and gating
//!   refresh             Phone home to control-plane
//!   export              Print the current license as JSON (for transfer)
//!   import <path>       Import a license JSON file from disk
//!   deactivate          Remove the active license (revert to OpenSource)
//!   fingerprint         Print this machine's fingerprint
//!
//! Design notes:
//!
//! - When no license is installed the user is in OpenSource mode and the
//!   CLI must never error out. Most subcommands degrade gracefully.
//! - `activate` accepts the license either as a base64-encoded blob, a
//!   raw JSON string, or a path to a JSON file. We auto-detect.

use std::fs;
use std::path::PathBuf;

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use clap::Subcommand;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_license::{
    EMBEDDED_FEATURES_TOML, FeatureGate, FingerprintCandidate, FingerprintStrength, License, LicenseError, LicenseTier,
    fingerprint_candidates, fingerprint_is_hostname_only, generate_fingerprint, load_default,
    save_default, storage,
};

#[derive(Subcommand)]
pub enum LicenseCommands {
    /// Install a license key into `~/.duduclaw/license.json`.
    ///
    /// The argument can be:
    ///   - a base64-encoded license blob (one line),
    ///   - the raw JSON contents of a license,
    ///   - or a path to a license JSON file.
    ///
    /// Auto-detected — base64 first, then JSON, then path.
    Activate {
        /// License key (base64 / JSON / path)
        key: String,
    },

    /// Show the active license tier, expiry, and which commercial modules
    /// are unlocked. Always succeeds — falls back to OpenSource mode if
    /// no license is installed.
    Status,

    /// Phone home to the DuDuClaw control-plane to refresh the license
    /// and reset the grace-period timer.
    ///
    /// A changed machine binding requires account-holder approval in the console.
    Refresh,

    /// Print the current license to stdout for transfer to another machine.
    Export {
        /// Wrap output in base64 (otherwise prints raw JSON).
        #[arg(long)]
        base64: bool,
    },

    /// Import a license JSON file. The new license overwrites whatever is
    /// currently installed (the old one is preserved as license.json.bak).
    Import {
        /// Path to the license JSON file
        path: PathBuf,
    },

    /// Remove the active license, reverting to OpenSource mode.
    /// Does not delete any other DuDuClaw data.
    Deactivate,

    /// Request moving the installed license to this machine. The account holder
    /// must approve the rebind in the console before it takes effect.
    Rebind,

    /// Show this machine's subscription status from the control-plane
    /// (tier / status / days until renewal).
    Subscriptions,

    /// Print this machine's fingerprint, for issuing a new license.
    Fingerprint {
        /// Also list historical fingerprints for diagnosis, including weak
        /// values that cannot activate a paid tier. The first line is the current
        /// fingerprint, so scripts can keep reading line 1.
        #[arg(long)]
        all: bool,
    },
}

// ── Public entry point ────────────────────────────────────────

pub async fn run(cmd: LicenseCommands) -> Result<()> {
    if should_refuse_in_ai_session(&cmd, &crate::ai_session_guard::command_markers()) {
        return Err(DuDuClawError::Agent(
            "這個指令不能在 AI 員工的工作階段中執行；請由管理者在自己的終端機執行。".into(),
        ));
    }
    match cmd {
        LicenseCommands::Activate { key } => cmd_activate(&key).await,
        LicenseCommands::Status => cmd_status().await,
        LicenseCommands::Refresh => cmd_refresh().await,
        LicenseCommands::Export { base64 } => cmd_export(base64).await,
        LicenseCommands::Import { path } => cmd_import(&path).await,
        LicenseCommands::Deactivate => cmd_deactivate().await,
        LicenseCommands::Rebind => cmd_rebind().await,
        LicenseCommands::Subscriptions => cmd_subscriptions().await,
        LicenseCommands::Fingerprint { all } => cmd_fingerprint(all).await,
    }
}

fn should_refuse_in_ai_session(cmd: &LicenseCommands, markers: &[&str]) -> bool {
    command_writes_state(cmd) && !markers.is_empty()
}

fn command_writes_state(cmd: &LicenseCommands) -> bool {
    matches!(
        cmd,
        LicenseCommands::Activate { .. }
            | LicenseCommands::Refresh
            | LicenseCommands::Import { .. }
            | LicenseCommands::Deactivate
            | LicenseCommands::Rebind
    )
}

/// Commands blocked by the agent-file-guard Bash lane. Read-only license
/// inspection remains available to employees.
pub const OPERATOR_COMMANDS: &[duduclaw_core::OperatorCommand] = &[duduclaw_core::OperatorCommand {
    name: "duduclaw license state change",
    path: &["license", "activate"],
}, duduclaw_core::OperatorCommand {
    name: "duduclaw license state change",
    path: &["license", "refresh"],
}, duduclaw_core::OperatorCommand {
    name: "duduclaw license state change",
    path: &["license", "import"],
}, duduclaw_core::OperatorCommand {
    name: "duduclaw license state change",
    path: &["license", "deactivate"],
}, duduclaw_core::OperatorCommand {
    name: "duduclaw license state change",
    path: &["license", "rebind"],
}];

pub fn bash_license_decision(
    command: &str,
    caller: &duduclaw_core::HookCaller,
) -> Option<duduclaw_core::GuardDecision> {
    duduclaw_core::bash_operator_command_decision(command, caller, OPERATOR_COMMANDS)
}

/// Resolve the control-plane base URL (override with `DUDUCLAW_CONTROL_URL`).
/// Used by machine rebinding and subscription status requests.
fn control_url() -> String {
    resolve_control_url(None)
}

/// §10.5 control-plane URL resolution: `DUDUCLAW_CONTROL_URL` env >
/// self-carried `license.control_url` > baked default. A blank env value is
/// treated as unset. Shared by `refresh` (which passes the license URL) and the
/// no-license flows (which pass `None`).
fn resolve_control_url(license_control_url: Option<&str>) -> String {
    if let Ok(env_url) = std::env::var("DUDUCLAW_CONTROL_URL") {
        let trimmed = env_url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(url) = license_control_url {
        let trimmed = url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    DEFAULT_CONTROL_URL.to_string()
}

fn validated_control_endpoint(endpoint: &str) -> Result<reqwest::Url> {
    let operator_override = std::env::var("DUDUCLAW_CONTROL_URL").ok();
    duduclaw_license::control_url::validate_control_url(endpoint, operator_override.as_deref())
        .map_err(|error| DuDuClawError::License(error.into()))
}

async fn control_request(endpoint: &str) -> Result<reqwest::RequestBuilder> {
    duduclaw_gateway::license_transport::request(reqwest::Method::POST, endpoint,
        std::time::Duration::from_secs(20)).await.map_err(DuDuClawError::License)
}

/// Extract a signed `License` from a `{ "license": {...} }` control-plane
/// response and install it locally after an authorized machine rebind.
fn install_license_from_envelope(envelope: &serde_json::Value, installed: &License, expected_fingerprint: &str, nonce: &str,
    path: &std::path::Path, snapshot: &storage::LicenseSnapshot) -> Result<License> {
    let value = envelope
        .get("license")
        .ok_or_else(|| DuDuClawError::License("response missing 'license'".into()))?;
    let license: License = serde_json::from_value(value.clone())
        .map_err(|e| DuDuClawError::License(format!("parse license: {e}")))?;
    duduclaw_license::refresh::validate_response(
        &duduclaw_gateway::license_runtime::production_registry(), &license, envelope,
        nonce, &installed.subscription_id, &installed.machine_fingerprint, expected_fingerprint,
    ).map_err(|e| DuDuClawError::License(format!("rebind response rejected; installed license unchanged: {e}")))?;
    validate_control_plane_license(&license)?;
    if license.machine_fingerprint != expected_fingerprint {
        return Err(DuDuClawError::License("rebind response did not use the requested fingerprint; installed license unchanged".into()));
    }
    save_response_if_current(path, snapshot, &license)?;
    Ok(license)
}

/// A rejected refresh/rebind response must never replace the installed license.
fn validate_control_plane_license(license: &License) -> Result<()> {
    let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML)
        .map_err(|e| DuDuClawError::License(format!("embedded features.toml is broken: {e}")))?;
    duduclaw_gateway::license_runtime::production_registry().verify(license)
        .and_then(|_| license.validate_any(
            &fingerprint_candidates(), gate.phone_home_interval_days(license.tier),
            gate.grace_period_days(license.tier),
        ).map(|_| ()))
        .and_then(|_| license.validate_tier_deployment_binding(
            duduclaw_gateway::license_runtime::is_self_host_deployment(),
        ))
        .map_err(|e| DuDuClawError::License(format!("control-plane license rejected; installed license unchanged: {e}")))
}

/// Hardware-backed legacy bindings remain usable. Weak matches are diagnostic
/// only and never enable a paid tier.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Binding {
    Strong,
    Legacy(String),
    Weak,
    None,
}

fn classify_binding(license_fp: &str, candidates: &[FingerprintCandidate]) -> Binding {
    match candidates.iter().enumerate().find(|(_, c)| c.fingerprint() == license_fp) {
        Some((_, candidate)) if candidate.strength() == FingerprintStrength::Weak => Binding::Weak,
        Some((0, _)) => Binding::Strong,
        Some(_) => Binding::Legacy(license_fp.to_string()),
        None => Binding::None,
    }
}

const REBIND_PENDING_MESSAGE: &str = "需要帳號持有人到控制台核准換綁；原授權未變更。";

fn response_pending(status: reqwest::StatusCode, envelope: &serde_json::Value) -> bool {
    status == reqwest::StatusCode::ACCEPTED
        || envelope.get("status").and_then(|v| v.as_str()) == Some("pending")
}

fn load_request_snapshot(path: &std::path::Path) -> duduclaw_license::Result<(License, storage::LicenseSnapshot)> {
    let store = storage::LockedLicenseStore::acquire(path)?;
    let license = store.load()?;
    let snapshot = store.snapshot()?;
    Ok((license, snapshot))
}

fn current_response_store(path: &std::path::Path, snapshot: &storage::LicenseSnapshot) -> Result<storage::LockedLicenseStore> {
    let store = storage::LockedLicenseStore::acquire(path)
        .map_err(|e| DuDuClawError::License(format!("lock license: {e}")))?;
    if !store.matches(snapshot).map_err(|e| DuDuClawError::License(format!("read license state: {e}")))? {
        return Err(DuDuClawError::License(
            "license changed while request was in flight; stale response discarded".into(),
        ));
    }
    Ok(store)
}

fn save_response_if_current(path: &std::path::Path, snapshot: &storage::LicenseSnapshot, license: &License) -> Result<()> {
    current_response_store(path, snapshot)?.save(license)
        .map_err(|e| DuDuClawError::License(format!("save license: {e}")))
}

fn validate_status_response(envelope: &serde_json::Value, expected_status: &str,
    installed: &License, proposed: &str, nonce: &str) -> Result<()> {
    duduclaw_license::refresh::validate_status_response(
        &duduclaw_gateway::license_runtime::production_registry(), envelope, expected_status,
        nonce, &installed.subscription_id, &installed.machine_fingerprint, proposed,
    ).map_err(|e| DuDuClawError::License(format!("response rejected; installed license unchanged: {e}")))
}

#[allow(clippy::too_many_arguments)]
fn apply_revoked_response(path: &std::path::Path, snapshot: &storage::LicenseSnapshot,
    registry: &duduclaw_license::PublicKeyRegistry, envelope: &serde_json::Value,
    installed: &License, proposed: &str, nonce: &str) -> Result<()> {
    duduclaw_license::refresh::validate_status_response(registry, envelope, "revoked", nonce,
        &installed.subscription_id, &installed.machine_fingerprint, proposed)
        .map_err(|e| DuDuClawError::License(format!("revocation rejected; installed license unchanged: {e}")))?;
    let reason = envelope.get("reason").and_then(|v| v.as_str())
        .ok_or_else(|| DuDuClawError::License("revocation reason missing".into()))?;
    current_response_store(path, snapshot)?.revoke(reason)
        .map_err(|e| DuDuClawError::License(format!("persist revocation: {e}")))
}

/// Warn (on stderr, so stdout stays machine-readable) when this host could not
/// produce a hardware-bound fingerprint at all.
///
/// Saying so out loud beats letting a weak binding pass for a strong one.
fn warn_if_hostname_only() {
    if duduclaw_license::fingerprint_hardware_unavailable() {
        eprintln!("⚠️  無法讀取本機硬體 UUID；只保留舊版指紋驗證，暫停新授權簽發與重新綁定。請稍後再試。");
        return;
    }
    if fingerprint_is_hostname_only() {
        eprintln!(
            "⚠️  No hardware identity is readable on this host (no platform UUID, \
             no usable MAC address),"
        );
        eprintln!(
            "    so this fingerprint binds to the hostname only — it provides no \
             hardware binding."
        );
    }
}

/// One-line human label for `duduclaw license status`.
fn binding_label(binding: &Binding) -> &'static str {
    match binding {
        Binding::Strong => "strong",
        Binding::Legacy(_) => "legacy (re-issue recommended)",
        Binding::Weak => "weak (OpenSource only; console approval required to rebind)",
        Binding::None => "none (does not match this machine)",
    }
}

/// Read a control-plane error body into a friendly message.
async fn control_error(resp: reqwest::Response) -> DuDuClawError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| {
            v.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| body.chars().take(200).collect());
    DuDuClawError::License(format!("control-plane returned HTTP {status}: {msg}"))
}

// ── rebind (move to this machine) ─────────────────────────────

async fn cmd_rebind() -> Result<()> {
    let path = storage::default_license_path()
        .map_err(|e| DuDuClawError::License(format!("license path: {e}")))?;
    let (installed, snapshot) = match load_request_snapshot(&path) {
        Ok(value) => value,
        Err(LicenseError::FileNotFound(_)) => {
            return Err(DuDuClawError::License(
                "no license installed — copy license.json from the old machine \
                 (or `duduclaw license import <file>`) first, then run rebind"
                    .into(),
            ));
        }
        Err(e) => return Err(DuDuClawError::License(format!("read license: {e}"))),
    };

    if duduclaw_license::fingerprint_hardware_unavailable() {
        return Err(DuDuClawError::License(
            "無法讀取本機硬體 UUID；請稍後再試，不會將授權重新綁定至 MAC。".into(),
        ));
    }
    if fingerprint_is_hostname_only() {
        return Err(DuDuClawError::License(
            "無法讀取可識別本機的硬體資訊，暫時無法提出有效的換綁申請。請恢復硬體識別後再試。".into(),
        ));
    }
    let new_fp = generate_fingerprint();
    if installed.machine_fingerprint == new_fp {
        println!("License is already bound to this machine — nothing to rebind.");
        return Ok(());
    }
    // A fingerprint is not an ownership credential. Only account-holder
    // approval in the console authorizes the issuer to change the binding.

    let endpoint = format!("{}/v1/license/rebind", resolve_control_url(installed.control_url.as_deref()).trim_end_matches('/'));
    let nonce = uuid::Uuid::new_v4().to_string();
    let body = serde_json::json!({
        "subscription_id": installed.subscription_id,
        "old_fingerprint": installed.machine_fingerprint,
        "new_fingerprint": new_fp,
        "nonce": nonce,
    });

    let endpoint = validated_control_endpoint(&endpoint)?;
    let request = control_request(endpoint.as_str()).await?;
    println!("→ 正在提出換綁申請；需要帳號持有人到控制台核准後才會生效。 ({endpoint})");
    let resp = request
        .json(&body)
        .send()
        .await
        .map_err(|e| DuDuClawError::License(format!("control-plane unreachable: {e}")))?;

    if !resp.status().is_success() {
        return Err(control_error(resp).await);
    }

    let status = resp.status();
    let envelope: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| DuDuClawError::License(format!("parse rebind response: {e}")))?;
    if response_pending(status, &envelope) {
        validate_status_response(&envelope, "pending", &installed, &new_fp, &nonce)?;
        println!("{REBIND_PENDING_MESSAGE}");
        return Ok(());
    }
    if envelope.get("status").and_then(|v| v.as_str()) == Some("revoked") {
        apply_revoked_response(&path, &snapshot, &duduclaw_gateway::license_runtime::production_registry(),
            &envelope, &installed, &new_fp, &nonce)?;
        return Err(DuDuClawError::License("license revoked by control-plane; reverted to OpenSource".into()));
    }
    let license = install_license_from_envelope(&envelope, &installed, &new_fp, &nonce, &path, &snapshot)?;

    println!("✓ License rebound to this machine");
    println!("  Tier:     {}", license.tier);
    println!("  Machine:  {new_fp}");
    Ok(())
}

// ── subscriptions (remote status) ─────────────────────────────

async fn cmd_subscriptions() -> Result<()> {
    let installed = match load_default() {
        Ok(l) => l,
        Err(LicenseError::FileNotFound(_)) => {
            println!("No license installed — nothing to look up.");
            return Ok(());
        }
        Err(e) => return Err(DuDuClawError::License(format!("read license: {e}"))),
    };

    let endpoint = format!("{}/v1/license/status", control_url().trim_end_matches('/'));
    let body = serde_json::json!({
        "subscription_id": installed.subscription_id,
        "machine_fingerprint": installed.machine_fingerprint,
    });

    let resp = control_request(&endpoint).await?
        .json(&body)
        .send()
        .await
        .map_err(|e| DuDuClawError::License(format!("control-plane unreachable: {e}")))?;

    if !resp.status().is_success() {
        return Err(control_error(resp).await);
    }

    let s: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| DuDuClawError::License(format!("parse status response: {e}")))?;

    let get = |k: &str| s.get(k).and_then(|v| v.as_str()).unwrap_or("?").to_string();
    println!("Subscription:     {}", get("subscription_id"));
    println!("Tier:             {}", get("tier"));
    println!("Status:           {}", get("status"));
    println!("Expires:          {}", get("expires_at"));
    if let Some(days) = s.get("days_until_expiry").and_then(|v| v.as_i64()) {
        if days < 0 {
            println!("Renewal:          ⚠️  expired {} days ago", -days);
        } else {
            println!("Renewal:          {days} days remaining");
        }
    }
    Ok(())
}

// ── activate ──────────────────────────────────────────────────

async fn cmd_activate(key_input: &str) -> Result<()> {
    let license = parse_license_input(key_input)?;

    // Sanity check: machine fingerprint must match. We don't verify the
    // signature here — that happens at gateway startup against the
    // embedded PublicKeyRegistry. A wrong fingerprint here is almost
    // always a user error (issued to a different machine).
    let candidates = fingerprint_candidates();
    match classify_binding(&license.machine_fingerprint, &candidates) {
        Binding::Strong => {}
        Binding::Legacy(_) => {
            // Only historical UUID or usable MAC bindings reach this branch.
            eprintln!(
                "ℹ️  This license is bound to a legacy fingerprint for this machine \
                 (hostname-based). It stays valid; re-issue against \
                 `duduclaw license fingerprint` when convenient."
            );
        }
        Binding::Weak => {
            return Err(DuDuClawError::License(validation_warning_message("weak_binding").into()));
        }
        Binding::None => {
            let current_fp = candidates[0].fingerprint();
            eprintln!("⚠️  License fingerprint mismatch.");
            eprintln!("   License is bound to: {}", license.machine_fingerprint);
            eprintln!("   Current machine:     {current_fp}");
            eprintln!();
            eprintln!(
                "   If you intended this license for a different machine, do not activate here."
            );
            eprintln!(
                "   If you are migrating, use `duduclaw license export` on the old machine first."
            );
            return Err(DuDuClawError::License(
                "license fingerprint mismatch — refusing to install".into(),
            ));
        }
    }

    if license.is_expired() {
        eprintln!("⚠️  License is already expired ({}).", license.expires_at);
        return Err(DuDuClawError::License("license is expired".into()));
    }

    let path = save_default(&license)
        .map_err(|e| DuDuClawError::License(format!("failed to save license: {e}")))?;

    println!("✓ License activated");
    println!("  Tier:           {}", license.tier);
    println!("  Customer ID:    {}", license.customer_id);
    println!(
        "  Expires:        {} ({} days)",
        license.expires_at,
        license.days_until_expiry()
    );
    println!("  Saved to:       {}", path.display());

    Ok(())
}

fn parse_license_input(input: &str) -> Result<License> {
    let trimmed = input.trim();

    // 1. Path
    let as_path = PathBuf::from(trimmed);
    if as_path.exists() {
        let json = fs::read_to_string(&as_path)
            .map_err(|e| DuDuClawError::License(format!("read {}: {e}", as_path.display())))?;
        return serde_json::from_str(&json)
            .map_err(|e| DuDuClawError::License(format!("parse license file: {e}")));
    }

    // 2. Raw JSON
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed)
            .map_err(|e| DuDuClawError::License(format!("parse license JSON: {e}")));
    }

    // 3. Base64
    if let Ok(decoded) = BASE64.decode(trimmed) {
        if let Ok(license) = serde_json::from_slice::<License>(&decoded) {
            return Ok(license);
        }
    }

    Err(DuDuClawError::License(
        "could not parse license — expected base64, JSON, or a path to a license file".into(),
    ))
}

// ── status ────────────────────────────────────────────────────

async fn cmd_status() -> Result<()> {
    let home = storage::license_dir()
        .map_err(|e| DuDuClawError::License(format!("license directory: {e}")))?;
    let warning = installed_warning(&home)?;
    if let Some(reason) = warning {
        println!("⚠️  {}", validation_warning_message(reason));
        println!("Effective mode: OpenSource（商用授權未生效）；請處理授權後重新啟動服務。");
    }
    match load_default() {
        Ok(license) => print_status(&license, warning),
        Err(LicenseError::FileNotFound(_)) => {
            print_opensource_status();
            Ok(())
        }
        Err(_) if warning.is_some() => Ok(()),
        Err(e) => Err(DuDuClawError::License(format!("read license: {e}"))),
    }
}

fn installed_warning(home: &std::path::Path) -> Result<Option<&'static str>> {
    let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML)
        .map_err(|e| DuDuClawError::License(format!("embedded features.toml is broken: {e}")))?;
    Ok(duduclaw_gateway::license_runtime::installed_license_warning(
        home, &duduclaw_gateway::license_runtime::production_registry(), &gate,
    ))
}

// Fixed operator-facing text: no license-controlled strings or identifiers.
fn validation_warning_message(reason: &str) -> &'static str {
    match reason {
        "weak_binding" => "授權未生效：舊綁定缺少可識別此機器的硬體資訊，目前僅使用 OpenSource。請執行 duduclaw license rebind，並由帳號持有人到控制台核准換綁。",
        "machine_mismatch" => "授權已失效：此機器的識別與授權不符。請聯絡簽發者確認，必要時執行 duduclaw license rebind。",
        "hardware_identity_unavailable" => "授權驗證失敗：無法讀取本機硬體識別。請稍後重啟服務再試；不會以 MAC 代替 UUID 綁定。",
        "expired" => "授權已失效：授權已過期，請聯絡簽發者續約。",
        "invalid_signature" => "授權已失效：簽章無法驗證，請向簽發者取得有效授權。",
        "grace_period_expired" => "授權已失效：離線寬限期已過，請執行 duduclaw license refresh。",
        "unreadable" => "授權驗證失敗：無法讀取授權檔，請檢查內容與檔案權限。",
        "revoked" => "授權已失效：授權已撤銷，請聯絡簽發者。",
        _ => "授權驗證失敗：授權資料或部署型態不符，請檢查 gateway 日誌並聯絡簽發者。",
    }
}

pub(crate) fn doctor_check(home: &std::path::Path) -> (String, duduclaw_core::types::CheckStatus, String) {
    use duduclaw_core::types::CheckStatus;
    let name = "授權狀態".to_string();
    match installed_warning(home) {
        Ok(Some(reason)) => (name, CheckStatus::Warn,
            format!("{} 有效授權層級：OpenSource。", validation_warning_message(reason))),
        Ok(None) => (name, CheckStatus::Pass,
            if home.join(storage::DEFAULT_LICENSE_FILENAME).exists() {
                "本機授權驗證通過。".into()
            } else {
                "未安裝商用授權；使用 OpenSource（個人版）。".into()
            }),
        Err(_) => (name, CheckStatus::Warn, "無法檢查授權狀態；請執行 duduclaw license status。".into()),
    }
}

fn print_opensource_status() {
    println!("Mode:          OpenSource (Apache 2.0)");
    println!("Tier:          {}", LicenseTier::OpenSource);
    println!();
    println!("All core features are unlocked. Commercial value-add modules");
    println!("(premium templates, evolution params, Dashboard Enterprise,");
    println!("priority security patches) are not loaded.");
    println!();
    println!("To install a commercial license:");
    println!("  duduclaw license activate <key>");
    println!();
    println!("For license inquiries, contact the exclusive distributor:");
    println!("  https://futurecorp.tw");
}

fn print_status(license: &License, warning: Option<&str>) -> Result<()> {
    let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML)
        .map_err(|e| DuDuClawError::License(format!("embedded features.toml is broken: {e}")))?;

    let candidates = fingerprint_candidates();
    let current_fp = candidates[0].fingerprint().to_owned();
    let binding = classify_binding(&license.machine_fingerprint, &candidates);
    let fp_matches = matches!(binding, Binding::Strong | Binding::Legacy(_));
    let expired = license.is_expired();
    let days_until = license.days_until_expiry();
    let phone_home_interval = gate.phone_home_interval_days(license.tier);
    let grace_period = gate.grace_period_days(license.tier);

    let effective_tier = if warning.is_some() { LicenseTier::OpenSource } else { license.tier };
    println!("Mode:             {}", if warning.is_some() { "OpenSource" } else { "Commercial" });
    println!("Tier:             {effective_tier}");
    println!("Installed tier:   {}", license.tier);
    println!("Customer ID:      {}", license.customer_id);
    println!("Subscription ID:  {}", license.subscription_id);
    println!("Schema version:   {}", license.version);
    println!("Public key ID:    {}", license.public_key_id);
    println!();
    println!("Issued at:        {}", license.issued_at);
    println!("Expires:          {}", license.expires_at);
    if warning.is_some() {
        println!("Status:           ⚠️  INVALID — installed license is not active");
    } else if expired {
        println!(
            "Status:           ⚠️  EXPIRED ({} days overdue)",
            -days_until
        );
    } else if days_until < 14 {
        println!("Status:           ⚠️  Expires in {days_until} days");
    } else {
        println!("Status:           ✓ Active ({days_until} days remaining)");
    }
    println!();

    // Machine binding
    if fp_matches {
        println!("Machine:          ✓ {}", license.machine_fingerprint);
        println!("Fingerprint binding: {}", binding_label(&binding));
        if let Binding::Legacy(_) = binding {
            println!(
                "                  This machine's strong fingerprint is {current_fp} — \
                 re-issue when convenient."
            );
        }
    } else {
        println!(
            "Machine:          ✗ mismatch (license: {}, current: {current_fp})",
            license.machine_fingerprint
        );
        println!("Fingerprint binding: {}", binding_label(&binding));
    }

    warn_if_hostname_only();

    // Phone-home freshness
    let days_since_ph = license.days_since_phone_home();
    if phone_home_interval > 0 {
        if license.grace_period_exceeded(grace_period) {
            println!(
                "Phone-home:       ⚠️  GRACE EXCEEDED ({days_since_ph} days since refresh, grace = {grace_period}d)"
            );
        } else if license.needs_phone_home(phone_home_interval) {
            println!(
                "Phone-home:       ⏳ overdue ({days_since_ph} days; refresh interval = {phone_home_interval}d)"
            );
            println!(
                "                  Run `duduclaw license refresh` when the control-plane is reachable."
            );
        } else {
            println!("Phone-home:       ✓ {days_since_ph} days ago");
        }
    } else {
        println!("Phone-home:       not required");
    }

    // Two separate lists on purpose (D17, 2026-09): the first two are real
    // capability gates a code path consults; the rest are things a human
    // promises to do and nothing in the binary enforces. Printing them in one
    // undifferentiated block made eight service promises look like feature
    // switches.
    println!();
    println!("Unlocked commercial modules:");
    let capability_gates = [
        ("premium_templates", "Premium industry SOUL.md templates"),
        (
            "industry_evolution_params",
            "Tuned Evolution / GVU parameters",
        ),
    ];
    for (flag, label) in capability_gates {
        let mark = if gate.check(effective_tier, flag) {
            "✓"
        } else {
            "—"
        };
        println!("  {mark} {label}");
    }

    println!();
    println!("Service commitments (not enforced by the software):");
    let commitments = [
        ("dashboard_enterprise", "Audit log export + ROI report"),
        ("priority_security_patch", "Immediate security patches"),
        ("private_discord_support", "Private Discord support channel"),
        ("odoo_integration_supported", "Odoo ERP integration (Cloud)"),
        ("redistribution", "Distributor delivery authorization"),
        ("dedicated_engineer", "Dedicated engineer"),
    ];
    for (flag, label) in commitments {
        let mark = if gate.service_commitment(effective_tier, flag) {
            "✓"
        } else {
            "—"
        };
        println!("  {mark} {label}");
    }

    Ok(())
}

// ── refresh ────────────────────────────────────────────────────

/// Default control-plane URL. Overridable via `DUDUCLAW_CONTROL_URL` env var.
use duduclaw_license::control_url::DEFAULT_CONTROL_URL;

async fn cmd_refresh() -> Result<()> {
    let path = storage::default_license_path()
        .map_err(|e| DuDuClawError::License(format!("license path: {e}")))?;
    let (license, snapshot) = match load_request_snapshot(&path) {
        Ok(value) => value,
        Err(LicenseError::FileNotFound(_)) => {
            println!("No license installed — nothing to refresh.");
            return Ok(());
        }
        Err(e) => {
            return Err(DuDuClawError::License(format!("read license: {e}")));
        }
    };

    // §10.5 precedence: env > self-carried license.control_url > default.
    let control_url = resolve_control_url(license.control_url.as_deref());
    let endpoint = format!("{}/v1/license/refresh", control_url.trim_end_matches('/'));

    let proposed_fingerprint = duduclaw_license::fingerprint::refresh_fingerprint(&license.machine_fingerprint)
        .map_err(|e| DuDuClawError::License(format!("cannot refresh machine identity: {e}")))?;
    let nonce = uuid::Uuid::new_v4().to_string();
    let mut request_body = serde_json::json!({
        "subscription_id": license.subscription_id,
        // Existing issuers authorize refresh against the installed binding.
        "machine_fingerprint": license.machine_fingerprint,
        "current_version": env!("CARGO_PKG_VERSION"),
        "nonce": nonce,
        // Phase 1: no telemetry yet — added in Phase 2 once gateway exposes counters.
        "telemetry": {},
    });
    if proposed_fingerprint != license.machine_fingerprint {
        // This proposal is only effective after the account holder approves
        // the rebind in the console. The installed binding is not a secret.
        request_body["new_fingerprint"] = serde_json::json!(proposed_fingerprint);
    }

    let endpoint = validated_control_endpoint(&endpoint)?;
    let client = control_request(endpoint.as_str()).await?;
    println!("→ Refreshing license at {endpoint} ...");
    let response = match client.json(&request_body).send().await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("✗ Could not reach control-plane: {e}");
            eprintln!();
            eprintln!("Your license remains valid for now — phone-home will be");
            eprintln!("retried automatically. If this persists for more than the");
            eprintln!("grace period your tier will downgrade to OpenSource.");
            return Err(DuDuClawError::License(format!(
                "control-plane unreachable: {e}"
            )));
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body_text = response.text().await.unwrap_or_else(|_| "<no body>".into());
        return Err(DuDuClawError::License(format!(
            "control-plane returned HTTP {status}: {body_text}"
        )));
    }

    let envelope: serde_json::Value = response
        .json()
        .await
        .map_err(|e| DuDuClawError::License(format!("parse refresh response: {e}")))?;

    if response_pending(status, &envelope) {
        validate_status_response(&envelope, "pending", &license, &proposed_fingerprint, &nonce)?;
        println!("{REBIND_PENDING_MESSAGE}");
        return Ok(());
    }
    match envelope.get("status").and_then(|v| v.as_str()) {
        Some("active") => {
            let new_license_value = envelope.get("license").ok_or_else(|| {
                DuDuClawError::License("missing 'license' in active response".into())
            })?;
            let new_license: License = serde_json::from_value(new_license_value.clone())
                .map_err(|e| DuDuClawError::License(format!("parse new license: {e}")))?;

            duduclaw_license::refresh::validate_response(
                &duduclaw_gateway::license_runtime::production_registry(), &new_license, &envelope,
                &nonce, &license.subscription_id, &license.machine_fingerprint, &proposed_fingerprint,
            ).map_err(|e| DuDuClawError::License(format!("refresh response rejected; installed license unchanged: {e}")))?;
            validate_control_plane_license(&new_license)?;
            save_response_if_current(&path, &snapshot, &new_license)?;

            println!("✓ License refreshed");
            println!("  Tier:           {}", new_license.tier);
            println!("  Days remaining: {}", new_license.days_until_expiry());
            println!("  Saved to:       {}", path.display());

            if let Some(warnings) = envelope.get("warnings").and_then(|v| v.as_array()) {
                if !warnings.is_empty() {
                    println!();
                    println!("Warnings from control-plane:");
                    for w in warnings {
                        if let Some(s) = w.as_str() {
                            println!("  - {s}");
                        }
                    }
                }
            }
            Ok(())
        }
        Some("revoked") => {
            apply_revoked_response(&path, &snapshot, &duduclaw_gateway::license_runtime::production_registry(),
                &envelope, &license, &proposed_fingerprint, &nonce)?;
            let reason = envelope
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or("revoked");
            let effective_from = envelope
                .get("effective_from")
                .and_then(|v| v.as_str())
                .unwrap_or("now");
            eprintln!("✗ License has been revoked");
            eprintln!("  Reason:         {reason}");
            eprintln!("  Effective from: {effective_from}");
            eprintln!();
            eprintln!("The verified revocation was saved; reverted to OpenSource mode.");
            Err(DuDuClawError::License(format!(
                "license revoked by control-plane: {reason}"
            )))
        }
        other => Err(DuDuClawError::License(format!(
            "unexpected refresh response status: {other:?}"
        ))),
    }
}

// ── export ────────────────────────────────────────────────────

async fn cmd_export(as_base64: bool) -> Result<()> {
    let license = match load_default() {
        Ok(l) => l,
        Err(LicenseError::FileNotFound(_)) => {
            return Err(DuDuClawError::License(
                "no license installed — nothing to export".into(),
            ));
        }
        Err(e) => {
            return Err(DuDuClawError::License(format!("read license: {e}")));
        }
    };

    let json = serde_json::to_string_pretty(&license)
        .map_err(|e| DuDuClawError::License(format!("serialize: {e}")))?;

    if as_base64 {
        println!("{}", BASE64.encode(json.as_bytes()));
    } else {
        println!("{json}");
    }

    Ok(())
}

// ── import ────────────────────────────────────────────────────

async fn cmd_import(path: &PathBuf) -> Result<()> {
    let json = fs::read_to_string(path)
        .map_err(|e| DuDuClawError::License(format!("read {}: {e}", path.display())))?;
    let license: License = serde_json::from_str(&json)
        .map_err(|e| DuDuClawError::License(format!("parse {}: {e}", path.display())))?;

    // Same fingerprint check as activate (legacy bindings included).
    let candidates = fingerprint_candidates();
    if classify_binding(&license.machine_fingerprint, &candidates) == Binding::None {
        eprintln!("⚠️  License fingerprint mismatch.");
        eprintln!("   License is bound to: {}", license.machine_fingerprint);
        eprintln!("   Current machine:     {}", candidates[0].fingerprint());
        return Err(DuDuClawError::License(
            "license fingerprint mismatch".into(),
        ));
    }

    if classify_binding(&license.machine_fingerprint, &candidates) == Binding::Weak {
        eprintln!("⚠️  {}", validation_warning_message("weak_binding"));
    }
    let saved_to =
        save_default(&license).map_err(|e| DuDuClawError::License(format!("save: {e}")))?;

    println!("✓ License imported");
    println!("  Installed tier: {}", license.tier);
    println!("  Saved:          {}", saved_to.display());
    Ok(())
}

// ── deactivate ────────────────────────────────────────────────

async fn cmd_deactivate() -> Result<()> {
    let path = storage::default_license_path()
        .map_err(|e| DuDuClawError::License(format!("license path: {e}")))?;
    let store = storage::LockedLicenseStore::acquire(&path)
        .map_err(|e| DuDuClawError::License(format!("lock license: {e}")))?;
    match store.load() {
        Ok(license) => {
            store.delete()
                .map_err(|e| DuDuClawError::License(format!("delete license: {e}")))?;
            println!("✓ License deactivated (tier was: {})", license.tier);
            println!("  Reverted to OpenSource mode.");
            println!("  All core features remain available.");
        }
        Err(LicenseError::FileNotFound(_)) => {
            println!("No license to deactivate — already in OpenSource mode.");
        }
        Err(e) => {
            return Err(DuDuClawError::License(format!("read license: {e}")));
        }
    }
    Ok(())
}

// ── fingerprint ───────────────────────────────────────────────

async fn cmd_fingerprint(all: bool) -> Result<()> {
    if duduclaw_license::fingerprint_hardware_unavailable() {
        return Err(DuDuClawError::License(
            "無法讀取本機硬體 UUID；不提供 MAC 指紋供新授權簽發。請稍後再試。".into(),
        ));
    }
    // Line 1 is ALWAYS the strong fingerprint — this is the value a customer
    // reports and a new license is issued against. `--all` appends the
    // historical values for diagnosis. Weak candidates never activate a paid tier.
    let mut candidates = fingerprint_candidates();
    if !all { candidates.truncate(1); }
    for fp in &candidates {
        println!("{}", fp.fingerprint());
    }
    if all && candidates.len() == 1 {
        eprintln!("(no legacy fingerprints — this machine accepts only the value above)");
    }
    warn_if_hostname_only();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<FingerprintCandidate> {
        vec![
            FingerprintCandidate::for_test("1837a5d96419f6a9d7396c3902882fe8", FingerprintStrength::Hardware), // strong
            FingerprintCandidate::for_test("1743ebb5466bcad6c206e7e42371f45d", FingerprintStrength::Hardware), // hardware-backed legacy
        ]
    }

    #[test]
    fn pending_response_selection_never_requires_a_license_payload() {
        for body in [serde_json::Value::Null, serde_json::json!({"license": "malformed"}),
            serde_json::json!({"status": "active", "license": {"tier": "oem"}})] {
            assert!(response_pending(reqwest::StatusCode::ACCEPTED, &body));
        }
        assert!(response_pending(reqwest::StatusCode::OK, &serde_json::json!({"status": "pending"})));
        for status in ["active", "pending-other", "pending "] {
            assert!(!response_pending(reqwest::StatusCode::OK, &serde_json::json!({"status": status})));
        }
        assert!(REBIND_PENDING_MESSAGE.contains("帳號持有人到控制台核准"));
        assert!(REBIND_PENDING_MESSAGE.contains("原授權未變更"));
    }

    fn response_test_license() -> License {
        License::new("sub-cli-response", "customer", LicenseTier::SelfHostPro,
            "binding", chrono::Duration::days(30), "test")
    }

    #[test]
    fn pending_response_requires_signed_status_even_for_http_202() {
        let installed = response_test_license();
        for body in [serde_json::Value::Null, serde_json::json!({"status":"pending", "nonce":"n"}),
            serde_json::json!({"status":"active", "nonce":"n"})] {
            assert!(response_pending(reqwest::StatusCode::ACCEPTED, &body));
            assert!(validate_status_response(&body, "pending", &installed, "new", "n").is_err());
        }
    }

    #[test]
    fn delayed_cli_response_preserves_concurrently_installed_license() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(storage::DEFAULT_LICENSE_FILENAME);
        let original = response_test_license();
        storage::save_to(&original, &path).unwrap();
        let (_, snapshot) = load_request_snapshot(&path).unwrap();
        let mut replacement = original.clone();
        replacement.max_agents = Some(2);
        replacement.control_url = Some("https://replacement.example.test".into());
        storage::save_to(&replacement, &path).unwrap();
        assert!(save_response_if_current(&path, &snapshot, &original).is_err());
        let after = storage::load_from(&path).unwrap();
        assert_eq!(serde_json::to_value(after).unwrap(), serde_json::to_value(replacement).unwrap());
        assert!(current_response_store(&path, &snapshot).is_err(), "stale revocation must also be fenced");
        assert!(!home.path().join("license_revoked.marker").exists());
    }

    #[test]
    fn delayed_cli_response_cannot_restore_deactivated_license() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(storage::DEFAULT_LICENSE_FILENAME);
        let original = response_test_license();
        storage::save_to(&original, &path).unwrap();
        let (_, snapshot) = load_request_snapshot(&path).unwrap();
        storage::LockedLicenseStore::acquire(&path).unwrap().delete().unwrap();
        assert!(save_response_if_current(&path, &snapshot, &original).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn unsigned_cli_revocation_never_changes_disk_state() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(storage::DEFAULT_LICENSE_FILENAME);
        let original = response_test_license();
        storage::save_to(&original, &path).unwrap();
        let before = std::fs::read(&path).unwrap();
        let (_, snapshot) = load_request_snapshot(&path).unwrap();
        for version in [serde_json::Value::Null, serde_json::json!(1), serde_json::json!(2)] {
            let body = serde_json::json!({"status":"revoked", "nonce":"n", "reason":"revoked",
                "response_signature_version":version});
            assert!(apply_revoked_response(&path, &snapshot, &duduclaw_license::PublicKeyRegistry::new(),
                &body, &original, "new", "n").is_err());
            assert_eq!(std::fs::read(&path).unwrap(), before);
            assert!(!home.path().join("license_revoked.marker").exists());
        }
    }

    #[test]
    fn signed_cli_revocation_is_fenced_by_concurrent_install() {
        let fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/license_response_v2.json")).unwrap();
        let registry = duduclaw_license::PublicKeyRegistry::new().with_key("test-vector",
            hex::decode(fixture["public_key_hex"].as_str().unwrap()).unwrap());
        let body = fixture["status_vectors"].as_array().unwrap().iter()
            .find(|body| body["status"] == "revoked").unwrap();
        let mut original: License = serde_json::from_value(fixture["license"].clone()).unwrap();
        original.machine_fingerprint = fixture["original_binding"].as_str().unwrap().into();
        let proposed = fixture["proposed_binding"].as_str().unwrap();
        let nonce = fixture["nonce"].as_str().unwrap();
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join(storage::DEFAULT_LICENSE_FILENAME);
        storage::save_to(&original, &path).unwrap();
        let (_, snapshot) = load_request_snapshot(&path).unwrap();
        let mut replacement = original.clone();
        replacement.subscription_id = "new-subscription".into();
        storage::save_to(&replacement, &path).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(apply_revoked_response(&path, &snapshot, &registry, body, &original, proposed, nonce).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!home.path().join("license_revoked.marker").exists());

        storage::save_to(&original, &path).unwrap();
        let (_, current) = load_request_snapshot(&path).unwrap();
        apply_revoked_response(&path, &current, &registry, body, &original, proposed, nonce).unwrap();
        assert!(!path.exists());
        assert!(home.path().join("license_revoked.marker").exists());
    }

    #[test]
    fn classify_binding_never_labels_weak_matches_as_valid() {
        let weak = FingerprintCandidate::for_test("shared-hostname-hash", FingerprintStrength::Weak);
        for candidates in [vec![weak.clone()], vec![FingerprintCandidate::for_test("real-hardware", FingerprintStrength::Hardware), weak]] {
            assert_eq!(classify_binding("shared-hostname-hash", &candidates), Binding::Weak);
        }
        assert!(binding_label(&Binding::Weak).contains("OpenSource only"));
        assert!(validation_warning_message("weak_binding").contains("控制台核准換綁"));
    }

    #[test]
    fn doctor_license_reports_missing_without_writing() {
        let home = tempfile::tempdir().unwrap();
        let row = doctor_check(home.path());
        assert_eq!(row.1, duduclaw_core::types::CheckStatus::Pass);
        assert!(row.2.contains("未安裝商用授權"));
        assert_eq!(std::fs::read_dir(home.path()).unwrap().count(), 0);
    }

    #[test]
    fn doctor_license_warns_for_unreadable_installed_license() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(storage::DEFAULT_LICENSE_FILENAME), "not json").unwrap();
        let row = doctor_check(home.path());
        assert_eq!(row.1, duduclaw_core::types::CheckStatus::Warn);
        assert!(row.2.contains("無法讀取授權檔"));
        assert!(row.2.contains("OpenSource"));
    }

    #[test]
    fn control_plane_response_with_untrusted_signature_is_rejected() {
        let license = License::new(
            "sub-test", "customer-test", LicenseTier::SelfHostPro, &generate_fingerprint(),
            chrono::Duration::days(30), "untrusted-test-issuer",
        );
        let error = validate_control_plane_license(&license).unwrap_err().to_string();
        assert!(error.contains("installed license unchanged"));
    }

    #[test]
    fn license_warning_messages_do_not_echo_unknown_input() {
        let private_value = "machine-secret-fingerprint";
        assert!(!validation_warning_message(private_value).contains(private_value));
        assert!(validation_warning_message("machine_mismatch").contains("此機器的識別與授權不符"));
    }

    #[test]
    fn classify_binding_detects_a_strong_binding() {
        assert_eq!(
            classify_binding("1837a5d96419f6a9d7396c3902882fe8", &candidates()),
            Binding::Strong
        );
    }

    #[test]
    fn classify_binding_detects_a_legacy_binding() {
        // A historical UUID or real-MAC fingerprint remains hardware-backed.
        let binding = classify_binding("1743ebb5466bcad6c206e7e42371f45d", &candidates());
        assert_eq!(
            binding,
            Binding::Legacy("1743ebb5466bcad6c206e7e42371f45d".to_string())
        );
        assert_eq!(binding_label(&binding), "legacy (re-issue recommended)");
    }

    #[test]
    fn classify_binding_rejects_another_machine() {
        let binding = classify_binding("deadbeefdeadbeefdeadbeefdeadbeef", &candidates());
        assert_eq!(binding, Binding::None);
        assert_eq!(binding_label(&binding), "none (does not match this machine)");
    }

    #[test]
    fn classify_binding_labels_strong_as_strong() {
        assert_eq!(binding_label(&Binding::Strong), "strong");
    }

    #[test]
    fn retired_partner_redemption_command_is_unavailable() {
        let command = <LicenseCommands as clap::Subcommand>::augment_subcommands(
            clap::Command::new("license"),
        );
        let error = command.try_get_matches_from(["license", "redeem", "PARTNER-TEST"])
            .expect_err("self-service partner redemption is retired");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
    }

    #[test]
    fn fingerprint_candidates_lead_with_the_strong_value() {
        // `duduclaw license fingerprint` (no --all) must print exactly
        // candidates[0]; `--all` prints the same value on line 1.
        let all = fingerprint_candidates();
        assert!(!all.is_empty());
        assert_eq!(all[0].fingerprint(), generate_fingerprint());
    }

    #[test]
    fn only_license_state_changes_are_operator_only() {
        assert!(command_writes_state(&LicenseCommands::Activate { key: "x".into() }));
        assert!(command_writes_state(&LicenseCommands::Refresh));
        assert!(command_writes_state(&LicenseCommands::Import { path: PathBuf::from("license.json") }));
        assert!(command_writes_state(&LicenseCommands::Deactivate));
        assert!(command_writes_state(&LicenseCommands::Rebind));
        assert!(!command_writes_state(&LicenseCommands::Status));
        assert!(!command_writes_state(&LicenseCommands::Export { base64: false }));
        assert!(!command_writes_state(&LicenseCommands::Subscriptions));
        assert!(!command_writes_state(&LicenseCommands::Fingerprint { all: false }));
    }

    #[test]
    fn ai_session_markers_refuse_license_writes_but_not_reads() {
        let marker = [duduclaw_core::ENV_AGENT_ID];
        assert!(should_refuse_in_ai_session(
            &LicenseCommands::Deactivate,
            &marker,
        ));
        assert!(should_refuse_in_ai_session(
            &LicenseCommands::Rebind,
            &marker,
        ));
        assert!(!should_refuse_in_ai_session(
            &LicenseCommands::Status,
            &marker,
        ));
        assert!(!should_refuse_in_ai_session(
            &LicenseCommands::Deactivate,
            &[],
        ));
    }

    #[test]
    fn bash_lane_blocks_license_writes_but_allows_reads() {
        let caller = duduclaw_core::HookCaller::Agent("worker".into());
        for command in [
            "duduclaw license activate abc",
            "duduclaw license refresh",
            "duduclaw license import ./license.json",
            "duduclaw license deactivate",
            "duduclaw license rebind",
        ] {
            assert!(bash_license_decision(command, &caller).is_some(), "{command}");
        }
        for command in [
            "duduclaw license status",
            "duduclaw license export",
            "duduclaw license fingerprint",
            "duduclaw license subscriptions",
        ] {
            assert!(bash_license_decision(command, &caller).is_none(), "{command}");
        }
    }
}
