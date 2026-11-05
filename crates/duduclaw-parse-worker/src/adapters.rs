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
