# Codex ACP Agent

[![MSRV](https://img.shields.io/badge/MSRV-1.89%2B-blue.svg)](rust-toolchain.toml)
[![Edition](https://img.shields.io/badge/Edition-2024-blueviolet.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/index.html)

An Agent Client Protocol (ACP)–compatible agent that bridges the OpenAI Codex runtime with ACP clients over stdio. This project is under active development — features are evolving and breaking changes are likely.

## Highlights

- Agent Client Protocol (ACP) over stdio using `agent-client-protocol`.
- Integrates with the Codex Rust workspace for conversation management and event streaming.
- Slash commands with ACP AvailableCommands updates (advertised to clients on session start).
- Status output tailored for IDEs (workspace, account, model, token usage).
- Discovers custom prompts via `Op::ListCustomPrompts` and advertises them as commands.

## Status: Work in Progress

This repository is a work-in-progress implementation. Some commands are stubs, some behaviors will change, and additional capabilities are planned. Use at your own risk and expect rough edges.

## Requirements

- Rust (Rust 2024 edition; rustc 1.89+ as pinned in `rust-toolchain.toml`).
- Network access for building Git dependencies (Codex workspace, ACP crate).

Optional for development:
- To run without Codex backend (for ACP flow testing), set `ACP_DEV_ALLOW_MOCK=1` to enable a mock session that supports slash commands like `/status` but does not call the Codex backend.

## Build

```bash
cargo build
```

## Run

The agent communicates over stdin/stdout using ACP JSON-RPC. Launch it and connect from an ACP client (e.g., an IDE integration or a CLI client implementing ACP):

```bash
# With tracing logs
RUST_LOG=info cargo run --quiet
```

Because this agent speaks on stdio, it is intended to be spawned by your client. For manual testing, you can pipe ACP JSON-RPC messages to stdin and read replies from stdout.

Example JSON-RPC (initialize → new session → /status):

```
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"v1","clientName":"cli","capabilities":{}}}
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/absolute/path","mcpServers":[]}}
{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"1","prompt":[{"type":"text","text":"/status"}]}}
```

## Usage (ACP over stdio)

Minimal smoke test from a shell piping JSON-RPC over stdio:

```bash
printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"v1","clientName":"cli","capabilities":{}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"'"$PWD"'","mcpServers":[]}}' \
  '{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"1","prompt":[{"type":"text","text":"/status"}]}}' \
| RUST_LOG=info cargo run --quiet
```

Or use the included script and Makefile target:

```bash
chmod +x scripts/stdio-smoke.sh
make smoke
```

### Configuration in [Zed](https://zed.dev)

> Add this configuration to zed settings.
```json
"agent_servers": {
  "Codex": {
    "command": "codex-acp",
    "args": [],
    "env": {
      "RUST_LOG": "info"
    }
  }
}
```

## Features

- ACP Agent implementation
  - Handles `initialize`, `authenticate` (no-op for now), `session/new`, `session/prompt`, `session/cancel`.
  - Streams Codex events (assistant text and deltas, reasoning deltas, token counts) as `session/update` notifications.

- Slash commands (advertised via `AvailableCommandsUpdate`)
  - Implemented today:
    - `/init` - Create an AGENTS.md file with instructions for Codex
    - `/model` — Show or set the current model (uses `Op::OverrideTurnContext`).
    - `/approvals` — Set approval mode (`untrusted | on-request | on-failure | never`).
    - `/status` — Rich status (workspace, account, model, token usage).
  - Stubs or client-driven features (not fully implemented in the agent):
    - `/init` — Creating AGENTS.md is a client/UX flow; we just inform the user.
    - `/diff` — Git diff visualization is a TUI/client concern.
    - `/mention` — Mentions are a client UX feature.

- Available commands with custom prompts
  - On new session the agent first advertises built-in commands.
  - It then requests `Op::ListCustomPrompts` from Codex and advertises discovered prompts as additional commands (name + path in description). These are discoverable in client popups that read `available_commands_update`.

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

## Development

- Branching: prefer topic branches; small, focused commits.
- Lint/test locally using `cargo check`, `cargo fmt`, `cargo clippy`, and `cargo test`.
- Logging uses `tracing` + `tracing-subscriber`; use `RUST_LOG=info` during development.

## Related Projects

- Zed ACP example (Claude): https://github.com/zed-industries/claude-code-acp
- Agent Client Protocol (Rust): https://crates.io/crates/agent-client-protocol
- OpenAI Codex (Rust workspace): https://github.com/openai/codex
