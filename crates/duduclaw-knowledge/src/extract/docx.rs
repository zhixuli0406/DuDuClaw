//! Word `.docx`: paragraphs and tables of `word/document.xml`, split into
//! sections at heading paragraphs.
//!
//! A paragraph is a heading when its style (resolved through
//! `word/styles.xml`) is named `heading N` / `Title` or carries an outline
//! level, or when the paragraph itself has `w:outlineLvl`. Table rows become
//! one line each, cells joined with ` | `. Deleted revisions (`w:delText`)
//! and field codes (`w:instrText`) are not text.

use std::collections::HashMap;

use super::xml::{XEvent, attr, events, open_zip, read_part};
use super::{Budget, ExtractError, RawSegment, SegmentKind};

/// styleId → heading level (1-based).
fn heading_styles(styles_xml: &str) -> Result<HashMap<String, usize>, ExtractError> {
    let mut map = HashMap::new();
    let mut current: Option<String> = None;
    for ev in events(styles_xml)? {
        match ev {
            XEvent::Start { name, attrs } if name == "style" => {
                current = attr(&attrs, "styleId").map(str::to_string);
            }
            XEvent::End { name } if name == "style" => current = None,
            XEvent::Start { name, attrs } | XEvent::Empty { name, attrs } => {
                let Some(id) = current.clone() else { continue };
                let val = attr(&attrs, "val").unwrap_or("");
                if name == "name" {
                    let lower = val.to_ascii_lowercase();
                    if lower == "title" {
                        map.insert(id, 1);
                    } else if let Some(n) = lower
                        .strip_prefix("heading ")
                        .and_then(|n| n.trim().parse::<usize>().ok())
                    {
                        map.insert(id, n.clamp(1, 9));
                    }
                } else if name == "outlineLvl"
                    && let Ok(n) = val.parse::<usize>()
                    && n < 9
                {
                    map.entry(id).or_insert(n + 1);
                }
            }
            _ => {}
        }
    }
    Ok(map)
}

/// One output line: text and its heading level, if it is a heading.
struct Line {
    text: String,
    level: Option<usize>,
}

#[derive(Default)]
struct State {
    stack: Vec<String>,
    para: String,
    para_level: Option<usize>,
    table_depth: usize,
    cell: String,
    cells: Vec<String>,
}

fn collect_lines(doc_xml: &str, styles: &HashMap<String, usize>) -> Result<Vec<Line>, ExtractError> {
    let mut st = State::default();
    let mut lines = Vec::new();
    for ev in events(doc_xml)? {
        match ev {
            XEvent::Start { name, attrs } => {
                on_open(&mut st, &name, &attrs, styles);
                st.stack.push(name);
            }
            XEvent::Empty { name, attrs } => {
                on_open(&mut st, &name, &attrs, styles);
                on_close(&mut st, &name, &mut lines);
            }
            XEvent::End { name } => {
                st.stack.pop();
                on_close(&mut st, &name, &mut lines);
            }
            XEvent::Text(t) => {
                if st.stack.last().map(String::as_str) == Some("t") {
                    st.para.push_str(&t);
                }
            }
        }
    }
    Ok(lines)
}

fn on_open(st: &mut State, name: &str, attrs: &[(String, String)], styles: &HashMap<String, usize>) {
    match name {
        "tbl" => st.table_depth += 1,
        "tc" if st.table_depth == 1 => st.cell.clear(),
        "tr" if st.table_depth == 1 => st.cells.clear(),
        "p" => {
            st.para.clear();
            st.para_level = None;
        }
        "pStyle" => {
            if let Some(level) = attr(attrs, "val").and_then(|v| styles.get(v)) {
                st.para_level = Some(*level);
            }
        }
        "outlineLvl" if st.stack.iter().any(|s| s == "pPr") => {
            if let Some(n) = attr(attrs, "val")
                .and_then(|v| v.parse::<usize>().ok())
                .filter(|n| *n < 9)
            {
                st.para_level = Some(n + 1);
            }
        }
        "tab" | "br" | "cr" if st.stack.iter().any(|s| s == "r") => st.para.push(' '),
        _ => {}
    }
}

fn on_close(st: &mut State, name: &str, lines: &mut Vec<Line>) {
    match name {
        "p" => {
            let text = st.para.trim().to_string();
            st.para.clear();
            if st.table_depth > 0 {
                if !text.is_empty() {
                    if !st.cell.is_empty() {
                        st.cell.push(' ');
                    }
                    st.cell.push_str(&text);
                }
            } else if !text.is_empty() {
                lines.push(Line {
                    text,
                    level: st.para_level,
                });
            }
        }
        "tc" if st.table_depth == 1 => st.cells.push(std::mem::take(&mut st.cell)),
        "tr" if st.table_depth == 1 => {
            let row = super::csv::row_line(st.cells.iter().map(String::as_str));
            st.cells.clear();
            if row.chars().any(|c| !c.is_whitespace() && c != '|') {
                lines.push(Line { text: row, level: None });
            }
        }
        "tbl" => st.table_depth = st.table_depth.saturating_sub(1),
        _ => {}
    }
}

/// Group lines into sections at headings.
fn sections(lines: Vec<Line>) -> Vec<RawSegment> {
    let mut out = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut body: Vec<String> = Vec::new();
    let flush = |out: &mut Vec<RawSegment>, path: &[(usize, String)], body: &mut Vec<String>| {
        if body.is_empty() {
            return;
        }
        let mut seg = RawSegment::new(SegmentKind::Section, body.join("\n"));
        let label = path.iter().map(|(_, t)| t.as_str()).collect::<Vec<_>>().join(" > ");
        seg.label = (!label.is_empty()).then_some(label);
        out.push(seg);
        body.clear();
    };
    for line in lines {
        if let Some(level) = line.level {
            flush(&mut out, &path, &mut body);
            path.retain(|(l, _)| *l < level);
            path.push((level, duduclaw_core::truncate_chars(&line.text, 80)));
        }
        body.push(line.text);
    }
    flush(&mut out, &path, &mut body);
    out
}

/// Extract a `.docx`.
pub fn extract(bytes: &[u8], budget: &mut Budget) -> Result<Vec<RawSegment>, ExtractError> {
    let mut zip = open_zip(bytes)?;
    let doc = read_part(&mut zip, "word/document.xml")?.ok_or_else(|| ExtractError::corrupt(super::Format::Docx))?;
    let styles = match read_part(&mut zip, "word/styles.xml")? {
        Some(xml) => heading_styles(&xml)?,
        None => HashMap::new(),
    };
    let lines = collect_lines(&doc, &styles)?;
    for l in &lines {
        budget.take(&l.text)?;
    }
    Ok(sections(lines))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::fixtures;

    #[test]
    fn headings_tables_and_paragraphs() {
        let bytes = fixtures::docx(&[
            fixtures::DocxBlock::Heading(1, "第一章 總則"),
            fixtures::DocxBlock::Para("本手冊適用全體員工。"),
            fixtures::DocxBlock::Heading(2, "1.1 請假"),
            fixtures::DocxBlock::Para("特休假依年資計算，滿一年者給予七日。"),
            fixtures::DocxBlock::Table(&[&["假別", "天數"], &["病假", "30"]]),
        ]);
        let segs = extract(&bytes, &mut Budget::new(10_000)).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].label.as_deref(), Some("第一章 總則"));
        assert_eq!(segs[1].label.as_deref(), Some("第一章 總則 > 1.1 請假"));
        assert!(segs[1].text.contains("特休假依年資計算"));
        assert!(segs[1].text.contains("假別 | 天數\n病假 | 30"));
    }

    #[test]
    fn deleted_revisions_are_not_text() {
        let xml = r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>保留</w:t></w:r><w:del><w:r><w:delText>刪除</w:delText></w:r></w:del></w:p></w:body></w:document>"#;
        let lines = collect_lines(xml, &HashMap::new()).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].text, "保留");
    }
}
