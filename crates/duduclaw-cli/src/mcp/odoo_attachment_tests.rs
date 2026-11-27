use super::*;

pub(in crate::mcp) fn clean_pdf() -> Vec<u8> {
    raw_object_pdf(b"<< /Title (private attachment) >>")
}

pub(in crate::mcp) fn image_bytes(format: image::ImageFormat, width: u32, height: u32) -> Vec<u8> {
    let pixels = image::DynamicImage::new_rgb8(width, height);
    let mut out = Cursor::new(Vec::new());
    pixels.write_to(&mut out, format).unwrap();
    out.into_inner()
}

fn valid_png() -> Vec<u8> { image_bytes(image::ImageFormat::Png, 2, 2) }

// Build correct offsets around verbatim PDF names, including #xx escapes.
pub(in crate::mcp) fn raw_object_pdf(object: &[u8]) -> Vec<u8> {
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

fn deflate(bytes: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

fn stream_object(filter: &str, data: &[u8], length: usize) -> Vec<u8> {
    let mut object = format!("<< /Type /ObjStm /N 1 /First 4 /Length {length} {filter} >>\nstream\n").into_bytes();
    object.extend_from_slice(data);
    object.extend_from_slice(b"\nendstream");
    object
}

fn stream_pdf(filter: &str, data: &[u8]) -> Vec<u8> {
    raw_object_pdf(&stream_object(filter, data, data.len()))
}

#[test]
fn pdf_requires_header_eof_and_no_active_names_in_original_bytes() {
    let pdf = clean_pdf();
    assert!(mimetype("x.pdf", &pdf).is_ok());
    assert!(mimetype("x.pdf", &[pdf.as_slice(), b" \r\n\t"].concat()).is_ok());
    assert!(pdf_valid(&[b"%PDF-2.0\n".as_slice(), &pdf].concat()));
    for bad in [b"%PDF-1.7 no eof".to_vec(), [pdf.as_slice(), b"payload"].concat(),
        [pdf.as_slice(), &vec![b' '; 1024]].concat(), b"prefix%PDF-1.7\n%%EOF".to_vec(),
        b"%PDF-3.0\n<<>>\n%%EOF".to_vec(), b"%PDF-1.7\nnot a document\n%%EOF".to_vec(),
        raw_object_pdf(b"<< /Broken [ >>")] {
        assert!(mimetype("x.pdf", &bad).is_err());
    }
    for token in ["JavaScript", "JS", "Launch", "EmbeddedFile", "EmbeddedFiles", "OpenAction", "AA",
        "RichMedia", "XFA", "SubmitForm", "ImportData", "GoToR", "GoToE", "Sound", "Movie", "Encrypt",
        "Java#53cript", "Open#41ction", "javascript", "jS", "Bad#zz", "Bad#", "Bad#0"] {
        for object in [format!("<< /{token} null >>"), format!("[<< /Nested [/{token}] >>]"),
            format!("<< /Length 0 /Nested [/{token}] >>\nstream\n\nendstream")] {
            assert!(!pdf_valid(&raw_object_pdf(object.as_bytes())), "{token}");
        }
    }
    // The conservative whole-buffer pass includes strings, comments and objects
    // absent from the xref table, rather than relying on one parser's object map.
    assert!(pdf_valid(&raw_object_pdf(b"<< /JavaScriptNotes (/SafeName) >>")));
    assert!(!pdf_valid(&raw_object_pdf(b"<< /Label (/JavaScript) >>")));
    assert!(!pdf_valid(&[pdf.as_slice(), b"\n% /OpenAction\n%%EOF"].concat()));
    let incremental = [pdf.as_slice(),
        b"\n99 0 obj\n<< /OpenAction 100 0 R >>\nendobj\ntrailer\n<< /Prev 9 >>\n%%EOF"].concat();
    assert!(!pdf_valid(&incremental));
    // Even an unindexed object in the original revision is scanned.
    assert!(!pdf_valid(&raw_object_pdf(b"<<>>\nendobj\n99 0 obj\n<< /Open#41ction null >>")));
}

#[test]
fn pdf_scans_flate_object_streams_and_only_accepts_supported_filters() {
    let plain = b"5 0 << /Label /SafeName >>";
    let compressed = deflate(plain);
    for filter in ["/Filter /FlateDecode", "/Filter [/FlateDecode]", "/Filter /Fl",
        "/Filter [/Fl]", "/F#69lter /Flate#44ecode",
        "/Filter /FlateDecode /DecodeParms << /Predictor 15 /Columns 1 >>"] {
        assert!(pdf_valid(&stream_pdf(filter, &compressed)), "{filter}");
    }
    let twice = deflate(&compressed);
    assert!(pdf_valid(&stream_pdf("/Filter [/FlateDecode /Fl]", &twice)));
    for payload in [b"5 0 << /JS (alert) >>".as_slice(), b"5 0 << /Java#53cript null >>",
        b"5 0 << /Open#41ction null >>", b"5 0 << /Encrypt null >>", b"5 0 << /Bad#zz null >>"] {
        let compressed = deflate(payload);
        assert!(!compressed.windows(3).any(|w| w == b"/JS"));
        assert!(!pdf_valid(&stream_pdf("/Filter /FlateDecode", &compressed)));
    }
    for filter in ["/Filter /LZWDecode", "/Filter /ASCII85Decode", "/Filter /ASCIIHexDecode",
        "/Filter /RunLengthDecode", "/Filter /Crypt", "/Filter /Unknown",
        "/Filter [/FlateDecode /DCTDecode]", "/Filter [/DCTDecode /Fl]",
        "/Filter [/DCTDecode /JPXDecode]", "/Filter 8 0 R", "/Filter []",
        "/Filter /Fl /Filter /DCTDecode"] {
        assert!(!pdf_valid(&stream_pdf(filter, &compressed)), "{filter}");
    }
    for filter in ["/Filter /DCTDecode", "/Filter [/JPXDecode]",
        "/Filter /CCITTFaxDecode", "/Filter /JBIG2Decode"] {
        assert!(pdf_valid(&stream_pdf(filter, b"\xff\xd8image\xff\xd9")), "{filter}");
    }
    assert!(pdf_valid(&stream_pdf("", plain)));
    assert!(!pdf_valid(&stream_pdf("", b"/JS")));
    assert!(!pdf_valid(&stream_pdf("/Filter /Fl", b"invalid zlib")));
    let nested = stream_object("/Filter /FlateDecode", &compressed, compressed.len());
    assert!(!pdf_valid(&stream_pdf("/Filter /Fl", &deflate(&nested))));
}

#[test]
fn pdf_refuses_xref_addressable_flate_streams_inside_raw_streams() {
    use std::io::Write;
    let compressed = deflate(b"6 0 << /OpenAction null >>");
    let mut nested = b"5 0 obj\n".to_vec();
    nested.extend_from_slice(&stream_object("/Filter /FlateDecode", &compressed, compressed.len()));
    nested.extend_from_slice(b"\nendobj\n");
    for filter in ["", "/Filter /DCTDecode", "/Filter /JPXDecode",
        "/Filter /CCITTFaxDecode", "/Filter /JBIG2Decode"] {
        // The outer Length covers the entire inner object, so its endstream
        // cannot accidentally terminate the outer stream during the scan.
        let mut pdf = stream_pdf(filter, &nested);
        let inner_offset = pdf.windows(8).position(|w| w == b"5 0 obj\n").unwrap();
        let previous_xref = pdf.windows(5).position(|w| w == b"xref\n").unwrap();
        pdf.push(b'\n');
        let xref_offset = pdf.len();
        write!(pdf, "xref\n5 1\n{inner_offset:010} 00000 n \ntrailer\n<< /Size 7 /Root 1 0 R /Prev {previous_xref} >>\nstartxref\n{xref_offset}\n%%EOF").unwrap();
        // The active name is compressed; refusal must come from stream syntax.
        assert!(pdf_names_valid(&pdf, &mut 0), "{filter}");
        assert!(pdf_streams_valid(&pdf, &mut 0).is_none(), "{filter}");
        assert!(!pdf_valid(&pdf), "{filter}");
    }
}

#[test]
fn pdf_nested_stream_check_requires_delimiters_and_eol() {
    for data in [b"stream\n".as_slice(), b"stream\r", b"stream\r\n",
        b">>stream \t\0\x0c\r\n", b"image\nstream\nmore image"] {
        assert!(pdf_has_nested_stream(data), "{data:?}");
        for filter in ["", "/Filter /DCTDecode", "/Filter /JPXDecode",
            "/Filter /CCITTFaxDecode", "/Filter /JBIG2Decode"] {
            assert!(!pdf_valid(&stream_pdf(filter, data)), "{filter}: {data:?}");
        }
        assert!(!pdf_valid(&stream_pdf("/Filter /Fl", &deflate(data))));
    }
    for data in [b"".as_slice(), b"s", b"strea", b"stream", b"stream ",
        b"endstream\n", b"upstream\n", b"streaming\n", b"stream /Safe"] {
        assert!(!pdf_has_nested_stream(data), "{data:?}");
    }
}

#[test]
fn pdf_refuses_nested_stream_syntax_in_raw_flate_bytes() {
    use std::io::Write;
    // Stored DEFLATE blocks retain the syntax in the compressed representation.
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::none());
    encoder.write_all(b"\nstream\n").unwrap();
    let compressed = encoder.finish().unwrap();
    assert!(pdf_has_nested_stream(&compressed));
    assert!(pdf_streams_valid(&stream_pdf("/Filter /FlateDecode", &compressed), &mut 0).is_none());
}

#[test]
fn pdf_refuses_duplicate_lengths_even_if_the_first_is_invalid() {
    for keys in ["/Length 0 /Length 0", "/Length 0 /L#65ngth 0",
        "/Length 8 0 R /Length 0", "/Length null /Length 0",
        "/Length -1 /Length 0", "/Length 184467440737095516160 /Length 0"] {
        let object = format!("<< {keys} >>\nstream\n\nendstream");
        assert!(!pdf_valid(&raw_object_pdf(object.as_bytes())), "{keys}");
    }
}

#[test]
fn pdf_stream_bounds_ignore_inconsistent_lengths_and_refuse_bombs() {
    let plain = b"5 0 << /Label /SafeName >>";
    let compressed = deflate(plain);
    for length in [0, 1, compressed.len(), compressed.len() + 20, usize::MAX] {
        assert!(pdf_valid(&raw_object_pdf(&stream_object("/Filter /Fl", &compressed, length))));
    }
    // A consistent length permits delimiter-looking bytes inside image data.
    assert!(pdf_valid(&stream_pdf("/Filter /DCTDecode", b"image\nendstream\nmore image")));
    assert!(!pdf_valid(&raw_object_pdf(b"<< /Filter /Fl >>\nstream\nno end marker")));
    assert!(!pdf_valid(&raw_object_pdf(b"<< /Filter /Fl >> stream no EOL endstream")));
    assert_eq!(pdf_inflate(&compressed, plain.len() + 1).unwrap(), plain);
    assert!(pdf_inflate(&compressed, plain.len()).is_none());
    let bomb = deflate(&vec![0; 1024 * 1024]);
    let cap = bomb.len() * 10;
    assert!(cap < 1024 * 1024);
    // The decoder reads at most cap + 1 bytes; output reserves only cap bytes.
    assert!(pdf_inflate(&bomb, cap).is_none());
    assert!(!pdf_valid(&stream_pdf("/Filter /FlateDecode", &bomb)));
    let mut trailing = compressed.clone();
    trailing.extend_from_slice(b"hidden");
    assert!(pdf_inflate(&trailing, 1024).is_none());
}

#[test]
fn pdf_name_stream_and_depth_counts_are_bounded() {
    let names = b"/Safe ".repeat(PDF_MAX_NAMES);
    let mut count = 0;
    assert!(pdf_names_valid(&names, &mut count));
    assert!(!pdf_names_valid(b"/OneMore", &mut count));
    // Inflated names share the original buffer's budget.
    let mut count = PDF_MAX_NAMES;
    let compressed = deflate(b"5 0 << /Safe null >>");
    assert!(pdf_streams_valid(&stream_pdf("/Filter /Fl", &compressed), &mut count).is_none());
    let object = b"<< /Length 0 >>\nstream\n\nendstream\n";
    assert!(pdf_valid(&raw_object_pdf(&object.repeat(PDF_MAX_STREAMS))));
    assert!(!pdf_valid(&raw_object_pdf(&object.repeat(PDF_MAX_STREAMS + 1))));
    let deep = [b"[".repeat(PDF_MAX_DEPTH + 1), b"]".repeat(PDF_MAX_DEPTH + 1)].concat();
    assert!(!pdf_valid(&raw_object_pdf(&deep)));
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

#[cfg(unix)]
#[tokio::test]
async fn protected_attachment_uses_verified_employee_host_root() {
    use duduclaw_isolation::{bounded::{List, Text}, capability::{CapabilityId, CapabilityRecord,
        EmployeeBinding, Lane, Principal}, ids::{EmployeeId, RunId, Sha256Digest}};
    use duduclaw_mcp_boundary::{RequestContext, resources::ResourceRoots};
    use crate::mcp_isolation::{IsolatedCall, ISOLATED_CALL};
    let home = fixture();
    std::fs::write(home.path().join("config.toml"),
        "[odoo]\nurl='https://odoo.example.com'\ndb='test'\n").unwrap();
    std::fs::write(home.path().join("agents/bot/agent.toml"),
        "[odoo]\nusername='bot'\napi_key_enc='private-key'\nallowed_models=['crm.lead']\nallowed_actions=['attach:crm.lead']\n").unwrap();
    let record = CapabilityRecord {
        id: CapabilityId(Sha256Digest([1; 32])), employee: EmployeeId([2; 16]), run: RunId([3; 16]),
        task: None, principal: Principal::Human { user_id: Text::new("owner".into()).unwrap(), generation: 1 },
        employee_binding: EmployeeBinding::Personal(Text::new("owner".into()).unwrap()), lane: Lane::Standard,
        tools: List::new(vec![Text::new("odoo_attachment_add".into()).unwrap()]).unwrap(),
        resources: List::new(vec![]).unwrap(), policy_revision: 1, gateway_epoch: [4; 16],
        issued_at: 1, not_before: 1, expires_at: u64::MAX, nonce: [5; 32], jti: [6; 16],
    };
    let isolated = Arc::new(IsolatedCall { home: home.path().to_path_buf(),
        context: RequestContext::from_verified_record(record), agent: "bot".into(),
        roots: Arc::new(ResourceRoots::default()), parser: None, services: None,
        worker: None, execution: None, forks: None, record_resolver: None });
    ISOLATED_CALL.scope(isolated, async {
        let args = json!({"model":"crm.lead","res_id":9,"file":"x.pdf"});
        assert!(validate_attachment(home.path(), "bot", &args).await.is_ok());
        assert!(validate_attachment(home.path(), "other", &args).await.is_err());
        assert!(!verified_employee(&home.path().join("other-home"), "bot"));
        let expected = clean_pdf();
        std::fs::write(home.path().join("attachments/x.pdf"), b"host secret").unwrap();
        std::fs::write(home.path().join("agents/other/attachments/x.pdf"), b"other employee secret").unwrap();
        assert_eq!(read_file(home.path(), "bot", &attachment("x.pdf"), 1).unwrap().0, expected);
        for file in ["../../other/attachments/x.pdf", "../../../attachments/x.pdf"] {
            let call = Attachment { file: file.into(), ..attachment("x.pdf") };
            assert!(read_file(home.path(), "bot", &call, 1).is_err());
        }
        let root = home.path().join("agents/bot/attachments");
        std::os::unix::fs::symlink(home.path().join("agents/other/attachments/x.pdf"), root.join("link.pdf")).unwrap();
        assert!(read_file(home.path(), "bot", &attachment("link.pdf"), 1).is_err());
        std::fs::rename(&root, home.path().join("saved-attachments")).unwrap();
        std::os::unix::fs::symlink(home.path().join("saved-attachments"), &root).unwrap();
        assert!(read_file(home.path(), "bot", &attachment("x.pdf"), 1).is_err());
    }).await;
}
