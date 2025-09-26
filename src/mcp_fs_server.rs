use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::net::TcpStream;
use tokio::time::{Duration, timeout};

use crate::fs_bridge;
use mcp_types::MCP_SCHEMA_VERSION;

static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

pub async fn run() -> Result<()> {
    let bridge_addr = std::env::var("ACP_FS_BRIDGE_ADDR")
        .context("ACP_FS_BRIDGE_ADDR environment variable is required")?;
    let session_id = std::env::var("ACP_FS_SESSION_ID")
        .context("ACP_FS_SESSION_ID environment variable is required")?;
    serve_loop(&bridge_addr, session_id).await
}

async fn serve_loop(bridge_addr: &str, session_id: String) -> Result<()> {
    let stdin = tokio::io::stdin();
    let mut reader = BufReader::new(stdin).lines();
    let stdout = tokio::io::stdout();
    let mut writer = BufWriter::new(stdout);

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }

        let msg: serde_json::Value = serde_json::from_str(&line)?;
        let method = msg.get("method").and_then(|m| m.as_str());
        let id = msg.get("id").cloned();

        match method {
            Some("initialize") => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id.clone().unwrap_or(json!(null)),
                    "result": {
                        "capabilities": {
                            "tools": {
                                "listChanged": true
                            }
                        },
                        "serverInfo": {
                            "name": "codex-acp-fs",
                            "title": "Codex ACP Filesystem",
                            "version": env!("CARGO_PKG_VERSION"),
                        },
                    "protocolVersion": MCP_SCHEMA_VERSION,
                    }
                });
                write_message(&mut writer, response).await?;

                let notification = json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized",
                    "params": null
                });
                write_message(&mut writer, notification).await?;
            }
            Some("ping") => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id.clone().unwrap_or(json!(null)),
                    "result": {}
                });
                write_message(&mut writer, response).await?;
            }
            Some("tools/list") => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id.clone().unwrap_or(json!(null)),
                    "result": {
                        "tools": [
                            read_tool_definition(),
                            write_tool_definition(),
                        ]
                    }
                });
                write_message(&mut writer, response).await?;
            }
            Some("tools/call") => {
                let result = handle_tool_call(bridge_addr, &session_id, &msg).await;
                match result {
                    Ok(value) => {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id.clone().unwrap_or(json!(null)),
                            "result": value
                        });
                        write_message(&mut writer, response).await?;
                    }
                    Err(err) => {
                        let response = json!({
                            "jsonrpc": "2.0",
                            "id": id.clone().unwrap_or(json!(null)),
                            "error": {
                                "code": -32001,
                                "message": err.to_string(),
                            }
                        });
                        write_message(&mut writer, response).await?;
                    }
                }
            }
            _ => {
                if let Some(id_value) = id {
                    let response = json!({
                        "jsonrpc": "2.0",
                        "id": id_value,
                        "error": {
                            "code": -32601,
                            "message": "method not found"
                        }
                    });
                    write_message(&mut writer, response).await?;
                }
            }
        }
    }

    writer.flush().await?;
    Ok(())
}

async fn handle_tool_call(
    bridge_addr: &str,
    session_id: &str,
    message: &serde_json::Value,
) -> Result<serde_json::Value> {
    let params = message
        .get("params")
        .and_then(|p| p.get("arguments"))
        .ok_or_else(|| anyhow!("missing tool call arguments"))?;

    let name = message
        .get("params")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .unwrap_or_default();

    match name {
        "read_text_file" => {
            let path = params
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| anyhow!("path is required"))?;
            let line = params
                .get("line")
                .and_then(|l| l.as_u64())
                .map(|v| v as u32);
            let limit = params
                .get("limit")
                .and_then(|l| l.as_u64())
                .map(|v| v as u32);
            let response = perform_bridge_request(
                bridge_addr,
                session_id,
                fs_bridge::BridgeOp::Read,
                path,
                line,
                limit,
                None,
            )
            .await?;
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": response
                }]
            }))
        }
        "write_text_file" => {
            let path = params
                .get("path")
                .and_then(|p| p.as_str())
                .ok_or_else(|| anyhow!("path is required"))?;
            let content = params
                .get("content")
                .and_then(|c| c.as_str())
                .ok_or_else(|| anyhow!("content is required"))?;
            perform_bridge_request(
                bridge_addr,
                session_id,
                fs_bridge::BridgeOp::Write,
                path,
                None,
                None,
                Some(content.to_string()),
            )
            .await?;
            Ok(json!({
                "content": [{
                    "type": "text",
                    "text": "write completed"
                }]
            }))
        }
        other => Err(anyhow!("unknown tool {other}")),
    }
}

async fn perform_bridge_request(
    bridge_addr: &str,
    session_id: &str,
    op: fs_bridge::BridgeOp,
    path: &str,
    line: Option<u32>,
    limit: Option<u32>,
    content: Option<String>,
) -> Result<String> {
    let request_id = NEXT_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let mut stream = TcpStream::connect(bridge_addr)
        .await
        .with_context(|| format!("failed to connect to bridge at {bridge_addr}"))?;
    let (reader_half, mut writer_half) = stream.split();
    let mut reader = BufReader::new(reader_half).lines();

    let payload = serde_json::to_string(&json!({
        "id": request_id,
        "session_id": session_id,
        "op": match op {
            fs_bridge::BridgeOp::Read => "read",
            fs_bridge::BridgeOp::Write => "write",
        },
        "path": path,
        "line": line,
        "limit": limit,
        "content": content,
    }))?;

    writer_half.write_all(payload.as_bytes()).await?;
    writer_half.write_all(b"\n").await?;
    writer_half.flush().await?;

    let line = timeout(Duration::from_secs(5), reader.next_line())
        .await
        .map_err(|_| anyhow!("bridge request timed out"))??
        .ok_or_else(|| anyhow!("bridge closed connection"))?;

    let response: serde_json::Value = serde_json::from_str(&line)?;
    let success = response
        .get("success")
        .and_then(|s| s.as_bool())
        .unwrap_or(false);
    if success {
        Ok(response
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_string())
    } else {
        let message = response
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("bridge error");
        Err(anyhow!(message.to_string()))
    }
}

async fn write_message(
    writer: &mut BufWriter<tokio::io::Stdout>,
    value: serde_json::Value,
) -> Result<()> {
    let payload = serde_json::to_string(&value)?;
    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

fn read_tool_definition() -> serde_json::Value {
    json!({
        "name": "read_text_file",
        "description": "Read workspace files via ACP bridge.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to read." },
                "line": { "type": "integer", "minimum": 1, "description": "Optional start line (1-indexed)." },
                "limit": { "type": "integer", "minimum": 1, "description": "Optional number of lines to read." }
            },
            "required": ["path"],
            "additionalProperties": false
        }
    })
}

fn write_tool_definition() -> serde_json::Value {
    json!({
        "name": "write_text_file",
        "description": "Write workspace files via ACP bridge.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "Path to write." },
                "content": { "type": "string", "description": "Full file contents." }
            },
            "required": ["path", "content"],
            "additionalProperties": false
        }
    })
}
