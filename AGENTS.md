# Repository Guidelines

## Project Structure & Module Organization
- Source: `src/`
  - `src/main.rs` — process entry; IO and runtime setup.
  - `src/agent.rs` — ACP agent implementation (sessions, streaming, approvals).
  - `src/agent/commands.rs` — slash command handling and helpers.
- Config/build: `Cargo.toml`, `rust-toolchain.toml`
- Docs: `README.md`
- Tests: inline unit tests; integration tests in `tests/`.

## Build, Test, and Development Commands
- `cargo build` — compile the project with dependencies.
- `cargo check` — fast type/lint pass.
- `RUST_LOG=info cargo run --quiet` — run the agent over stdio.
- `cargo fmt --all` — format with rustfmt.
- `cargo clippy -- -D warnings` — lint and deny warnings.
- `cargo test` — run unit and integration tests.

## Coding Style & Naming Conventions
- Rust 2024 edition; 4-space indentation; `rustfmt` enforced.
- Naming: `snake_case` (functions/vars), `CamelCase` (types/traits), `SCREAMING_SNAKE_CASE` (consts).
- Imports: explicit paths; group std/crate/local; avoid unused imports.
- Errors: prefer `anyhow::Result<T>` at boundaries; map external errors early.
- Logging: use `tracing`; gate verbosity with `RUST_LOG`.

## Testing Guidelines
- Unit tests inline via `#[cfg(test)] mod tests { ... }`; keep deterministic (avoid timing races).
- Integration tests in `tests/` (e.g., `tests/agent_status_test.rs`).
- Name tests by behavior in `snake_case` (e.g., `advertises_available_commands`).
- Run with `cargo test`; ensure `cargo clippy` passes before pushing.

## Commit & Pull Request Guidelines
- Commits: small, focused; use Conventional Commits (`feat:`, `fix:`, `refactor:`).
- Pull Requests include: problem statement and approach; linked issues; test plan (commands run, expected output) with relevant `RUST_LOG` snippets; screenshots/logs for ACP interactions when helpful.

## Security & Configuration Tips
- Auth: use `codex login` or `OPENAI_API_KEY` for API key mode.
- Secrets: never commit keys or `auth.json`; use env/OS keychain.
- Network: first build fetches git dependencies; subsequent builds cached.
- Safety: default to on-request approvals; avoid destructive examples.

## Agent-Specific Instructions
- Add new slash commands in `src/agent/commands.rs`; document usage in help output.
- Prefer clear, actionable logs and deterministic async flows (use `LocalSet` where applicable`).
