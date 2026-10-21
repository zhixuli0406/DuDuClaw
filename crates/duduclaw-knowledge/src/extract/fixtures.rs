//! Small generated documents for the extractor tests (test builds only).
//! Built in code so the repository carries no binary fixtures.

use std::io::Write;

fn zip_of(parts: &[(&str, String)]) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut buf);
        let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (name, body) in parts {
            w.start_file(*name, opts).unwrap();
            w.write_all(body.as_bytes()).unwrap();
        }
        w.finish().unwrap();
    }
    buf.into_inner()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

pub(crate) enum DocxBlock<'a> {
    Heading(u8, &'a str),
    Para(&'a str),
    Table(&'a [&'a [&'a str]]),
}

pub(crate) fn docx(blocks: &[DocxBlock<'_>]) -> Vec<u8> {
    let mut body = String::new();
    for b in blocks {
        match b {
            DocxBlock::Heading(level, t) => body.push_str(&format!(
                r#"<w:p><w:pPr><w:pStyle w:val="{level}"/></w:pPr><w:r><w:t>{}</w:t></w:r></w:p>"#,
                esc(t)
            )),
            DocxBlock::Para(t) => body.push_str(&format!(
                r#"<w:p><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#,
                esc(t)
            )),
            DocxBlock::Table(rows) => {
                body.push_str("<w:tbl>");
                for row in rows.iter() {
                    body.push_str("<w:tr>");
                    for cell in row.iter() {
                        body.push_str(&format!(
                            r#"<w:tc><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:tc>"#,
                            esc(cell)
                        ));
                    }
                    body.push_str("</w:tr>");
                }
                body.push_str("</w:tbl>");
            }
        }
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}</w:body></w:document>"#
    );
    // Localised Word templates use numeric style ids ("1" = heading 1).
    let styles = r#"<?xml version="1.0" encoding="UTF-8"?><w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:style w:type="paragraph" w:styleId="1"><w:name w:val="heading 1"/></w:style><w:style w:type="paragraph" w:styleId="2"><w:name w:val="heading 2"/></w:style></w:styles>"#;
    zip_of(&[
        ("[Content_Types].xml", r#"<?xml version="1.0"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#.to_string()),
        ("word/document.xml", document),
        ("word/styles.xml", styles.to_string()),
    ])
}

pub(crate) fn pptx(slides: &[(&str, Option<&str>)]) -> Vec<u8> {
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut ids = String::new();
    let mut rels = String::new();
    // Slides are registered in reverse part-number order to prove the
    // reader follows presentation.xml, not file names.
    for (i, (text, notes)) in slides.iter().enumerate() {
        let n = slides.len() - i;
        ids.push_str(&format!(r#"<p:sldId id="{}" r:id="rId{n}"/>"#, 256 + i));
        rels.push_str(&format!(
            r#"<Relationship Id="rId{n}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide{n}.xml"/>"#
        ));
        parts.push((
            format!("ppt/slides/slide{n}.xml"),
            format!(
                r#"<p:sld xmlns:p="p" xmlns:a="a"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>{}</a:t></a:r></a:p><a:p><a:fld type="slidenum"><a:t>{n}</a:t></a:fld></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#,
                esc(text)
            ),
        ));
        if let Some(note) = notes {
            parts.push((
                format!("ppt/slides/_rels/slide{n}.xml.rels"),
                format!(
                    r#"<Relationships xmlns="r"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide{}.xml"/></Relationships>"#,
                    n + 10
                ),
            ));
            parts.push((
                format!("ppt/notesSlides/notesSlide{}.xml", n + 10),
                format!(
                    r#"<p:notes xmlns:p="p" xmlns:a="a"><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:notes>"#,
                    esc(note)
                ),
            ));
        }
    }
    parts.push((
        "ppt/presentation.xml".into(),
        format!(r#"<p:presentation xmlns:p="p" xmlns:r="r"><p:sldIdLst>{ids}</p:sldIdLst></p:presentation>"#),
    ));
    parts.push((
        "ppt/_rels/presentation.xml.rels".into(),
        format!(r#"<Relationships xmlns="r">{rels}</Relationships>"#),
    ));
    let refs: Vec<(&str, String)> = parts.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    zip_of(&refs)
}

pub(crate) fn xlsx(sheets: &[(&str, &[&[&str]])]) -> Vec<u8> {
    let main = "http://schemas.openxmlformats.org/spreadsheetml/2006/main";
    let rel_ns = "http://schemas.openxmlformats.org/officeDocument/2006/relationships";
    let mut sheet_list = String::new();
    let mut rels = String::new();
    let mut overrides = String::new();
    let mut parts: Vec<(String, String)> = Vec::new();
    for (i, (name, rows)) in sheets.iter().enumerate() {
        let n = i + 1;
        sheet_list.push_str(&format!(r#"<sheet name="{}" sheetId="{n}" r:id="rId{n}"/>"#, esc(name)));
        rels.push_str(&format!(
            r#"<Relationship Id="rId{n}" Type="{rel_ns}/worksheet" Target="worksheets/sheet{n}.xml"/>"#
        ));
        overrides.push_str(&format!(
            r#"<Override PartName="/xl/worksheets/sheet{n}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>"#
        ));
        let mut data = String::new();
        for (r, row) in rows.iter().enumerate() {
            data.push_str(&format!(r#"<row r="{}">"#, r + 1));
            for (c, cell) in row.iter().enumerate() {
                let col = (b'A' + c as u8) as char;
                if cell.parse::<f64>().is_ok() {
                    data.push_str(&format!(r#"<c r="{col}{}"><v>{cell}</v></c>"#, r + 1));
                } else {
                    data.push_str(&format!(
                        r#"<c r="{col}{}" t="inlineStr"><is><t>{}</t></is></c>"#,
                        r + 1,
                        esc(cell)
                    ));
                }
            }
            data.push_str("</row>");
        }
        parts.push((
            format!("xl/worksheets/sheet{n}.xml"),
            format!(r#"<?xml version="1.0" encoding="UTF-8"?><worksheet xmlns="{main}"><sheetData>{data}</sheetData></worksheet>"#),
        ));
    }
    parts.push((
        "[Content_Types].xml".into(),
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>{overrides}</Types>"#
        ),
    ));
    parts.push((
        "_rels/.rels".into(),
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="{rel_ns}/officeDocument" Target="xl/workbook.xml"/></Relationships>"#),
    ));
    parts.push((
        "xl/workbook.xml".into(),
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><workbook xmlns="{main}" xmlns:r="{rel_ns}"><sheets>{sheet_list}</sheets></workbook>"#),
    ));
    parts.push((
        "xl/_rels/workbook.xml.rels".into(),
        format!(r#"<?xml version="1.0" encoding="UTF-8"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#),
    ));
    let refs: Vec<(&str, String)> = parts.iter().map(|(n, b)| (n.as_str(), b.clone())).collect();
    zip_of(&refs)
}

/// A PDF with one page per entry (ASCII text, Courier). An empty entry makes
/// a page with no text operators, like a scanned page.
pub(crate) fn pdf(pages: &[&str]) -> Vec<u8> {
    use pdf_extract::content::{Content, Operation};
    use pdf_extract::{Dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let mut font = Dictionary::new();
    font.set("Type", Object::Name(b"Font".to_vec()));
    font.set("Subtype", Object::Name(b"Type1".to_vec()));
    font.set("BaseFont", Object::Name(b"Courier".to_vec()));
    let font_id = doc.add_object(font);
    let mut fonts = Dictionary::new();
    fonts.set("F1", Object::Reference(font_id));
    let mut resources = Dictionary::new();
    resources.set("Font", Object::Dictionary(fonts));
    let resources_id = doc.add_object(resources);
    let mut kids = Vec::new();
    for text in pages {
        let ops = if text.is_empty() {
            Vec::new()
        } else {
            vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), Object::Integer(12)]),
                Operation::new("Td", vec![Object::Integer(72), Object::Integer(720)]),
                Operation::new("Tj", vec![Object::string_literal(text.as_bytes().to_vec())]),
                Operation::new("ET", vec![]),
            ]
        };
        let content = Content { operations: ops };
        let content_id = doc.add_object(Stream::new(Dictionary::new(), content.encode().unwrap()));
        let mut page = Dictionary::new();
        page.set("Type", Object::Name(b"Page".to_vec()));
        page.set("Parent", Object::Reference(pages_id));
        page.set("Contents", Object::Reference(content_id));
        kids.push(Object::Reference(doc.add_object(page)));
    }
    let mut pages_dict = Dictionary::new();
    pages_dict.set("Type", Object::Name(b"Pages".to_vec()));
    pages_dict.set("Count", Object::Integer(kids.len() as i64));
    pages_dict.set("Kids", Object::Array(kids));
    pages_dict.set("Resources", Object::Reference(resources_id));
    pages_dict.set(
        "MediaBox",
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(612),
            Object::Integer(792),
        ]),
    );
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));
    let mut catalog = Dictionary::new();
    catalog.set("Type", Object::Name(b"Catalog".to_vec()));
    catalog.set("Pages", Object::Reference(pages_id));
    let catalog_id = doc.add_object(catalog);
    doc.trailer.set("Root", Object::Reference(catalog_id));
    let mut out = Vec::new();
    doc.save_to(&mut out).unwrap();
    out
}
