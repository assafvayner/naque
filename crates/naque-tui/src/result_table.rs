//! Scrollable result table with CSV/JSON export.
//!
//! `NULL` values (`None`) are displayed as the dimmed string `"NULL"` in the
//! TUI render. In CSV export `NULL` becomes an empty field; in JSON export it
//! becomes a JSON `null`.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Widget,
};

use crate::Theme;

/// A scrollable table of query results.
pub struct ResultTable {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
    /// First visible row index.
    offset: usize,
}

impl ResultTable {
    /// Create a new result table starting at row offset 0.
    pub fn new(columns: Vec<String>, rows: Vec<Vec<Option<String>>>) -> Self {
        Self {
            columns,
            rows,
            offset: 0,
        }
    }

    /// Advance the visible window by `page` rows, clamping so the offset never
    /// moves past the last row index.
    pub fn scroll_down(&mut self, page: usize) {
        if self.rows.is_empty() {
            return;
        }
        let max_offset = self.rows.len() - 1;
        self.offset = (self.offset + page).min(max_offset);
    }

    /// Move the visible window back by `page` rows, clamping at 0.
    pub fn scroll_up(&mut self, page: usize) {
        self.offset = self.offset.saturating_sub(page);
    }

    /// Render a header row followed by the visible window of data rows.
    ///
    /// `NULL` values are rendered as `"NULL"` with the DIM modifier.
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let _ = theme; // theme reserved for future highlight support

        let mut y = area.y;

        // Header row
        {
            let header_text: Vec<Span> = self
                .columns
                .iter()
                .enumerate()
                .flat_map(|(i, col)| {
                    let sep = if i == 0 { "" } else { " | " };
                    vec![
                        Span::raw(sep),
                        Span::styled(col.as_str(), Style::default().add_modifier(Modifier::BOLD)),
                    ]
                })
                .collect();
            let line = Line::from(header_text);
            if y < area.y + area.height {
                line.render(
                    Rect {
                        x: area.x,
                        y,
                        width: area.width,
                        height: 1,
                    },
                    buf,
                );
                y += 1;
            }
        }

        // Separator
        if y < area.y + area.height {
            let sep = "-".repeat(area.width as usize);
            Line::from(Span::raw(sep)).render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y += 1;
        }

        // Data rows (starting at offset)
        let dim_style = Style::default().add_modifier(Modifier::DIM);
        for row in self.rows.iter().skip(self.offset) {
            if y >= area.y + area.height {
                break;
            }
            let cells: Vec<Span> = row
                .iter()
                .enumerate()
                .flat_map(|(i, cell)| {
                    let sep = if i == 0 { "" } else { " | " };
                    let cell_span = match cell {
                        Some(v) => Span::raw(v.as_str()),
                        None => Span::styled("NULL", dim_style),
                    };
                    vec![Span::raw(sep), cell_span]
                })
                .collect();
            let line = Line::from(cells);
            line.render(
                Rect {
                    x: area.x,
                    y,
                    width: area.width,
                    height: 1,
                },
                buf,
            );
            y += 1;
        }
    }

    /// Export as RFC 4180-ish CSV.
    ///
    /// - Header row comes first.
    /// - `NULL` → empty field.
    /// - Fields containing a comma, double-quote, or newline are wrapped in
    ///   double-quotes with internal double-quotes doubled (`""`).
    /// - Lines end with `\n` (LF only).
    pub fn to_csv(&self) -> String {
        let mut out = String::new();

        // Header
        out.push_str(&csv_row(self.columns.iter().map(|c| c.as_str())));
        out.push('\n');

        // Data rows
        for row in &self.rows {
            let fields: Vec<&str> = row.iter().map(|c| c.as_deref().unwrap_or("")).collect();
            out.push_str(&csv_row(fields.iter().copied()));
            out.push('\n');
        }

        out
    }

    /// Export as a JSON array of objects.
    ///
    /// Each object is keyed by column name. `NULL` → JSON `null`.
    /// String values are serialized with `serde_json`.
    pub fn to_json(&self) -> String {
        let objects: Vec<serde_json::Value> = self
            .rows
            .iter()
            .map(|row| {
                let mut map = serde_json::Map::new();
                for (col, cell) in self.columns.iter().zip(row.iter()) {
                    let value = match cell {
                        Some(v) => serde_json::Value::String(v.clone()),
                        None => serde_json::Value::Null,
                    };
                    map.insert(col.clone(), value);
                }
                serde_json::Value::Object(map)
            })
            .collect();
        serde_json::to_string(&serde_json::Value::Array(objects))
            .expect("serializing owned data should never fail")
    }
}

/// Build a single CSV row from an iterator of field strings.
fn csv_row<'a>(fields: impl Iterator<Item = &'a str>) -> String {
    let parts: Vec<String> = fields.map(csv_field).collect();
    parts.join(",")
}

/// Encode a single CSV field, quoting when necessary.
fn csv_field(s: &str) -> String {
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        let escaped = s.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;

    fn table() -> ResultTable {
        ResultTable::new(
            vec!["id".into(), "name".into()],
            vec![
                vec![Some("1".into()), Some("a".into())],
                vec![Some("2".into()), None],
            ],
        )
    }

    // --- to_csv ---

    #[test]
    fn csv_basic_with_null() {
        let csv = table().to_csv();
        assert_eq!(csv, "id,name\n1,a\n2,\n");
    }

    #[test]
    fn csv_quotes_field_with_comma() {
        let t = ResultTable::new(vec!["val".into()], vec![vec![Some("hello,world".into())]]);
        let csv = t.to_csv();
        assert_eq!(csv, "val\n\"hello,world\"\n");
    }

    #[test]
    fn csv_quotes_field_with_embedded_quote() {
        let t = ResultTable::new(vec!["val".into()], vec![vec![Some("say \"hi\"".into())]]);
        let csv = t.to_csv();
        assert_eq!(csv, "val\n\"say \"\"hi\"\"\"\n");
    }

    #[test]
    fn csv_quotes_field_with_newline() {
        let t = ResultTable::new(vec!["val".into()], vec![vec![Some("line1\nline2".into())]]);
        let csv = t.to_csv();
        assert_eq!(csv, "val\n\"line1\nline2\"\n");
    }

    #[test]
    fn csv_header_only_when_no_rows() {
        let t = ResultTable::new(vec!["a".into(), "b".into()], vec![]);
        assert_eq!(t.to_csv(), "a,b\n");
    }

    // --- to_json ---

    #[test]
    fn json_basic_with_null() {
        let json = table().to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        let arr = parsed.as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["id"], "1");
        assert_eq!(arr[0]["name"], "a");
        assert_eq!(arr[1]["id"], "2");
        assert_eq!(arr[1]["name"], serde_json::Value::Null);
    }

    #[test]
    fn json_empty_table() {
        let t = ResultTable::new(vec!["x".into()], vec![]);
        let json = t.to_json();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 0);
    }

    // --- scroll ---

    #[test]
    fn scroll_down_advances_offset() {
        let mut t = table();
        t.scroll_down(1);
        assert_eq!(t.offset, 1);
    }

    #[test]
    fn scroll_down_clamps_at_last_row() {
        let mut t = table(); // 2 rows, last index = 1
        t.scroll_down(100);
        assert_eq!(t.offset, 1);
    }

    #[test]
    fn scroll_up_decrements_offset() {
        let mut t = table();
        t.scroll_down(1);
        t.scroll_up(1);
        assert_eq!(t.offset, 0);
    }

    #[test]
    fn scroll_up_clamps_at_zero() {
        let mut t = table();
        t.scroll_up(100);
        assert_eq!(t.offset, 0);
    }

    #[test]
    fn scroll_down_on_empty_table_is_noop() {
        let mut t = ResultTable::new(vec!["x".into()], vec![]);
        t.scroll_down(5);
        assert_eq!(t.offset, 0);
    }

    // --- render smoke tests ---

    fn buf_to_string(buf: &Buffer, width: u16, height: u16) -> String {
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                if let Some(c) = buf.cell((x, y)) {
                    out.push_str(c.symbol());
                }
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn render_contains_column_header() {
        let t = table();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        t.render(&Theme::new(false), area, &mut buf);
        let content = buf_to_string(&buf, 40, 10);
        assert!(content.contains("id"), "expected 'id' in render: {content}");
        assert!(
            content.contains("name"),
            "expected 'name' in render: {content}"
        );
    }

    #[test]
    fn render_contains_cell_value() {
        let t = table();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        t.render(&Theme::new(false), area, &mut buf);
        let content = buf_to_string(&buf, 40, 10);
        assert!(
            content.contains('1'),
            "expected cell value '1' in render: {content}"
        );
        assert!(
            content.contains('a'),
            "expected cell value 'a' in render: {content}"
        );
    }
}
