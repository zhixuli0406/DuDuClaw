//! Synchronous fixed-endpoint client; async gateway callers use spawn_blocking.
use duduclaw_isolation::update::{UpdateRequest, UpdateRequestV1, UpdateResponse, UpdateResponseV1};
use crate::{Error, Result, linux::transport};
pub fn request(request: UpdateRequest) -> Result<UpdateResponse> {
    let UpdateRequest::V1(inner) = &request;
    let expected = match inner { UpdateRequestV1::Apply { request_id, .. } | UpdateRequestV1::Status { request_id } => *request_id };
    let fd = transport::connect()?;
    transport::send(&fd, &request)?;
    let response: UpdateResponse = transport::receive(&fd)?;
    let UpdateResponse::V1(inner) = &response;
    let actual = match inner {
        UpdateResponseV1::Accepted { request_id } | UpdateResponseV1::Status { request_id, .. }
        | UpdateResponseV1::Error { request_id, .. } => *request_id,
    };
    if actual != expected { return Err(Error::Invalid); }
    Ok(response)
}

/// Authenticated live probe. Callers compare all returned protocols and the
/// generation against the other components before admitting protected TEAM runs.
pub fn probe(nonce: duduclaw_isolation::ids::RequestId) -> Result<crate::readiness::GenerationEvidence> {
    if nonce.0 == [0; 16] { return Err(Error::Invalid); }
    let fd = transport::connect()?;
    transport::send(&fd, &crate::readiness::ProbeRequest { readiness_schema: crate::readiness::READINESS_SCHEMA, nonce })?;
    let response: crate::readiness::ProbeResponse = transport::receive(&fd)?;
    response.checked(nonce)
}
