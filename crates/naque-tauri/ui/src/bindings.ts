// IPC bindings: typed wrappers over the Tauri commands and events the naque
// backend exposes. Rust enums serialize (serde default) as externally-tagged
// objects — `{ Variant: payload }` for newtype/struct variants, and the bare
// string `"Variant"` for unit variants — which the union types below model.

import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';

export type Usage = { input_tokens: number; output_tokens: number };
export type StepStatus = 'Running' | 'Ok' | 'Err';

export type TranscriptEntry =
  | { User: string }
  | { Agent: string }
  | { Sql: { sql: string; label: string } }
  | { Info: string }
  | { Error: string }
  | { Rejected: string }
  | { Reasoning: string }
  | { ToolStep: { name: string; detail: string | null; status: StepStatus; summary: string | null } }
  | { Result: { columns: string[]; rows: (string | null)[][]; byte_columns: number[] } };

export type AgentEvent =
  | 'TurnStarted'
  | 'Cancelled'
  | { LlmCallStarted: { iteration: number } }
  | { TextDelta: string }
  | { ToolCallStarted: { name: string; detail: string | null } }
  | { ToolCallFinished: { name: string; summary: string; is_error: boolean } }
  | { UsageUpdated: Usage }
  | { TurnFinished: { iterations: number; hit_iteration_cap: boolean } };

export interface ApprovalRequestDto {
  id: number;
  sql: string;
  label: string;
  catastrophic: boolean;
}
export interface PathRequestDto {
  id: number;
  path: string;
  action: string;
}

export interface StateSnapshot {
  transcript: TranscriptEntry[];
  mode: string;
  profile: string;
  engine: string;
  usage: Usage;
  turn_running: boolean;
  can_start_turn: boolean;
  iteration: number;
  max_iterations: number;
  awaiting_approval: boolean;
  pending_approval: ApprovalRequestDto | null;
  pending_path: PathRequestDto | null;
  spinner: number;
}

export interface LaunchConfig {
  profile?: string | null;
  env?: string | null;
  url?: string | null;
  mode?: string | null;
  provider?: string | null;
  model?: string | null;
  no_guard?: boolean;
}

export interface LlmProviderStatus {
  id: string;
  label: string;
  configured: boolean;
}
export interface LlmStatusDto {
  providers: LlmProviderStatus[];
  active: string | null;
}

export const api = {
  submit: (line: string) => invoke<void>('submit', { line }),
  cancelTurn: () => invoke<void>('cancel_turn'),
  respondApproval: (id: number, accept: boolean, editedSql: string | null) =>
    invoke<void>('respond_approval', { id, accept, editedSql }),
  respondPathApproval: (id: number, grant: string) =>
    invoke<void>('respond_path_approval', { id, grant }),
  listProfiles: () => invoke<string[]>('list_profiles'),
  listEnvironments: (profile: string) => invoke<string[]>('list_environments', { profile }),
  switchProfile: (profile: string, env: string) => invoke<void>('switch_profile', { profile, env }),
  getState: () => invoke<StateSnapshot>('get_state'),
  autoConnect: () => invoke<boolean>('auto_connect'),
  connect: (launch: LaunchConfig) => invoke<void>('connect', { launch }),
  launcherProfiles: () => invoke<string[]>('launcher_profiles'),
  launcherEnvironments: (profile: string) => invoke<string[]>('launcher_environments', { profile }),
  llmStatus: () => invoke<LlmStatusDto>('llm_status'),
  setLlmCredentials: (provider: string, apiKey: string) =>
    invoke<void>('set_llm_credentials', { provider, apiKey }),
  clearLlmCredentials: (provider: string) => invoke<void>('clear_llm_credentials', { provider }),
};

export const onState = (cb: (s: StateSnapshot) => void): Promise<UnlistenFn> =>
  listen<StateSnapshot>('state', (e) => cb(e.payload));
export const onAgentEvent = (cb: (ev: AgentEvent) => void): Promise<UnlistenFn> =>
  listen<AgentEvent>('agent_event', (e) => cb(e.payload));
export const onQuit = (cb: () => void): Promise<UnlistenFn> => listen('quit', () => cb());

// Variant discriminator for the externally-tagged transcript union.
export function entryKind(e: TranscriptEntry): string {
  return Object.keys(e)[0];
}