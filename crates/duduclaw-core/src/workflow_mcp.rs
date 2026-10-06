//! Versioned service-only MCP workflow frames. Arguments never contain authority.
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const VERSION: u32 = 1;
pub const MAX_FRAME_BYTES: usize = 262_144;
pub const META_KEY: &str = "duduclaw_workflow";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationAuthority {
    BoundHumanApproval {
        approval_id: String,
    },
    ActiveWorkflowRevisionGrant {
        grant_id: String,
        epoch: i64,
        spec_hash: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EffectiveToolCall {
    pub version: u32,
    pub actor: String,
    pub session_id: String,
    pub run_id: String,
    pub step_key: String,
    pub tool: String,
    pub effective_arguments: Value,
    pub payload_hash: String,
    pub policy_revision: String,
    pub authority_digest: String,
    pub environment_hash: String,
    pub approval_requirements: Vec<String>,
    pub expires_at: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PrepareTicket {
    pub effective: EffectiveToolCall,
    pub nonce: String,
    pub mac: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExecuteTicket {
    pub version: u32,
    pub session_id: String,
    pub operation_id: String,
    pub binding_digest: String,
    pub prepare_digest: String,
    pub expires_at: String,
    pub mac: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OperationReply {
    pub version: u32,
    pub operation_id: String,
    pub binding_digest: String,
    pub authority_source: OperationAuthority,
    pub state: String,
    pub receipt_digest: Option<String>,
    pub result: Option<Value>,
    pub error_code: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrepareContext {
    pub run_id: String,
    pub step_key: String,
    pub input_hash: String,
    pub session_id: String,
    pub expires_at: String,
    pub mac: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkflowMetadata {
    Prepare {
        version: u32,
        context: PrepareContext,
    },
    Execute {
        version: u32,
        ticket: ExecuteTicket,
    },
    Read {
        version: u32,
        context: PrepareContext,
    },
}
impl WorkflowMetadata {
    pub fn validate(&self) -> Result<(), String> {
        let version = match self {
            Self::Prepare { version, .. }
            | Self::Execute { version, .. }
            | Self::Read { version, .. } => *version,
        };
        if version != VERSION {
            return Err("unsupported workflow extension version".into());
        }
        Ok(())
    }
}

/// Stable sorted-object JSON shared by both processes. No secret material is logged.
pub fn canonical_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    fn sort(value: Value) -> Value {
        match value {
            Value::Object(obj) => {
                let sorted: std::collections::BTreeMap<_, _> =
                    obj.into_iter().map(|(k, v)| (k, sort(v))).collect();
                Value::Object(sorted.into_iter().collect())
            }
            Value::Array(items) => Value::Array(items.into_iter().map(sort).collect()),
            other => other,
        }
    }
    let bytes = serde_json::to_vec(&sort(
        serde_json::to_value(value).map_err(|e| e.to_string())?,
    ))
    .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("workflow frame exceeds bound".into());
    }
    Ok(bytes)
}
pub fn digest<T: Serialize>(value: &T) -> Result<String, String> {
    Ok(format!("{:x}", Sha256::digest(canonical_bytes(value)?)))
}
pub fn sign<T: Serialize>(secret: &[u8], domain: &str, value: &T) -> Result<String, String> {
    if secret.len() != 32 {
        return Err("workflow session key must be 256 bits".into());
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|e| e.to_string())?;
    mac.update(b"duduclaw-workflow-v1\0");
    mac.update(domain.as_bytes());
    mac.update(b"\0");
    mac.update(&canonical_bytes(value)?);
    Ok(mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
pub fn verify<T: Serialize>(
    secret: &[u8],
    domain: &str,
    value: &T,
    tag: &str,
) -> Result<(), String> {
    if secret.len() != 32 || tag.len() != 64 || !tag.is_ascii() {
        return Err("invalid workflow MAC".into());
    }
    let raw: Result<Vec<u8>, _> = (0..64)
        .step_by(2)
        .map(|i| u8::from_str_radix(&tag[i..i + 2], 16))
        .collect();
    let raw = raw.map_err(|_| "invalid workflow MAC")?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|e| e.to_string())?;
    mac.update(b"duduclaw-workflow-v1\0");
    mac.update(domain.as_bytes());
    mac.update(b"\0");
    mac.update(&canonical_bytes(value)?);
    mac.verify_slice(&raw)
        .map_err(|_| "invalid workflow MAC".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn workflow_canonical_fixture_matches_cross_crate_authority_and_environment() {
        let fixture: Value =
            serde_json::from_str(include_str!("workflow_mcp_fixture.json")).unwrap();
        for key in ["authority_human", "authority_grant", "environment"] {
            assert_eq!(
                digest(&fixture[key]).unwrap(),
                fixture["digests"][key].as_str().unwrap()
            );
        }
        for key in ["authority_human", "authority_grant"] {
            let source: OperationAuthority = serde_json::from_value(fixture[key].clone()).unwrap();
            assert_eq!(serde_json::to_value(source).unwrap(), fixture[key]);
        }
    }
    #[test]
    fn workflow_mac_binds_domain_and_payload() {
        let key = [7; 32];
        let value = serde_json::json!({"actor":"a","run":"r"});
        let tag = sign(&key, "prepare", &value).unwrap();
        assert!(verify(&key, "prepare", &value, &tag).is_ok());
        assert!(verify(&key, "execute", &value, &tag).is_err());
        assert!(
            verify(
                &key,
                "prepare",
                &serde_json::json!({"actor":"b","run":"r"}),
                &tag
            )
            .is_err()
        );
    }
    #[test]
    fn workflow_metadata_rejects_unknown_fields_and_versions() {
        assert!(
            serde_json::from_value::<WorkflowMetadata>(
                serde_json::json!({"phase":"execute","version":1,"ticket":{},"approved":true})
            )
            .is_err()
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReadReply {
    pub version: u32,
    pub run_id: String,
    pub step_key: String,
    pub tool: String,
    pub actor: String,
    pub payload_hash: String,
    pub policy_revision: String,
    pub authority_digest: String,
    pub environment_hash: String,
    pub observed_at: String,
    pub result: Value,
    pub result_hash: String,
    pub evidence: Value,
    pub error_code: Option<String>,
}
/// Signed DTOs omit only their own MAC; nested payload fields remain bound.
pub fn unsigned<T: Serialize>(value: &T) -> Result<Value, String> {
    let mut value = serde_json::to_value(value).map_err(|e| e.to_string())?;
    value
        .as_object_mut()
        .ok_or("expected workflow object")?
        .remove("mac");
    Ok(value)
}
