//! Local integration seam to E. Only an already-authorized fd reaches a parser.
//! Table options are host-side adapter inputs, not additions to the worker wire
//! protocol. The E adapter must enforce them in its isolated worker; it may not
//! parse the file in the gateway or reopen it using a pathname.
use std::{fs::File, future::Future, pin::Pin};
use duduclaw_isolation::{bounded::List, worker::*};
use rand::{rngs::OsRng, RngCore};
use serde::Deserialize;
use serde_json::{Map, Value};
use crate::{RequestContext, VerificationError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableKind { Csv, Spreadsheet }

#[derive(Clone, Debug)]
pub struct TableOptions {
    pub kind: TableKind,
    pub delimiter: u8,
    pub has_header: bool,
    pub offset: usize,
    pub limit: usize,
    pub sheet: Option<String>,
}

impl TableOptions {
    pub fn validate(&self) -> Result<(), VerificationError> {
        if !(1..=2000).contains(&self.limit) || self.offset > 1_000_000
            || !self.delimiter.is_ascii() || matches!(self.delimiter, 0 | b'\r' | b'\n')
            || self.sheet.as_ref().is_some_and(|s| s.is_empty() || s.len() > 1024)
            || (self.kind == TableKind::Csv && self.sheet.is_some()) {
            return Err(VerificationError::Invalid);
        }
        Ok(())
    }
}

/// Implemented by E's authenticated worker client. A missing client is a denial.
/// Return a bounded JSON table encoded in Text items in wire order, input slot 0,
/// without pages. All worker strings remain data and are revalidated by D.
pub trait FileParser: Send + Sync {
    fn parse<'a>(&'a self, context: RequestContext, request: ParseRequest, input: File,
        options: TableOptions)
        -> Pin<Box<dyn Future<Output = Result<ParseResponse, VerificationError>> + Send + 'a>>;
}

pub fn table_request(input: &File, options: &TableOptions) -> Result<ParseRequest, VerificationError> {
    options.validate()?;
    let meta = input.metadata().map_err(|_| VerificationError::Unavailable)?;
    let cap = match options.kind { TableKind::Csv => 64 * 1024 * 1024, TableKind::Spreadsheet => 32 * 1024 * 1024 };
    if !meta.is_file() || meta.len() > cap { return Err(VerificationError::Invalid); }
    let mut id = [0; 16];
    OsRng.try_fill_bytes(&mut id).map_err(|_| VerificationError::Unavailable)?;
    // Decode the unchanged base schema so this adapter also compiles with E's
    // additive optional request fields on the integration branch.
    let request: ParseRequest = serde_json::from_value(serde_json::json!({
        "version": WORKER_PROTOCOL_VERSION, "request_id": id, "operation": "extract_document",
        "inputs": [{ "slot": 0, "format": match options.kind {
            TableKind::Csv => ContentFormat::Text, TableKind::Spreadsheet => ContentFormat::Spreadsheet,
        }, "size_bytes": meta.len() }],
        "outputs": [],
        "limits": {
            "memory_bytes": 256 * 1024 * 1024,
            "wall_time_ms": 30_000,
            "max_result_text_bytes": MAX_RESULT_TEXT_BYTES,
        },
    })).map_err(|_| VerificationError::Invalid)?;
    request.validate().map_err(|_| VerificationError::Invalid)?;
    Ok(request)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TableResult {
    columns: List<String, 256>,
    rows: List<Map<String, Value>, 2000>,
    truncated: bool,
    #[serde(default)]
    sheet: Option<String>,
    #[serde(default)]
    sheets: Option<List<String, 256>>,
}

/// Never return raw parser text: doing so would bypass column-level redaction.
pub fn decode_table_response(request: &ParseRequest, options: &TableOptions, response: ParseResponse)
    -> Result<Value, VerificationError> {
    options.validate()?;
    response.validate_for(request).map_err(|_| VerificationError::Invalid)?;
    let format = match options.kind { TableKind::Csv => ContentFormat::Text,
        TableKind::Spreadsheet => ContentFormat::Spreadsheet };
    if request.operation != ParseOperation::ExtractDocument || request.inputs.as_slice().len() != 1
        || request.inputs.as_slice()[0].format != format || !request.outputs.as_slice().is_empty() {
        return Err(VerificationError::Invalid);
    }
    let ParseResult::Complete { items } = response.result else { return Err(VerificationError::Unavailable); };
    let mut text = String::new();
    for item in items.as_slice() {
        let ParsedItem::Text { input_slot: 0, page: None, text: part } = item else {
            return Err(VerificationError::Invalid);
        };
        if text.len().saturating_add(part.as_str().len()) > MAX_RESULT_TEXT_BYTES {
            return Err(VerificationError::Invalid);
        }
        text.push_str(part.as_str());
    }
    let result: TableResult = serde_json::from_str(&text).map_err(|_| VerificationError::Invalid)?;
    let columns = result.columns.as_slice();
    if result.rows.as_slice().len() > options.limit
        || columns.iter().any(|s| s.is_empty() || s.len() > 1024)
        || columns.iter().collect::<std::collections::BTreeSet<_>>().len() != columns.len() {
        return Err(VerificationError::Invalid);
    }
    for row in result.rows.as_slice() {
        if row.len() != columns.len() || columns.iter().any(|name| !row.contains_key(name)) {
            return Err(VerificationError::Invalid);
        }
        for value in row.values() {
            if value.is_array() || value.is_object()
                || value.as_str().is_some_and(|s| s.len() > 8192)
                || (options.kind == TableKind::Csv && !value.is_string()) {
                return Err(VerificationError::Invalid);
            }
        }
    }
    let mut payload = serde_json::json!({"columns": columns, "rows": result.rows.as_slice(),
        "row_count": result.rows.as_slice().len(), "truncated": result.truncated});
    match options.kind {
        TableKind::Csv if result.sheet.is_some() || result.sheets.is_some() => return Err(VerificationError::Invalid),
        TableKind::Csv => {},
        TableKind::Spreadsheet => {
            let sheet = result.sheet.ok_or(VerificationError::Invalid)?;
            let sheets = result.sheets.ok_or(VerificationError::Invalid)?;
            if sheet.is_empty() || sheet.len() > 1024
                || sheets.as_slice().iter().any(|s| s.is_empty() || s.len() > 1024)
                || !sheets.as_slice().contains(&sheet)
                || sheets.as_slice().iter().collect::<std::collections::BTreeSet<_>>().len() != sheets.as_slice().len()
                || options.sheet.as_ref().is_some_and(|selected| selected != &sheet) {
                return Err(VerificationError::Invalid);
            }
            payload["sheet"] = Value::String(sheet);
            payload["sheets"] = serde_json::json!(sheets.as_slice());
        }
    }
    Ok(payload)
}

#[cfg(test)]
#[path = "parsing_tests.rs"]
mod tests;
