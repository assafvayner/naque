//! TUI rendering and terminal event loop.

use std::io;
use std::time::Duration;

use anyhow::Context;
use futures_util::StreamExt;
use naque_tui::{ActivityLine, ApprovalPrompt, ResultTable, StatusBar, Theme};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::ExecutableCommand;
use ratatui::crossterm::event::{Event, EventStream, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
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

/// Render one frame of the main UI.
///
/// Layout (top to bottom):
/// 1. Transcript area — scrollable list of history entries.
/// 2. Result table — last query result, if any.
/// 3. Activity line — pinned spinner row while a turn runs (height 0 when idle).
/// 4. Approval prompt — overlaid when `pending` is Some.
/// 5. Status bar — single line.
/// 6. Input line — `> {input}`.
pub fn render(frame: &mut Frame, app: &App, theme: &Theme, input: &str, pending: Option<&ApprovalPrompt>) {
    let size = frame.area();

    // Determine heights: input = 1, status = 1, result = up to 8 if present,
    // activity = 1 while running (0 when idle), transcript = remainder.
    // The approval prompt is no longer a layout band — it is drawn as a
    // centered modal popup over the whole screen (below).
    let has_result = app.last_result().is_some();

    let result_height: u16 = if has_result { 8 } else { 0 };
    let activity_height: u16 = if app.live.running { 1 } else { 0 };
    let fixed_bottom: u16 = 1 + 1; // status + input
    let transcript_height = size.height.saturating_sub(fixed_bottom + result_height + activity_height);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(transcript_height),
            Constraint::Length(result_height),
            Constraint::Length(activity_height),
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
        // the top when the history is short. scroll_offset shifts the view up
        // from the tail (0 = following tail).
        let chunk = chunks[0];
        let visible = chunk.height as usize;
        let off = app.live.scroll_offset as usize;
        let end = lines.len().saturating_sub(off);
        let start = end.saturating_sub(visible);
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

    // ---- Result table -------------------------------------------------------
    if let (Some(result), true) = (app.last_result(), has_result) {
        let table = ResultTable::new(result.columns.iter().map(|c| c.name.clone()).collect(), result.rows.clone());
        let buf = frame.buffer_mut();
        table.render(theme, chunks[1], buf);
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
        line.render(theme, chunks[2], buf);
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
        bar.render(theme, chunks[3], buf);
    }

    // ---- Input line --------------------------------------------------------
    {
        let line = Line::from(Span::raw(format!("> {input}")));
        frame.render_widget(Paragraph::new(line), chunks[4]);

        // After a first idle Ctrl+C, prompt the user that another press exits.
        if app.quit_armed {
            let hint = "press ^C again to exit";
            let hint_w = hint.len() as u16;
            let input_chunk = chunks[4];
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
fn transcript_lines<'a>(entry: &'a TranscriptEntry, theme: &Theme) -> Vec<Line<'a>> {
    match entry {
        TranscriptEntry::User(text) => {
            vec![Line::from(Span::raw(format!("you: {text}")))]
        },
        TranscriptEntry::Agent(text) => {
            vec![Line::from(Span::raw(format!(" ai: {text}")))]
        },
        TranscriptEntry::Sql { sql, label } => {
            vec![Line::from(vec![
                Span::raw("sql["),
                Span::styled(label.clone(), theme.label_style(label)),
                Span::raw(format!("]: {sql}")),
            ])]
        },
        TranscriptEntry::Info(text) => {
            vec![Line::from(Span::raw(format!("inf: {text}")))]
        },
        TranscriptEntry::Error(text) => {
            vec![Line::from(Span::raw(format!("err: {text}")))]
        },
        TranscriptEntry::Rejected(sql) => {
            vec![Line::from(Span::raw(format!("rej: {sql}")))]
        },
        TranscriptEntry::Reasoning(text) => {
            vec![Line::from(Span::styled(format!("  · {text}"), theme.dim_style()))]
        },
        TranscriptEntry::ToolStep {
            name,
            sql,
            status,
            summary,
        } => {
            let (glyph, glyph_style) = match status {
                crate::app::StepStatus::Running => ("▸", theme.activity_style()),
                crate::app::StepStatus::Ok => ("✓", theme.label_style("read-only")),
                crate::app::StepStatus::Err => ("✗", theme.label_style("DDL: DROP")),
            };
            let detail = match status {
                crate::app::StepStatus::Running => sql.clone().unwrap_or_default(),
                _ => summary.clone().or_else(|| sql.clone()).unwrap_or_default(),
            };
            vec![Line::from(vec![
                Span::raw("  "),
                Span::styled(glyph, glyph_style),
                Span::raw(format!(" {name} ")),
                Span::styled(detail, theme.dim_style()),
            ])]
        },
    }
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
        let _ = disable_raw_mode();
        let _ = io::stdout().execute(LeaveAlternateScreen);
    }
}

async fn event_loop<B: ratatui::backend::Backend + Send>(
    app: &mut App,
    theme: &Theme,
    terminal: &mut Terminal<B>,
) -> anyhow::Result<()> {
    let mut input_buf = String::new();
    let mut events = EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(80));
    let mut pending: Option<(ApprovalRequest, ApprovalPrompt)> = None;

    loop {
        {
            let prompt_ref = pending.as_ref().map(|(_, p)| p);
            terminal.draw(|f| render(f, app, theme, &input_buf, prompt_ref))?;
        }

        tokio::select! {
            maybe_ev = events.next() => {
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

                match key.code {
                    KeyCode::Enter if !app.is_turn_running() => {
                        let line = std::mem::take(&mut input_buf);
                        if line.trim().is_empty() { continue; }
                        dispatch_line(app, &line).await;
                        if app.should_quit() { break; }
                    },
                    KeyCode::PageUp => {
                        app.live.follow = false;
                        app.live.scroll_offset = app.live.scroll_offset.saturating_add(5);
                    },
                    KeyCode::PageDown => {
                        app.live.scroll_offset = app.live.scroll_offset.saturating_sub(5);
                        if app.live.scroll_offset == 0 {
                            app.live.follow = true;
                            app.live.new_below = 0;
                        }
                    },
                    KeyCode::End => {
                        app.live.scroll_offset = 0;
                        app.live.follow = true;
                        app.live.new_below = 0;
                    },
                    KeyCode::Backspace if !app.is_turn_running() => { input_buf.pop(); },
                    KeyCode::Char(c) if !app.is_turn_running() => { input_buf.push(c); },
                    _ => {},
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
                    },
                }
            }
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

/// Route a submitted line. NL goes through the spawned streaming path; commands
/// and raw SQL run inline (fast, no streaming).
async fn dispatch_line(app: &mut App, line: &str) {
    use naque_tui::{Input, route_input};
    match route_input(line) {
        Input::NaturalLanguage(text) => {
            // `start_turn` records an explicit error when the agent is
            // unavailable, so the user always sees why nothing happened.
            app.start_turn(&text);
        },
        Input::RawSql(sql) => {
            // Raw SQL runs inline. If the gate would auto-approve (e.g. wildcard,
            // or a read in readonly), run it directly. If it would prompt, we
            // cannot show the modal from this inline path yet (Task 4.7b wires
            // raw-SQL approval through the modal) — surface an explicit message
            // instead of silently approving or rejecting.
            if app.raw_sql_auto_approves(&sql).await {
                let _ = app
                    .execute_sql(&sql, naque_core::gate::QueryKind::Primary, &mut crate::approval::AutoApprove)
                    .await;
            } else {
                app.push_info(format!(
                    "Raw SQL needs approval in {} mode; modal approval for raw SQL is coming in the next step. \
                     Use `/mode wildcard` to run it now, or ask in natural language.",
                    app.mode()
                ));
            }
        },
        Input::DbCommand(cmd) => {
            let _ = app.handle_db_command(&cmd).await;
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
            mode: app.mode(),
            in_transaction: false,
            tokens: app.usage().input_tokens + app.usage().output_tokens,
            cost_usd: estimate_cost_usd(app.usage()),
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

        // Push transcript entries.
        app.transcript.push(TranscriptEntry::User("hello world".into()));
        app.transcript.push(TranscriptEntry::Agent("hi there".into()));

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

        terminal.draw(|f| render(f, &app, &theme, "draft input", None)).unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("testprofile"), "expected profile name in buffer:\n{text}");
        assert!(text.contains("hello world"), "expected transcript substring in buffer:\n{text}");
        assert!(text.contains("42"), "expected result cell value in buffer:\n{text}");
        assert!(text.contains("draft input"), "expected input in buffer:\n{text}");
    }

    #[tokio::test]
    async fn render_with_approval_prompt() {
        let app = make_test_app().await;
        let theme = Theme::new(false);

        let prompt =
            ApprovalPrompt::new("DROP TABLE foo".into(), "DDL: DROP".into(), None, naque_core::GateDecision::Prompt);

        let backend = TestBackend::new(80, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal.draw(|f| render(f, &app, &theme, "", Some(&prompt))).unwrap();

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

        terminal.draw(|f| render(f, &app, &theme, "my query", None)).unwrap();

        let text = buf_text(&terminal);
        assert!(text.contains("hello"), "expected 'hello' in buffer:\n{text}");
        assert!(text.contains("my query"), "expected input:\n{text}");
        drop(tmp);
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
            sql: Some("SELECT count(*) FROM orders".into()),
            status: crate::app::StepStatus::Running,
            summary: None,
        });

        let backend = TestBackend::new(80, 12);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::new(true);
        terminal.draw(|f| render(f, &app, &theme, "", None)).unwrap();

        let text = buffer_text(&terminal);
        assert!(text.contains("run_query"), "pinned line/step missing: {text:?}");
        assert!(text.contains("^C to cancel"), "cancel hint missing");
        assert!(text.contains("checking orders"), "reasoning missing");
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
        terminal.draw(|f| render(f, &app, &Theme::new(false), "", None)).unwrap();
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
        terminal.draw(|f| render(f, &app, &Theme::new(true), "", None)).unwrap();
        assert!(!buffer_text(&terminal).contains("again to exit"), "hint must not show when idle");

        // After a first idle Ctrl+C: the hint appears.
        app.quit_armed = true;
        terminal.draw(|f| render(f, &app, &Theme::new(true), "", None)).unwrap();
        assert!(buffer_text(&terminal).contains("again to exit"), "hint must show when quit_armed");
    }
}
