//! DuDuClaw License Key Generator (internal tool)
//!
//! Generates Ed25519-signed license keys for commercial customers.
//! This binary is NOT distributed — used only by DuDuClaw maintainers.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey, VerifyingKey};
use rand::rngs::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::PathBuf;

/// License tier matching the duduclaw-license crate definition.
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

/// License payload that gets signed.
#[derive(Debug, Serialize, Deserialize)]
struct LicensePayload {
    version: u8,
    tier: LicenseTier,
    customer_name: String,
    machine_fingerprint: String,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    features: Vec<String>,
}

/// Signed license key (payload + signature).
#[derive(Debug, Serialize, Deserialize)]
struct SignedLicense {
    payload: String, // base64-encoded JSON payload
    signature: String, // base64-encoded Ed25519 signature
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
    /// Issue a single license key
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
        /// Machine fingerprint (MAC + hostname hash)
        #[arg(short, long)]
        fingerprint: String,
        /// License duration in days
        #[arg(short, long, default_value = "365")]
        days: u32,
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
        /// Path to the verifying public key
        #[arg(short, long)]
        key: PathBuf,
        /// License key string (base64)
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
            let feature_list = features
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

    std::fs::write(&priv_path, BASE64.encode(signing_key.to_bytes()))
        .expect("Failed to write private key");
    std::fs::write(&pub_path, BASE64.encode(verifying_key.to_bytes()))
        .expect("Failed to write public key");

    println!("Keypair generated:");
    println!("  Private: {}", priv_path.display());
    println!("  Public:  {}", pub_path.display());
    println!("\nKEEP THE PRIVATE KEY SECRET. Embed the public key in the duduclaw binary.");
}

fn cmd_issue(
    key_path: &PathBuf,
    tier: LicenseTier,
    customer: &str,
    fingerprint: &str,
    days: u32,
    features: &[String],
) {
    let signing_key = load_signing_key(key_path);
    let now = Utc::now();

    let payload = LicensePayload {
        version: 1,
        tier,
        customer_name: customer.to_string(),
        machine_fingerprint: fingerprint.to_string(),
        issued_at: now,
        expires_at: now + Duration::days(i64::from(days)),
        features: features.to_vec(),
    };

    let signed = sign_license(&signing_key, &payload);
    let license_json = serde_json::to_string(&signed).expect("Failed to serialize license");
    let license_b64 = BASE64.encode(license_json.as_bytes());

    println!("{license_b64}");
    eprintln!("\nLicense issued:");
    eprintln!("  Customer:    {customer}");
    eprintln!("  Tier:        {tier}");
    eprintln!("  Fingerprint: {fingerprint}");
    eprintln!("  Issued:      {}", payload.issued_at);
    eprintln!("  Expires:     {}", payload.expires_at);
}

fn cmd_batch(key_path: &PathBuf, input: &PathBuf, output: &PathBuf) {
    let signing_key = load_signing_key(key_path);
    std::fs::create_dir_all(output).expect("Failed to create output directory");

    let mut reader = csv::Reader::from_path(input).expect("Failed to read CSV");
    let mut count = 0u32;

    for result in reader.records() {
        let record = result.expect("Failed to parse CSV row");
        let customer = record.get(0).unwrap_or("unknown");
        let tier: LicenseTier = match record.get(1).unwrap_or("pro") {
            "community" => LicenseTier::Community,
            "enterprise" => LicenseTier::Enterprise,
            "oem" => LicenseTier::Oem,
            _ => LicenseTier::Pro,
        };
        let fingerprint = record.get(2).unwrap_or("");
        let days: u32 = record
            .get(3)
            .and_then(|d| d.parse().ok())
            .unwrap_or(365);
        let features: Vec<String> = record
            .get(4)
            .map(|f| f.split(';').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        let now = Utc::now();
        let payload = LicensePayload {
            version: 1,
            tier,
            customer_name: customer.to_string(),
            machine_fingerprint: fingerprint.to_string(),
            issued_at: now,
            expires_at: now + Duration::days(i64::from(days)),
            features,
        };

        let signed = sign_license(&signing_key, &payload);
        let license_json = serde_json::to_string(&signed).expect("Serialize failed");
        let license_b64 = BASE64.encode(license_json.as_bytes());

        let safe_name = customer.replace(|c: char| !c.is_alphanumeric(), "_");
        let file_path = output.join(format!("{safe_name}.license"));
        std::fs::write(&file_path, &license_b64).expect("Failed to write license file");

        count += 1;
        eprintln!("  [{count}] {customer} ({tier}) → {}", file_path.display());
    }

    eprintln!("\nBatch complete: {count} licenses generated in {}", output.display());
}

fn cmd_verify(key_path: &PathBuf, license_b64: &str) {
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

    let license_json_bytes = BASE64
        .decode(license_b64.trim())
        .expect("Invalid base64 in license");
    let license_json = String::from_utf8(license_json_bytes).expect("Invalid UTF-8 in license");
    let signed: SignedLicense =
        serde_json::from_str(&license_json).expect("Invalid license JSON");

    let payload_bytes = BASE64
        .decode(&signed.payload)
        .expect("Invalid payload base64");
    let sig_bytes = BASE64
        .decode(&signed.signature)
        .expect("Invalid signature base64");
    let signature = ed25519_dalek::Signature::from_slice(&sig_bytes)
        .expect("Invalid signature length");

    match verifying_key.verify_strict(&payload_bytes, &signature) {
        Ok(()) => {
            let payload: LicensePayload =
                serde_json::from_slice(&payload_bytes).expect("Invalid payload JSON");
            let now = Utc::now();
            let expired = now > payload.expires_at;

            println!("Signature: VALID");
            println!("Customer:  {}", payload.customer_name);
            println!("Tier:      {}", payload.tier);
            println!("Issued:    {}", payload.issued_at);
            println!("Expires:   {}", payload.expires_at);
            if expired {
                println!("Status:    EXPIRED");
            } else {
                let days_left = (payload.expires_at - now).num_days();
                println!("Status:    ACTIVE ({days_left} days remaining)");
            }
        }
        Err(e) => {
            eprintln!("Signature: INVALID — {e}");
            std::process::exit(1);
        }
    }
}

fn cmd_fingerprint() {
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string());

    // Use hostname + a stable machine identifier as fingerprint source
    let mut hasher = Sha256::new();
    hasher.update(hostname.as_bytes());
    // On macOS, use IOPlatformSerialNumber; on Linux, /etc/machine-id
    // For now, just use hostname as the base
    let hash = hasher.finalize();
    let fingerprint = hex::encode(&hash[..16]); // 128-bit truncated

    println!("{fingerprint}");
    eprintln!("Machine: {hostname}");
    eprintln!("Fingerprint: {fingerprint}");
}

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

fn sign_license(signing_key: &SigningKey, payload: &LicensePayload) -> SignedLicense {
    let payload_json = serde_json::to_string(payload).expect("Failed to serialize payload");
    let payload_b64 = BASE64.encode(payload_json.as_bytes());
    let signature = signing_key.sign(payload_json.as_bytes());

    SignedLicense {
        payload: payload_b64,
        signature: BASE64.encode(signature.to_bytes()),
    }
}
