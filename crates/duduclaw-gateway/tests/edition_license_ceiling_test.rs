//! Each case owns a process: no environment or global-license races.
use duduclaw_auth::UserContext;
use duduclaw_core::EditionProfile;
use duduclaw_gateway::{handlers::MethodHandler, license_runtime::{self, LicenseRuntime}, protocol::WsFrame};
use duduclaw_license::{License, LicenseTier, PublicKeyRegistry};
use ed25519_dalek::Signer;
use serde_json::json;

#[tokio::test]
async fn edition_overrides_cannot_elevate_personal_license_rpc() {
    const CASE: &str = "DUDUCLAW_EDITION_CEILING_TEST_CASE";
    let Ok(case) = std::env::var(CASE) else {
        for license in ["personal", "missing", "invalid", "business"] {
            for setting in ["env", "config", "downgrade"] {
                let mut command = std::process::Command::new(std::env::current_exe().unwrap());
                command.args(["--exact", "edition_overrides_cannot_elevate_personal_license_rpc", "--nocapture"])
                    .env(CASE, format!("{license}:{setting}"))
                    .env("DUDUCLAW_DEPLOYMENT", if matches!(license, "business" | "invalid") { "cloud" } else { "self-host" });
                if setting == "config" {
                    command.env_remove("DUDUCLAW_EDITION");
                } else {
                    command.env("DUDUCLAW_EDITION", if setting == "downgrade" { "personal" } else { "enterprise" });
                }
                let output = command.output().unwrap();
                assert!(output.status.success(), "{license}:{setting}: {}\n{}",
                    String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            }
        }
        return;
    };
    let (kind, setting) = case.split_once(':').unwrap();
    let home = tempfile::tempdir().unwrap();
    let key = ed25519_dalek::SigningKey::generate(&mut rand::rngs::OsRng);
    let registry = PublicKeyRegistry::new().with_key("edition-test", key.verifying_key().to_bytes().to_vec());
    let tier = if matches!(kind, "business" | "invalid") { LicenseTier::Business } else { LicenseTier::PersonalProSelfHost };
    if kind != "missing" {
        let mut license = License::new("sub_edition_test", "fixture", tier,
            duduclaw_license::generate_fingerprint(), chrono::Duration::days(1), "edition-test");
        license.nfr = true;
        license.signature = key.sign(&license.canonical_payload().unwrap()).to_bytes().to_vec();
        if kind == "invalid" { license.signature[0] ^= 1; }
        duduclaw_license::storage::save_to(&license, &home.path().join("license.json")).unwrap();
    }
    let runtime = LicenseRuntime::bootstrap(home.path().into(), registry).await;
    let expected_tier = if matches!(kind, "missing" | "invalid") { LicenseTier::OpenSource } else { tier };
    assert_eq!(runtime.current_tier().await, expected_tier);
    license_runtime::set_global(runtime);
    let handler = MethodHandler::new(home.path().into()).await;
    if setting == "config" {
        handler.set_edition_override(Some(EditionProfile::Enterprise)).await;
    }
    let refused = setting == "downgrade" || kind != "business";
    for method in ["users.list", "users.create", "departments.list"] {
        let frame = handler.handle(method, json!({}), &UserContext::admin_fallback()).await;
        let code = match &frame {
            WsFrame::Response { error: Some(error), .. } => error.get("code").and_then(|c| c.as_str()),
            _ => None,
        };
        assert_eq!(code == Some("enterprise_edition_required"), refused, "{case}: {method}: {frame:?}");
    }
}
