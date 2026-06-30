//! TUI rendering and terminal event loop.

use std::io;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use naque_tui::{ActivityLine, ApprovalPrompt, History, InputLine, ResultTable, StatusBar, Theme};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::ExecutableCommand;
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind,
};
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{Frame, Terminal};

use crate::app::{App, TranscriptEntry};
use crate::approval::{ApprovalDecision, ApprovalRequest};

/// Rough cost estimate for the default model (claude-opus-4-8):
/// $5 per 1M input tokens, $25 per 1M output tokens.
fn estimate_cost_usd(usage: &naque_llm::Usage) -> f64 {
    (usage.input_tokens as f64 / 1_000_000.0) * 5.0 + (usage.output_tokens as f64 / 1_000_000.0) * 25.0
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Prompt prefix on the first input row; continuation rows align under it.
const INPUT_PREFIX: &str = "> ";
const INPUT_CONT: &str = "  ";
/// Cap on input-band rows; taller input scrolls vertically to follow the cursor.
const INPUT_MAX_ROWS: u16 = 6;
/// Cap on rows shown for queued submissions before collapsing to a summary.
const QUEUE_MAX_ROWS: usize = 5;

/// Render one frame of the main UI.
///
/// Layout (top to bottom):
/// 1. Transcript area — scrollable list of history entries (query results render inline here, so they scroll with the
///    chat).
/// 2. Activity line — pinned spinner row while a turn runs (height 0 when idle).
/// 3. Queued submissions — lines entered while a turn runs, dimmed.
/// 4. Status bar — single line.
/// 5. Input line(s) — `> {input}`, wrapped onto multiple rows when needed.
/// 6. Approval prompt — overlaid as a centered modal when `pending` is Some.
pub fn render(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    input: &InputLine,
    queued: &[String],
    pending: Option<&ApprovalPrompt>,
) {
    let size = frame.area();
    let prefix_w = INPUT_PREFIX.len() as u16;

    // Wrap the input up front so the bottom band can grow to fit the wrapped
    // rows (capped at INPUT_MAX_ROWS). This is what keeps long/multiline input
    // on screen instead of scrolling it horizontally out of view.
    let input_text_w = size.width.saturating_sub(prefix_w).max(1);
    let wrapped = input.wrap(input_text_w);
    let input_rows = (wrapped.rows.len() as u16).clamp(1, INPUT_MAX_ROWS);

    // Determine heights: input = input_rows, status = 1, activity = 1 while
    // running, queued = one row per queued line (capped), transcript = remainder.
    // Query results are no longer a pinned band — they render inline in the
    // transcript so they scroll with the chat. The approval prompt is not a
    // layout band — it is drawn as a centered modal popup over the whole screen.
    let activity_height: u16 = if app.live.running { 1 } else { 0 };
    let queued_height = queued.len().min(QUEUE_MAX_ROWS) as u16;
    let fixed_bottom: u16 = 1 + input_rows; // status + input
    let transcript_height = size.height.saturating_sub(fixed_bottom + activity_height + queued_height);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(transcript_height),
            Constraint::Length(activity_height),
            Constraint::Length(queued_height), // queued submissions
            Constraint::Length(1),             // status bar
            Constraint::Length(input_rows),    // input line(s)
        ])
        .split(size);

    // ---- Transcript --------------------------------------------------------
    // Fresh session (nothing said yet, no turn running): show the welcome splash
    // instead of an empty transcript. It clears as soon as the conversation starts.
    if app.transcript().is_empty() && !app.live.running {
        render_welcome(frame, app, theme, chunks[0]);
    } else {
        let width = chunks[0].width;
        let mut lines: Vec<Line> = Vec::new();
        // Line span [start, len) each transcript entry occupies, for reveal.
        let mut spans: Vec<(usize, usize)> = Vec::with_capacity(app.transcript().len());
        for (i, entry) in app.transcript().iter().enumerate() {
            let expanded = app.expanded_steps.contains(&i);
            let selected = app.selected_step == Some(i);
            let begin = lines.len();
            lines.extend(transcript_lines(entry, theme, expanded, selected, width));
            spans.push((begin, lines.len() - begin));
        }

        let chunk = chunks[0];
        let visible = chunk.height as usize;
        let total = lines.len();
        // A selection pins the view on the selected step; otherwise follow the
        // tail offset by scroll_offset (0 = following the tail).
        let start = match app.selected_step.and_then(|i| spans.get(i).copied()) {
            Some((s, l)) => reveal_window(total, visible, s, l),
            None => {
                let off = app.live.scroll_offset as usize;
                total.saturating_sub(off).saturating_sub(visible)
            },
        };
        let end = (start + visible).min(total);
        let tail: Vec<Line> = lines[start..end].to_vec();
        let used = tail.len() as u16;
        let pad = chunk.height.saturating_sub(used);
        let anchored = Rect {
            x: chunk.x,
            y: chunk.y + pad,
            width: chunk.width,
            height: used,
        };

        let para = Paragraph::new(tail).block(Block::default()).wrap(Wrap { trim: false });
        frame.render_widget(para, anchored);

        if !app.live.follow && app.live.new_below > 0 {
            let hint = format!("↓ {} new", app.live.new_below);
            let hint_area = Rect {
                x: chunk.x,
                y: chunk.y + chunk.height.saturating_sub(1),
                width: chunk.width,
                height: 1,
            };
            frame.render_widget(Paragraph::new(Line::from(Span::styled(hint, theme.dim_style()))), hint_area);
        }
    }

    // ---- Activity line ------------------------------------------------------
    if app.live.running {
        let usage = &app.live.live_usage;
        let line = ActivityLine {
            action: app.live.current_action.clone().unwrap_or_else(|| "working".into()),
            spinner_frame: app.live.spinner_frame,
            iteration: app.live.iteration,
            max_iterations: app.live.max_iterations,
            tokens: usage.input_tokens + usage.output_tokens,
            awaiting_approval: app.live.awaiting_approval,
        };
        let buf = frame.buffer_mut();
        line.render(theme, chunks[1], buf);
    }

    // ---- Status bar --------------------------------------------------------
    {
        let usage = app.usage();
        let tokens = usage.input_tokens + usage.output_tokens;
        let cost_usd = estimate_cost_usd(usage);

        let bar = StatusBar {
            profile: app.profile_name.clone(),
            env: app.active_env.clone(),
            mode: app.mode(),
            in_transaction: false,
            tokens,
            cost_usd,
            mark: Some(app.logo().mark_span(theme.color)),
        };
        let buf = frame.buffer_mut();
        bar.render(theme, chunks[3], buf);
    }

    // ---- Queued submissions ------------------------------------------------
    // Lines the user enqueued while a turn was running, shown dimmed just above
    // the status bar so they can see what will run next.
    if queued_height > 0 {
        let shown = queued_height as usize;
        let overflow = queued.len() > QUEUE_MAX_ROWS;
        let body = if overflow { shown.saturating_sub(1) } else { shown };
        let mut lines: Vec<Line> = Vec::with_capacity(shown);
        for q in queued.iter().take(body) {
            lines.push(Line::from(Span::styled(format!("» {q}"), theme.dim_style())));
        }
        if overflow {
            let more = queued.len() - body;
            lines.push(Line::from(Span::styled(format!("» … {more} more queued"), theme.dim_style())));
        }
        frame.render_widget(Paragraph::new(lines), chunks[2]);
    }

    // ---- Input line(s) -----------------------------------------------------
    {
        let input_chunk = chunks[4];
        let rows_shown = input_rows as usize;
        let total = wrapped.rows.len();
        // Window the wrapped rows so the cursor row stays visible when the input
        // is taller than the cap.
        let window_start = if wrapped.cursor_row < rows_shown {
            0
        } else {
            wrapped.cursor_row - rows_shown + 1
        };
        let end = (window_start + rows_shown).min(total);
        let mut lines: Vec<Line> = Vec::with_capacity(end - window_start);
        for (i, row) in wrapped.rows[window_start..end].iter().enumerate() {
            let pfx = if window_start + i == 0 {
                INPUT_PREFIX
            } else {
                INPUT_CONT
            };
            lines.push(Line::from(Span::raw(format!("{pfx}{row}"))));
        }
        frame.render_widget(Paragraph::new(lines), input_chunk);

        // Place the terminal cursor at the input position. Skipped while an
        // approval modal owns focus (it has its own input handling).
        if pending.is_none() {
            let max_col = input_text_w.saturating_sub(1);
            let col = wrapped.cursor_col.min(max_col);
            let cx = input_chunk.x + prefix_w + col;
            let cy = input_chunk.y + (wrapped.cursor_row - window_start) as u16;
            frame.set_cursor_position((cx, cy));
        }

        // After a first idle Ctrl+C, prompt the user that another press exits.
        if app.quit_armed {
            let hint = "press ^C again to exit";
            let hint_w = hint.len() as u16;
            if input_chunk.width > hint_w {
                let hint_area = Rect {
                    x: input_chunk.x + input_chunk.width - hint_w,
                    y: input_chunk.y,
                    width: hint_w,
                    height: 1,
                };
                frame.render_widget(Paragraph::new(Line::from(Span::styled(hint, theme.dim_style()))), hint_area);
            }
        }
    }

    // ---- Approval prompt (centered modal popup) ----------------------------
    // Drawn last so it sits on top of the transcript/result. `Clear` wipes the
    // cells behind the modal so nothing bleeds through, then a bordered block
    // frames the prompt. The modal is sized tall enough to always show the
    // header, optional catastrophic warning, the SQL, and ALL THREE picker
    // options with the ❯ selection marker.
    if let Some(prompt) = pending {
        let modal = centered_modal_rect(size, prompt);
        frame.render_widget(Clear, modal);

        let block = Block::default().borders(Borders::ALL).title(" Approval required ");
        let inner = block.inner(modal);
        frame.render_widget(block, modal);

        let buf = frame.buffer_mut();
        prompt.render(theme, inner, buf);
    }
}

/// Render the fresh-session welcome splash into `area`: the speckled NAQUE
/// wordmark plus a tagline and key hints, centered. Shown only while the
/// transcript is empty; replaced by the conversation on first input.
fn render_welcome(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let mut lines = app.logo().wordmark_lines(area.width, theme.color);
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("ask your database in natural language", theme.dim_style())));
    lines.push(Line::from(Span::styled(
        "/help for commands    /profile to switch    ^C to quit",
        theme.dim_style(),
    )));

    // Vertically center the splash (top-align if it is taller than the area).
    let block_h = lines.len() as u16;
    let top_pad = area.height.saturating_sub(block_h) / 2;
    let inner = Rect {
        x: area.x,
        y: area.y + top_pad,
        width: area.width,
        height: block_h.min(area.height),
    };

    frame.render_widget(Paragraph::new(lines).alignment(Alignment::Center), inner);
}

/// Compute a centered [`Rect`] for the approval modal, sized to fit the prompt
/// content (header + optional warning + blank + SQL lines + blank + 3 options)
/// plus the surrounding border.
fn centered_modal_rect(area: Rect, prompt: &ApprovalPrompt) -> Rect {
    // Content lines (matching ApprovalPrompt::render layout):
    //   header(1) + warning(0|1) + blank(1) + sql_lines(N) + blank(1) + options(3)
    let warning = if prompt.is_catastrophic() { 1 } else { 0 };
    let sql_lines = prompt.sql_line_count().max(1) as u16;
    let content_height = 1 + warning + 1 + sql_lines + 1 + 3;

    // +2 for top/bottom border.
    let desired_h = content_height + 2;
    let height = desired_h.min(area.height.max(1));

    // Width: at most ~80, at least enough for the longest content line, bounded
    // by the available area minus a small margin.
    let widest = prompt.content_width() as u16 + 4; // padding + borders
    let max_w = area.width.saturating_sub(4).max(1);
    let width = widest.clamp(20.min(max_w), 80.min(max_w)).min(max_w);

    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;

    Rect { x, y, width, height }
}

/// Build the styled transcript line(s) for one [`TranscriptEntry`].
///
/// `Sql` entries get their `[label]` badge colored by classification via
/// [`Theme::label_style`], so read/write/DDL/catastrophic statements are
/// visually distinguishable (and degrade correctly under NO_COLOR).
/// Split `text` on newlines into lines, prefixing the first with `gutter` and
/// continuation lines with a matching-width indent so the body stays aligned.
fn prefixed_lines(gutter: &'static str, text: &str) -> Vec<Line<'static>> {
    let indent = " ".repeat(gutter.chars().count());
    text.split('\n')
        .enumerate()
        .map(|(i, raw)| {
            let g = if i == 0 { gutter } else { indent.as_str() };
            Line::from(Span::raw(format!("{g}{raw}")))
        })
        .collect()
}

/// Render an agent answer as Markdown, prefixed with the ` ai: ` gutter on the
/// first line and aligned indentation on the rest.
fn agent_lines(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let body = naque_tui::render_markdown(text, theme);
    let mut out: Vec<Line<'static>> = Vec::with_capacity(body.len().max(1));
    for (i, line) in body.into_iter().enumerate() {
        let gutter = if i == 0 { " ai: " } else { "     " };
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        spans.push(Span::raw(gutter));
        spans.extend(line.spans);
        out.push(Line::from(spans));
    }
    if out.is_empty() {
        out.push(Line::from(Span::raw(" ai: ")));
    }
    out
}

/// Logical SQL lines shown before truncation in a collapsed running step.
const SQL_PREVIEW_LINES: usize = 5;

/// Data rows shown for a result table before it is collapsed (ctrl+r expands).
const RESULT_PREVIEW_ROWS: usize = 10;

/// Tools whose `detail` is a SQL statement rendered as a multi-line block.
fn is_sql_tool(name: &str) -> bool {
    matches!(name, "run_query" | "explain")
}

/// Truncate `s` to at most `max` columns (char-approximated), appending '…' on
/// overflow so a long line cannot soft-wrap and blow the height budget.
fn truncate_cols(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

/// Lines for one tool step: a header plus, for SQL tools that are running (or a
/// finished step the user expanded), a capped `│ `-gutter SQL block.
// All 8 parameters are distinct concerns (identity, status, interaction flags, layout, theme);
// bundling them into a struct would add indirection for a single call site.
#[allow(clippy::too_many_arguments)]
fn tool_step_lines(
    name: &str,
    detail: Option<&str>,
    status: &crate::app::StepStatus,
    summary: Option<&str>,
    expanded: bool,
    selected: bool,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    use crate::app::StepStatus;

    let (glyph, glyph_style) = match status {
        StepStatus::Running => ("▸", theme.activity_style()),
        StepStatus::Ok => ("✓", theme.label_style("read-only")),
        StepStatus::Err => ("✗", theme.label_style("DDL: DROP")),
    };
    let marker = if selected {
        Span::styled("❯ ", theme.activity_style())
    } else {
        Span::raw("  ")
    };

    let running = matches!(status, StepStatus::Running);
    let is_sql = is_sql_tool(name);
    let sql = detail.map(str::trim).filter(|s| !s.is_empty());
    let show_block = is_sql && sql.is_some() && (running || expanded);

    let mut header: Vec<Span<'static>> = vec![
        marker,
        Span::styled(glyph.to_string(), glyph_style),
        Span::raw(format!(" {name}")),
    ];
    if !show_block {
        // SQL tool not showing a block => finished & collapsed: show the summary.
        // Target tools: always show the detail target (fall back to the summary).
        let inline = if is_sql {
            summary.map(str::to_string)
        } else {
            sql.map(str::to_string).or_else(|| summary.map(str::to_string))
        };
        if let Some(d) = inline {
            header.push(Span::styled(format!(" {d}"), theme.dim_style()));
        }
    }

    let mut out: Vec<Line<'static>> = vec![Line::from(header)];

    if show_block {
        let sql = sql.unwrap();
        let gutter = "  │ ";
        let content_w = (width as usize).saturating_sub(gutter.chars().count());
        let logical: Vec<&str> = sql.split('\n').collect();
        let total = logical.len();
        let cap = if expanded { total } else { SQL_PREVIEW_LINES.min(total) };
        for raw in logical.iter().take(cap) {
            let text = truncate_cols(raw, content_w);
            out.push(Line::from(Span::styled(format!("{gutter}{text}"), theme.dim_style())));
        }
        if !expanded && total > cap {
            let more = total - cap;
            out.push(Line::from(Span::styled(format!("  … +{more} more · ctrl+r to expand"), theme.dim_style())));
        } else if expanded && total > SQL_PREVIEW_LINES {
            out.push(Line::from(Span::styled("  ctrl+r to collapse", theme.dim_style())));
        }
        if !running && let Some(s) = summary {
            out.push(Line::from(Span::styled(format!("  └ {s}"), theme.dim_style())));
        }
    }

    out
}

/// First visible transcript line so the selected entry (its `[sel_start,
/// sel_start+sel_len)` line span) is on screen: bottom-aligned, or top-aligned
/// when the entry is taller than the viewport.
fn reveal_window(total: usize, visible: usize, sel_start: usize, sel_len: usize) -> usize {
    if visible == 0 || total <= visible {
        return 0;
    }
    if sel_len >= visible {
        sel_start.min(total - visible)
    } else {
        (sel_start + sel_len).min(total).saturating_sub(visible)
    }
}

fn transcript_lines<'a>(
    entry: &'a TranscriptEntry,
    theme: &Theme,
    expanded: bool,
    selected: bool,
    width: u16,
) -> Vec<Line<'a>> {
    match entry {
        TranscriptEntry::User(text) => prefixed_lines("you: ", text),
        TranscriptEntry::Agent(text) => agent_lines(text, theme),
        TranscriptEntry::Sql { sql, label } => {
            vec![Line::from(vec![
                Span::raw("sql["),
                Span::styled(label.clone(), theme.label_style(label)),
                Span::raw(format!("]: {sql}")),
            ])]
        },
        TranscriptEntry::Info(text) => prefixed_lines("inf: ", text),
        TranscriptEntry::Error(text) => prefixed_lines("err: ", text),
        TranscriptEntry::Rejected(sql) => prefixed_lines("rej: ", sql),
        TranscriptEntry::Reasoning(text) => {
            vec![Line::from(Span::styled(format!("  · {text}"), theme.dim_style()))]
        },
        TranscriptEntry::ToolStep {
            name,
            detail,
            status,
            summary,
        } => tool_step_lines(name, detail.as_deref(), status, summary.as_deref(), expanded, selected, width, theme),
        TranscriptEntry::Result {
            columns,
            rows,
            byte_columns,
        } => result_lines(columns, rows, byte_columns, expanded, selected, width, theme),
    }
}

/// Lines for an inline result table: a `❯`/two-space marker on the header, a
/// `RESULT_PREVIEW_ROWS`-row preview (all rows when `expanded`), and a trailing
/// hint when there are more rows than shown.
fn result_lines(
    columns: &[String],
    rows: &[Vec<Option<String>>],
    byte_columns: &[usize],
    expanded: bool,
    selected: bool,
    width: u16,
    theme: &Theme,
) -> Vec<Line<'static>> {
    // Indent the table two columns so it aligns under the chat gutter; the
    // marker occupies that indent on the header row when selected.
    const INDENT: &str = "  ";
    let indent_w = INDENT.len() as u16;
    let table = ResultTable::new(columns.to_vec(), rows.to_vec()).with_byte_columns(byte_columns.to_vec());
    let cap = (!expanded).then_some(RESULT_PREVIEW_ROWS);
    let body = table.lines(cap, width.saturating_sub(indent_w));

    let mut out: Vec<Line<'static>> = Vec::with_capacity(body.len() + 1);
    for (i, line) in body.into_iter().enumerate() {
        let lead = if i == 0 && selected {
            Span::styled("❯ ", theme.activity_style())
        } else {
            Span::raw(INDENT)
        };
        let mut spans = Vec::with_capacity(line.spans.len() + 1);
        spans.push(lead);
        spans.extend(line.spans);
        out.push(Line::from(spans));
    }

    let total = rows.len();
    if !expanded && total > RESULT_PREVIEW_ROWS {
        let more = total - RESULT_PREVIEW_ROWS;
        out.push(Line::from(Span::styled(
            format!("  … +{more} more row{} · ctrl+r to expand", if more == 1 { "" } else { "s" }),
            theme.dim_style(),
        )));
    } else if expanded && total > RESULT_PREVIEW_ROWS {
        out.push(Line::from(Span::styled("  ctrl+r to collapse", theme.dim_style())));
    }

    out
}

// ---------------------------------------------------------------------------
// Terminal runner
// ---------------------------------------------------------------------------

/// Enter raw mode + alternate screen, run the interactive loop, then restore.
///
/// The `runtime` is passed so we can `block_on` the async `event_loop` without
/// requiring `run` to be async (which would complicate terminal restore on
/// error). Natural-language turns run on tasks spawned via `start_turn` and
/// stream live; the loop drains their events and drives approvals.
pub fn run(mut app: App, theme: Theme, runtime: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    // Enter raw mode and alternate screen.
    enable_raw_mode().context("enable raw mode")?;
    io::stdout().execute(EnterAlternateScreen).context("enter alternate screen")?;

    // RAII guard: from this point on, the terminal is ALWAYS restored on drop —
    // whether `run` returns early via `?`, completes normally, or unwinds on panic.
    let _guard = TerminalGuard;

    // Enable mouse capture so the scroll wheel reaches us. Done after the guard
    // is in place so a failure here still tears down raw mode + alt screen; the
    // guard's DisableMouseCapture is a harmless no-op if this never ran.
    io::stdout().execute(EnableMouseCapture).context("enable mouse capture")?;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    // The guard's Drop restores the terminal once `terminal`/`_guard` go out of scope.
    runtime.block_on(event_loop(&mut app, &theme, &mut terminal))
}

/// Restores the terminal (leaves raw mode + alternate screen) when dropped.
///
/// Constructing this immediately after `enable_raw_mode` + `EnterAlternateScreen`
/// guarantees cleanup on every exit path — normal return, `?` propagation, or panic.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = io::stdout().execute(DisableMouseCapture);
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

/// The text after a leading `/` while it is still a single bare word (no
/// whitespace) — the prefix the autocomplete popup filters on. `None` when the
/// input is not currently typing a slash command.
fn slash_suggest_prefix(text: &str) -> Option<&str> {
    let rest = text.strip_prefix('/')?;
    if rest.contains(char::is_whitespace) {
        None
    } else {
        Some(rest)
    }
}

/// Text to drop into the input when completing `cmd`. Commands that take
/// arguments get a trailing space so the cursor lands ready for the argument
/// (and the trailing space closes the popup); argument-less commands do not.
fn complete_slash(cmd: &naque_tui::SlashCommand) -> String {
    if cmd.args.is_empty() {
        format!("/{}", cmd.name)
    } else {
        format!("/{} ", cmd.name)
    }
}

/// Draw the autocomplete popup as a floating box just above the input line.
fn render_suggest_popup(frame: &mut Frame, theme: &Theme, sg: &naque_tui::SlashSuggest) {
    let area = frame.area();
    if area.height < 4 || area.width < 6 {
        return;
    }
    // The input line is the bottom row; grow the popup upward from just above it.
    let input_row = area.height.saturating_sub(1);
    let height = sg.preferred_height().min(input_row).max(3);
    let width = (sg.content_width() + 2).min(area.width).max(10);
    let top = input_row.saturating_sub(height);
    let popup = Rect {
        x: area.x,
        y: top,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    sg.render(theme, popup, frame.buffer_mut());
}

async fn event_loop<B: ratatui::backend::Backend + Send>(
    app: &mut App,
    theme: &Theme,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let mut input = InputLine::new();
    let mut history = History::new();
    // Submissions entered while a turn is running, dispatched in order once it
    // finishes. Lets the user line up the next query without waiting.
    let mut queued: Vec<String> = Vec::new();
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(80));
    let mut pending: Option<(ApprovalRequest, ApprovalPrompt)> = None;
    // Slash-command autocomplete: highlighted row + an Esc-dismissed flag.
    let mut suggest_selected: usize = 0;
    let mut suggest_dismissed = false;

    loop {
        // Re-arm the popup once the input is no longer a slash command.
        if !input.text().starts_with('/') {
            suggest_dismissed = false;
            suggest_selected = 0;
        }

        // Build the autocomplete popup for this frame, if it should show. Shown
        // even while a turn runs, since the input stays editable (e.g. to queue
        // a slash command). Suppressed only while an approval modal owns focus.
        let suggest = if pending.is_none() && !suggest_dismissed {
            let matches = slash_suggest_prefix(input.text()).map(naque_tui::matching).unwrap_or_default();
            (!matches.is_empty()).then(|| naque_tui::SlashSuggest::new(matches, suggest_selected))
        } else {
            None
        };

        {
            let prompt_ref = pending.as_ref().map(|(_, p)| p);
            terminal.draw(|f| {
                render(f, app, theme, &input, &queued, prompt_ref);
                if let Some(sg) = suggest.as_ref() {
                    render_suggest_popup(f, theme, sg);
                }
            })?;
        }

        tokio::select! {
            maybe_ev = events.next() => {
                // Mouse wheel scrolls the transcript. Ignored while an approval
                // modal owns focus (consistent with key routing below).
                if let Some(Ok(Event::Mouse(m))) = &maybe_ev {
                    if pending.is_none() {
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                app.clear_step_selection();
                                app.live.scroll_up(3);
                            },
                            MouseEventKind::ScrollDown => {
                                app.clear_step_selection();
                                app.live.scroll_down(3);
                            },
                            _ => {},
                        }
                    }
                    continue;
                }
                let Some(Ok(Event::Key(key))) = maybe_ev else { continue };
                if key.kind != KeyEventKind::Press { continue; }

                // Approval modal active: route keys to the prompt.
                if let Some((_, prompt)) = pending.as_mut() {
                    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                        let (req, _) = pending.take().unwrap();
                        let _ = req.reply.send(ApprovalDecision::Reject);
                        continue;
                    }
                    if let Some(choice) = prompt.handle_key(key) {
                        let (req, _) = pending.take().unwrap();
                        let decision = approval_choice_to_decision(choice, &req.sql, terminal, app, theme);
                        let _ = req.reply.send(decision);
                    }
                    continue;
                }

                // Ctrl+C: cancel a running turn, or arm/confirm quit when idle.
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    if app.is_turn_running() {
                        app.cancel_turn();
                        app.quit_armed = false;
                    } else if app.quit_armed {
                        break;
                    } else {
                        app.quit_armed = true;
                    }
                    continue;
                }
                app.quit_armed = false;

                // Slash-command popup is open: intercept navigation/complete/dismiss.
                if let Some(sg) = suggest.as_ref() {
                    match key.code {
                        KeyCode::Up if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            suggest_selected = suggest_selected.saturating_sub(1);
                            continue;
                        },
                        KeyCode::Down if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                            suggest_selected = (suggest_selected + 1).min(sg.len() - 1);
                            continue;
                        },
                        KeyCode::Tab => {
                            if let Some(cmd) = sg.selected_command() {
                                input.set_text(complete_slash(&cmd));
                                suggest_selected = 0;
                            }
                            continue;
                        },
                        KeyCode::Esc => {
                            suggest_dismissed = true;
                            continue;
                        },
                        _ => {},
                    }
                }

                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                // Wrap width for vertical cursor movement (matches the renderer:
                // full width minus the 2-column prompt). Falls back if unknown.
                let text_w = terminal.size().map(|s| s.width.saturating_sub(2)).unwrap_or(78).max(1);
                // Alt/Shift+Enter inserts a newline for multi-line composition;
                // plain Enter submits (or queues while a turn is running).
                let newline = key.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT);
                match key.code {
                    KeyCode::Enter if newline => input.insert('\n'),
                    KeyCode::Enter => {
                        let line = input.take();
                        if line.trim().is_empty() {
                            continue;
                        }
                        history.push(&line);
                        // While a turn runs the agent is busy, so queue the line
                        // and dispatch it when the turn finishes.
                        if app.is_turn_running() {
                            queued.push(line);
                        } else {
                            submit_line(app, terminal, theme, &line).await;
                            if app.should_quit() {
                                break;
                            }
                        }
                    },
                    KeyCode::PageUp => {
                        app.clear_step_selection();
                        app.live.scroll_up(5);
                    },
                    KeyCode::PageDown => {
                        app.clear_step_selection();
                        app.live.scroll_down(5);
                    },
                    // Ctrl+End: back to the live tail / input.
                    KeyCode::End if ctrl => {
                        app.clear_step_selection();
                        app.live.scroll_to_latest();
                    },
                    // Ctrl+↑/↓ select among tool steps; Ctrl+R toggles the selected
                    // (or newest) step's SQL block. Placed before the bare Up/Down
                    // arms so the modified arrows win.
                    KeyCode::Up if ctrl => app.select_prev_step(),
                    KeyCode::Down if ctrl => app.select_next_step(),
                    KeyCode::Char('r') if ctrl => app.toggle_selected_step(),
                    // --- input editing (always available, even while a turn runs) ---
                    KeyCode::Left => input.move_left(),
                    KeyCode::Right => input.move_right(),
                    KeyCode::Home => input.move_home(),
                    KeyCode::End => input.move_end(),
                    // Up/Down move between wrapped rows; at the top/bottom edge they
                    // recall session history (popup intercepts these when open).
                    KeyCode::Up => {
                        if !input.move_up(text_w)
                            && let Some(text) = history.older(input.text())
                        {
                            input.set_text(text);
                        }
                    },
                    KeyCode::Down => {
                        if !input.move_down(text_w)
                            && let Some(text) = history.newer()
                        {
                            input.set_text(text);
                        }
                    },
                    KeyCode::Char('a') if ctrl => input.move_home(),
                    KeyCode::Char('e') if ctrl => input.move_end(),
                    KeyCode::Delete => input.delete(),
                    KeyCode::Backspace => input.backspace(),
                    KeyCode::Char(c) if !ctrl => input.insert(c),
                    _ => {},
                }

                // Editing the command word resets the popup highlight to the top.
                if matches!(key.code, KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete) {
                    suggest_selected = 0;
                }
            }

            _ = ticker.tick() => { app.live.tick(); }

            // Disabled while an approval modal is up: the turn task is parked
            // awaiting the approval oneshot, so it emits no events and there is
            // nothing to drain. Leaving the arm enabled would busy-poll every
            // 20ms for the whole time the user reads the prompt. When the user
            // decides and `pending` clears, draining resumes (the unbounded
            // channel buffers any events produced meanwhile, so nothing is lost).
            step = turn_step(app), if app.is_turn_running() && pending.is_none() => {
                match step {
                    // A streamed event arrived — fold it into transcript + live state.
                    TurnStep::Event(ev) => {
                        if !app.live.follow { app.live.new_below = app.live.new_below.saturating_add(1); }
                        app.apply_event(&ev);
                    },
                    // The spawned task finished — reclaim the agent + drain remaining
                    // buffered events (finalize_turn does the draining itself).
                    TurnStep::Finished => {
                        app.finalize_turn().await;
                        // Dispatch queued submissions in order. Commands run inline
                        // and we keep going; the first natural-language line starts a
                        // new turn, so we stop and let its completion drain the rest.
                        while !app.is_turn_running() {
                            let Some(line) = (!queued.is_empty()).then(|| queued.remove(0)) else {
                                break;
                            };
                            submit_line(app, terminal, theme, &line).await;
                            if app.should_quit() {
                                break;
                            }
                        }
                    },
                }
            }
        }

        if app.should_quit() {
            break;
        }

        // Surface a pending approval request from the running turn as modal state.
        if pending.is_none() {
            if let Some(req) = app.try_recv_approval() {
                let prompt = ApprovalPrompt::new(req.sql.clone(), req.label.clone(), None, req.decision);
                app.live.awaiting_approval = true;
                pending = Some((req, prompt));
            }
        } else if !app.is_turn_running() {
            // Turn ended (e.g. cancelled) while a prompt was up — drop it.
            if let Some((req, _)) = pending.take() {
                let _ = req.reply.send(ApprovalDecision::Reject);
            }
        }
        // Clear the awaiting_approval flag once the prompt is dismissed.
        if pending.is_none() {
            app.live.awaiting_approval = false;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Async event-loop helpers
// ---------------------------------------------------------------------------

/// One step of progress on the in-flight turn.
enum TurnStep {
    /// The next streamed event from the turn task.
    Event(naque_llm::AgentEvent),
    /// The spawned task has finished (its channel closed or it joined).
    Finished,
}

/// Drive the in-flight turn one step: race the next streamed event against the
/// task's completion. A single `&mut App` borrow keeps this usable from a
/// `select!` arm (two separate arms would each borrow `app` and conflict).
///
/// Completion is detected two ways: the event channel closing (`next_event`
/// returns `None`) or the join handle finishing. The latter is polled on a
/// short cadence because the handle is owned by `app.inflight` and cannot be
/// awaited directly here; the loop's tick keeps the UI responsive meanwhile.
async fn turn_step(app: &mut App) -> TurnStep {
    loop {
        if app.poll_finished() {
            return TurnStep::Finished;
        }
        tokio::select! {
            biased;
            maybe_event = app.next_event() => {
                match maybe_event {
                    Some(ev) => return TurnStep::Event(ev),
                    None => return TurnStep::Finished,
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

/// Route a submitted input line: slash commands that need their own UI flow are
/// handled here; everything else goes through [`dispatch_line`]. Shared by the
/// interactive Enter path and the queued-submission drain.
async fn submit_line<B: ratatui::backend::Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    theme: &Theme,
    line: &str,
) {
    let trimmed = line.trim();
    let is_cmd = |c: &str| trimmed == c || trimmed.starts_with(&format!("{c} "));
    if is_cmd("/profile") {
        handle_profile_command(app, theme, terminal).await;
    } else if is_cmd("/env") {
        handle_env_command(app, theme, terminal).await;
    } else if is_cmd("/save") {
        let args = trimmed.strip_prefix("/save").map(str::trim).unwrap_or("");
        handle_save_command(app, theme, terminal, args).await;
    } else if is_cmd("/learn") {
        handle_learn_command(app, theme, terminal).await;
    } else {
        dispatch_line(app, terminal, theme, line).await;
    }
}

/// Route a submitted line. NL goes through the spawned streaming path; commands
/// and raw SQL run inline (fast, no streaming).
async fn dispatch_line<B: ratatui::backend::Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    theme: &Theme,
    line: &str,
) {
    use naque_tui::{Input, route_input};
    match route_input(line) {
        Input::NaturalLanguage(text) => {
            // `start_turn` records an explicit error when the agent is
            // unavailable, so the user always sees why nothing happened. The
            // spawned turn streams its own live progress, so it is not wrapped.
            app.start_turn(&text);
        },
        Input::RawSql(sql) => {
            // Raw SQL runs inline. If the gate would auto-approve (e.g. wildcard,
            // or a read in readonly), run it directly. If it would prompt, we
            // cannot show the modal from this inline path yet (Task 4.7b wires
            // raw-SQL approval through the modal) — surface an explicit message
            // instead of silently approving or rejecting.
            if app.raw_sql_auto_approves(&sql).await {
                let base = snapshot_app(terminal, app, theme);
                let mut approver = crate::approval::AutoApprove;
                let fut = app.execute_sql(&sql, naque_core::gate::QueryKind::Primary, &mut approver);
                let _ = run_with_spinner(terminal, &base, theme, "Running query…", fut).await;
            } else {
                app.push_info(format!(
                    "Raw SQL needs approval in {} mode; modal approval for raw SQL is coming in the next step. \
                     Use `/mode wildcard` to run it now, or ask in natural language.",
                    app.mode()
                ));
            }
        },
        Input::DbCommand(cmd) => {
            let label = match cmd.trim() {
                "reset" => "Reconnecting…",
                "dt" => "Loading tables…",
                _ => "Working…",
            };
            let base = snapshot_app(terminal, app, theme);
            let fut = app.handle_db_command(&cmd);
            let _ = run_with_spinner(terminal, &base, theme, label, fut).await;
        },
        Input::ToolCommand(cmd) => {
            let _ = app.handle_tool_command(&cmd, &mut crate::approval::AutoApprove).await;
        },
        Input::Empty => {},
    }
}

fn approval_choice_to_decision<B: ratatui::backend::Backend>(
    choice: naque_tui::ApprovalChoice,
    sql: &str,
    terminal: &mut Terminal<B>,
    app: &App,
    theme: &Theme,
) -> ApprovalDecision {
    use naque_tui::ApprovalChoice;
    match choice {
        ApprovalChoice::Accept => ApprovalDecision::Accept,
        ApprovalChoice::Reject => ApprovalDecision::Reject,
        // `inline_edit` runs a synchronous `crossterm::event::read()` sub-loop.
        // Safe to block here: the multi-thread runtime keeps the ticker alive on
        // other workers, and the turn task is parked awaiting this approval, so
        // no async work needs to progress during the edit.
        ApprovalChoice::Edit => match inline_edit(terminal, sql, |f, buf| render_edit(f, app, theme, buf)) {
            Some(edited) => ApprovalDecision::AcceptEdited(edited),
            None => ApprovalDecision::Reject,
        },
    }
}

// ---------------------------------------------------------------------------
// Inline SQL edit box
// ---------------------------------------------------------------------------

/// Run a minimal single-line editor pre-filled with `initial`.
///
/// `draw_frame(frame, edit_buffer)` is called each iteration to render the
/// surrounding UI plus the edit line. Returns `Some(edited)` on Enter, or
/// `None` if the user cancels with Esc (which the caller maps to Reject).
fn inline_edit<B, F>(terminal: &mut Terminal<B>, initial: &str, mut draw_frame: F) -> Option<String>
where
    B: ratatui::backend::Backend,
    F: FnMut(&mut Frame, &str),
{
    let mut buf = initial.to_string();
    loop {
        {
            let buf_ref = buf.as_str();
            let _ = terminal.draw(|f| draw_frame(f, buf_ref));
        }

        if let Ok(Event::Key(key)) = ratatui::crossterm::event::read() {
            match key.code {
                KeyCode::Enter => {
                    let trimmed = buf.trim();
                    if trimmed.is_empty() {
                        // Empty edit cancels rather than running nothing.
                        return None;
                    }
                    return Some(trimmed.to_string());
                },
                KeyCode::Esc => return None,
                KeyCode::Backspace => {
                    buf.pop();
                },
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return None;
                },
                KeyCode::Char(c) => buf.push(c),
                _ => {},
            }
        }
    }
}

fn run_picker<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    theme: &Theme,
    title: &str,
    options: Vec<String>,
) -> Option<usize> {
    use naque_tui::{Picker, PickerOption, PickerOutcome};
    if options.is_empty() {
        return None;
    }
    let count = options.len();
    let mut picker = Picker::new(
        options
            .into_iter()
            .map(|label| PickerOption { label, shortcut: None })
            .collect(),
    );
    loop {
        let _ = terminal.draw(|f| {
            let area = f.area();
            let h = (count as u16 + 2).min(area.height);
            let rect = Rect {
                x: 0,
                y: area.height.saturating_sub(h),
                width: area.width,
                height: h,
            };
            f.render_widget(Clear, rect);
            let block = Block::default().borders(Borders::ALL).title(format!(" {title} "));
            let inner = block.inner(rect);
            f.render_widget(block, rect);
            picker.render(theme, inner, f.buffer_mut());
        });
        if let Ok(Event::Key(key)) = ratatui::crossterm::event::read() {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return None;
            }
            match picker.handle_key(key) {
                Some(PickerOutcome::Selected(i)) => return Some(i),
                Some(PickerOutcome::Cancelled) => return None,
                None => {},
            }
        }
    }
}

async fn handle_profile_command<B: ratatui::backend::Backend>(
    app: &mut App,
    theme: &Theme,
    terminal: &mut Terminal<B>,
) {
    let profiles = match app.list_profiles() {
        Ok(p) if !p.is_empty() => p,
        Ok(_) => {
            app.push_info("no profiles saved yet; use /save <profile> <env>");
            return;
        },
        Err(e) => {
            app.push_info(format!("cannot list profiles: {e}"));
            return;
        },
    };
    let Some(pi) = run_picker(terminal, theme, "profile", profiles.clone()) else {
        return;
    };
    let profile = profiles[pi].clone();
    let envs = match app.list_environments(&profile) {
        Ok(e) if !e.is_empty() => e,
        _ => {
            app.push_info(format!("profile '{profile}' has no environments"));
            return;
        },
    };
    let Some(ei) = run_picker(terminal, theme, "environment", envs.clone()) else {
        return;
    };
    connect_to(app, theme, terminal, &profile, &envs[ei]).await;
}

/// Connect to `profile`/`env` with a progress spinner, recording an info line on
/// failure. Shared by the `/profile` and `/env` interactive flows.
async fn connect_to<B: ratatui::backend::Backend>(
    app: &mut App,
    theme: &Theme,
    terminal: &mut Terminal<B>,
    profile: &str,
    env: &str,
) {
    let base = snapshot_app(terminal, app, theme);
    let fut = app.switch_to(profile, env);
    if let Err(e) = run_with_spinner(terminal, &base, theme, &format!("Connecting to {profile}/{env}…"), fut).await {
        app.push_info(format!("switch failed: {e}"));
    }
}

async fn handle_env_command<B: ratatui::backend::Backend>(app: &mut App, theme: &Theme, terminal: &mut Terminal<B>) {
    let Some(profile) = app.active_profile.clone() else {
        app.push_info("no active profile; pick one with /profile");
        return;
    };
    let envs = match app.list_environments(&profile) {
        Ok(e) if !e.is_empty() => e,
        _ => {
            app.push_info(format!("profile '{profile}' has no environments"));
            return;
        },
    };
    let Some(ei) = run_picker(terminal, theme, "environment", envs.clone()) else {
        return;
    };
    connect_to(app, theme, terminal, &profile, &envs[ei]).await;
}

const SPINNER_GRACE: Duration = Duration::from_millis(120);
const SPINNER_TICK: Duration = Duration::from_millis(80);

/// Render the current app frame and return it as an owned buffer, so a slow
/// operation can keep the chat visible behind an animated progress line without
/// holding a borrow on `app` (which the operation's future needs mutably).
fn snapshot_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &App,
    theme: &Theme,
) -> ratatui::buffer::Buffer {
    let empty = InputLine::new();
    let area = terminal.get_frame().area();
    terminal
        .draw(|f| render(f, app, theme, &empty, &[], None))
        .map(|completed| completed.buffer.clone())
        .unwrap_or_else(|_| ratatui::buffer::Buffer::empty(area))
}

/// Drive `fut` to completion while keeping `base` (a chat snapshot) visible and
/// overlaying an animated bottom progress line once the operation has run longer
/// than a short grace period (so fast operations never flash it). `base` is an
/// owned buffer, so `fut` may borrow `&mut App`.
async fn run_with_spinner<B, T, Fut>(
    terminal: &mut Terminal<B>,
    base: &ratatui::buffer::Buffer,
    theme: &Theme,
    label: &str,
    fut: Fut,
) -> T
where
    B: ratatui::backend::Backend,
    Fut: std::future::Future<Output = T>,
{
    tokio::pin!(fut);
    let start = tokio::time::Instant::now() + SPINNER_GRACE;
    let mut ticker = tokio::time::interval_at(start, SPINNER_TICK);
    let mut frame_idx = 0usize;
    loop {
        tokio::select! {
            biased;
            out = &mut fut => return out,
            _ = ticker.tick() => {
                let spinner = naque_tui::SPINNER_FRAMES[frame_idx % naque_tui::SPINNER_FRAMES.len()];
                frame_idx = frame_idx.wrapping_add(1);
                let _ = terminal.draw(|f| {
                    let buf = f.buffer_mut();
                    if buf.area == base.area {
                        buf.clone_from(base);
                    }
                    render_busy(f, theme, spinner, label);
                });
            }
        }
    }
}

/// Overlay an animated `⟨spinner⟩ ⟨label⟩` progress line on the bottom row,
/// leaving the snapshotted chat visible above it.
fn render_busy(frame: &mut Frame, theme: &Theme, spinner: &str, label: &str) {
    let area = frame.area();
    if area.height == 0 {
        return;
    }
    let row = Rect {
        x: 0,
        y: area.height - 1,
        width: area.width,
        height: 1,
    };
    frame.render_widget(Clear, row);
    let line = Line::from(Span::styled(format!("{spinner} {label}"), theme.activity_style()));
    frame.render_widget(Paragraph::new(line), row);
}

/// Interactive `/save`: animate a phase-labelled spinner across the two slow
/// phases (schema learning, overview generation) instead of freezing the UI.
async fn handle_save_command<B: ratatui::backend::Backend>(
    app: &mut App,
    theme: &Theme,
    terminal: &mut Terminal<B>,
    args: &str,
) {
    let Some((profile, env)) = app.resolve_save_target(args) else {
        return;
    };
    if app.schema().is_none() {
        let base = snapshot_app(terminal, app, theme);
        let fut = app.introspect_future();
        match run_with_spinner(terminal, &base, theme, "Learning schema…", fut).await {
            Ok(model) => {
                let n = model.tables.len();
                app.set_schema(model);
                app.push_info(format!("learned {n} table(s)"));
            },
            Err(e) => {
                app.push_info(format!("learn failed: {e}"));
                return;
            },
        }
    }
    let Some(schema_md) = app.schema_markdown_current() else {
        app.push_info("no schema to save; connect and /learn first");
        return;
    };
    let base = snapshot_app(terminal, app, theme);
    let fut = app.overview_future(schema_md);
    let (agent, outcome) = run_with_spinner(terminal, &base, theme, "Generating overview…", fut).await;
    app.restore_agent(agent);
    if let Some(err) = outcome.error {
        app.push_info(format!("overview generation failed: {err}"));
    }
    if let Err(e) = app.finish_save(&profile, &env, &outcome.text) {
        app.push_info(format!("save failed: {e}"));
    }
}

/// Interactive `/learn`: introspect the schema with a progress spinner.
async fn handle_learn_command<B: ratatui::backend::Backend>(app: &mut App, theme: &Theme, terminal: &mut Terminal<B>) {
    let base = snapshot_app(terminal, app, theme);
    let fut = app.introspect_future();
    match run_with_spinner(terminal, &base, theme, "Learning schema…", fut).await {
        Ok(model) => {
            let n = model.tables.len();
            app.set_schema(model);
            app.push_info(format!("learned {n} table(s)"));
        },
        Err(e) => {
            app.push_info(format!("learn failed: {e}"));
        },
    }
}

/// Render the edit-mode frame for the live-`App` approver.
fn render_edit(frame: &mut Frame, app: &App, theme: &Theme, edit_buf: &str) {
    let size = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(size.height.saturating_sub(3)),
            Constraint::Length(1), // status bar
            Constraint::Length(1), // hint line
            Constraint::Length(1), // edit line
        ])
        .split(size);

    frame.render_widget(Paragraph::new("editing query (Enter to run, Esc to cancel)"), chunks[0]);

    {
        let bar = StatusBar {
            profile: app.profile_name.clone(),
            env: app.active_env.clone(),
            mode: app.mode(),
            in_transaction: false,
            tokens: app.usage().input_tokens + app.usage().output_tokens,
            cost_usd: estimate_cost_usd(app.usage()),
            mark: Some(app.logo().mark_span(theme.color)),
        };
        let buf = frame.buffer_mut();
        bar.render(theme, chunks[1], buf);
    }

    frame.render_widget(Paragraph::new("edit SQL:"), chunks[2]);
    frame.render_widget(Paragraph::new(Line::from(Span::raw(format!("> {edit_buf}")))), chunks[3]);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use naque_core::PermissionMode;
    use naque_llm::{AgentConfig, MockProvider};
    use naque_tui::ApprovalPrompt;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use tempfile::NamedTempFile;

    use super::*;
    use crate::app::App;
    use crate::approval::AutoApprove;

    async fn make_test_app() -> App {
        let tmp = NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let db = naque_db::Database::connect(&url).await.unwrap();
        let agent = naque_llm::Agent::new(
            Box::new(MockProvider::new(vec![])),
            AgentConfig {
                model: "mock".into(),
                max_iterations: 5,
                max_tokens: 512,
                system_preamble: "test".into(),
            },
        );
        App::new(db, agent, PermissionMode::Default, "testprofile", false, 100)
    }

    fn buf_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer().clone();
        let area = buf.area;
        let width = area.width;
        let height = area.height;
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = buf.cell((x, y)).unwrap();
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn cost_estimate_uses_opus_4_8_pricing() {
        // 1M input + 1M output → $5 + $25 = $30.
        let usage = naque_llm::Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        };
        let cost = estimate_cost_usd(&usage);
        assert!((cost - 30.0).abs() < 1e-6, "expected $30.00 for 1M+1M tokens, got {cost}");
    }

    #[tokio::test]
    async fn render_basic_frame() {
        let mut app = make_test_app().await;

        // Push transcript entries, including an inline result table.
        app.transcript.push(TranscriptEntry::User("hello world".into()));
        app.transcript.push(TranscriptEntry::Agent("hi there".into()));
        app.transcript.push(TranscriptEntry::Result {
            columns: vec!["id".into()],
            rows: vec![vec![Some("42".into())]],
            byte_columns: Vec::new(),
        });

        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from("draft input"), &[], None))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("testprofile"), "expected profile name in buffer:\n{text}");
        assert!(text.contains("hello world"), "expected transcript substring in buffer:\n{text}");
        assert!(text.contains("42"), "expected result cell value in buffer:\n{text}");
        assert!(text.contains("draft input"), "expected input in buffer:\n{text}");
    }

    #[tokio::test]
    async fn welcome_splash_shows_on_empty_session_then_clears() {
        let mut app = make_test_app().await;
        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // Fresh session: the welcome splash (tagline + wordmark glyphs) is shown.
        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from(""), &[], None))
            .unwrap();
        let text = buf_text(&terminal);
        assert!(text.contains("ask your database"), "welcome tagline should show on a fresh session:\n{text}");
        assert!(
            text.contains('\u{2588}') || text.contains('\u{2580}') || text.contains('\u{2584}'),
            "welcome wordmark glyphs should render:\n{text}"
        );

        // Once the conversation starts, the splash is replaced by the transcript.
        app.transcript.push(TranscriptEntry::User("hi".into()));
        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from(""), &[], None))
            .unwrap();
        let text = buf_text(&terminal);
        assert!(!text.contains("ask your database"), "welcome must clear once the transcript is non-empty:\n{text}");
    }

    #[tokio::test]
    async fn render_with_approval_prompt() {
        let app = make_test_app().await;
        let theme = Theme::new(false);

        let prompt =
            ApprovalPrompt::new("DROP TABLE foo".into(), "DDL: DROP".into(), None, naque_core::GateDecision::Prompt);

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from(""), &[], Some(&prompt)))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("DROP TABLE foo"), "expected SQL in approval prompt:\n{text}");
        assert!(text.contains("Accept"), "expected Accept option in approval prompt:\n{text}");
    }

    #[tokio::test]
    async fn render_with_result_after_query() {
        // Keep `_tmp` alive for the duration of the test so the file is not deleted.
        let tmp = NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let db = naque_db::Database::connect(&url).await.unwrap();
        let agent = naque_llm::Agent::new(
            Box::new(MockProvider::new(vec![])),
            AgentConfig {
                model: "mock".into(),
                max_iterations: 5,
                max_tokens: 512,
                system_preamble: "test".into(),
            },
        );
        let mut app = App::new(db, agent, PermissionMode::Wildcard, "testprofile", false, 100);

        // Seed data and run a query through the app.
        app.handle_line("!CREATE TABLE t(id INTEGER, val TEXT)", &mut AutoApprove)
            .await
            .unwrap();
        app.handle_line("!INSERT INTO t VALUES (1, 'hello')", &mut AutoApprove)
            .await
            .unwrap();
        app.handle_line("!SELECT * FROM t", &mut AutoApprove).await.unwrap();

        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from("my query"), &[], None))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("hello"), "expected 'hello' in buffer:\n{text}");
        assert!(text.contains("my query"), "expected input:\n{text}");
        drop(tmp);
    }

    #[tokio::test]
    async fn long_input_wraps_instead_of_scrolling_off_screen() {
        let mut app = make_test_app().await;
        app.transcript.push(TranscriptEntry::User("hi".into()));
        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        // A line far wider than the terminal. With horizontal scrolling only the
        // tail near the cursor would show; wrapping keeps the head visible too.
        let long = format!("HEAD{}TAIL", "x".repeat(120));
        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from(long.as_str()), &[], None))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("HEAD"), "wrapped input should keep the head visible:\n{text}");
        assert!(text.contains("TAIL"), "wrapped input should keep the tail visible:\n{text}");
    }

    #[tokio::test]
    async fn explicit_newlines_render_on_separate_rows() {
        let mut app = make_test_app().await;
        app.transcript.push(TranscriptEntry::User("hi".into()));
        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from("first line\nsecond line"), &[], None))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("first line"), "expected first input line:\n{text}");
        assert!(text.contains("second line"), "expected second input line:\n{text}");
    }

    #[tokio::test]
    async fn queued_lines_render_above_the_input() {
        let mut app = make_test_app().await;
        app.transcript.push(TranscriptEntry::User("hi".into()));
        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let queued = vec!["queued one".to_string(), "queued two".to_string()];
        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from("typing next"), &queued, None))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("queued one"), "expected first queued line:\n{text}");
        assert!(text.contains("queued two"), "expected second queued line:\n{text}");
        assert!(text.contains("typing next"), "expected live input alongside the queue:\n{text}");
    }

    #[tokio::test]
    async fn run_with_spinner_returns_future_output() {
        let theme = Theme::new(false);
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        // With `biased;` and the grace period, an already-ready future returns
        // its value immediately, drawing nothing.
        let base = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 10));
        let out = run_with_spinner(&mut terminal, &base, &theme, "Working…", async { 42usize }).await;
        assert_eq!(out, 42);
    }

    #[tokio::test(start_paused = true)]
    async fn run_with_spinner_skips_draw_within_grace_period() {
        let theme = Theme::new(false);
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        // A future that finishes before the grace period must never draw the
        // progress line. With a paused clock the grace timer never fires for an
        // already-ready future, so the buffer stays blank.
        let base = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 10));
        let out = run_with_spinner(&mut terminal, &base, &theme, "Learning schema…", async { 7usize }).await;
        assert_eq!(out, 7);
        assert!(!buf_text(&terminal).contains("Learning schema"), "fast op must not flash the progress line");
    }

    #[tokio::test(start_paused = true)]
    async fn run_with_spinner_keeps_base_visible() {
        let theme = Theme::new(false);
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).unwrap();

        // A base snapshot carrying distinctive chat content.
        let mut base = ratatui::buffer::Buffer::empty(ratatui::layout::Rect::new(0, 0, 40, 10));
        base.set_string(0, 0, "chat-marker", theme.dim_style());

        // The future completes only after the grace period elapses, so the
        // progress line is drawn at least once. `start_paused` auto-advances the
        // clock, so this is deterministic.
        let out = run_with_spinner(&mut terminal, &base, &theme, "Working…", async {
            tokio::time::sleep(SPINNER_GRACE * 2).await;
            1usize
        })
        .await;
        assert_eq!(out, 1);

        let text = buf_text(&terminal);
        assert!(text.contains("chat-marker"), "snapshotted chat must stay visible:\n{text}");
        assert!(text.contains("Working"), "progress label must overlay the chat:\n{text}");
    }
}

#[cfg(test)]
mod render_tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::*;

    fn buffer_text(t: &Terminal<TestBackend>) -> String {
        let buf = t.backend().buffer().clone();
        let area = buf.area;
        let width = area.width;
        let height = area.height;
        let mut out = String::new();
        for y in 0..height {
            for x in 0..width {
                let cell = buf.cell((x, y)).unwrap();
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }

    #[tokio::test]
    async fn renders_pinned_line_and_steps() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = crate::app::tests::make_app(&url, naque_core::PermissionMode::Wildcard, vec![]).await;

        app.live.running = true;
        app.live.current_action = Some("run_query".into());
        app.live.iteration = 1;
        app.transcript_mut().push(TranscriptEntry::Reasoning("checking orders".into()));
        app.transcript_mut().push(TranscriptEntry::ToolStep {
            name: "run_query".into(),
            detail: Some("SELECT count(*) FROM orders".into()),
            status: crate::app::StepStatus::Running,
            summary: None,
        });

        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::new(true);
        terminal
            .draw(|f| render(f, &app, &theme, &InputLine::from(""), &[], None))
            .unwrap();

        let text = buffer_text(&terminal);
        assert!(text.contains("run_query"), "pinned line/step missing: {text:?}");
        assert!(text.contains("^C to cancel"), "cancel hint missing");
        assert!(text.contains("checking orders"), "reasoning missing");
        assert!(
            text.contains("SELECT count(*) FROM orders"),
            "running query SQL must render in the transcript:\n{text}"
        );
        assert!(text.contains("│"), "SQL block gutter must render:\n{text}");
    }

    #[tokio::test]
    async fn renders_legibly_without_color() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = crate::app::tests::make_app(&url, naque_core::PermissionMode::Wildcard, vec![]).await;
        app.live.running = true;
        app.live.current_action = Some("run_query".into());
        app.live.iteration = 3;

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, &app, &Theme::new(false), &InputLine::from(""), &[], None))
            .unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("run_query") && text.contains("iter 3/"), "{text:?}");
    }

    #[tokio::test]
    async fn renders_quit_hint_only_when_armed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let url = format!("sqlite:{}", tmp.path().display());
        let mut app = crate::app::tests::make_app(&url, naque_core::PermissionMode::Wildcard, vec![]).await;

        let backend = TestBackend::new(80, 8);
        let mut terminal = Terminal::new(backend).unwrap();

        // Idle, not armed: no hint.
        terminal
            .draw(|f| render(f, &app, &Theme::new(true), &InputLine::from(""), &[], None))
            .unwrap();
        assert!(!buffer_text(&terminal).contains("again to exit"), "hint must not show when idle");

        // After a first idle Ctrl+C: the hint appears.
        app.quit_armed = true;
        terminal
            .draw(|f| render(f, &app, &Theme::new(true), &InputLine::from(""), &[], None))
            .unwrap();
        assert!(buffer_text(&terminal).contains("again to exit"), "hint must show when quit_armed");
    }

    #[test]
    fn render_busy_shows_label() {
        let theme = Theme::new(false);
        let height = 10u16;
        let backend = TestBackend::new(60, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_busy(f, &theme, "⠋", "Learning schema…")).unwrap();

        // The progress line lives on the bottom row, not a centered modal.
        let buf = terminal.backend().buffer().clone();
        let bottom: String = (0..buf.area.width)
            .map(|x| buf.cell((x, height - 1)).unwrap().symbol().to_string())
            .collect();
        assert!(
            bottom.contains("Learning schema"),
            "progress line must show its label on the bottom row: {bottom:?}"
        );
    }

    #[test]
    fn render_busy_handles_one_by_one() {
        let theme = Theme::new(false);
        let backend = TestBackend::new(1, 1);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_busy(f, &theme, "⠋", "Learning schema…")).unwrap();
    }

    #[test]
    fn slash_prefix_detects_command_word() {
        assert_eq!(slash_suggest_prefix("/mo"), Some("mo"));
        assert_eq!(slash_suggest_prefix("/"), Some(""));
        assert_eq!(slash_suggest_prefix("/mode wildcard"), None, "a space ends the command word");
        assert_eq!(slash_suggest_prefix("hello"), None);
        assert_eq!(slash_suggest_prefix("!SELECT 1"), None);
    }

    #[test]
    fn complete_slash_appends_space_only_for_arg_commands() {
        let mode = naque_tui::SLASH_COMMANDS.iter().find(|c| c.name == "mode").unwrap();
        assert_eq!(complete_slash(mode), "/mode ");
        let help = naque_tui::SLASH_COMMANDS.iter().find(|c| c.name == "help").unwrap();
        assert_eq!(complete_slash(help), "/help");
    }

    #[test]
    fn agent_answer_splits_into_aligned_lines() {
        let entry = TranscriptEntry::Agent("line one\nline two".into());
        let lines = transcript_lines(&entry, &Theme::new(false), false, false, 80);
        assert_eq!(lines.len(), 2, "a two-line answer must render as two lines");
        let text = |l: &Line| -> String { l.spans.iter().map(|s| s.content.as_ref()).collect() };
        assert_eq!(text(&lines[0]), " ai: line one");
        assert_eq!(text(&lines[1]), "     line two", "continuation aligns under the body");
    }

    fn line_text(l: &Line) -> String {
        l.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn truncate_cols_boundaries() {
        assert_eq!(truncate_cols("hello", 0), "");
        assert_eq!(truncate_cols("hello", 1), "…");
        assert_eq!(truncate_cols("hi", 2), "hi", "exact fit is unchanged");
        assert_eq!(truncate_cols("hello", 3), "he…");
    }

    #[test]
    fn tool_step_running_sql_block_caps_and_expands() {
        let theme = Theme::new(false);
        let sql = "SELECT a\nFROM t\nWHERE x\nGROUP BY a\nHAVING c\nORDER BY a\nLIMIT 10";
        // 7 logical lines; SQL_PREVIEW_LINES = 5.

        // Collapsed: header + 5 block lines + a "+2 more" hint.
        let lines =
            tool_step_lines("run_query", Some(sql), &crate::app::StepStatus::Running, None, false, false, 80, &theme);
        assert_eq!(line_text(&lines[0]), "  ▸ run_query");
        assert_eq!(line_text(&lines[1]), "  │ SELECT a");
        assert_eq!(lines.len(), 1 + 5 + 1, "header + 5 block + hint");
        assert!(line_text(&lines[6]).contains("+2 more"), "{:?}", line_text(&lines[6]));
        assert!(line_text(&lines[6]).contains("ctrl+r to expand"));

        // Expanded: header + all 7 block lines + collapse hint.
        let lines =
            tool_step_lines("run_query", Some(sql), &crate::app::StepStatus::Running, None, true, false, 80, &theme);
        assert_eq!(lines.len(), 1 + 7 + 1, "header + 7 block + collapse hint");
        assert_eq!(line_text(&lines[7]), "  │ LIMIT 10");
        assert!(line_text(&lines[8]).contains("ctrl+r to collapse"));
    }

    #[test]
    fn tool_step_truncates_long_line_to_width() {
        let theme = Theme::new(false);
        let long = "X".repeat(200);
        let lines =
            tool_step_lines("run_query", Some(&long), &crate::app::StepStatus::Running, None, false, false, 40, &theme);
        // gutter "  │ " is 4 cols, so content fits in 36 incl. the ellipsis.
        let block = line_text(&lines[1]);
        assert!(block.chars().count() <= 40, "line exceeds width: {}", block.chars().count());
        assert!(block.ends_with('…'));
    }

    #[test]
    fn tool_step_selected_marker_and_finished_forms() {
        let theme = Theme::new(false);
        // Selected running step gets the ❯ marker on the header.
        let lines = tool_step_lines(
            "run_query",
            Some("SELECT 1"),
            &crate::app::StepStatus::Running,
            None,
            false,
            true,
            80,
            &theme,
        );
        assert!(line_text(&lines[0]).starts_with("❯ "), "{:?}", line_text(&lines[0]));

        // Finished SQL step, collapsed: a single summary one-liner, no block.
        let lines = tool_step_lines(
            "run_query",
            Some("SELECT 1"),
            &crate::app::StepStatus::Ok,
            Some("12 rows"),
            false,
            false,
            80,
            &theme,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "  ✓ run_query 12 rows");

        // Finished SQL step, expanded: header + block + summary footer.
        let lines = tool_step_lines(
            "run_query",
            Some("SELECT 1"),
            &crate::app::StepStatus::Ok,
            Some("12 rows"),
            true,
            false,
            80,
            &theme,
        );
        assert_eq!(line_text(&lines[0]), "  ✓ run_query");
        assert_eq!(line_text(&lines[1]), "  │ SELECT 1");
        assert_eq!(line_text(&lines[2]), "  └ 12 rows");
    }

    #[test]
    fn tool_step_target_tools_show_inline_detail() {
        let theme = Theme::new(false);
        let l = tool_step_lines(
            "inspect_table",
            Some("orders"),
            &crate::app::StepStatus::Running,
            None,
            false,
            false,
            80,
            &theme,
        );
        assert_eq!(line_text(&l[0]), "  ▸ inspect_table orders");
        assert_eq!(l.len(), 1, "target tools never render a block");

        let l = tool_step_lines(
            "sample_table",
            Some("users \u{00B7} limit 10"),
            &crate::app::StepStatus::Running,
            None,
            false,
            false,
            80,
            &theme,
        );
        assert_eq!(line_text(&l[0]), "  ▸ sample_table users \u{00B7} limit 10");
    }

    #[test]
    fn result_entry_previews_rows_and_expands() {
        let theme = Theme::new(false);
        let cols = vec!["n".to_string()];
        let rows: Vec<Vec<Option<String>>> = (0..25).map(|i| vec![Some(i.to_string())]).collect();
        let entry = TranscriptEntry::Result {
            columns: cols,
            rows,
            byte_columns: Vec::new(),
        };

        // Collapsed: header + sep + 10 rows + "+15 more" hint = 13 lines.
        let lines = transcript_lines(&entry, &theme, false, false, 80);
        assert_eq!(lines.len(), 2 + RESULT_PREVIEW_ROWS + 1);
        assert!(line_text(lines.last().unwrap()).contains("+15 more rows"), "{:?}", line_text(lines.last().unwrap()));
        assert!(line_text(lines.last().unwrap()).contains("ctrl+r to expand"));

        // Expanded: header + sep + 25 rows + collapse hint = 28 lines.
        let lines = transcript_lines(&entry, &theme, true, false, 80);
        assert_eq!(lines.len(), 2 + 25 + 1);
        assert!(line_text(lines.last().unwrap()).contains("ctrl+r to collapse"));

        // Selected: the header row carries the ❯ marker.
        let lines = transcript_lines(&entry, &theme, false, true, 80);
        assert!(line_text(&lines[0]).starts_with("❯ "), "{:?}", line_text(&lines[0]));
    }

    #[test]
    fn reveal_window_positions_selection() {
        // Everything fits: window starts at 0.
        assert_eq!(reveal_window(8, 10, 3, 2), 0);
        // Short selection in the middle is bottom-aligned: [42, 52) shows [50, 52).
        assert_eq!(reveal_window(100, 10, 50, 2), 42);
        // Selection at the very end reproduces the tail window.
        assert_eq!(reveal_window(100, 10, 98, 2), 90);
        // Selection taller than the viewport is top-aligned at its first line.
        assert_eq!(reveal_window(100, 10, 30, 20), 30);
    }

    #[test]
    fn suggest_popup_draws_matches_above_input() {
        let sg = naque_tui::SlashSuggest::new(naque_tui::matching("c"), 0); // clear, cost
        let backend = TestBackend::new(60, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| render_suggest_popup(f, &Theme::new(false), &sg)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("/clear"), "popup should list /clear:\n{text}");
        assert!(text.contains("/cost"), "popup should list /cost:\n{text}");
    }
}
