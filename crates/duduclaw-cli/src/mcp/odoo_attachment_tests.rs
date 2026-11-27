use super::*;

use lopdf::{dictionary, Document, Object};

fn pdf_document() -> Document {
    let mut doc = Document::with_version("1.7");
    let pages = doc.new_object_id();
    let page = doc.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages,
        "MediaBox" => vec![0.into(), 0.into(), 100.into(), 100.into()],
    });
    doc.objects.insert(pages, dictionary! {
        "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1,
    }.into());
    let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages });
    doc.trailer.set("Root", catalog);
    let info = doc.add_object(dictionary! { "Title" => Object::string_literal("private attachment") });
    doc.trailer.set("Info", info);
    doc.trailer.set("ID", vec![Object::string_literal("attachment-test-id"),
        Object::string_literal("attachment-test-id")]);
    doc
}

fn save_pdf(mut doc: Document) -> Vec<u8> {
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

pub(in crate::mcp) fn clean_pdf() -> Vec<u8> { save_pdf(pdf_document()) }

pub(in crate::mcp) fn image_bytes(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let pixels = image::DynamicImage::new_rgb8(width, height);
    let mut out = Cursor::new(Vec::new());
    pixels.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn valid_png() -> Vec<u8> { image_bytes(image::ImageFormat::Png, 2, 2) }

// Build correct offsets around verbatim PDF names; serializing an escaped name
// through lopdf would escape the '#' again and stop testing the attack.
fn raw_object_pdf(object: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut bytes = b"%PDF-1.7\n".to_vec();
    let mut offsets = vec![0];
    for (index, body) in [b"<< /Type /Catalog /Pages 2 0 R >>".as_slice(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>", object].iter().enumerate() {
        offsets.push(bytes.len());
        writeln!(bytes, "{} 0 obj", index + 1).unwrap();
        bytes.extend_from_slice(body);
        bytes.extend_from_slice(b"\nendobj\n");
    }
    let start = bytes.len();
    bytes.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
    for offset in offsets.into_iter().skip(1) { writeln!(bytes, "{offset:010} 00000 n ").unwrap(); }
    write!(bytes, "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{start}\n%%EOF").unwrap();
    bytes
}

fn attachment(file: &str) -> Attachment {
    parse(&json!({"model":"crm.lead","res_id":9,"file":file})).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("agents/bot/attachments/sub")).unwrap();
    std::fs::create_dir_all(home.path().join("agents/other/attachments")).unwrap();
    std::fs::create_dir_all(home.path().join("attachments")).unwrap();
    for path in ["agents/bot/attachments/x.pdf", "agents/bot/attachments/sub/x.pdf",
        "agents/other/attachments/x.pdf", "attachments/x.pdf"] {
        std::fs::write(home.path().join(path), clean_pdf()).unwrap();
    }
    home
}

#[test]
fn closed_arguments_paths_and_display_names() {
    assert_eq!(attachment("sub/x.pdf").name, "x.pdf");
    for path in ["", "/tmp/x.pdf", "../other/attachments/x.pdf", "../../../attachments/x.pdf",
        "sub/../x.pdf", "sub//x.pdf", "./x.pdf", "x.pdf\0", "C:\\x.pdf", "x.pdf:stream", "sub\\x.pdf"] {
        assert!(parse(&json!({"model":"crm.lead","res_id":9,"file":path})).is_err(), "{path:?}");
    }
    for field in ["content", "datas", "mimetype", "agent", "home", "public", "access_token"] {
        let mut params = json!({"model":"crm.lead","res_id":9,"file":"x.pdf"});
        params[field] = json!("untrusted");
        assert!(parse(&params).is_err(), "{field}");
    }
    for name in ["".to_string(), "../x.pdf".into(), "sub\\x.pdf".into(), "x\n.pdf".into(), "🙂".repeat(256)] {
        assert!(parse(&json!({"model":"crm.lead","res_id":9,"file":"x.pdf","name":name})).is_err());
    }
    assert!(parse(&json!({"model":"crm.lead","res_id":9,"file":"x.pdf","name":"中".repeat(255)})).is_ok());
    for id in [json!(0), json!(-1), json!(1.5), json!("9")] {
        assert!(parse(&json!({"model":"crm.lead","res_id":id,"file":"x.pdf"})).is_err());
    }
}

#[test]
fn only_own_regular_files_and_bounded_size() {
    let home = fixture();
    for path in ["x.pdf", "sub/x.pdf"] {
        let (bytes, mime, name) = read_file(home.path(), "bot", &attachment(path), 1).unwrap();
        assert_eq!(bytes, clean_pdf());
        assert_eq!(name, "x.pdf");
        assert_eq!(mime, "application/pdf");
    }
    assert!(read_file(home.path(), "bot", &attachment("attachments/x.pdf"), 1).is_err());
    assert!(read_file(home.path(), "bot", &attachment("sub"), 1).is_err());
    let mut oversized = vec![b'x'; 1024 * 1024 + 1];
    oversized[..5].copy_from_slice(b"%PDF-");
    std::fs::write(home.path().join("agents/bot/attachments/large.pdf"), &oversized).unwrap();
    assert!(read_file(home.path(), "bot", &attachment("large.pdf"), 1).unwrap_err().contains("attachment_max_mb"));
    // Exact input-size boundary uses an allowed text file, not a fake PDF.
    std::fs::write(home.path().join("agents/bot/attachments/exact.txt"), vec![b'x'; 1024 * 1024]).unwrap();
    assert!(read_file(home.path(), "bot", &attachment("exact.txt"), 1).is_ok());
    for limit in [0, 26] { assert!(read_file(home.path(), "bot", &attachment("x.pdf"), limit).is_err()); }
}

#[cfg(unix)]
#[test]
fn no_symlink_component_or_root_and_open_handle_survives_path_replacement() {
    use std::os::unix::fs::symlink;
    let home = fixture();
    let root = home.path().join("agents/bot/attachments");
    symlink(home.path().join("attachments/x.pdf"), root.join("link.pdf")).unwrap();
    symlink(home.path().join("agents/other/attachments"), root.join("linked-dir")).unwrap();
    symlink(root.join("x.pdf"), root.join("internal.pdf")).unwrap();
    for path in ["link.pdf", "linked-dir/x.pdf", "internal.pdf"] {
        assert!(read_file(home.path(), "bot", &attachment(path), 1).is_err());
    }
    // A replacement after open cannot redirect the bytes read from the fd.
    let mut opened = open_file(home.path(), "bot", "x.pdf").unwrap();
    std::fs::rename(root.join("x.pdf"), root.join("old.pdf")).unwrap();
    symlink(home.path().join("attachments/x.pdf"), root.join("x.pdf")).unwrap();
    std::fs::write(home.path().join("attachments/x.pdf"), b"outside secret").unwrap();
    let mut read = Vec::new();
    opened.file.read_to_end(&mut read).unwrap();
    assert_eq!(read, clean_pdf());
    std::fs::rename(&root, home.path().join("saved-attachments")).unwrap();
    symlink(home.path().join("saved-attachments"), &root).unwrap();
    assert!(read_file(home.path(), "bot", &attachment("old.pdf"), 1).is_err());
}

#[test]
fn extension_and_magic_must_agree_and_text_must_be_utf8_without_nul() {
    let png = valid_png();
    let pdf = clean_pdf();
    for (file, bytes, mime) in [
        ("x.pdf", pdf.as_slice(), "application/pdf"),
        ("x.txt", "中文🙂".as_bytes(), "text/plain"), ("x.csv", b"a,b\n1,2".as_slice(), "text/csv"),
    ] { assert_eq!(mimetype(file, bytes).unwrap(), mime); }
    for (file, bytes) in [("x.pdf", png.as_slice()), ("x.png", pdf.as_slice()),
        ("x.svg", b"<svg/>".as_slice()), ("x.html", b"<html/>".as_slice()),
        ("x.htm", b"hello".as_slice()), ("x.xml", b"<x/>".as_slice()),
        ("x.exe", b"MZ".as_slice()), ("x.sh", b"#!/bin/sh".as_slice()),
        ("x.js", b"alert(1)".as_slice()), ("x.unknown", pdf.as_slice()),
        ("x.txt", b"hello\0world".as_slice()), ("x.csv", b"\xff".as_slice()),
        ("x.webp", b"RIFF\0\0\0\0WEBPnope".as_slice())] {
        assert!(prepare_file(&attachment(file), bytes.to_vec(), 1024 * 1024).is_err(), "{file}");
    }
}

fn office_zip(parts: &[&str]) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for part in parts {
        writer.start_file(*part, zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(b"<?xml version=\"1.0\"?><test/>").unwrap();
    }
    writer.finish().unwrap().into_inner()
}

#[test]
fn office_requires_real_zip_content_types_and_matching_part() {
    for (ext, part) in [("xlsx", "xl/workbook.xml"), ("docx", "word/document.xml"), ("pptx", "ppt/presentation.xml")] {
        let file = format!("x.{ext}");
        assert!(mimetype(&file, &office_zip(&["[Content_Types].xml", part])).is_ok());
        assert!(mimetype(&file, &office_zip(&[part])).is_err());
        assert!(mimetype(&file, &office_zip(&["[Content_Types].xml", "unrelated.xml"])).is_err());
        assert!(mimetype(&file, &office_zip(&["[Content_Types].xml", part, "xl/vbaProject.bin"])).is_err());
        assert!(mimetype(&file, b"PK\x03\x04not a zip").is_err());
    }
}

#[test]
fn display_extension_must_match_validated_format() {
    let home = fixture();
    for name in ["invoice.html", "invoice.pdf.exe", "invoice", "invoice.pdf ", "invoice.png"] {
        let mut call = attachment("x.pdf"); call.name = name.into();
        assert!(read_file(home.path(), "bot", &call, 1).is_err(), "{name}");
    }
    let mut call = attachment("x.pdf"); call.name = "發票.PDF".into();
    assert!(read_file(home.path(), "bot", &call, 1).is_ok());
    assert!(display_name_matches("photo.jpeg", "image/jpeg"));
    assert!(!display_name_matches("photo.png", "image/jpeg"));
}

#[test]
fn images_are_decoded_and_only_reencoded_pixels_are_returned() {
    let home = fixture();
    for (file, format, mime, name) in [
        ("x.PNG", image::ImageFormat::Png, "image/png", "x.PNG"),
        ("x.jpg", image::ImageFormat::Jpeg, "image/jpeg", "x.jpg"),
        ("x.jpeg", image::ImageFormat::Jpeg, "image/jpeg", "x.jpeg"),
        ("x.webp", image::ImageFormat::WebP, "image/png", "x.png"),
    ] {
        let original = image_bytes(format, 2, 2);
        let mut input = original.clone();
        if format != image::ImageFormat::WebP { input.extend_from_slice(b"private-trailing-payload"); }
        std::fs::write(home.path().join("agents/bot/attachments").join(file), &input).unwrap();
        let (bytes, actual_mime, actual_name) = read_file(home.path(), "bot", &attachment(file), 1).unwrap();
        assert_eq!(actual_mime, mime);
        assert_eq!(actual_name, name);
        assert_eq!(bytes, reencode_image(&original, format, 1024 * 1024).unwrap());
        assert!(!bytes.windows(b"private-trailing-payload".len()).any(|w| w == b"private-trailing-payload"));
        let output_format = if format == image::ImageFormat::WebP { image::ImageFormat::Png } else { format };
        assert_eq!(image::guess_format(&bytes).unwrap(), output_format);
        assert!(image::load_from_memory(&bytes).is_ok());
    }
    let mut call = attachment("x.webp");
    call.name = "客戶圖片.WEBP".into();
    assert_eq!(read_file(home.path(), "bot", &call, 1).unwrap().2, "客戶圖片.png");
    call.name = "客戶圖片.html".into();
    assert!(read_file(home.path(), "bot", &call, 1).is_err());
}

#[test]
fn shallow_jpeg_webp_and_wrong_image_formats_are_refused() {
    for (file, bytes) in [
        ("x.jpg", b"\xff\xd8\xff\xda\x00\x02data\xff\xd9".as_slice()),
        ("x.webp", b"RIFF\x0f\0\0\0WEBPVP8 example".as_slice()),
        ("x.png", b"\x89PNG\r\n\x1a\n".as_slice()),
    ] { assert!(prepare_file(&attachment(file), bytes.to_vec(), 1024 * 1024).is_err()); }
    let png = valid_png();
    assert!(prepare_file(&attachment("x.jpg"), png.clone(), 1024 * 1024).is_err());
    assert!(prepare_file(&attachment("x.webp"), png, 1024 * 1024).is_err());
    let jpeg = image_bytes(image::ImageFormat::Jpeg, 2, 2);
    assert!(prepare_file(&attachment("x.png"), jpeg, 1024 * 1024).is_err());
}

#[test]
fn image_dimensions_and_both_input_and_output_sizes_are_bounded() {
    for (width, height) in [(IMAGE_MAX_DIMENSION + 1, 1), (1, IMAGE_MAX_DIMENSION + 1)] {
        let png = image_bytes(image::ImageFormat::Png, width, height);
        assert!(prepare_file(&attachment("x.png"), png, 1024 * 1024).is_err());
    }
    let png = valid_png();
    assert!(prepare_file(&attachment("x.png"), png.clone(), png.len() as u64 - 1).is_err());
    let webp = image_bytes(image::ImageFormat::WebP, 2, 2);
    let (output, _, _) = prepare_file(&attachment("x.webp"), webp.clone(), 1024 * 1024).unwrap();
    assert!(output.len() > webp.len());
    assert!(prepare_file(&attachment("x.webp"), webp.clone(), webp.len() as u64).is_err());
}

#[test]
fn pdf_requires_real_parse_header_eof_and_no_active_names_anywhere() {
    let pdf = clean_pdf();
    assert!(mimetype("x.pdf", &pdf).is_ok());
    assert!(mimetype("x.pdf", &[pdf.as_slice(), b" \r\n\t"].concat()).is_ok());
    for bad in [b"%PDF-1.7 no eof".to_vec(), [pdf.as_slice(), b"payload"].concat(),
        [pdf.as_slice(), &vec![b' '; 1024]].concat(), b"prefix%PDF-1.7\n%%EOF".to_vec(),
        b"%PDF-1.7\nnot a document\n%%EOF".to_vec(), raw_object_pdf(b"<< /Broken [ >>")] {
        assert!(mimetype("x.pdf", &bad).is_err());
    }
    for token in ["JavaScript", "JS", "Launch", "EmbeddedFile", "EmbeddedFiles", "OpenAction", "AA",
        "RichMedia", "XFA", "SubmitForm", "ImportData", "GoToR", "GoToE", "Sound", "Movie",
        "Java#53cript", "Open#41ction", "javascript", "jS"] {
        for object in [format!("<< /{token} null >>"), format!("[<< /Nested [/{token}] >>]"),
            format!("<< /Length 0 /Nested [/{token}] >>\nstream\n\nendstream")] {
            let bytes = raw_object_pdf(object.as_bytes());
            assert!(Document::load_mem(&bytes).unwrap().objects.contains_key(&(4, 0)), "{token}");
            assert!(!pdf_valid(&bytes), "{token}");
        }
    }
    // Whole names only; text strings and longer names are not PDF actions.
    assert!(pdf_valid(&raw_object_pdf(b"<< /JavaScriptNotes (/JavaScript) >>")));
}

#[test]
fn pdf_walks_compressed_object_streams_and_refuses_encryption() {
    for active in [false, true] {
        let mut doc = pdf_document();
        doc.add_object(dictionary! { "Nested" => vec![Object::Name(
            if active { b"JavaScript".to_vec() } else { b"SafeName".to_vec() })] });
        let mut bytes = Vec::new();
        doc.save_modern(&mut bytes).unwrap();
        let loaded = Document::load_mem(&bytes).unwrap();
        assert!(loaded.reference_table.entries.values().any(|e|
            matches!(e, lopdf::xref::XrefEntry::Compressed { .. })));
        assert!(!bytes.windows(b"/JavaScript".len()).any(|w| w == b"/JavaScript"));
        assert_eq!(pdf_valid(&bytes), !active);
    }
    for password in ["", "secret"] {
        let mut doc = pdf_document();
        let state = lopdf::EncryptionState::try_from(lopdf::EncryptionVersion::V1 {
            document: &doc, owner_password: "owner", user_password: password,
            permissions: lopdf::Permissions::PRINTABLE,
        }).unwrap();
        doc.encrypt(&state).unwrap();
        let bytes = save_pdf(doc);
        assert!(!pdf_valid(&bytes));
    }
}

#[test]
fn pdf_refuses_incomplete_object_streams_and_names_in_original_stream_dictionaries() {
    let mut objects = lopdf::ObjectStream::builder().build();
    objects.add_object((5, 0), dictionary! { "Label" => "SafeName" }.into()).unwrap();
    let stream = objects.to_stream_object().unwrap();
    let first = stream.dict.get(b"First").unwrap().as_i64().unwrap();
    for (count, extra, valid) in [(1, "", true), (2, "", false),
        (1, "/DecodeParms << /Java#53cript null >>", false)] {
        let mut body = format!("<< /Type /ObjStm /N {count} /First {first} /Filter /FlateDecode /Length {} {extra} >>\nstream\n",
            stream.content.len()).into_bytes();
        body.extend_from_slice(&stream.content);
        body.extend_from_slice(b"\nendstream");
        assert_eq!(pdf_valid(&raw_object_pdf(&body)), valid, "{count}, {extra}");
    }
    let malformed = raw_object_pdf(b"<< /Type /ObjStm /N 1 /First 4 /Filter /FlateDecode /Length 4 >>\nstream\nnope\nendstream");
    assert!(!pdf_valid(&malformed));
}

#[test]
fn office_refuses_external_relationships_in_any_part() {
    use std::io::Write;
    for mode in [r#"TargetMode="External""#, "TargetMode = 'External'", r#"TargetMode="Ext&#101;rnal""#] {
        for rel in ["_rels/.rels", "word/_rels/document.xml.rels"] {
            let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
            for part in ["[Content_Types].xml", "word/document.xml", rel] {
                writer.start_file(part, zip::write::SimpleFileOptions::default()).unwrap();
                let xml = if part == rel { format!("<Relationships><Relationship {mode} Target='https://example.com'/></Relationships>") }
                    else { "<test/>".into() };
                writer.write_all(xml.as_bytes()).unwrap();
            }
            assert!(mimetype("x.docx", &writer.finish().unwrap().into_inner()).is_err(), "{rel}: {mode}");
        }
    }
}
