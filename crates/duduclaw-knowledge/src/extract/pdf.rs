//! PDF text layer through `pdf-extract` (lopdf underneath): one segment per
//! page, 1-based page numbers.
//!
//! - A document encrypted with a user password is refused (`encrypted`); one
//!   protected only by an owner password (empty user password) is read.
//! - A page that yields no text (a scanned page, an image) is skipped and
//!   reported in a `page_no_text` warning; a document with no text at all
//!   fails with `no_text_layer` (in [`super::extract`]).
//! - A page whose parse panics is skipped and reported (`page_unreadable`)
//!   instead of failing the whole file; this runs in the extraction child, so
//!   a crash that is not a panic still only kills that child.

use std::panic::{AssertUnwindSafe, catch_unwind};

use super::{Budget, ExtractError, RawSegment, SegmentKind, Warning};

/// Extract a PDF.
pub fn extract(bytes: &[u8], budget: &mut Budget) -> Result<(Vec<RawSegment>, Vec<Warning>), ExtractError> {
    let mut doc = match catch_unwind(|| pdf_extract::Document::load_mem(bytes)) {
        Ok(Ok(doc)) => doc,
        _ => return Err(ExtractError::corrupt(super::Format::Pdf)),
    };
    if doc.is_encrypted() && doc.decrypt("").is_err() {
        return Err(ExtractError::encrypted());
    }
    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let mut segments = Vec::new();
    let mut empty = Vec::new();
    let mut unreadable = Vec::new();
    for page in pages {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let mut text = String::new();
            let mut out = pdf_extract::PlainTextOutput::new(&mut text);
            pdf_extract::output_doc_page(&doc, &mut out, page).map(|_| text)
        }));
        match result {
            Ok(Ok(text)) if !text.trim().is_empty() => {
                budget.take(&text)?;
                let mut seg = RawSegment::new(SegmentKind::Page, text);
                seg.page = Some(page);
                segments.push(seg);
            }
            Ok(Ok(_)) => empty.push(page),
            _ => unreadable.push(page),
        }
    }
    let mut warnings = Vec::new();
    if !empty.is_empty() {
        warnings.push(Warning {
            code: "page_no_text".into(),
            pages: empty,
            count: None,
        });
    }
    if !unreadable.is_empty() {
        warnings.push(Warning {
            code: "page_unreadable".into(),
            pages: unreadable,
            count: None,
        });
    }
    Ok((segments, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extract::fixtures;

    #[test]
    fn pages_and_empty_page_warning() {
        let bytes = fixtures::pdf(&[
            "Leave policy: annual leave is seven days.",
            "",
            "Overtime pay is 1.34x.",
        ]);
        let (segs, warnings) = extract(&bytes, &mut Budget::new(10_000)).unwrap();
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].page, Some(1));
        assert!(segs[0].text.contains("annual leave"));
        assert_eq!(segs[1].page, Some(3));
        assert_eq!(warnings[0].code, "page_no_text");
        assert_eq!(warnings[0].pages, vec![2]);
    }

    #[test]
    fn not_a_pdf_is_corrupt() {
        assert_eq!(
            extract(b"hello", &mut Budget::new(10)).unwrap_err().code,
            "unsupported_format"
        );
    }

    #[test]
    fn scanned_only_pdf_has_no_text_layer() {
        let bytes = fixtures::pdf(&["", ""]);
        let err = crate::extract::extract(crate::extract::Format::Pdf, &bytes, 10_000).unwrap_err();
        assert_eq!(err.code, "no_text_layer");
    }
}
