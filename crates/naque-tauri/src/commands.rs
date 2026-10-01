//! Tauri commands — the frontend→backend IPC surface.
//!
//! Thin wrappers that post an [`EngineCommand`](crate::pump::EngineCommand) to
//! the pump (or, for `connect`/`launcher_*`, build the engine / read the
//! profile store directly). The pump owns the live `App`, so every command that
//! touches the engine round-trips through it.

use naque::{ApprovalDecision, PathGrant};
use tauri::{AppHandle, State};
use tokio::sync::oneshot;

use crate::ipc::AppState;
use crate::pump::{self, EngineCommand};

fn send(state: &AppState, cmd: EngineCommand) -> Result<(), String> {
    let tx = state.sender().ok_or_else(|| "not connected".to_string())?;
    tx.send(cmd).map_err(|_| "engine disconnected".to_string())
}

#[tauri::command]
pub async fn submit(state: State<'_, AppState>, line: String) -> Result<(), String> {
    send(&state, EngineCommand::Submit { line })
}

#[tauri::command]
pub async fn cancel_turn(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, EngineCommand::CancelTurn)
}

/// Resolve an approval prompt. `accept` with `edited_sql` rewrites the SQL
/// (`AcceptEdited`); `accept` without it runs as-is; `!accept` rejects.
#[tauri::command]
pub async fn respond_approval(
    state: State<'_, AppState>,
    id: u64,
    accept: bool,
    edited_sql: Option<String>,
) -> Result<(), String> {
    let decision = match (accept, edited_sql) {
        (true, Some(sql)) => ApprovalDecision::AcceptEdited(sql),
        (true, None) => ApprovalDecision::Accept,
        (false, _) => ApprovalDecision::Reject,
    };
    send(&state, EngineCommand::RespondApproval { id, decision })
}

#[tauri::command]
pub async fn respond_path_approval(state: State<'_, AppState>, id: u64, grant: String) -> Result<(), String> {
    let g = match grant.as_str() {
        "once" => PathGrant::Once,
        "session" => PathGrant::Session,
        _ => PathGrant::Deny,
    };
    send(&state, EngineCommand::RespondPath { id, grant: g })
}

#[tauri::command]
pub async fn list_profiles(state: State<'_, AppState>) -> Result<Vec<String>, String> {
    let (s, r) = oneshot::channel();
    send(&state, EngineCommand::ListProfiles { reply: s })?;
    r.await.map_err(|_| "engine dropped reply".to_string())?
}

#[tauri::command]
pub async fn list_environments(state: State<'_, AppState>, profile: String) -> Result<Vec<String>, String> {
    let (s, r) = oneshot::channel();
    send(&state, EngineCommand::ListEnvironments { profile, reply: s })?;
    r.await.map_err(|_| "engine dropped reply".to_string())?
}

#[tauri::command]
pub async fn switch_profile(state: State<'_, AppState>, profile: String, env: String) -> Result<(), String> {
    let (s, r) = oneshot::channel();
    send(&state, EngineCommand::SwitchProfile { profile, env, reply: s })?;
    r.await.map_err(|_| "engine dropped reply".to_string())?
}

#[tauri::command]
pub async fn get_state(state: State<'_, AppState>) -> Result<crate::ipc::StateSnapshot, String> {
    let (s, r) = oneshot::channel();
    send(&state, EngineCommand::GetState { reply: s })?;
    r.await.map_err(|_| "engine dropped reply".to_string())?
}

/// Attempt the launch config captured from CLI args. Returns `true` when a
/// connection resolved and the engine started; `Err` means no connection was
/// found and the frontend should show the in-app launcher picker.
#[tauri::command]
pub async fn auto_connect(state: State<'_, AppState>, app_handle: AppHandle) -> Result<bool, String> {
    let Some(launch) = state.take_pending_launch() else {
        return Err("no connection configured; use the launcher".to_string());
    };
    match naque::setup::build_app(&launch).await {
        Ok(app) => {
            pump::start(app_handle, app);
            Ok(true)
        },
        Err(e) => Err(format!("{e:#}")),
    }
}

/// Connect from an explicit launch config chosen in the in-app launcher.
#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    launch: naque::setup::LaunchConfig,
) -> Result<(), String> {
    if state.sender().is_some() {
        return Err("already connected; restart to change connection".to_string());
    }
    let app = naque::setup::build_app(&launch).await.map_err(|e| format!("{e:#}"))?;
    pump::start(app_handle, app);
    Ok(())
}

/// Profiles available before any engine is connected (for the launcher picker).
#[tauri::command]
pub async fn launcher_profiles() -> Result<Vec<String>, String> {
    let store = open_store()?;
    store.list_profiles().map_err(|e| e.to_string())
}

/// Environments within a profile (for the launcher picker).
#[tauri::command]
pub async fn launcher_environments(profile: String) -> Result<Vec<String>, String> {
    let store = open_store()?;
    let p = store
        .load_profile(&profile)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "profile not found".to_string())?;
    Ok(p.environments.keys().cloned().collect())
}

fn open_store() -> Result<naque_profile::Store, String> {
    let home = naque_profile::Store::default_home().unwrap_or_else(|| ".naque".into());
    Ok(naque_profile::Store::open(home))
}

/// LLM provider key management. The GUI can't read shell env vars, so API keys
/// entered in Settings are stored in the system keyring (service `"naque"`),
/// the same store used for database passwords. `build_app` resolves env→keyring,
/// so both sources work.
#[tauri::command]
pub async fn llm_status() -> Result<crate::ipc::LlmStatusDto, String> {
    use naque_profile::{Secrets, SystemSecrets, detect_provider, resolve_api_key};

    let providers = [
        ("claude", "Anthropic Claude", true),
        ("openai", "OpenAI", true),
        ("gemini", "Google Gemini", true),
        ("hf", "Hugging Face", true),
        ("ollama", "Ollama (local, no key)", false),
    ]
    .into_iter()
    .map(|(id, label, needs_key)| crate::ipc::LlmProviderStatus {
        id: id.to_string(),
        label: label.to_string(),
        configured: if needs_key {
            resolve_api_key(id, &SystemSecrets).is_some()
        } else {
            true
        },
    })
    .collect();
    Ok(crate::ipc::LlmStatusDto {
        providers,
        active: detect_provider(&SystemSecrets as &dyn Secrets),
    })
}

#[tauri::command]
pub async fn set_llm_credentials(provider: String, api_key: String) -> Result<(), String> {
    naque_profile::set_api_key(&provider, &api_key).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn clear_llm_credentials(provider: String) -> Result<(), String> {
    naque_profile::clear_api_key(&provider).map_err(|e| e.to_string())
}
