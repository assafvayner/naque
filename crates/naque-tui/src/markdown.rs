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
}
