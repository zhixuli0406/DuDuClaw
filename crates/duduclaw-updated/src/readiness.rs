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

/// The one update-compatibility rule, used by the gateway's update admission
/// and by the installer's composite readiness check: a non-zero generation,
/// exactly `installed_version` (this release's `CARGO_PKG_VERSION`), and
/// protocol 1 for all eight components. An unparsable `installed_version` is
/// incompatible. Pure.
pub fn compatible_update(evidence: &GenerationEvidence, installed_version: &str) -> bool {
    let p = &evidence.protocols;
    evidence.generation.0 != [0; 32]
        && crate::parse_version(installed_version).is_ok_and(|version| version == evidence.version)
        && [p.gateway, p.spawnd, p.trampoline, p.egress, p.updated, p.mcp_bridge, p.parse_worker, p.importer]
            .into_iter().all(|version| version == 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> GenerationEvidence {
        let one = ProtocolCompatibility { gateway: 1, spawnd: 1, trampoline: 1, egress: 1, updated: 1,
            mcp_bridge: 1, parse_worker: 1, importer: 1 };
        GenerationEvidence { generation: Sha256Digest([1; 32]), product: Product::Duduclaw,
            version: crate::parse_version("1.83.0").unwrap(), protocols: one }
    }

    #[test]
    fn compatible_update_requires_generation_exact_version_and_protocol_one() {
        assert!(compatible_update(&evidence(), "1.83.0"));
        assert!(compatible_update(&evidence(), "v1.83.0"));
        for version in ["1.83.1", "1.82.0", "", "1.83", "1.83.0-rc1"] {
            assert!(!compatible_update(&evidence(), version), "{version}");
        }
        let mut zero = evidence(); zero.generation = Sha256Digest([0; 32]);
        assert!(!compatible_update(&zero, "1.83.0"));
        let flips: [fn(&mut ProtocolCompatibility); 8] = [|p| p.gateway = 2, |p| p.spawnd = 0, |p| p.trampoline = 2,
            |p| p.egress = 2, |p| p.updated = 2, |p| p.mcp_bridge = 2, |p| p.parse_worker = 2, |p| p.importer = 2];
        for flip in flips {
            let mut other = evidence();
            flip(&mut other.protocols);
            assert!(!compatible_update(&other, "1.83.0"));
        }
    }
    #[test]
    fn readiness_requires_fresh_nonce_and_complete_evidence() {
        for (schema, nonce) in [(0, RequestId([1; 16])), (1, RequestId([2; 16])), (1, RequestId([1; 16]))] {
            assert!(ProbeResponse { readiness_schema: schema, nonce, evidence: None }.checked(RequestId([1; 16])).is_err());
        }
        assert!(serde_json::from_str::<ProbeRequest>(r#"{"readiness_schema":1,"nonce":[1,1,1,1,1,1,1,1,1,1,1,1,1,1,1,1],"path":"/tmp"}"#).is_err());
    }
}
