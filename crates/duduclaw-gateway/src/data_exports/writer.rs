//! One physical-row representation for CSV and native XLSX.
use super::{ExportFormat, ExportResult, MAX_BYTES};
use rust_xlsxwriter::Workbook;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum Cell {
    Text(String),
    Number(i64),
}
impl From<String> for Cell {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}
impl From<&str> for Cell {
    fn from(value: &str) -> Self {
        Self::Text(value.into())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogicalRow {
    pub id: String,
    pub cells: Vec<Cell>,
}
#[derive(Debug)]
pub struct PhysicalRow {
    pub id: String,
    pub index: usize,
    pub count: usize,
    pub cells: Vec<Cell>,
}

// Count UTF-16 units, so supplementary characters cannot exceed Excel's limit.
fn chunks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut units = 0;
    for c in text.chars() {
        if units + c.len_utf16() > 32_000 {
            out.push(std::mem::take(&mut current));
            units = 0;
        }
        units += c.len_utf16();
        current.push(c);
    }
    out.push(current);
    out
}
pub fn physical_rows(row: &LogicalRow) -> Vec<PhysicalRow> {
    let split: Vec<Vec<Cell>> = row
        .cells
        .iter()
        .map(|cell| match cell {
            // Mask the complete cell before splitting, so a credential spanning
            // two physical rows cannot evade detection at the output boundary.
            Cell::Text(s) => {
                let masked = if s.len() as u64 <= MAX_BYTES {
                    duduclaw_security::credential_detect::mask_document(s)
                        .unwrap_or_else(|_| duduclaw_security::credential_detect::UNAVAILABLE_MASK.into())
                } else {
                    duduclaw_security::credential_detect::UNAVAILABLE_MASK.into()
                };
                chunks(&masked).into_iter().map(Cell::Text).collect()
            },
            Cell::Number(n) => vec![Cell::Number(*n)],
        })
        .collect();
    let count = split.iter().map(Vec::len).max().unwrap_or(1);
    (0..count)
        .map(|i| PhysicalRow {
            id: row.id.clone(),
            index: i + 1,
            count,
            cells: split
                .iter()
                .map(|parts| {
                    parts
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| Cell::Text(String::new()))
                })
                .collect(),
        })
        .collect()
}
fn csv_text(text: &str) -> String {
    // Spreadsheet applications ignore leading whitespace/control characters.
    let trimmed = text.trim_start_matches(|c: char| c.is_whitespace() || c.is_control());
    if trimmed.starts_with(['=', '+', '-', '@']) || text.starts_with(['\t', '\r', '\n']) {
        format!("'{text}")
    } else {
        text.into()
    }
}
fn escaped_fields(cells: &[Cell], headers: &[&str]) -> ExportResult<String> {
    let fields: Vec<_> = cells
        .iter()
        .zip(headers)
        .filter_map(|(cell, header)| {
            if let Cell::Text(text) = cell {
                if csv_text(text) != *text {
                    return Some(*header);
                }
            }
            None
        })
        .collect();
    serde_json::to_string(&fields).map_err(|_| "EXPORT_ENCODE".to_string())
}
struct LimitedFile(std::fs::File, u64);
impl Write for LimitedFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.1 + buf.len() as u64 > MAX_BYTES {
            return Err(std::io::Error::other("EXPORT_TOO_LARGE"));
        }
        let n = self.0.write(buf)?;
        self.1 += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}
pub fn encode(
    path: &Path,
    format: ExportFormat,
    headers: &[&str],
    rows: &[LogicalRow],
    mut checkpoint: impl FnMut() -> ExportResult<()>,
) -> ExportResult<usize> {
    let mut all_headers = vec!["record_id", "part_index", "part_count"];
    all_headers.extend_from_slice(headers);
    all_headers.push("csv_escaped_fields");
    let mut physical_count = 0;
    match format {
        ExportFormat::Csv => {
            let mut file = LimitedFile(super::private_file(path)?, 0);
            file.write_all(&[0xef, 0xbb, 0xbf])
                .map_err(|_| "EXPORT_TOO_LARGE".to_string())?;
            let mut writer = csv::Writer::from_writer(file);
            writer
                .write_record(&all_headers)
                .map_err(|_| "EXPORT_IO".to_string())?;
            for row in rows {
                checkpoint()?;
                let input_bytes = row.cells.iter().fold(row.id.len(), |total, cell| {
                    total.saturating_add(match cell { Cell::Text(text) => text.len(), Cell::Number(_) => 8 })
                });
                if input_bytes as u64 > MAX_BYTES { return Err("EXPORT_TOO_LARGE".into()); }
                for part in physical_rows(row) {
                    let mut cells = vec![part.id, part.index.to_string(), part.count.to_string()];
                    cells.extend(part.cells.iter().map(|c| match c {
                        Cell::Text(s) => csv_text(s),
                        Cell::Number(n) => n.to_string(),
                    }));
                    // An explicit escape manifest makes CSV text losslessly reversible.
                    cells.push(escaped_fields(&part.cells, headers)?);
                    writer
                        .write_record(&cells)
                        .map_err(|_| "EXPORT_TOO_LARGE".to_string())?;
                    physical_count += 1;
                }
            }
            writer.flush().map_err(|_| "EXPORT_TOO_LARGE".to_string())?;
            writer
                .get_ref()
                .0
                .sync_all()
                .map_err(|_| "EXPORT_IO".to_string())?;
        }
        ExportFormat::Xlsx => {
            let mut workbook = Workbook::new();
            let sheet = workbook.add_worksheet();
            for (col, header) in all_headers.iter().enumerate() {
                sheet
                    .write_string(0, col as u16, *header)
                    .map_err(|_| "EXPORT_ENCODE".to_string())?;
            }
            for row in rows {
                checkpoint()?;
                let input_bytes = row.cells.iter().fold(row.id.len(), |total, cell| {
                    total.saturating_add(match cell { Cell::Text(text) => text.len(), Cell::Number(_) => 8 })
                });
                if input_bytes as u64 > MAX_BYTES { return Err("EXPORT_TOO_LARGE".into()); }
                for part in physical_rows(row) {
                    if physical_count >= 1_048_575 {
                        return Err("EXPORT_TOO_LARGE".into());
                    }
                    let r = (physical_count + 1) as u32;
                    sheet
                        .write_string(r, 0, &part.id)
                        .map_err(|_| "EXPORT_ENCODE".to_string())?;
                    sheet
                        .write_number(r, 1, part.index as f64)
                        .map_err(|_| "EXPORT_ENCODE".to_string())?;
                    sheet
                        .write_number(r, 2, part.count as f64)
                        .map_err(|_| "EXPORT_ENCODE".to_string())?;
                    for (col, cell) in part.cells.iter().enumerate() {
                        // Always write user-controlled content as strings. No formula or URL auto-detection.
                        match cell {
                            Cell::Text(s) => {
                                sheet
                                    .write_string(r, (col + 3) as u16, s)
                                    .map_err(|_| "EXPORT_ENCODE".to_string())?;
                            }
                            Cell::Number(n) if n.unsigned_abs() <= 9_007_199_254_740_991 => {
                                sheet
                                    .write_number(r, (col + 3) as u16, *n as f64)
                                    .map_err(|_| "EXPORT_ENCODE".to_string())?;
                            }
                            Cell::Number(n) => {
                                sheet
                                    .write_string(r, (col + 3) as u16, n.to_string())
                                    .map_err(|_| "EXPORT_ENCODE".to_string())?;
                            }
                        }
                    }
                    sheet
                        .write_string(
                            r,
                            (headers.len() + 3) as u16,
                            escaped_fields(&part.cells, headers)?,
                        )
                        .map_err(|_| "EXPORT_ENCODE".to_string())?;
                    physical_count += 1;
                }
            }
            let bytes = workbook
                .save_to_buffer()
                .map_err(|_| "EXPORT_ENCODE".to_string())?;
            if bytes.len() as u64 > MAX_BYTES {
                return Err("EXPORT_TOO_LARGE".into());
            }
            checkpoint()?;
            let mut file = super::private_file(path)?;
            file.write_all(&bytes)
                .map_err(|_| "EXPORT_IO".to_string())?;
            file.sync_all().map_err(|_| "EXPORT_IO".to_string())?;
        }
    }
    Ok(physical_count)
}

#[cfg(test)]
mod credential_sink_tests {
    use super::*;

    // Written for F1; not run (the review task prohibits builds/tests).
    #[test]
    fn export_masks_before_chunking_and_csv_encoding() {
        let dir = tempfile::tempdir().unwrap();
        let row = LogicalRow {
            id: "message:1".into(),
            cells: vec![Cell::Text(format!("{}password: fixture-export", "a ".repeat(15_997)))],
        };
        let parts = physical_rows(&row);
        assert!(parts.iter().all(|part| part.cells.iter().all(|cell| match cell {
            Cell::Text(value) => !value.contains("fixture-export"),
            Cell::Number(_) => true,
        })));
        let path = dir.path().join("export.csv");
        encode(&path, ExportFormat::Csv, &["content"], &[row], || Ok(())).unwrap();
        let stored = std::fs::read_to_string(path).unwrap();
        assert!(!stored.contains("fixture-export"));
        assert!(stored.contains("message:1"));
    }
}
