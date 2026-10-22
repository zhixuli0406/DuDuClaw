//! Excel `.xlsx` / `.xls` through calamine: one segment per worksheet, one
//! line per non-empty row (cells joined with ` | `), each line tagged with
//! its 1-based spreadsheet row number. The first non-empty row is treated as
//! the header and repeated with every chunk of that sheet by the chunker.
//!
//! `.xlsx` sheets are read cell by cell (calamine's streaming reader), so a
//! sheet that declares a huge dimension cannot make the reader allocate a
//! dense grid; at most [`MAX_CELLS`] non-empty cells per workbook are read
//! before the file is refused. `.xls` (BIFF) is bounded by the format itself
//! (65,536 × 256) and read through the dense range.

use std::io::Cursor;

use calamine::{Data, Reader, Xls, Xlsx};

use std::collections::BTreeMap;

use super::csv::{row_line, sheet_segment};
use super::{Budget, ExtractError, Format, RawSegment};

/// Non-empty cells read from one `.xlsx` workbook at most.
pub const MAX_CELLS: usize = 2_000_000;
/// Highest column read (beyond it a row is cut; XFD = 16,384 is Excel's).
const MAX_COLUMN: u32 = 16_384;
/// Rows whose last value is in a column below this keep empty cells.
const PAD_COLUMNS: u32 = 64;

fn too_many_cells() -> ExtractError {
    ExtractError::new(
        "document_limits",
        "試算表的儲存格太多（超過 200 萬格），已拒絕讀取。請拆成較小的檔案。",
    )
}

/// Stream every sheet of an `.xlsx` workbook.
fn read_xlsx(
    mut book: Xlsx<Cursor<Vec<u8>>>,
    budget: &mut Budget,
) -> Result<Vec<RawSegment>, ExtractError> {
    let mut out = Vec::new();
    let mut cells_read = 0usize;
    for name in book.sheet_names() {
        let Ok(mut reader) = book.worksheet_cells_reader(&name) else {
            continue;
        };
        let mut rows: BTreeMap<u32, BTreeMap<u32, String>> = BTreeMap::new();
        loop {
            let cell = match reader.next_cell() {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(_) => return Err(ExtractError::corrupt(Format::Xlsx)),
            };
            let (row, col) = cell.get_position();
            let text = cell_text(&Data::from(cell.get_value().clone()));
            if text.is_empty() || col >= MAX_COLUMN {
                continue;
            }
            cells_read += 1;
            if cells_read > MAX_CELLS {
                return Err(too_many_cells());
            }
            budget.take(&text)?;
            rows.entry(row).or_default().insert(col, text);
        }
        let lines = rows
            .into_iter()
            .map(|(row, cells)| {
                // Keep column positions (empty cells between values stay
                // empty) for ordinary tables; a row that reaches past
                // PAD_COLUMNS lists its values without padding, so sparse far
                // cells cannot inflate a line.
                let last = cells.keys().next_back().copied().unwrap_or(0);
                let line = if last < PAD_COLUMNS {
                    let filled: Vec<String> = (0..=last)
                        .map(|c| cells.get(&c).cloned().unwrap_or_default())
                        .collect();
                    row_line(filled.iter().map(String::as_str))
                } else {
                    row_line(cells.values().map(String::as_str))
                };
                (row + 1, line)
            })
            .collect();
        if let Some(seg) = sheet_segment(&name, lines) {
            out.push(seg);
        }
    }
    Ok(out)
}

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
            let line = row_line(
                row.iter()
                    .map(cell_text)
                    .collect::<Vec<_>>()
                    .iter()
                    .map(String::as_str),
            );
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
pub fn extract(
    bytes: &[u8],
    format: Format,
    budget: &mut Budget,
) -> Result<Vec<RawSegment>, ExtractError> {
    let cursor = Cursor::new(bytes.to_vec());
    match format {
        Format::Xls => match calamine::open_workbook_from_rs::<Xls<_>, _>(cursor) {
            Ok(book) => read_sheets(book, budget),
            Err(calamine::XlsError::Password) => Err(ExtractError::encrypted()),
            Err(_) => Err(ExtractError::corrupt(format)),
        },
        _ => match calamine::open_workbook_from_rs::<Xlsx<_>, _>(cursor) {
            Ok(book) => read_xlsx(book, budget),
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
    fn huge_declared_dimension_is_streamed() {
        // A sheet claiming the full Excel grid but holding two cells.
        let bytes = fixtures::xlsx_raw_sheet(
            r#"<dimension ref="A1:XFD1048576"/><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>品號</t></is></c></row><row r="1048576"><c r="XFD1048576"><v>7</v></c></row></sheetData>"#,
        );
        let segs = extract(&bytes, Format::Xlsx, &mut Budget::new(10_000)).unwrap();
        assert_eq!(segs[0].text, "品號\n7");
        assert_eq!(segs[0].line_rows.as_deref(), Some(&[1, 1_048_576][..]));
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
