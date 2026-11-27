//! Pure Docs/Sheets programs. The executor owns approval, fresh Drive scope
//! checks, state reads, sending, and post-write verification. Never log state.
use serde_json::{json, Value};
use super::{
    args::ArgError,
    args_docs::{valid_tab_id, DocsSheetsArgs, Operation, Position},
    program::OutcomeError,
    scope::{Plan, ScopeVerdict},
    transport::{GoogleId, JsonBody, WriteCall},
};

fn target(args: &DocsSheetsArgs) -> &GoogleId {
    match args {
        DocsSheetsArgs::Insert { target, .. } | DocsSheetsArgs::Format { target, .. }
        | DocsSheetsArgs::AddSheet { target, .. } | DocsSheetsArgs::AddNote { target, .. } => target,
    }
}
fn is_doc(args: &DocsSheetsArgs) -> bool {
    matches!(args, DocsSheetsArgs::Insert { .. } | DocsSheetsArgs::Format { .. })
}
pub(crate) fn plan(args: &DocsSheetsArgs) -> Plan {
    Plan::Modify { target: target(args).clone(), rename: false }
}
fn checked_scope(args: &DocsSheetsArgs, verdict: &ScopeVerdict) -> Result<(), ArgError> {
    match verdict {
        ScopeVerdict::InScope { target_id: Some(id), .. } if id == target(args).as_str() => Ok(()),
        _ => Err(ArgError::ScopeUnconfirmed),
    }
}
/// Call only after the common Drive metadata folder check, even for Docs.
pub(crate) fn read_call(args: &DocsSheetsArgs, verdict: &ScopeVerdict) -> Result<WriteCall, ArgError> {
    checked_scope(args, verdict)?;
    Ok(if is_doc(args) {
        WriteCall::DocsGet { document_id: target(args).clone(), include_tabs: matches!(args, DocsSheetsArgs::Insert { tab_id: Some(_), .. }) }
    } else { WriteCall::SheetsGetProperties { spreadsheet_id: target(args).clone() } })
}

#[derive(Clone)]
enum ReplyKind { Empty, Header, Tab, AddSheet, DuplicateSheet }

/// Keep this plan with the approved invocation. Phase 1 may have committed
/// even if Phase 2 fails; never retry the whole operation on an uncertain reply.
pub(crate) struct BatchPlan {
    target: GoogleId,
    document: bool,
    requests: Vec<Value>,
    replies: Vec<ReplyKind>,
    headers: Vec<(usize, String)>,
}
impl BatchPlan {
    pub(crate) fn phase1(&self) -> Result<WriteCall, ArgError> { self.call(&self.requests) }
    fn call(&self, requests: &[Value]) -> Result<WriteCall, ArgError> {
        let requests_json = JsonBody::new(json!({"requests":requests})).map_err(|_| ArgError::ContentTooLarge)?;
        Ok(if self.document {
            WriteCall::DocsBatchUpdate { document_id: self.target.clone(), requests_json }
        } else { WriteCall::SheetsBatchUpdate { spreadsheet_id: self.target.clone(), requests_json } })
    }
    /// Only returns a follow-up after validating the entire successful reply.
    pub(crate) fn phase2(&self, status: u16, body: &[u8]) -> Result<Option<WriteCall>, OutcomeError> {
        let requests = self.phase2_requests(status, body)?;
        if requests.is_empty() { return Ok(None); }
        self.call(&requests).map(Some).map_err(|_| OutcomeError::Unconfirmed)
    }
    fn phase2_requests(&self, status: u16, body: &[u8]) -> Result<Vec<Value>, OutcomeError> {
        let response = self.response(status, body, &self.replies)?;
        let mut requests = Vec::new();
        for (index, text) in &self.headers {
            let id = response["replies"][*index]["createHeader"]["headerId"].as_str().ok_or(OutcomeError::Unconfirmed)?;
            if !text.is_empty() {
                requests.push(json!({"insertText":{"location":{"segmentId":id,"index":0},"text":text}}));
            }
        }
        Ok(requests)
    }
    /// Phase 1 outcome contains generated tab/sheet/header IDs. For header
    /// insertion, success is final only after interpret_phase2 also succeeds.
    pub(crate) fn interpret(&self, status: u16, body: &[u8]) -> Result<DocsSheetsOutcome, OutcomeError> {
        let response = self.response(status, body, &self.replies)?;
        let mut outcome = DocsSheetsOutcome {
            file_id: self.target.as_str().to_owned(),
            link: format!("https://docs.google.com/{}/d/{}/edit", if self.document { "document" } else { "spreadsheets" }, self.target.as_str()),
            tab_ids: Vec::new(), sheet_ids: Vec::new(), header_ids: Vec::new(),
            needs_phase2: self.headers.iter().any(|(_, text)| !text.is_empty()),
        };
        for (reply, kind) in response["replies"].as_array().ok_or(OutcomeError::Unconfirmed)?.iter().zip(&self.replies) {
            match kind {
                ReplyKind::Tab => outcome.tab_ids.push(reply["addDocumentTab"]["tabProperties"]["tabId"].as_str().ok_or(OutcomeError::Unconfirmed)?.to_owned()),
                ReplyKind::Header => outcome.header_ids.push(reply["createHeader"]["headerId"].as_str().ok_or(OutcomeError::Unconfirmed)?.to_owned()),
                ReplyKind::AddSheet | ReplyKind::DuplicateSheet => {
                    let key = if matches!(kind, ReplyKind::AddSheet) { "addSheet" } else { "duplicateSheet" };
                    outcome.sheet_ids.push(reply[key]["properties"]["sheetId"].as_u64().ok_or(OutcomeError::Unconfirmed)? as u32);
                }
                ReplyKind::Empty => {}
            }
        }
        Ok(outcome)
    }
    pub(crate) fn interpret_phase2(&self, status: u16, body: &[u8]) -> Result<(), OutcomeError> {
        let count = self.headers.iter().filter(|(_, text)| !text.is_empty()).count();
        if count == 0 { return Err(OutcomeError::Unconfirmed); }
        self.response(status, body, &vec![ReplyKind::Empty; count]).map(|_| ())
    }
    fn response(&self, status: u16, body: &[u8], kinds: &[ReplyKind]) -> Result<Value, OutcomeError> {
        if !(200..=299).contains(&status) {
            if status == 403 {
                if let Ok(value) = serde_json::from_slice::<Value>(body) {
                    let error = &value["error"];
                    if error["status"].as_str() == Some("insufficientPermissions")
                        || error["errors"].as_array().is_some_and(|items| items.iter().any(|v| v["reason"].as_str() == Some("insufficientPermissions")))
                    { return Err(OutcomeError::InsufficientPermissions); }
                }
            }
            return Err(OutcomeError::Google(status));
        }
        let value: Value = serde_json::from_slice(body).map_err(|_| OutcomeError::Unconfirmed)?;
        let id_key = if self.document { "documentId" } else { "spreadsheetId" };
        if value[id_key].as_str() != Some(self.target.as_str()) || value.get("error").is_some() { return Err(OutcomeError::Unconfirmed); }
        let replies = value["replies"].as_array().ok_or(OutcomeError::Unconfirmed)?;
        if replies.len() != kinds.len() { return Err(OutcomeError::Unconfirmed); }
        for (reply, kind) in replies.iter().zip(kinds) {
            let valid = match kind {
                ReplyKind::Empty => reply.as_object().is_some_and(|m| m.is_empty()),
                ReplyKind::Header => reply["createHeader"]["headerId"].as_str().is_some_and(valid_tab_id),
                ReplyKind::Tab => reply["addDocumentTab"]["tabProperties"]["tabId"].as_str().is_some_and(valid_tab_id),
                ReplyKind::AddSheet | ReplyKind::DuplicateSheet => {
                    let key = if matches!(kind, ReplyKind::AddSheet) { "addSheet" } else { "duplicateSheet" };
                    reply[key]["properties"]["sheetId"].as_u64().is_some_and(|id| id <= i32::MAX as u64)
                }
            };
            if !valid { return Err(OutcomeError::Unconfirmed); }
        }
        Ok(value)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DocsSheetsOutcome {
    pub(crate) file_id: String,
    pub(crate) link: String,
    pub(crate) tab_ids: Vec<String>,
    pub(crate) sheet_ids: Vec<u32>,
    pub(crate) header_ids: Vec<String>,
    pub(crate) needs_phase2: bool,
}

fn index(value: &Value) -> Result<u32, ArgError> {
    value.as_u64().filter(|n| *n <= i32::MAX as u64).map(|n| n as u32).ok_or(ArgError::InvalidState)
}
fn body<'a>(state: &'a Value, tab_id: Option<&str>) -> Result<&'a Value, ArgError> {
    let Some(id) = tab_id else { return state.get("body").ok_or(ArgError::InvalidState); };
    let mut pending: Vec<_> = state["tabs"].as_array().ok_or(ArgError::InvalidState)?.iter().collect();
    let mut found = None;
    while let Some(tab) = pending.pop() {
        if tab["tabProperties"]["tabId"].as_str() == Some(id) {
            if found.is_some() { return Err(ArgError::InvalidState); }
            found = Some(tab.get("documentTab").and_then(|v| v.get("body")).ok_or(ArgError::InvalidState)?);
        }
        if let Some(children) = tab.get("childTabs") {
            pending.extend(children.as_array().ok_or(ArgError::InvalidState)?);
        }
    }
    found.ok_or(ArgError::InvalidState)
}

struct IndexedChar { ch: char, start: u32, end: u32, paragraph: (u32, u32) }
struct BodyIndex<'a> {
    chars: Vec<IndexedChar>,
    tables: Vec<&'a Value>,
    start: u32,
    end: u32,
}
impl<'a> BodyIndex<'a> {
    fn new(body: &'a Value) -> Result<Self, ArgError> {
        let content = body["content"].as_array().ok_or(ArgError::InvalidState)?;
        let first = content.iter().find(|v| v.get("paragraph").is_some()).ok_or(ArgError::InvalidState)?;
        let start = index(&first["startIndex"])?;
        let end = index(&content.last().ok_or(ArgError::InvalidState)?["endIndex"])?
            .checked_sub(1).ok_or(ArgError::InvalidState)?;
        if start < 1 || start > end { return Err(ArgError::InvalidState); }
        let mut result = Self { chars: Vec::new(), tables: Vec::new(), start, end };
        result.collect(content, 0)?;
        Ok(result)
    }
    fn collect(&mut self, content: &'a [Value], depth: usize) -> Result<(), ArgError> {
        if depth > 64 { return Err(ArgError::InvalidState); }
        for element in content {
            if let Some(paragraph) = element.get("paragraph") {
                let ps = index(&element["startIndex"])?;
                let pe = index(&element["endIndex"])?;
                if pe <= ps { return Err(ArgError::InvalidState); }
                for run in paragraph["elements"].as_array().ok_or(ArgError::InvalidState)? {
                    let Some(text_run) = run.get("textRun") else { continue; };
                    let text = text_run["content"].as_str().ok_or(ArgError::InvalidState)?;
                    let mut pos = index(&run["startIndex"])?;
                    let end = index(&run["endIndex"])?;
                    if pos < ps || end > pe || pos > end { return Err(ArgError::InvalidState); }
                    for ch in text.chars() {
                        let next = pos.checked_add(ch.len_utf16() as u32).ok_or(ArgError::InvalidState)?;
                        if self.chars.last().is_some_and(|prev| prev.end > pos) { return Err(ArgError::InvalidState); }
                        self.chars.push(IndexedChar { ch, start: pos, end: next, paragraph: (ps, pe) });
                        pos = next;
                    }
                    if pos != end { return Err(ArgError::InvalidState); }
                }
            } else if let Some(table) = element.get("table") {
                self.tables.push(element);
                for row in table["tableRows"].as_array().ok_or(ArgError::InvalidState)? {
                    for cell in row["tableCells"].as_array().ok_or(ArgError::InvalidState)? {
                        self.collect(cell["content"].as_array().ok_or(ArgError::InvalidState)?, depth + 1)?;
                    }
                }
            } else if let Some(toc) = element.get("tableOfContents") {
                self.collect(toc["content"].as_array().ok_or(ArgError::InvalidState)?, depth + 1)?;
            } else if element.get("sectionBreak").is_none() { return Err(ArgError::InvalidState); }
        }
        Ok(())
    }
    fn anchor(&self, text: &str, paragraph: bool) -> Result<(u32, u32), ArgError> {
        let needle: Vec<_> = text.chars().collect();
        if needle.is_empty() { return Err(ArgError::InvalidArgument); }
        let mut found = None;
        // windows includes overlapping occurrences ("aa" in "aaa" is ambiguous).
        for window in self.chars.windows(needle.len()) {
            if !window.iter().zip(&needle).all(|(a, b)| a.ch == *b)
                || !window.windows(2).all(|pair| pair[0].end == pair[1].start) { continue; }
            if found.is_some() { return Err(ArgError::AnchorNotUnique); }
            let first = &window[0];
            let last = window.last().ok_or(ArgError::InvalidState)?;
            found = Some((first.start, last.end, first.paragraph, last.paragraph));
        }
        let (start, end, first, last) = found.ok_or(ArgError::AnchorNotUnique)?;
        if paragraph {
            if first != last { return Err(ArgError::InvalidArgument); }
            Ok(first)
        } else { Ok((start, end)) }
    }
}

fn sheets(state: &Value) -> Result<Vec<(u32, &str)>, ArgError> {
    let mut result = Vec::new();
    for sheet in state["sheets"].as_array().ok_or(ArgError::InvalidState)? {
        let properties = &sheet["properties"];
        let id = index(&properties["sheetId"])?;
        let title = properties["title"].as_str().filter(|v| !v.is_empty()).ok_or(ArgError::InvalidState)?;
        if result.iter().any(|(existing, _)| *existing == id) { return Err(ArgError::InvalidState); }
        result.push((id, title));
    }
    if result.is_empty() { return Err(ArgError::InvalidState); }
    Ok(result)
}
fn sheet_id(sheets: &[(u32, &str)], name: Option<&str>) -> Result<u32, ArgError> {
    let Some(name) = name else { return sheets.first().map(|s| s.0).ok_or(ArgError::InvalidState); };
    let mut found = sheets.iter().filter(|(_, title)| *title == name);
    let id = found.next().ok_or(ArgError::SheetNotUnique)?.0;
    if found.next().is_some() { return Err(ArgError::SheetNotUnique); }
    Ok(id)
}

pub(crate) fn build(args: &DocsSheetsArgs, verdict: &ScopeVerdict, state: &Value) -> Result<BatchPlan, ArgError> {
    checked_scope(args, verdict)?;
    let document = is_doc(args);
    let key = if document { "documentId" } else { "spreadsheetId" };
    // SheetsGetProperties deliberately requests only sheets.properties. Its
    // response must be bound to read_call by the executor; no ID is returned.
    if (document || state.get(key).is_some()) && state[key].as_str() != Some(target(args).as_str()) {
        return Err(ArgError::InvalidState);
    }
    let mut result = BatchPlan { target: target(args).clone(), document, requests: Vec::new(), replies: Vec::new(), headers: Vec::new() };
    match args {
        DocsSheetsArgs::Insert { text, position, tab_id, .. } => {
            let content = BodyIndex::new(body(state, tab_id.as_deref())?)?;
            let position = match position {
                Position::Start => content.start, Position::End => content.end,
                Position::After(text) => content.anchor(text, false)?.1,
            };
            if position > content.end { return Err(ArgError::InvalidArgument); }
            let mut location = json!({"index":position});
            if let Some(id) = tab_id { location["tabId"] = json!(id); }
            result.requests.push(json!({"insertText":{"location":location,"text":text}}));
            result.replies.push(ReplyKind::Empty);
        }
        DocsSheetsArgs::Format { operations, .. } => {
            let content = BodyIndex::new(body(state, None)?)?;
            for op in operations {
                let (request, kind) = match op {
                    Operation::ParagraphStyle { text, named_style, alignment } => {
                        let (start, end) = content.anchor(text, true)?;
                        let mut style = json!({"namedStyleType":named_style.as_str()});
                        let fields = if let Some(alignment) = alignment {
                            style["alignment"] = json!(alignment); "namedStyleType,alignment"
                        } else { "namedStyleType" };
                        (json!({"updateParagraphStyle":{"range":{"startIndex":start,"endIndex":end},"paragraphStyle":style,"fields":fields}}), ReplyKind::Empty)
                    }
                    Operation::TextStyle { text, bold, italic, underline, font_size_pt } => {
                        let (start, end) = content.anchor(text, false)?;
                        let mut style = json!({});
                        let mut fields = Vec::new();
                        for (name, value) in [("bold", bold), ("italic", italic), ("underline", underline)] {
                            if let Some(value) = value { style[name] = json!(value); fields.push(name); }
                        }
                        if let Some(size) = font_size_pt {
                            style["fontSize"] = json!({"magnitude":size,"unit":"PT"}); fields.push("fontSize");
                        }
                        (json!({"updateTextStyle":{"range":{"startIndex":start,"endIndex":end},"textStyle":style,"fields":fields.join(",")}}), ReplyKind::Empty)
                    }
                    Operation::TableCellStyle { table_index, row, column, background_hex } => {
                        let table = content.tables.get(*table_index as usize).ok_or(ArgError::InvalidArgument)?;
                        let rows = table["table"]["tableRows"].as_array().ok_or(ArgError::InvalidState)?;
                        let cells = rows.get(*row as usize).and_then(|r| r["tableCells"].as_array()).ok_or(ArgError::InvalidArgument)?;
                        if cells.get(*column as usize).is_none() { return Err(ArgError::InvalidArgument); }
                        let digits: Vec<_> = background_hex.chars().skip(1).map(|c| c.to_digit(16).ok_or(ArgError::InvalidArgument)).collect::<Result<_, _>>()?;
                        if digits.len() != 6 { return Err(ArgError::InvalidArgument); }
                        let rgb: Vec<_> = digits.chunks_exact(2).map(|p| (p[0] * 16 + p[1]) as f64 / 255.0).collect();
                        (json!({"updateTableCellStyle":{
                            "tableRange":{"tableCellLocation":{"tableStartLocation":{"index":index(&table["startIndex"])?},"rowIndex":row,"columnIndex":column},"rowSpan":1,"columnSpan":1},
                            "tableCellStyle":{"backgroundColor":{"color":{"rgbColor":{"red":rgb[0],"green":rgb[1],"blue":rgb[2]}}}},"fields":"backgroundColor"
                        }}), ReplyKind::Empty)
                    }
                    Operation::Header { text } => {
                        if !result.headers.is_empty() { return Err(ArgError::InvalidArgument); }
                        result.headers.push((result.requests.len(), text.clone()));
                        (json!({"createHeader":{"type":"DEFAULT"}}), ReplyKind::Header)
                    }
                    Operation::AddTab { title } => (json!({"addDocumentTab":{"tabProperties":{"title":title}}}), ReplyKind::Tab),
                };
                result.requests.push(request); result.replies.push(kind);
            }
        }
        DocsSheetsArgs::AddSheet { title, copy_from_sheet, .. } => {
            let sheets = sheets(state)?;
            if sheets.iter().any(|(_, name)| *name == title) { return Err(ArgError::SheetConflict); }
            if let Some(name) = copy_from_sheet {
                result.requests.push(json!({"duplicateSheet":{"sourceSheetId":sheet_id(&sheets, Some(name))?,"newSheetName":title}}));
                result.replies.push(ReplyKind::DuplicateSheet);
            } else {
                result.requests.push(json!({"addSheet":{"properties":{"title":title}}}));
                result.replies.push(ReplyKind::AddSheet);
            }
        }
        DocsSheetsArgs::AddNote { range, note, .. } => {
            let id = sheet_id(&sheets(state)?, range.sheet.as_deref())?;
            result.requests.push(json!({"repeatCell":{"range":{
                "sheetId":id,"startRowIndex":range.start_row,"endRowIndex":range.end_row,
                "startColumnIndex":range.start_column,"endColumnIndex":range.end_column
            },"cell":{"note":note},"fields":"note"}}));
            result.replies.push(ReplyKind::Empty);
        }
    }
    // Enforce the transport body limit before returning an executable plan.
    result.phase1()?;
    Ok(result)
}

#[cfg(test)]
#[path = "program_docs_tests.rs"]
mod tests;
