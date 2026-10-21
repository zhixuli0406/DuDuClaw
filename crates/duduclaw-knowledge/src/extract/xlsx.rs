//! Excel `.xlsx` / `.xls` through calamine: one segment per worksheet, one
//! line per non-empty row (cells joined with ` | `), each line tagged with
//! its 1-based spreadsheet row number. The first non-empty row is treated as
//! the header and repeated with every chunk of that sheet by the chunker.

use std::io::Cursor;

use calamine::{Data, Reader, Xls, Xlsx};

use super::csv::{row_line, sheet_segment};
use super::{Budget, ExtractError, Format, RawSegment};

fn cell_text(cell: &Data) -> String {
    match cell {
        Data::Empty => String::new(),
        Data::String(s) => s.clone(),
        Data::Int(i) => i.to_string(),
        Data::Float(f) => {
            if f.fract() == 0.0 && f.abs() < 9.0e15 {
                (*f as i64).to_string()
            } else {
                f.to_string()
            }
        }
        Data::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        Data::DateTime(dt) => match dt.as_datetime() {
            Some(ndt) => {
                if ndt.time() == chrono::NaiveTime::MIN {
                    ndt.format("%Y-%m-%d").to_string()
                } else {
                    ndt.format("%Y-%m-%d %H:%M:%S").to_string()
                }
            }
            None => dt.as_f64().to_string(),
        },
        Data::DateTimeIso(s) | Data::DurationIso(s) => s.clone(),
        Data::Error(_) => String::new(),
    }
}

fn read_sheets<R>(mut book: R, budget: &mut Budget) -> Result<Vec<RawSegment>, ExtractError>
where
    R: Reader<Cursor<Vec<u8>>>,
{
    let mut out = Vec::new();
    for name in book.sheet_names() {
        let Ok(range) = book.worksheet_range(&name) else {
            continue;
        };
        let first_row = range.start().map(|(r, _)| r).unwrap_or(0);
        let mut rows = Vec::new();
        for (i, row) in range.rows().enumerate() {
            let line = row_line(row.iter().map(cell_text).collect::<Vec<_>>().iter().map(String::as_str));
            budget.take(&line)?;
            rows.push((first_row + i as u32 + 1, line));
        }
        if let Some(seg) = sheet_segment(&name, rows) {
            out.push(seg);
        }
    }
    Ok(out)
}

/// Extract an `.xlsx` / `.xls` workbook.
pub fn extract(bytes: &[u8], format: Format, budget: &mut Budget) -> Result<Vec<RawSegment>, ExtractError> {
    let cursor = Cursor::new(bytes.to_vec());
    match format {
        Format::Xls => match calamine::open_workbook_from_rs::<Xls<_>, _>(cursor) {
            Ok(book) => read_sheets(book, budget),
            Err(calamine::XlsError::Password) => Err(ExtractError::encrypted()),
            Err(_) => Err(ExtractError::corrupt(format)),
        },
        _ => match calamine::open_workbook_from_rs::<Xlsx<_>, _>(cursor) {
            Ok(book) => read_sheets(book, budget),
            Err(calamine::XlsxError::Password) => Err(ExtractError::encrypted()),
            Err(_) => Err(ExtractError::corrupt(format)),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::fixtures;

    #[test]
    fn sheets_rows_and_numbers() {
        let bytes = fixtures::xlsx(&[
            (
                "報價",
                &[
                    &["品號", "品名", "單價"],
                    &["A-1234", "螺絲", "1200"],
                    &["B-9", "墊片", "30"],
                ],
            ),
            ("聯絡人", &[&["姓名", "分機"], &["王小明", "201"]]),
        ]);
        let segs = extract(&bytes, Format::Xlsx, &mut Budget::new(10_000)).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].sheet.as_deref(), Some("報價"));
        assert_eq!(
            segs[0].text,
            "品號 | 品名 | 單價\nA-1234 | 螺絲 | 1200\nB-9 | 墊片 | 30"
        );
        assert_eq!(segs[0].line_rows.as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(segs[1].sheet.as_deref(), Some("聯絡人"));
    }

    #[test]
    fn garbage_is_corrupt() {
        assert_eq!(
            extract(b"PK\x03\x04garbage", Format::Xlsx, &mut Budget::new(100))
                .unwrap_err()
                .code,
            "unsupported_format"
        );
    }
}
