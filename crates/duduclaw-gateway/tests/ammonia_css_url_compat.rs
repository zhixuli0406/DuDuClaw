use std::collections::HashSet;

#[test]
fn quoted_css_url_filter_is_case_insensitive() {
    // RUSTSEC-2026-0373 requires CSS property filtering and a quoted URL.
    // Keep the dependency's security contract covered even though the canvas
    // currently relies on its iframe CSP for network isolation.
    let mut builder = ammonia::Builder::new();
    builder
        .filter_style_properties(HashSet::from(["background-image"]))
        .add_generic_attributes(&["style"])
        .url_schemes(HashSet::from(["cid"]));

    for function in ["url", "URL", "Url", "uRl"] {
        let input =
            format!("<p style='background-image:{function}(\"https://example.com/leak\")'>ok</p>");
        let out = builder.clean(&input).to_string();
        assert!(!out.contains("example.com/leak"), "got: {out}");
        assert!(out.contains("ok"), "got: {out}");
    }
}
