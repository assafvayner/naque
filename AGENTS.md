# naque

`naque` is a terminal (TUI) tool for querying databases through an AI agent: a user types natural language, an iterative agent translates it to SQL, inspects the schema, self-corrects, and runs it against a live Postgres/SQLite session. A four-level permission model with defense-in-depth read-only enforcement keeps execution safe by default.

## Code Standards

These rules apply to ALL code written or modified in this repo:

### Style
- NO trivial comments — do not add comments that restate what the code does
- Descriptive variable and function names
- No wildcard imports (e.g., `use foo::*`)
- Latest stable Rust features are allowed

### Error Handling
- Use `Result<T, E>` with explicit error handling — never panic
- Define custom error types using `thiserror` for domain-specific errors
- Provide helpful, actionable error messages

### Performance
- Be mindful of allocations in hot paths
- Prefer structured logging (tracing/log macros with fields, not string formatting)

### Dependencies
- Add all dependencies to `Cargo.toml`
- Prefer well-maintained crates from crates.io

### Testing
- Unit tests: place in the same file using `#[cfg(test)]` modules
- Integration tests: place in the `tests/` directory

### Formatting and Linting
- Format: `cargo +nightly fmt`
- Lint: `cargo clippy -r --verbose -- -D warnings`
- ALWAYS run both after making changes — do not skip this step

### Minimal Changes
- Verify that every change is minimal and necessary — do not include unrelated modifications

## Repo Notes

- **Design doc:** [docs/superpowers/specs/2026-06-24-naque-design.md](docs/superpowers/specs/2026-06-24-naque-design.md) — read the section relevant to your change before editing.
- **Workspace layout** (`crates/*`): `naque-core` (domain types, permission gate) → `naque-sql` (parse/classify via sqlparser) → `naque-db` (Postgres/SQLite, sessions, read-only execution) → `naque-schema` (introspection, cache, drift) → `naque-llm` (`LlmProvider` trait + OpenAI/HF/Gemini impls, agent loop, tools) → `naque-profile` (`~/.naque/` + `naque.toml`, env/keyring creds) → `naque-tui` (ratatui widgets) → `naque` (binary: wiring, CLI, event loop).
- **Security boundary is the core invariant.** Read-only enforcement is deterministic (sqlparser classification + DB-level read-only); keep the LLM out of the security path. Every statement — agent-generated or raw `!` SQL — must pass through the single permission gate. The catastrophic guard (`DROP`, `TRUNCATE`, unqualified `DELETE`/`UPDATE`) fires in every mode, including wildcard. Anything not confidently read-only is treated as a write and gated.
- **Dependencies are workspace-level.** Declare external crates in the root `Cargo.toml` `[workspace.dependencies]`; member crates inherit them with `{ workspace = true }`.
- **Secrets never touch disk.** `naque.toml` is committed and shared — passwords are referenced via `password_env` / `password_keyring` only, never written in plaintext.
- **LLM tests use the mock provider** (`naque-llm/src/mock.rs`) — no network in unit tests.
