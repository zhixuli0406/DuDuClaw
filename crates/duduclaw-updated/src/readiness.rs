//! Private updater health protocol; does not extend the shared update wire.
use duduclaw_isolation::{ids::{RequestId, Sha256Digest}, update::{Product, ReleaseVersion, ProtocolCompatibility}};
use serde::{Deserialize, Serialize};
use crate::{Error, Result};

pub const READINESS_SCHEMA: u16 = 1;
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeRequest { pub readiness_schema: u16, pub nonce: RequestId }
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeResponse {
    pub readiness_schema: u16,
    pub nonce: RequestId,
    pub evidence: Option<GenerationEvidence>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationEvidence {
    pub generation: Sha256Digest,
    pub product: Product,
    pub version: ReleaseVersion,
    pub protocols: ProtocolCompatibility,
}
impl ProbeResponse {
    pub fn checked(self, nonce: RequestId) -> Result<GenerationEvidence> {
        if self.readiness_schema != READINESS_SCHEMA || self.nonce != nonce || nonce.0 == [0; 16] {
            return Err(Error::Invalid);
        }
        let evidence = self.evidence.ok_or(Error::Maintenance)?;
        if evidence.generation.0 == [0; 32] || evidence.protocols.updated != 1 {
            return Err(Error::Incompatible);
        }
        Ok(evidence)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn readiness_requires_fresh_nonce_and_complete_evidence() {
        for (schema, nonce) in [(0, RequestId([1; 16])), (1, RequestId([2; 16])), (1, RequestId([1; 16]))] {
            assert!(ProbeResponse { readiness_schema: schema, nonce, evidence: None }.checked(RequestId([1; 16])).is_err());
        }
        assert!(serde_json::from_str::<ProbeRequest>(r#"{"readiness_schema":1,"nonce":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"path":"/tmp"}"#).is_err());
    }
}
