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

#[path = "worker_media.rs"]
mod media;
pub use media::{HtmlQuery, HtmlOutputFormat};

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
    ExtractKnowledge,
    ReadTable,
    InspectDocument,
    TransformImage,
    ExtractHtml,
    InspectImage,
    InspectAudio,
    ScanSkill,
    PrepareSkill,
    ExtractSkill,
    SummarizeRecording,
    InspectSkillManifest,
    ReadEvolutionEvents,
    ReadSkillIdentity,
    PrepareSynthesizedSkill,
    TranscribeAudio,
    SynthesizeSpeech,
    InspectSkillArchive,
    InspectPackArchive,
    ExtractPackArchive,
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

/// Exact parser selection, separate from the coarse input content hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KnowledgeFormat { Pdf, Docx, Pptx, Xlsx, Xls, Csv, Txt, Md, Html }

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SegmentKind { Page, Slide, Sheet, Section, Text }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeSegment {
    pub kind: SegmentKind,
    pub label: Option<Text<256>>,
    pub page: Option<u32>,
    pub sheet: Option<Text<256>>,
    pub line_rows: Option<List<u32, 8192>>,
    pub text: Text<8192>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeWarning {
    pub code: Text<128>,
    pub pages: List<u32, 1000>,
    pub count: Option<u32>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KnowledgeOutput {
    pub segments: List<KnowledgeSegment, 128>,
    pub warnings: List<KnowledgeWarning, 128>,
    pub extractor_version: Text<128>,
}
impl KnowledgeOutput {
    fn text_bytes(&self) -> usize {
        self.extractor_version.as_str().len()
            + self.warnings.as_slice().iter().map(|w| w.code.as_str().len()).sum::<usize>()
            + self.segments.as_slice().iter().map(|s| s.text.as_str().len()
                + s.label.as_ref().map_or(0, |v| v.as_str().len())
                + s.sheet.as_ref().map_or(0, |v| v.as_str().len())).sum::<usize>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TableFormat { Csv, Spreadsheet }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableOptions {
    pub format: TableFormat,
    pub delimiter: u8,
    pub has_header: bool,
    pub offset: u32,
    pub limit: u16,
    pub sheet: Option<Text<256>>,
}
impl TableOptions {
    fn validate(&self) -> Result<(), ContractError> {
        if self.limit == 0 || self.limit > 2000 || self.delimiter == 0
            || matches!(self.delimiter, b'\r' | b'\n' | b'"') || self.delimiter > 127 {
            return Err(ContractError::InvalidValue("worker table options"));
        }
        if self.format == TableFormat::Spreadsheet && !self.has_header {
            return Err(ContractError::InvalidValue("worksheet requires a header row"));
        }
        if self.format == TableFormat::Csv && self.sheet.is_some() {
            return Err(ContractError::InvalidValue("CSV sheet selection"));
        }
        Ok(())
    }
}
/// Numbers use JSON's finite number type. Strings retain the existing 8 KiB cap.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case", deny_unknown_fields)]
pub enum TableCell { Null, Bool(bool), Number(serde_json::Number), String(Text<8192>) }
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TableOutput {
    pub columns: List<Text<256>, 256>,
    pub rows: List<List<TableCell, 256>, 2000>,
    pub truncated: bool,
    pub sheet: Option<Text<256>>,
    pub sheets: List<Text<256>, 128>,
}
impl TableOutput {
    fn text_bytes(&self) -> usize {
        self.columns.as_slice().iter().map(|c| c.as_str().len()).sum::<usize>()
            + self.sheets.as_slice().iter().map(|c| c.as_str().len()).sum::<usize>()
            + self.sheet.as_ref().map_or(0, |c| c.as_str().len())
            + self.rows.as_slice().iter().flat_map(|r| r.as_slice()).map(|c| match c {
                TableCell::String(s) => s.as_str().len(),
                TableCell::Number(n) => n.to_string().len(),
                TableCell::Bool(_) => 5,
                TableCell::Null => 4,
            }).sum::<usize>()
    }
    fn validate(&self, options: &TableOptions) -> Result<(), ContractError> {
        let columns = self.columns.as_slice();
        let mut unique = std::collections::BTreeSet::new();
        if columns.iter().any(|c| c.as_str().is_empty() || !unique.insert(c.as_str()))
            || self.rows.as_slice().len() > usize::from(options.limit)
 {
            return Err(ContractError::InvalidValue("worker table shape"));
        }
        match options.format {
            TableFormat::Csv if self.sheet.is_some() || !self.sheets.as_slice().is_empty() =>
                return Err(ContractError::InvalidValue("CSV sheet result")),
            TableFormat::Spreadsheet if self.sheet.as_ref().is_none_or(|sheet|
                !self.sheets.as_slice().contains(sheet)
                    || options.sheet.as_ref().is_some_and(|requested| requested != sheet)) =>
                return Err(ContractError::InvalidValue("worker sheet binding")),
            _ => {},
        }
        Ok(())
    }
}

/// Caller policy can only narrow the worker's independent safety ceilings.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentPolicy {
    pub max_uncompressed_bytes: u64,
    pub max_entries: u32,
    pub max_compression_ratio: u32,
    pub max_xml_depth: u32,
    pub xml_scan_part_bytes: u64,
    pub xml_scan_total_bytes: u64,
}
impl DocumentPolicy {
    fn validate(&self) -> Result<(), ContractError> {
        if self.max_uncompressed_bytes == 0 || self.max_entries == 0
            || self.max_compression_ratio == 0 || self.max_xml_depth == 0
            || self.xml_scan_part_bytes == 0 || self.xml_scan_total_bytes == 0 {
            return Err(ContractError::InvalidValue("worker document policy"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "adapter", content = "options", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParseOptions {
    Knowledge { format: KnowledgeFormat, max_chars: u32 },
    Table(TableOptions),
    Document(DocumentPolicy),
    ImageResize { max_dimension: u32 },
    ImageMask { regions: List<[u32; 4], 128>, fill: [u8; 3] },
    Html { queries: List<HtmlQuery, 32> },
    SearchHtml,
    PackArchive,
    SkillKnowledge { name: Text<64>, agent: Text<64>, at: Text<64> },
    SkillManifest { name: Text<64> },
    SkillIdentity { name: Text<64> },
    SynthesizedSkill { agent: Text<64>, at: Text<64> },
    Recording { desktop: bool, name: Text<64> },
    SkillScan { must_not: List<Text<1024>, 128> },
    SkillPrepare { action: SkillAction, agent: Text<64>, at: Text<64>, must_not: List<Text<1024>, 128> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillAction { Adopt, Graduate }

/// Redacted recording evidence, never executable instructions or host paths.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordingSummary {
    pub prompt: Text<65536>,
    pub env: List<Text<128>, 128>,
    pub api_count: u16,
    pub action_count: u16,
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
    /// Additive opt-in adapter schema. Old requests omit this field entirely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ParseOptions>,
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
        match (&self.operation, &self.options) {
            (ParseOperation::ReadSkillIdentity, Some(ParseOptions::SkillIdentity { name }))
                if !name.as_str().is_empty() && name.as_str().bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') => {},
            (ParseOperation::PrepareSynthesizedSkill, Some(ParseOptions::SynthesizedSkill { agent, at }))
                if !agent.as_str().is_empty() && agent.as_str().bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    && !at.as_str().is_empty() => {},
            (ParseOperation::InspectSkillManifest, Some(ParseOptions::SkillManifest { name }))
                if !name.as_str().is_empty() && name.as_str().bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') => {},
            (ParseOperation::SummarizeRecording, Some(ParseOptions::Recording { name, .. }))
                if !name.as_str().is_empty() && name.as_str().bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_') => {},
            (ParseOperation::ExtractSkill, Some(ParseOptions::SkillKnowledge { name, agent, at }))
                if !name.as_str().is_empty() && !agent.as_str().is_empty() && !at.as_str().is_empty() => {},
            (ParseOperation::PrepareSkill, Some(ParseOptions::SkillPrepare { agent, at, .. }))
                if !agent.as_str().is_empty() && agent.as_str().bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    && !at.as_str().is_empty() => {},
            (ParseOperation::ScanSkill, Some(ParseOptions::SkillScan { .. })) => {},
            (ParseOperation::ExtractKnowledge, Some(ParseOptions::Knowledge { max_chars, .. }))
                if *max_chars > 0 => {},
            (ParseOperation::ExtractPackArchive, Some(ParseOptions::PackArchive)) => {},
            (ParseOperation::TransformImage, Some(ParseOptions::ImageResize { max_dimension }))
                if (1..=16_384).contains(max_dimension) => {},
            (ParseOperation::TransformImage, Some(ParseOptions::ImageMask { regions, .. }))
                if !regions.as_slice().is_empty() => {},
            (ParseOperation::ExtractHtml, Some(ParseOptions::SearchHtml)) => {},
            (ParseOperation::ExtractHtml, Some(ParseOptions::Html { queries })) => {
                if queries.as_slice().is_empty() || queries.as_slice().iter().any(|query|
                    query.name.as_str().is_empty() || query.selector.as_str().is_empty()
                        || query.max_matches > 128) {
                    return Err(ContractError::InvalidValue("worker HTML queries"));
                }
            },
            (ParseOperation::ReadTable, Some(ParseOptions::Table(options))) => options.validate()?,
            (ParseOperation::InspectDocument, Some(ParseOptions::Document(policy))) => policy.validate()?,
            (ParseOperation::PrepareSynthesizedSkill | ParseOperation::InspectSkillManifest | ParseOperation::SummarizeRecording | ParseOperation::ExtractSkill | ParseOperation::PrepareSkill | ParseOperation::ScanSkill | ParseOperation::ExtractKnowledge | ParseOperation::ReadTable | ParseOperation::InspectDocument
                | ParseOperation::TransformImage | ParseOperation::ExtractHtml | ParseOperation::ExtractPackArchive, _) =>
                return Err(ContractError::InvalidValue("worker adapter options")),
            (_, None) => {},
            _ => return Err(ContractError::InvalidValue("worker adapter options")),
        }
        if self.options.is_some() && (self.inputs.as_slice().len() != 1
            || (!matches!(self.operation, ParseOperation::TransformImage | ParseOperation::ExtractPackArchive) && !self.outputs.as_slice().is_empty())
            || (matches!(self.operation, ParseOperation::TransformImage | ParseOperation::ExtractPackArchive) && self.outputs.as_slice().len() != 1)) {
            return Err(ContractError::InvalidValue("worker adapter fd plan"));
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

/// File records describe byte ranges in a single pre-authorized output fd.
/// Names remain untrusted: callers must apply their destination path policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveEntry {
    pub name: Text<4096>,
    pub directory: bool,
    pub offset: u64,
    pub length: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioFormat { Wav, Mp3 }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillPage {
    pub path: Text<256>,
    pub content: Text<8192>,
    pub rationale: Text<1024>,
    pub related: List<Text<256>, 64>,
}

/// Allowed bounded result item; all strings are content, never authority (§6.2).
/// Archive entries are summaries only: package E must not return or execute an
/// installation command. Model metadata does not authorize loading a plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ParsedItem {
    Recording { input_slot: u16, summary: RecordingSummary },
    Text { input_slot: u16, page: Option<u32>, text: Text<8192> },
    Knowledge { input_slot: u16, output: KnowledgeOutput },
    Html { input_slot: u16, query: Text<256>, value: Text<8192> },
    Table { input_slot: u16, output: TableOutput },
    Transcript { input_slot: u16, start_ms: u64, end_ms: u64, text: Text<8192> },
    Image { input_slot: u16, width: u32, height: u32 },
    SkillKnowledge { input_slot: u16, concepts: u16, entities: u16, pages: List<SkillPage, 64> },
    PreparedSkill { input_slot: u16, content: Text<65536> },
    SkillScan { input_slot: u16, passed: bool, risk: Text<16>, findings: List<Text<2048>, 128> },
    Audio { input_slot: u16, format: AudioFormat, sample_rate: u32, channels: u16, frames: u64 },
    ArchiveSummary { input_slot: u16, entries: u32, expanded_bytes: u64 },
    ArchiveManifest { input_slot: u16, entries: List<ArchiveEntry, 1000>, legacy_encoding: Option<Text<8>>, junk_skipped: u32 },
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
                ParsedItem::Html { input_slot, query, value } => {
                    if request.operation != ParseOperation::ExtractHtml {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    match &request.options {
                        Some(ParseOptions::Html { queries }) if queries.as_slice().iter()
                            .any(|q| q.name == *query) => {},
                        Some(ParseOptions::SearchHtml) if query.as_str() == "search" => {},
                        _ => return Err(ContractError::InvalidValue("worker HTML query binding")),
                    }
                    text_bytes += query.as_str().len() + value.as_str().len();
                    Some(*input_slot)
                }
                ParsedItem::Knowledge { input_slot, output } => {
                    if request.operation != ParseOperation::ExtractKnowledge {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    let Some(ParseOptions::Knowledge { max_chars, .. }) = &request.options else {
                        return Err(ContractError::InvalidValue("worker knowledge options"));
                    };
                    if output.segments.as_slice().iter().map(|s| s.text.as_str().chars().count())
                        .sum::<usize>() > *max_chars as usize {
                        return Err(ContractError::LimitExceeded("worker character budget"));
                    }
                    text_bytes += output.text_bytes();
                    Some(*input_slot)
                }
                ParsedItem::Table { input_slot, output } => {
                    if request.operation != ParseOperation::ReadTable {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    let Some(ParseOptions::Table(options)) = &request.options else {
                        return Err(ContractError::InvalidValue("worker table options"));
                    };
                    output.validate(options)?;
                    text_bytes += output.text_bytes();
                    Some(*input_slot)
                }
                ParsedItem::Recording { input_slot, summary } => {
                    if request.operation != ParseOperation::SummarizeRecording || summary.api_count > 40 || summary.action_count > 120 {
                        return Err(ContractError::InvalidValue("worker recording summary"));
                    }
                    text_bytes += summary.prompt.as_str().len() + summary.env.as_slice().iter().map(|e| e.as_str().len()).sum::<usize>();
                    Some(*input_slot)
                }
                ParsedItem::Text { input_slot, text, .. } => {
                    if !matches!(request.operation, ParseOperation::ExtractDocument | ParseOperation::LocalInference | ParseOperation::ReadEvolutionEvents | ParseOperation::ReadSkillIdentity) {
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
                ParsedItem::SkillKnowledge { input_slot, concepts, entities, pages } => {
                    if request.operation != ParseOperation::ExtractSkill || *concepts > 30 || *entities > 10
                        || pages.as_slice().len() != usize::from(*concepts) + usize::from(*entities) + 1 {
                        return Err(ContractError::InvalidValue("worker skill knowledge result"));
                    }
                    text_bytes += pages.as_slice().iter().map(|p| p.path.as_str().len() + p.content.as_str().len()
                        + p.rationale.as_str().len() + p.related.as_slice().iter().map(|r| r.as_str().len()).sum::<usize>()).sum::<usize>();
                    Some(*input_slot)
                }
                ParsedItem::PreparedSkill { input_slot, content } => {
                    if !matches!(request.operation, ParseOperation::PrepareSkill | ParseOperation::PrepareSynthesizedSkill) { return Err(ContractError::InvalidValue("worker prepared skill result")); }
                    text_bytes += content.as_str().len();
                    Some(*input_slot)
                }
                ParsedItem::SkillScan { input_slot, passed, risk, findings } => {
                    if !matches!(request.operation, ParseOperation::InspectSkillManifest | ParseOperation::ScanSkill | ParseOperation::PrepareSkill)
                        || !matches!(risk.as_str(), "Clean" | "Low" | "Medium" | "High" | "Critical")
                        || *passed != !matches!(risk.as_str(), "High" | "Critical") {
                        return Err(ContractError::InvalidValue("worker skill scan result"));
                    }
                    text_bytes += risk.as_str().len() + findings.as_slice().iter().map(|f| f.as_str().len()).sum::<usize>();
                    Some(*input_slot)
                }
                ParsedItem::Audio { input_slot, sample_rate, channels, frames, .. } => {
                    if request.operation != ParseOperation::InspectAudio || !(8000..=192000).contains(sample_rate)
                        || !(1..=8).contains(channels) || *frames == 0 {
                        return Err(ContractError::InvalidValue("worker audio result"));
                    }
                    Some(*input_slot)
                }
                ParsedItem::Image { input_slot, width, height } => {
                    if request.operation != ParseOperation::InspectImage || *width == 0 || *height == 0 {
                        return Err(ContractError::InvalidValue("worker image"));
                    }
                    Some(*input_slot)
                }
                ParsedItem::ArchiveSummary { input_slot, .. } => {
                    if !matches!(request.operation, ParseOperation::InspectSkillArchive | ParseOperation::InspectPackArchive | ParseOperation::InspectDocument) {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    Some(*input_slot)
                }
                ParsedItem::ArchiveManifest { input_slot, entries, legacy_encoding, .. } => {
                    if legacy_encoding.as_ref().is_some_and(|value| !matches!(value.as_str(), "gbk" | "big5")) {
                        return Err(ContractError::InvalidValue("worker archive encoding"));
                    }
                    text_bytes += legacy_encoding.as_ref().map_or(0, |value| value.as_str().len());
                    if request.operation != ParseOperation::ExtractPackArchive {
                        return Err(ContractError::InvalidValue("worker result kind"));
                    }
                    let output = request.outputs.as_slice().first()
                        .ok_or(ContractError::InvalidValue("worker archive output"))?;
                    for entry in entries.as_slice() {
                        text_bytes += entry.name.as_str().len();
                        if entry.offset.checked_add(entry.length).is_none_or(|end| end > output.max_bytes.get())
                            || (entry.directory && entry.length != 0) {
                            return Err(ContractError::InvalidValue("worker archive range"));
                        }
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
            options: None,
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

#[cfg(test)]
mod adapter_tests {
    use super::*;
    fn request(operation: ParseOperation, options: Option<ParseOptions>) -> ParseRequest {
        ParseRequest { version: WORKER_PROTOCOL_VERSION, request_id: RequestId([3; 16]), operation,
            inputs: List::new(vec![InputFd { slot: 0, format: ContentFormat::Text, size_bytes: 10 }]).unwrap(),
            outputs: List::new(vec![]).unwrap(), options,
            limits: WorkerLimits { memory_bytes: NonZeroU64::new(512 * 1024 * 1024).unwrap(),
                wall_time_ms: NonZeroU32::new(30_000).unwrap(),
                max_result_text_bytes: NonZeroU32::new(MAX_RESULT_TEXT_BYTES as u32).unwrap() } }
    }
    fn table_options() -> TableOptions {
        TableOptions { format: TableFormat::Csv, delimiter: b',', has_header: true,
            offset: 0, limit: 2, sheet: None }
    }
    fn response(request: &ParseRequest, item: ParsedItem) -> ParseResponse {
        ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Complete { items: List::new(vec![item]).unwrap() } }
    }
    #[test]
    fn adapter_options_are_opt_in_and_operation_bound() {
        let legacy = request(ParseOperation::ExtractDocument, None);
        let json = serde_json::to_value(&legacy).unwrap();
        assert!(json.get("options").is_none());
        assert!(ParseRequest::decode_payload(&serde_json::to_vec(&legacy).unwrap()).is_ok());
        let mut rich = request(ParseOperation::ReadTable, Some(ParseOptions::Table(table_options())));
        assert!(rich.validate().is_ok());
        rich.operation = ParseOperation::InspectDocument;
        assert!(rich.validate().is_err());
        rich.operation = ParseOperation::ReadTable;
        rich.options = None;
        assert!(rich.validate().is_err());
        let mut options = table_options();
        options.limit = 2001;
        rich.options = Some(ParseOptions::Table(options));
        assert!(rich.validate().is_err());
        rich.options = Some(ParseOptions::Knowledge { format: KnowledgeFormat::Csv, max_chars: 0 });
        rich.operation = ParseOperation::ExtractKnowledge;
        assert!(rich.validate().is_err());
    }
    #[test]
    fn rich_result_text_and_character_limits_include_metadata() {
        let mut request = request(ParseOperation::ExtractKnowledge,
            Some(ParseOptions::Knowledge { format: KnowledgeFormat::Txt, max_chars: 2 }));
        let output = KnowledgeOutput { extractor_version: Text::new("kb-extract/1".into()).unwrap(),
            warnings: List::new(vec![]).unwrap(), segments: List::new(vec![KnowledgeSegment {
                kind: SegmentKind::Text, label: None, page: None, sheet: None, line_rows: None,
                text: Text::new("三個字".into()).unwrap(),
            }]).unwrap() };
        let result = response(&request, ParsedItem::Knowledge { input_slot: 0, output });
        assert!(result.validate_for(&request).is_err());
        request.options = Some(ParseOptions::Knowledge { format: KnowledgeFormat::Txt, max_chars: 3 });
        assert!(result.validate_for(&request).is_ok());
        request.limits.max_result_text_bytes = NonZeroU32::new(9).unwrap();
        assert!(result.validate_for(&request).is_err());
    }
    #[test]
    fn table_results_bind_options_and_reject_duplicate_headers() {
        let request = request(ParseOperation::ReadTable, Some(ParseOptions::Table(table_options())));
        let mut output = TableOutput { columns: List::new(vec![Text::new("a".into()).unwrap()]).unwrap(),
            rows: List::new(vec![List::new(vec![TableCell::Null, TableCell::Bool(true)]).unwrap()]).unwrap(),
            truncated: false, sheet: None, sheets: List::new(vec![]).unwrap() };
        // Ragged rows are preserved for the caller's extra-column policy.
        assert!(response(&request, ParsedItem::Table { input_slot: 0, output: output.clone() })
            .validate_for(&request).is_ok());
        output.columns = List::new(vec![Text::new("a".into()).unwrap(), Text::new("a".into()).unwrap()]).unwrap();
        assert!(response(&request, ParsedItem::Table { input_slot: 0, output }).validate_for(&request).is_err());
        assert!(response(&request, ParsedItem::Text { input_slot: 0, page: None, text: Text::new("wrong".into()).unwrap() })
            .validate_for(&request).is_err());
    }
}
