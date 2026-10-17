//! Preserve XLSX name-cache behavior while propagating ZIP 9 decoding errors.

use std::io::{Cursor, Read, Write};

use calamine::{Data, DataType, Reader, Xlsb, XlsbError, Xlsx, XlsxError};
use zip::{write::SimpleFileOptions, CompressionMethod, ZipArchive, ZipWriter};

fn workbook_bytes() -> Vec<u8> {
    let mut workbook = rust_xlsxwriter::Workbook::new();
    let sheet = workbook.add_worksheet();
    sheet.write_string(0, 0, "繁體中文").unwrap();
    sheet.write_number(1, 0, 42).unwrap();
    workbook.save_to_buffer().unwrap()
}

fn assert_workbook_values(bytes: Vec<u8>) {
    let mut workbook = Xlsx::new(Cursor::new(bytes)).unwrap();
    let sheet = workbook.worksheet_range_at(0).unwrap().unwrap();
    assert_eq!(
        sheet.get_value((0, 0)),
        Some(&Data::String("繁體中文".into()))
    );
    assert_eq!(
        sheet.get_value((1, 0)).and_then(DataType::as_f64),
        Some(42.0)
    );
}

#[test]
fn native_writer_workbook_remains_readable() {
    assert_workbook_values(workbook_bytes());
}

#[test]
fn cached_zip_paths_preserve_case_and_backslash_normalization() {
    let mut input = ZipArchive::new(Cursor::new(workbook_bytes())).unwrap();
    let mut output = ZipWriter::new(Cursor::new(Vec::new()));
    for index in 0..input.len() {
        let mut entry = input.by_index(index).unwrap();
        let name = entry
            .name()
            .unwrap()
            .replace('/', "\\")
            .to_ascii_uppercase();
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        output
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(CompressionMethod::Stored),
            )
            .unwrap();
        output.write_all(&body).unwrap();
    }
    assert_workbook_values(output.finish().unwrap().into_inner());
}

#[test]
fn malformed_zip_is_reported_through_existing_xlsx_zip_error() {
    let mut malformed = workbook_bytes();
    malformed.truncate(16);
    assert!(matches!(
        Xlsx::new(Cursor::new(malformed)),
        Err(XlsxError::Zip(_))
    ));
}

#[test]
fn malformed_zip_is_reported_through_existing_xlsb_zip_error() {
    let mut malformed = workbook_bytes();
    malformed.truncate(16);
    assert!(matches!(
        Xlsb::new(Cursor::new(malformed)),
        Err(XlsbError::Zip(_))
    ));
}
