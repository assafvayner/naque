//! UI render-snapshot harness.
//!
//! Renders a set of naque TUI frames into an HTML document for visual review.
//! Runs a real App on an in-memory SQLite database with a MockProvider so the
//! transcript and result table are populated naturally.
//!
//! Usage:
//!   cargo run -p naque --example render_demo -- /path/to/output.html

use std::path::PathBuf;

use naque::app::App;
use naque::approval::AutoApprove;
use naque_core::{CatastrophicReason, GateDecision, PermissionMode};
use naque_db::Database;
use naque_llm::{Agent, AgentConfig, LlmResponse, MockProvider, Usage};
use naque_tui::{ApprovalPrompt, StatusBar, Theme};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let out_path: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ui-snapshots.html"));

    let rt = tokio::runtime::Runtime::new().expect("tokio runtime");

    // Build app and run a few handle_line calls to populate transcript + result.
    let app = rt.block_on(build_populated_app());

    let mut frames: Vec<(String, String)> = Vec::new();

    // --- Frame 1: Main view, color, default mode ---
    {
        let theme = Theme::new(true);
        let (w, h) = (110, 30);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                naque::ui::render(f, &app, &theme, &naque_tui::InputLine::from("show me recent orders"), &[], None)
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        frames.push(("Frame 1: Main view — color, default mode (110×30)".to_string(), buffer_to_html(&buf)));
    }

    // --- Frame 2: Main view, NO_COLOR ---
    {
        let theme = Theme::new(false);
        let (w, h) = (110, 30);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                naque::ui::render(f, &app, &theme, &naque_tui::InputLine::from("show me recent orders"), &[], None)
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        frames.push(("Frame 2: Main view — NO_COLOR (110×30)".to_string(), buffer_to_html(&buf)));
    }

    // --- Frame 3: Approval prompt — WRITE ---
    {
        let theme = Theme::new(true);
        let (w, h) = (110, 14);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        let prompt = ApprovalPrompt::new(
            "UPDATE users SET tier='gold' WHERE id=42".to_string(),
            "WRITE: UPDATE".to_string(),
            None,
            GateDecision::Prompt,
        );
        terminal
            .draw(|f| {
                naque::ui::render(f, &app, &theme, &naque_tui::InputLine::from(""), &[], Some(&prompt));
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        frames.push(("Frame 3: Approval prompt — WRITE (color, 110×14)".to_string(), buffer_to_html(&buf)));
    }

    // --- Frame 4: Catastrophic guard — DROP (color) ---
    {
        let theme = Theme::new(true);
        let (w, h) = (110, 14);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        let prompt = ApprovalPrompt::new(
            "DROP TABLE orders".to_string(),
            "DDL: DROP".to_string(),
            Some(CatastrophicReason::DropObject),
            GateDecision::PromptCatastrophic,
        );
        terminal
            .draw(|f| {
                naque::ui::render(f, &app, &theme, &naque_tui::InputLine::from(""), &[], Some(&prompt));
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        frames.push(("Frame 4: Catastrophic guard — DROP (color, 110×14)".to_string(), buffer_to_html(&buf)));
    }

    // --- Frame 5: Catastrophic guard — DROP, NO_COLOR ---
    {
        let theme = Theme::new(false);
        let (w, h) = (110, 14);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        let prompt = ApprovalPrompt::new(
            "DROP TABLE orders".to_string(),
            "DDL: DROP".to_string(),
            Some(CatastrophicReason::DropObject),
            GateDecision::PromptCatastrophic,
        );
        terminal
            .draw(|f| {
                naque::ui::render(f, &app, &theme, &naque_tui::InputLine::from(""), &[], Some(&prompt));
            })
            .unwrap();
        let buf = terminal.backend().buffer().clone();
        frames.push(("Frame 5: Catastrophic guard — DROP, NO_COLOR (110×14)".to_string(), buffer_to_html(&buf)));
    }

    // --- Frame 6: Status bars — all four modes (color) ---
    {
        frames.push(("Frame 6: Status bars — all four modes (color)".to_string(), render_all_mode_status_bars()));
    }

    // --- Frame 7: Result table (color, 90×14) ---
    {
        frames.push((
            "Frame 7: Result table — 4 columns, NULLs (color, 90×14)".to_string(),
            render_result_table_standalone(),
        ));
    }

    // --- Frames 8 & 9: Live turn in progress ---
    let live_app = rt.block_on(build_live_app());

    // Frame 8: color
    {
        let theme = Theme::new(true);
        let (w, h) = (110u16, 30u16);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| naque::ui::render(f, &live_app, &theme, &naque_tui::InputLine::from(""), &[], None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();

        // Plain-text dump for legibility review (color frame).
        println!("=== Frame 8: Live turn in progress — color (plain text) ===");
        for y in 0..h {
            let line: String = (0..w)
                .map(|x| buf.cell((x, y)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
                .collect();
            println!("|{}", line.trim_end());
        }

        frames.push(("Frame 8: Live turn in progress — color (110×30)".to_string(), buffer_to_html(&buf)));
    }

    // Frame 9: NO_COLOR
    {
        let theme = Theme::new(false);
        let (w, h) = (110u16, 30u16);
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| naque::ui::render(f, &live_app, &theme, &naque_tui::InputLine::from(""), &[], None))
            .unwrap();
        let buf = terminal.backend().buffer().clone();

        // Plain-text dump for legibility review (no-color frame).
        println!("=== Frame 9: Live turn in progress — NO_COLOR (plain text) ===");
        for y in 0..h {
            let line: String = (0..w)
                .map(|x| buf.cell((x, y)).map_or(' ', |c| c.symbol().chars().next().unwrap_or(' ')))
                .collect();
            println!("|{}", line.trim_end());
        }

        frames.push(("Frame 9: Live turn in progress — NO_COLOR (110×30)".to_string(), buffer_to_html(&buf)));
    }

    // Assemble HTML document.
    let html = build_html_document(&frames);
    std::fs::write(&out_path, &html).expect("write output file");
    eprintln!("Written {} bytes to {}", html.len(), out_path.display());
}

// ---------------------------------------------------------------------------
// App builder
// ---------------------------------------------------------------------------

async fn build_populated_app() -> App {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite:{}", tmp.path().display());
    let db = Database::connect(&url).await.expect("connect");

    let agent = Agent::new(
        Box::new(MockProvider::new(vec![LlmResponse {
            text: Some("I'll look up recent orders for you.".to_string()),
            tool_calls: vec![],
            usage: Usage {
                input_tokens: 120,
                output_tokens: 14,
            },
            stop_reason: "end_turn".to_string(),
        }])),
        AgentConfig {
            model: "mock".to_string(),
            max_iterations: 10,
            max_tokens: 1024,
            system_preamble: "You are a SQL assistant.".to_string(),
        },
    );

    let mut app = App::new(db, agent, PermissionMode::Default, "prod-analytics", false, 200);

    // DDL — create tables.
    app.handle_line(
        "!CREATE TABLE orders (id INTEGER PRIMARY KEY, customer TEXT, amount REAL, status TEXT)",
        &mut AutoApprove,
    )
    .await
    .ok();

    // Inserts — including one NULL.
    app.handle_line("!INSERT INTO orders VALUES (1, 'Alice', 129.99, 'shipped')", &mut AutoApprove)
        .await
        .ok();
    app.handle_line("!INSERT INTO orders VALUES (2, 'Bob', 49.50, NULL)", &mut AutoApprove)
        .await
        .ok();
    app.handle_line("!INSERT INTO orders VALUES (3, 'Carol', 299.00, 'pending')", &mut AutoApprove)
        .await
        .ok();

    // SELECT — populates last_result.
    app.handle_line("!SELECT id, customer, amount, status FROM orders ORDER BY id", &mut AutoApprove)
        .await
        .ok();

    // The handle_line calls above already pushed Sql transcript entries.
    // Trigger a NL turn to populate User + Agent transcript entries naturally.
    // The MockProvider will return the scripted response.
    app.handle_natural_language("show me recent orders", &mut AutoApprove)
        .await
        .ok();

    app
}

// ---------------------------------------------------------------------------
// Live-turn app builder (Frames 8 & 9)
// ---------------------------------------------------------------------------

/// Build a minimal App whose live state shows a running turn: a streamed
/// Reasoning entry followed by a Running ToolStep — no TurnFinished so
/// `running` stays `true` and the pinned activity line is visible.
async fn build_live_app() -> App {
    use naque_llm::AgentEvent;

    let tmp = tempfile::NamedTempFile::new().unwrap();
    let url = format!("sqlite:{}", tmp.path().display());
    let db = Database::connect(&url).await.expect("connect");

    let agent = Agent::new(
        Box::new(MockProvider::new(vec![])),
        AgentConfig {
            model: "mock".to_string(),
            max_iterations: 12,
            max_tokens: 1024,
            system_preamble: "You are a SQL assistant.".to_string(),
        },
    );

    let mut app = App::new(db, agent, PermissionMode::Default, "prod-analytics", false, 12);

    // Drive live state via the public apply_event path.
    app.apply_event(&AgentEvent::TurnStarted);
    app.apply_event(&AgentEvent::LlmCallStarted { iteration: 1 });
    app.apply_event(&AgentEvent::TextDelta("Let me check the orders table.".into()));
    app.apply_event(&AgentEvent::ToolCallStarted {
        name: "run_query".into(),
        detail: Some("SELECT count(*) FROM orders".into()),
    });
    app.apply_event(&AgentEvent::UsageUpdated(Usage {
        input_tokens: 1200,
        output_tokens: 48,
    }));
    // Do NOT send TurnFinished — we want running=true for the in-progress view.

    app
}

// ---------------------------------------------------------------------------
// Frame 6 helper: render all four mode status bars
// ---------------------------------------------------------------------------

fn render_all_mode_status_bars() -> String {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let theme = Theme::new(true);
    let width: u16 = 90;
    let modes = [
        PermissionMode::Strict,
        PermissionMode::Default,
        PermissionMode::ReadOnly,
        PermissionMode::Wildcard,
    ];

    let height: u16 = modes.len() as u16;
    let area = Rect::new(0, 0, width, height);
    let mut buf = Buffer::empty(area);

    for (i, mode) in modes.iter().enumerate() {
        let bar = StatusBar {
            profile: "prod-analytics".to_string(),
            env: Some("prod".to_string()),
            mode: *mode,
            in_transaction: false,
            tokens: 134,
            cost_usd: 0.002,
            mark: None,
        };
        let row_area = Rect::new(0, i as u16, width, 1);
        bar.render(&theme, row_area, &mut buf);
    }

    buffer_to_html(&buf)
}

// ---------------------------------------------------------------------------
// Frame 7 helper: standalone result table
// ---------------------------------------------------------------------------

fn render_result_table_standalone() -> String {
    use naque_tui::ResultTable;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;

    let theme = Theme::new(true);
    let (w, h): (u16, u16) = (90, 14);
    let area = Rect::new(0, 0, w, h);
    let mut buf = Buffer::empty(area);

    let table = ResultTable::new(
        vec![
            "id".to_string(),
            "customer".to_string(),
            "amount".to_string(),
            "status".to_string(),
        ],
        vec![
            vec![
                Some("1".into()),
                Some("Alice".into()),
                Some("129.99".into()),
                Some("shipped".into()),
            ],
            vec![Some("2".into()), Some("Bob".into()), Some("49.50".into()), None],
            vec![
                Some("3".into()),
                Some("Carol".into()),
                Some("299.00".into()),
                Some("pending".into()),
            ],
            vec![Some("4".into()), Some("Dave".into()), None, Some("processing".into())],
        ],
    );

    table.render(&theme, area, &mut buf);
    buffer_to_html(&buf)
}

// ---------------------------------------------------------------------------
// buffer_to_html
// ---------------------------------------------------------------------------

/// Convert a ratatui [`Buffer`] to an HTML `<pre>` block preserving fg color,
/// bg color, bold, reversed, dim, italic, and underline modifiers.
fn buffer_to_html(buf: &ratatui::buffer::Buffer) -> String {
    use ratatui::style::{Color, Modifier};

    let area = buf.area;
    let width = area.width;
    let height = area.height;

    let mut html = String::new();
    html.push_str(
        r#"<pre style="font-family:monospace;font-size:13px;background:#000;color:#ccc;padding:8px;line-height:1.2;overflow-x:auto;white-space:pre">"#,
    );

    for y in 0..height {
        let mut x = 0u16;
        while x < width {
            let Some(cell) = buf.cell((area.x + x, area.y + y)) else {
                x += 1;
                continue;
            };

            // Collect all consecutive cells with the same style.
            let cell_style = cell.style();
            let mut span_text = String::new();
            span_text.push_str(cell.symbol());
            x += 1;

            while x < width {
                let Some(next) = buf.cell((area.x + x, area.y + y)) else {
                    break;
                };
                if next.style() != cell_style {
                    break;
                }
                span_text.push_str(next.symbol());
                x += 1;
            }

            // Resolve fg and bg colors accounting for REVERSED.
            let modifier = cell_style.add_modifier;
            let reversed = modifier.contains(Modifier::REVERSED);

            let raw_fg = cell_style.fg.unwrap_or(Color::Reset);
            let raw_bg = cell_style.bg.unwrap_or(Color::Reset);

            let (fg, bg) = if reversed {
                // Swap: use fg as background, bg (or default) as foreground.
                let display_fg = match raw_bg {
                    Color::Reset => Color::Black,
                    c => c,
                };
                let display_bg = match raw_fg {
                    Color::Reset => Color::White,
                    c => c,
                };
                (display_fg, display_bg)
            } else {
                (raw_fg, raw_bg)
            };

            // Build CSS.
            let mut css = String::new();
            let fg_css = color_to_css(fg);
            let bg_css = color_to_css(bg);

            if fg_css != "inherit" {
                css.push_str(&format!("color:{fg_css};"));
            }
            if bg_css != "inherit" {
                css.push_str(&format!("background:{bg_css};"));
            }
            if modifier.contains(Modifier::BOLD) {
                css.push_str("font-weight:bold;");
            }
            if modifier.contains(Modifier::DIM) {
                css.push_str("opacity:0.6;");
            }
            if modifier.contains(Modifier::ITALIC) {
                css.push_str("font-style:italic;");
            }
            if modifier.contains(Modifier::UNDERLINED) {
                css.push_str("text-decoration:underline;");
            }

            let escaped = html_escape(&span_text);
            if css.is_empty() {
                html.push_str(&escaped);
            } else {
                html.push_str(&format!(r#"<span style="{css}">{escaped}</span>"#));
            }
        }
        html.push('\n');
    }

    html.push_str("</pre>");
    html
}

fn color_to_css(color: ratatui::style::Color) -> &'static str {
    use ratatui::style::Color;
    match color {
        Color::Reset => "inherit",
        Color::Black => "#000",
        Color::Red => "#d33",
        Color::Green => "#3a3",
        Color::Yellow => "#cc0",
        Color::Blue => "#39f",
        Color::Magenta => "#c3c",
        Color::Cyan => "#0cc",
        Color::Gray => "#aaa",
        Color::DarkGray => "#666",
        Color::LightRed => "#f66",
        Color::LightGreen => "#6d6",
        Color::LightYellow => "#ff6",
        Color::LightBlue => "#6af",
        Color::LightMagenta => "#f6f",
        Color::LightCyan => "#6ff",
        Color::White => "#fff",
        // Rgb and Indexed have no fixed hex mapping; naque-tui only uses the named colors above.
        Color::Rgb(_, _, _) | Color::Indexed(_) => "inherit",
    }
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// HTML document builder
// ---------------------------------------------------------------------------

fn build_html_document(frames: &[(String, String)]) -> String {
    let mut doc = String::new();
    doc.push_str(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>naque UI snapshots</title>
<style>
  body { background: #1e1e1e; color: #ccc; font-family: sans-serif; padding: 24px; margin: 0; }
  h1 { color: #fff; margin-bottom: 32px; }
  .frame { margin-bottom: 48px; }
  h2 { color: #aaf; font-size: 14px; margin-bottom: 6px; font-family: monospace; }
</style>
</head>
<body>
<h1>naque UI Snapshots</h1>
"#,
    );

    for (caption, pre_html) in frames {
        let escaped_caption = html_escape(caption);
        doc.push_str(&format!("<div class=\"frame\">\n<h2>{escaped_caption}</h2>\n{pre_html}\n</div>\n"));
    }

    doc.push_str("</body>\n</html>\n");
    doc
}
