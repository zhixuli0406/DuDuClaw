//! Nonsecret metadata protocol for the host-only Personal account selector.
//! A stamp is kept in private dispatcher state, never read from model params.
use super::CredentialRef;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const GOOGLE_SELECTION_ROUTE: &str = "/api/internal/google-selection";
pub const GOOGLE_SELECTION_TTL_SECS: u64 = 1800;
pub const GOOGLE_ACCOUNT_APPROVAL_KEY: &str = "host_google_account";

/// Purpose is deliberately closed and distinct from an execute body. The
/// existing host MAC signs bytes, not the URL, so moving a signed select/bind
/// body to the effect route must fail deserialization there.
#[derive(Serialize, Deserialize)]
#[serde(tag = "purpose", rename_all = "snake_case", deny_unknown_fields)]
pub enum GoogleSelectionRequest {
    SelectGoogleAccount {
        tool: String,
        arguments: Value,
        credential_ref: CredentialRef,
    },
    BindGoogleArguments {
        opaque_id: String,
        tool: String,
        credential_ref: CredentialRef,
        original_arguments_digest: String,
        account_binding_digest: String,
        arguments: Value,
    },
}
impl GoogleSelectionRequest {
    pub fn tool(&self) -> &str {
        match self {
            Self::SelectGoogleAccount { tool, .. } | Self::BindGoogleArguments { tool, .. } => tool,
        }
    }
    pub fn credential_ref(&self) -> &CredentialRef {
        match self {
            Self::SelectGoogleAccount { credential_ref, .. }
            | Self::BindGoogleArguments { credential_ref, .. } => credential_ref,
        }
    }
    pub fn arguments(&self) -> &Value {
        match self {
            Self::SelectGoogleAccount { arguments, .. }
            | Self::BindGoogleArguments { arguments, .. } => arguments,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovedGoogleIdentity {
    Personal,
    #[serde(rename = "home_oauth_fallback")]
    HomeOAuthFallback,
    HomeServiceAccountFallback,
    HomeAppsScriptFallback,
}

/// Display/binding metadata only. This cannot reconstruct a sealed Home
/// receipt, material or claim and never asserts a verified Google subject.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleHomeConnectionApproval {
    pub credential_id: String,
    pub logical_epoch: u64,
    pub source_generation: String,
    pub original_selector: String,
    pub original_unavailable: String,
}
impl GoogleHomeConnectionApproval {
    fn is_well_formed(&self) -> bool {
        let uuid = |s: &str| uuid::Uuid::parse_str(s).is_ok_and(|u| u.to_string() == s);
        uuid(&self.credential_id)
            && self.logical_epoch > 0
            && uuid(&self.source_generation)
            && matches!(self.original_selector.as_str(), "owner" | "user")
            && matches!(
                self.original_unavailable.as_str(),
                "no_owner"
                    | "ambiguous_owner"
                    | "person_inactive"
                    | "awaiting_consent"
                    | "not_connected"
                    | "needs_reconnect"
            )
    }
}

/// Additive non-OAuth source metadata. No invented OAuth identity or epoch.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum GoogleHomeSourceApproval {
    ServiceAccount {
        source_binding_digest: String,
        source_generation: Option<String>,
        declared_subject: Option<String>,
        original_selector: String,
        original_unavailable: String,
    },
    AppsScript {
        source_binding_digest: String,
        source_generation: Option<String>,
        original_selector: String,
        original_unavailable: String,
    },
}
impl GoogleHomeSourceApproval {
    fn valid(&self) -> bool {
        let (digest, generation, selector, unavailable) = match self {
            Self::ServiceAccount {
                source_binding_digest,
                source_generation,
                declared_subject,
                original_selector,
                original_unavailable,
            } => {
                if declared_subject.as_ref().is_some_and(|s| {
                    s.len() > 254
                        || s != s.trim()
                        || s.chars().any(char::is_control)
                        || super::normalize_email(s).is_err()
                }) {
                    return false;
                }
                (
                    source_binding_digest,
                    source_generation,
                    original_selector,
                    original_unavailable,
                )
            }
            Self::AppsScript {
                source_binding_digest,
                source_generation,
                original_selector,
                original_unavailable,
            } => (
                source_binding_digest,
                source_generation,
                original_selector,
                original_unavailable,
            ),
        };
        google_digest_well_formed(digest)
            && generation
                .as_ref()
                .is_none_or(|s| uuid::Uuid::parse_str(s).is_ok_and(|u| u.to_string() == *s))
            && matches!(selector.as_str(), "owner" | "user")
            && matches!(
                unavailable.as_str(),
                "no_owner"
                    | "ambiguous_owner"
                    | "person_inactive"
                    | "awaiting_consent"
                    | "not_connected"
                    | "needs_reconnect"
            )
    }
    pub fn retained_capacity(&self) -> usize {
        let mut n = std::mem::size_of_val(self);
        match self {
            Self::ServiceAccount {
                source_binding_digest,
                source_generation,
                declared_subject,
                original_selector,
                original_unavailable,
            } => {
                for s in [
                    source_binding_digest,
                    original_selector,
                    original_unavailable,
                ] {
                    n += s.capacity();
                }
                n += source_generation.as_ref().map_or(0, String::capacity)
                    + declared_subject.as_ref().map_or(0, String::capacity);
            }
            Self::AppsScript {
                source_binding_digest,
                source_generation,
                original_selector,
                original_unavailable,
            } => {
                for s in [
                    source_binding_digest,
                    original_selector,
                    original_unavailable,
                ] {
                    n += s.capacity();
                }
                n += source_generation.as_ref().map_or(0, String::capacity);
            }
        }
        n
    }
}

/// Selected credential owner and requester are separate facts. This protocol
/// does not grant a Personal call permission to switch to Home on failure.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleAccountApproval {
    pub selected_identity: ApprovedGoogleIdentity,
    pub selected_google_account: String,
    pub selected_credential_owner: String,
    pub selected_person_login: String,
    pub requester_kind: String,
    pub configured_home_fallback: bool,
    pub home_fallback_authorized: bool,
    pub account_binding_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_connection: Option<GoogleHomeConnectionApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_source: Option<GoogleHomeSourceApproval>,
}
impl std::fmt::Debug for GoogleAccountApproval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleAccountApproval")
            .field("selected_identity", &self.selected_identity)
            .field("home_fallback_authorized", &self.home_fallback_authorized)
            .finish_non_exhaustive()
    }
}
impl GoogleAccountApproval {
    pub fn is_well_formed(&self) -> bool {
        if self.requester_kind != "unknown"
            || !google_digest_well_formed(&self.account_binding_digest)
        {
            return false;
        }
        match self.selected_identity {
            ApprovedGoogleIdentity::Personal => {
                self.home_connection.is_none()
                    && self.home_source.is_none()
                    && super::normalize_email(&self.selected_google_account)
                        .is_ok_and(|s| s == self.selected_google_account)
                    && !self.selected_credential_owner.is_empty()
                    && self.selected_credential_owner.len() <= 128
                    && self
                        .selected_credential_owner
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    && !self.selected_person_login.is_empty()
                    && self.selected_person_login.len() <= 1024
                    && !self.selected_person_login.chars().any(char::is_control)
                    && !self.home_fallback_authorized
            }
            ApprovedGoogleIdentity::HomeOAuthFallback => {
                self.home_source.is_none()
                    && self.configured_home_fallback
                    && self.home_fallback_authorized
                    && self.selected_google_account.is_empty()
                    && self.selected_credential_owner.is_empty()
                    && self.selected_person_login.is_empty()
                    && self
                        .home_connection
                        .as_ref()
                        .is_some_and(GoogleHomeConnectionApproval::is_well_formed)
            }
            ApprovedGoogleIdentity::HomeServiceAccountFallback
            | ApprovedGoogleIdentity::HomeAppsScriptFallback => {
                self.configured_home_fallback
                    && self.home_fallback_authorized
                    && self.selected_google_account.is_empty()
                    && self.selected_credential_owner.is_empty()
                    && self.selected_person_login.is_empty()
                    && self.home_connection.is_none()
                    && self.home_source.as_ref().is_some_and(|source| {
                        source.valid()
                            && matches!(
                                (self.selected_identity, source),
                                (
                                    ApprovedGoogleIdentity::HomeServiceAccountFallback,
                                    GoogleHomeSourceApproval::ServiceAccount { .. }
                                ) | (
                                    ApprovedGoogleIdentity::HomeAppsScriptFallback,
                                    GoogleHomeSourceApproval::AppsScript { .. }
                                )
                            )
                    })
            }
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleSelectionStamp {
    pub opaque_id: String,
    pub expires_at: u64,
    pub original_arguments_digest: String,
    pub final_arguments_digest: Option<String>,
    pub approval: GoogleAccountApproval,
}
impl std::fmt::Debug for GoogleSelectionStamp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoogleSelectionStamp")
            .field("expires_at", &self.expires_at)
            .field("bound", &self.final_arguments_digest.is_some())
            .finish_non_exhaustive()
    }
}
impl GoogleSelectionStamp {
    pub fn is_well_formed(&self) -> bool {
        uuid::Uuid::parse_str(&self.opaque_id).is_ok_and(|id| id.to_string() == self.opaque_id)
            && self.expires_at > 0
            && google_digest_well_formed(&self.original_arguments_digest)
            && self
                .final_arguments_digest
                .as_deref()
                .is_none_or(google_digest_well_formed)
            && self.approval.is_well_formed()
    }
}

pub fn google_digest_well_formed(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
/// Canonicalize object keys independently of serde_json's map feature flags.
/// Array order, scalar types and values remain part of the actual call.
pub fn google_arguments_digest(arguments: &Value) -> String {
    fn canonical(v: &Value) -> Value {
        match v {
            Value::Object(map) => {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by(|a, b| a.0.cmp(b.0));
                Value::Object(
                    entries
                        .into_iter()
                        .map(|(k, v)| (k.clone(), canonical(v)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.iter().map(canonical).collect()),
            other => other.clone(),
        }
    }
    format!(
        "{:x}",
        Sha256::digest(canonical(arguments).to_string().as_bytes())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_purpose_and_extra_credentials_are_not_accepted() {
        let reference = serde_json::json!({"kind":"legacy","sha256":"a".repeat(64)});
        for value in [
            serde_json::json!({"purpose":"execute_google","tool":"gmail_read","arguments":{},"credential_ref":reference}),
            serde_json::json!({"purpose":"select_google_account","tool":"gmail_read","arguments":{},"credential_ref":reference,"access_token":"must-not-be-wire-data"}),
            serde_json::json!({"tool":"gmail_read","arguments":{},"credential_ref":reference}),
        ] {
            assert!(serde_json::from_value::<GoogleSelectionRequest>(value).is_err());
        }
    }
    #[test]
    fn canonical_digest_preserves_values_and_array_order() {
        assert_eq!(
            google_arguments_digest(&serde_json::json!({"b":2,"a":{"z":1,"x":3}})),
            google_arguments_digest(&serde_json::json!({"a":{"x":3,"z":1},"b":2}))
        );
        assert_ne!(
            google_arguments_digest(&serde_json::json!([1, 2])),
            google_arguments_digest(&serde_json::json!([2, 1]))
        );
    }
}

#[cfg(test)]
mod home_wire_tests {
    use super::*;
    fn personal() -> serde_json::Value {
        serde_json::json!({"selected_identity":"personal","selected_google_account":"alice@unit.test","selected_credential_owner":"owner-1","selected_person_login":"login@unit.test","requester_kind":"unknown","configured_home_fallback":true,"home_fallback_authorized":false,"account_binding_digest":"a".repeat(64)})
    }
    #[test]
    fn original_personal_wire_roundtrip_has_no_added_home_field() {
        let original = personal();
        let parsed: GoogleAccountApproval = serde_json::from_value(original.clone()).unwrap();
        assert!(parsed.is_well_formed());
        assert_eq!(serde_json::to_value(parsed).unwrap(), original);
    }
    #[test]
    fn logical_home_wire_has_no_invented_subject_and_is_not_personal_approval() {
        let mut original = personal();
        original["selected_identity"] = serde_json::json!("home_oauth_fallback");
        for field in [
            "selected_google_account",
            "selected_credential_owner",
            "selected_person_login",
        ] {
            original[field] = serde_json::json!("");
        }
        original["home_fallback_authorized"] = serde_json::json!(true);
        original["home_connection"] = serde_json::json!({"credential_id":uuid::Uuid::new_v4().to_string(),"logical_epoch":1,"source_generation":uuid::Uuid::new_v4().to_string(),"original_selector":"owner","original_unavailable":"not_connected"});
        let approval: GoogleAccountApproval = serde_json::from_value(original.clone()).unwrap();
        assert!(approval.is_well_formed());
        original["selected_google_account"] = serde_json::json!("invented@unit.test");
        assert!(
            !serde_json::from_value::<GoogleAccountApproval>(original)
                .unwrap()
                .is_well_formed()
        );
    }
}

#[cfg(test)]
mod w3_wire_oracles {
    use super::*;
    fn approval() -> serde_json::Value {
        serde_json::json!({"selected_identity":"home_service_account_fallback","selected_google_account":"","selected_credential_owner":"","selected_person_login":"","requester_kind":"unknown","configured_home_fallback":true,"home_fallback_authorized":true,"account_binding_digest":"a".repeat(64),"home_source":{"kind":"service_account","source_binding_digest":"b".repeat(64),"source_generation":null,"declared_subject":"declared@unit.test","original_selector":"owner","original_unavailable":"not_connected"}})
    }
    #[test]
    fn w3_new_tags_are_truthful_and_cannot_mix_oauth_or_other_source_fields() {
        let raw = approval();
        let parsed: GoogleAccountApproval = serde_json::from_value(raw.clone()).unwrap();
        assert!(parsed.is_well_formed());
        assert!(parsed.home_connection.is_none());
        assert!(
            !serde_json::to_value(&parsed)
                .unwrap()
                .as_object()
                .unwrap()
                .contains_key("home_connection")
        );
        let mut wrong = raw.clone();
        wrong["selected_identity"] = serde_json::json!("home_apps_script_fallback");
        assert!(
            !serde_json::from_value::<GoogleAccountApproval>(wrong)
                .unwrap()
                .is_well_formed()
        );
        let mut wrong = raw.clone();
        wrong["home_source"]["logical_epoch"] = serde_json::json!(1);
        assert!(serde_json::from_value::<GoogleAccountApproval>(wrong).is_err());
        let mut wrong = raw;
        wrong["selected_google_account"] = serde_json::json!("invented@unit.test");
        assert!(
            !serde_json::from_value::<GoogleAccountApproval>(wrong)
                .unwrap()
                .is_well_formed()
        );
    }
    #[test]
    fn w3_apps_script_accepts_no_declared_subject_or_oauth_epoch() {
        let mut raw = approval();
        raw["selected_identity"] = serde_json::json!("home_apps_script_fallback");
        raw["home_source"]["kind"] = serde_json::json!("apps_script");
        raw["home_source"]
            .as_object_mut()
            .unwrap()
            .remove("declared_subject");
        let parsed: GoogleAccountApproval = serde_json::from_value(raw.clone()).unwrap();
        assert!(parsed.is_well_formed());
        raw["home_source"]["declared_subject"] = serde_json::json!("fake@unit.test");
        assert!(serde_json::from_value::<GoogleAccountApproval>(raw).is_err());
    }
}
