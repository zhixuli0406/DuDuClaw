//! CSV: one sheet segment, the first non-empty row is the header.

use super::encoding::decode_text;
use super::{Budget, ExtractError, RawSegment, SegmentKind};

/// Join cells into one line. Cell-internal line breaks become spaces so one
/// spreadsheet row is always one text line.
pub(crate) fn row_line<'a>(cells: impl Iterator<Item = &'a str>) -> String {
    cells
        .map(|c| c.replace(['\r', '\n'], " ").trim().to_string())
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Build a sheet segment from `(row number, line)` pairs. `None` when empty.
pub(crate) fn sheet_segment(sheet: &str, rows: Vec<(u32, String)>) -> Option<RawSegment> {
    let rows: Vec<(u32, String)> = rows
        .into_iter()
        .filter(|(_, line)| line.chars().any(|c| !c.is_whitespace() && c != '|'))
        .collect();
    if rows.is_empty() {
        return None;
    }
    let text = rows
        .iter()
        .map(|(_, l)| l.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let mut seg = RawSegment::new(SegmentKind::Sheet, text);
    seg.sheet = Some(sheet.to_string());
    seg.line_rows = Some(rows.iter().map(|(n, _)| *n).collect());
    Some(seg)
}

/// Parse CSV `bytes`; `sheet` names the segment (the file name).
pub fn extract(
    bytes: &[u8],
    sheet: &str,
    budget: &mut Budget,
) -> Result<Vec<RawSegment>, ExtractError> {
    // Excel and most Windows tools write CRLF. The csv reader counts a bare
    // `\r` as a line of its own in `Position::line`, which doubled every row
    // number on CRLF files (and broke the row a citation points at), so
    // unify line endings before parsing.
    let text = decode_text(bytes)?.replace("\r\n", "\n").replace('\r', "\n");
    budget.take(&text)?;
    let mut reader = ::csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(text.as_bytes());
    let mut rows = Vec::new();
    for (i, record) in reader.records().enumerate() {
        let record = record.map_err(|_| {
            ExtractError::new(
                "unsupported_format",
                "CSV 格式有誤（引號不成對或欄位無法解析）。",
            )
        })?;
        // The reader skips blank lines, so the record index is not the line.
        let line = record
            .position()
            .map(|p| p.line() as u32)
            .unwrap_or((i + 1) as u32);
        rows.push((line, row_line(record.iter())));
    }
    Ok(sheet_segment(sheet, rows).into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crlf_csv_rows_keep_their_line_numbers() {
        // A CRLF file (Excel, most Windows tools) must cite the same rows and
        // produce the same text as the LF version of the same sheet.
        let lf = extract(b"a,b\nx,1\n,,\ny,2\n", "", &mut Budget::new(1 << 20)).unwrap();
        let crlf =
            extract(b"a,b\r\nx,1\r\n,,\r\ny,2\r\n", "", &mut Budget::new(1 << 20)).unwrap();
        assert_eq!(crlf[0].line_rows, lf[0].line_rows);
        assert_eq!(crlf[0].line_rows.as_deref(), Some(&[1, 2, 4][..]));
        assert_eq!(crlf[0].text, lf[0].text);
        assert!(!crlf[0].text.contains('\r'));
    }

    #[test]
    fn big5_csv_rows_keep_numbers() {
        let (bytes, _, _) =
            encoding_rs::BIG5.encode("品號,品名,單價\nA-1234,螺絲,\"1,200\"\n\n,,\nB-9,墊片,30\n");
        let segs = extract(&bytes, "報價.csv", &mut Budget::new(10_000)).unwrap();
        assert_eq!(segs.len(), 1);
        assert_eq!(
            segs[0].text,
            "品號 | 品名 | 單價\nA-1234 | 螺絲 | 1,200\nB-9 | 墊片 | 30"
        );
        assert_eq!(segs[0].line_rows.as_deref(), Some(&[1, 2, 5][..]));
    }
}
