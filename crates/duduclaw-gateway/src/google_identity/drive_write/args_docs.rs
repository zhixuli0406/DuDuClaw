//! Closed Docs/Sheets arguments. Construct through args::parse_any.
use serde::Deserialize;
use serde_json::Value;
use super::{args::{self, ArgError}, transport::GoogleId};

#[derive(Clone)]
pub(crate) enum DocsSheetsArgs {
    Insert { target: GoogleId, text: String, position: Position, tab_id: Option<String> },
    Format { target: GoogleId, operations: Vec<Operation> },
    AddSheet { target: GoogleId, title: String, copy_from_sheet: Option<String> },
    AddNote { target: GoogleId, range: A1Range, note: String },
}

#[derive(Clone)]
pub(crate) enum Position { Start, End, After(String) }

#[derive(Clone, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum NamedStyle {
    NormalText, Title, Subtitle,
    #[serde(rename = "HEADING_1")] Heading1,
    #[serde(rename = "HEADING_2")] Heading2,
    #[serde(rename = "HEADING_3")] Heading3,
    #[serde(rename = "HEADING_4")] Heading4,
    #[serde(rename = "HEADING_5")] Heading5,
    #[serde(rename = "HEADING_6")] Heading6,
}
impl NamedStyle {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::NormalText => "NORMAL_TEXT", Self::Title => "TITLE", Self::Subtitle => "SUBTITLE",
            Self::Heading1 => "HEADING_1", Self::Heading2 => "HEADING_2", Self::Heading3 => "HEADING_3",
            Self::Heading4 => "HEADING_4", Self::Heading5 => "HEADING_5", Self::Heading6 => "HEADING_6",
        }
    }
}

#[derive(Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub(crate) enum Alignment { Start, Center, End, Justified }

// Explicit null is not an omitted style field.
fn present<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where D: serde::Deserializer<'de>, T: Deserialize<'de> { T::deserialize(d).map(Some) }

#[derive(Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Operation {
    ParagraphStyle {
        text: String, named_style: NamedStyle,
        #[serde(default, deserialize_with = "present")]
        alignment: Option<Alignment>,
    },
    TextStyle {
        text: String,
        #[serde(default, deserialize_with = "present")]
        bold: Option<bool>,
        #[serde(default, deserialize_with = "present")]
        italic: Option<bool>,
        #[serde(default, deserialize_with = "present")]
        underline: Option<bool>,
        #[serde(default, deserialize_with = "present")]
        font_size_pt: Option<f64>,
    },
    TableCellStyle { table_index: u32, row: u32, column: u32, background_hex: String },
    Header { text: String },
    AddTab { title: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InsertWire {
    document_id: String, text: String,
    #[serde(default, deserialize_with = "present")]
    at: Option<String>,
    #[serde(default, deserialize_with = "present")]
    after_text: Option<String>,
    #[serde(default, deserialize_with = "present")]
    tab_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FormatWire { document_id: String, operations: Vec<Operation> }
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SheetWire {
    spreadsheet_id: String, title: String,
    #[serde(default, deserialize_with = "present")]
    copy_from_sheet: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NoteWire { spreadsheet_id: String, range: String, note: String }

fn bounded(text: &str, min: usize, max: usize) -> Result<(), ArgError> {
    let count = text.chars().count();
    if count > max { return Err(ArgError::ContentTooLarge); }
    if count < min { return Err(ArgError::InvalidArgument); }
    Ok(())
}
pub(crate) fn valid_tab_id(value: &str) -> bool {
    (1..=128).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}
fn sheet_name(value: &str) -> Result<(), ArgError> {
    bounded(value, 1, 100)?;
    if value.trim().is_empty() || value.chars().any(|c| c.is_control() || matches!(c, '[' | ']' | '*' | '?' | '/' | '\\' | ':')) {
        return Err(ArgError::InvalidArgument);
    }
    Ok(())
}
fn decode<T: serde::de::DeserializeOwned>(value: &Value) -> Result<T, ArgError> {
    serde_json::from_value(value.clone()).map_err(|_| ArgError::InvalidArgument)
}
pub(crate) fn parse(tool: &str, value: &Value) -> Result<DocsSheetsArgs, ArgError> {
    Ok(match tool {
        "docs_insert_text" => {
            let w: InsertWire = decode(value)?;
            bounded(&w.text, 1, 20_000)?;
            let position = match (w.at.as_deref(), w.after_text) {
                (Some("start"), None) => Position::Start,
                (Some("end"), None) => Position::End,
                (None, Some(text)) => { bounded(&text, 1, 1_000)?; Position::After(text) }
                _ => return Err(ArgError::InvalidArgument),
            };
            if w.tab_id.as_deref().is_some_and(|id| !valid_tab_id(id)) { return Err(ArgError::InvalidArgument); }
            DocsSheetsArgs::Insert { target: args::id(&w.document_id)?, text: w.text, position, tab_id: w.tab_id }
        }
        "docs_format" => {
            let w: FormatWire = decode(value)?;
            if !(1..=50).contains(&w.operations.len()) { return Err(ArgError::InvalidArgument); }
            for op in &w.operations {
                match op {
                    Operation::ParagraphStyle { text, .. } => bounded(text, 1, usize::MAX)?,
                    Operation::TextStyle { text, bold, italic, underline, font_size_pt } => {
                        bounded(text, 1, usize::MAX)?;
                        if bold.is_none() && italic.is_none() && underline.is_none() && font_size_pt.is_none()
                            || font_size_pt.is_some_and(|n| !n.is_finite() || !(6.0..=96.0).contains(&n))
                        { return Err(ArgError::InvalidArgument); }
                    }
                    Operation::TableCellStyle { table_index, row, column, background_hex } => {
                        if *table_index > 99 || *row > 499 || *column > 49
                            || background_hex.len() != 7 || !background_hex.starts_with('#')
                            || !background_hex.chars().skip(1).all(|c| c.is_ascii_hexdigit())
                        { return Err(ArgError::InvalidArgument); }
                    }
                    Operation::Header { text } => bounded(text, 0, 1_000)?,
                    Operation::AddTab { title } => bounded(title, 1, 100)?,
                }
            }
            DocsSheetsArgs::Format { target: args::id(&w.document_id)?, operations: w.operations }
        }
        "sheets_add_sheet" => {
            let w: SheetWire = decode(value)?;
            sheet_name(&w.title)?;
            if let Some(name) = &w.copy_from_sheet { sheet_name(name)?; }
            DocsSheetsArgs::AddSheet { target: args::id(&w.spreadsheet_id)?, title: w.title, copy_from_sheet: w.copy_from_sheet }
        }
        "sheets_add_note" => {
            let w: NoteWire = decode(value)?;
            bounded(&w.note, 0, 5_000)?;
            DocsSheetsArgs::AddNote { target: args::id(&w.spreadsheet_id)?, range: parse_a1(&w.range)?, note: w.note }
        }
        _ => return Err(ArgError::UnknownTool),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct A1Range {
    pub(crate) sheet: Option<String>,
    pub(crate) start_row: u32, pub(crate) end_row: u32,
    pub(crate) start_column: u32, pub(crate) end_column: u32,
}

pub(crate) fn parse_a1(value: &str) -> Result<A1Range, ArgError> {
    let invalid = ArgError::InvalidArgument;
    let (sheet, cells) = if let Some(rest) = value.strip_prefix('\'') {
        let mut chars = rest.char_indices().peekable();
        let mut name = String::new();
        let cells = loop {
            let (_, c) = chars.next().ok_or(invalid)?;
            if c != '\'' { name.push(c); continue; }
            if chars.peek().is_some_and(|(_, c)| *c == '\'') {
                chars.next(); name.push('\''); continue;
            }
            let (offset, bang) = chars.next().ok_or(invalid)?;
            if bang != '!' { return Err(invalid); }
            break rest.get(offset + bang.len_utf8()..).ok_or(invalid)?;
        };
        sheet_name(&name)?;
        (Some(name), cells)
    } else if let Some((name, cells)) = value.split_once('!') {
        sheet_name(name)?;
        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') { return Err(invalid); }
        (Some(name.to_owned()), cells)
    } else { (None, value) };
    fn cell(value: &str) -> Result<(u32, u32), ArgError> {
        let mut chars = value.chars().peekable();
        let mut column = 0u32;
        while let Some(c) = chars.peek().copied().filter(char::is_ascii_uppercase) {
            chars.next();
            column = column.checked_mul(26).and_then(|n| n.checked_add(c as u32 - 'A' as u32 + 1)).ok_or(ArgError::InvalidArgument)?;
        }
        if column == 0 || column > 18_278 || !chars.peek().is_some_and(|c| matches!(c, '1'..='9')) { return Err(ArgError::InvalidArgument); }
        let mut row = 0u32;
        for c in chars {
            if !c.is_ascii_digit() { return Err(ArgError::InvalidArgument); }
            row = row.checked_mul(10).and_then(|n| n.checked_add(c as u32 - '0' as u32)).ok_or(ArgError::InvalidArgument)?;
        }
        if row == 0 || row > i32::MAX as u32 { return Err(ArgError::InvalidArgument); }
        Ok((row, column))
    }
    let (start, end) = cells.split_once(':').unwrap_or((cells, cells));
    let (sr, sc) = cell(start)?;
    let (er, ec) = cell(end)?;
    if er < sr || ec < sc { return Err(invalid); }
    if u64::from(er - sr + 1) * u64::from(ec - sc + 1) > 1_000 { return Err(ArgError::ContentTooLarge); }
    Ok(A1Range { sheet, start_row: sr - 1, end_row: er, start_column: sc - 1, end_column: ec })
}
