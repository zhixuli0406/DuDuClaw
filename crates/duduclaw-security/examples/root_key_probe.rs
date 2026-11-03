//! Container acceptance probe. Never prints key material or enables protection.
use duduclaw_security::credential_protection::{CredentialProtection, ProtectionStatus};

fn print_status(status: &ProtectionStatus) {
    println!("{}", serde_json::json!({ "state": status.state, "level": status.level,
        "platform_level": status.platform_level, "backend": status.backend }));
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    if let Err(error) = duduclaw_core::ns_guard::harden_gateway_process() {
        eprintln!("gateway_hardening_error={error}");
    }
    println!("root_key_state={:?} refusal_reason={:?}",
        duduclaw_core::root_key::state_kind(), duduclaw_core::root_key::refused_reason());
    let full = std::env::var("ROOT_KEY_PROBE_FULL").ok().as_deref() == Some("1");
    let home = if full {
        std::path::PathBuf::from(std::env::var("ROOT_KEY_PROBE_HOME")?)
    } else {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
        std::env::temp_dir().join(format!("duduclaw-root-key-probe-{}-{nonce}", std::process::id()))
    };
    if !full { std::fs::create_dir(&home)?; }
    let protection = CredentialProtection::platform(&home);
    let mut status = protection.status();
    print_status(&status);
    if full {
        if status.state.code() == "not_enabled" {
            match protection.enable(false) {
                Ok(enabled) => { println!("enable_code=ok"); status = enabled; }
                Err(error) => println!("enable_code={} message_zh={}", error.code, error.message_zh),
            }
            print_status(&status);
        }
        println!("person_identity_allowed={}", status.person_identity_allowed);
        let attack = std::env::var("ROOT_KEY_PROBE_ATTACK")?;
        let result = std::process::Command::new(attack)
            .arg(std::process::id().to_string())
            .env_clear().env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", &home).status()?;
        println!("attack_exit_code={:?}", result.code());
        status = protection.status();
        print_status(&status);
    } else {
        std::fs::remove_dir(&home)?;
    }
    Ok(())
}
