//! Parser implementation loaded only by the disposable worker binary.
//! OS memory/time limits are mandatory: library limits do not contain allocator
//! failures or bugs in third-party decoders. No parser opens a caller pathname.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};

use duduclaw_isolation::bounded::{List, Text};
use duduclaw_isolation::worker::{ContentFormat, ParseError, ParseOperation, ParseRequest,
    ParseResponse, ParseResult, ParsedItem, WORKER_PROTOCOL_VERSION};

pub(crate) mod archive;
mod document;
mod audio;
mod recording;
mod recording_artifacts;
mod evolution;
mod skill_text;
mod skill_knowledge;
// Compile the existing deterministic scanner only in the disposable E engine.
// Keep one ruleset for the legacy host path and protected worker path.
#[path = "../../duduclaw-gateway/src/skill_lifecycle/security_scanner.rs"]
mod security_scanner;
#[path = "../../duduclaw-gateway/src/skill_lifecycle/sensitive_patterns.rs"]
mod sensitive_patterns;

pub const MAX_PARSE_INPUT_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_IMAGE_DIMENSION: u32 = 16_384;

/// Decode already-authorized descriptors. The transport verifies access flags,
/// seals and descriptor count before calling this function.
pub fn parse(request: &ParseRequest, inputs: &mut [File]) -> ParseResponse {
    let result = catch_unwind(AssertUnwindSafe(|| parse_inner(request, inputs)))
        .unwrap_or(Err(ParseError::InvalidContent));
    ParseResponse {
        version: WORKER_PROTOCOL_VERSION,
        request_id: request.request_id,
        result: match result {
            Ok(items) => ParseResult::Complete { items },
            Err(code) => ParseResult::Failed { code },
        },
    }
}

fn parse_inner(request: &ParseRequest, inputs: &mut [File])
    -> Result<List<ParsedItem, 128>, ParseError>
{
    request.validate().map_err(|_| ParseError::InvalidContent)?;
    if inputs.len() != request.inputs.as_slice().len() {
        return Err(ParseError::InvalidContent);
    }
    if !request.outputs.as_slice().is_empty() {
        return Err(ParseError::UnsupportedFormat);
    }
    let mut output = Output::new(request.limits.max_result_text_bytes.get() as usize);
    let mut aggregate = 0u64;
    for (declared, input) in request.inputs.as_slice().iter().zip(inputs) {
        let meta = input.metadata().map_err(|_| ParseError::InvalidContent)?;
        if !meta.is_file() || meta.len() != declared.size_bytes {
            return Err(ParseError::InvalidContent);
        }
        aggregate = aggregate.checked_add(meta.len()).ok_or(ParseError::LimitExceeded)?;
        if aggregate > MAX_PARSE_INPUT_BYTES { return Err(ParseError::LimitExceeded); }
        input.seek(SeekFrom::Start(0)).map_err(|_| ParseError::InvalidContent)?;
        let mut bytes = Vec::new();
        input.take(MAX_PARSE_INPUT_BYTES + 1).read_to_end(&mut bytes)
            .map_err(|_| ParseError::InvalidContent)?;
        if bytes.len() as u64 != declared.size_bytes { return Err(ParseError::InvalidContent); }
        let slot = declared.slot;
        if request.operation == ParseOperation::ReadSkillIdentity {
            if declared.format != ContentFormat::Text { return Err(ParseError::InvalidContent); }
            let name = if let Some(duduclaw_isolation::worker::ParseOptions::SkillIdentity { name }) = &request.options {
                skill_text::inspect_identity(&bytes, name.as_str())?
            } else { skill_text::identity(&bytes)? };
            output.text(slot, None, &name)?;
            continue;
        }
        if let Some(duduclaw_isolation::worker::ParseOptions::SynthesizedSkill { agent, at }) = &request.options {
            if declared.format != ContentFormat::Text { return Err(ParseError::InvalidContent); }
            let content = skill_text::synthesize(&bytes, agent.as_str(), at.as_str())?;
            output.item(ParsedItem::PreparedSkill { input_slot: slot,
                content: Text::new(content).map_err(|_| ParseError::LimitExceeded)? })?;
            continue;
        }
        if request.operation == ParseOperation::ReadEvolutionEvents {
            if declared.format != ContentFormat::Text { return Err(ParseError::InvalidContent); }
            output.text(slot, None, &evolution::normalize(&bytes)?)?;
            continue;
        }

        // SFR: a protected recording's framed artifacts (trace archive + JSON).
        if let Some(duduclaw_isolation::worker::ParseOptions::RecordingArtifacts { name }) = &request.options {
            if declared.format != ContentFormat::Archive { return Err(ParseError::InvalidContent); }
            output.item(ParsedItem::Recording { input_slot: slot,
                summary: recording_artifacts::summarize(&bytes, name.as_str())? })?;
            continue;
        }
        if let Some(duduclaw_isolation::worker::ParseOptions::Recording { desktop, name }) = &request.options {
            if declared.format != ContentFormat::Text { return Err(ParseError::InvalidContent); }
            output.item(ParsedItem::Recording { input_slot: slot, summary: recording::summarize(&bytes, *desktop, name.as_str())? })?;
            continue;
        }
        if let Some(duduclaw_isolation::worker::ParseOptions::SkillKnowledge { name, agent, at }) = &request.options {
            if declared.format != ContentFormat::Text || bytes.len() > 64 * 1024 { return Err(ParseError::LimitExceeded); }
            output.item(skill_knowledge::extract(&bytes, slot, name.as_str(), agent.as_str(), at.as_str())?)?;
            continue;
        }
        if let Some(duduclaw_isolation::worker::ParseOptions::SkillManifest { name }) = &request.options {
            if declared.format != ContentFormat::Text || bytes.len() > 64 * 1024 { return Err(ParseError::LimitExceeded); }
            let content = std::str::from_utf8(&bytes).map_err(|_| ParseError::InvalidContent)?;
            skill_text::validate_manifest(content, name.as_str())?;
            let scan = security_scanner::scan_skill(content, None);
            let findings = scan.findings.iter().map(|f| Text::new(f.description.clone()).map_err(|_| ParseError::LimitExceeded))
                .collect::<Result<Vec<_>, _>>()?;
            output.item(ParsedItem::SkillScan { input_slot: slot, passed: scan.passed,
                risk: Text::new(format!("{:?}", scan.risk_level)).map_err(|_| ParseError::LimitExceeded)?,
                findings: List::new(findings).map_err(|_| ParseError::LimitExceeded)? })?;
            continue;
        }
        if let Some(duduclaw_isolation::worker::ParseOptions::SkillScan { must_not }
            | duduclaw_isolation::worker::ParseOptions::SkillPrepare { must_not, .. }) = &request.options {
            if declared.format != ContentFormat::Text || bytes.len() > 64 * 1024 { return Err(ParseError::LimitExceeded); }
            let content = std::str::from_utf8(&bytes).map_err(|_| ParseError::InvalidContent)?;
            let patterns: Vec<String> = must_not.as_slice().iter().map(|p| p.as_str().to_owned()).collect();
            let scan = security_scanner::scan_skill(content, Some(&patterns));
            let findings = scan.findings.iter().map(|f| Text::new(format!("[{:?}] {:?} (line {}): {} [pattern: {}]",
                f.severity, f.category, f.line_number.map(|n| n.to_string()).unwrap_or_else(|| "?".into()),
                f.description, f.matched_pattern)).map_err(|_| ParseError::LimitExceeded))
                .collect::<Result<Vec<_>, _>>()?;
            let risk = Text::new(format!("{:?}", scan.risk_level)).map_err(|_| ParseError::LimitExceeded)?;
            output.item(ParsedItem::SkillScan { input_slot: slot, passed: scan.passed, risk,
                findings: List::new(findings).map_err(|_| ParseError::LimitExceeded)? })?;
            if scan.passed {
                if let Some(duduclaw_isolation::worker::ParseOptions::SkillPrepare { action, agent, at, .. }) = &request.options {
                    let content = skill_text::prepare(content, *action, agent.as_str(), at.as_str())?;
                    output.item(ParsedItem::PreparedSkill { input_slot: slot,
                        content: Text::new(content).map_err(|_| ParseError::LimitExceeded)? })?;
                }
            }
            continue;
        }
        if request.options.is_some() {
            output.items.push(crate::adapter_engine::parse(request, &bytes)?);
            continue;
        }
        match (request.operation, declared.format) {
            (ParseOperation::ExtractDocument, format) => {
                document::extract(format, &bytes, slot, &mut output)?;
            }
            (ParseOperation::InspectAudio, ContentFormat::Audio) => {
                output.item(audio::inspect(&bytes, slot)?)?;
            }
            (ParseOperation::InspectImage, ContentFormat::Image) => {
                let (width, height) = image_dimensions(&bytes)?;
                output.item(ParsedItem::Image { input_slot: slot, width, height })?;
            }
            (ParseOperation::InspectSkillArchive | ParseOperation::InspectPackArchive,
                ContentFormat::Archive) => {
                let summary = archive::inspect(&bytes, true)?;
                output.item(ParsedItem::ArchiveSummary { input_slot: slot,
                    entries: summary.entries, expanded_bytes: summary.expanded_bytes })?;
            }
            _ => return Err(ParseError::UnsupportedFormat),
        }
    }
    List::new(output.items).map_err(|_| ParseError::LimitExceeded)
}

pub(super) struct Output {
    items: Vec<ParsedItem>,
    remaining: usize,
}

impl Output {
    fn new(remaining: usize) -> Self { Self { items: Vec::new(), remaining } }

    fn item(&mut self, item: ParsedItem) -> Result<(), ParseError> {
        if self.items.len() == 128 { return Err(ParseError::LimitExceeded); }
        self.items.push(item);
        Ok(())
    }

    pub(super) fn text(&mut self, slot: u16, page: Option<u32>, text: &str)
        -> Result<(), ParseError>
    {
        if text.len() > self.remaining { return Err(ParseError::LimitExceeded); }
        self.remaining -= text.len();
        let mut chunk = String::new();
        for ch in text.chars() {
            if chunk.len() + ch.len_utf8() > 8192 {
                self.item(ParsedItem::Text { input_slot: slot, page,
                    text: Text::new(std::mem::take(&mut chunk))
                        .map_err(|_| ParseError::LimitExceeded)? })?;
            }
            chunk.push(ch);
        }
        if !chunk.is_empty() {
            self.item(ParsedItem::Text { input_slot: slot, page,
                text: Text::new(chunk).map_err(|_| ParseError::LimitExceeded)? })?;
        }
        Ok(())
    }
}

fn image_error(error: image::ImageError) -> ParseError {
    match error {
        image::ImageError::Limits(_) => ParseError::LimitExceeded,
        image::ImageError::Unsupported(_) => ParseError::UnsupportedFormat,
        _ => ParseError::InvalidContent,
    }
}

fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32), ParseError> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format().map_err(|_| ParseError::InvalidContent)?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_BYTES);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(image_error)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 { return Err(ParseError::InvalidContent); }
    if width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION
        || decoder.total_bytes() > MAX_IMAGE_BYTES {
        return Err(ParseError::LimitExceeded);
    }
    // Decode only after the dimensions and exact output allocation were bounded.
    // The RPC returns metadata, never these disposable pixels.
    let length = usize::try_from(decoder.total_bytes()).map_err(|_| ParseError::LimitExceeded)?;
    let mut pixels = vec![0u8; length];
    decoder.read_image(&mut pixels).map_err(image_error)?;
    Ok((width, height))
}

#[cfg(test)]
mod tests;
