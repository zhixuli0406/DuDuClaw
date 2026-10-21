//! Does an imported skill depend on another vendor's tools that DuDuClaw
//! does not have (飛書 / Lark, 豆包 built-ins such as 「構建網頁應用」, Coze
//! plugins, Microsoft 365 Graph)? Such a skill is still imported, but
//! disabled with a "需要的外部能力" note.
//!
//! The answer only ever *disables* (it never enables or grants anything),
//! so an over-eager match costs an administrator one approval click while a
//! missed one would leave a skill that cannot run. Latin vendor words are
//! counted as whole tokens (so `lark` does not match `clarke`); CJK phrases
//! have no word boundaries and are counted as substrings.

/// One external capability a skill needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Need {
    pub code: &'static str,
    /// zh-TW label for reports.
    pub label: &'static str,
    /// The skill cannot do its job without it (→ disabled).
    pub hard: bool,
    pub hits: usize,
}

struct Rule {
    code: &'static str,
    label: &'static str,
    /// Any hit ⇒ hard dependency (API hosts, CLIs, tool names).
    strong: &'static [&'static str],
    /// Mentions; `weak_hard_at` of them ⇒ hard (the skill is built around it).
    weak: &'static [&'static str],
    weak_hard_at: usize,
}

const RULES: &[Rule] = &[
    Rule {
        code: "feishu",
        label: "飛書（Lark）文件、畫板與 API",
        strong: &[
            "open.feishu.cn",
            "open.larksuite.com",
            "lark-cli",
            "larksuite-cli",
            "lark-docx",
            "SKIP_LARK",
            "飛書雲文件",
            "飞书云文档",
            "飛書畫板",
            "飞书画板",
            "飛書試算表",
            "飞书表格",
        ],
        weak: &["飛書", "飞书", "feishu", "lark"],
        weak_hard_at: 5,
    },
    Rule {
        code: "doubao",
        label: "豆包內建工具（如「構建網頁應用」）",
        strong: &[
            "構建網頁應用",
            "构建网页应用",
            "app_builder",
            "豆包辦公",
            "豆包办公",
        ],
        weak: &[],
        weak_hard_at: usize::MAX,
    },
    Rule {
        code: "coze",
        label: "扣子（Coze）外掛",
        strong: &["coze.cn", "coze.com", "扣子插件", "扣子外掛"],
        weak: &["coze", "扣子"],
        weak_hard_at: 3,
    },
    Rule {
        code: "m365",
        label: "Microsoft 365（Graph、SharePoint）",
        strong: &[
            "graph.microsoft.com",
            "Microsoft Graph",
            "SharePoint",
            "Dataverse",
        ],
        weak: &[],
        weak_hard_at: usize::MAX,
    },
];

/// Every capability `texts` mention, strongest evidence first.
pub fn detect(texts: &[&str]) -> Vec<Need> {
    let mut out = Vec::new();
    for rule in RULES {
        let strong: usize = rule.strong.iter().map(|p| count(texts, p)).sum();
        let weak: usize = rule.weak.iter().map(|p| count(texts, p)).sum();
        if strong + weak == 0 {
            continue;
        }
        out.push(Need {
            code: rule.code,
            label: rule.label,
            hard: strong > 0 || weak >= rule.weak_hard_at,
            hits: strong + weak,
        });
    }
    out
}

fn count(texts: &[&str], pattern: &str) -> usize {
    if pattern.is_ascii() && pattern.chars().all(|c| c.is_ascii_alphanumeric()) {
        // Whole-token, case-insensitive (`lark` ≠ `clarke`, `Lark` = `lark`).
        texts
            .iter()
            .map(|t| {
                t.split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|tok| tok.eq_ignore_ascii_case(pattern))
                    .count()
            })
            .sum()
    } else {
        texts.iter().map(|t| t.matches(pattern).count()).sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feishu_strong_signal_is_hard() {
        let n = detect(&["用 larksuite-cli 寫入飛書畫板"]);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].code, "feishu");
        assert!(n[0].hard);
    }

    #[test]
    fn a_few_feishu_mentions_are_soft() {
        let n = detect(&["交付位置：本地文件或已獲授權的飛書文件；無法寫入飛書時交付 Markdown"]);
        assert_eq!(n.len(), 1);
        assert!(!n[0].hard, "{n:?}");
    }

    #[test]
    fn doubao_web_builder_is_hard_and_latin_tokens_are_whole_words() {
        assert!(detect(&["預設必須呼叫 `構建網頁應用`工具"])[0].hard);
        assert!(detect(&["Arthur C. Clarke wrote"]).is_empty());
        assert_eq!(detect(&["Lark and lark"])[0].hits, 2);
    }
}
