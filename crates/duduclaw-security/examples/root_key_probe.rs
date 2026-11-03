//! Container acceptance probe. Never prints key material or enables protection.
use duduclaw_security::credential_protection::CredentialProtection;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = duduclaw_core::ns_guard::harden_gateway_process() {
        eprintln!("gateway_hardening_error={error}");
    }
    println!("root_key_state={:?} refusal_reason={:?}",
        duduclaw_core::root_key::state_kind(), duduclaw_core::root_key::refused_reason());
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let home = std::env::temp_dir().join(format!("duduclaw-root-key-probe-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&home)?;
    let status = CredentialProtection::platform(&home).status();
    println!("{}", serde_json::json!({ "state": status.state, "level": status.level,
        "platform_level": status.platform_level, "backend": status.backend }));
    std::fs::remove_dir(&home)?;
    Ok(())
}
