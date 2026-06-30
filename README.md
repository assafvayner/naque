# naque

**Agentic AI query tool over databases.**

`naque` is a terminal (TUI) tool for querying databases in plain language. You type a
request, an iterative AI agent translates it to SQL, inspects the schema, runs queries,
reads the results and errors, and self-corrects — all against a live Postgres or SQLite
session. A four-level permission model with defense-in-depth read-only enforcement keeps
execution safe by default: the LLM is never in the security path.

```
> how many users signed up in the last 7 days, by day?

  agent → inspect_table(users) → run_query(SELECT ...)

  signup_date │ count
  ────────────┼──────
  2026-06-19  │   142
  2026-06-20  │   178
  ...
```

## Highlights

- **Natural language → SQL**, via an agent that inspects the schema, samples tables,
  runs `EXPLAIN`, executes queries, and self-corrects within a turn (bounded by a
  per-turn iteration cap).
- **Hard safety boundary.** Read-only enforcement is deterministic (SQL parsing +
  database-level read-only), not delegated to the model. An always-on catastrophic guard
  blocks `DROP`/`TRUNCATE`/unqualified `DELETE`/`UPDATE` in *every* mode.
- **Four permission modes** — `strict`, `default`, `readonly`, `wildcard` — trading
  approval friction for autonomy.
- **psql-like session.** Persistent connection with `SET`s, `search_path`, and
  transactions; `\d`/`\dt`/`\reset` and friends for muscle memory.
- **Raw SQL passthrough** with `!` — still gated through the same permission chokepoint.
- **Schema learning** per project, from the DB catalog and/or user-provided docs, with a
  drift fingerprint that warns when the live schema diverges from what was learned.
- **Model-agnostic LLM backend** — Claude, OpenAI, Gemini, Hugging Face Inference
  Providers, or local Ollama, behind one provider trait.
- **Profiles & shareable config** — a committable `naque.toml` plus a per-machine
  `~/.naque/` store. Secrets are referenced (env var / OS keyring), never written to disk.

## Installation

Install the `naque` binary straight from the repo with a recent stable toolchain
(MSRV 1.95):

```bash
cargo install --git https://github.com/assafvayner/naque naque
```

This builds the whole workspace and drops the `naque` binary into `~/.cargo/bin`
(ensure it's on your `PATH`). No system OpenSSL is required (TLS is via rustls); SQLite
is bundled.

### From a clone

```bash
git clone https://github.com/assafvayner/naque
cd naque
cargo build --release
# binary at target/release/naque
```

Run directly during development:

```bash
cargo run -p naque -- --url postgres://user@localhost/mydb
```

## Usage

```bash
naque [PROFILE] [OPTIONS]
```

| Flag | Description |
|---|---|
| `PROFILE` | Profile name to launch (overrides `naque.toml` `project` / central default) |
| `--url <DSN>` | Explicit connection string (overrides profile resolution) |
| `--mode <MODE>` | Permission mode: `strict` \| `default` \| `readonly` \| `wildcard` |
| `--no-guard` | Disable the always-on catastrophic guard (`--yolo`) |
| `--provider <P>` | AI provider override: `claude` \| `openai` \| `gemini` \| `hf` \| `ollama` |
| `--model <M>` | Model name override (e.g. `claude-opus-4-8`, `zai-org/GLM-5.2`) |
| `--no-color` | Disable colored output |

### Connection precedence (first match wins)

1. `--url`
2. Active profile (positional arg → `naque.toml` `project` → central default)
3. `DATABASE_URL` environment variable

```bash
naque --url postgres://user@host/mydb     # connect directly by URL
naque myproj                              # launch the 'myproj' profile
DATABASE_URL=postgres://... naque         # use the DATABASE_URL env var
naque myproj --mode readonly              # profile in read-only mode
```

Launching with no connection configured prints first-run guidance instead of an error.

## Input grammar

Inside the TUI, the first character of your input routes the request:

| Prefix | Class | Goes to |
|---|---|---|
| *(bare text)* | Natural language | Agent loop |
| `!` | Raw SQL passthrough | Permission gate → primary connection |
| `\` | psql-style session command | Live DB session |
| `/` | naque tool command | Tool control |

**Session commands (`\`)** — `\reset`, `\d <table>`, `\dt`, `\dn`, `\set`, `\timing`,
`\x`, `\q`.

**Tool commands (`/`)** — `/help`, `/clear`, `/mode <mode>`, `/learn [--docs <path>]
[--refresh]`, `/schema`, `/profile <list|use|new|edit|rm>`, `/config [key [value]]`,
`/allow-dir <path-or-glob>`, `/cost`, `/export <csv|json> [path]`, `/quit`.

## Safety model

Every statement — agent-generated or raw `!` SQL — flows through a single permission gate:

```
statement → SQL classify ─► catastrophic? ─► ALWAYS-ON guard (hard confirm, any mode)
                │                                   │
                ▼                                   ▼
          mode decision                       execute via naque-db
```

### Permission modes

| Mode | Introspection | Read primaries | Writes |
|---|---|---|---|
| `strict` | confirm | confirm | confirm |
| `default` | free | confirm | confirm |
| `readonly` | free | auto (under DB read-only) | confirm |
| `wildcard` | free | auto | auto |

The **catastrophic guard** (`DROP`, `TRUNCATE`, `DELETE`/`UPDATE` without a `WHERE`) fires
a hard confirm in *every* mode, including `wildcard`, unless explicitly disabled with
`--no-guard`.

### Defense in depth for read-only

1. **Deterministic classification** — statements are parsed with `sqlparser`; anything not
   confidently read-only is treated as a write and gated (fail safe).
2. **Database-level read-only** — reads execute under the DB's own read-only mode
   (Postgres `SET TRANSACTION READ ONLY`, SQLite `PRAGMA query_only=ON`), so a
   misclassification still cannot slip a mutation through.

The LLM is never on the security path.

### Filesystem & web access

Beyond SQL, the agent can read local files and fetch web pages to gather context — point
it at the SQL, schema, or ORM models that describe your database, or a docs URL.

**Filesystem reads** (`read_file`, `list_directory`) are gated by a permission dimension
**separate** from the SQL modes above: a path is readable only if it matches one of your
allowed globs. Set them with `read_paths` in config (see below), or grant one for the
current session with `/allow-dir <path-or-glob>`. When the agent reaches for a path outside
the allowed set, the TUI prompts you — **allow once**, **allow this session**, or **deny**.
Symlinks and `..` are resolved before the check, so reads cannot escape the allowed roots.

**Web fetch** (`web_fetch`) is **enabled by default** and needs no API key — it issues a
direct HTTP GET, converts HTML to Markdown, and returns text (binary responses are
refused). Requests to loopback/private/link-local hosts are blocked. Turn it off with
`web_access = false` in config. There is no web *search* tool; pass the agent a URL (or let
it use one already in the conversation).

## Configuration

`naque` layers a per-machine central store with a per-project, committable file.

### `~/.naque/` (central, per-machine)

```
~/.naque/
  config.toml      # global settings (default mode, provider/model, row cap, max-iterations)
  profiles.toml    # global named profiles (connection refs + per-profile settings)
  cache/<key>/     # learned schema, ingested docs, drift fingerprint (per profile)
  logs/            # optional
```

### `naque.toml` (per-project, shareable)

On startup naque walks up from the current directory (git-style) for the nearest
`naque.toml` and layers it on top of the central config. It is meant to be committed, so it
contains **no secrets** — passwords are referenced by env var or keyring only.

```toml
project = "myapp-dev"            # active profile for this directory

[config]
mode = "readonly"
row_cap = 500
# Globs the agent may read (read_file / list_directory). Relative globs resolve
# against this project directory; '~' expands to the home directory. Unioned
# across config layers — global + per-profile grants accumulate.
read_paths = ["sql/**", "migrations/**/*.sql", "~/db-notes/**"]
web_access = true                    # web_fetch on by default; set false to disable

[profiles.myapp-dev]
engine = "postgres"
host = "localhost"
port = 5432
dbname = "myapp"
user = "app"
password_env = "MYAPP_DB_PASSWORD"   # reference only — never the secret itself
docs = ["docs/schema.md"]

[profiles.myapp-prod]
engine = "postgres"
host = "db.internal"
dbname = "myapp"
user = "readonly"
password_keyring = "myapp-prod"      # pulled from the OS keyring
```

**Config precedence** (low → high): built-in defaults → `~/.naque/config.toml` →
`./naque.toml` `[config]` → environment variables → CLI flags. Scalar keys are
overridden by the highest layer that sets them; `read_paths` is the exception — it is
**unioned** across all layers (and any `/allow-dir` session grants), so a path is readable
if any layer allows it.

**Credentials** are never stored in plaintext: use `password_env` / `password_keyring`,
the standard `DATABASE_URL` / `PG*` env vars, or an explicit `--url`.

## LLM providers

A provider is selected via `--provider`/`--model`, per-profile config, or auto-detected
from environment variables. Auto-detection priority (first key present wins):

| Provider | `--provider` | Credential env var |
|---|---|---|
| Anthropic Claude | `claude` | `ANTHROPIC_API_KEY` |
| OpenAI | `openai` | `OPENAI_API_KEY` |
| Google Gemini | `gemini` | `GEMINI_API_KEY` (or `GOOGLE_API_KEY`) |
| Hugging Face Inference Providers | `hf` | `HF_TOKEN` |
| Ollama (local) | `ollama` | *(none — local endpoint)* |

## Architecture

A Rust workspace under `crates/*`, each crate independently testable:

| Crate | Responsibility |
|---|---|
| `naque-core` | Dependency-free domain types: permission modes, statement classification, the gate decision |
| `naque-sql` | Parse + classify statements (read / write / DDL / catastrophic) via `sqlparser` |
| `naque-db` | Postgres/SQLite abstraction, connection + session management, read-only enforcement, execution, result types |
| `naque-schema` | Schema learning (DB introspection + doc ingestion), local cache, compact-catalog rendering, drift fingerprint |
| `naque-llm` | `LlmProvider` trait + impls, the agent loop, tool definitions/dispatch |
| `naque-profile` | Config + profile persistence, `naque.toml` discovery/merge, credential resolution (env/keyring) |
| `naque-tui` | ratatui widgets: input router, option picker, approval flow, result table + export, status bar |
| `naque` | Binary: wiring, CLI args, event loop |

Dependency direction: `naque-core` → `naque-sql` → `naque-db` → `naque-schema` →
`naque-llm` → `naque-profile` → `naque-tui` → `naque`.

## Development

```bash
cargo build --workspace
cargo test --workspace                                   # unit + integration tests
cargo +nightly fmt --all                                 # format (nightly required)
cargo clippy --workspace --all-targets -- -D warnings    # lint
```

The repo's full code standards live in [`AGENTS.md`](AGENTS.md).

### Tests

- **Unit tests** live alongside the code in `#[cfg(test)]` modules.
- **LLM tests** use the mock provider (`naque-llm/src/mock.rs`) — no network access.
- **SQLite integration tests** run against a temp-file database and need no setup.
- **Postgres integration tests** read `NAQUE_TEST_PG_URL` and skip cleanly when it's
  unset, so the suite stays green without a Postgres container:

  ```bash
  export NAQUE_TEST_PG_URL=postgres://naque:naque@localhost:55432/naque
  cargo test -p naque-db --test postgres_integration
  ```

CI runs format check, clippy (`-D warnings`), and the test suite on every PR and push to
`main` — see [`.github/workflows/ci.yml`](.github/workflows/ci.yml).
