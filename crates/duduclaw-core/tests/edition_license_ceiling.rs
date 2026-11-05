//! Exercise the public library API, including release artifacts built with assertions.
use duduclaw_core::EditionProfile::{self, Personal, Enterprise};

#[test]
fn production_resolver_never_elevates() {
    for requested in ["enterprise", "enterprise_edition", " ENTERPRISE "] {
        for tier in [None, Some("personal_pro_self_host"), Some("invalid")] {
            assert_eq!(EditionProfile::resolve(Some(requested), None, tier), Personal);
            assert_eq!(EditionProfile::resolve(None, Some(requested), tier), Personal);
            assert_eq!(EditionProfile::resolve(Some(requested), Some(requested), tier), Personal);
        }
    }
}

#[test]
fn licensed_enterprise_and_explicit_downgrades_remain_available() {
    for tier in ["business", "oem", "partner", "self_host_pro"] {
        assert_eq!(EditionProfile::resolve(None, None, Some(tier)), Enterprise);
        assert_eq!(EditionProfile::resolve(Some("enterprise"), None, Some(tier)), Enterprise);
        assert_eq!(EditionProfile::resolve(Some("personal"), None, Some(tier)), Personal);
        assert_eq!(EditionProfile::resolve(None, Some("personal"), Some(tier)), Personal);
    }
}
