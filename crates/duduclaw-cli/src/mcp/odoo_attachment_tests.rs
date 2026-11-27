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
    let png = b"\x89PNG\r\n\x1a\nexample";
    for (file, bytes, mime) in [
        ("x.pdf", PDF, "application/pdf"), ("x.PNG", png.as_slice(), "image/png"),
        ("x.jpg", b"\xff\xd8\xffexample".as_slice(), "image/jpeg"),
        ("x.jpeg", b"\xff\xd8\xffexample".as_slice(), "image/jpeg"),
        ("x.webp", b"RIFF\x14\0\0\0WEBPVP8 example".as_slice(), "image/webp"),
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
