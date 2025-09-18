#!/usr/bin/env bash
set -euo pipefail

# Minimal ACP stdio smoke test: initialize -> new session -> /status

RUST_LOG=${RUST_LOG:-info}
CMD=(cargo run --quiet)

printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"v1","clientName":"cli","capabilities":{}}}' \
  '{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"'"$PWD"'","mcpServers":[]}}' \
  '{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"1","prompt":[{"type":"text","text":"/status"}]}}' \
| RUST_LOG="$RUST_LOG" "${CMD[@]}"

