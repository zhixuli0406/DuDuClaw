//! Normalise extracted segments and cut them into chunks.
//!
//! - The source's full text is its normalised segments joined by a blank
//!   line. Every segment and chunk records `char_start..char_end` in that
//!   text, counted in Unicode scalar values, and a chunk's text is exactly
//!   that slice (never a byte slice, so CJK cannot split).
//! - Chunks never cross a segment, with one exception: a PDF page ending in
//!   a fragment under [`PAGE_TAIL_MIN`] characters hands that fragment to
//!   the next page's first chunk (`page_end` then records both pages).
//! - Inside a segment, cuts prefer sentence ends (`。！？；`, a line break,
//!   `. `) near [`TARGET`] characters, within [`MIN`]..[`MAX`], with about
//!   [`OVERLAP`] characters of overlap.
//! - Spreadsheet segments are cut at row boundaries; the header row goes
//!   into every chunk's `heading_path` instead of being copied into the text.

use crate::extract::{Format, RawSegment, SegmentKind};
use crate::locator::{Locator, LocatorKind};
use crate::normalize::normalize_text;

pub const TARGET: usize = 500;
pub const MIN: usize = 200;
pub const MAX: usize = 800;
pub const OVERLAP: usize = 75;
pub const PAGE_TAIL_MIN: usize = 100;
const SEPARATOR: &str = "\n\n";

/// A normalised segment with its position in the source text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub ord: u32,
    pub kind: SegmentKind,
    pub label: Option<String>,
    pub page: Option<u32>,
    pub sheet: Option<String>,
    pub row_start: Option<u32>,
    pub row_end: Option<u32>,
    pub char_start: usize,
    pub char_end: usize,
    pub text: String,
}

/// A chunk ready to be written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkDraft {
    pub ord: u32,
    pub text: String,
    pub heading_path: String,
    pub locator: Locator,
    pub char_start: usize,
    pub char_end: usize,
    pub segment_ord: u32,
    pub text_sha256: String,
}

/// Everything the indexer writes for one source.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub segments: Vec<Segment>,
    pub chunks: Vec<ChunkDraft>,
    pub chars: usize,
    pub pages: Option<u32>,
    pub content_sha256: String,
}

/// Per-line normalisation for spreadsheets: rows stay one line each, so the
/// line → row-number mapping survives.
fn normalize_rows(text: &str) -> String {
    text.split('\n')
        .map(|l| normalize_text(l).replace('\n', " "))
        .collect::<Vec<_>>()
        .join("\n")
}

fn is_boundary_char(chars: &[char], i: usize) -> bool {
    // `i` is an exclusive end: is there a sentence end just before it?
    let c = chars[i - 1];
    match c {
        '。' | '！' | '？' | '；' | '\n' => true,
        // NFKC turns full-width ！？； into ASCII, so after those a CJK
        // character also ends the sentence; `.` needs whitespace (3.5, a.b).
        '!' | '?' | ';' => chars
            .get(i)
            .is_none_or(|n| n.is_whitespace() || !n.is_ascii_alphanumeric()),
        '.' => chars.get(i).is_none_or(|n| n.is_whitespace()),
        _ => false,
    }
}

/// Cut `chars` into `(start, end)` ranges (relative, exclusive end).
pub(crate) fn pack(chars: &[char]) -> Vec<(usize, usize)> {
    let len = chars.len();
    let boundaries: Vec<usize> = (1..=len).filter(|&i| is_boundary_char(chars, i)).collect();
    let nearest = |lo: usize, hi: usize, target: usize| -> Option<usize> {
        boundaries
            .iter()
            .copied()
            .filter(|&b| b >= lo && b <= hi)
            .min_by_key(|&b| (b.abs_diff(target), b > target))
    };
    let mut out = Vec::new();
    let mut s = 0;
    while s < len {
        if len - s <= MAX {
            out.push((s, len));
            break;
        }
        let e = nearest(s + MIN, s + MAX, s + TARGET).unwrap_or(s + TARGET);
        out.push((s, e));
        let want = e.saturating_sub(OVERLAP);
        let next = nearest(e.saturating_sub(OVERLAP * 2), e - 1, want)
            .filter(|&b| b > s)
            .unwrap_or(want);
        s = if next > s { next } else { e };
    }
    // Trim whitespace at both ends without breaking the exact-slice property.
    out.into_iter()
        .filter_map(|(mut a, mut b)| {
            while a < b && chars[a].is_whitespace() {
                a += 1;
            }
            while b > a && chars[b - 1].is_whitespace() {
                b -= 1;
            }
            (b > a).then_some((a, b))
        })
        .collect()
}

/// Normalise segments and lay them out in one text.
fn layout(raw: Vec<RawSegment>) -> (Vec<Segment>, Vec<Option<Vec<u32>>>, String) {
    let mut segments = Vec::new();
    let mut rows = Vec::new();
    let mut full = String::new();
    let mut pos = 0usize;
    for seg in raw {
        let text = if seg.kind == SegmentKind::Sheet {
            normalize_rows(&seg.text)
        } else {
            normalize_text(&seg.text)
        };
        if text.trim().is_empty() {
            continue;
        }
        if !full.is_empty() {
            full.push_str(SEPARATOR);
            pos += SEPARATOR.chars().count();
        }
        let n = text.chars().count();
        let line_rows = seg.line_rows.filter(|r| r.len() == text.split('\n').count());
        segments.push(Segment {
            ord: segments.len() as u32,
            kind: seg.kind,
            label: seg.label.map(|l| normalize_text(&l)).filter(|l| !l.is_empty()),
            page: seg.page,
            sheet: seg.sheet,
            row_start: line_rows.as_ref().and_then(|r| r.first().copied()),
            row_end: line_rows.as_ref().and_then(|r| r.last().copied()),
            char_start: pos,
            char_end: pos + n,
            text: text.clone(),
        });
        rows.push(line_rows);
        full.push_str(&text);
        pos += n;
    }
    (segments, rows, full)
}

struct Builder<'a> {
    full: &'a [char],
    line_starts: Vec<usize>,
    chunks: Vec<ChunkDraft>,
}

impl Builder<'_> {
    /// 1-based ordinal of the non-empty line containing `pos`.
    fn para_index(&self, pos: usize) -> u32 {
        let idx = self.line_starts.partition_point(|&s| s <= pos);
        idx.max(1) as u32
    }

    fn push(&mut self, start: usize, end: usize, heading: String, locator: Locator, segment_ord: u32) {
        let text: String = self.full[start..end].iter().collect();
        let text_sha256 = crate::sha256_hex(text.as_bytes());
        self.chunks.push(ChunkDraft {
            ord: self.chunks.len() as u32,
            text,
            heading_path: heading,
            locator,
            char_start: start,
            char_end: end,
            segment_ord,
            text_sha256,
        });
    }
}

fn line_starts(full: &[char]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut at_line_start = true;
    for (i, c) in full.iter().enumerate() {
        if *c == '\n' {
            at_line_start = true;
        } else if at_line_start {
            starts.push(i);
            at_line_start = false;
        }
    }
    starts
}

fn sheet_chunks(b: &mut Builder<'_>, seg: &Segment, rows: Option<&Vec<u32>>) {
    let chars: Vec<char> = seg.text.chars().collect();
    // Line ranges relative to the segment.
    let mut lines = Vec::new();
    let mut s = 0;
    for (i, c) in chars.iter().enumerate() {
        if *c == '\n' {
            lines.push((s, i));
            s = i + 1;
        }
    }
    lines.push((s, chars.len()));
    let header: String = chars[lines[0].0..lines[0].1].iter().collect();
    let sheet = seg.sheet.clone().unwrap_or_default();
    let heading = if sheet.is_empty() {
        duduclaw_core::truncate_chars(&header, 300)
    } else {
        duduclaw_core::truncate_chars(&format!("{sheet} | {header}"), 300)
    };
    let row_of = |i: usize| rows.and_then(|r| r.get(i).copied());
    let locator = |a: usize, z: usize| {
        let mut l = Locator::new(LocatorKind::SheetRows);
        l.sheet = Some(sheet.clone());
        l.row_start = row_of(a);
        l.row_end = row_of(z);
        l
    };
    if lines.len() == 1 {
        b.push(seg.char_start, seg.char_end, heading, locator(0, 0), seg.ord);
        return;
    }
    let mut group: Option<(usize, usize)> = None; // line indices
    for i in 1..lines.len() {
        let (ls, le) = lines[i];
        if le - ls > MAX {
            if let Some((a, z)) = group.take() {
                b.push(
                    seg.char_start + lines[a].0,
                    seg.char_start + lines[z].1,
                    heading.clone(),
                    locator(a, z),
                    seg.ord,
                );
            }
            let mut p = ls;
            while p < le {
                let q = (p + MAX).min(le);
                b.push(
                    seg.char_start + p,
                    seg.char_start + q,
                    heading.clone(),
                    locator(i, i),
                    seg.ord,
                );
                p = q;
            }
            continue;
        }
        group = match group {
            Some((a, z)) if le - lines[a].0 <= TARGET => Some((a, i.max(z))),
            Some((a, z)) => {
                b.push(
                    seg.char_start + lines[a].0,
                    seg.char_start + lines[z].1,
                    heading.clone(),
                    locator(a, z),
                    seg.ord,
                );
                Some((i, i))
            }
            None => Some((i, i)),
        };
    }
    if let Some((a, z)) = group {
        b.push(
            seg.char_start + lines[a].0,
            seg.char_start + lines[z].1,
            heading,
            locator(a, z),
            seg.ord,
        );
    }
}

fn text_locator(format: Format, seg: &Segment, b: &Builder<'_>, start: usize) -> (Locator, String) {
    let heading = seg.label.clone().unwrap_or_default();
    let mut l = match (format, seg.kind) {
        (_, SegmentKind::Slide) => {
            let mut l = Locator::new(LocatorKind::Slide);
            l.page_start = seg.page;
            l
        }
        (Format::Html, _) => Locator::new(LocatorKind::Url),
        (_, SegmentKind::Section) if !heading.is_empty() => Locator::new(LocatorKind::Section),
        _ => Locator::new(LocatorKind::Paragraph),
    };
    if l.kind != LocatorKind::Slide {
        l.para_index = Some(b.para_index(start));
    }
    if !heading.is_empty() {
        l.heading = Some(heading.clone());
    }
    (l, heading)
}

/// Normalise, lay out and chunk one source.
pub fn prepare(format: Format, raw: Vec<RawSegment>) -> Prepared {
    let (segments, rows, full_text) = layout(raw);
    let full: Vec<char> = full_text.chars().collect();
    let mut b = Builder {
        full: &full,
        line_starts: line_starts(&full),
        chunks: Vec::new(),
    };
    // A PDF page's short tail waiting to join the next page's first chunk.
    let mut carry: Option<(usize, u32)> = None;
    for (i, seg) in segments.iter().enumerate() {
        match seg.kind {
            SegmentKind::Sheet => sheet_chunks(&mut b, seg, rows[i].as_ref()),
            SegmentKind::Page => {
                let chars = &full[seg.char_start..seg.char_end];
                let mut ranges = pack(chars);
                let next_is_page = segments.get(i + 1).is_some_and(|n| n.kind == SegmentKind::Page);
                let tail = ranges
                    .last()
                    .copied()
                    .filter(|(a, z)| z - a < PAGE_TAIL_MIN && next_is_page);
                if tail.is_some() {
                    ranges.pop();
                }
                for (k, (a, z)) in ranges.iter().enumerate() {
                    let (mut start, mut page_start) = (seg.char_start + a, seg.page);
                    if k == 0
                        && let Some((cs, cp)) = carry.take()
                    {
                        start = cs;
                        page_start = Some(cp);
                    }
                    let mut l = Locator::new(LocatorKind::Page);
                    l.page_start = page_start;
                    l.page_end = seg.page.filter(|p| Some(*p) != page_start);
                    b.push(start, seg.char_start + z, String::new(), l, seg.ord);
                }
                if let Some((a, _)) = tail {
                    // Keep the earliest start when several tiny pages chain.
                    let start = carry.map(|(cs, _)| cs).unwrap_or(seg.char_start + a);
                    let page = carry.map(|(_, p)| p).or(seg.page).unwrap_or(0);
                    carry = Some((start, page));
                }
            }
            _ => {
                let chars = &full[seg.char_start..seg.char_end];
                for (a, z) in pack(chars) {
                    let (locator, heading) = text_locator(format, seg, &b, seg.char_start + a);
                    b.push(seg.char_start + a, seg.char_start + z, heading, locator, seg.ord);
                }
            }
        }
    }
    // A carried tail with no following page chunk (cannot normally happen,
    // because a tail is only carried when another page follows).
    if let Some((start, page)) = carry {
        let end = segments.last().map(|s| s.char_end).unwrap_or(start);
        let mut l = Locator::new(LocatorKind::Page);
        l.page_start = Some(page);
        b.push(
            start,
            end,
            String::new(),
            l,
            segments.last().map(|s| s.ord).unwrap_or(0),
        );
    }
    let pages = segments
        .iter()
        .filter(|s| matches!(s.kind, SegmentKind::Page | SegmentKind::Slide))
        .filter_map(|s| s.page)
        .max();
    let content_sha256 = crate::sha256_hex(full_text.as_bytes());
    let chunks = b.chunks;
    Prepared {
        chars: full.len(),
        segments,
        chunks,
        pages,
        content_sha256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(kind: SegmentKind, text: &str) -> RawSegment {
        RawSegment::new(kind, text.to_string())
    }

    fn assert_exact_slices(p: &Prepared) {
        let full: String = p
            .segments
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(SEPARATOR);
        let chars: Vec<char> = full.chars().collect();
        assert_eq!(chars.len(), p.chars);
        for c in &p.chunks {
            let slice: String = chars[c.char_start..c.char_end].iter().collect();
            assert_eq!(slice, c.text, "chunk {} is not its slice", c.ord);
        }
        for s in &p.segments {
            let slice: String = chars[s.char_start..s.char_end].iter().collect();
            assert_eq!(slice, s.text);
        }
    }

    #[test]
    fn long_cjk_text_is_cut_on_sentences_with_overlap() {
        let sentence = "員工到職滿一年後依勞基法給予特別休假七日，請於系統申請。";
        let text = sentence.repeat(60);
        let p = prepare(Format::Txt, vec![seg(SegmentKind::Text, &text)]);
        assert!(p.chunks.len() > 2);
        assert_exact_slices(&p);
        for w in p.chunks.windows(2) {
            assert!(w[1].char_start < w[0].char_end, "consecutive chunks overlap");
            assert!(w[0].text.ends_with('。'), "cut at a sentence end: {}", w[0].text);
        }
        for c in &p.chunks {
            let n = c.text.chars().count();
            assert!(n <= MAX, "{n}");
        }
    }

    #[test]
    fn full_width_marks_still_end_sentences_after_nfkc() {
        let text = "請問特休怎麼算？依年資計算！".repeat(80);
        let p = prepare(Format::Txt, vec![seg(SegmentKind::Text, &text)]);
        assert_exact_slices(&p);
        assert!(p.chunks.len() > 1);
        for c in &p.chunks[..p.chunks.len() - 1] {
            assert!(c.text.ends_with('?') || c.text.ends_with('!'), "{}", c.text);
        }
    }

    #[test]
    fn text_without_boundaries_is_hard_cut_on_chars() {
        let text = "字".repeat(2000);
        let p = prepare(Format::Txt, vec![seg(SegmentKind::Text, &text)]);
        assert_exact_slices(&p);
        assert!(p.chunks.iter().all(|c| c.text.chars().all(|ch| ch == '字')));
    }

    #[test]
    fn pdf_chunks_keep_pages_and_carry_short_tails() {
        let mut p1 = seg(SegmentKind::Page, "第一頁很短的頁尾。");
        p1.page = Some(1);
        let mut p2 = seg(SegmentKind::Page, &"第二頁內容。".repeat(30));
        p2.page = Some(2);
        let mut p3 = seg(SegmentKind::Page, "最後一頁。");
        p3.page = Some(3);
        let p = prepare(Format::Pdf, vec![p1, p2, p3]);
        assert_exact_slices(&p);
        let first = &p.chunks[0];
        assert_eq!(first.locator.page_start, Some(1));
        assert_eq!(first.locator.page_end, Some(2));
        let last = p.chunks.last().unwrap();
        assert_eq!(last.locator.page_start, Some(3));
        assert_eq!(last.locator.page_end, None);
        assert_eq!(p.pages, Some(3));
    }

    #[test]
    fn sheet_rows_map_to_row_numbers_and_header_goes_to_heading() {
        let mut s = seg(SegmentKind::Sheet, "品號 | 單價\nA-1 | 10\nA-2 | 20\nA-3 | 30");
        s.sheet = Some("報價".into());
        s.line_rows = Some(vec![1, 3, 4, 9]);
        let p = prepare(Format::Xlsx, vec![s]);
        assert_exact_slices(&p);
        assert_eq!(p.chunks.len(), 1);
        let c = &p.chunks[0];
        assert_eq!(c.heading_path, "報價 | 品號 | 單價");
        assert_eq!(c.locator.row_start, Some(3));
        assert_eq!(c.locator.row_end, Some(9));
        assert!(!c.text.contains("品號"));
    }

    #[test]
    fn many_rows_split_into_groups() {
        let mut lines = vec!["品號 | 說明".to_string()];
        for i in 0..100 {
            lines.push(format!("P-{i:03} | 這是第 {i} 項產品的說明文字"));
        }
        let mut s = seg(SegmentKind::Sheet, &lines.join("\n"));
        s.sheet = Some("產品".into());
        s.line_rows = Some((1..=101).collect());
        let p = prepare(Format::Xlsx, vec![s]);
        assert_exact_slices(&p);
        assert!(p.chunks.len() > 3);
        assert_eq!(p.chunks[0].locator.row_start, Some(2));
        let last = p.chunks.last().unwrap();
        assert_eq!(last.locator.row_end, Some(101));
    }

    #[test]
    fn section_locator_and_paragraph_index() {
        let mut a = seg(SegmentKind::Section, "第一章\n總則內容");
        a.label = Some("第一章".into());
        let b = seg(SegmentKind::Text, "前言一\n前言二");
        let p = prepare(Format::Docx, vec![b, a]);
        assert_exact_slices(&p);
        assert_eq!(p.chunks[0].locator.kind, LocatorKind::Paragraph);
        assert_eq!(p.chunks[0].locator.para_index, Some(1));
        assert_eq!(p.chunks[1].locator.kind, LocatorKind::Section);
        assert_eq!(p.chunks[1].locator.para_index, Some(3));
        assert_eq!(p.chunks[1].heading_path, "第一章");
    }

    #[test]
    fn empty_segments_are_dropped_and_hash_is_stable() {
        let p1 = prepare(
            Format::Txt,
            vec![seg(SegmentKind::Text, "  \n"), seg(SegmentKind::Text, "內容")],
        );
        let p2 = prepare(Format::Txt, vec![seg(SegmentKind::Text, "內容")]);
        assert_eq!(p1.segments.len(), 1);
        assert_eq!(p1.content_sha256, p2.content_sha256);
    }
}
