//! DuDuClaw License Key Generator (internal tool)
//!
//! Generates Ed25519-signed license keys for commercial customers.
//! This binary is NOT distributed — used only by DuDuClaw maintainers.
//!
//! Output format: flat JSON matching `feature_gate.rs` expectations:
//! ```json
//! {
//!   "tier": "pro",
//!   "customer_name": "...",
//!   "machine_fingerprint": "...",
//!   "issued_at": "2026-04-02T12:00:00Z",
//!   "expires_at": "2027-04-02T12:00:00Z",
//!   "signature": "BASE64_ED25519_64_BYTES"
//! }
//! ```

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// License tier matching the feature_gate.rs definition.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
enum LicenseTier {
    Community,
    Pro,
    Enterprise,
    Oem,
}

impl std::fmt::Display for LicenseTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Community => write!(f, "community"),
            Self::Pro => write!(f, "pro"),
            Self::Enterprise => write!(f, "enterprise"),
            Self::Oem => write!(f, "oem"),
        }
    }
}

/// Canonical payload — the exact fields and order that get signed.
/// MUST match `build_canonical_payload()` in `feature_gate.rs`.
// CANONICAL ORDER — DO NOT REORDER FIELDS.
// serde_json serializes structs in declaration order.
// Changing field order will invalidate ALL existing license signatures.
#[derive(Debug, Serialize, Deserialize)]
struct CanonicalPayload {
    tier: String,
    customer_name: String,
    machine_fingerprint: String,
    issued_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
}

/// Full license key written to ~/.duduclaw/license.key (flat JSON).
/// NOTE: `features` is NOT included in the Ed25519 signature — it is informational.
/// Only the fields in CanonicalPayload are cryptographically protected.
#[derive(Debug, Serialize, Deserialize)]
struct FlatLicense {
    tier: String,
    customer_name: String,
    machine_fingerprint: String,
    issued_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    features: Vec<String>,
    signature: String, // Base64-encoded Ed25519 signature of CanonicalPayload JSON
}

/// DuDuClaw License Key Generator
#[derive(Parser)]
#[command(name = "license-keygen", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Generate a new Ed25519 keypair for license signing
    Keygen {
        /// Output directory for the keypair files
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    /// Issue a single license key (flat JSON, ready for `duduclaw license activate`)
    Issue {
        /// Path to the signing private key
        #[arg(short, long)]
        key: PathBuf,
        /// License tier
        #[arg(short, long)]
        tier: LicenseTier,
        /// Customer name
        #[arg(short, long)]
        customer: String,
        /// Machine fingerprint (from `duduclaw license fingerprint`)
        #[arg(short, long)]
        fingerprint: String,
        /// License duration in days (omit for perpetual)
        #[arg(short, long)]
        days: Option<u32>,
        /// Additional feature flags (comma-separated)
        #[arg(short = 'F', long)]
        features: Option<String>,
    },
    /// Batch issue licenses from a CSV file
    Batch {
        /// Path to the signing private key
        #[arg(short, long)]
        key: PathBuf,
        /// Input CSV file (columns: customer,tier,fingerprint,days,features)
        #[arg(short, long)]
        input: PathBuf,
        /// Output directory for generated license files
        #[arg(short, long, default_value = "./licenses")]
        output: PathBuf,
    },
    /// Verify a license key against the public key
    Verify {
        /// Path to the verifying public key (base64-encoded)
        #[arg(short, long)]
        key: PathBuf,
        /// License key (flat JSON string or path to .license file)
        #[arg(short, long)]
        license: String,
    },
    /// Generate a machine fingerprint for the current machine
    Fingerprint,
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Keygen { output } => cmd_keygen(&output),
        Commands::Issue {
            key,
            tier,
            customer,
            fingerprint,
            days,
            features,
        } => {
            let feature_list: Vec<String> = features
                .map(|f| f.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            cmd_issue(&key, tier, &customer, &fingerprint, days, &feature_list);
        }
        Commands::Batch { key, input, output } => cmd_batch(&key, &input, &output),
        Commands::Verify { key, license } => cmd_verify(&key, &license),
        Commands::Fingerprint => cmd_fingerprint(),
    }
}

fn cmd_keygen(output: &PathBuf) {
    let signing_key = SigningKey::generate(&mut OsRng);
    let verifying_key = signing_key.verifying_key();

    let priv_path = output.join("license-signing.key");
    let pub_path = output.join("license-verifying.pub");
    let pub_hex_path = output.join("license-verifying.hex");

    std::fs::write(&priv_path, BASE64.encode(signing_key.to_bytes()))
        .expect("Failed to write private key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&priv_path, std::fs::Permissions::from_mode(0o600))
            .expect("Failed to set private key permissions");
    }
    std::fs::write(&pub_path, BASE64.encode(verifying_key.to_bytes()))
        .expect("Failed to write public key (base64)");
    std::fs::write(&pub_hex_path, hex::encode(verifying_key.to_bytes()))
        .expect("Failed to write public key (hex)");

    println!("Keypair generated:");
    println!("  Private (base64): {}", priv_path.display());
    println!("  Public  (base64): {}", pub_path.display());
    println!("  Public  (hex):    {}", pub_hex_path.display());
    println!();
    println!("KEEP THE PRIVATE KEY SECRET.");
    println!("Deploy the hex public key to ~/.duduclaw/.license_pubkey");
    println!("  or embed it as LICENSE_PUBKEY_HEX in the duduclaw binary.");
}

fn cmd_issue(
    key_path: &PathBuf,
    tier: LicenseTier,
    customer: &str,
    fingerprint: &str,
    days: Option<u32>,
    features: &[String],
) {
    let signing_key = load_signing_key(key_path);
    let now = Utc::now();
    let expires_at = days.map(|d| now + Duration::days(i64::from(d)));

    let flat = sign_flat_license(&signing_key, tier, customer, fingerprint, now, expires_at, features);
    let json = serde_json::to_string_pretty(&flat).expect("Failed to serialize license");

    // stdout: the license key (pipe-friendly)
    println!("{json}");

    // stderr: summary
    eprintln!("\nLicense issued:");
    eprintln!("  Customer:    {customer}");
    eprintln!("  Tier:        {tier}");
    eprintln!("  Fingerprint: {fingerprint}");
    eprintln!("  Issued:      {now}");
    match expires_at {
        Some(exp) => eprintln!("  Expires:     {exp}"),
        None => eprintln!("  Expires:     perpetual"),
    }
}

fn cmd_batch(key_path: &PathBuf, input: &PathBuf, output: &PathBuf) {
    let signing_key = load_signing_key(key_path);
    std::fs::create_dir_all(output).expect("Failed to create output directory");

    let mut reader = csv::Reader::from_path(input).expect("Failed to read CSV");
    let mut count = 0u32;
    let mut errors = 0u32;

    for (line_no, result) in reader.records().enumerate() {
        let record = match result {
            Ok(r) => r,
            Err(e) => {
                eprintln!("  [SKIP] Line {}: CSV parse error: {e}", line_no + 2);
                errors += 1;
                continue;
            }
        };
        let customer = record.get(0).unwrap_or("unknown");
        let tier: LicenseTier = match record.get(1).unwrap_or("pro") {
            "community" => LicenseTier::Community,
            "enterprise" => LicenseTier::Enterprise,
            "oem" => LicenseTier::Oem,
            _ => LicenseTier::Pro,
        };
        let fingerprint = record.get(2).unwrap_or("");
        let days: Option<u32> = record.get(3).and_then(|d| d.parse().ok());
        let features: Vec<String> = record
            .get(4)
            .map(|f| f.split(';').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        let now = Utc::now();
        let expires_at = days.map(|d| now + Duration::days(i64::from(d)));

        let flat = sign_flat_license(&signing_key, tier, customer, fingerprint, now, expires_at, &features);
        let json = match serde_json::to_string_pretty(&flat) {
            Ok(j) => j,
            Err(e) => {
                eprintln!("  [FAIL] {customer}: serialize error: {e}");
                errors += 1;
                continue;
            }
        };

        let safe_name = customer.replace(|c: char| !c.is_alphanumeric(), "_");
        let file_path = output.join(format!("{safe_name}.license"));
        if let Err(e) = std::fs::write(&file_path, &json) {
            eprintln!("  [FAIL] {customer}: write error: {e}");
            errors += 1;
            continue;
        }

        count += 1;
        eprintln!("  [{count}] {customer} ({tier}) → {}", file_path.display());
    }

    eprintln!("\nBatch complete: {count} succeeded, {errors} failed in {}", output.display());
    if errors > 0 {
        std::process::exit(1);
    }
}

fn cmd_verify(key_path: &PathBuf, license_input: &str) {
    // Accept either inline JSON or a file path
    let license_json = if std::path::Path::new(license_input).exists() {
        std::fs::read_to_string(license_input).expect("Failed to read license file")
    } else {
        license_input.to_string()
    };

    let flat: FlatLicense =
        serde_json::from_str(&license_json).expect("Invalid license JSON — expected flat format");

    // Reconstruct canonical payload (same as feature_gate.rs)
    let canonical = CanonicalPayload {
        tier: flat.tier.clone(),
        customer_name: flat.customer_name.clone(),
        machine_fingerprint: flat.machine_fingerprint.clone(),
        issued_at: flat.issued_at,
        expires_at: flat.expires_at,
    };
    let canonical_bytes = serde_json::to_vec(&canonical).expect("Failed to serialize canonical");

    // Load public key
    let pub_bytes_b64 =
        std::fs::read_to_string(key_path).expect("Failed to read public key file");
    let pub_bytes = BASE64
        .decode(pub_bytes_b64.trim())
        .expect("Invalid base64 in public key");
    let pub_array: [u8; 32] = pub_bytes
        .try_into()
        .expect("Public key must be 32 bytes");
    let verifying_key =
        VerifyingKey::from_bytes(&pub_array).expect("Invalid Ed25519 public key");

    // Decode signature
    let sig_bytes = BASE64
        .decode(&flat.signature)
        .expect("Invalid base64 in signature");
    let signature = ed25519_dalek::Signature::from_slice(&sig_bytes)
        .expect("Invalid signature length (expected 64 bytes)");

    // Verify
    match verifying_key.verify(&canonical_bytes, &signature) {
        Ok(()) => {
            let now = Utc::now();
            let expired = flat.expires_at.is_some_and(|exp| now > exp);

            println!("Signature: VALID");
            println!("Customer:  {}", flat.customer_name);
            println!("Tier:      {}", flat.tier);
            println!("Issued:    {}", flat.issued_at);
            match flat.expires_at {
                Some(exp) => {
                    println!("Expires:   {exp}");
                    if expired {
                        println!("Status:    EXPIRED");
                    } else {
                        let days_left = (exp - now).num_days();
                        println!("Status:    ACTIVE ({days_left} days remaining)");
                    }
                }
                None => {
                    println!("Expires:   perpetual");
                    println!("Status:    ACTIVE (perpetual)");
                }
            }
            if !flat.features.is_empty() {
                println!("Features:  {}", flat.features.join(", "));
            }
            println!("Machine:   {}", flat.machine_fingerprint);
        }
        Err(e) => {
            eprintln!("Signature: INVALID — {e}");
            std::process::exit(1);
        }
    }
}

/// Generate machine fingerprint: SHA-256(hostname::mac)[..16] as 32 hex chars.
///
/// MUST match `build_machine_fingerprint()` in duduclaw-cli/src/main.rs
/// and `machine_fingerprint()` in feature_gate.rs.
fn cmd_fingerprint() {
    let hostname = gethostname::gethostname().to_string_lossy().to_string();
    let mac = mac_address::get_mac_address()
        .ok()
        .flatten()
        .map(|m| m.to_string())
        .unwrap_or_else(|| "00:00:00:00:00:00".into());
    let combined = format!("{hostname}::{mac}");

    let mut hasher = Sha256::new();
    hasher.update(combined.as_bytes());
    let hash = hasher.finalize();
    let fingerprint = hex::encode(&hash[..16]);

    println!("{fingerprint}");
    eprintln!("Machine:     {hostname}");
    eprintln!("MAC:         {mac}");
    eprintln!("Fingerprint: {fingerprint}");
}

// ── Helpers ──────────────────────────────────────────────

fn load_signing_key(path: &PathBuf) -> SigningKey {
    let key_b64 = std::fs::read_to_string(path).expect("Failed to read signing key file");
    let key_bytes = BASE64
        .decode(key_b64.trim())
        .expect("Invalid base64 in signing key");
    let key_array: [u8; 32] = key_bytes
        .try_into()
        .expect("Signing key must be 32 bytes");
    SigningKey::from_bytes(&key_array)
}

/// Sign a flat license key matching the format expected by feature_gate.rs.
///
/// The signature covers the canonical payload (tier + customer_name +
/// machine_fingerprint + issued_at + expires_at), serialized as JSON by serde.
/// This MUST match `build_canonical_payload()` in feature_gate.rs exactly.
fn sign_flat_license(
    signing_key: &SigningKey,
    tier: LicenseTier,
    customer_name: &str,
    machine_fingerprint: &str,
    issued_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    features: &[String],
) -> FlatLicense {
    let tier_str = tier.to_string();

    // Build canonical payload — same struct order as feature_gate.rs
    let canonical = CanonicalPayload {
        tier: tier_str.clone(),
        customer_name: customer_name.to_string(),
        machine_fingerprint: machine_fingerprint.to_string(),
        issued_at,
        expires_at,
    };
    let canonical_bytes = serde_json::to_vec(&canonical).expect("Failed to serialize canonical payload");

    // Sign canonical bytes
    let signature = signing_key.sign(&canonical_bytes);

    FlatLicense {
        tier: tier_str,
        customer_name: customer_name.to_string(),
        machine_fingerprint: machine_fingerprint.to_string(),
        issued_at,
        expires_at,
        features: features.to_vec(),
        signature: BASE64.encode(signature.to_bytes()),
    }
}
