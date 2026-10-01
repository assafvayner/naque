//! IPC payload types and the managed state shared across Tauri commands.
//!
//! The pump owns the live `naque::App`; commands reach it through an
//! unbounded channel held in [`AppState`]. Snapshots carry the transcript +
//! status for the frontend to render, plus the currently pending approval
//! (if any) so a reloaded view can restore an open prompt modal.

use std::sync::Mutex;

use tokio::sync::mpsc;

use crate::pump::EngineCommand;

/// Backend state managed by Tauri: the command channel to the pump (set once
/// the engine connects) and the pending launch config from CLI args (consumed
/// by the first `auto_connect`).
pub struct AppState {
    pending_launch: Mutex<Option<naque::setup::LaunchConfig>>,
    engine_tx: Mutex<Option<mpsc::UnboundedSender<EngineCommand>>>,
}

impl AppState {
    pub fn new(launch: naque::setup::LaunchConfig) -> Self {
        Self {
            pending_launch: Mutex::new(Some(launch)),
            engine_tx: Mutex::new(None),
        }
    }

    /// Consume and return the pending launch config (the first `auto_connect`
    /// claim). Subsequent calls return `None` so the frontend falls back to the
    /// in-app launcher picker.
    pub fn take_pending_launch(&self) -> Option<naque::setup::LaunchConfig> {
        self.pending_launch.lock().unwrap().take()
    }

    /// Install the pump's command sender (called once when the engine connects).
    pub fn set_sender(&self, tx: mpsc::UnboundedSender<EngineCommand>) {
        *self.engine_tx.lock().unwrap() = Some(tx);
    }

    /// A cloned sender to post a command to the pump, or `None` when the engine
    /// is not connected yet (the frontend should show the launcher).
    pub fn sender(&self) -> Option<mpsc::UnboundedSender<EngineCommand>> {
        self.engine_tx.lock().unwrap().clone()
    }
}

/// Authoritative render snapshot emitted to the frontend. The frontend replaces
/// its whole view from this on every non-streaming transition; live `TextDelta`
/// chunks arrive separately as `agent_event` for smooth streaming between
/// snapshots.
#[derive(serde::Serialize, Clone)]
pub struct StateSnapshot {
    pub transcript: Vec<naque::TranscriptEntry>,
    pub mode: String,
    pub profile: String,
    pub engine: String,
    pub usage: naque_llm::Usage,
    pub turn_running: bool,
    pub can_start_turn: bool,
    pub iteration: u32,
    pub max_iterations: u32,
    pub awaiting_approval: bool,
    pub pending_approval: Option<ApprovalRequestDto>,
    pub pending_path: Option<PathRequestDto>,
    /// Monotonic tick while a turn runs (drives the frontend's activity
    /// spinner). Only advances on the heartbeat pulse, so the frontend's own
    /// CSS animation is the source of smoothness; this just confirms the pump
    /// is alive.
    pub spinner: u32,
}

/// A gated SQL statement awaiting the user's accept/edit/reject decision. The
/// non-serializable `oneshot` reply stays backend-side, keyed by `id`.
#[derive(serde::Serialize, Clone)]
pub struct ApprovalRequestDto {
    pub id: u64,
    pub sql: String,
    pub label: String,
    pub catastrophic: bool,
}

/// A filesystem read/list grant request awaiting the user's once/session/deny.
#[derive(serde::Serialize, Clone)]
pub struct PathRequestDto {
    pub id: u64,
    pub path: String,
    pub action: String,
}

/// One AI provider's id/label and whether a key is currently available (env
/// var or keyring). Ollama needs no key, so it reports `configured = true`.
#[derive(serde::Serialize, Clone)]
pub struct LlmProviderStatus {
    pub id: String,
    pub label: String,
    pub configured: bool,
}

/// Snapshot of configured providers + the one auto-detection would select, so
/// the launcher can show what's ready and disable Connect until a key exists.
#[derive(serde::Serialize, Clone)]
pub struct LlmStatusDto {
    pub providers: Vec<LlmProviderStatus>,
    pub active: Option<String>,
}
