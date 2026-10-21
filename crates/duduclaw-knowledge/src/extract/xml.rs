//! A small streaming view over `quick-xml` for the OOXML readers.
//!
//! - Element and attribute names are reduced to their local part (`w:t` →
//!   `t`), which is all the readers need.
//! - Character references and the five predefined entities are resolved;
//!   any other entity reference is dropped. Nothing external is ever fetched
//!   or expanded (quick-xml has no DTD processing).
//! - Streaming, no recursion: element depth costs a counter, not stack.

use std::io::Read;

use quick_xml::events::{BytesStart, Event};
use quick_xml::reader::Reader;

use super::ExtractError;

/// Largest single XML part read from an archive.
pub(crate) const MAX_PART_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum XEvent {
    Start {
        name: String,
        attrs: Vec<(String, String)>,
    },
    /// `<x/>`: a start immediately followed by its end.
    Empty {
        name: String,
        attrs: Vec<(String, String)>,
    },
    End {
        name: String,
    },
    Text(String),
}

fn local(name: &[u8]) -> String {
    let name = match name.iter().rposition(|&b| b == b':') {
        Some(i) => &name[i + 1..],
        None => name,
    };
    String::from_utf8_lossy(name).into_owned()
}

fn attrs(e: &BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .flatten()
        .map(|a| {
            let value = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(|v| v.into_owned())
                .unwrap_or_else(|_| String::from_utf8_lossy(&a.value).into_owned());
            (local(a.key.as_ref()), value)
        })
        .collect()
}

fn predefined(name: &str) -> Option<char> {
    Some(match name {
        "lt" => '<',
        "gt" => '>',
        "amp" => '&',
        "apos" => '\'',
        "quot" => '"',
        _ => return None,
    })
}

/// Parse a whole XML document into events.
pub(crate) fn events(xml: &str) -> Result<Vec<XEvent>, ExtractError> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    loop {
        let ev = reader
            .read_event()
            .map_err(|_| ExtractError::new("unsupported_format", "文件內容的 XML 格式有誤，檔案可能已損毀。"))?;
        match ev {
            Event::Start(e) => out.push(XEvent::Start {
                name: local(e.name().as_ref()),
                attrs: attrs(&e),
            }),
            Event::Empty(e) => out.push(XEvent::Empty {
                name: local(e.name().as_ref()),
                attrs: attrs(&e),
            }),
            Event::End(e) => out.push(XEvent::End {
                name: local(e.name().as_ref()),
            }),
            Event::Text(t) => {
                if let Ok(s) = t.decode() {
                    out.push(XEvent::Text(s.into_owned()));
                }
            }
            Event::CData(t) => {
                if let Ok(s) = t.decode() {
                    out.push(XEvent::Text(s.into_owned()));
                }
            }
            Event::GeneralRef(r) => {
                let ch = match r.resolve_char_ref() {
                    Ok(Some(c)) => Some(c),
                    _ => r.decode().ok().and_then(|n| predefined(&n)),
                };
                if let Some(c) = ch {
                    out.push(XEvent::Text(c.to_string()));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// Value of attribute `name` (local part).
pub(crate) fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
}

/// Open an archive.
pub(crate) fn open_zip(bytes: &[u8]) -> Result<zip::ZipArchive<std::io::Cursor<&[u8]>>, ExtractError> {
    zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|_| ExtractError::new("unsupported_format", "無法開啟這個 Office 檔，檔案可能已損毀。"))
}

/// Read one archive part as UTF-8 text; `Ok(None)` when it does not exist.
pub(crate) fn read_part(
    zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
) -> Result<Option<String>, ExtractError> {
    let entry = match zip.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(_) => {
            return Err(ExtractError::new(
                "unsupported_format",
                "Office 檔內容無法讀取，檔案可能已損毀。",
            ));
        }
    };
    let mut buf = Vec::new();
    entry
        .take(MAX_PART_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|_| ExtractError::new("unsupported_format", "Office 檔內容無法讀取，檔案可能已損毀。"))?;
    if buf.len() as u64 > MAX_PART_BYTES {
        return Err(ExtractError::new(
            "document_limits",
            "Office 檔內的單一部分過大，已拒絕讀取。",
        ));
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

/// Resolve a relationship target relative to the part that owns it
/// (`ppt/slides/slide1.xml` + `../notesSlides/notesSlide1.xml`).
pub(crate) fn resolve_target(owner: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut parts: Vec<&str> = owner.split('/').collect();
    parts.pop();
    for seg in target.split('/') {
        match seg {
            ".." => {
                parts.pop();
            }
            "." | "" => {}
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// `Id → (Type, Target)` from a `.rels` part.
pub(crate) fn relationships(xml: &str) -> Result<Vec<(String, String, String)>, ExtractError> {
    Ok(events(xml)?
        .into_iter()
        .filter_map(|e| match e {
            XEvent::Start { name, attrs } | XEvent::Empty { name, attrs } if name == "Relationship" => Some((
                attr(&attrs, "Id")?.to_string(),
                attr(&attrs, "Type").unwrap_or("").to_string(),
                attr(&attrs, "Target")?.to_string(),
            )),
            _ => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entities_and_names() {
        let ev = events("<w:p><w:t>A&amp;B&#x4E2D;&unknown;</w:t></w:p>").unwrap();
        let text: String = ev
            .iter()
            .filter_map(|e| {
                if let XEvent::Text(t) = e {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(text, "A&B中");
        assert!(matches!(&ev[0], XEvent::Start { name, .. } if name == "p"));
    }

    #[test]
    fn targets_resolve() {
        assert_eq!(
            resolve_target("ppt/slides/slide1.xml", "../notesSlides/notesSlide3.xml"),
            "ppt/notesSlides/notesSlide3.xml"
        );
        assert_eq!(
            resolve_target("ppt/presentation.xml", "slides/slide2.xml"),
            "ppt/slides/slide2.xml"
        );
        assert_eq!(resolve_target("a/b.xml", "/ppt/x.xml"), "ppt/x.xml");
    }
}
