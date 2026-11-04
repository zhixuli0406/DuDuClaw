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
