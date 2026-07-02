//! The engine pump: a single background task that owns the live `naque::App`
//! and mirrors the terminal's `event_loop` over Tauri IPC.
//!
//! Commands arrive on an unbounded channel (the frontend posts them via
//! `tauri::command`); streamed agent events are emitted as `agent_event` and
//! authoritative snapshots as `state`. Approvals keep their `oneshot` reply
//! sender backend-side, keyed by an id the frontend echoes back.

use std::collections::HashMap;
use std::time::Duration;

use naque::App;
use naque::approval::{ApprovalRequest, PathApprovalRequest};
use naque_core::gate::{GateDecision, QueryKind};
use naque_db::Engine;
use naque_llm::AgentEvent;
use naque_tui::{Input, route_input};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::{mpsc, oneshot};

use crate::ipc::{AppState, ApprovalRequestDto, PathRequestDto, StateSnapshot};

/// A frontend→pump command. Those carrying a `reply` oneshot return a value to
/// the invoking `tauri::command`.
pub enum EngineCommand {
    Submit {
        line: String,
    },
    CancelTurn,
    RespondApproval {
        id: u64,
        decision: naque::ApprovalDecision,
    },
    RespondPath {
        id: u64,
        grant: naque::PathGrant,
    },
    ListProfiles {
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    ListEnvironments {
        profile: String,
        reply: oneshot::Sender<Result<Vec<String>, String>>,
    },
    SwitchProfile {
        profile: String,
        env: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
    GetState {
        reply: oneshot::Sender<Result<StateSnapshot, String>>,
    },
}

/// Connect a freshly built engine: spawn the pump and register its command
/// channel so `tauri::command` handlers can reach it.
pub fn start(handle: AppHandle, app: App) {
    let (tx, rx) = mpsc::unbounded_channel::<EngineCommand>();
    let state = handle.state::<AppState>();
    state.set_sender(tx);
    tauri::async_runtime::spawn(pump(app, rx, handle));
}

/// One step of progress on the in-flight turn — mirrors `ui::turn_step` so the
/// `&mut App` borrow stays usable from a `select!` arm.
enum TurnStep {
    Event(AgentEvent),
    Finished,
}

async fn turn_step(app: &mut App) -> TurnStep {
    loop {
        if app.poll_finished() {
            return TurnStep::Finished;
        }
        tokio::select! {
            biased;
            maybe = app.next_event() => match maybe {
                Some(ev) => return TurnStep::Event(ev),
                None => return TurnStep::Finished,
            },
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
    }
}

/// The pump loop. Owns `app`; processes commands and drains streamed events,
/// emitting `agent_event` (every event) and `state` (on non-streaming
/// transitions) plus a `quit` notice when the engine requests exit.
async fn pump(mut app: App, mut rx: mpsc::UnboundedReceiver<EngineCommand>, handle: AppHandle) {
    // Initial snapshot so the frontend renders the status bar + welcome.
    let snap = build_snapshot(&app, &HashMap::new(), &HashMap::new(), 0).await;
    let _ = handle.emit("state", snap);

    let mut pending_approval: HashMap<u64, ApprovalRequest> = HashMap::new();
    let mut pending_path: HashMap<u64, PathApprovalRequest> = HashMap::new();
    let mut next_id: u64 = 0;
    let mut queued: Vec<String> = Vec::new();
    // Heartbeat: while a turn runs, re-emit `state` on a steady cadence so the
    // frontend always has fresh feedback even when the agent is silent (e.g.
    // waiting on a slow LLM round-trip). The TUI gets this implicitly from its
    // 80ms redraw; without it the GUI would freeze between events. The branch
    // body intentionally leaves `emit = true` so the existing post-select
    // `emit_state` does the work (and avoids borrowing `app` while `turn_step`
    // holds `&mut app`).
    let mut heartbeat = tokio::time::interval(Duration::from_millis(250));
    let mut tick_count: u32 = 0;

    loop {
        let mut emit = true;
        tokio::select! {
            biased;
            cmd = rx.recv() => match cmd {
                Some(EngineCommand::Submit { line }) => {
                    if app.is_turn_running() {
                        queued.push(line);
                    } else {
                        submit(&mut app, &line).await;
                    }
                }
                Some(EngineCommand::CancelTurn) => app.cancel_turn(),
                Some(EngineCommand::RespondApproval { id, decision }) => {
                    if let Some(req) = pending_approval.remove(&id) {
                        let _ = req.reply.send(decision);
                    }
                }
                Some(EngineCommand::RespondPath { id, grant }) => {
                    if let Some(req) = pending_path.remove(&id) {
                        let _ = req.reply.send(grant);
                    }
                }
                Some(EngineCommand::ListProfiles { reply }) => {
                    let r = app.list_profiles().map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                Some(EngineCommand::ListEnvironments { profile, reply }) => {
                    let r = app.list_environments(&profile).map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                Some(EngineCommand::SwitchProfile { profile, env, reply }) => {
                    let r = app.switch_to(&profile, &env).await.map_err(|e| e.to_string());
                    let _ = reply.send(r);
                }
                Some(EngineCommand::GetState { reply }) => {
                    let snap = build_snapshot(&app, &pending_approval, &pending_path, tick_count).await;
                    let _ = reply.send(Ok(snap));
                }
                None => break,
            },
            step = turn_step(&mut app),
                if app.is_turn_running() && pending_approval.is_empty() && pending_path.is_empty() =>
            {
                match step {
                    TurnStep::Event(ev) => {
                        let is_text = matches!(&ev, AgentEvent::TextDelta(_));
                        app.apply_event(&ev);
                        let _ = handle.emit("agent_event", &ev);
                        if is_text {
                            // TextDeltas stream rapidly; the next non-text event
                            // emits a full authoritative snapshot, so skip the
                            // heavier `state` emit here to stay low-chattiness.
                            emit = false;
                        }
                    },
                    TurnStep::Finished => {
                        app.finalize_turn().await;
                        // Drain queued submissions in order (mirrors the TUI):
                        // commands run inline; the first natural-language line
                        // starts a new turn, so stop and let it stream.
                        while !app.is_turn_running() {
                            let Some(line) = queued.first().cloned() else { break; };
                            queued.remove(0);
                            submit(&mut app, &line).await;
                            if app.should_quit() {
                                break;
                            }
                        }
                    },
                }
            }
            // Periodic re-emit during a turn for live feedback (see `heartbeat`
            // above). Gated on the turn running + no pending prompt so it stays
            // idle when the agent is parked on an approval.
            _ = heartbeat.tick(),
                if app.is_turn_running() && pending_approval.is_empty() && pending_path.is_empty() =>
            {
                tick_count = tick_count.wrapping_add(1);
            }
        }

        if app.should_quit() {
            let _ = handle.emit("quit", ());
            break;
        }

        // Surface a pending approval from the running turn (mirrors the TUI's
        // try_recv_approval). The agent awaits one tool reply at a time, so at
        // most one SQL prompt and one path prompt are live together.
        if pending_approval.is_empty()
            && pending_path.is_empty()
            && let Some(req) = app.try_recv_approval()
        {
            let id = next_id;
            next_id += 1;
            pending_approval.insert(id, req);
        } else if !app.is_turn_running()
            && let Some((_, req)) = pending_approval.drain().next()
        {
            // Turn ended (e.g. cancelled) while a prompt was up — reject it.
            let _ = req.reply.send(naque::ApprovalDecision::Reject);
        }

        if pending_approval.is_empty()
            && let Some(req) = app.try_recv_path_approval()
        {
            let id = next_id;
            next_id += 1;
            pending_path.insert(id, req);
        } else if !app.is_turn_running()
            && let Some((_, req)) = pending_path.drain().next()
        {
            let _ = req.reply.send(naque::PathGrant::Deny);
        }

        if emit {
            emit_state(&handle, &app, &pending_approval, &pending_path, tick_count).await;
        }
    }
}

async fn emit_state(
    handle: &AppHandle,
    app: &App,
    pending_approval: &HashMap<u64, ApprovalRequest>,
    pending_path: &HashMap<u64, PathApprovalRequest>,
    spinner: u32,
) {
    let snap = build_snapshot(app, pending_approval, pending_path, spinner).await;
    let _ = handle.emit("state", snap);
}

async fn build_snapshot(
    app: &App,
    pending_approval: &HashMap<u64, ApprovalRequest>,
    pending_path: &HashMap<u64, PathApprovalRequest>,
    spinner: u32,
) -> StateSnapshot {
    let engine = match app.engine().await {
        Engine::Postgres => "postgres",
        Engine::Sqlite => "sqlite",
    };
    StateSnapshot {
        transcript: app.transcript().to_vec(),
        mode: app.mode().to_string(),
        profile: app.profile_name().to_string(),
        engine: engine.to_string(),
        usage: app.usage().clone(),
        turn_running: app.is_turn_running(),
        can_start_turn: app.can_start_turn(),
        iteration: app.iteration(),
        max_iterations: app.max_iterations(),
        awaiting_approval: !pending_approval.is_empty() || !pending_path.is_empty(),
        pending_approval: pending_approval.iter().next().map(|(id, r)| ApprovalRequestDto {
            id: *id,
            sql: r.sql.clone(),
            label: r.label.clone(),
            catastrophic: matches!(r.decision, GateDecision::PromptCatastrophic),
        }),
        pending_path: pending_path.iter().next().map(|(id, r)| PathRequestDto {
            id: *id,
            path: r.path.clone(),
            action: r.action.clone(),
        }),
        spinner,
    }
}

/// Route a submitted line exactly like the terminal's `dispatch_line`: NL goes
/// through the spawned streaming path; raw SQL runs inline when the gate would
/// auto-approve (parity with the TUI, which does not yet modal-approve raw
/// SQL); `\` and `/` commands run inline.
async fn submit(app: &mut App, line: &str) {
    match route_input(line) {
        Input::NaturalLanguage(text) => {
            if app.can_start_turn() {
                app.start_turn(&text);
            } else {
                app.push_info("a turn is already running; wait for it to finish or cancel it.");
            }
        },
        Input::RawSql(sql) => {
            if app.raw_sql_auto_approves(&sql).await {
                let _ = app.execute_sql(&sql, QueryKind::Primary, &mut naque::AutoApprove).await;
            } else {
                app.push_info(format!(
                    "Raw SQL needs approval in {} mode; switch to /mode wildcard or ask in natural language.",
                    app.mode()
                ));
            }
        },
        Input::DbCommand(cmd) => {
            let _ = app.handle_db_command(&cmd).await;
        },
        Input::ToolCommand(cmd) => {
            let _ = app.handle_tool_command(&cmd, &mut naque::AutoApprove).await;
        },
        Input::Empty => {},
    }
}
