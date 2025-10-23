# Repository Guidelines

This document describes how to work in this repo using idiomatic Rust patterns and the current module layout.

## Project Structure

- src/
  - lib.rs — library crate root, exports `agent` and `fs` (forbid unsafe).
  - main.rs — binary entrypoint; stdio wiring and runtime setup.
  - agent/
    - mod.rs — ACP agent (sessions, streaming, approvals).
    - commands.rs — slash command handlers and helpers.
    - events.rs — Codex Event → ACP updates; `EventHandler`, `ReasoningAggregator`.
    - modes.rs — session modes and approval presets (`APPROVAL_PRESETS`).
    - session.rs — `SessionState`, `SessionModeLookup`.
    - client_ops.rs — client capability checks and FS read/write wrappers.
    - utils.rs — formatting, FS tool metadata, command title helpers.
    - tests/ — unit tests (e.g., `modes_test.rs`, `reasoning_test.rs`).
  - fs/
    - mod.rs, bridge.rs, mcp_server.rs — filesystem bridge + `acp_fs` MCP server.
- Cargo.toml, rust-toolchain.toml
- README.md, AGENTS.md
- Makefile, scripts/stdio-smoke.sh

## Build, Test, Run

- cargo check — fast type pass.
- cargo build — compile.
- cargo fmt --all — format with rustfmt.
- cargo clippy -- -D warnings — lint and deny warnings.
- cargo test — run unit tests.
- RUST_LOG=info cargo run --quiet — run the agent over stdio.
- make smoke — run a simple stdio JSON-RPC smoke test.

## Coding Style & Conventions

- Rust 2024 edition; 4-space indentation; rustfmt enforced.
- Unsafe: forbidden at crate root (`#![forbid(unsafe_code)]`).
- Naming: snake_case (fns/vars), CamelCase (types/traits), SCREAMING_SNAKE_CASE (consts).
- Imports: group `std`, external crates, then local modules; avoid unused imports.
- Visibility: default to private; prefer `pub(crate)` over `pub` unless part of the public API.
- Errors: convert external errors early; map to ACP `Error` at boundaries. Use `anyhow` for internal convenience where appropriate.
- Logging: `tracing`; control via `RUST_LOG`.

## Testing Guidelines

- Keep tests deterministic (avoid timing races); prefer current-thread executors (`LocalSet`) for async tests when needed.
- Name tests by behavior in snake_case (e.g., `is_read_only_detection`).

## Pull Requests & Commits

- Conventional Commits: `feat:`, `fix:`, `refactor:`, `docs:`, `test:`, etc.
- PRs include: problem statement, approach, linked issues, and a test plan (commands run, expected output). Include brief `RUST_LOG` snippets when relevant.

## Security & Configuration

- Auth: use `codex login` or `OPENAI_API_KEY`.
- Do not commit secrets (API keys, auth.json); rely on env/OS keychain.
- First build fetches git dependencies; subsequent builds are cached.

## Agent-Specific Notes

- Add/extend slash commands in `agent/commands.rs` (advertised via `AVAILABLE_COMMANDS`).
- Use `events::EventHandler` to construct ACP updates; aggregate reasoning with `ReasoningAggregator`.
- Use `modes::{session_modes_for_config, find_preset_by_mode_id}` to manage session modes.
- Prefer `client_ops` for capability checks and FS read/write requests.
