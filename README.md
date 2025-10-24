# Codex ACP Agent

[![MSRV](https://img.shields.io/badge/MSRV-1.90%2B-blue.svg)](rust-toolchain.toml)
[![Edition](https://img.shields.io/badge/Edition-2024-blueviolet.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

> Most of this repository code is implemented and reviewed by `codex` agents.

An Agent Client Protocol (ACP)–compatible agent that bridges the OpenAI Codex runtime with ACP clients over stdio. This project is under active development — features are evolving and breaking changes are likely.

## Highlights

- Agent Client Protocol (ACP) over stdio using `agent-client-protocol`.
- Integrates with the Codex Rust workspace for conversation management and event streaming.
- Slash commands with ACP AvailableCommands updates (advertised to clients on session start).
- Status output tailored for IDEs (workspace, account, model, token usage).
- Supports ACP session modes: `read-only`, `auto` (default), and `full-access`.
- Automatically launches an internal MCP filesystem server (`acp_fs`) built with `rmcp`, so Codex reads/writes files through ACP tooling instead of shell commands.

## Features

- ACP Agent implementation
  - Handles `initialize`, `authenticate` (API key), `session/new`, `session/prompt`, `session/cancel`.
  - Streams Codex events (assistant text and deltas, reasoning deltas, token counts) as `session/update` notifications.

- Slash commands (advertised via `AvailableCommandsUpdate`)
  - Implemented today:
    - `/new` — Start a new chat during a conversation.
    - `/init` — Create an `AGENTS.md` with repository contributor guidance. Uses a bundled prompt (`src/agent/prompt_init_command.md`).
    - `/status` — Rich status (workspace, account, model, token usage).
    - `/compact` — Request Codex to compact/summarize the conversation to reduce context size.
    - `/review` — Ask Codex to review current changes, highlight issues, and suggest fixes.

- Session modes
  - Advertises `read-only`, `auto` (current), and `full-access` on new session.
  - Clients may switch modes via ACP `session/setMode`; the agent emits `CurrentModeUpdate`.

## Build

### Requirements

- Rust (Rust 2024 edition; rustc 1.90+ as pinned in `rust-toolchain.toml`).
- Network access for building Git dependencies (Codex workspace, ACP crate).

```bash
make build
```

> Tip: use `make release` (or `cargo build --release`) when shipping the binary to an IDE like Zed. The release build lives at `target/release/codex-acp`.

### Configuration in [Zed](https://zed.dev)

> Add this configuration to zed settings.
```json
"agent_servers": {
  "Codex": {
    "command": "/path/to/codex-acp",
    "args": [],
    "env": {
      "RUST_LOG": "info"
    }
  }
}
```

## Filesystem tooling

When a session starts, `codex-acp` spins up an in-process TCP bridge and registers an MCP server named `acp_fs` using `rmcp`. Codex then calls structured tools:

- `read_text_file` — reads workspace files via ACP `client.read_text_file`, falling back to local disk if the client lacks FS support.
- `write_text_file` — writes workspace files via ACP `client.write_text_file`, with a local fallback.
- `edit_text_file` — apply a focused replace in a file and persist.
- `multi_edit_text_file` — apply multiple sequential replacements and persist.

`codex-acp` also injects a default instruction reminding the model to use these tools rather than shelling out with `cat`/`tee`. If your client exposes filesystem capabilities, file access stays within ACP.

## Status Output (`/status`)

The `/status` command prints a human-friendly summary, e.g.:

```
📂 Workspace
  • Path: ~/path/to/workspace
  • Approval Mode: on-request
  • Sandbox: workspace-write
  • AGENTS files: (none)

👤 Account
  • Signed in with ChatGPT (or API key / Not signed in)
  • Login: user@example.com
  • Plan: Plus

🧠 Model
  • Name: gpt-5
  • Provider: OpenAI
  • Reasoning Effort: Medium
  • Reasoning Summaries: Auto

📊 Token Usage
  • Session ID: <uuid>
  • Input: 0
  • Output: 0
  • Total: 0
```

Notes
- Some fields may be unknown depending on your auth mode and environment.
- Token counts are aggregated from Codex `EventMsg::TokenCount` when available.

## Logging

`codex-acp` uses `tracing` + `tracing-subscriber` and can log to stderr and/or a file. Configure it via environment variables:

Environment variables (highest precedence first):
- `CODEX_LOG_FILE` — Path to append logs (non-rotating). Parent directories are created automatically. ANSI is disabled for file logs.
- `CODEX_LOG_DIR` — Directory for daily-rotated logs (file name: `acp.log`). Directory is created automatically. ANSI is disabled for file logs.
- `CODEX_LOG_STDERR` — Set to `0`, `false`, `off`, or `no` to disable stderr logging. Enabled by default.
- `RUST_LOG` — Standard filtering directives (defaults to `info` if unset/invalid). Examples: `info`, `debug`, `codex_acp=trace,rmcp=info`.

Behavior:
- If `CODEX_LOG_FILE` is set, logs go to stderr (unless disabled) and the specified file.
- Else if `CODEX_LOG_DIR` is set, logs go to stderr (unless disabled) and a daily-rotated file in that directory.
- Else logs go to stderr only (unless disabled).

Examples:
```bash
# Console only
RUST_LOG=info cargo run --quiet

# Console + append to file (non-rotating)
RUST_LOG=debug CODEX_LOG_FILE=./logs/codex-acp.log cargo run --quiet

# Console + daily rotation under logs directory
RUST_LOG=info CODEX_LOG_DIR=./logs cargo run --quiet

# File only (disable stderr)
CODEX_LOG_STDERR=0 CODEX_LOG_FILE=./logs/codex-acp.log cargo run --quiet

# MCP filesystem server also honors logging env:
RUST_LOG=debug CODEX_LOG_DIR=./logs cargo run --quiet -- --acp-fs-mcp
```

## Development

- Branching: prefer topic branches; small, focused commits.
- Lint/test locally using `cargo check`, `cargo fmt`, `cargo clippy`, and `cargo test`.
- Logging: see the Logging section above for configuration. Typical dev setup: `RUST_LOG=info`.

## Related Projects

- Zed ACP example (Claude): https://github.com/zed-industries/claude-code-acp
- Agent Client Protocol (Rust): https://crates.io/crates/agent-client-protocol
- OpenAI Codex (Rust workspace): https://github.com/openai/codex
