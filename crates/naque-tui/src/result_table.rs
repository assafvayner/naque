//! Scrollable result table with CSV/JSON export.
//!
//! `NULL` values (`None`) are displayed as the dimmed string `"NULL"` in the
//! TUI render. In CSV export `NULL` becomes an empty field; in JSON export it
//! becomes a JSON `null`.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::Theme;

/// Column separator rendered between adjacent columns.
const SEP: &str = " | ";
/// Text used to display a SQL `NULL` cell.
const NULL_TEXT: &str = "NULL";
/// Maximum rendered width of any one column, in display characters.
const MAX_COL_WIDTH: usize = 40;

/// Display width (in `char`s) of a string. Used for column-width math; this is
/// a grapheme-naive count, which is adequate for the ASCII-dominant tabular
/// data the result table renders.
fn display_len(s: &str) -> usize {
    s.chars().count()
}

/// Left-align `s` to exactly `width` display characters: pad with trailing
/// spaces when shorter, or truncate when longer.
fn pad_or_truncate(s: &str, width: usize) -> String {
    let len = display_len(s);
    if len == width {
        s.to_string()
    } else if len < width {
        let mut out = String::with_capacity(width);
        out.push_str(s);
        out.extend(std::iter::repeat_n(' ', width - len));
        out
    } else {
        s.chars().take(width).collect()
    }
}

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

    /// Compute each column's display width: `max(header_len, max visible cell
    /// display len)`, capped at [`MAX_COL_WIDTH`]. `NULL` cells count as the
    /// width of the literal `"NULL"`.
    ///
    /// Only the visible window (from `offset`) contributes, so columns stay
    /// stable as the user scrolls within a page.
    fn column_widths(&self) -> Vec<usize> {
        let mut widths: Vec<usize> = self.columns.iter().map(|c| display_len(c).min(MAX_COL_WIDTH)).collect();

        for row in self.rows.iter().skip(self.offset) {
            for (i, cell) in row.iter().enumerate() {
                if i >= widths.len() {
                    break;
                }
                let len = match cell {
                    Some(v) => display_len(v),
                    None => NULL_TEXT.len(),
                };
                widths[i] = widths[i].max(len.min(MAX_COL_WIDTH));
            }
        }

        widths
    }

    /// Render a header row followed by the visible window of data rows.
    ///
    /// Columns are aligned: each cell is left-padded (or truncated) to its
    /// column width so values line up under their headers. The separator line
    /// spans the actual total table width, not the full area width.
    ///
    /// `NULL` values are rendered as `"NULL"` with the DIM modifier.
    pub fn render(&self, theme: &Theme, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }
        let _ = theme; // theme reserved for future highlight support

        let widths = self.column_widths();
        // Total table width: sum of column widths plus " | " (3 chars) between
        // each adjacent pair of columns.
        let table_width: usize = widths.iter().sum::<usize>() + SEP.len() * widths.len().saturating_sub(1);

        let mut y = area.y;

        // Header row
        {
            let header_text: Vec<Span> = self
                .columns
                .iter()
                .enumerate()
                .flat_map(|(i, col)| {
                    let sep = if i == 0 { "" } else { SEP };
                    let w = widths.get(i).copied().unwrap_or_else(|| display_len(col));
                    vec![
                        Span::raw(sep),
                        Span::styled(pad_or_truncate(col, w), Style::default().add_modifier(Modifier::BOLD)),
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

        // Separator — sized to the actual table width (clamped to the area).
        if y < area.y + area.height {
            let sep_width = table_width.min(area.width as usize);
            let sep = "-".repeat(sep_width);
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
                    let sep = if i == 0 { "" } else { SEP };
                    let w = widths.get(i).copied().unwrap_or(0);
                    let cell_span = match cell {
                        Some(v) => Span::raw(pad_or_truncate(v, w)),
                        None => Span::styled(pad_or_truncate(NULL_TEXT, w), dim_style),
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
    /// - Fields containing a comma, double-quote, or newline are wrapped in double-quotes with internal double-quotes
    ///   doubled (`""`).
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
        serde_json::to_string(&serde_json::Value::Array(objects)).expect("serializing owned data should never fail")
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
    use ratatui::layout::Rect;

    use super::*;

    fn table() -> ResultTable {
        ResultTable::new(
            vec!["id".into(), "name".into()],
            vec![vec![Some("1".into()), Some("a".into())], vec![Some("2".into()), None]],
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
        assert!(content.contains("name"), "expected 'name' in render: {content}");
    }

    #[test]
    fn render_contains_cell_value() {
        let t = table();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        t.render(&Theme::new(false), area, &mut buf);
        let content = buf_to_string(&buf, 40, 10);
        assert!(content.contains('1'), "expected cell value '1' in render: {content}");
        assert!(content.contains('a'), "expected cell value 'a' in render: {content}");
    }

    // --- column alignment ---

    #[test]
    fn column_widths_use_max_of_header_and_cells() {
        // "customer" header (8) vs longest value "Carolyn" (7) → 8.
        // "id" header (2) vs values "1"/"2" (1) → 2.
        let t = ResultTable::new(
            vec!["id".into(), "customer".into()],
            vec![
                vec![Some("1".into()), Some("Bob".into())],
                vec![Some("2".into()), Some("Carolyn".into())],
            ],
        );
        assert_eq!(t.column_widths(), vec![2, 8]);
    }

    #[test]
    fn column_widths_cap_at_max() {
        let long = "x".repeat(100);
        let t = ResultTable::new(vec!["c".into()], vec![vec![Some(long)]]);
        assert_eq!(t.column_widths(), vec![MAX_COL_WIDTH]);
    }

    #[test]
    fn column_widths_count_null_as_four() {
        // header "c" (1) vs NULL (4) → 4.
        let t = ResultTable::new(vec!["c".into()], vec![vec![None]]);
        assert_eq!(t.column_widths(), vec![4]);
    }

    #[test]
    fn render_aligns_cells_under_headers() {
        // Column widths: id=2, customer=8. Header row and each data row should
        // start each column at the same x offset.
        let t = ResultTable::new(
            vec!["id".into(), "customer".into()],
            vec![
                vec![Some("1".into()), Some("Bob".into())],
                vec![Some("2".into()), Some("Carolyn".into())],
            ],
        );
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        t.render(&Theme::new(false), area, &mut buf);

        let row = |y: u16| -> String {
            (0..40)
                .map(|x| buf.cell((x, y)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
                .collect()
        };

        // The second column starts after "id" (2) + " | " (3) = offset 5.
        // Header: "id" then " | " then "customer".
        let header = row(0);
        assert_eq!(&header[0..2], "id");
        assert_eq!(&header[2..5], " | ");
        assert_eq!(&header[5..13], "customer");

        // Data row 0: "1 " (padded to 2) then " | " then "Bob     " (padded 8).
        let data0 = row(2);
        assert_eq!(&data0[0..2], "1 ");
        assert_eq!(&data0[2..5], " | ");
        assert_eq!(&data0[5..13], "Bob     ");

        // Data row 1: "2 " then " | " then "Carolyn ".
        let data1 = row(3);
        assert_eq!(&data1[0..2], "2 ");
        assert_eq!(&data1[5..13], "Carolyn ");
    }

    #[test]
    fn render_separator_matches_table_width_not_area() {
        // id=2, name=4 ("NULL" widens it to 4). table width = 2 + 3 + 4 = 9.
        let t = table();
        let area = Rect::new(0, 0, 40, 10);
        let mut buf = Buffer::empty(area);
        t.render(&Theme::new(false), area, &mut buf);

        let sep_row: String = (0..40)
            .map(|x| buf.cell((x, 1)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
            .collect();
        let dashes = sep_row.chars().take_while(|&c| c == '-').count();
        assert_eq!(dashes, 9, "separator should be table width 9, got {dashes}");
        // Beyond the table width the separator row must be blank.
        assert!(sep_row[9..].chars().all(|c| c == ' '), "separator must not span full area width: {sep_row:?}");
    }

    // --- pad_or_truncate ---

    #[test]
    fn pad_or_truncate_pads_short() {
        assert_eq!(pad_or_truncate("ab", 5), "ab   ");
    }

    #[test]
    fn pad_or_truncate_truncates_long() {
        assert_eq!(pad_or_truncate("abcdef", 3), "abc");
    }

    #[test]
    fn pad_or_truncate_exact() {
        assert_eq!(pad_or_truncate("abc", 3), "abc");
    }
}
