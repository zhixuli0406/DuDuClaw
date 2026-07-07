//! Markdown → platform-native message rendering.
//!
//! LLM replies arrive as standard (GitHub-flavored) markdown. Each chat
//! platform renders a different subset with different syntax:
//!
//! | Platform    | Bold        | Code block         | Table             | Headers   |
//! |-------------|-------------|--------------------|-------------------|-----------|
//! | Telegram    | HTML `<b>`  | `<pre><code>`      | none → `<pre>`    | none → `<b>` |
//! | Discord     | `**x**`     | native fence       | none → fence      | native `#` |
//! | Slack       | markdown block (native) or mrkdwn `*x*` | native | native (markdown block) | bold |
//! | WhatsApp    | `*x*`       | ``` fence          | none → fence      | none → `*x*` |
//! | LINE        | none (plain)| none               | none → key-value  | none → 【x】 |
//! | Feishu      | Card 2.0 markdown (near-CommonMark) | native | native | native |
//! | Google Chat | `*x*`       | ``` fence          | none → fence      | none → `*x*` |
//! | MS Teams    | markdown text (no tables) / Adaptive Card | fence | none → fence | none → `**x**` |
//!
//! This module parses markdown into a small block IR once, then renders it
//! per-platform. All text handling is CJK-safe (no raw byte slicing;
//! display-width aware table alignment).

// ── Block IR ───────────────────────────────────────────────────

/// A block-level markdown element.
#[derive(Debug, Clone, PartialEq)]
pub enum MdBlock {
    /// Contiguous non-special lines (lists stay inside paragraphs;
    /// inline markdown is kept raw and converted at render time).
    Paragraph(String),
    /// `# Heading` (level 1-6).
    Heading { level: u8, text: String },
    /// Fenced code block. `lang` may be empty.
    CodeFence { lang: String, code: String },
    /// GFM pipe table.
    Table { headers: Vec<String>, rows: Vec<Vec<String>> },
    /// Contiguous `>` block quote (content lines joined with '\n').
    Quote(String),
    /// Horizontal rule.
    Divider,
}

/// Parse markdown text into block IR. Never fails — unrecognised input
/// degrades to `Paragraph`.
pub fn parse_markdown_blocks(text: &str) -> Vec<MdBlock> {
    let lines: Vec<&str> = text.lines().collect();
    let mut blocks: Vec<MdBlock> = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    let mut i = 0;

    fn flush_para(para: &mut Vec<&str>, blocks: &mut Vec<MdBlock>) {
        // Trim leading/trailing blank lines inside the paragraph run.
        while para.first().is_some_and(|l| l.trim().is_empty()) {
            para.remove(0);
        }
        while para.last().is_some_and(|l| l.trim().is_empty()) {
            para.pop();
        }
        if !para.is_empty() {
            blocks.push(MdBlock::Paragraph(para.join("\n")));
            para.clear();
        }
    }

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim_start();

        // ── Fenced code block ──
        if let Some(rest) = trimmed.strip_prefix("```") {
            flush_para(&mut para, &mut blocks);
            let lang = rest.trim().to_string();
            let mut code_lines: Vec<&str> = Vec::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                code_lines.push(lines[i]);
                i += 1;
            }
            i += 1; // skip closing fence (or run past EOF)
            blocks.push(MdBlock::CodeFence { lang, code: code_lines.join("\n") });
            continue;
        }

        // ── Table (header row + separator row) ──
        if trimmed.starts_with('|')
            && i + 1 < lines.len()
            && is_table_separator(lines[i + 1])
        {
            flush_para(&mut para, &mut blocks);
            let headers = split_table_row(trimmed);
            i += 2; // skip header + separator
            let mut rows: Vec<Vec<String>> = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                rows.push(split_table_row(lines[i].trim_start()));
                i += 1;
            }
            blocks.push(MdBlock::Table { headers, rows });
            continue;
        }

        // ── Heading ──
        if trimmed.starts_with('#') {
            let level = trimmed.chars().take_while(|&c| c == '#').count();
            if (1..=6).contains(&level) {
                if let Some(text) = trimmed[level..].strip_prefix(' ') {
                    flush_para(&mut para, &mut blocks);
                    blocks.push(MdBlock::Heading {
                        level: level as u8,
                        text: text.trim().to_string(),
                    });
                    i += 1;
                    continue;
                }
            }
        }

        // ── Divider ──
        {
            let t = trimmed.trim_end();
            if t.len() >= 3
                && (t.chars().all(|c| c == '-')
                    || t.chars().all(|c| c == '*')
                    || t.chars().all(|c| c == '_'))
            {
                flush_para(&mut para, &mut blocks);
                blocks.push(MdBlock::Divider);
                i += 1;
                continue;
            }
        }

        // ── Block quote (contiguous run) ──
        if trimmed.starts_with('>') {
            flush_para(&mut para, &mut blocks);
            let mut quote_lines: Vec<String> = Vec::new();
            while i < lines.len() {
                let t = lines[i].trim_start();
                if let Some(rest) = t.strip_prefix('>') {
                    quote_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                    i += 1;
                } else {
                    break;
                }
            }
            blocks.push(MdBlock::Quote(quote_lines.join("\n")));
            continue;
        }

        // ── Blank line: paragraph boundary ──
        if trimmed.is_empty() {
            flush_para(&mut para, &mut blocks);
            i += 1;
            continue;
        }

        para.push(line);
        i += 1;
    }
    flush_para(&mut para, &mut blocks);
    blocks
}

/// Is this line a GFM table separator (`| --- | :---: |`)?
fn is_table_separator(line: &str) -> bool {
    let t = line.trim();
    if !t.starts_with('|') && !t.contains('|') {
        return false;
    }
    let mut saw_dash = false;
    for c in t.chars() {
        match c {
            '|' | ':' | '-' | ' ' => {
                if c == '-' {
                    saw_dash = true;
                }
            }
            _ => return false,
        }
    }
    saw_dash
}

/// Split a `| a | b |` row into trimmed cells.
fn split_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    // Split on unescaped pipes.
    let mut cells: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            cur.push('|');
            chars.next();
        } else if c == '|' {
            cells.push(cur.trim().to_string());
            cur = String::new();
        } else {
            cur.push(c);
        }
    }
    cells.push(cur.trim().to_string());
    cells
}

// ── Display width (CJK-aware) ──────────────────────────────────

/// Terminal/monospace display width of a char (CJK & fullwidth = 2).
fn char_width(c: char) -> usize {
    let cp = c as u32;
    // Common wide ranges: CJK, Hangul, fullwidth forms, kana, emoji.
    if (0x1100..=0x115F).contains(&cp)         // Hangul Jamo
        || (0x2E80..=0x303E).contains(&cp)     // CJK radicals, punctuation
        || (0x3041..=0x33FF).contains(&cp)     // Kana, CJK symbols
        || (0x3400..=0x4DBF).contains(&cp)     // CJK ext A
        || (0x4E00..=0x9FFF).contains(&cp)     // CJK unified
        || (0xA000..=0xA4CF).contains(&cp)     // Yi
        || (0xAC00..=0xD7A3).contains(&cp)     // Hangul syllables
        || (0xF900..=0xFAFF).contains(&cp)     // CJK compat
        || (0xFE30..=0xFE4F).contains(&cp)     // CJK compat forms
        || (0xFF00..=0xFF60).contains(&cp)     // Fullwidth forms
        || (0xFFE0..=0xFFE6).contains(&cp)
        || (0x1F300..=0x1FAFF).contains(&cp)   // Emoji
        || (0x20000..=0x3FFFD).contains(&cp)   // CJK ext B+
    {
        2
    } else {
        1
    }
}

/// Display width of a string in monospace cells (CJK-aware).
pub fn display_width(s: &str) -> usize {
    s.chars().map(char_width).sum()
}

/// Truncate to at most `max_cells` display cells, appending `…` if cut.
fn truncate_display(s: &str, max_cells: usize) -> String {
    if display_width(s) <= max_cells {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = char_width(c);
        if w + cw > max_cells.saturating_sub(1) {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

// ── Table renderers ────────────────────────────────────────────

/// Max display cells per table column (monospace rendering).
const TABLE_COL_CAP: usize = 28;

/// Render a table as aligned monospace text (for code-block contexts:
/// Telegram `<pre>`, Discord/WhatsApp ``` fences).
pub fn render_table_monospace(headers: &[String], rows: &[Vec<String>]) -> String {
    let ncols = headers.len().max(rows.iter().map(|r| r.len()).max().unwrap_or(0));
    if ncols == 0 {
        return String::new();
    }
    let cell = |row: &[String], i: usize| -> String {
        truncate_display(row.get(i).map(|s| s.as_str()).unwrap_or(""), TABLE_COL_CAP)
    };
    // Column widths.
    let mut widths = vec![0usize; ncols];
    for (i, w) in widths.iter_mut().enumerate() {
        *w = display_width(&cell(headers, i));
        for r in rows {
            *w = (*w).max(display_width(&cell(r, i)));
        }
    }
    let render_row = |row: &[String]| -> String {
        let mut line = String::new();
        for (i, w) in widths.iter().enumerate() {
            if i > 0 {
                line.push_str(" | ");
            }
            let c = cell(row, i);
            line.push_str(&c);
            // Pad with spaces to column width (except last column).
            if i + 1 < ncols {
                for _ in display_width(&c)..*w {
                    line.push(' ');
                }
            }
        }
        line.trim_end().to_string()
    };

    let mut out = render_row(headers);
    out.push('\n');
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            out.push_str("-+-");
        }
        out.push_str(&"-".repeat(*w));
    }
    for r in rows {
        out.push('\n');
        out.push_str(&render_row(r));
    }
    out
}

/// Render a table as a key-value record list (for proportional-font
/// contexts like LINE where column alignment is impossible).
pub fn render_table_kv(headers: &[String], rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    for (ri, row) in rows.iter().enumerate() {
        if ri > 0 {
            out.push('\n');
        }
        // First cell as the record title when present.
        let title = row.first().map(|s| s.as_str()).unwrap_or("");
        let first_header = headers.first().map(|s| s.as_str()).unwrap_or("");
        if title.is_empty() {
            out.push_str(&format!("▸ #{}", ri + 1));
        } else if first_header.is_empty() {
            out.push_str(&format!("▸ {title}"));
        } else {
            out.push_str(&format!("▸ {first_header}: {title}"));
        }
        for (ci, val) in row.iter().enumerate().skip(1) {
            if val.trim().is_empty() {
                continue;
            }
            let key = headers.get(ci).map(|s| s.as_str()).unwrap_or("");
            out.push('\n');
            if key.is_empty() {
                out.push_str(&format!("　• {val}"));
            } else {
                out.push_str(&format!("　• {key}: {val}"));
            }
        }
    }
    if rows.is_empty() {
        out = headers.join(" / ");
    }
    out
}

// ── Inline conversion ──────────────────────────────────────────

/// Inline markdown target dialects.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InlineTarget {
    /// Telegram HTML parse mode (`<b>`, `<i>`, `<code>`, `<a>`), with
    /// `&`/`<`/`>` escaped.
    TelegramHtml,
    /// WhatsApp: `*bold*`, `~strike~`, plain `text (url)` links.
    WhatsApp,
    /// Google Chat markup: `*bold*`, `~strike~`, `<url|text>` links.
    GoogleChat,
    /// Plain text (LINE): emphasis stripped, `text (url)` links.
    Plain,
}

/// Escape Telegram-HTML special chars.
pub fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Convert inline markdown (`**bold**`, `~~strike~~`, `` `code` ``,
/// `[text](url)`) in a single line/paragraph to the target dialect.
/// Code spans are preserved verbatim (with target-appropriate wrapping).
pub fn convert_inline(text: &str, target: InlineTarget) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;

    // Push plain text with target escaping.
    let push_text = |out: &mut String, s: &str| match target {
        InlineTarget::TelegramHtml => out.push_str(&escape_html(s)),
        _ => out.push_str(s),
    };

    while i < chars.len() {
        let c = chars[i];

        // ── Inline code span ──
        if c == '`' {
            if let Some(close) = find_char(&chars, i + 1, '`') {
                let code: String = chars[i + 1..close].iter().collect();
                match target {
                    InlineTarget::TelegramHtml => {
                        out.push_str("<code>");
                        out.push_str(&escape_html(&code));
                        out.push_str("</code>");
                    }
                    InlineTarget::WhatsApp | InlineTarget::GoogleChat => {
                        out.push('`');
                        out.push_str(&code);
                        out.push('`');
                    }
                    InlineTarget::Plain => out.push_str(&code),
                }
                i = close + 1;
                continue;
            }
        }

        // ── Link [text](url) ──
        if c == '[' {
            if let Some((text_part, url, next)) = parse_link(&chars, i) {
                match target {
                    InlineTarget::TelegramHtml => {
                        out.push_str(&format!(
                            "<a href=\"{}\">{}</a>",
                            escape_html(&url).replace('"', "&quot;"),
                            escape_html(&text_part)
                        ));
                    }
                    InlineTarget::GoogleChat => {
                        out.push_str(&format!("<{url}|{text_part}>"));
                    }
                    InlineTarget::WhatsApp | InlineTarget::Plain => {
                        if text_part == url {
                            out.push_str(&url);
                        } else {
                            out.push_str(&format!("{text_part} ({url})"));
                        }
                    }
                }
                i = next;
                continue;
            }
        }

        // ── Bold **x** / __x__ ──
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            if let Some(close) = find_pair(&chars, i + 2, c) {
                let inner: String = chars[i + 2..close].iter().collect();
                let inner = convert_inline(&inner, target);
                match target {
                    InlineTarget::TelegramHtml => {
                        out.push_str("<b>");
                        out.push_str(&inner);
                        out.push_str("</b>");
                    }
                    InlineTarget::WhatsApp | InlineTarget::GoogleChat => {
                        out.push('*');
                        out.push_str(&inner);
                        out.push('*');
                    }
                    InlineTarget::Plain => out.push_str(&inner),
                }
                i = close + 2;
                continue;
            }
        }

        // ── Strikethrough ~~x~~ ──
        if c == '~' && i + 1 < chars.len() && chars[i + 1] == '~' {
            if let Some(close) = find_pair(&chars, i + 2, '~') {
                let inner: String = chars[i + 2..close].iter().collect();
                let inner = convert_inline(&inner, target);
                match target {
                    InlineTarget::TelegramHtml => {
                        out.push_str("<s>");
                        out.push_str(&inner);
                        out.push_str("</s>");
                    }
                    InlineTarget::WhatsApp | InlineTarget::GoogleChat => {
                        out.push('~');
                        out.push_str(&inner);
                        out.push('~');
                    }
                    InlineTarget::Plain => out.push_str(&inner),
                }
                i = close + 2;
                continue;
            }
        }

        push_text(&mut out, &c.to_string());
        i += 1;
    }
    out
}

/// Find the next `needle` at or after `from`; returns its index.
fn find_char(chars: &[char], from: usize, needle: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == needle)
}

/// Find a double-`needle` closer (`**`, `__`, `~~`) at or after `from`;
/// returns the index of the first char of the pair.
fn find_pair(chars: &[char], from: usize, needle: char) -> Option<usize> {
    let mut j = from;
    while j + 1 < chars.len() {
        if chars[j] == needle && chars[j + 1] == needle {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Try to parse `[text](url)` at position `open` (which must be `[`).
/// Returns (text, url, index-after-`)`).
fn parse_link(chars: &[char], open: usize) -> Option<(String, String, usize)> {
    let close_bracket = find_char(chars, open + 1, ']')?;
    if close_bracket + 1 >= chars.len() || chars[close_bracket + 1] != '(' {
        return None;
    }
    let close_paren = find_char(chars, close_bracket + 2, ')')?;
    let text: String = chars[open + 1..close_bracket].iter().collect();
    let url: String = chars[close_bracket + 2..close_paren].iter().collect();
    // Reject nested-bracket false positives and empty URLs.
    if text.contains('[') || url.trim().is_empty() || url.contains(' ') {
        return None;
    }
    Some((text, url.trim().to_string(), close_paren + 1))
}

// ── Per-platform renderers ─────────────────────────────────────

/// Map markdown list markers (`- ` / `* `) to a nicer bullet for
/// plain-text-ish targets. Preserves indentation.
fn prettify_bullets(line: &str) -> String {
    let indent_len = line.len() - line.trim_start().len();
    let (indent, rest) = line.split_at(indent_len);
    if let Some(item) = rest.strip_prefix("- ").or_else(|| rest.strip_prefix("* ")) {
        format!("{indent}• {item}")
    } else {
        line.to_string()
    }
}

/// Render markdown to Telegram HTML (parse_mode=HTML).
///
/// Uses only entities in the Bot API HTML subset: `<b> <i> <s> <code>
/// <pre> <a> <blockquote>`; everything else is escaped text.
pub fn to_telegram_html(text: &str) -> String {
    let blocks = parse_markdown_blocks(text);
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            MdBlock::Paragraph(p) => {
                let lines: Vec<String> = p
                    .lines()
                    .map(|l| convert_inline(&prettify_bullets(l), InlineTarget::TelegramHtml))
                    .collect();
                parts.push(lines.join("\n"));
            }
            MdBlock::Heading { text, .. } => {
                parts.push(format!("<b>{}</b>", convert_inline(&text, InlineTarget::TelegramHtml)));
            }
            MdBlock::CodeFence { lang, code } => {
                if lang.is_empty() {
                    parts.push(format!("<pre>{}</pre>", escape_html(&code)));
                } else {
                    parts.push(format!(
                        "<pre><code class=\"language-{}\">{}</code></pre>",
                        escape_html(&lang),
                        escape_html(&code)
                    ));
                }
            }
            MdBlock::Table { headers, rows } => {
                parts.push(format!(
                    "<pre>{}</pre>",
                    escape_html(&render_table_monospace(&headers, &rows))
                ));
            }
            MdBlock::Quote(q) => {
                // Expandable blockquote for long quotes keeps messages compact.
                let tag = if q.lines().count() > 3 { "<blockquote expandable>" } else { "<blockquote>" };
                parts.push(format!(
                    "{tag}{}</blockquote>",
                    convert_inline(&q, InlineTarget::TelegramHtml)
                ));
            }
            MdBlock::Divider => parts.push("———".to_string()),
        }
    }
    parts.join("\n\n")
}

/// Render markdown to WhatsApp text formatting.
pub fn to_whatsapp_text(text: &str) -> String {
    let blocks = parse_markdown_blocks(text);
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            MdBlock::Paragraph(p) => {
                let lines: Vec<String> = p
                    .lines()
                    .map(|l| convert_inline(l, InlineTarget::WhatsApp))
                    .collect();
                parts.push(lines.join("\n"));
            }
            MdBlock::Heading { text, .. } => {
                parts.push(format!("*{}*", convert_inline(&text, InlineTarget::WhatsApp)));
            }
            MdBlock::CodeFence { code, .. } => parts.push(format!("```\n{code}\n```")),
            MdBlock::Table { headers, rows } => {
                parts.push(format!("```\n{}\n```", render_table_monospace(&headers, &rows)));
            }
            MdBlock::Quote(q) => {
                let quoted: Vec<String> = q
                    .lines()
                    .map(|l| format!("> {}", convert_inline(l, InlineTarget::WhatsApp)))
                    .collect();
                parts.push(quoted.join("\n"));
            }
            MdBlock::Divider => parts.push("────────".to_string()),
        }
    }
    parts.join("\n\n")
}

/// Render markdown to Google Chat text markup.
pub fn to_googlechat_text(text: &str) -> String {
    let blocks = parse_markdown_blocks(text);
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            MdBlock::Paragraph(p) => {
                let lines: Vec<String> = p
                    .lines()
                    .map(|l| convert_inline(l, InlineTarget::GoogleChat))
                    .collect();
                parts.push(lines.join("\n"));
            }
            MdBlock::Heading { text, .. } => {
                parts.push(format!("*{}*", convert_inline(&text, InlineTarget::GoogleChat)));
            }
            MdBlock::CodeFence { code, .. } => parts.push(format!("```\n{code}\n```")),
            MdBlock::Table { headers, rows } => {
                parts.push(format!("```\n{}\n```", render_table_monospace(&headers, &rows)));
            }
            MdBlock::Quote(q) => {
                let quoted: Vec<String> = q
                    .lines()
                    .map(|l| format!("> {}", convert_inline(l, InlineTarget::GoogleChat)))
                    .collect();
                parts.push(quoted.join("\n"));
            }
            MdBlock::Divider => parts.push("────────".to_string()),
        }
    }
    parts.join("\n\n")
}

/// Render markdown to LINE-friendly plain text (no markup support at all).
pub fn to_line_plain(text: &str) -> String {
    let blocks = parse_markdown_blocks(text);
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            MdBlock::Paragraph(p) => {
                let lines: Vec<String> = p
                    .lines()
                    .map(|l| convert_inline(&prettify_bullets(l), InlineTarget::Plain))
                    .collect();
                parts.push(lines.join("\n"));
            }
            MdBlock::Heading { text, .. } => {
                parts.push(format!("【{}】", convert_inline(&text, InlineTarget::Plain)));
            }
            MdBlock::CodeFence { code, .. } => {
                parts.push(format!("──────\n{code}\n──────"));
            }
            MdBlock::Table { headers, rows } => parts.push(render_table_kv(&headers, &rows)),
            MdBlock::Quote(q) => {
                let quoted: Vec<String> = q
                    .lines()
                    .map(|l| format!("❝ {}", convert_inline(l, InlineTarget::Plain)))
                    .collect();
                parts.push(quoted.join("\n"));
            }
            MdBlock::Divider => parts.push("──────".to_string()),
        }
    }
    parts.join("\n\n")
}

/// Discord renders standard markdown natively EXCEPT tables — replace
/// tables with monospace code fences and pass everything else through.
pub fn preprocess_discord_markdown(text: &str) -> String {
    // Fast path: no pipe tables → untouched (preserves exact formatting).
    if !text.lines().any(|l| l.trim_start().starts_with('|')) {
        return text.to_string();
    }
    let blocks = parse_markdown_blocks(text);
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            MdBlock::Paragraph(p) => parts.push(p),
            MdBlock::Heading { level, text } => {
                parts.push(format!("{} {text}", "#".repeat(level as usize)))
            }
            MdBlock::CodeFence { lang, code } => parts.push(format!("```{lang}\n{code}\n```")),
            MdBlock::Table { headers, rows } => {
                parts.push(format!("```\n{}\n```", render_table_monospace(&headers, &rows)));
            }
            MdBlock::Quote(q) => {
                let quoted: Vec<String> = q.lines().map(|l| format!("> {l}")).collect();
                parts.push(quoted.join("\n"));
            }
            MdBlock::Divider => parts.push("---".to_string()),
        }
    }
    parts.join("\n\n")
}

/// MS Teams `textFormat: markdown` renders bold/italic/code/links but NOT
/// tables or headings — downgrade those, pass the rest through.
pub fn to_teams_markdown(text: &str) -> String {
    let blocks = parse_markdown_blocks(text);
    let mut parts: Vec<String> = Vec::new();
    for block in blocks {
        match block {
            MdBlock::Paragraph(p) => parts.push(p),
            MdBlock::Heading { text, .. } => parts.push(format!("**{text}**")),
            MdBlock::CodeFence { lang, code } => parts.push(format!("```{lang}\n{code}\n```")),
            MdBlock::Table { headers, rows } => {
                parts.push(format!("```\n{}\n```", render_table_monospace(&headers, &rows)));
            }
            MdBlock::Quote(q) => {
                let quoted: Vec<String> = q.lines().map(|l| format!("> {l}")).collect();
                parts.push(quoted.join("\n"));
            }
            MdBlock::Divider => parts.push("---".to_string()),
        }
    }
    parts.join("\n\n")
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "# 標題\n\n這是 **粗體** 與 `code` 和 [連結](https://example.com)。\n\n| 名稱 | 狀態 |\n|------|------|\n| foo | ok |\n| 中文名 | 進行中 |\n\n```rust\nfn main() {}\n```\n\n> 引用文字\n\n- 項目一\n- 項目二";

    #[test]
    fn parse_blocks_structure() {
        let blocks = parse_markdown_blocks(SAMPLE);
        assert!(matches!(blocks[0], MdBlock::Heading { level: 1, .. }));
        assert!(matches!(blocks[1], MdBlock::Paragraph(_)));
        assert!(matches!(blocks[2], MdBlock::Table { .. }));
        assert!(matches!(blocks[3], MdBlock::CodeFence { .. }));
        assert!(matches!(blocks[4], MdBlock::Quote(_)));
        assert!(matches!(blocks[5], MdBlock::Paragraph(_)));
    }

    #[test]
    fn parse_table_cells() {
        let blocks = parse_markdown_blocks("| a | b |\n|---|---|\n| 1 | 2 |");
        let MdBlock::Table { headers, rows } = &blocks[0] else {
            panic!("expected table")
        };
        assert_eq!(headers, &["a", "b"]);
        assert_eq!(rows, &[vec!["1".to_string(), "2".to_string()]]);
    }

    #[test]
    fn unclosed_code_fence_no_panic() {
        let blocks = parse_markdown_blocks("```\nunclosed");
        assert!(matches!(blocks[0], MdBlock::CodeFence { .. }));
    }

    #[test]
    fn telegram_html_escapes_and_converts() {
        let html = to_telegram_html("**bold** & <tag> `a<b`");
        assert!(html.contains("<b>bold</b>"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("&lt;tag&gt;"));
        assert!(html.contains("<code>a&lt;b</code>"));
    }

    #[test]
    fn telegram_html_table_in_pre() {
        let html = to_telegram_html(SAMPLE);
        assert!(html.contains("<pre>"));
        assert!(html.contains("名稱"));
        assert!(html.contains("<pre><code class=\"language-rust\">"));
        assert!(html.contains("<blockquote>引用文字</blockquote>"));
        assert!(html.contains("• 項目一"));
    }

    #[test]
    fn telegram_html_link() {
        let html = to_telegram_html("[點我](https://a.b/c)");
        assert_eq!(html, "<a href=\"https://a.b/c\">點我</a>");
    }

    #[test]
    fn whatsapp_conversion() {
        let wa = to_whatsapp_text("# 標題\n\n**bold** ~~strike~~ [x](https://a.b)");
        assert!(wa.contains("*標題*"));
        assert!(wa.contains("*bold*"));
        assert!(wa.contains("~strike~"));
        assert!(wa.contains("x (https://a.b)"));
    }

    #[test]
    fn googlechat_conversion() {
        let gc = to_googlechat_text("**bold** [x](https://a.b)");
        assert!(gc.contains("*bold*"));
        assert!(gc.contains("<https://a.b|x>"));
    }

    #[test]
    fn line_plain_strips_markup() {
        let plain = to_line_plain("# 標題\n\n**bold** 與 `code`\n\n| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(plain.contains("【標題】"));
        assert!(plain.contains("bold 與 code"));
        assert!(!plain.contains("**"));
        assert!(!plain.contains('|'));
        assert!(plain.contains("▸ a: 1"));
        assert!(plain.contains("• b: 2"));
    }

    #[test]
    fn discord_table_to_fence_rest_untouched(){
        let src = "# H1\n\n**bold**\n\n| a | b |\n|---|---|\n| 1 | 2 |";
        let out = preprocess_discord_markdown(src);
        assert!(out.contains("# H1"));
        assert!(out.contains("**bold**"));
        assert!(out.contains("```"));
        assert!(!out.contains("|---|"));
        // Fast path: no table → byte-identical.
        assert_eq!(preprocess_discord_markdown("**x** # y"), "**x** # y");
    }

    #[test]
    fn teams_markdown_downgrades_headings_tables() {
        let out = to_teams_markdown("# 大標\n\n| a | b |\n|---|---|\n| 1 | 2 |");
        assert!(out.contains("**大標**"));
        assert!(out.contains("```"));
    }

    #[test]
    fn cjk_table_alignment() {
        let t = render_table_monospace(
            &["名稱".into(), "s".into()],
            &[vec!["中文字".into(), "x".into()], vec!["ab".into(), "y".into()]],
        );
        let lines: Vec<&str> = t.lines().collect();
        // Separator positions must align: pipe column index consistent by display width.
        // 名稱=4 cells, 中文字=6 cells → col width 6.
        assert!(lines[0].starts_with("名稱"));
        assert!(lines[2].contains("中文字 | x"));
        assert!(lines[3].contains("ab     | y"));
    }

    #[test]
    fn display_width_cjk() {
        assert_eq!(display_width("中文"), 4);
        assert_eq!(display_width("ab中"), 4);
    }

    #[test]
    fn inline_code_protects_content() {
        let out = convert_inline("`**not bold**`", InlineTarget::WhatsApp);
        assert_eq!(out, "`**not bold**`");
    }

    #[test]
    fn quote_expandable_when_long() {
        let q = "> a\n> b\n> c\n> d\n> e";
        let html = to_telegram_html(q);
        assert!(html.contains("<blockquote expandable>"));
    }
}
