<script lang="ts">
  import { onMount } from 'svelte';
  import {
    api,
    onState,
    onAgentEvent,
    onQuit,
    type StateSnapshot,
    type TranscriptEntry,
    type AgentEvent,
    type LaunchConfig,
    type LlmStatusDto,
    entryKind,
  } from './bindings';
  import { renderProse, renderCell, escapeHtml } from './format';

  // --- connection + view state -------------------------------------------
  let connected = false;
  let connecting = false;
  let error: string | null = null;
  let snapshot: StateSnapshot | null = null;
  let transcript: TranscriptEntry[] = [];

  // launcher (pre-connect) picker
  let launcherOpen = false;
  let launcherProfiles: string[] = [];
  let launcherEnvs: string[] = [];
  let launcherProfile = '';
  let launcherEnv = '';
  let urlInput = '';

  // LLM provider settings (API keys live in the system keyring — the GUI can't
  // see shell env vars, so keys entered here are stored under service "naque"
  // and resolved by build_app at connect time).
  let llmStatus: LlmStatusDto | null = null;
  let settingsProvider = 'claude';
  let settingsKey = '';
  let settingsSaved = false;

  // in-session switcher (post-connect)
  let switcherOpen = false;
  let switchProfiles: string[] = [];
  let switchEnvs: string[] = [];
  let switchProfile = '';
  let switchEnv = '';

  // approval modals
  let pendingApproval = snapshot?.pending_approval ?? null;
  let pendingPath = snapshot?.pending_path ?? null;
  let editingApproval = false;
  let editedSql = '';

  // tool-step expansion (click to reveal SQL / full summary)
  let expanded: Set<number> = new Set();

  // input + slash autocomplete
  let input = '';
  let suggestIndex = 0;

  const SLASH_COMMANDS: { name: string; args: string; help: string }[] = [
    { name: 'help', args: '', help: 'show this help' },
    { name: 'mode', args: '<mode>', help: 'set permission mode: default | readonly | strict | wildcard' },
    { name: 'learn', args: '', help: 'introspect the database schema' },
    { name: 'clear', args: '', help: 'clear the chat window and the agent memory' },
    { name: 'cost', args: '', help: 'show token usage so far' },
    { name: 'export', args: '<csv|json>', help: 'export the last result' },
    { name: 'profile', args: '', help: 'switch profile + environment (picker)' },
    { name: 'env', args: '', help: 'switch environment within the current profile' },
    { name: 'save', args: '[profile] [env]', help: 'save current connection, schema & context' },
    { name: 'context', args: '[note]', help: 'show the context doc, or append a note' },
    { name: 'allow-dir', args: '<path-or-glob>', help: 'grant the agent filesystem read access (this session)' },
    { name: 'quit', args: '', help: 'exit naque' },
    { name: 'exit', args: '', help: 'exit naque' },
  ];

  function slashPrefix(text: string): string | null {
    const rest = text.trimStart().startsWith('/') ? text.trimStart().slice(1) : null;
    if (rest === null || rest.includes(' ')) return null;
    return rest;
  }
  $: suggestions = slashPrefix(input) === null ? [] : SLASH_COMMANDS.filter((c) => c.name.startsWith(slashPrefix(input)!));

  // --- lifecycle ---------------------------------------------------------
  onMount(async () => {
    await Promise.all([
      onState((s) => {
        snapshot = s;
        transcript = [...s.transcript];
        pendingApproval = s.pending_approval;
        pendingPath = s.pending_path;
        if (pendingApproval) {
          editingApproval = false;
          editedSql = pendingApproval.sql;
        }
      }),
      onAgentEvent((ev) => applyAgentEvent(ev)),
      onQuit(() => {
        connected = false;
        error = 'session ended';
      }),
    ]);
    try {
      const ok = await api.autoConnect();
      if (ok) connected = true;
    } catch (e) {
      connected = false;
      openLauncher();
    }
  });

  // Live-stream TextDeltas between authoritative snapshots; other event
  // variants trigger a `state` snapshot that reconciles, so only TextDelta is
  // folded locally (mirrors the TUI's `apply_event_to_transcript` for it).
  function applyAgentEvent(ev: AgentEvent) {
    if (typeof ev === 'object' && 'TextDelta' in ev) {
      const chunk = ev.TextDelta;
      const last = transcript[transcript.length - 1];
      if (last && entryKind(last) === 'Reasoning') {
        transcript = [...transcript.slice(0, -1), { Reasoning: last.Reasoning + chunk }];
      } else {
        transcript = [...transcript, { Reasoning: chunk }];
      }
    }
  }

  // --- launcher ----------------------------------------------------------
  async function openLauncher() {
    error = null;
    launcherOpen = true;
    try {
      launcherProfiles = await api.launcherProfiles();
    } catch (e) {
      launcherProfiles = [];
    }
    await refreshLlmStatus();
  }

  async function refreshLlmStatus() {
    try {
      llmStatus = await api.llmStatus();
    } catch (e) {
      llmStatus = null;
    }
  }

  async function saveLlmKey() {
    if (!settingsKey.trim()) return;
    try {
      await api.setLlmCredentials(settingsProvider, settingsKey.trim());
      settingsKey = '';
      settingsSaved = true;
      await refreshLlmStatus();
    } catch (e) {
      error = String(e);
    }
  }

  async function clearLlmKey(provider: string) {
    try {
      await api.clearLlmCredentials(provider);
      if (settingsProvider === provider) settingsKey = '';
      await refreshLlmStatus();
    } catch (e) {
      error = String(e);
    }
  }

  async function pickLauncherProfile(p: string) {
    launcherProfile = p;
    launcherEnv = '';
    try {
      launcherEnvs = await api.launcherEnvironments(p);
      if (launcherEnvs.length) launcherEnv = launcherEnvs[0];
    } catch (e) {
      launcherEnvs = [];
    }
  }

  async function connectLaunch() {
    connecting = true;
    error = null;
    try {
      const launch: LaunchConfig = urlInput.trim()
        ? { url: urlInput.trim() }
        : { profile: launcherProfile, env: launcherEnv };
      await api.connect(launch);
      connected = true;
      launcherOpen = false;
    } catch (e) {
      error = String(e);
    } finally {
      connecting = false;
    }
  }

  // --- in-session switcher ----------------------------------------------
  async function openSwitcher() {
    switcherOpen = true;
    switchProfiles = await api.listProfiles();
    if (snapshot) switchProfile = snapshot.profile;
    switchEnvs = switchProfile ? await api.listEnvironments(switchProfile) : [];
    switchEnv = switchEnvs[0] ?? '';
  }
  async function pickSwitchProfile(p: string) {
    switchProfile = p;
    switchEnvs = await api.listEnvironments(p);
    switchEnv = switchEnvs[0] ?? '';
  }
  async function switchTo() {
    try {
      await api.switchProfile(switchProfile, switchEnv);
      switcherOpen = false;
    } catch (e) {
      error = String(e);
    }
  }

  // --- input -------------------------------------------------------------
  async function submit() {
    const line = input;
    if (!line.trim()) return;
    const trimmed = line.trim();
    if (trimmed === '/profile' || trimmed === '/env' || trimmed.startsWith('/profile ') || trimmed.startsWith('/env ')) {
      input = '';
      openSwitcher();
      return;
    }
    input = '';
    suggestions = [];
    try {
      await api.submit(line);
    } catch (e) {
      error = String(e);
    }
  }

  function onInputKey(event: KeyboardEvent) {
    const target = event.target as HTMLTextAreaElement;
    // Enter submits; Shift/Alt+Enter inserts a newline (TUI parity).
    if (event.key === 'Enter' && !event.shiftKey && !event.altKey) {
      event.preventDefault();
      submit();
      return;
    }
    if (suggestions.length) {
      if (event.key === 'Tab') {
        event.preventDefault();
        const cmd = suggestions[suggestIndex];
        input = cmd.args ? `/${cmd.name} ` : `/${cmd.name}`;
        suggestions = [];
        return;
      }
      if (event.key === 'ArrowDown') {
        event.preventDefault();
        suggestIndex = Math.min(suggestIndex + 1, suggestions.length - 1);
        return;
      }
      if (event.key === 'ArrowUp') {
        event.preventDefault();
        suggestIndex = Math.max(suggestIndex - 1, 0);
        return;
      }
    }
    // Reset popup highlight while editing the command word.
    void target;
    suggestIndex = 0;
  }

  function complete(name: string) {
    const cmd = SLASH_COMMANDS.find((c) => c.name === name)!;
    input = cmd.args ? `/${cmd.name} ` : `/${cmd.name}`;
    suggestions = [];
  }

  // --- approvals ---------------------------------------------------------
  async function resolveApproval(accept: boolean) {
    if (!pendingApproval) return;
    const id = pendingApproval.id;
    const edited = editingApproval ? editedSql : null;
    pendingApproval = null;
    editingApproval = false;
    await api.respondApproval(id, accept, edited);
  }
  async function resolvePath(grant: string) {
    if (!pendingPath) return;
    const id = pendingPath.id;
    pendingPath = null;
    await api.respondPathApproval(id, grant);
  }

  async function cancelTurn() {
    await api.cancelTurn();
  }

  function toggleExpand(i: number) {
    const next = new Set(expanded);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    expanded = next;
  }

  // --- derived view helpers ---------------------------------------------
  $: mode = snapshot?.mode ?? 'default';
  $: profile = snapshot?.profile ?? '(none)';
  $: engine = snapshot?.engine ?? '';
  $: usage = snapshot?.usage;
  $: turnRunning = snapshot?.turn_running ?? false;
  $: iteration = snapshot?.iteration ?? 0;
  $: maxIter = snapshot?.max_iterations ?? 0;
  $: canStart = snapshot?.can_start_turn ?? true;
  $: keyBearerConfigured = (llmStatus?.providers ?? []).some((p) => p.configured && p.id !== 'ollama');
</script>

{#if !connected}
  <!-- Launcher: no connection auto-resolved, pick a profile/env or a URL. -->
  <div class="launcher">
    <h1>naque</h1>
    <p class="muted">natural-language SQL over your database</p>
    {#if error}<div class="error">{error}</div>{/if}

    <div class="launcher-cols">
      <div class="pane">
        <h3>Profiles</h3>
        <ul class="list">
          {#each launcherProfiles as p (p)}
            <li class:active={p === launcherProfile} on:click={() => pickLauncherProfile(p)}>{p}</li>
          {:else}
            <li class="muted">no saved profiles — set a URL or run <code>naque /save</code> from the TUI</li>
          {/each}
        </ul>
      </div>
      <div class="pane">
        <h3>Environments</h3>
        <ul class="list">
          {#each launcherEnvs as e (e)}
            <li class:active={e === launcherEnv} on:click={() => (launcherEnv = e)}>{e}</li>
          {:else}
            <li class="muted">{launcherProfile ? 'no environments' : 'pick a profile'}</li>
          {/each}
        </ul>
      </div>
      <div class="pane">
        <h3>Or connect by URL</h3>
        <input class="url" placeholder="postgres://user@host/db" bind:value={urlInput} />
        <p class="muted small">DATABASE_URL, --url, and saved profiles are also auto-detected on launch.</p>
      </div>
    </div>

    <!-- AI provider: the GUI can't read shell env vars, so API keys are stored
         in the system keyring (service "naque") and resolved at connect time. -->
    <div class="llm-settings">
      <h3>AI provider</h3>
      {#if llmStatus}
        <div class="llm-providers">
          {#each llmStatus.providers as p (p.id)}
            <div class="llm-row" class:active={llmStatus.active === p.id}>
              <span class="llm-label">{p.label}</span>
              {#if p.configured && p.id !== 'ollama'}
                <span class="status ok">key set</span>
                <button class="ghost tiny" on:click={() => clearLlmKey(p.id)}>remove</button>
              {:else if p.id === 'ollama'}
                <span class="muted small">no key needed</span>
             {:else}
                <span class="muted small">no key</span>
              {/if}
              {#if llmStatus.active === p.id}<span class="status ok">active</span>{/if}
            </div>
          {/each}
        </div>
        {#if !keyBearerConfigured}
          <p class="muted small">Add an API key below to use a cloud provider (Ollama needs no key).</p>
        {/if}
      {/if}
      <div class="llm-input">
        <select bind:value={settingsProvider}>
          <option value="claude">Anthropic (Claude)</option>
          <option value="openai">OpenAI</option>
          <option value="gemini">Google Gemini</option>
          <option value="hf">Hugging Face</option>
        </select>
        <input
          type="password"
          class="url"
          placeholder="paste API key"
          bind:value={settingsKey}
          on:keydown={(e) => { if (e.key === 'Enter') saveLlmKey(); }}
        />
        <button class="primary" disabled={!settingsKey.trim()} on:click={saveLlmKey}>Save key</button>
      </div>
      {#if settingsSaved}<p class="muted small">saved to keyring</p>{/if}
    </div>

    <button class="primary" disabled={connecting || (!urlInput.trim() && !launcherProfile)} on:click={connectLaunch}>
      {connecting ? 'Connecting…' : 'Connect'}
    </button>
  </div>
{:else}
  <div class="app">
    <header class="statusbar">
      <span class="pill" title="Active profile">{profile}</span>
      <span class="pill" title="Database engine">{engine}</span>
      <span class="pill mode" title="Permission mode">{mode}</span>
      {#if turnRunning}<span class="pill iter">iter {iteration}/{maxIter}</span>{/if}
      {#if turnRunning}<span class="working" title="agent is working">
        <span class="dot"></span><span class="dot"></span><span class="dot"></span>
        working
      </span>{/if}
      {#if usage}<span class="pill muted" title="Token usage">{usage.input_tokens} in · {usage.output_tokens} out</span>{/if}
      <span class="spacer"></span>
      {#if turnRunning}<button class="ghost" on:click={cancelTurn}>Cancel</button>{/if}
      <button class="ghost" on:click={openSwitcher}>Switch…</button>
    </header>

    <main class="chat">
      {#if transcript.length === 0}
        <div class="empty muted">Ask your database in natural language. Type <code>/help</code> for commands, <code>!sql</code> for raw SQL, <code>\dt</code> for tables.</div>
      {/if}
      {#each transcript as e, i (i)}
        {@const kind = entryKind(e)}
        {#if kind === 'User'}
          <div class="entry user"><span class="who">you</span><div class="body">{escapeHtml(e.User)}</div></div>
        {:else if kind === 'Agent'}
          <div class="entry agent"><span class="who">naque</span><div class="body prose">{@html renderProse(e.Agent)}</div></div>
        {:else if kind === 'Reasoning'}
          <div class="entry reasoning"><div class="body prose dim">{@html renderProse(e.Reasoning)}</div></div>
        {:else if kind === 'Sql'}
          <div class="entry sql"><span class="tag">{e.Sql.label}</span><pre class="code">{escapeHtml(e.Sql.sql)}</pre></div>
        {:else if kind === 'Info'}
          <div class="entry info">{escapeHtml(e.Info)}</div>
        {:else if kind === 'Error'}
          <div class="entry error">{escapeHtml(e.Error)}</div>
        {:else if kind === 'Rejected'}
          <div class="entry rejected">rejected: <code>{escapeHtml(e.Rejected)}</code></div>
        {:else if kind === 'ToolStep'}
          {@const step = e.ToolStep}
          <div class="entry step" class:open={expanded.has(i)} on:click={() => toggleExpand(i)}>
            <span class="step-name">{step.name}</span>
            {#if step.status === 'Running'}<span class="status running">running</span>{/if}
            {#if step.status === 'Ok'}<span class="status ok">ok</span>{/if}
            {#if step.status === 'Err'}<span class="status err">err</span>{/if}
            {#if step.summary}<span class="step-summary">{step.summary}</span>{/if}
            {#if expanded.has(i) && step.detail}<pre class="code small">{escapeHtml(step.detail)}</pre>{/if}
          </div>
        {:else if kind === 'Result'}
          {@const r = e.Result}
          <div class="entry result">
            <table>
              <thead><tr>{#each r.columns as c}<th>{escapeHtml(c)}</th>{/each}</tr></thead>
              <tbody>
                {#each r.rows as row}
                  <tr>{#each row as cell, ci}<td>{@html renderCell(cell, r.byte_columns.includes(ci))}</td>{/each}</tr>
                {/each}
              </tbody>
            </table>
          </div>
        {/if}
      {/each}
    </main>

    <!-- Approval modal (SQL gate) -->
    {#if pendingApproval}
      <div class="modal-backdrop">
        <div class="modal">
          {#if pendingApproval.catastrophic}<div class="catastrophic">⚠ catastrophic — this statement is dangerous in every mode</div>{/if}
          <h3>{pendingApproval.label}</h3>
          <pre class="code">{escapeHtml(editingApproval ? editedSql : pendingApproval.sql)}</pre>
          <div class="modal-actions">
            <button class="ghost" on:click={() => { editingApproval = !editingApproval; editedSql = pendingApproval.sql; }}>{editingApproval ? 'cancel edit' : 'edit'}</button>
            <button class="danger" on:click={() => resolveApproval(false)}>Reject</button>
            <button class="primary" on:click={() => resolveApproval(true)}>{editingApproval ? 'Run edited' : 'Accept'}</button>
          </div>
          {#if editingApproval}<textarea class="edit" bind:value={editedSql} rows="4"></textarea>{/if}
        </div>
      </div>
    {/if}

    <!-- Path-grant modal (FS access) -->
    {#if pendingPath}
      <div class="modal-backdrop">
        <div class="modal">
          <h3>Filesystem access</h3>
          <p>Allow the agent to <strong>{pendingPath.action}</strong> <code>{escapeHtml(pendingPath.path)}</code>?</p>
          <div class="modal-actions">
            <button class="danger" on:click={() => resolvePath('deny')}>Deny</button>
            <button class="ghost" on:click={() => resolvePath('once')}>Once</button>
            <button class="primary" on:click={() => resolvePath('session')}>This session</button>
          </div>
        </div>
      </div>
    {/if}

    <!-- In-session switcher -->
    {#if switcherOpen}
      <div class="modal-backdrop" on:click={() => (switcherOpen = false)}>
        <div class="modal" on:click|stopPropagation>
          <h3>Switch connection</h3>
          <div class="launcher-cols">
            <div class="pane">
              <h3>Profiles</h3>
              <ul class="list">
                {#each switchProfiles as p (p)}
                  <li class:active={p === switchProfile} on:click={() => pickSwitchProfile(p)}>{p}</li>
                {/each}
              </ul>
            </div>
            <div class="pane">
              <h3>Environments</h3>
              <ul class="list">
                {#each switchEnvs as e (e)}
                  <li class:active={e === switchEnv} on:click={() => (switchEnv = e)}>{e}</li>
                {/each}
              </ul>
            </div>
          </div>
          <div class="modal-actions">
            <button class="ghost" on:click={() => (switcherOpen = false)}>Close</button>
            <button class="primary" disabled={!switchProfile || !switchEnv} on:click={switchTo}>Switch</button>
          </div>
        </div>
      </div>
    {/if}

    <!-- Input + slash autocomplete -->
    <footer class="input-bar">
      {#if suggestions.length}
        <div class="suggest">
          {#each suggestions as c, i (c.name)}
            <div class="suggest-row" class:sel={i === suggestIndex} on:click={() => complete(c.name)}>
              <span class="suggest-cmd">/{c.name}{c.args ? ' ' + c.args : ''}</span>
              <span class="muted">{c.help}</span>
            </div>
          {/each}
        </div>
      {/if}
      <textarea
        rows="2"
        bind:value={input}
        on:keydown={onInputKey}
        placeholder={canStart ? 'ask anything…  (/help, !sql, \\dt)' : 'turn running…'}
        disabled={!canStart && !turnRunning}
      ></textarea>
    </footer>
  </div>
{/if}

<style>
  :global(body, html) { margin: 0; height: 100%; background: #0e1116; color: #d7dde5; font-family: ui-sans-serif, system-ui, -apple-system, 'Segoe UI', sans-serif; }
  :global(#app) { height: 100vh; }
  .muted { color: #7a8694; }
  .dim { opacity: 0.7; }
  .small { font-size: 0.85em; }
  code { background: #1b2230; padding: 0 0.3em; border-radius: 3px; font-family: ui-monospace, 'SF Mono', Menlo, monospace; }

  .launcher { max-width: 720px; margin: 0 auto; padding: 48px 24px; }
  .launcher h1 { margin: 0 0 4px; font-size: 2rem; letter-spacing: -0.02em; }
  .launcher-cols { display: grid; grid-template-columns: 1fr 1fr 1fr; gap: 16px; margin: 24px 0; }
  .pane h3 { margin: 0 0 8px; font-size: 0.8rem; text-transform: uppercase; color: #7a8694; }
  .list { list-style: none; padding: 0; margin: 0; max-height: 200px; overflow: auto; }
  .list li { padding: 6px 8px; border-radius: 6px; cursor: pointer; }
  .list li.active { background: #1b2230; color: #8ad0ff; }
  .list li:hover { background: #161b26; }
  .url { width: 100%; box-sizing: border-box; background: #161b26; border: 1px solid #2a3340; color: #d7dde5; padding: 8px; border-radius: 6px; font-family: ui-monospace, monospace; }

  .llm-settings { margin: 16px 0; padding: 14px; background: #11161d; border: 1px solid #1b2230; border-radius: 8px; }
  .llm-settings h3 { margin: 0 0 10px; font-size: 0.8rem; text-transform: uppercase; color: #7a8694; }
  .llm-providers { display: flex; flex-direction: column; gap: 6px; margin-bottom: 12px; }
  .llm-row { display: flex; align-items: center; gap: 8px; font-size: 0.9rem; }
  .llm-row.active .llm-label { color: #8ad0ff; }
  .llm-label { flex: 1; }
  .llm-input { display: grid; grid-template-columns: auto 1fr auto; gap: 8px; align-items: center; }
  .llm-input select { background: #161b26; border: 1px solid #2a3340; color: #d7dde5; padding: 8px; border-radius: 6px; }
  button.tiny { padding: 3px 8px; font-size: 0.8rem; }

  button { background: #1b2230; color: #d7dde5; border: 1px solid #2a3340; padding: 8px 14px; border-radius: 6px; cursor: pointer; }
  button:disabled { opacity: 0.4; cursor: not-allowed; }
  button.primary { background: #2d6cf6; border-color: #2d6cf6; color: #fff; }
  button.danger { background: #a93434; border-color: #a93434; color: #fff; }
  button.ghost { background: transparent; }

  .app { display: flex; flex-direction: column; height: 100vh; }
  .statusbar { display: flex; align-items: center; gap: 8px; padding: 6px 12px; background: #11161d; border-bottom: 1px solid #1b2230; font-size: 0.85rem; }
  .working { display: inline-flex; align-items: center; gap: 4px; color: #f6c177; font-size: 0.85rem; }
  .working .dot { width: 5px; height: 5px; border-radius: 50%; background: #f6c177; animation: pulse 1s infinite ease-in-out; }
  .working .dot:nth-child(2) { animation-delay: 0.2s; }
  .working .dot:nth-child(3) { animation-delay: 0.4s; }
  @keyframes pulse { 0%, 80%, 100% { opacity: 0.25; transform: scale(0.8); } 40% { opacity: 1; transform: scale(1); } }
  .pill { background: #1b2230; padding: 2px 8px; border-radius: 999px; }
  .pill.mode { color: #8ad0ff; }
  .pill.iter { color: #f6c177; }
  .spacer { flex: 1; }

  .chat { flex: 1; overflow-y: auto; padding: 16px 20px; display: flex; flex-direction: column; gap: 12px; }
  .empty { text-align: center; margin-top: 40px; }
  .entry { line-height: 1.5; }
  .entry .who { display: inline-block; width: 3.5em; color: #7a8694; font-size: 0.8rem; }
  .entry.user .body, .entry.agent .body { display: inline; }
  .entry.agent { color: #e6e9ee; }
  .entry.user .body { color: #d7dde5; }
  .prose :global(.bytes) { background: #1b2230; padding: 0 0.3em; border-radius: 3px; color: #8ad0ff; }
  .entry.reasoning .body { border-left: 2px solid #2a3340; padding-left: 10px; }
  .entry.info { color: #7a8694; }
  .entry.error { color: #f08080; }
  .entry.rejected { color: #f08080; }
  .entry.sql .tag { color: #8ad0ff; font-size: 0.75rem; margin-right: 6px; }
  .code { background: #11161d; border: 1px solid #1b2230; padding: 8px 10px; border-radius: 6px; overflow-x: auto; margin: 4px 0; font-family: ui-monospace, monospace; font-size: 0.85rem; white-space: pre-wrap; }
  .code.small { font-size: 0.8rem; margin-top: 6px; }

  .entry.step { cursor: pointer; padding: 4px 0; }
  .step-name { color: #8ad0ff; font-family: ui-monospace, monospace; font-size: 0.85rem; }
  .step-summary { color: #7a8694; margin-left: 8px; font-size: 0.85rem; }
  .status { font-size: 0.7rem; padding: 0 5px; border-radius: 999px; margin-left: 6px; }
  .status.running { background: #1b2230; color: #f6c177; }
  .status.ok { background: #15301f; color: #7fd98a; }
  .status.err { background: #3a1717; color: #f08080; }

  .entry.result table { border-collapse: collapse; font-size: 0.85rem; }
  .entry.result th, .entry.result td { border: 1px solid #1b2230; padding: 4px 8px; text-align: left; }
  .entry.result th { background: #11161d; color: #7a8694; font-weight: 600; }
  .entry.result td :global(.null) { color: #566070; font-style: italic; }

  .modal-backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.6); display: flex; align-items: center; justify-content: center; z-index: 10; }
  .modal { background: #11161d; border: 1px solid #2a3340; border-radius: 10px; padding: 20px; max-width: 640px; width: 90vw; max-height: 80vh; overflow: auto; }
  .modal h3 { margin: 0 0 8px; }
  .modal-actions { display: flex; gap: 8px; justify-content: flex-end; margin-top: 12px; }
  .catastrophic { background: #3a1717; color: #f08080; padding: 8px; border-radius: 6px; margin-bottom: 12px; font-size: 0.9rem; }
  .edit { width: 100%; box-sizing: border-box; background: #0e1116; border: 1px solid #2a3340; color: #d7dde5; margin-top: 8px; font-family: ui-monospace, monospace; }

  .input-bar { border-top: 1px solid #1b2230; padding: 8px 12px; position: relative; }
  .input-bar textarea { width: 100%; box-sizing: border-box; background: #11161d; border: 1px solid #2a3340; color: #d7dde5; border-radius: 8px; padding: 10px; resize: none; font-family: inherit; font-size: 0.95rem; }
  .suggest { position: absolute; bottom: 100%; left: 12px; right: 12px; background: #11161d; border: 1px solid #2a3340; border-radius: 8px; max-height: 220px; overflow: auto; }
  .suggest-row { padding: 6px 10px; cursor: pointer; display: flex; gap: 12px; }
  .suggest-row.sel, .suggest-row:hover { background: #1b2230; }
  .suggest-cmd { font-family: ui-monospace, monospace; color: #8ad0ff; }
</style>