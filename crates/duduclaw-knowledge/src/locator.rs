//! Where a chunk sits in its source, and how that reads to a person.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocatorKind {
    Page,
    Slide,
    SheetRows,
    Section,
    Paragraph,
    Url,
}

impl LocatorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            LocatorKind::Page => "page",
            LocatorKind::Slide => "slide",
            LocatorKind::SheetRows => "sheet_rows",
            LocatorKind::Section => "section",
            LocatorKind::Paragraph => "paragraph",
            LocatorKind::Url => "url",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "page" => LocatorKind::Page,
            "slide" => LocatorKind::Slide,
            "sheet_rows" => LocatorKind::SheetRows,
            "section" => LocatorKind::Section,
            "paragraph" => LocatorKind::Paragraph,
            "url" => LocatorKind::Url,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Locator {
    pub kind: LocatorKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub page_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sheet: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_end: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub para_index: Option<u32>,
    /// Section heading path for `section` locators (「第二章 > 2.1 請假」).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heading: Option<String>,
}

impl Locator {
    pub fn new(kind: LocatorKind) -> Self {
        Self {
            kind,
            page_start: None,
            page_end: None,
            sheet: None,
            row_start: None,
            row_end: None,
            para_index: None,
            heading: None,
        }
    }

    /// zh-TW display: 「第 3 頁」「第 3–4 頁」「第 5 張投影片」
    /// 「工作表「報價」第 12–18 列」「「第二章 請假」」「第 7 段」「網頁」.
    pub fn display(&self) -> String {
        fn range(a: Option<u32>, b: Option<u32>) -> Option<String> {
            match (a, b) {
                (Some(a), Some(b)) if b > a => Some(format!("{a}–{b}")),
                (Some(a), _) => Some(a.to_string()),
                _ => None,
            }
        }
        match self.kind {
            LocatorKind::Page => range(self.page_start, self.page_end)
                .map(|r| format!("第 {r} 頁"))
                .unwrap_or_else(|| "文件".to_string()),
            LocatorKind::Slide => range(self.page_start, self.page_end)
                .map(|r| format!("第 {r} 張投影片"))
                .unwrap_or_else(|| "投影片".to_string()),
            LocatorKind::SheetRows => {
                let sheet = self.sheet.as_deref().unwrap_or("");
                match (sheet.is_empty(), range(self.row_start, self.row_end)) {
                    (false, Some(r)) => format!("工作表「{sheet}」第 {r} 列"),
                    (false, None) => format!("工作表「{sheet}」"),
                    // CSV: a single unnamed sheet.
                    (true, Some(r)) => format!("第 {r} 列"),
                    (true, None) => "表格".to_string(),
                }
            }
            LocatorKind::Section => match self.heading.as_deref().filter(|h| !h.is_empty()) {
                Some(h) => format!("「{}」", duduclaw_core::truncate_chars(h, 60)),
                None => self
                    .para_index
                    .map(|p| format!("第 {p} 段"))
                    .unwrap_or_else(|| "文件".to_string()),
            },
            LocatorKind::Paragraph => self
                .para_index
                .map(|p| format!("第 {p} 段"))
                .unwrap_or_else(|| "文件".to_string()),
            LocatorKind::Url => "網頁".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays() {
        let mut l = Locator::new(LocatorKind::Page);
        l.page_start = Some(3);
        assert_eq!(l.display(), "第 3 頁");
        l.page_end = Some(4);
        assert_eq!(l.display(), "第 3–4 頁");
        let mut s = Locator::new(LocatorKind::SheetRows);
        s.sheet = Some("報價".into());
        s.row_start = Some(12);
        s.row_end = Some(18);
        assert_eq!(s.display(), "工作表「報價」第 12–18 列");
        s.sheet = Some(String::new());
        assert_eq!(s.display(), "第 12–18 列");
        let mut h = Locator::new(LocatorKind::Section);
        h.heading = Some("第二章 請假".into());
        assert_eq!(h.display(), "「第二章 請假」");
        let mut sl = Locator::new(LocatorKind::Slide);
        sl.page_start = Some(5);
        assert_eq!(sl.display(), "第 5 張投影片");
        assert_eq!(Locator::new(LocatorKind::Url).display(), "網頁");
    }
}
