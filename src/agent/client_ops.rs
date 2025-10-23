use std::path::PathBuf;

use agent_client_protocol as acp;
use agent_client_protocol::Error;
use tokio::sync::{mpsc, oneshot};

/// Returns whether the client supports reading text files.
pub fn supports_fs_read(cap: &acp::ClientCapabilities) -> bool {
    cap.fs.read_text_file
}

/// Request the client to read a text file, optionally starting at a given line and with a line limit.
///
/// - `session_id`: The ACP session id. The agent may remap it to an internal one before forwarding downstream.
/// - `path`: The file path to read (absolute or relative to the workspace).
/// - `line`: Optional starting line (1-based).
/// - `limit`: Optional line count limit.
///
/// On transport/channel errors, returns an internal error with a descriptive message.
pub async fn read_text_file(
    client_tx: &mpsc::UnboundedSender<super::ClientOp>,
    session_id: &acp::SessionId,
    path: impl Into<PathBuf>,
    line: Option<u32>,
    limit: Option<u32>,
) -> Result<acp::ReadTextFileResponse, Error> {
    let request = acp::ReadTextFileRequest {
        session_id: session_id.clone(),
        path: path.into(),
        line,
        limit,
        meta: None,
    };

    let (tx, rx) = oneshot::channel();
    client_tx
        .send(super::ClientOp::ReadTextFile(request, tx))
        .map_err(|_| internal_err("client read_text_file channel closed"))?;

    rx.await
        .map_err(|_| internal_err("client read_text_file response dropped"))?
}

/// Request the client to write a text file.
///
/// - `overwrite` semantics are up to the client/tooling on the other side.
/// - Consider checking `supports_fs_write` before calling.
///
/// On transport/channel errors, returns an internal error with a descriptive message.
#[allow(dead_code)]
pub async fn write_text_file(
    client_tx: &mpsc::UnboundedSender<super::ClientOp>,
    session_id: &acp::SessionId,
    path: impl Into<PathBuf>,
    content: String,
) -> Result<acp::WriteTextFileResponse, Error> {
    let request = acp::WriteTextFileRequest {
        session_id: session_id.clone(),
        path: path.into(),
        content,
        meta: None,
    };

    let (tx, rx) = oneshot::channel();
    client_tx
        .send(super::ClientOp::WriteTextFile(request, tx))
        .map_err(|_| internal_err("client write_text_file channel closed"))?;

    rx.await
        .map_err(|_| internal_err("client write_text_file response dropped"))?
}

fn internal_err(msg: &str) -> Error {
    Error::internal_error().with_data(msg)
}
