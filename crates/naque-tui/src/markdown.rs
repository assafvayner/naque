//! Minimal Markdown rendering for agent answers.
//!
//! Agent replies are usually light Markdown: paragraphs, `**bold**`, `inline
//! code`, fenced code blocks, headings, and bullet lists. This module turns
//! that into styled ratatui [`Line`]s — splitting on newlines (so multi-line
//! answers render as real lines) and applying a few inline/block styles. It is
//! deliberately small: no tables, links, or nested emphasis, and unknown markup
//! is rendered verbatim rather than guessed at.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::Theme;

/// Render Markdown `text` into styled lines (one per source line, code blocks
/// and headings/bullets handled). Code and de-emphasis use the theme so the
/// result degrades correctly under NO_COLOR.
pub fn render_markdown(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let code_style = theme.dim_style();
    let mut in_code = false;

    for raw in text.split('\n') {
        if raw.trim_start().starts_with("```") {
            in_code = !in_code;
            continue; // the fence line itself isn't rendered
        }
        if in_code {
            out.push(Line::from(vec![
                Span::styled("│ ".to_string(), code_style),
                Span::styled(raw.to_string(), code_style),
            ]));
            continue;
        }
        out.push(render_block_line(raw.trim_end(), theme));
    }
    out
}

/// Render a single non-code-block line: heading, bullet, or paragraph.
fn render_block_line(line: &str, theme: &Theme) -> Line<'static> {
    let base = Style::default();
    let stripped = line.trim_start();
    let indent = &line[..line.len() - stripped.len()];

    // Heading: 1–6 leading '#' followed by a space → bold, hashes removed.
    let hashes = stripped.chars().take_while(|&c| c == '#').count();
    if (1..=6).contains(&hashes) && stripped[hashes..].starts_with(' ') {
        let content = stripped[hashes + 1..].trim_start();
        let mut spans = vec![Span::raw(indent.to_string())];
        spans.extend(parse_inline(content, base.add_modifier(Modifier::BOLD), theme));
        return Line::from(spans);
    }

    // Bullet: "- ", "* ", or "+ " → a • marker, indentation preserved.
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = stripped.strip_prefix(marker) {
            let mut spans = vec![Span::raw(format!("{indent}• "))];
            spans.extend(parse_inline(rest, base, theme));
            return Line::from(spans);
        }
    }

    Line::from(parse_inline(line, base, theme))
}

/// Split inline text into styled spans, handling `` `code` `` and `**bold**`.
///
/// `base` is the style for plain runs (bold for headings); inline code always
/// uses the theme's de-emphasis style. Unterminated markers render verbatim.
fn parse_inline(text: &str, base: Style, theme: &Theme) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut buf = String::new();
    let bold = base.add_modifier(Modifier::BOLD);
    let code = theme.dim_style();
    let mut i = 0;

    while i < chars.len() {
        // Byte tag: <bytes>...</bytes> -> inner text verbatim + size suffix.
        if chars[i] == '<' {
            let open = ['<', 'b', 'y', 't', 'e', 's', '>'];
            let close = ['<', '/', 'b', 'y', 't', 'e', 's', '>'];
            if chars[i..].starts_with(&open) {
                let inner_start = i + open.len();
                if let Some(rel) = find_char_subslice(&chars[inner_start..], &close) {
                    let inner: String = chars[inner_start..inner_start + rel].iter().collect();
                    flush(&mut buf, &mut spans, base);
                    let rendered = match crate::bytes::parse_byte_count(&inner) {
                        Some(n) => match crate::bytes::byte_suffix(n) {
                            Some(suffix) => format!("{inner}{suffix}"),
                            None => inner,
                        },
                        None => inner,
                    };
                    spans.push(Span::styled(rendered, base));
                    i = inner_start + rel + close.len();
                    continue;
                }
            }
        }
        // Inline code: `...`
        if chars[i] == '`'
            && let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == '`')
        {
            flush(&mut buf, &mut spans, base);
            spans.push(Span::styled(chars[i + 1..close].iter().collect::<String>(), code));
            i = close + 1;
            continue;
        }
        // Bold: **...**
        if chars[i] == '*'
            && i + 1 < chars.len()
            && chars[i + 1] == '*'
            && let Some(close) =
                (i + 2..chars.len().saturating_sub(1)).find(|&j| chars[j] == '*' && chars[j + 1] == '*')
        {
            flush(&mut buf, &mut spans, base);
            spans.push(Span::styled(chars[i + 2..close].iter().collect::<String>(), bold));
            i = close + 2;
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush(&mut buf, &mut spans, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

fn flush(buf: &mut String, spans: &mut Vec<Span<'static>>, style: Style) {
    if !buf.is_empty() {
        spans.push(Span::styled(std::mem::take(buf), style));
    }
}

/// Index of the first occurrence of `needle` within `haystack` (char slices),
/// or `None`. An empty `needle` returns `None` (callers must not pass one).
fn find_char_subslice(haystack: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&i| haystack[i..i + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn theme() -> Theme {
        Theme::new(true)
    }

    /// Concatenated plain text of a line.
    fn plain(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn splits_into_one_line_per_source_line() {
        let lines = render_markdown("first\nsecond\nthird", &theme());
        assert_eq!(lines.len(), 3);
        assert_eq!(plain(&lines[0]), "first");
        assert_eq!(plain(&lines[2]), "third");
    }

    #[test]
    fn bold_segment_gets_bold_modifier() {
        let lines = render_markdown("a **big** deal", &theme());
        assert_eq!(plain(&lines[0]), "a big deal");
        let bold_span = lines[0].spans.iter().find(|s| s.content.as_ref() == "big").unwrap();
        assert!(bold_span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn inline_code_strips_backticks() {
        let lines = render_markdown("use `SELECT` now", &theme());
        assert_eq!(plain(&lines[0]), "use SELECT now");
        assert!(lines[0].spans.iter().any(|s| s.content.as_ref() == "SELECT"));
    }

    #[test]
    fn heading_drops_hashes_and_bolds() {
        let lines = render_markdown("## Results", &theme());
        assert_eq!(plain(&lines[0]), "Results");
        assert!(lines[0].spans.iter().any(|s| s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn bullet_becomes_dot_marker() {
        let lines = render_markdown("- one\n- two", &theme());
        assert_eq!(plain(&lines[0]), "• one");
        assert_eq!(plain(&lines[1]), "• two");
    }

    #[test]
    fn fenced_code_block_renders_without_fences() {
        let md = "before\n```\nSELECT 1;\n```\nafter";
        let lines = render_markdown(md, &theme());
        let texts: Vec<String> = lines.iter().map(plain).collect();
        // Fence lines (```), removed; the code line is kept (with a gutter).
        assert!(texts.iter().any(|t| t.contains("SELECT 1;")), "code kept: {texts:?}");
        assert!(!texts.iter().any(|t| t.contains("```")), "fences removed: {texts:?}");
        assert!(texts.contains(&"before".to_string()));
        assert!(texts.contains(&"after".to_string()));
    }

    #[test]
    fn unterminated_markers_render_verbatim() {
        let lines = render_markdown("a **bold and `code", &theme());
        assert_eq!(plain(&lines[0]), "a **bold and `code");
    }

    #[test]
    fn empty_input_yields_one_empty_line() {
        let lines = render_markdown("", &theme());
        assert_eq!(lines.len(), 1);
        assert_eq!(plain(&lines[0]), "");
    }

    #[test]
    fn bytes_tag_expands_with_suffix() {
        let lines = render_markdown("size is <bytes>4500000000</bytes> total", &theme());
        assert_eq!(plain(&lines[0]), "size is 4500000000 (4.5 GB) total");
    }

    #[test]
    fn bytes_tag_below_threshold_has_no_suffix() {
        let lines = render_markdown("only <bytes>9999</bytes> bytes", &theme());
        assert_eq!(plain(&lines[0]), "only 9999 bytes");
    }

    #[test]
    fn bytes_tag_with_commas_prints_inner_verbatim() {
        let lines = render_markdown("<bytes>4,500,000,000</bytes>", &theme());
        assert_eq!(plain(&lines[0]), "4,500,000,000 (4.5 GB)");
    }

    #[test]
    fn unterminated_bytes_tag_renders_verbatim() {
        let lines = render_markdown("a <bytes>123 b", &theme());
        assert_eq!(plain(&lines[0]), "a <bytes>123 b");
    }

    #[test]
    fn non_numeric_bytes_tag_renders_inner_only() {
        let lines = render_markdown("<bytes>lots</bytes>", &theme());
        assert_eq!(plain(&lines[0]), "lots");
    }

    #[test]
    fn bytes_tag_inside_code_block_not_expanded() {
        let md = "```\n<bytes>4500000000</bytes>\n```";
        let lines = render_markdown(md, &theme());
        let texts: Vec<String> = lines.iter().map(plain).collect();
        assert!(
            texts.iter().any(|t| t.contains("<bytes>4500000000</bytes>")),
            "code block must stay verbatim: {texts:?}"
        );
    }

    #[test]
    fn two_bytes_tags_on_one_line_both_expand() {
        let lines = render_markdown("<bytes>1000000</bytes> and <bytes>2000000</bytes>", &theme());
        assert_eq!(plain(&lines[0]), "1000000 (1.0 MB) and 2000000 (2.0 MB)");
    }

    #[test]
    fn empty_bytes_tag_renders_empty() {
        let lines = render_markdown("<bytes></bytes>", &theme());
        assert_eq!(plain(&lines[0]), "");
    }
}
