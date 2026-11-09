//! Trusted adapters: operation options are policy data, never executable paths.
use std::fs::File;
use std::io::Read;
use crate::{client, worker::*};

fn invoke(reader: &mut impl Read, operation: ParseOperation, format: ContentFormat,
    options: ParseOptions, limits: Option<WorkerLimits>) -> Result<ParseResponse, ParseError>
{
    let mut request = client::request(operation, format, 0);
    request.options = Some(options);
    if let Some(limits) = limits { request.limits = limits; }
    client::invoke_reader(&request, reader)
}

fn knowledge(response: ParseResponse) -> Result<KnowledgeOutput, ParseError> {
    match response.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::Knowledge { input_slot: 0, output }] => Ok(output.clone()),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}

fn content_format(format: KnowledgeFormat) -> ContentFormat {
    match format {
        KnowledgeFormat::Pdf => ContentFormat::Pdf,
        KnowledgeFormat::Docx | KnowledgeFormat::Pptx => ContentFormat::OfficeDocument,
        KnowledgeFormat::Xlsx | KnowledgeFormat::Xls => ContentFormat::Spreadsheet,
        _ => ContentFormat::Text,
    }
}

pub fn extract_knowledge_file(file: &mut File, format: KnowledgeFormat, max_chars: u32,
    limits: WorkerLimits) -> Result<KnowledgeOutput, ParseError>
{
    knowledge(invoke(file, ParseOperation::ExtractKnowledge, content_format(format),
        ParseOptions::Knowledge { format, max_chars }, Some(limits))?)
}

pub fn extract_knowledge_bytes(bytes: &[u8], format: KnowledgeFormat, max_chars: u32,
    limits: WorkerLimits) -> Result<KnowledgeOutput, ParseError>
{
    knowledge(invoke(&mut std::io::Cursor::new(bytes), ParseOperation::ExtractKnowledge,
        content_format(format), ParseOptions::Knowledge { format, max_chars }, Some(limits))?)
}

pub fn read_table_file(file: &mut File, options: TableOptions) -> Result<TableOutput, ParseError> {
    let format = match options.format { TableFormat::Csv => ContentFormat::Text,
        TableFormat::Spreadsheet => ContentFormat::Spreadsheet };
    match invoke(file, ParseOperation::ReadTable, format, ParseOptions::Table(options), None)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::Table { input_slot: 0, output }] => Ok(output.clone()),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}

pub fn inspect_document_reader(reader: &mut impl Read, policy: DocumentPolicy) -> Result<(), ParseError> {
    match invoke(reader, ParseOperation::InspectDocument, ContentFormat::Archive,
        ParseOptions::Document(policy), None)?.result {
        ParseResult::Complete { items } if matches!(items.as_slice(),
            [ParsedItem::ArchiveSummary { input_slot: 0, .. }]) => Ok(()),
        ParseResult::Failed { code } => Err(code),
        _ => Err(ParseError::InvalidContent),
    }
}

pub fn inspect_document_bytes(bytes: &[u8], policy: DocumentPolicy) -> Result<(), ParseError> {
    inspect_document_reader(&mut std::io::Cursor::new(bytes), policy)
}
pub fn inspect_document_file(file: &mut File, policy: DocumentPolicy) -> Result<(), ParseError> {
    inspect_document_reader(file, policy)
}

/// Inspect the actual snapshot; provider selection and credentials stay host-owned.
pub fn inspect_audio_file(file: &mut File) -> Result<(AudioFormat, u64), ParseError> {
    let request = client::request(ParseOperation::InspectAudio, ContentFormat::Audio, 0);
    match client::invoke_reader(&request, file)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::Audio { input_slot: 0, format, sample_rate, frames, .. }] => {
                let millis = frames.checked_mul(1000).ok_or(ParseError::LimitExceeded)?
                    .checked_div(u64::from(*sample_rate)).ok_or(ParseError::InvalidContent)?;
                if millis == 0 { return Err(ParseError::InvalidContent); }
                Ok((*format, millis))
            }
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}

pub fn summarize_recording(bytes: &[u8], desktop: bool, name: &str) -> Result<RecordingSummary, ParseError> {
    let options = ParseOptions::Recording { desktop,
        name: duduclaw_isolation::bounded::Text::new(name.to_owned()).map_err(|_| ParseError::InvalidContent)? };
    match invoke(&mut std::io::Cursor::new(bytes), ParseOperation::SummarizeRecording,
        ContentFormat::Text, options, None)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::Recording { input_slot: 0, summary }] => Ok(summary.clone()),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}
/// SFR: summarize a protected recording from its framed, verified artifacts
/// (`recording_layout::bundle`). Every byte is parsed only in the worker:
/// the frame, the trace archive and the JSON artifacts.
pub fn summarize_recording_artifacts(bundle: &[u8], name: &str) -> Result<RecordingSummary, ParseError> {
    if bundle.len() as u64 > duduclaw_isolation::recording_layout::MAX_BUNDLE_BYTES { return Err(ParseError::LimitExceeded); }
    let options = ParseOptions::RecordingArtifacts {
        name: duduclaw_isolation::bounded::Text::new(name.to_owned()).map_err(|_| ParseError::InvalidContent)? };
    match invoke(&mut std::io::Cursor::new(bundle), ParseOperation::SummarizeRecording,
        ContentFormat::Archive, options, None)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::Recording { input_slot: 0, summary }] => Ok(summary.clone()),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}

/// Scan model output as a real bounded file before approval/publication.
pub fn scan_skill_bytes(bytes: &[u8]) -> Result<(bool, String, Vec<String>), ParseError> {
    let options = ParseOptions::SkillScan { must_not: duduclaw_isolation::bounded::List::new(vec![])
        .map_err(|_| ParseError::InvalidContent)? };
    match invoke(&mut std::io::Cursor::new(bytes), ParseOperation::ScanSkill,
        ContentFormat::Text, options, None)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::SkillScan { input_slot: 0, passed, risk, findings }] => Ok((*passed,
                risk.as_str().to_owned(), findings.as_slice().iter().map(|f| f.as_str().to_owned()).collect())),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}

pub fn inspect_skill_manifest(bytes: &[u8], name: &str) -> Result<(bool, String, usize), ParseError> {
    let options = ParseOptions::SkillManifest { name: duduclaw_isolation::bounded::Text::new(name.to_owned())
        .map_err(|_| ParseError::InvalidContent)? };
    match invoke(&mut std::io::Cursor::new(bytes), ParseOperation::InspectSkillManifest,
        ContentFormat::Text, options, None)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::SkillScan { input_slot: 0, passed, risk, findings }] => Ok((*passed, risk.as_str().to_owned(), findings.as_slice().len())),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}

pub fn read_evolution_events(bytes: &[u8]) -> Result<String, ParseError> {
    let request = client::request(ParseOperation::ReadEvolutionEvents, ContentFormat::Text, 0);
    match client::invoke_reader(&request, &mut std::io::Cursor::new(bytes))?.result {
        ParseResult::Complete { items } => {
            let mut text = String::new();
            for item in items.as_slice() {
                let ParsedItem::Text { input_slot: 0, page: None, text: part } = item else { return Err(ParseError::InvalidContent); };
                text.push_str(part.as_str());
            }
            Ok(text)
        }
        ParseResult::Failed { code } => Err(code),
    }
}


pub fn read_skill_identity(bytes: &[u8]) -> Result<String, ParseError> {
    let request = client::request(ParseOperation::ReadSkillIdentity, ContentFormat::Text, bytes.len() as u64);
    match client::invoke_reader(&request, &mut std::io::Cursor::new(bytes))?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::Text { input_slot: 0, page: None, text }] if !text.as_str().is_empty()
                && text.as_str().len() <= 64
                && text.as_str().bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') => Ok(text.as_str().to_owned()),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}

pub fn prepare_synthesized_skill(bytes: &[u8], agent: &str, at: &str) -> Result<String, ParseError> {
    use duduclaw_isolation::bounded::Text;
    let options = ParseOptions::SynthesizedSkill {
        agent: Text::new(agent.to_owned()).map_err(|_| ParseError::InvalidContent)?,
        at: Text::new(at.to_owned()).map_err(|_| ParseError::InvalidContent)?,
    };
    match invoke(&mut std::io::Cursor::new(bytes), ParseOperation::PrepareSynthesizedSkill,
        ContentFormat::Text, options, None)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::PreparedSkill { input_slot: 0, content }] => Ok(content.as_str().to_owned()),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}


pub fn inspect_skill_identity(bytes: &[u8], name: &str) -> Result<String, ParseError> {
    let options = ParseOptions::SkillIdentity { name: duduclaw_isolation::bounded::Text::new(name.to_owned())
        .map_err(|_| ParseError::InvalidContent)? };
    match invoke(&mut std::io::Cursor::new(bytes), ParseOperation::ReadSkillIdentity,
        ContentFormat::Text, options, None)?.result {
        ParseResult::Complete { items } => match items.as_slice() {
            [ParsedItem::Text { input_slot: 0, page: None, text }] if text.as_str() == name => Ok(text.as_str().to_owned()),
            _ => Err(ParseError::InvalidContent),
        },
        ParseResult::Failed { code } => Err(code),
    }
}
