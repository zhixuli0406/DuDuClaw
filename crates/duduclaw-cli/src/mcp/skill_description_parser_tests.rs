use super::parse_skill_description_from_content;

#[test]
fn extracts_description_from_well_formed_frontmatter() {
    let content = "---\nname: foo\ndescription: Does the foo thing\n---\n\n# Body";
    assert_eq!(
        parse_skill_description_from_content(content),
        "Does the foo thing"
    );
}

#[test]
fn handles_quoted_description_double_quotes() {
    let content = "---\ndescription: \"Quoted description with: colons\"\n---";
    assert_eq!(
        parse_skill_description_from_content(content),
        "Quoted description with: colons"
    );
}

#[test]
fn handles_quoted_description_single_quotes() {
    let content = "---\ndescription: 'single-quoted'\n---";
    assert_eq!(
        parse_skill_description_from_content(content),
        "single-quoted"
    );
}

#[test]
fn returns_empty_when_no_frontmatter() {
    assert_eq!(parse_skill_description_from_content("just body text"), "");
}

#[test]
fn returns_empty_when_frontmatter_unterminated() {
    // No closing `---` — guard against panics.
    let content = "---\ndescription: orphan\nname: foo\n";
    assert_eq!(parse_skill_description_from_content(content), "");
}

#[test]
fn returns_empty_when_description_key_missing() {
    let content = "---\nname: foo\nversion: 1\n---\n\nbody";
    assert_eq!(parse_skill_description_from_content(content), "");
}

#[test]
fn ignores_description_inside_body_only_reads_frontmatter() {
    // Body-side mention shouldn't be picked up.
    let content = "---\nname: foo\n---\n\nA `description: in body` should be ignored.";
    assert_eq!(parse_skill_description_from_content(content), "");
}

#[test]
fn handles_crlf_line_endings() {
    // Windows-style line endings shouldn't break parsing.
    let content = "---\r\ndescription: windows skill\r\n---\r\n\r\nbody";
    assert_eq!(
        parse_skill_description_from_content(content),
        "windows skill"
    );
}

#[test]
fn handles_extra_whitespace_around_value() {
    let content = "---\ndescription:   spaced out value   \n---";
    assert_eq!(
        parse_skill_description_from_content(content),
        "spaced out value"
    );
}

#[test]
fn handles_empty_frontmatter() {
    let content = "---\n---\nbody";
    assert_eq!(parse_skill_description_from_content(content), "");
}
