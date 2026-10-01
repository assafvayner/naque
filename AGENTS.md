# naque

`naque` is a terminal (TUI) tool for querying databases through an AI agent: a user types natural language, an iterative agent translates it to SQL, inspects the schema, self-corrects, and runs it against a live Postgres/SQLite session. A four-level permission model with defense-in-depth read-only enforcement keeps execution safe by default.

## Commands

- Format: `cargo +nightly fmt --all` — **nightly is required** (`rustfmt.toml` enables `unstable_features`).
- Lint: `cargo clippy --workspace --all-targets -- -D warnings` (CI also sets `RUSTFLAGS=-D warnings`).
- Test: `cargo test --workspace --all-targets` plus `cargo test --workspace --doc`.
- Single integration test: `NAQUE_TEST_PG_URL=postgres://... cargo test -p naque-db --test postgres_integration`.
- Run the binary: `cargo run -p naque -- --url postgres://user@localhost/mydb`.
- MSRV 1.95, edition 2024.

Always run `fmt` and `clippy` after changes — CI gates every PR/push on both.

## Testing quirks

- LLM tests use the mock provider (`naque-llm/src/mock.rs`) — no network in unit tests.
- Postgres integration tests (`naque-db`, `naque-schema`) read `NAQUE_TEST_PG_URL` and **skip cleanly when unset**, so the suite stays green without a Postgres container. Set the var to exercise them.
- SQLite integration tests run against a temp-file DB; no setup needed.

## Workspace layout

Rust workspace under `crates/*`, each crate independently testable. Dependency direction is one-way:

`naque-core` (domain types, permission gate) → `naque-sql` (parse/classify via `sqlparser`) → `naque-db` (Postgres/SQLite, sessions, read-only execution) → `naque-schema` (introspection, cache, drift) → `naque-llm` (`LlmProvider` trait + OpenAI/HF/Gemini/Claude/Ollama impls, agent loop, tools) → `naque-profile` (`~/.naque/` + `naque.toml`, env/keyring creds) → `naque-tui` (ratatui widgets) → `naque` (binary: wiring, CLI, event loop).

Binary entrypoint: `crates/naque/src/main.rs` → `cli::Args` (clap) → `setup::build_app` → `naque::ui::run`.

## Conventions

- No trivial comments — don't restate what the code does.
- No wildcard imports (`use foo::*`).
- Imports are grouped `StdExternalCrate` at module granularity (per `rustfmt.toml`) — match this when adding imports.
- Errors: `Result<T, E>` with explicit handling, never `panic`. Domain errors use `thiserror`; the binary uses `anyhow`.
- No `tracing`/`log` direct dependency — diagnostics go to stderr (`eprintln!`). Don't introduce `tracing` expecting structured logging.
- Every change must be minimal and necessary — no unrelated modifications.

## Constraints

- **Security boundary is the core invariant.** Read-only enforcement is deterministic (sqlparser classification + DB-level read-only); the LLM is never in the security path. Every statement — agent-generated or raw `!` SQL — passes through the single permission gate. The catastrophic guard (`DROP`, `TRUNCATE`, unqualified `DELETE`/`UPDATE`) fires in every mode, including `wildcard`. Anything not confidently read-only is treated as a write and gated.
- **Dependencies are workspace-level.** Declare external crates in the root `Cargo.toml` `[workspace.dependencies]`; member crates inherit them with `{ workspace = true }`.
- **Secrets never touch disk.** `naque.toml` is committed and shared — passwords are referenced via `password_env` / `password_keyring` only, never written in plaintext.