use super::*;

const PDF: &[u8] = b"%PDF-1.7\nprivate test attachment\n%%EOF";

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
        std::fs::write(home.path().join(path), PDF).unwrap();
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
        let (bytes, mime) = read_file(home.path(), "bot", &attachment(path), 1).unwrap();
        assert_eq!(bytes, PDF);
        assert_eq!(mime, "application/pdf");
    }
    assert!(read_file(home.path(), "bot", &attachment("attachments/x.pdf"), 1).is_err());
    assert!(read_file(home.path(), "bot", &attachment("sub"), 1).is_err());
    let mut oversized = vec![b'x'; 1024 * 1024 + 1];
    oversized[..5].copy_from_slice(b"%PDF-");
    std::fs::write(home.path().join("agents/bot/attachments/large.pdf"), &oversized).unwrap();
    assert!(read_file(home.path(), "bot", &attachment("large.pdf"), 1).unwrap_err().contains("attachment_max_mb"));
    oversized.pop();
    let end = oversized.len();
    oversized[end - 5..].copy_from_slice(b"%%EOF");
    std::fs::write(home.path().join("agents/bot/attachments/large.pdf"), oversized).unwrap();
    assert!(read_file(home.path(), "bot", &attachment("large.pdf"), 1).is_ok());
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
    assert_eq!(read, PDF);
    std::fs::rename(&root, home.path().join("saved-attachments")).unwrap();
    symlink(home.path().join("saved-attachments"), &root).unwrap();
    assert!(read_file(home.path(), "bot", &attachment("old.pdf"), 1).is_err());
}

#[test]
fn extension_and_magic_must_agree_and_text_must_be_utf8_without_nul() {
    let png = valid_png();
    for (file, bytes, mime) in [
        ("x.pdf", PDF, "application/pdf"), ("x.PNG", png.as_slice(), "image/png"),
        ("x.jpg", b"\xff\xd8\xff\xda\x00\x02data\xff\xd9".as_slice(), "image/jpeg"),
        ("x.jpeg", b"\xff\xd8\xff\xda\x00\x02data\xff\xd9".as_slice(), "image/jpeg"),
        ("x.webp", b"RIFF\x0f\0\0\0WEBPVP8 example".as_slice(), "image/webp"),
        ("x.txt", "中文🙂".as_bytes(), "text/plain"), ("x.csv", b"a,b\n1,2".as_slice(), "text/csv"),
    ] { assert_eq!(mimetype(file, bytes).unwrap(), mime); }
    for (file, bytes) in [("x.pdf", png.as_slice()), ("x.png", PDF),
        ("x.svg", b"<svg/>".as_slice()), ("x.html", b"<html/>".as_slice()),
        ("x.htm", b"hello".as_slice()), ("x.xml", b"<x/>".as_slice()),
        ("x.exe", b"MZ".as_slice()), ("x.sh", b"#!/bin/sh".as_slice()),
        ("x.js", b"alert(1)".as_slice()), ("x.unknown", PDF),
        ("x.txt", b"hello\0world".as_slice()), ("x.csv", b"\xff".as_slice()),
        ("x.webp", b"RIFF\0\0\0\0WEBPnope".as_slice())] {
        assert!(mimetype(file, bytes).is_err(), "{file}");
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

fn png_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut chunk = (data.len() as u32).to_be_bytes().to_vec();
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(data);
    let mut crc = !0u32;
    for byte in &chunk[4..] {
        crc ^= u32::from(*byte);
        for _ in 0..8 { crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1))); }
    }
    chunk.extend_from_slice(&(!crc).to_be_bytes());
    chunk
}
fn valid_png() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend(png_chunk(b"IHDR", &[0,0,0,1,0,0,0,1,8,2,0,0,0]));
    bytes.extend(png_chunk(b"IDAT", &[0x78,0x9c,0x63,0,2,0,0,4,0,1]));
    bytes.extend(png_chunk(b"IEND", &[]));
    bytes
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
fn png_requires_chunk_walk_crc_and_iend_at_eof() {
    let png = valid_png();
    assert!(mimetype("x.png", &png).is_ok());
    let mut trailing = png.clone(); trailing.extend_from_slice(b"<script/>");
    let mut crc = png.clone(); crc[29] ^= 1;
    let mut length = png.clone(); length[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut iend = png.clone(); let last = iend.len() - 1; iend[last] ^= 1;
    for bad in [trailing, crc, length, iend, png[..png.len()-12].to_vec()] {
        assert!(mimetype("x.png", &bad).is_err());
    }
}

#[test]
fn jpeg_and_webp_refuse_truncation_and_appended_content() {
    let jpeg = b"\xff\xd8\xff\xda\x00\x02data\xff\xd9";
    assert!(mimetype("x.jpg", jpeg).is_ok());
    for bad in [jpeg[..jpeg.len()-2].to_vec(), [jpeg.as_slice(), b"payload"].concat(),
        [jpeg.as_slice(), b"payload\xff\xd9"].concat()] {
        assert!(mimetype("x.jpg", &bad).is_err());
    }
    let webp = b"RIFF\x0f\0\0\0WEBPVP8 example";
    assert!(mimetype("x.webp", webp).is_ok());
    for bad in [webp[..webp.len()-1].to_vec(), [webp.as_slice(), b"payload"].concat()] {
        assert!(mimetype("x.webp", &bad).is_err());
    }
}

#[test]
fn pdf_requires_eof_and_refuses_active_content() {
    assert!(mimetype("x.pdf", &[PDF, b" \r\n\t"].concat()).is_ok());
    for bad in [b"%PDF-1.7 no eof".to_vec(), [PDF, b"payload"].concat(),
        [PDF, &vec![b' '; 1024]].concat(), b"prefix%PDF-1.7\n%%EOF".to_vec()] {
        assert!(mimetype("x.pdf", &bad).is_err());
    }
    for token in ["/JavaScript", "/JS", "/Launch", "/EmbeddedFile", "/OpenAction", "/AA", "/RichMedia", "/XFA"] {
        assert!(mimetype("x.pdf", format!("%PDF-1.7\n{token}\n%%EOF").as_bytes()).is_err(), "{token}");
    }
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
