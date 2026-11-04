use std::collections::BTreeMap;
use std::io::{Cursor, Write};

use calamine::{Data, Reader, Xls, Xlsx, Ods};
use duduclaw_isolation::worker::{ContentFormat, ParseError};
use quick_xml::events::Event;

use super::{Output, archive};

struct Body {
    text: String,
    limit: usize,
    exceeded: bool,
}

impl Body {
    fn new(limit: usize) -> Self { Self { text: String::new(), limit, exceeded: false } }
    fn push(&mut self, text: &str) -> Result<(), ParseError> {
        if text.len() > self.limit.saturating_sub(self.text.len()) {
            self.exceeded = true;
            return Err(ParseError::LimitExceeded);
        }
        self.text.push_str(text);
        Ok(())
    }
    fn newline(&mut self) -> Result<(), ParseError> {
        if !self.text.is_empty() && !self.text.ends_with('\n') { self.push("\n")?; }
        Ok(())
    }
}

impl Write for Body {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let text = std::str::from_utf8(bytes).map_err(|_| std::io::ErrorKind::InvalidData)?;
        self.push(text).map_err(|_| std::io::ErrorKind::OutOfMemory)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

pub(super) fn extract(format: ContentFormat, bytes: &[u8], slot: u16, out: &mut Output)
    -> Result<(), ParseError>
{
    match format {
        ContentFormat::Text => {
            let text = decode_text(bytes)?;
            let leading = text.trim_start();
            let first_token = leading.split(|c: char| c.is_whitespace() || c == '>')
                .next().unwrap_or("").to_ascii_lowercase();
            if matches!(first_token.as_str(), "<html" | "<!doctype") {
                let visible = html(&text, out.remaining)?;
                out.text(slot, None, &visible)
            } else {
                out.text(slot, None, &text)
            }
        }
        ContentFormat::Pdf => pdf(bytes, slot, out),
        ContentFormat::OfficeDocument => office(bytes, slot, out),
        ContentFormat::Spreadsheet => spreadsheet(bytes, slot, out),
        _ => Err(ParseError::UnsupportedFormat),
    }
}

fn decode_text(bytes: &[u8]) -> Result<String, ParseError> {
    let body = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let text = if body.get(..2) == Some(&[0xff, 0xfe]) {
        encoding_rs::UTF_16LE.decode(body).0.into_owned()
    } else if body.get(..2) == Some(&[0xfe, 0xff]) {
        encoding_rs::UTF_16BE.decode(body).0.into_owned()
    } else if let Ok(text) = std::str::from_utf8(body) {
        text.to_owned()
    } else {
        encoding_rs::BIG5.decode(body).0.into_owned()
    };
    let count = text.chars().count();
    let replacement = text.chars().filter(|ch| *ch == '\u{fffd}').count();
    if text.chars().any(|c| c == '\0') || replacement.saturating_mul(100) > count {
        return Err(ParseError::InvalidContent);
    }
    Ok(text)
}

fn html(source: &str, limit: usize) -> Result<String, ParseError> {
    use ego_tree::iter::Edge;
    use scraper::{Html, Node};
    const SKIP: &[&str] = &["script", "style", "nav", "footer", "header", "noscript",
        "template", "svg", "form", "aside", "head"];
    const BLOCK: &[&str] = &["p", "div", "li", "tr", "br", "pre", "blockquote", "section",
        "article", "main", "table", "ul", "ol", "dd", "dt", "h1", "h2", "h3", "h4", "h5", "h6", "td", "th"];
    let doc = Html::parse_document(source);
    let mut body = Body::new(limit);
    let mut depth = 0usize;
    let mut skip = 0usize;
    for edge in doc.root_element().traverse() {
        match edge {
            Edge::Open(node) => match node.value() {
                Node::Element(el) => {
                    depth += 1;
                    if depth > 32 { return Err(ParseError::LimitExceeded); }
                    if skip > 0 || SKIP.contains(&el.name()) { skip += 1; }
                    else if BLOCK.contains(&el.name()) { body.newline()?; }
                }
                Node::Text(text) if skip == 0 => {
                    for word in text.split_whitespace() {
                        if !body.text.is_empty() && !body.text.ends_with(['\n', ' ']) { body.push(" ")?; }
                        body.push(word)?;
                    }
                }
                _ => {}
            },
            Edge::Close(node) => if let Node::Element(el) = node.value() {
                depth = depth.saturating_sub(1);
                if skip > 0 { skip -= 1; }
                else if BLOCK.contains(&el.name()) { body.newline()?; }
            },
        }
    }
    Ok(body.text)
}

fn pdf(bytes: &[u8], slot: u16, out: &mut Output) -> Result<(), ParseError> {
    if bytes.get(..5) != Some(b"%PDF-") { return Err(ParseError::InvalidContent); }
    let mut doc = pdf_extract::Document::load_mem(bytes).map_err(|_| ParseError::InvalidContent)?;
    if doc.is_encrypted() && doc.decrypt("").is_err() { return Err(ParseError::InvalidContent); }
    let pages = doc.get_pages();
    if pages.len() > 1000 { return Err(ParseError::LimitExceeded); }
    for page in pages.keys() {
        let mut body = Body::new(out.remaining);
        let result = {
            let writer: &mut dyn Write = &mut body;
            let mut output = pdf_extract::PlainTextOutput::new(writer);
            pdf_extract::output_doc_page(&doc, &mut output, *page)
        };
        if body.exceeded { return Err(ParseError::LimitExceeded); }
        result.map_err(|_| ParseError::InvalidContent)?;
        out.text(slot, Some(*page), &body.text)?;
    }
    Ok(())
}

fn local_name(bytes: &[u8]) -> Result<String, ParseError> {
    let name = std::str::from_utf8(bytes).map_err(|_| ParseError::InvalidContent)?;
    Ok(name.rsplit(':').next().unwrap_or(name).to_owned())
}

/// Only visible Office text runs are emitted. DTDs, unresolved entities and
/// excessive nesting are refused; no external relationship is fetched.
fn office_xml(xml: &str, limit: usize) -> Result<String, ParseError> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut stack = Vec::<String>::new();
    let mut body = Body::new(limit);
    loop {
        let event = reader.read_event().map_err(|_| ParseError::InvalidContent)?;
        match event {
            Event::Start(element) => {
                for attr in element.attributes() { attr.map_err(|_| ParseError::InvalidContent)?; }
                stack.push(local_name(element.name().as_ref())?);
                if stack.len() > 32 { return Err(ParseError::LimitExceeded); }
            }
            Event::Empty(element) => {
                if stack.len() >= 32 { return Err(ParseError::LimitExceeded); }
                for attr in element.attributes() { attr.map_err(|_| ParseError::InvalidContent)?; }
                let name = local_name(element.name().as_ref())?;
                if name == "br" { body.newline()?; }
                if name == "tab" { body.push("\t")?; }
            }
            Event::End(element) => {
                let name = local_name(element.name().as_ref())?;
                if stack.pop().as_deref() != Some(name.as_str()) { return Err(ParseError::InvalidContent); }
                if name == "tc" {
                    body.text = body.text.trim_end().to_owned();
                    body.push(" | ")?;
                }
                if name == "tr" {
                    if let Some(trimmed) = body.text.strip_suffix(" | ") { body.text = trimmed.to_owned(); }
                    body.newline()?;
                } else if name == "p" {
                    if stack.iter().any(|s| s == "tc") { body.push(" ")?; }
                    else { body.newline()?; }
                }
            }
            Event::Text(text) => {
                if visible_run(&stack) {
                    body.push(&text.decode().map_err(|_| ParseError::InvalidContent)?)?;
                }
            }
            Event::CData(text) => {
                if visible_run(&stack) {
                    body.push(&text.decode().map_err(|_| ParseError::InvalidContent)?)?;
                }
            }
            Event::GeneralRef(reference) => {
                let decoded = reference.decode().map_err(|_| ParseError::InvalidContent)?;
                let value = reference.resolve_char_ref().map_err(|_| ParseError::InvalidContent)?
                    .or_else(|| match decoded.as_ref() {
                        "lt" => Some('<'), "gt" => Some('>'), "amp" => Some('&'),
                        "apos" => Some('\''), "quot" => Some('"'), _ => None,
                    }).ok_or(ParseError::InvalidContent)?;
                if visible_run(&stack) { body.push(&value.to_string())?; }
            }
            Event::DocType(_) => return Err(ParseError::InvalidContent),
            Event::Eof => {
                if !stack.is_empty() { return Err(ParseError::InvalidContent); }
                break;
            }
            _ => {}
        }
    }
    Ok(body.text.trim_end().to_owned())
}

fn visible_run(stack: &[String]) -> bool {
    stack.last().is_some_and(|s| s == "t")
        && !stack.iter().any(|s| matches!(s.as_str(), "del" | "fld" | "instrText" | "delText"))
}

fn office(bytes: &[u8], slot: u16, out: &mut Output) -> Result<(), ParseError> {
    archive::inspect(bytes, false)?;
    let mut zip = archive::open(bytes)?;
    if let Some(xml) = archive::part(&mut zip, "word/document.xml")? {
        let text = office_xml(&xml, out.remaining)?;
        return out.text(slot, None, &text);
    }
    let slides = slide_order(&mut zip)?;
    if slides.is_empty() { return Err(ParseError::UnsupportedFormat); }
    for (index, name) in slides.iter().enumerate() {
        let xml = archive::part(&mut zip, name)?.ok_or(ParseError::InvalidContent)?;
        let mut text = office_xml(&xml, out.remaining)?;
        let (directory, filename) = name.rsplit_once('/').ok_or(ParseError::InvalidContent)?;
        let rels_name = format!("{directory}/_rels/{filename}.rels");
        if let Some(xml) = archive::part(&mut zip, &rels_name)? {
            for (_, (kind, target)) in relationships(&xml)? {
                if is_relationship(&kind, "notesSlide") {
                    let target = resolve_target(name, &target)?;
                    let xml = archive::part(&mut zip, &target)?.ok_or(ParseError::InvalidContent)?;
                    let notes = office_xml(&xml, out.remaining)?;
                    if !notes.is_empty() {
                        let mut body = Body::new(out.remaining);
                        body.push(&text)?;
                        body.newline()?;
                        body.push("備忘稿：")?;
                        body.push(&notes.replace('\n', " "))?;
                        text = body.text;
                    }
                }
            }
        }
        out.text(slot, Some(index as u32 + 1), &text)?;
    }
    Ok(())
}

fn is_relationship(kind: &str, suffix: &str) -> bool {
    kind == format!("http://schemas.openxmlformats.org/officeDocument/2006/relationships/{suffix}")
        || kind == format!("http://purl.oclc.org/ooxml/officeDocument/relationships/{suffix}")
}

fn resolve_target(owner: &str, target: &str) -> Result<String, ParseError> {
    if target.len() > 4096 || target.chars().any(|c| c.is_control() || matches!(c, ':' | '\\')) {
        return Err(ParseError::InvalidContent);
    }
    let mut parts = owner.split('/').collect::<Vec<_>>();
    parts.pop();
    let relative = if let Some(absolute) = target.strip_prefix('/') {
        parts.clear();
        absolute
    } else { target };
    for part in relative.split('/') {
        match part {
            ".." => { parts.pop().ok_or(ParseError::InvalidContent)?; }
            "." => {}
            "" => return Err(ParseError::InvalidContent),
            name => parts.push(name),
        }
        if parts.len() > 32 { return Err(ParseError::LimitExceeded); }
    }
    Ok(parts.join("/"))
}

type MetadataElement = (String, Vec<(String, String)>);

fn metadata_elements(xml: &str) -> Result<Vec<MetadataElement>, ParseError> {
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut depth = 0usize;
    let mut elements = Vec::new();
    loop {
        let event = reader.read_event().map_err(|_| ParseError::InvalidContent)?;
        let opens = matches!(&event, Event::Start(_));
        match event {
            Event::Start(element) | Event::Empty(element) => {
                if depth >= 32 { return Err(ParseError::LimitExceeded); }
                if opens { depth += 1; }
                let name = local_name(element.name().as_ref())?;
                let mut attrs = Vec::new();
                for attr in element.attributes() {
                    let attr = attr.map_err(|_| ParseError::InvalidContent)?;
                    let key = local_name(attr.key.as_ref())?;
                    let value = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)
                        .map_err(|_| ParseError::InvalidContent)?.into_owned();
                    attrs.push((key, value));
                    if attrs.len() > 64 { return Err(ParseError::LimitExceeded); }
                }
                elements.push((name, attrs));
                if elements.len() > 10_000 { return Err(ParseError::LimitExceeded); }
            }
            Event::End(_) => { depth = depth.checked_sub(1).ok_or(ParseError::InvalidContent)?; }
            Event::DocType(_) => return Err(ParseError::InvalidContent),
            Event::Eof => {
                if depth != 0 { return Err(ParseError::InvalidContent); }
                break;
            }
            _ => {}
        }
    }
    Ok(elements)
}

fn relationships(xml: &str) -> Result<BTreeMap<String, (String, String)>, ParseError> {
    let mut result = BTreeMap::new();
    for (name, attrs) in metadata_elements(xml)? {
        if name != "Relationship" { continue; }
        let attr = |key: &str| attrs.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
        if attr("TargetMode") == Some("External") { continue; }
        let id = attr("Id").ok_or(ParseError::InvalidContent)?;
        let target = attr("Target").ok_or(ParseError::InvalidContent)?;
        let kind = attr("Type").ok_or(ParseError::InvalidContent)?;
        if result.insert(id.to_owned(), (kind.to_owned(), target.to_owned())).is_some() {
            return Err(ParseError::InvalidContent);
        }
    }
    Ok(result)
}

fn slide_order(zip: &mut zip::ZipArchive<Cursor<&[u8]>>) -> Result<Vec<String>, ParseError> {
    if let (Some(presentation), Some(rels)) = (
        archive::part(zip, "ppt/presentation.xml")?,
        archive::part(zip, "ppt/_rels/presentation.xml.rels")?,
    ) {
        let rels = relationships(&rels)?;
        let mut slides = Vec::new();
        for (name, attrs) in metadata_elements(&presentation)? {
            if name != "sldId" { continue; }
            let id = attrs.iter().find(|(key, value)| key == "id"
                && !value.chars().all(|c| c.is_ascii_digit()))
                .map(|(_, value)| value).ok_or(ParseError::InvalidContent)?;
            let (kind, target) = rels.get(id).ok_or(ParseError::InvalidContent)?;
            if !is_relationship(kind, "slide") { return Err(ParseError::InvalidContent); }
            slides.push(resolve_target("ppt/presentation.xml", target)?);
        }
        if !slides.is_empty() { return Ok(slides); }
    }
    let mut slides = Vec::<(u32, String)>::new();
    for index in 0..zip.len() {
        let entry = zip.by_index(index).map_err(|_| ParseError::InvalidContent)?;
        let name = std::str::from_utf8(entry.name_raw()).map_err(|_| ParseError::InvalidContent)?;
        if let Some(number) = name.strip_prefix("ppt/slides/slide").and_then(|n| n.strip_suffix(".xml"))
            .and_then(|n| n.parse::<u32>().ok()) {
            slides.push((number, name.to_owned()));
        }
    }
    slides.sort();
    Ok(slides.into_iter().map(|(_, name)| name).collect())
}

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty | Data::Error(_) => String::new(),
        Data::String(text) | Data::DateTimeIso(text) | Data::DurationIso(text) => text.clone(),
        Data::Bool(value) => if *value { "TRUE" } else { "FALSE" }.to_owned(),
        Data::Int(value) => value.to_string(),
        Data::Float(value) => value.to_string(),
        Data::DateTime(value) => match value.as_datetime() {
            Some(date) => {
                let formatted = date.to_string();
                formatted.strip_suffix(" 00:00:00").unwrap_or(&formatted).to_owned()
            }
            None => value.as_f64().to_string(),
        },
    }
}

fn spreadsheet(bytes: &[u8], slot: u16, out: &mut Output) -> Result<(), ParseError> {
    let cursor = Cursor::new(bytes);
    if bytes.get(..8) == Some(&[0xd0, 0xcf, 0x11, 0xe0, 0xa1, 0xb1, 0x1a, 0xe1]) {
        let book = calamine::open_workbook_from_rs::<Xls<_>, _>(cursor)
            .map_err(|_| ParseError::InvalidContent)?;
        return dense_sheets(book, slot, out);
    }
    archive::inspect(bytes, false)?;
    let mut zip = archive::open(bytes)?;
    if archive::part(&mut zip, "xl/workbook.xml")?.is_some() {
        let mut book = calamine::open_workbook_from_rs::<Xlsx<_>, _>(cursor)
            .map_err(|_| ParseError::InvalidContent)?;
        let mut cells_read = 0usize;
        for (sheet_index, name) in book.sheet_names().into_iter().enumerate() {
            let mut reader = book.worksheet_cells_reader(&name).map_err(|_| ParseError::InvalidContent)?;
            let mut rows = BTreeMap::<u32, BTreeMap<u32, String>>::new();
            let mut bytes_read = 0usize;
            while let Some(cell) = reader.next_cell().map_err(|_| ParseError::InvalidContent)? {
                cells_read += 1;
                if cells_read > 50_000 { return Err(ParseError::LimitExceeded); }
                let (row, col) = cell.get_position();
                let text = cell_text(&Data::from(cell.get_value().clone()));
                if text.is_empty() { continue; }
                bytes_read = bytes_read.checked_add(text.len()).ok_or(ParseError::LimitExceeded)?;
                if bytes_read > out.remaining || col >= 16_384 || row >= 1_048_576 {
                    return Err(ParseError::LimitExceeded);
                }
                if rows.entry(row).or_default().insert(col, text).is_some() {
                    return Err(ParseError::InvalidContent);
                }
            }
            let mut body = Body::new(out.remaining);
            if sheet_index > 0 { body.push("\n")?; }
            for cells in rows.values() {
                if !body.text.is_empty() { body.newline()?; }
                let last = *cells.keys().next_back().ok_or(ParseError::InvalidContent)?;
                if last < 64 {
                    for col in 0..=last {
                        if col > 0 { body.push(" | ")?; }
                        body.push(cells.get(&col).map(String::as_str).unwrap_or(""))?;
                    }
                } else {
                    for (index, text) in cells.values().enumerate() {
                        if index > 0 { body.push(" | ")?; }
                        body.push(text)?;
                    }
                }
            }
            out.text(slot, None, &body.text)?;
        }
        Ok(())
    } else if archive::part(&mut zip, "mimetype")?.as_deref()
        == Some("application/vnd.oasis.opendocument.spreadsheet") {
        let book = calamine::open_workbook_from_rs::<Ods<_>, _>(cursor)
            .map_err(|_| ParseError::InvalidContent)?;
        dense_sheets(book, slot, out)
    } else {
        Err(ParseError::UnsupportedFormat)
    }
}

fn dense_sheets<'a, R: Reader<Cursor<&'a [u8]>>>(mut book: R, slot: u16, out: &mut Output)
    -> Result<(), ParseError>
{
    let mut cells = 0usize;
    for (sheet_index, name) in book.sheet_names().into_iter().enumerate() {
        let range = book.worksheet_range(&name).map_err(|_| ParseError::InvalidContent)?;
        cells = cells.checked_add(range.get_size().0.saturating_mul(range.get_size().1))
            .ok_or(ParseError::LimitExceeded)?;
        if cells > 50_000 { return Err(ParseError::LimitExceeded); }
        let mut body = Body::new(out.remaining);
        if sheet_index > 0 { body.push("\n")?; }
        for row in range.rows() {
            if !body.text.is_empty() { body.newline()?; }
            for (index, cell) in row.iter().enumerate() {
                if index > 0 { body.push(" | ")?; }
                body.push(&cell_text(cell))?;
            }
        }
        out.text(slot, None, &body.text)?;
    }
    Ok(())
}
