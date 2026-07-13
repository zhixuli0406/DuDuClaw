//! First-run license seeding (E2 — enterprise Docker distribution).
//!
//! Symmetric to [`crate::branding::seed_bundle_if_absent`]: when a binary ships
//! co-located with a signed OEM `license.json` (its path handed in via the
//! `DUDUCLAW_LICENSE_FILE` env var, exactly like the compose pack mounts it),
//! verify that file's Ed25519 signature against the baked production issuer
//! registry and copy it into `<home>/license.json` **before** the license
//! runtime bootstraps. A customer who runs `docker compose up` then gets the
//! baked (unbound OEM) license with **zero** `duduclaw license activate`.
//!
//! Discipline (mirrors the branding seeder):
//!   - **idempotent** — an existing `<home>/license.json` is never overwritten
//!     (a customer who later activates their own key keeps it);
//!   - **fail-closed** — an unreadable, malformed, or signature-invalid
//!     candidate is warned once and NOT seeded (no default / unsigned write).
//!     The private issuer key is never involved; license contents are not logged.
//!
//! Note this only *plants the file*. Whether the seeded license is honoured
//! (tier, expiry, unbound-OEM machine binding, grace) is decided later by
//! `license_runtime::LicenseRuntime::bootstrap` → `License::validate`, so the
//! unbound-OEM security gate is enforced in exactly one place.

use std::path::{Path, PathBuf};

use duduclaw_license::{storage, License, PublicKeyRegistry};
use tracing::{info, warn};

/// Env var naming the seed-candidate license file (the compose pack sets it to
/// the read-only-mounted `/opt/license.json`).
pub const LICENSE_FILE_ENV: &str = "DUDUCLAW_LICENSE_FILE";

/// Outcome of [`seed_license_if_absent`], surfaced for unit-test assertions and
/// caller logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LicenseSeedOutcome {
    /// A candidate license verified and was copied into the home dir.
    Seeded { source: PathBuf },
    /// `license.json` already existed in the home dir — left untouched.
    AlreadyPresent,
    /// No candidate source (env unset/empty or the pointed-at file is missing).
    NoCandidate,
    /// The candidate failed verification (unreadable / malformed / bad
    /// signature / empty registry / persist error) — nothing was seeded.
    VerifyFailed { source: PathBuf },
}

/// First-run OEM-license seeding. Uses the baked production issuer registry
/// (identical to the gateway's license verifier) and the `DUDUCLAW_LICENSE_FILE`
/// candidate. Call once at gateway bootstrap, **before** the license runtime
/// loads `<home>/license.json`.
pub fn seed_license_if_absent(home_dir: &Path) -> LicenseSeedOutcome {
    seed_license_using(
        home_dir,
        &crate::license_runtime::production_registry(),
        license_seed_candidate(),
    )
}

/// The single candidate path: `DUDUCLAW_LICENSE_FILE` when set and non-empty.
fn license_seed_candidate() -> Option<PathBuf> {
    std::env::var_os(LICENSE_FILE_ENV)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Registry- and candidate-injectable core of [`seed_license_if_absent`], split
/// out so the idempotency / fail-closed behaviour is unit-testable with a
/// throwaway issuer key and an explicit candidate path.
fn seed_license_using(
    home_dir: &Path,
    registry: &PublicKeyRegistry,
    candidate: Option<PathBuf>,
) -> LicenseSeedOutcome {
    let dest = home_dir.join(storage::DEFAULT_LICENSE_FILENAME);
    // Idempotent: presence (not validity) of an existing license blocks seeding.
    if dest.exists() {
        return LicenseSeedOutcome::AlreadyPresent;
    }

    let source = match candidate {
        Some(p) if p.exists() => p,
        _ => return LicenseSeedOutcome::NoCandidate,
    };

    let raw = match std::fs::read(&source) {
        Ok(r) => r,
        Err(e) => {
            warn!(
                source = %source.display(),
                error = %e,
                "license seed candidate unreadable — not seeding"
            );
            return LicenseSeedOutcome::VerifyFailed { source };
        }
    };
    let license: License = match serde_json::from_slice(&raw) {
        Ok(l) => l,
        Err(e) => {
            warn!(
                source = %source.display(),
                error = %e,
                "license seed candidate malformed JSON — not seeding"
            );
            return LicenseSeedOutcome::VerifyFailed { source };
        }
    };

    // Fail-closed: no embedded issuer keys ⇒ we cannot trust anything.
    if registry.is_empty() {
        warn!(
            source = %source.display(),
            "license seed candidate present but issuer registry is empty — \
             cannot verify signature; not seeding"
        );
        return LicenseSeedOutcome::VerifyFailed { source };
    }
    if let Err(e) = registry.verify(&license) {
        warn!(
            source = %source.display(),
            error = %e,
            "license seed candidate failed signature verification — not seeding"
        );
        return LicenseSeedOutcome::VerifyFailed { source };
    }

    // Verified → persist atomically (tmp + rename, chmod 0600 on Unix). The
    // signature is over the canonical payload (not the file bytes), so a
    // re-serialised copy still verifies at bootstrap.
    if let Err(e) = storage::save_to(&license, &dest) {
        warn!(
            source = %source.display(),
            error = %e,
            "failed to persist seeded license — not seeded"
        );
        return LicenseSeedOutcome::VerifyFailed { source };
    }

    info!(
        source = %source.display(),
        tier = %license.tier,
        "seeded license.json into home dir on first run"
    );
    LicenseSeedOutcome::Seeded { source }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration as ChronoDuration;
    use duduclaw_license::LicenseTier;

    // Mirror the license_runtime test signer: `ring` (a gateway dep) produces
    // standards-compliant Ed25519 signatures that verify under the license
    // crate's `ed25519-dalek` `PublicKeyRegistry`.
    fn gen_issuer_keypair() -> (ring::signature::Ed25519KeyPair, Vec<u8>) {
        use ring::signature::KeyPair as _;
        let rng = ring::rand::SystemRandom::new();
        let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&rng).expect("pkcs8");
        let kp = ring::signature::Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("from pkcs8");
        let pubkey = kp.public_key().as_ref().to_vec();
        (kp, pubkey)
    }

    fn sign(license: &mut License, kp: &ring::signature::Ed25519KeyPair) {
        let payload = license.canonical_payload().expect("canonical payload");
        license.signature = kp.sign(&payload).as_ref().to_vec();
    }

    fn signed_oem_license(kp: &ring::signature::Ed25519KeyPair) -> License {
        let mut lic = License::new(
            "sub_oem_seed",
            "cus_oem_seed",
            LicenseTier::Oem,
            "", // unbound
            ChronoDuration::days(365),
            "v1",
        );
        lic.max_agents = Some(5);
        sign(&mut lic, kp);
        lic
    }

    #[test]
    fn seeds_verified_license_into_empty_home() {
        let tmp = tempfile::tempdir().unwrap();
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let lic = signed_oem_license(&kp);
        let src = tmp.path().join("mounted-license.json");
        std::fs::write(&src, serde_json::to_vec(&lic).unwrap()).unwrap();

        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let outcome = seed_license_using(&home, &registry, Some(src.clone()));
        assert_eq!(outcome, LicenseSeedOutcome::Seeded { source: src });

        // The dest now holds a license that re-verifies against the same registry.
        let loaded = storage::load_from(&home.join("license.json")).unwrap();
        assert!(registry.verify(&loaded).is_ok());
        assert_eq!(loaded.tier, LicenseTier::Oem);
        assert_eq!(loaded.max_agents, Some(5));
        assert!(loaded.machine_fingerprint.is_empty(), "unbound preserved");
    }

    #[test]
    fn idempotent_when_license_already_present() {
        let tmp = tempfile::tempdir().unwrap();
        let (kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        // Pre-existing license.json — must be left untouched.
        std::fs::write(home.join("license.json"), b"{\"pre\":\"existing\"}").unwrap();

        let lic = signed_oem_license(&kp);
        let src = tmp.path().join("mounted-license.json");
        std::fs::write(&src, serde_json::to_vec(&lic).unwrap()).unwrap();

        let outcome = seed_license_using(&home, &registry, Some(src));
        assert_eq!(outcome, LicenseSeedOutcome::AlreadyPresent);
        // Original bytes preserved.
        let bytes = std::fs::read(home.join("license.json")).unwrap();
        assert_eq!(bytes, b"{\"pre\":\"existing\"}");
    }

    #[test]
    fn no_candidate_when_source_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let (_kp, pubkey) = gen_issuer_keypair();
        let registry = PublicKeyRegistry::new().with_key("v1", pubkey);
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(
            seed_license_using(&home, &registry, None),
            LicenseSeedOutcome::NoCandidate
        );
        assert_eq!(
            seed_license_using(&home, &registry, Some(tmp.path().join("nope.json"))),
            LicenseSeedOutcome::NoCandidate
        );
    }

    #[test]
    fn fail_closed_on_bad_signature() {
        let tmp = tempfile::tempdir().unwrap();
        let (kp, _pubkey) = gen_issuer_keypair();
        // A DIFFERENT registry that does not trust `kp`.
        let (_other_kp, other_pubkey) = gen_issuer_keypair();
        let other_registry = PublicKeyRegistry::new().with_key("v1", other_pubkey);
        let lic = signed_oem_license(&kp);
        let src = tmp.path().join("mounted-license.json");
        std::fs::write(&src, serde_json::to_vec(&lic).unwrap()).unwrap();

        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let outcome = seed_license_using(&home, &other_registry, Some(src.clone()));
        assert_eq!(outcome, LicenseSeedOutcome::VerifyFailed { source: src });
        // Nothing was written.
        assert!(!home.join("license.json").exists());
    }

    #[test]
    fn fail_closed_on_empty_registry() {
        let tmp = tempfile::tempdir().unwrap();
        let (kp, _pubkey) = gen_issuer_keypair();
        let empty = PublicKeyRegistry::new();
        let lic = signed_oem_license(&kp);
        let src = tmp.path().join("mounted-license.json");
        std::fs::write(&src, serde_json::to_vec(&lic).unwrap()).unwrap();

        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).unwrap();
        let outcome = seed_license_using(&home, &empty, Some(src.clone()));
        assert_eq!(outcome, LicenseSeedOutcome::VerifyFailed { source: src });
        assert!(!home.join("license.json").exists());
    }
}
