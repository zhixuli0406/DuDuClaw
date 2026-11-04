//! Bounded, fd-only parse-worker contracts (design §6.2, §6.3).
//!
//! Package E runs parsers under DynamicUser with PrivateNetwork, no persistent
//! data and explicit resource/time limits. Every result remains untrusted data;
//! the gateway applies authorization and normal write APIs. A successful decode
//! proves neither file safety nor isolation. Inputs/outputs name ancillary slots,
//! never host paths. Reject extra fds and close all fds on any receive failure.

use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Serialize};

use crate::bounded::{List, Text};
use crate::ids::{RequestId, Sha256Digest};
use crate::ContractError;

/// Phase-0 JSON payload ceiling; transport must check before allocation (§6.2).
pub const MAX_WORKER_FRAME_BYTES: usize = 256 * 1024;
/// Phase-0 ceiling on aggregate text bytes in one result (§6.2).
pub const MAX_RESULT_TEXT_BYTES: usize = 128 * 1024;
/// Phase-0 per-input ceiling; large input stays on an fd, not in JSON (§6.2).
pub const MAX_INPUT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Phase-0 aggregate input ceiling (§6.2).
pub const MAX_TOTAL_INPUT_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Phase-0 output-file ceiling; the worker also needs OS-enforced limits (§6.2).
pub const MAX_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;
/// Wire schema version for request and response (§6.2).
pub const WORKER_PROTOCOL_VERSION: u16 = 1;

/// Parser operation selected by trusted gateway routing (design §6.2).
/// No executable, parser plugin, URL or command-line override is accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseOperation {
    ExtractDocument,
    InspectImage,
    TranscribeAudio,
    SynthesizeSpeech,
    InspectSkillArchive,
    InspectPackArchive,
    LoadModel,
    LocalInference,
}

/// Supported content hint; parsers must verify actual bytes (design §6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentFormat {
    Text,
    Pdf,
    OfficeDocument,
    Spreadsheet,
    Image,
    Audio,
    Archive,
    Model,
}

/// Declared read-only input fd, indexed in SCM_RIGHTS order (design §6.2/§6.3).
/// The receiver verifies regular-file type/seals, access mode and actual size;
/// it must not trust this declared size as an OS-enforced limit.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputFd {
    pub slot: u16,
    pub format: ContentFormat,
    pub size_bytes: u64,
}

/// Gateway-created temporary output fd and its byte budget (design §6.2).
/// These follow all input fds in SCM_RIGHTS order. The worker cannot choose a
/// persistent destination. Package E verifies the returned bytes and actual
/// size before the gateway publishes anything.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputFd {
    pub slot: u16,
    pub max_bytes: NonZeroU64,
}

/// Finite worker budgets; OS enforcement belongs to package E (design §6.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerLimits {
    pub memory_bytes: NonZeroU64,
    pub wall_time_ms: NonZeroU32,
    pub max_result_text_bytes: NonZeroU32,
}

/// Versioned worker request with no caller-controlled path or endpoint (§6.2).
/// Input slots are 0..N; output slots are N..N+M. No networking fd is accepted
/// here. If a root policy requires a fixed service channel, the launcher uses
/// a separate `WorkerServiceBinding` bootstrap message, never document data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParseRequest {
    pub version: u16,
    pub request_id: RequestId,
    pub operation: ParseOperation,
    pub inputs: List<InputFd, 16>,
    pub outputs: List<OutputFd, 8>,
    pub limits: WorkerLimits,
}

impl ParseRequest {
    pub fn expected_fd_count(&self) -> usize {
        self.inputs.as_slice().len() + self.outputs.as_slice().len()
    }

    pub fn validate(&self) -> Result<(), ContractError> {
        if self.version != WORKER_PROTOCOL_VERSION {
            return Err(ContractError::UnsupportedVersion);
        }
        if self.inputs.as_slice().is_empty() {
            return Err(ContractError::InvalidValue("worker input required"));
        }
        if self.limits.max_result_text_bytes.get() as usize > MAX_RESULT_TEXT_BYTES {
            return Err(ContractError::LimitExceeded("worker result text budget"));
        }
        let mut total = 0u64;
        for (index, input) in self.inputs.as_slice().iter().enumerate() {
            if input.slot as usize != index {
                return Err(ContractError::InvalidValue("worker input fd order"));
            }
            if input.size_bytes > MAX_INPUT_BYTES {
                return Err(ContractError::LimitExceeded("worker input"));
            }
            total = total.checked_add(input.size_bytes)
                .ok_or(ContractError::LimitExceeded("worker inputs"))?;
        }
        if total > MAX_TOTAL_INPUT_BYTES {
            return Err(ContractError::LimitExceeded("worker inputs"));
        }
        for (index, output) in self.outputs.as_slice().iter().enumerate() {
            if output.slot as usize != self.inputs.as_slice().len() + index {
                return Err(ContractError::InvalidValue("worker output fd order"));
            }
            if output.max_bytes.get() > MAX_OUTPUT_BYTES {
                return Err(ContractError::LimitExceeded("worker output"));
            }
        }
        Ok(())
    }

    pub fn decode_payload(payload: &[u8]) -> Result<Self, ContractError> {
        check_frame_size(payload)?;
        let request: Self = serde_json::from_slice(payload).map_err(|_| ContractError::InvalidFrame)?;
        request.validate()?;
        Ok(request)
    }
}

/// Root-created service channel for a worker (design §6.2, R6-M4).
/// This is sent only on the trusted launcher bootstrap path with exactly one
/// preconnected fd. The remote variant binds a root-published fixed destination
/// policy; the local variant is a socketpair to the inference forwarder in the
/// worker's private namespace. Neither variant permits worker DNS or connect.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkerServiceBinding {
    FixedRemoteService { policy_digest: Sha256Digest },
    LocalInference { request_id: RequestId },
}

/// Allowed bounded result item; all strings are content, never authority (§6.2).
/// Archive entries are summaries only: package E must not return or execute an
/// installation command. Model metadata does not authorize loading a plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParsedItem {
    Text { input_slot: u16, page: Option<u32>, text: Text<8192> },
    Transcript { input_slot: u16, start_ms: u64, end_ms: u64, text: Text<8192> },
    Image { input_slot: u16, width: u32, height: u32 },
    ArchiveSummary { input_slot: u16, entries: u32, expanded_bytes: u64 },
    ModelSummary { input_slot: u16, tensor_count: u32, total_bytes: u64 },
    OutputArtifact { output_slot: u16, format: ContentFormat, size_bytes: u64, sha256: Sha256Digest },
}

/// Safe worker failure code; parser diagnostics remain outside the RPC (§4.4/§6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParseError {
    UnsupportedFormat,
    InvalidContent,
    LimitExceeded,
    Timeout,
    Cancelled,
    Unavailable,
}

/// Complete bounded results or a sanitized refusal (design §6.2).
/// Partial parse output is never silently treated as a successful full result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParseResult {
    Complete { items: List<ParsedItem, 128> },
    Failed { code: ParseError },
}

/// Versioned response correlated with the original request (design §6.2).
/// Responses transfer zero fds; artifacts are written only to previously
/// supplied output fds. Package E must enforce file budgets while writing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParseResponse {
    pub version: u16,
    pub request_id: RequestId,
    pub result: ParseResult,
}

impl ParseResponse {
    pub fn validate_for(&self, request: &ParseRequest) -> Result<(), ContractError> {
        request.validate()?;
        if self.version != WORKER_PROTOCOL_VERSION {
            return Err(ContractError::UnsupportedVersion);
        }
        if self.request_id != request.request_id {
            return Err(ContractError::InvalidValue("worker request binding"));
        }
        let ParseResult::Complete { items } = &self.result else { return Ok(()); };
        let mut text_bytes = 0usize;
        let mut written_outputs = Vec::new();
        for item in items.as_slice() {
            let input_slot = match item {
                ParsedItem::Text { input_slot, text, .. } => {
                    if !matches!(request.operation, ParseOperation::ExtractDocument | ParseOperation::LocalInference) {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    text_bytes += text.as_str().len();
                    Some(*input_slot)
                }
                ParsedItem::Transcript { input_slot, start_ms, end_ms, text } => {
                    if request.operation != ParseOperation::TranscribeAudio || end_ms < start_ms {
                        return Err(ContractError::InvalidValue("worker transcript"));
                    }
                    text_bytes += text.as_str().len();
                    Some(*input_slot)
                }
                ParsedItem::Image { input_slot, width, height } => {
                    if request.operation != ParseOperation::InspectImage || *width == 0 || *height == 0 {
                        return Err(ContractError::InvalidValue("worker image"));
                    }
                    Some(*input_slot)
                }
                ParsedItem::ArchiveSummary { input_slot, .. } => {
                    if !matches!(request.operation, ParseOperation::InspectSkillArchive | ParseOperation::InspectPackArchive) {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    Some(*input_slot)
                }
                ParsedItem::ModelSummary { input_slot, .. } => {
                    if request.operation != ParseOperation::LoadModel {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    Some(*input_slot)
                }
                ParsedItem::OutputArtifact { output_slot, size_bytes, .. } => {
                    let output = request.outputs.as_slice().iter().find(|output| output.slot == *output_slot)
                        .ok_or(ContractError::InvalidValue("worker output slot"))?;
                    if *size_bytes > output.max_bytes.get() {
                        return Err(ContractError::LimitExceeded("worker artifact"));
                    }
                    if written_outputs.iter().any(|slot| slot == output_slot) {
                        return Err(ContractError::InvalidValue("duplicate worker artifact"));
                    }
                    written_outputs.push(*output_slot);
                    None
                }
            };
            if let Some(slot) = input_slot {
                if slot as usize >= request.inputs.as_slice().len() {
                    return Err(ContractError::InvalidValue("worker input slot"));
                }
            }
        }
        if text_bytes > request.limits.max_result_text_bytes.get() as usize {
            return Err(ContractError::LimitExceeded("worker result text"));
        }
        Ok(())
    }

    pub fn decode_payload(payload: &[u8], request: &ParseRequest) -> Result<Self, ContractError> {
        check_frame_size(payload)?;
        let response: Self = serde_json::from_slice(payload).map_err(|_| ContractError::InvalidFrame)?;
        response.validate_for(request)?;
        Ok(response)
    }
}

fn check_frame_size(payload: &[u8]) -> Result<(), ContractError> {
    if payload.len() > MAX_WORKER_FRAME_BYTES {
        return Err(ContractError::LimitExceeded("worker frame"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> ParseRequest {
        ParseRequest {
            version: WORKER_PROTOCOL_VERSION,
            request_id: RequestId([1; 16]),
            operation: ParseOperation::ExtractDocument,
            inputs: List::new(vec![InputFd { slot: 0, format: ContentFormat::Pdf, size_bytes: 20 }]).unwrap(),
            outputs: List::new(vec![OutputFd { slot: 1, max_bytes: NonZeroU64::new(1024).unwrap() }]).unwrap(),
            limits: WorkerLimits {
                memory_bytes: NonZeroU64::new(128 * 1024 * 1024).unwrap(),
                wall_time_ms: NonZeroU32::new(30000).unwrap(),
                max_result_text_bytes: NonZeroU32::new(8192).unwrap(),
            },
        }
    }

    fn response() -> ParseResponse {
        ParseResponse {
            version: WORKER_PROTOCOL_VERSION,
            request_id: RequestId([1; 16]),
            result: ParseResult::Complete { items: List::new(vec![ParsedItem::Text {
                input_slot: 0, page: Some(1), text: Text::new("bounded text".to_owned()).unwrap(),
            }]).unwrap() },
        }
    }

    #[test]
    fn worker_round_trip_and_fd_plan() {
        let request = request();
        assert_eq!(request.expected_fd_count(), 2);
        assert_eq!(ParseRequest::decode_payload(&serde_json::to_vec(&request).unwrap()).unwrap(), request);
        let response = response();
        assert_eq!(ParseResponse::decode_payload(&serde_json::to_vec(&response).unwrap(), &request).unwrap(), response);
    }

    #[test]
    fn worker_rejects_paths_and_unbounded_payloads() {
        let mut json = serde_json::to_value(request()).unwrap();
        json["inputs"][0]["path"] = "/etc/shadow".into();
        assert!(ParseRequest::decode_payload(&serde_json::to_vec(&json).unwrap()).is_err());
        assert!(ParseRequest::decode_payload(&vec![b' '; MAX_WORKER_FRAME_BYTES + 1]).is_err());
        assert!(ParseResponse::decode_payload(&vec![b' '; MAX_WORKER_FRAME_BYTES + 1], &request()).is_err());
        assert!(Text::<8192>::new("a".repeat(8193)).is_err());
    }

    #[test]
    fn worker_rejects_fd_alias_and_oversized_inputs() {
        let mut request = request();
        request.inputs = List::new(vec![InputFd { slot: 1, format: ContentFormat::Pdf, size_bytes: 20 }]).unwrap();
        assert!(request.validate().is_err());
        request.inputs = List::new(vec![InputFd { slot: 0, format: ContentFormat::Pdf, size_bytes: MAX_INPUT_BYTES + 1 }]).unwrap();
        assert!(request.validate().is_err());
        request.inputs = List::new(vec![InputFd { slot: 0, format: ContentFormat::Pdf, size_bytes: 20 }]).unwrap();
        request.outputs = List::new(vec![OutputFd { slot: 0, max_bytes: NonZeroU64::new(1024).unwrap() }]).unwrap();
        assert!(request.validate().is_err());
    }

    #[test]
    fn worker_rejects_wrong_binding_kind_and_output_budget() {
        let request = request();
        let mut response = response();
        response.request_id = RequestId([2; 16]);
        assert!(response.validate_for(&request).is_err());
        response.request_id = request.request_id;
        response.result = ParseResult::Complete { items: List::new(vec![ParsedItem::Image { input_slot: 0, width: 1, height: 1 }]).unwrap() };
        assert!(response.validate_for(&request).is_err());
        response.result = ParseResult::Complete { items: List::new(vec![ParsedItem::OutputArtifact {
            output_slot: 1, format: ContentFormat::Text, size_bytes: 1025, sha256: Sha256Digest([0; 32]),
        }]).unwrap() };
        assert!(response.validate_for(&request).is_err());
    }
}
