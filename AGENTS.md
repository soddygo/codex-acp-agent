# Repository Guidelines

> This document guides contributors working on the Codex ACP Rust agent. Keep changes focused and incremental.

## Project Structure & Module Organization

- Source: `src/`
  - `src/main.rs` — process entry; wires runtime, agent, and filesystem bridge startup.
  - `src/agent.rs` — core ACP agent implementation (sessions, streaming, approvals).
  - `src/agent/commands.rs` — slash command handling and helpers.
  - `src/fs/mod.rs` — filesystem bridge module exports.
  - `src/fs/bridge.rs` — TCP bridge that proxies file reads/writes via the client.
  - `src/fs/mcp_server.rs` — standalone MCP server exposing `read_text_file`/`write_text_file` tools.
- Config/build: `Cargo.toml`, `rust-toolchain.toml`
- Scripts: `scripts/stdio-smoke.sh` — quick stdio smoke test harness.
- Docs: `README.md`, `AGENTS.md`

## Build, Test, and Development Commands

- `cargo build` — compile with dependencies.
- `RUST_LOG=info cargo run --quiet` — run the agent over stdio.
- `ACP_DEV_ALLOW_MOCK=1 RUST_LOG=info cargo run --quiet` — run without Codex backend (slash-commands only).
- `cargo check` — fast type/lint pass.
- `cargo fmt --all` — format code.
- `cargo clippy -- -D warnings` — lint and deny warnings.
- `cargo test` — run unit/integration tests (add as described below).

## Coding Style & Naming Conventions

- Language: Rust 2024 edition; 4-space indentation; `rustfmt` enforced.
- Naming: `snake_case` for functions/vars, `CamelCase` for types/traits, `SCREAMING_SNAKE_CASE` for consts.
- Imports: use explicit paths; group std/crate/local; keep unused imports out.
- Errors: prefer `anyhow::Result<T>` at boundaries; map external errors early.
- Logging: use `tracing`; gate verbosity with `RUST_LOG`.

## Testing Guidelines

- Unit tests: inline with modules using `#[cfg(test)] mod tests { ... }`.
- Integration tests: create `tests/` with files like `agent_status_test.rs`.
- Test names: describe behavior in `snake_case` (e.g., `advertises_available_commands`).
- Determinism: avoid timing races; prefer local `LocalSet` and channel fakes where possible.

## Commit & Pull Request Guidelines

- Commits: concise scope; prefer Conventional Commits (e.g., `feat:`, `fix:`, `refactor:`).
- PRs must include:
  - Problem statement and approach; link issues if any.
  - Test plan (commands run, expected output). Include relevant `RUST_LOG` snippets.
  - Screenshots/logs for ACP interactions when helpful.

## Security & Configuration Tips

- Auth: use `codex login` (ChatGPT) or `OPENAI_API_KEY` for API key mode.
- Secrets: never commit keys or `auth.json`; rely on env/OS keychain.
- Network: first build fetches git dependencies; subsequent builds are cached.
- Safety: default to on-request approvals; avoid destructive commands in examples.

## Documentation for ACP

### MCP Servers

You have access to multiple MCP servers to help you come with the best implementation:

- `acp`: Agent Client Protocol (ACP) repository. Main source of truth for documentation about the Agent Client Protocol. Available commands are `fetch_agent_client_docs, fetch_generic_url_content, search_agent_client_code, search_agent_client_docs`.
- `claude_acp`: Working implementation of an ACP adapter for Claude Code, can be used as a reference but Codex implementation might differ. Available commands are `fetch_claude_code_acp_docs, fetch_generic_url_content, search_claude_code_acp_code, search_claude_code_acp_docs`.
- `codex_rust`: Rust repository of the Codex implementation. Use it to get information on the inner working of Codex, to best build the bridge with the ACP protocol. Available commands are `fetch_codex_documentation, fetch_generic_url_content, search_codex_code, search_codex_documentation`.

### Local files

- [acp.md](/Users/arthurgamblin/Developer/ai/codex-acp/acp.md): Global documentation for the Agent Client Protocol. Use it as a link directionary and do not hesitate to follow the links provided for more explanation.
- [filesystem_acp.md](/Users/arthurgamblin/Developer/ai/codex-acp/filesystem_acp.md): Short explanation of the ACP filesystem.

## Instructions for Each Interaction

### Project Knowledge Graph Memory

You have access to the Project Memory Knowledge Graph server. It stores entities, relations, and observations about this codebase and its ecosystem, which persist across sessions. Use it to help with consistency, recall, and avoiding repeated work. This MCP tool is called "memory".

### 1. Memory Retrieval

- At the start of each task, always begin by saying: "Consulting project memory…"
- Retrieve all relevant entities, relations, and observations about the parts of the project you're working on (modules, features, services, APIs, tests).
- Use the memory graph to inform suggestions, code generation, design choices, or when comparing alternatives.

### 2. Memory Creation / Update

- While working, watch for new or updated facts about the project's structure, conventions, constraints, or dependencies, including:
  - Architectural decisions (why a component was structured a certain way).
  - Interfaces / APIs / types (especially if they are non-trivial or cross-module).
  - Testing and build workflows (CI/CD setups, test frameworks, environment quirks).
  - Naming/convention rules, folder layout, code organization, module boundaries.
  - Known pitfalls, performance bottlenecks, or workarounds (e.g. gotchas in tooling or libraries).
  - Dependency versions and why certain versions are chosen or pinned.
  - Security, compliance, or deployment constraints.
- Also capture relations, e.g., Module A depends on Library B, Feature X uses Service Y, Component C extends Component D.

### 3. Formatting and Entities

- Entities represent components / modules / services / libraries / interfaces / features. Give them meaningful names (e.g. `AuthService`, `DatabaseLayer`, `GraphQL_API`, `CI_CD_Pipeline`).
- Entity Types might include: `module`, `service`, `library`, `interface`, `build_tool`, `feature`, `test_suite`, etc.
- Observations are atomic facts about an entity (e.g. "Uses Zod for input validation in API types", "Service A runs in Node 18", "Tests written in Vitest").
- Relations should describe how entities connect, in active voice. Examples: `depends_on`, `implements`, `exposes`, `calls`, `imports_from`, `extends`.

### 4. When to Skip Storing

- Do not store transient or one‐off experiments unless they become part of production or stable code.
- Do not store secrets, credentials, or large data blobs.
- Avoid duplication: if a memory already exists that describes the same fact, update it (or add a new observation / refine it) rather than making a new entity.

### 5. Usage in Responses

- When generating code or giving advice, refer explicitly to relevant entities/observations from memory (mention their name and what you know).
- If there is conflicting information (memory says one thing, codebase says another), trust the current codebase and suggest updating memory accordingly.
- Suggest checking memory entities that might impact decisions (e.g. "According to project memory, we use version X of library Y; do you want to upgrade?").

### 6. Cleanup & Maintenance

- Occasionally review older entities / observations for relevance. Remove or archive ones that are obsolete.
- Merge or refactor memory entries when project structure changes (e.g. when modules are renamed or refactored).
- Ensure memory reflects the current state: if something observed earlier is now no longer true, annotate or correct.

### Example Triggers

- "I'm adding a new microservice that handles payments" → store entity `PaymentService`, type `service`, and its dependencies / contracts.
- "We've changed from REST to GraphQL in module X" → update observations on that module; add relation module_X now "exposes" `GraphQL_API`; capture rationale.
- "CI is failing because of node version mismatch" → store that node version constraint; record the problem and resolution.
