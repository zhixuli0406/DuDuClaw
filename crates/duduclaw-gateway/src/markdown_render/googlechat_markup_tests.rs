//! Model / user text cannot become live Google Chat markup (security review
//! M2): no `<users/…>` mention and no `<url|text>` link except the ones the
//! renderer emits itself, and those only for http(s) URLs.

use super::to_googlechat_text;

fn assert_inert(output: &str) {
    assert!(!output.contains("<users/"), "live mention in {output:?}");
    assert!(!output.contains("<javascript"), "live link in {output:?}");
}

#[test]
fn mention_in_table_title_cell_heading_and_paragraph_is_inert() {
    for input in [
        "名稱 | 說明\n---|---\n<users/all> | 通知 <users/123>",
        "| 名稱 | 說明 |\n|---|---|\n| 甲 | <users/all> |",
        "名稱 | <users/all>\n---|---\n甲 | 乙",
        "# <users/all> 公告",
        "請 <users/all> 注意",
        "- <users/123> 項目",
        "> <users/all> 引用",
        "**<users/all>** 與 ~~<users/all>~~",
    ] {
        let output = to_googlechat_text(input);
        assert_inert(&output);
        assert!(output.contains("＜users/"), "{input:?} → {output:?}");
    }
    assert!(to_googlechat_text("名稱 | 說明\n---|---\n<users/all> | x").contains("*＜users/all＞*"));
    assert_eq!(to_googlechat_text("# <users/all> 公告"), "*＜users/all＞ 公告*");
}

#[test]
fn link_injection_in_cell_heading_and_paragraph_is_inert() {
    for input in [
        // Link text closing the construct early and adding a mention.
        "a | b\n---|---\nx | [點我>後面<users/all](https://a.b)",
        "# [x|<users/all>](https://a.b)",
        "看 [文件](https://a.b/<users/all>)",
        "看 [文件](https://a.b|<users/all>)",
        // Non-http schemes are never links.
        "a | b\n---|---\nx | [點我](javascript:alert)",
        "# [點我](javascript:alert)",
        "看 [點我](javascript:alert)",
    ] {
        assert_inert(&to_googlechat_text(input));
    }
    assert_eq!(
        to_googlechat_text("看 [點我](javascript:alert)"),
        "看 點我 (javascript:alert)"
    );
    assert_eq!(
        to_googlechat_text("[x>y|z<users/all>](https://a.b)"),
        "<https://a.b|x＞y｜z＜users/all＞>"
    );
    assert_eq!(to_googlechat_text("[x](mailto:a@b.c)"), "x (mailto:a@b.c)");
    assert_eq!(to_googlechat_text("[x](ftp://h/f)"), "x (ftp://h/f)");
}

#[test]
fn normal_https_link_still_renders() {
    assert_eq!(
        to_googlechat_text("[文件](https://example.com/docs?a=1)"),
        "<https://example.com/docs?a=1|文件>"
    );
    assert_eq!(to_googlechat_text("[x](http://a.b)"), "<http://a.b|x>");
    assert!(
        to_googlechat_text("a | b\n---|---\n甲 | [文件](https://example.com)")
            .contains("b: <https://example.com|文件>")
    );
    // Empty link text: a bare URL, which Chat links by itself.
    assert_eq!(to_googlechat_text("[](https://a.b)"), "https://a.b");
}

#[test]
fn ordinary_text_and_code_are_unchanged() {
    assert_eq!(to_googlechat_text("甲 | 乙\n一般文字"), "甲 | 乙\n一般文字");
    // Code spans and fences are shown literally by Chat and stay verbatim.
    assert_eq!(to_googlechat_text("`<users/all>`"), "`<users/all>`");
    assert_eq!(to_googlechat_text("```\n<users/all>\n```"), "```\n<users/all>\n```");
}
