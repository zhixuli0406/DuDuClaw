//! `duduclaw license` subcommand — client-side license management.
//!
//! Wraps the OSS `duduclaw-license` crate with a friendly CLI surface:
//!
//!   activate <key>      Install a license key into ~/.duduclaw/license.json
//!   status              Show the active license tier, expiry, and gating
//!   refresh             Phone home to control-plane (Phase 2; stub for now)
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
//! - Phone-home is intentionally a stub until the control-plane crate
//!   lands (see commercial/docs/spec-license-module.md §6).

use std::fs;
use std::path::PathBuf;

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use clap::Subcommand;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_license::{
    generate_fingerprint, load_default, save_default, storage, FeatureGate, License,
    LicenseError, LicenseTier, EMBEDDED_FEATURES_TOML,
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
    /// Currently a stub — control-plane lands in Phase 2 of the M1 plan
    /// (see commercial/docs/spec-license-module.md §6).
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

    /// Redeem a partner (NFR) code for a FREE license and activate it on
    /// this machine. The free partner path — no payment.
    Redeem {
        /// Partner redemption code (e.g. `PARTNER-XXXXXXXX`).
        code: String,
        /// Optional partner / customer identifier recorded with the license.
        #[arg(long)]
        customer_id: Option<String>,
        /// Optional email to ALSO receive the License Key by mail.
        #[arg(long)]
        email: Option<String>,
    },

    /// Move the installed license to THIS machine (re-sign for the current
    /// fingerprint). Run after copying `license.json` from the old machine.
    Rebind,

    /// Show this machine's subscription status from the control-plane
    /// (tier / status / days until renewal).
    Subscriptions,

    /// Print this machine's fingerprint, for issuing a new license.
    Fingerprint,
}

// ── Public entry point ────────────────────────────────────────

pub async fn run(cmd: LicenseCommands) -> Result<()> {
    match cmd {
        LicenseCommands::Activate { key } => cmd_activate(&key).await,
        LicenseCommands::Status => cmd_status().await,
        LicenseCommands::Refresh => cmd_refresh().await,
        LicenseCommands::Export { base64 } => cmd_export(base64).await,
        LicenseCommands::Import { path } => cmd_import(&path).await,
        LicenseCommands::Deactivate => cmd_deactivate().await,
        LicenseCommands::Redeem {
            code,
            customer_id,
            email,
        } => cmd_redeem(&code, customer_id, email).await,
        LicenseCommands::Rebind => cmd_rebind().await,
        LicenseCommands::Subscriptions => cmd_subscriptions().await,
        LicenseCommands::Fingerprint => cmd_fingerprint().await,
    }
}

/// Resolve the control-plane base URL (override with `DUDUCLAW_CONTROL_URL`).
fn control_url() -> String {
    std::env::var("DUDUCLAW_CONTROL_URL").unwrap_or_else(|_| DEFAULT_CONTROL_URL.to_string())
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| DuDuClawError::License(format!("build http client: {e}")))
}

/// Extract a signed `License` from a `{ "license": {...} }` control-plane
/// response and install it locally (shared by redeem + rebind).
fn install_license_from_envelope(envelope: &serde_json::Value) -> Result<License> {
    let value = envelope
        .get("license")
        .ok_or_else(|| DuDuClawError::License("response missing 'license'".into()))?;
    let license: License = serde_json::from_value(value.clone())
        .map_err(|e| DuDuClawError::License(format!("parse license: {e}")))?;
    save_default(&license).map_err(|e| DuDuClawError::License(format!("save license: {e}")))?;
    Ok(license)
}

/// Read a control-plane error body into a friendly message.
async fn control_error(resp: reqwest::Response) -> DuDuClawError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let msg = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect());
    DuDuClawError::License(format!("control-plane returned HTTP {status}: {msg}"))
}

// ── redeem (partner / free) ───────────────────────────────────

async fn cmd_redeem(
    code: &str,
    customer_id: Option<String>,
    email: Option<String>,
) -> Result<()> {
    let fingerprint = generate_fingerprint();
    let endpoint = format!("{}/v1/partner/redeem", control_url().trim_end_matches('/'));
    let body = serde_json::json!({
        "code": code,
        "machine_fingerprint": fingerprint,
        "customer_id": customer_id,
        "email": email,
    });

    println!("→ Redeeming partner code at {endpoint} ...");
    let resp = http_client()?
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| DuDuClawError::License(format!("control-plane unreachable: {e}")))?;

    if !resp.status().is_success() {
        return Err(control_error(resp).await);
    }

    let envelope: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| DuDuClawError::License(format!("parse redeem response: {e}")))?;
    let license = install_license_from_envelope(&envelope)?;

    println!("✓ Partner license activated (free)");
    println!("  Tier:     {}", license.tier);
    println!(
        "  Expires:  {} ({} days)",
        license.expires_at,
        license.days_until_expiry()
    );
    println!("  Run `duduclaw license status` to see unlocked modules.");
    Ok(())
}

// ── rebind (move to this machine) ─────────────────────────────

async fn cmd_rebind() -> Result<()> {
    let installed = match load_default() {
        Ok(l) => l,
        Err(LicenseError::FileNotFound(_)) => {
            return Err(DuDuClawError::License(
                "no license installed — copy license.json from the old machine \
                 (or `duduclaw license import <file>`) first, then run rebind"
                    .into(),
            ));
        }
        Err(e) => return Err(DuDuClawError::License(format!("read license: {e}"))),
    };

    let new_fp = generate_fingerprint();
    if installed.machine_fingerprint == new_fp {
        println!("License is already bound to this machine — nothing to rebind.");
        return Ok(());
    }

    let endpoint = format!("{}/v1/license/rebind", control_url().trim_end_matches('/'));
    let body = serde_json::json!({
        "subscription_id": installed.subscription_id,
        "old_fingerprint": installed.machine_fingerprint,
        "new_fingerprint": new_fp,
    });

    println!("→ Rebinding license to this machine at {endpoint} ...");
    let resp = http_client()?
        .post(&endpoint)
        .json(&body)
        .send()
        .await
        .map_err(|e| DuDuClawError::License(format!("control-plane unreachable: {e}")))?;

    if !resp.status().is_success() {
        return Err(control_error(resp).await);
    }

    let envelope: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| DuDuClawError::License(format!("parse rebind response: {e}")))?;
    let license = install_license_from_envelope(&envelope)?;

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

    let resp = http_client()?
        .post(&endpoint)
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
    let current_fp = generate_fingerprint();
    if !license.is_valid_for_machine(&current_fp) {
        eprintln!("⚠️  License fingerprint mismatch.");
        eprintln!("   License is bound to: {}", license.machine_fingerprint);
        eprintln!("   Current machine:     {current_fp}");
        eprintln!();
        eprintln!("   If you intended this license for a different machine, do not activate here.");
        eprintln!("   If you are migrating, use `duduclaw license export` on the old machine first.");
        return Err(DuDuClawError::License(
            "license fingerprint mismatch — refusing to install".into(),
        ));
    }

    if license.is_expired() {
        eprintln!("⚠️  License is already expired ({}).", license.expires_at);
        return Err(DuDuClawError::License("license is expired".into()));
    }

    let path = save_default(&license).map_err(|e| {
        DuDuClawError::License(format!("failed to save license: {e}"))
    })?;

    println!("✓ License activated");
    println!("  Tier:           {}", license.tier);
    println!("  Customer ID:    {}", license.customer_id);
    println!("  Expires:        {} ({} days)", license.expires_at, license.days_until_expiry());
    println!("  Saved to:       {}", path.display());

    Ok(())
}

fn parse_license_input(input: &str) -> Result<License> {
    let trimmed = input.trim();

    // 1. Path
    let as_path = PathBuf::from(trimmed);
    if as_path.exists() {
        let json = fs::read_to_string(&as_path).map_err(|e| {
            DuDuClawError::License(format!("read {}: {e}", as_path.display()))
        })?;
        return serde_json::from_str(&json).map_err(|e| {
            DuDuClawError::License(format!("parse license file: {e}"))
        });
    }

    // 2. Raw JSON
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed).map_err(|e| {
            DuDuClawError::License(format!("parse license JSON: {e}"))
        });
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
    match load_default() {
        Ok(license) => print_status(&license),
        Err(LicenseError::FileNotFound(_)) => {
            print_opensource_status();
            Ok(())
        }
        Err(e) => Err(DuDuClawError::License(format!("read license: {e}"))),
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
    println!("To get a license:");
    println!("  https://duduclaw.tw/pricing");
}

fn print_status(license: &License) -> Result<()> {
    let gate = FeatureGate::from_str(EMBEDDED_FEATURES_TOML).map_err(|e| {
        DuDuClawError::License(format!("embedded features.toml is broken: {e}"))
    })?;

    let current_fp = generate_fingerprint();
    let fp_matches = license.is_valid_for_machine(&current_fp);
    let expired = license.is_expired();
    let days_until = license.days_until_expiry();
    let phone_home_interval = gate.phone_home_interval_days(license.tier);
    let grace_period = gate.grace_period_days(license.tier);

    println!("Mode:             Commercial");
    println!("Tier:             {}", license.tier);
    println!("Customer ID:      {}", license.customer_id);
    println!("Subscription ID:  {}", license.subscription_id);
    println!("Schema version:   {}", license.version);
    println!("Public key ID:    {}", license.public_key_id);
    println!();
    println!("Issued at:        {}", license.issued_at);
    println!("Expires:          {}", license.expires_at);
    if expired {
        println!("Status:           ⚠️  EXPIRED ({} days overdue)", -days_until);
    } else if days_until < 14 {
        println!("Status:           ⚠️  Expires in {days_until} days");
    } else {
        println!("Status:           ✓ Active ({days_until} days remaining)");
    }
    println!();

    // Machine binding
    if fp_matches {
        println!("Machine:          ✓ {current_fp}");
    } else {
        println!("Machine:          ✗ mismatch (license: {}, current: {current_fp})", license.machine_fingerprint);
    }

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
            println!("                  Run `duduclaw license refresh` when the control-plane is reachable.");
        } else {
            println!("Phone-home:       ✓ {days_since_ph} days ago");
        }
    } else {
        println!("Phone-home:       not required");
    }

    println!();
    println!("Unlocked commercial modules:");
    let feature_flags = [
        ("premium_templates", "Premium industry SOUL.md templates"),
        ("industry_evolution_params", "Tuned Evolution / GVU parameters"),
        ("dashboard_enterprise", "Audit log export + ROI report"),
        ("priority_security_patch", "Immediate security patches"),
        ("private_discord_support", "Private Discord support channel"),
        ("odoo_integration_supported", "Odoo ERP integration (Cloud)"),
        ("white_label", "White-label / OEM redistribution"),
    ];
    for (flag, label) in feature_flags {
        let mark = if gate.check(license.tier, flag) {
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
const DEFAULT_CONTROL_URL: &str = "https://api.duduclaw.tw";

async fn cmd_refresh() -> Result<()> {
    let license = match load_default() {
        Ok(l) => l,
        Err(LicenseError::FileNotFound(_)) => {
            println!("No license installed — nothing to refresh.");
            return Ok(());
        }
        Err(e) => {
            return Err(DuDuClawError::License(format!("read license: {e}")));
        }
    };

    let control_url = std::env::var("DUDUCLAW_CONTROL_URL")
        .unwrap_or_else(|_| DEFAULT_CONTROL_URL.to_string());
    let endpoint = format!("{}/v1/license/refresh", control_url.trim_end_matches('/'));

    let request_body = serde_json::json!({
        "subscription_id": license.subscription_id,
        "machine_fingerprint": license.machine_fingerprint,
        "current_version": env!("CARGO_PKG_VERSION"),
        // Phase 1: no telemetry yet — added in Phase 2 once gateway exposes counters.
        "telemetry": {},
    });

    println!("→ Refreshing license at {endpoint} ...");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| DuDuClawError::License(format!("build http client: {e}")))?;

    let response = match client.post(&endpoint).json(&request_body).send().await {
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
        let body_text = response
            .text()
            .await
            .unwrap_or_else(|_| "<no body>".into());
        return Err(DuDuClawError::License(format!(
            "control-plane returned HTTP {status}: {body_text}"
        )));
    }

    let envelope: serde_json::Value = response
        .json()
        .await
        .map_err(|e| DuDuClawError::License(format!("parse refresh response: {e}")))?;

    match envelope.get("status").and_then(|v| v.as_str()) {
        Some("active") => {
            let new_license_value = envelope.get("license").ok_or_else(|| {
                DuDuClawError::License("missing 'license' in active response".into())
            })?;
            let new_license: License = serde_json::from_value(new_license_value.clone())
                .map_err(|e| {
                    DuDuClawError::License(format!("parse new license: {e}"))
                })?;

            let saved_to = save_default(&new_license).map_err(|e| {
                DuDuClawError::License(format!("save new license: {e}"))
            })?;

            println!("✓ License refreshed");
            println!("  Tier:           {}", new_license.tier);
            println!(
                "  Days remaining: {}",
                new_license.days_until_expiry()
            );
            println!("  Saved to:       {}", saved_to.display());

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
            eprintln!("The local license file is preserved so you can export it for");
            eprintln!("audit purposes. Run `duduclaw license deactivate` to remove it");
            eprintln!("and revert to OpenSource mode.");
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
    let json = fs::read_to_string(path).map_err(|e| {
        DuDuClawError::License(format!("read {}: {e}", path.display()))
    })?;
    let license: License = serde_json::from_str(&json).map_err(|e| {
        DuDuClawError::License(format!("parse {}: {e}", path.display()))
    })?;

    // Same fingerprint check as activate
    let current_fp = generate_fingerprint();
    if !license.is_valid_for_machine(&current_fp) {
        eprintln!("⚠️  License fingerprint mismatch.");
        eprintln!("   License is bound to: {}", license.machine_fingerprint);
        eprintln!("   Current machine:     {current_fp}");
        return Err(DuDuClawError::License(
            "license fingerprint mismatch".into(),
        ));
    }

    let saved_to = save_default(&license)
        .map_err(|e| DuDuClawError::License(format!("save: {e}")))?;

    println!("✓ License imported");
    println!("  Tier:    {}", license.tier);
    println!("  Saved:   {}", saved_to.display());
    Ok(())
}

// ── deactivate ────────────────────────────────────────────────

async fn cmd_deactivate() -> Result<()> {
    match load_default() {
        Ok(license) => {
            storage::delete_default()
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

async fn cmd_fingerprint() -> Result<()> {
    println!("{}", generate_fingerprint());
    Ok(())
}
