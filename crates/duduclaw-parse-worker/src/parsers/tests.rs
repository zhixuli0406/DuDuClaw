use std::io::{Cursor, Write};
use std::num::{NonZeroU32, NonZeroU64};

use duduclaw_isolation::bounded::List;
use duduclaw_isolation::ids::RequestId;
use duduclaw_isolation::worker::{InputFd, WorkerLimits};

use super::*;

// Use the same generated documents as the existing knowledge extractor.
#[path = "../../../duduclaw-knowledge/src/extract/fixtures.rs"]
mod fixtures;

fn request(bytes: &[u8], operation: ParseOperation, format: ContentFormat, limit: u32) -> ParseRequest {
    ParseRequest {
        version: WORKER_PROTOCOL_VERSION, request_id: RequestId([1; 16]), operation, options: None,
        inputs: List::new(vec![InputFd { slot: 0, format, size_bytes: bytes.len() as u64 }]).unwrap(),
        outputs: List::new(vec![]).unwrap(),
        limits: WorkerLimits {
            memory_bytes: NonZeroU64::new(256 * 1024 * 1024).unwrap(),
            wall_time_ms: NonZeroU32::new(30_000).unwrap(),
            max_result_text_bytes: NonZeroU32::new(limit).unwrap(),
        },
    }
}

fn run(bytes: &[u8], operation: ParseOperation, format: ContentFormat, limit: u32) -> ParseResponse {
    let request = request(bytes, operation, format, limit);
    let mut file = tempfile::tempfile().unwrap();
    file.write_all(bytes).unwrap();
    let response = parse(&request, &mut [file]);
    response.validate_for(&request).unwrap();
    response
}

fn text(response: ParseResponse) -> String {
    let ParseResult::Complete { items } = response.result else { panic!("expected complete response") };
    items.as_slice().iter().filter_map(|item| match item {
        ParsedItem::Text { text, .. } => Some(text.as_str()), _ => None,
    }).collect()
}

fn zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in parts {
        zip.start_file(*name, options).unwrap();
        zip.write_all(bytes).unwrap();
    }
    zip.finish().unwrap().into_inner()
}

#[test]
fn text_chunks_preserve_utf8_and_budget_is_not_truncation() {
    let source = "員工👩🏽‍💻".repeat(1000);
    let response = run(source.as_bytes(), ParseOperation::ExtractDocument, ContentFormat::Text, 128 * 1024);
    assert_eq!(text(response), source);
    let failed = run(source.as_bytes(), ParseOperation::ExtractDocument, ContentFormat::Text, 10);
    assert_eq!(failed.result, ParseResult::Failed { code: ParseError::LimitExceeded });
}

#[test]
fn html_skips_active_and_hidden_subtrees() {
    let source = b"<!doctype html><html><head><script>bad</script></head><body><p>Visible &amp; safe</p><nav>menu</nav></body></html>";
    let result = text(run(source, ParseOperation::ExtractDocument, ContentFormat::Text, 1024));
    assert_eq!(result.trim(), "Visible & safe");
}

#[test]
fn docx_and_pdf_extract_the_existing_knowledge_fixtures() {
    let source = fixtures::docx(&[fixtures::DocxBlock::Para("員工 & policy")]);
    assert_eq!(text(run(&source, ParseOperation::ExtractDocument, ContentFormat::OfficeDocument, 1024)), "員工 & policy");
    let pdf = fixtures::pdf(&["Annual leave: seven days."]);
    assert!(text(run(&pdf, ParseOperation::ExtractDocument, ContentFormat::Pdf, 1024)).contains("Annual leave"));
    assert_eq!(run(&pdf, ParseOperation::ExtractDocument, ContentFormat::Pdf, 4).result,
        ParseResult::Failed { code: ParseError::LimitExceeded });
}

#[test]
fn xlsx_sparse_dimensions_do_not_allocate_a_dense_grid() {
    let bytes = fixtures::xlsx_raw_sheet(
        r#"<dimension ref="A1:XFD1048576"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>品號</t></is></c></row><row r="1048576"><c r="XFD1048576"><v>7</v></c></row></sheetData>"#);
    assert_eq!(text(run(&bytes, ParseOperation::ExtractDocument, ContentFormat::Spreadsheet, 1024)), "品號\n7");
}

#[test]
fn malformed_cfb_and_mismatched_formats_fail_bounded() {
    let malformed = [0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1, 0, 0, 0, 0];
    assert_eq!(run(&malformed, ParseOperation::ExtractDocument, ContentFormat::Spreadsheet, 1024).result,
        ParseResult::Failed { code: ParseError::InvalidContent });
    assert_eq!(run(b"not a PDF", ParseOperation::ExtractDocument, ContentFormat::Pdf, 1024).result,
        ParseResult::Failed { code: ParseError::InvalidContent });
    let response = run(b"text", ParseOperation::LoadModel, ContentFormat::Model, 1024);
    assert_eq!(response.result, ParseResult::Failed { code: ParseError::UnsupportedFormat });
    assert!(serde_json::to_vec(&response).unwrap().len() < 256);
}

#[test]
fn xml_depth_and_dtd_are_refused() {
    let deep = format!("{}<w:t>x</w:t>{}", "<x>".repeat(40), "</x>".repeat(40));
    let bytes = zip(&[("word/document.xml", deep.as_bytes())]);
    assert_eq!(run(&bytes, ParseOperation::ExtractDocument, ContentFormat::OfficeDocument, 1024).result,
        ParseResult::Failed { code: ParseError::LimitExceeded });
    let dtd = zip(&[("word/document.xml", b"<!DOCTYPE x [<!ENTITY a SYSTEM 'file:///etc/shadow'>]><x><w:t>&a;</w:t></x>")]);
    assert_eq!(run(&dtd, ParseOperation::ExtractDocument, ContentFormat::OfficeDocument, 1024).result,
        ParseResult::Failed { code: ParseError::InvalidContent });
}

#[test]
fn zip_summary_counts_actual_bytes_and_rejects_bombs() {
    let bytes = zip(&[("SKILL.md", b"# Useful skill"), ("scripts/main.py", b"print('ok')")]);
    let result = run(&bytes, ParseOperation::InspectSkillArchive, ContentFormat::Archive, 1024);
    assert_eq!(result.result, ParseResult::Complete { items: List::new(vec![ParsedItem::ArchiveSummary {
        input_slot: 0, entries: 2, expanded_bytes: 25,
    }]).unwrap() });
    let bomb = zip(&[("large.txt", &vec![0; 1024 * 1024])]);
    assert_eq!(run(&bomb, ParseOperation::InspectSkillArchive, ContentFormat::Archive, 1024).result,
        ParseResult::Failed { code: ParseError::LimitExceeded });
}

#[test]
fn archives_reject_traversal_deep_paths_and_nested_payloads() {
    for name in ["../escape", "/absolute", "C:\\absolute", "a/./b"] {
        let bytes = zip(&[(name, b"x")]);
        assert_eq!(run(&bytes, ParseOperation::InspectPackArchive, ContentFormat::Archive, 1024).result,
            ParseResult::Failed { code: ParseError::InvalidContent });
    }
    let name = format!("{}file", "dir/".repeat(33));
    let deep = zip(&[(&name, b"x")]);
    assert_eq!(run(&deep, ParseOperation::InspectPackArchive, ContentFormat::Archive, 1024).result,
        ParseResult::Failed { code: ParseError::LimitExceeded });
    let inner = zip(&[("x", b"inner")]);
    let nested = zip(&[("innocent.data", &inner)]);
    assert_eq!(run(&nested, ParseOperation::InspectPackArchive, ContentFormat::Archive, 1024).result,
        ParseResult::Failed { code: ParseError::InvalidContent });
}

#[test]
fn oversized_fd_is_refused_before_allocating_input() {
    let mut file = tempfile::tempfile().unwrap();
    file.set_len(MAX_PARSE_INPUT_BYTES + 1).unwrap();
    let mut req = request(&[], ParseOperation::ExtractDocument, ContentFormat::Text, 1024);
    req.inputs = List::new(vec![InputFd { slot: 0, format: ContentFormat::Text,
        size_bytes: MAX_PARSE_INPUT_BYTES + 1 }]).unwrap();
    assert_eq!(parse(&req, std::slice::from_mut(&mut file)).result,
        ParseResult::Failed { code: ParseError::LimitExceeded });
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1)); }
    }
    !crc
}

#[test]
fn image_dimensions_are_checked_before_pixel_allocations() {
    use image::ImageEncoder;
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(&[1, 2, 3], 1, 1, image::ExtendedColorType::Rgb8).unwrap();
    assert_eq!(image_dimensions(&png), Ok((1, 1)));
    assert!(image_dimensions(&png[..40.min(png.len())]).is_err());
    png[16..20].copy_from_slice(&100_000u32.to_be_bytes());
    let crc = crc32(&png[12..29]);
    png[29..33].copy_from_slice(&crc.to_be_bytes());
    assert_eq!(image_dimensions(&png), Err(ParseError::LimitExceeded));
}

#[test]
fn presentation_order_and_speaker_notes_match_knowledge_fixture() {
    let bytes = fixtures::pptx(&[("產品介紹", Some("講者提醒")), ("售後服務", None)]);
    let response = run(&bytes, ParseOperation::ExtractDocument, ContentFormat::OfficeDocument, 4096);
    let ParseResult::Complete { items } = response.result else { panic!("expected slides") };
    assert_eq!(items.as_slice().len(), 2);
    assert!(matches!(&items.as_slice()[0], ParsedItem::Text { page: Some(1), text, .. }
        if text.as_str() == "產品介紹\n備忘稿：講者提醒"));
    assert!(matches!(&items.as_slice()[1], ParsedItem::Text { page: Some(2), text, .. }
        if text.as_str() == "售後服務"));
}

#[test]
fn word_table_keeps_cell_delimiters() {
    let bytes = fixtures::docx(&[fixtures::DocxBlock::Table(&[&["Name", "Price"], &["Widget", "10"]])]);
    assert_eq!(text(run(&bytes, ParseOperation::ExtractDocument, ContentFormat::OfficeDocument, 1024)),
        "Name | Price\nWidget | 10");
}

#[test]
fn sheet_boundaries_and_document_headings_preserve_text() {
    let bytes = fixtures::xlsx(&[("First", &[&["alpha"]]), ("Second", &[&["beta"]])]);
    assert_eq!(text(run(&bytes, ParseOperation::ExtractDocument, ContentFormat::Spreadsheet, 1024)), "alpha\nbeta");
    let bytes = fixtures::docx(&[fixtures::DocxBlock::Heading(1, "Policy"), fixtures::DocxBlock::Para("body")]);
    assert_eq!(text(run(&bytes, ParseOperation::ExtractDocument, ContentFormat::OfficeDocument, 1024)), "Policy\nbody");
}
