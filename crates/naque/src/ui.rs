//! TUI rendering and terminal event loop.

use anyhow::Context;
use ratatui::{
    backend::CrosstermBackend,
    crossterm::{
        event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
        terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
        ExecutableCommand,
    },
    layout::{Constraint, Direction, Layout, Rect},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;

use naque_core::gate::GateDecision;
use naque_tui::{ApprovalChoice, ApprovalPrompt, ResultTable, StatusBar, Theme};

use crate::app::{App, TranscriptEntry};
use crate::approval::{ApprovalDecision, Approver};

/// Rough cost estimate for the default model (claude-opus-4-8):
/// $5 per 1M input tokens, $25 per 1M output tokens.
fn estimate_cost_usd(usage: &naque_llm::Usage) -> f64 {
    (usage.input_tokens as f64 / 1_000_000.0) * 5.0
        + (usage.output_tokens as f64 / 1_000_000.0) * 25.0
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

/// Render one frame of the main UI.
///
/// Layout (top to bottom):
/// 1. Transcript area — scrollable list of history entries.
/// 2. Result table — last query result, if any.
/// 3. Approval prompt — overlaid when `pending` is Some.
/// 4. Status bar — single line.
/// 5. Input line — `> {input}`.
pub fn render(
    frame: &mut Frame,
    app: &App,
    theme: &Theme,
    input: &str,
    pending: Option<&ApprovalPrompt>,
) {
    let size = frame.area();

    // Determine heights: input = 1, status = 1, result = up to 8 if present,
    // transcript = remainder. The approval prompt is no longer a layout band —
    // it is drawn as a centered modal popup over the whole screen (below).
    let has_result = app.last_result().is_some();

    let result_height: u16 = if has_result { 8 } else { 0 };
    let fixed_bottom: u16 = 1 + 1; // status + input
    let transcript_height = size.height.saturating_sub(fixed_bottom + result_height);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(transcript_height),
            Constraint::Length(result_height),
            Constraint::Length(1), // status bar
            Constraint::Length(1), // input line
        ])
        .split(size);

    // ---- Transcript --------------------------------------------------------
    {
        let lines: Vec<Line> = app
            .transcript()
            .iter()
            .flat_map(|entry| transcript_lines(entry, theme))
            .collect();

        // Bottom-align: show the most recent entries adjacent to the result /
        // input area. Render only the last N lines that fit, and anchor them to
        // the bottom of the transcript chunk so there is no large empty gap at
        // the top when the history is short.
        let chunk = chunks[0];
        let visible = chunk.height as usize;
        let start = lines.len().saturating_sub(visible);
        let tail: Vec<Line> = lines[start..].to_vec();
        let used = tail.len() as u16;
        let pad = chunk.height.saturating_sub(used);
        let anchored = Rect {
            x: chunk.x,
            y: chunk.y + pad,
            width: chunk.width,
            height: used,
        };

        let para = Paragraph::new(tail)
            .block(Block::default())
            .wrap(Wrap { trim: false });
        frame.render_widget(para, anchored);
    }

    // ---- Result table -------------------------------------------------------
    if let (Some(result), true) = (app.last_result(), has_result) {
        let table = ResultTable::new(
            result.columns.iter().map(|c| c.name.clone()).collect(),
            result.rows.clone(),
        );
        let buf = frame.buffer_mut();
        table.render(theme, chunks[1], buf);
    }

    // ---- Status bar --------------------------------------------------------
    {
        let usage = app.usage();
        let tokens = usage.input_tokens + usage.output_tokens;
        let cost_usd = estimate_cost_usd(usage);

        let bar = StatusBar {
            profile: app.profile_name.clone(),
            mode: app.mode(),
            in_transaction: false,
            tokens,
            cost_usd,
        };
        let buf = frame.buffer_mut();
        bar.render(theme, chunks[2], buf);
    }

    // ---- Input line --------------------------------------------------------
    {
        let line = Line::from(Span::raw(format!("> {input}")));
        frame.render_widget(Paragraph::new(line), chunks[3]);
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

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Approval required ");
        let inner = block.inner(modal);
        frame.render_widget(block, modal);

        let buf = frame.buffer_mut();
        prompt.render(theme, inner, buf);
    }
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

    Rect {
        x,
        y,
        width,
        height,
    }
}

/// Build the styled transcript line(s) for one [`TranscriptEntry`].
///
/// `Sql` entries get their `[label]` badge colored by classification via
/// [`Theme::label_style`], so read/write/DDL/catastrophic statements are
/// visually distinguishable (and degrade correctly under NO_COLOR).
fn transcript_lines<'a>(entry: &'a TranscriptEntry, theme: &Theme) -> Vec<Line<'a>> {
    match entry {
        TranscriptEntry::User(text) => {
            vec![Line::from(Span::raw(format!("you: {text}")))]
        }
        TranscriptEntry::Agent(text) => {
            vec![Line::from(Span::raw(format!(" ai: {text}")))]
        }
        TranscriptEntry::Sql { sql, label } => {
            vec![Line::from(vec![
                Span::raw("sql["),
                Span::styled(label.clone(), theme.label_style(label)),
                Span::raw(format!("]: {sql}")),
            ])]
        }
        TranscriptEntry::Info(text) => {
            vec![Line::from(Span::raw(format!("inf: {text}")))]
        }
        TranscriptEntry::Error(text) => {
            vec![Line::from(Span::raw(format!("err: {text}")))]
        }
        TranscriptEntry::Rejected(sql) => {
            vec![Line::from(Span::raw(format!("rej: {sql}")))]
        }
    }
}

// ---------------------------------------------------------------------------
// TuiApprover
// ---------------------------------------------------------------------------

/// An [`Approver`] that draws the approval prompt into a running terminal and
/// waits for a keyboard decision.
///
/// Owns a `&mut Terminal` for the duration of the approval, so the calling
/// loop must provide it for each prompt.
pub struct TuiApprover<'a, B: ratatui::backend::Backend> {
    pub terminal: &'a mut Terminal<B>,
    pub app: &'a App,
    pub theme: &'a Theme,
    pub input: &'a str,
}

impl<B: ratatui::backend::Backend + Send> Approver for TuiApprover<'_, B> {
    fn approve(&mut self, sql: &str, label: &str, decision: GateDecision) -> ApprovalDecision {
        let catastrophic = match decision {
            GateDecision::PromptCatastrophic => {
                // Use a placeholder CatastrophicReason; the actual reason is embedded
                // in the gate logic. We pass None for simplicity since the label
                // already identifies the statement.
                None
            }
            _ => None,
        };

        let mut prompt =
            ApprovalPrompt::new(sql.to_string(), label.to_string(), catastrophic, decision);

        // Draw loop: render the frame with the pending prompt, wait for a key.
        loop {
            {
                let app = self.app;
                let theme = self.theme;
                let input = self.input;
                let prompt_ref = &prompt;
                let _ = self
                    .terminal
                    .draw(|f| render(f, app, theme, input, Some(prompt_ref)));
            }

            // Read a key event (blocking).
            if let Ok(Event::Key(key)) = event::read() {
                // Ctrl-C during approval → reject.
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return ApprovalDecision::Reject;
                }
                if let Some(choice) = prompt.handle_key(key) {
                    return match choice {
                        ApprovalChoice::Accept => ApprovalDecision::Accept,
                        ApprovalChoice::Reject => ApprovalDecision::Reject,
                        ApprovalChoice::Edit => {
                            // Inline edit: pre-fill with the current SQL. Esc cancels → Reject.
                            let app = self.app;
                            let theme = self.theme;
                            match inline_edit(self.terminal, sql, |f, buf| {
                                render_edit(f, app, theme, buf);
                            }) {
                                Some(edited) => ApprovalDecision::AcceptEdited(edited),
                                None => ApprovalDecision::Reject,
                            }
                        }
                    };
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Terminal runner
// ---------------------------------------------------------------------------

/// Enter raw mode + alternate screen, run the interactive loop, then restore.
///
/// The `runtime` is passed so we can `block_on` the async `handle_line`
/// without requiring `run` to be async (which would complicate terminal
/// restore on error).
pub fn run(mut app: App, theme: Theme, runtime: &tokio::runtime::Runtime) -> anyhow::Result<()> {
    // Enter raw mode and alternate screen.
    enable_raw_mode().context("enable raw mode")?;
    io::stdout()
        .execute(EnterAlternateScreen)
        .context("enter alternate screen")?;

    // RAII guard: from this point on, the terminal is ALWAYS restored on drop —
    // whether `run` returns early via `?`, completes normally, or unwinds on panic.
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    // The guard's Drop restores the terminal once `terminal`/`_guard` go out of scope.
    event_loop(&mut app, &theme, &mut terminal, runtime)
}

/// Restores the terminal (leaves raw mode + alternate screen) when dropped.
///
/// Constructing this immediately after `enable_raw_mode` + `EnterAlternateScreen`
/// guarantees cleanup on every exit path — normal return, `?` propagation, or panic.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

fn event_loop<B: ratatui::backend::Backend + Send>(
    app: &mut App,
    theme: &Theme,
    terminal: &mut Terminal<B>,
    runtime: &tokio::runtime::Runtime,
) -> anyhow::Result<()> {
    let mut input_buf = String::new();

    loop {
        // Draw frame.
        {
            let input = &input_buf;
            terminal.draw(|f| render(f, app, theme, input, None))?;
        }

        // Wait for an event.
        let event = event::read().context("read terminal event")?;

        match event {
            Event::Key(KeyEvent {
                code: KeyCode::Char('c'),
                modifiers,
                ..
            }) if modifiers.contains(KeyModifiers::CONTROL) => {
                // Ctrl-C → quit.
                break;
            }

            Event::Key(KeyEvent {
                code: KeyCode::Enter,
                ..
            }) => {
                let line = std::mem::take(&mut input_buf);
                if line.trim().is_empty() {
                    continue;
                }

                // Build a TuiApprover that borrows the terminal.
                // We need to split the borrow: `app` is mutably borrowed by
                // handle_line, and the approver needs a reference to `app` for
                // rendering. To avoid the conflict we pass a snapshot of what
                // the approver needs (profile_name, mode, theme) rather than
                // borrowing `app` itself inside the approver. We accomplish this
                // by using a simple struct that captures those cheaply-cloned fields.
                let profile_snap = app.profile_name.clone();
                let mode_snap = app.mode();
                let usage_snap = app.usage().clone();

                // Temporary app snapshot for the approver to render from.
                // We use a Delayed-draw approach: the approver renders a static
                // frame (no live transcript updates during approval). This avoids
                // the borrow conflict entirely.
                let snap_app = AppSnapshot {
                    profile_name: profile_snap,
                    mode: mode_snap,
                    usage: usage_snap,
                };

                let mut approver = SnapshotApprover {
                    terminal,
                    snap: &snap_app,
                    theme,
                    input: "",
                };

                runtime.block_on(app.handle_line(&line, &mut approver))?;

                if app.should_quit() {
                    break;
                }
            }

            Event::Key(KeyEvent {
                code: KeyCode::Backspace,
                ..
            }) => {
                input_buf.pop();
            }

            Event::Key(KeyEvent {
                code: KeyCode::Char(c),
                ..
            }) => {
                input_buf.push(c);
            }

            _ => {}
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Snapshot approver — renders a static frame during the approval mini-loop.
// ---------------------------------------------------------------------------

/// A lightweight, borrow-safe snapshot of the fields needed for approval
/// rendering without holding a live borrow on `App`.
struct AppSnapshot {
    profile_name: String,
    mode: naque_core::PermissionMode,
    usage: naque_llm::Usage,
}

/// An [`Approver`] that draws approval prompts using an [`AppSnapshot`] so
/// that the live `App` borrow can remain with `handle_line`.
struct SnapshotApprover<'a, B: ratatui::backend::Backend> {
    terminal: &'a mut Terminal<B>,
    snap: &'a AppSnapshot,
    theme: &'a Theme,
    input: &'a str,
}

impl<B: ratatui::backend::Backend + Send> Approver for SnapshotApprover<'_, B> {
    fn approve(&mut self, sql: &str, label: &str, decision: GateDecision) -> ApprovalDecision {
        let mut prompt = ApprovalPrompt::new(sql.to_string(), label.to_string(), None, decision);

        loop {
            {
                let snap = self.snap;
                let theme = self.theme;
                let input = self.input;
                let prompt_ref = &prompt;
                let _ = self.terminal.draw(|f| {
                    render_snapshot(f, snap, theme, input, Some(prompt_ref));
                });
            }

            if let Ok(Event::Key(key)) = event::read() {
                if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return ApprovalDecision::Reject;
                }
                if let Some(choice) = prompt.handle_key(key) {
                    return match choice {
                        ApprovalChoice::Accept => ApprovalDecision::Accept,
                        ApprovalChoice::Reject => ApprovalDecision::Reject,
                        ApprovalChoice::Edit => {
                            // Inline edit: pre-fill with the current SQL. Esc cancels → Reject.
                            let snap = self.snap;
                            let theme = self.theme;
                            match inline_edit(self.terminal, sql, |f, buf| {
                                render_snapshot_edit(f, snap, theme, buf);
                            }) {
                                Some(edited) => ApprovalDecision::AcceptEdited(edited),
                                None => ApprovalDecision::Reject,
                            }
                        }
                    };
                }
            }
        }
    }
}

/// Render a frame using an [`AppSnapshot`] (no live transcript or result).
fn render_snapshot(
    frame: &mut Frame,
    snap: &AppSnapshot,
    theme: &Theme,
    input: &str,
    pending: Option<&ApprovalPrompt>,
) {
    let size = frame.area();
    let transcript_height = size.height.saturating_sub(2);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(transcript_height),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(size);

    // Blank transcript area.
    frame.render_widget(Paragraph::new(""), chunks[0]);

    // Status bar.
    {
        let tokens = snap.usage.input_tokens + snap.usage.output_tokens;
        let cost_usd = estimate_cost_usd(&snap.usage);
        let bar = StatusBar {
            profile: snap.profile_name.clone(),
            mode: snap.mode,
            in_transaction: false,
            tokens,
            cost_usd,
        };
        let buf = frame.buffer_mut();
        bar.render(theme, chunks[1], buf);
    }

    // Input line.
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(format!("> {input}")))),
        chunks[2],
    );

    // Approval prompt — centered modal popup (drawn last, on top).
    if let Some(prompt) = pending {
        let modal = centered_modal_rect(size, prompt);
        frame.render_widget(Clear, modal);

        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Approval required ");
        let inner = block.inner(modal);
        frame.render_widget(block, modal);

        let buf = frame.buffer_mut();
        prompt.render(theme, inner, buf);
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

        if let Ok(Event::Key(key)) = event::read() {
            match key.code {
                KeyCode::Enter => {
                    let trimmed = buf.trim();
                    if trimmed.is_empty() {
                        // Empty edit cancels rather than running nothing.
                        return None;
                    }
                    return Some(trimmed.to_string());
                }
                KeyCode::Esc => return None,
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return None;
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
        }
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

    frame.render_widget(
        Paragraph::new("editing query (Enter to run, Esc to cancel)"),
        chunks[0],
    );

    {
        let bar = StatusBar {
            profile: app.profile_name.clone(),
            mode: app.mode(),
            in_transaction: false,
            tokens: app.usage().input_tokens + app.usage().output_tokens,
            cost_usd: estimate_cost_usd(app.usage()),
        };
        let buf = frame.buffer_mut();
        bar.render(theme, chunks[1], buf);
    }

    frame.render_widget(Paragraph::new("edit SQL:"), chunks[2]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(format!("> {edit_buf}")))),
        chunks[3],
    );
}

/// Render the edit-mode frame for the snapshot approver.
fn render_snapshot_edit(frame: &mut Frame, snap: &AppSnapshot, theme: &Theme, edit_buf: &str) {
    let size = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(size.height.saturating_sub(3)),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(size);

    frame.render_widget(
        Paragraph::new("editing query (Enter to run, Esc to cancel)"),
        chunks[0],
    );

    {
        let bar = StatusBar {
            profile: snap.profile_name.clone(),
            mode: snap.mode,
            in_transaction: false,
            tokens: snap.usage.input_tokens + snap.usage.output_tokens,
            cost_usd: estimate_cost_usd(&snap.usage),
        };
        let buf = frame.buffer_mut();
        bar.render(theme, chunks[1], buf);
    }

    frame.render_widget(Paragraph::new("edit SQL:"), chunks[2]);
    frame.render_widget(
        Paragraph::new(Line::from(Span::raw(format!("> {edit_buf}")))),
        chunks[3],
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use naque_core::PermissionMode;
    use naque_llm::{AgentConfig, MockProvider};
    use naque_tui::ApprovalPrompt;
    use ratatui::{backend::TestBackend, Terminal};
    use tempfile::NamedTempFile;

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
        App::new(
            db,
            agent,
            PermissionMode::Default,
            "testprofile",
            false,
            100,
        )
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
        assert!(
            (cost - 30.0).abs() < 1e-6,
            "expected $30.00 for 1M+1M tokens, got {cost}"
        );
    }

    #[tokio::test]
    async fn render_basic_frame() {
        let mut app = make_test_app().await;

        // Push transcript entries.
        app.transcript
            .push(TranscriptEntry::User("hello world".into()));
        app.transcript
            .push(TranscriptEntry::Agent("hi there".into()));

        // Set a result (fabricate a QueryResult).
        app.last_result = Some(naque_db::QueryResult {
            columns: vec![naque_db::Column {
                name: "id".into(),
                type_name: "integer".into(),
            }],
            rows: vec![vec![Some("42".into())]],
            rows_affected: None,
        });

        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, &app, &theme, "draft input", None))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(
            text.contains("testprofile"),
            "expected profile name in buffer:\n{text}"
        );
        assert!(
            text.contains("hello world"),
            "expected transcript substring in buffer:\n{text}"
        );
        assert!(
            text.contains("42"),
            "expected result cell value in buffer:\n{text}"
        );
        assert!(
            text.contains("draft input"),
            "expected input in buffer:\n{text}"
        );
    }

    #[tokio::test]
    async fn render_with_approval_prompt() {
        let app = make_test_app().await;
        let theme = Theme::new(false);

        let prompt = ApprovalPrompt::new(
            "DROP TABLE foo".into(),
            "DDL: DROP".into(),
            None,
            naque_core::GateDecision::Prompt,
        );

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, &app, &theme, "", Some(&prompt)))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(
            text.contains("DROP TABLE foo"),
            "expected SQL in approval prompt:\n{text}"
        );
        assert!(
            text.contains("Accept"),
            "expected Accept option in approval prompt:\n{text}"
        );
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
        let mut app = App::new(
            db,
            agent,
            PermissionMode::Wildcard,
            "testprofile",
            false,
            100,
        );

        // Seed data and run a query through the app.
        app.handle_line("!CREATE TABLE t(id INTEGER, val TEXT)", &mut AutoApprove)
            .await
            .unwrap();
        app.handle_line("!INSERT INTO t VALUES (1, 'hello')", &mut AutoApprove)
            .await
            .unwrap();
        app.handle_line("!SELECT * FROM t", &mut AutoApprove)
            .await
            .unwrap();

        let theme = Theme::new(false);
        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, &app, &theme, "my query", None))
            .unwrap();

        let text = buf_text(&terminal);
        assert!(
            text.contains("hello"),
            "expected 'hello' in buffer:\n{text}"
        );
        assert!(text.contains("my query"), "expected input:\n{text}");
        drop(tmp);
    }
}
