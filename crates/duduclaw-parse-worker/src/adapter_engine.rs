//! Rich adapters execute only after the process sandbox and fd checks.
use std::io::{Cursor, Read};
use calamine::{Data, Reader};
use duduclaw_isolation::{bounded::{List, Text}, worker::*};
use crate::parsers::archive;

pub(crate) fn parse(request: &ParseRequest, bytes: &[u8]) -> Result<ParsedItem, ParseError> {
    match &request.options {
        Some(ParseOptions::Knowledge { format, max_chars }) => {
            let format: crate::extract::Format = serde_json::from_value(serde_json::to_value(format)
                .map_err(|_| ParseError::InvalidContent)?).map_err(|_| ParseError::InvalidContent)?;
            if format.is_ooxml() { inspect_document(bytes, &default_document_policy())?; }
            let raw = crate::extract::extract(format, bytes, *max_chars as usize)
                .map_err(|error| if error.code == "too_many_chars" { ParseError::LimitExceeded }
                    else { ParseError::InvalidContent })?;
            let output: KnowledgeOutput = serde_json::from_value(serde_json::to_value(raw)
                .map_err(|_| ParseError::InvalidContent)?).map_err(|_| ParseError::LimitExceeded)?;
            Ok(ParsedItem::Knowledge { input_slot: 0, output })
        }
        Some(ParseOptions::Table(options)) => {
            let output = match options.format {
                TableFormat::Csv => read_csv(bytes, options)?,
                TableFormat::Spreadsheet => read_spreadsheet(bytes, options)?,
            };
            Ok(ParsedItem::Table { input_slot: 0, output })
        }
        Some(ParseOptions::Document(policy)) => {
            let (entries, expanded_bytes) = inspect_document(bytes, policy)?;
            Ok(ParsedItem::ArchiveSummary { input_slot: 0, entries, expanded_bytes })
        }
        _ => Err(ParseError::UnsupportedFormat),
    }
}

fn default_document_policy() -> DocumentPolicy {
    DocumentPolicy { max_uncompressed_bytes: 64 * 1024 * 1024, max_entries: 1000,
        max_compression_ratio: 100, max_xml_depth: 32,
        xml_scan_part_bytes: 8 * 1024 * 1024, xml_scan_total_bytes: 64 * 1024 * 1024 }
}

/// Unlike the legacy prefix scanner, every XML part must fit the scan budget.
/// Nested containers share one budget and an explicit work stack.
fn inspect_document(bytes: &[u8], policy: &DocumentPolicy) -> Result<(u32, u64), ParseError> {
    if !zip_magic(bytes) { return Ok((0, 0)); }
    let cap = policy.max_uncompressed_bytes.min(64 * 1024 * 1024);
    let mut pending = vec![(bytes.to_vec(), 0u32)];
    let mut total = 0u64;
    let mut entries = 0u32;
    let mut xml_bytes = 0u64;
    while let Some((bytes, depth)) = pending.pop() {
        if depth >= 4 { return Err(ParseError::LimitExceeded); }
        archive::inspect(&bytes, false)?;
        let mut zip = archive::open(&bytes)?;
        entries = entries.checked_add(zip.len() as u32).ok_or(ParseError::LimitExceeded)?;
        if entries > policy.max_entries.min(1000) { return Err(ParseError::LimitExceeded); }
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|_| ParseError::InvalidContent)?;
            if entry.is_dir() { continue; }
            if entry.size() > cap.saturating_sub(total)
                || entry.size() > entry.compressed_size().max(1)
                    .saturating_mul(u64::from(policy.max_compression_ratio.min(100))) {
                return Err(ParseError::LimitExceeded);
            }
            let name = std::str::from_utf8(entry.name_raw()).map_err(|_| ParseError::InvalidContent)?.to_lowercase();
            let mut body = Vec::new();
            (&mut entry).take(cap.saturating_sub(total) + 1).read_to_end(&mut body)
                .map_err(|_| ParseError::InvalidContent)?;
            if body.len() as u64 != entry.size() { return Err(ParseError::InvalidContent); }
            total = total.checked_add(body.len() as u64).ok_or(ParseError::LimitExceeded)?;
            if total > cap { return Err(ParseError::LimitExceeded); }
            if zip_magic(&body) {
                if body.len() > 32 * 1024 * 1024 { return Err(ParseError::LimitExceeded); }
                pending.push((body, depth + 1));
            } else if name.rsplit_once('.').is_some_and(|(_, ext)| matches!(ext, "xml" | "rels")) {
                if body.len() as u64 > policy.xml_scan_part_bytes.min(8 * 1024 * 1024) {
                    return Err(ParseError::LimitExceeded);
                }
                xml_bytes = xml_bytes.checked_add(body.len() as u64).ok_or(ParseError::LimitExceeded)?;
                if xml_bytes > policy.xml_scan_total_bytes.min(64 * 1024 * 1024) {
                    return Err(ParseError::LimitExceeded);
                }
                inspect_xml(&body, policy.max_xml_depth.min(32))?;
            }
        }
    }
    Ok((entries, total))
}
fn zip_magic(bytes: &[u8]) -> bool {
    matches!(bytes.get(..4), Some(b"PK\x03\x04" | b"PK\x05\x06"))
}
fn inspect_xml(bytes: &[u8], max_depth: u32) -> Result<(), ParseError> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_reader(bytes);
    let mut depth = 0u32;
    loop {
        match reader.read_event().map_err(|_| ParseError::InvalidContent)? {
            Event::Start(element) => {
                depth += 1;
                if depth > max_depth { return Err(ParseError::LimitExceeded); }
                for attr in element.attributes() { attr.map_err(|_| ParseError::InvalidContent)?; }
            }
            Event::Empty(element) => {
                if depth >= max_depth { return Err(ParseError::LimitExceeded); }
                for attr in element.attributes() { attr.map_err(|_| ParseError::InvalidContent)?; }
            }
            Event::End(_) => { depth = depth.checked_sub(1).ok_or(ParseError::InvalidContent)?; }
            Event::DocType(_) => return Err(ParseError::InvalidContent),
            Event::Eof => return if depth == 0 { Ok(()) } else { Err(ParseError::InvalidContent) },
            _ => {},
        }
    }
}

fn text<const N: usize>(value: String) -> Result<Text<N>, ParseError> {
    Text::new(value).map_err(|_| ParseError::LimitExceeded)
}
fn list<T, const N: usize>(value: Vec<T>) -> Result<List<T, N>, ParseError> {
    List::new(value).map_err(|_| ParseError::LimitExceeded)
}
fn normalize_columns(raw: &[String]) -> Result<List<Text<256>, 256>, ParseError> {
    if raw.len() > 256 { return Err(ParseError::LimitExceeded); }
    let mut out = Vec::<String>::new();
    for (idx, name) in raw.iter().enumerate() {
        let trimmed = name.trim();
        let base = if trimmed.is_empty() { format!("c{}", idx + 1) } else { trimmed.to_owned() };
        let mut candidate = base.clone();
        let mut suffix = idx + 1;
        while out.contains(&candidate) {
            candidate = format!("{base}_{suffix}");
            suffix += 1;
        }
        out.push(candidate);
    }
    list(out.into_iter().map(text).collect::<Result<Vec<_>, _>>()?)
}
fn read_csv(bytes: &[u8], options: &TableOptions) -> Result<TableOutput, ParseError> {
    let mut reader = csv::ReaderBuilder::new().delimiter(options.delimiter)
        .has_headers(false).flexible(true).from_reader(bytes);
    let mut records = reader.records();
    let mut columns = list(Vec::new())?;
    if options.has_header {
        if let Some(row) = records.next() {
            let row = row.map_err(|_| ParseError::InvalidContent)?;
            columns = normalize_columns(&row.iter().map(str::to_owned).collect::<Vec<_>>())?;
        }
    }
    let mut rows = Vec::new();
    let mut truncated = false;
    for (index, row) in records.enumerate() {
        let row = row.map_err(|_| ParseError::InvalidContent)?;
        if index < options.offset as usize { continue; }
        if rows.len() >= usize::from(options.limit) { truncated = true; break; }
        if columns.as_slice().is_empty() {
            columns = normalize_columns(&(1..=row.len()).map(|n| format!("c{n}")).collect::<Vec<_>>())?;
        }
        rows.push(list(row.iter().map(|value| text(value.to_owned()).map(TableCell::String))
            .collect::<Result<Vec<_>, _>>()?)?);
    }
    Ok(TableOutput { columns, rows: list(rows)?, truncated, sheet: None, sheets: list(vec![])? })
}

fn cell_to_json(cell: &Data) -> serde_json::Value {
    use serde_json::{Value, json};
    match cell {
        Data::Int(i) => json!(i),
        Data::Float(f) if f.fract() == 0.0 && f.abs() < 9.0e15 => json!(*f as i64),
        Data::Float(f) => json!(f),
        Data::Bool(b) => json!(b),
        Data::Empty => Value::Null,
        Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => json!(s),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(date) => json!(date.format("%Y-%m-%dT%H:%M:%S").to_string()),
            None => json!(dt.as_f64()),
        },
        Data::Error(e) => json!(format!("#ERROR:{e:?}")),
    }
}
fn cell(value: serde_json::Value) -> Result<TableCell, ParseError> {
    use serde_json::Value;
    match value {
        Value::Null => Ok(TableCell::Null),
        Value::Bool(value) => Ok(TableCell::Bool(value)),
        Value::Number(value) => Ok(TableCell::Number(value)),
        Value::String(value) => text(value).map(TableCell::String),
        _ => Err(ParseError::InvalidContent),
    }
}
fn read_spreadsheet(bytes: &[u8], options: &TableOptions) -> Result<TableOutput, ParseError> {
    if bytes.len() > 32 * 1024 * 1024 { return Err(ParseError::LimitExceeded); }
    if zip_magic(bytes) { inspect_document(bytes, &default_document_policy())?; }
    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes))
        .map_err(|_| ParseError::InvalidContent)?;
    let names = book.sheet_names().to_vec();
    let requested = options.sheet.as_ref().map(|s| s.as_str());
    let sheet = match requested {
        Some(name) => names.iter().find(|s| s.as_str() == name),
        None => names.first(),
    }.ok_or(ParseError::InvalidContent)?.clone();
    let range = book.worksheet_range(&sheet).map_err(|_| ParseError::InvalidContent)?;
    if range.get_size().0.saturating_mul(range.get_size().1) > 50_000 {
        return Err(ParseError::LimitExceeded);
    }
    let mut iter = range.rows();
    let raw = iter.next().unwrap_or(&[]).iter().map(|c| match cell_to_json(c) {
        serde_json::Value::String(s) => s,
        serde_json::Value::Null => String::new(),
        other => other.to_string(),
    }).collect::<Vec<_>>();
    let columns = normalize_columns(&raw)?;
    let mut rows = Vec::new();
    let mut truncated = false;
    for (index, row) in iter.enumerate() {
        if index < options.offset as usize { continue; }
        if rows.len() >= usize::from(options.limit) { truncated = true; break; }
        rows.push(list(row.iter().map(|c| cell(cell_to_json(c))).collect::<Result<Vec<_>, _>>()?)?);
    }
    Ok(TableOutput { columns, rows: list(rows)?, truncated, sheet: Some(text(sheet)?),
        sheets: list(names.into_iter().map(text).collect::<Result<Vec<_>, _>>()?)? })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn options(format: TableFormat) -> TableOptions {
        TableOptions { format, delimiter: b',', has_header: true, offset: 0, limit: 2, sheet: None }
    }
    fn zip(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, bytes) in parts {
            writer.start_file(*name, zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Stored)).unwrap();
            writer.write_all(bytes).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }
    #[test]
    fn typed_csv_preserves_headers_ragged_cells_offset_and_truncation() {
        let output = read_csv(b"a,a,\n1,2\n3,4,5,6\n7,8,9\n", &options(TableFormat::Csv)).unwrap();
        assert_eq!(output.columns.as_slice().iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            ["a", "a_2", "c3"]);
        assert_eq!(output.rows.as_slice()[0].as_slice().len(), 2);
        assert_eq!(output.rows.as_slice()[1].as_slice().len(), 4);
        assert!(output.truncated);
        let mut options = options(TableFormat::Csv);
        options.has_header = false;
        options.offset = 1;
        options.delimiter = b'\t';
        let output = read_csv(b"first\trow\nsecond\trow\n", &options).unwrap();
        assert_eq!(output.columns.as_slice()[0].as_str(), "c1");
        assert!(!output.truncated);
        assert!(matches!(&output.rows.as_slice()[0].as_slice()[0], TableCell::String(s) if s.as_str() == "second"));
    }
    #[test]
    fn custom_document_policy_narrows_xml_and_expansion() {
        let bytes = zip(&[("word/document.xml", b"<a><b><c/></b></a>")]);
        let mut policy = default_document_policy();
        assert!(inspect_document(&bytes, &policy).is_ok());
        policy.max_xml_depth = 2;
        assert_eq!(inspect_document(&bytes, &policy), Err(ParseError::LimitExceeded));
        policy = default_document_policy();
        policy.max_uncompressed_bytes = 1;
        assert_eq!(inspect_document(&bytes, &policy), Err(ParseError::LimitExceeded));
        policy = default_document_policy();
        policy.xml_scan_part_bytes = 1;
        assert_eq!(inspect_document(&bytes, &policy), Err(ParseError::LimitExceeded));
        let dtd = zip(&[("word/document.xml", b"<!DOCTYPE a SYSTEM 'http://127.0.0.1'><a/>")]);
        assert_eq!(inspect_document(&dtd, &default_document_policy()), Err(ParseError::InvalidContent));
    }
    #[test]
    fn nested_document_containers_share_budget_and_depth() {
        let mut bytes = zip(&[("a.xml", b"<a/>")]);
        for _ in 0..4 { bytes = zip(&[("nested.zip", &bytes)]); }
        assert_eq!(inspect_document(&bytes, &default_document_policy()), Err(ParseError::LimitExceeded));
        let nested = zip(&[("one.xml", b"<a/>"), ("two.xml", b"<a/>")]);
        let bytes = zip(&[("nested.zip", &nested)]);
        let mut policy = default_document_policy();
        policy.max_entries = 2;
        assert_eq!(inspect_document(&bytes, &policy), Err(ParseError::LimitExceeded));
    }
    #[test]
    fn knowledge_csv_preserves_row_metadata_and_markdown_sections() {
        let bytes = b"name,amount\r\nx,1\r\n,,\r\ny,2\r\n";
        let mut request = crate::client::request(ParseOperation::ExtractKnowledge, ContentFormat::Text, bytes.len() as u64);
        request.options = Some(ParseOptions::Knowledge { format: KnowledgeFormat::Csv, max_chars: 1000 });
        let ParsedItem::Knowledge { output, .. } = parse(&request, bytes).unwrap() else { panic!("knowledge") };
        assert_eq!(output.segments.as_slice()[0].kind, SegmentKind::Sheet);
        assert_eq!(output.segments.as_slice()[0].line_rows.as_ref().unwrap().as_slice(), [1, 2, 4]);
        assert_eq!(output.segments.as_slice()[0].text.as_str(), "name | amount\nx | 1\ny | 2");
        request.options = Some(ParseOptions::Knowledge { format: KnowledgeFormat::Md, max_chars: 1000 });
        let ParsedItem::Knowledge { output, .. } = parse(&request, b"# Title\nhello\n## Sub\nworld").unwrap() else { panic!("knowledge") };
        assert!(output.segments.as_slice().iter().any(|s| s.kind == SegmentKind::Section));
    }
    #[test]
    fn typed_workbook_selection_and_cell_types_match_legacy() {
        let bytes = crate::extract::fixtures::xlsx(&[
            ("numbers", &[&["name", "amount"], &["one", "12"], &["two", "3"]]),
            ("other", &[&["label"], &["value"]]),
        ]);
        let output = read_spreadsheet(&bytes, &options(TableFormat::Spreadsheet)).unwrap();
        assert_eq!(output.sheet.as_ref().unwrap().as_str(), "numbers");
        assert_eq!(output.sheets.as_slice().len(), 2);
        assert!(matches!(&output.rows.as_slice()[0].as_slice()[1], TableCell::Number(n) if n.as_i64() == Some(12)));
        let mut selected = options(TableFormat::Spreadsheet);
        selected.sheet = Some(Text::new("other".into()).unwrap());
        let output = read_spreadsheet(&bytes, &selected).unwrap();
        assert_eq!(output.columns.as_slice()[0].as_str(), "label");
        selected.sheet = Some(Text::new("missing".into()).unwrap());
        assert_eq!(read_spreadsheet(&bytes, &selected), Err(ParseError::InvalidContent));
    }
    #[test]
    fn typed_table_and_knowledge_output_bounds_are_fail_closed() {
        let bytes = format!("a\n{}", "x".repeat(8193));
        assert_eq!(read_csv(bytes.as_bytes(), &options(TableFormat::Csv)), Err(ParseError::LimitExceeded));
        let mut request = crate::client::request(ParseOperation::ExtractKnowledge, ContentFormat::Text, 8193);
        request.options = Some(ParseOptions::Knowledge { format: KnowledgeFormat::Txt, max_chars: 10000 });
        assert_eq!(parse(&request, "x".repeat(8193).as_bytes()), Err(ParseError::LimitExceeded));
        assert_eq!(read_spreadsheet(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1],
            &options(TableFormat::Spreadsheet)), Err(ParseError::InvalidContent));
    }
}
