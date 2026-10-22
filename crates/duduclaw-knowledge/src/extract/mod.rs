//! Document text extraction. **Runs only inside the
//! `duduclaw knowledge-extract` child process** — a parser panic, stack
//! overflow or runaway allocation then kills that child, not the gateway.
//!
//! The child prints one JSON line ([`ExtractOutput`]) and exits 0, or exits 2
//! with an [`ExtractError`] JSON line on stderr. Codes are a closed set and
//! every message is zh-TW text the uploader can act on.

use serde::{Deserialize, Serialize};

mod csv;
mod docx;
pub mod encoding;
mod html;
mod pdf;
mod pptx;
mod text;
mod xlsx;
mod xml;

/// Supported input formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Pdf,
    Docx,
    Pptx,
    Xlsx,
    Xls,
    Csv,
    Txt,
    Md,
    Html,
}

impl Format {
    pub fn as_str(self) -> &'static str {
        match self {
            Format::Pdf => "pdf",
            Format::Docx => "docx",
            Format::Pptx => "pptx",
            Format::Xlsx => "xlsx",
            Format::Xls => "xls",
            Format::Csv => "csv",
            Format::Txt => "txt",
            Format::Md => "md",
            Format::Html => "html",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "pdf" => Format::Pdf,
            "docx" => Format::Docx,
            "pptx" => Format::Pptx,
            "xlsx" => Format::Xlsx,
            "xls" => Format::Xls,
            "csv" => Format::Csv,
            "txt" => Format::Txt,
            "md" => Format::Md,
            "html" => Format::Html,
            _ => return None,
        })
    }

    /// Zip-based Office formats, which must pass the gateway's
    /// `document_limits` pre-parse gate before extraction.
    pub fn is_ooxml(self) -> bool {
        matches!(self, Format::Docx | Format::Pptx | Format::Xlsx)
    }

    /// Map a file name to a format. `Err` carries the closed-set refusal for
    /// legacy and unsupported types.
    pub fn from_file_name(name: &str) -> Result<Self, ExtractError> {
        let ext = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "pdf" => Ok(Format::Pdf),
            "docx" => Ok(Format::Docx),
            "pptx" => Ok(Format::Pptx),
            "xlsx" | "xlsm" => Ok(Format::Xlsx),
            "xls" => Ok(Format::Xls),
            "csv" => Ok(Format::Csv),
            "txt" | "text" | "log" => Ok(Format::Txt),
            "md" | "markdown" => Ok(Format::Md),
            "html" | "htm" => Ok(Format::Html),
            "doc" | "ppt" | "rtf" => Err(ExtractError::new(
                "legacy_office",
                "這是舊版 Office 格式（.doc／.ppt），目前無法讀取。請用 Word 或 PowerPoint 另存成 .docx／.pptx 後再上傳。",
            )),
            _ => Err(ExtractError::new(
                "unsupported_format",
                "不支援這種檔案格式。可以上傳 PDF（含文字層）、Word（.docx）、Excel（.xlsx／.xls）、PowerPoint（.pptx）、CSV、純文字或 Markdown。",
            )),
        }
    }
}

/// Segment kinds (one page, slide, sheet, heading section or text body).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SegmentKind {
    Page,
    Slide,
    Sheet,
    Section,
    Text,
}

impl SegmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SegmentKind::Page => "page",
            SegmentKind::Slide => "slide",
            SegmentKind::Sheet => "sheet",
            SegmentKind::Section => "section",
            SegmentKind::Text => "text",
        }
    }
}

/// One extracted segment, before normalisation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawSegment {
    pub kind: SegmentKind,
    /// Heading path (sections) or a display label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 1-based page or slide number.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    /// Sheet segments: the spreadsheet row number of every text line (the
    /// first line is the header row).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_rows: Option<Vec<u32>>,
    pub text: String,
}

impl RawSegment {
    pub fn new(kind: SegmentKind, text: String) -> Self {
        Self {
            kind,
            label: None,
            page: None,
            sheet: None,
            line_rows: None,
            text,
        }
    }
}

/// A non-fatal finding shown on the source (e.g. pages without text).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Warning {
    pub code: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

/// What the child prints on success.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractOutput {
    pub segments: Vec<RawSegment>,
    #[serde(default)]
    pub warnings: Vec<Warning>,
    pub extractor_version: String,
}

/// A closed-set extraction failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtractError {
    pub code: String,
    pub message: String,
}

impl ExtractError {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
        }
    }

    pub fn too_many_chars(max: usize) -> Self {
        Self::new(
            "too_many_chars",
            &format!("文件文字超過 {max} 字的上限，整份未匯入。請拆成較小的檔案後再上傳。"),
        )
    }

    pub fn empty_text() -> Self {
        Self::new("empty_text", "文件裡沒有可讀取的文字。")
    }

    pub fn encrypted() -> Self {
        Self::new(
            "encrypted",
            "檔案有密碼保護，無法讀取。請移除密碼後再上傳。",
        )
    }

    pub fn corrupt(format: Format) -> Self {
        Self::new(
            "unsupported_format",
            &format!(
                "無法讀取這個 {} 檔，檔案可能已損毀或不是這種格式。",
                format.as_str()
            ),
        )
    }
}

/// Running character budget shared by the format readers, so a huge document
/// is refused as soon as it crosses the ceiling instead of after it has been
/// fully materialised.
pub(crate) struct Budget {
    max: usize,
    used: usize,
}

impl Budget {
    pub(crate) fn new(max: usize) -> Self {
        Self { max, used: 0 }
    }

    pub(crate) fn take(&mut self, text: &str) -> Result<(), ExtractError> {
        self.used = self.used.saturating_add(text.chars().count());
        if self.used > self.max {
            Err(ExtractError::too_many_chars(self.max))
        } else {
            Ok(())
        }
    }
}

/// OLE compound file magic (legacy .doc/.xls/.ppt, and *encrypted* OOXML).
pub(crate) fn is_cfb(bytes: &[u8]) -> bool {
    bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1])
}

/// Extract `bytes` of `format`. `max_chars` bounds the extracted text.
pub fn extract(
    format: Format,
    bytes: &[u8],
    max_chars: usize,
) -> Result<ExtractOutput, ExtractError> {
    if format.is_ooxml() && is_cfb(bytes) {
        // An OOXML file stored as a compound file is an encrypted package.
        return Err(ExtractError::encrypted());
    }
    let mut budget = Budget::new(max_chars);
    let (segments, warnings) = match format {
        Format::Pdf => pdf::extract(bytes, &mut budget)?,
        Format::Docx => (docx::extract(bytes, &mut budget)?, Vec::new()),
        Format::Pptx => (pptx::extract(bytes, &mut budget)?, Vec::new()),
        Format::Xlsx | Format::Xls => (xlsx::extract(bytes, format, &mut budget)?, Vec::new()),
        Format::Csv => (csv::extract(bytes, "", &mut budget)?, Vec::new()),
        Format::Txt => (text::extract_plain(bytes, &mut budget)?, Vec::new()),
        Format::Md => (text::extract_markdown(bytes, &mut budget)?, Vec::new()),
        Format::Html => (html::extract(bytes, &mut budget)?, Vec::new()),
    };
    if segments.iter().all(|s| s.text.trim().is_empty()) {
        return Err(if format == Format::Pdf {
            ExtractError::new(
                "no_text_layer",
                "這份 PDF 沒有文字層（可能是掃描檔）。目前不支援 OCR，請改上傳有文字的 PDF 或 Word 檔。",
            )
        } else {
            ExtractError::empty_text()
        });
    }
    Ok(ExtractOutput {
        segments,
        warnings,
        extractor_version: crate::EXTRACTOR_VERSION.to_string(),
    })
}

#[cfg(test)]
pub(crate) mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_map_to_formats() {
        assert_eq!(Format::from_file_name("員工手冊.PDF").unwrap(), Format::Pdf);
        assert_eq!(Format::from_file_name("a.b.xlsx").unwrap(), Format::Xlsx);
        assert_eq!(
            Format::from_file_name("old.doc").unwrap_err().code,
            "legacy_office"
        );
        assert_eq!(
            Format::from_file_name("photo.png").unwrap_err().code,
            "unsupported_format"
        );
        assert_eq!(
            Format::from_file_name("noext").unwrap_err().code,
            "unsupported_format"
        );
    }

    #[test]
    fn encrypted_ooxml_is_refused() {
        let mut bytes = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
        bytes.extend_from_slice(&[0; 64]);
        assert_eq!(
            extract(Format::Docx, &bytes, 1000).unwrap_err().code,
            "encrypted"
        );
    }

    #[test]
    fn budget_refuses_whole_document() {
        let text = "字".repeat(2000);
        assert_eq!(
            extract(Format::Txt, text.as_bytes(), 1000)
                .unwrap_err()
                .code,
            "too_many_chars"
        );
    }

    #[test]
    fn empty_text_is_refused() {
        assert_eq!(
            extract(Format::Txt, b"   \n\n ", 1000).unwrap_err().code,
            "empty_text"
        );
    }
}
