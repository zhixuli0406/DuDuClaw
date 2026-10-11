//! Links the core as a normal dependency, without its unit-test-only fixture.
use duduclaw_core::product_profile::ProductProfile;

#[cfg(not(debug_assertions))]
#[test]
fn release_test_hooks_cannot_create_an_unprovisioned_policy() {
    match ProductProfile::resolve(None, None).unwrap() {
        ProductProfile::Standard => {
            assert!(ProductProfile::resolve(Some("restricted"), None).is_err());
            assert!(ProductProfile::resolve(None, Some("restricted")).is_err());
            assert!(ProductProfile::Restricted.check_available().is_err());
            assert!(ProductProfile::Restricted.require_channel("slack").is_err());
            assert!(ProductProfile::Restricted
                .require_runtime("claude")
                .is_err());
            assert!(ProductProfile::Restricted
                .require_provider("openai")
                .is_err());
        }
        ProductProfile::Restricted => {
            // A real private manifest takes precedence over all test fixtures.
            assert!(ProductProfile::resolve(Some("standard"), None).is_err());
            assert!(ProductProfile::resolve(None, Some("standard")).is_err());
            assert!(ProductProfile::Restricted.check_available().is_ok());
        }
    }
}
