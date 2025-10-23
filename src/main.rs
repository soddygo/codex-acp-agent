use codex_acp::agent;
use codex_acp::{CodexAgent, FsBridge, SessionModeLookup};

use agent_client_protocol::{AgentSideConnection, Client, Error};
use anyhow::Result;
use std::env;
use tokio::{io, sync::mpsc, task};
use tokio_util::compat::{TokioAsyncReadCompatExt as _, TokioAsyncWriteCompatExt as _};
use tracing::error;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use codex_core::config::{Config, ConfigOverrides};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    if env::args().nth(1).as_deref() == Some("--acp-fs-mcp") {
        return codex_acp::fs::run_mcp_server().await;
    }

    // Initialize tracing with env filter (RUST_LOG compatible).
    let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(filter)
        .init();

    let outgoing = io::stdout().compat_write();
    let incoming = io::stdin().compat();

    let local_set = task::LocalSet::new();
    local_set.run_until(async move {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let (client_tx, mut client_rx) = mpsc::unbounded_channel();

        let config = Config::load_with_cli_overrides(vec![], ConfigOverrides::default()).await?;
        let fs_bridge = FsBridge::start(client_tx.clone(), config.cwd.clone()).await?;
        let agent = CodexAgent::with_config(tx, client_tx, config, Some(fs_bridge));
        let session_modes = SessionModeLookup::from(&agent);
        let (conn, handle_io) = AgentSideConnection::new(agent, outgoing, incoming, |fut| {
            task::spawn_local(fut);
        });

        task::spawn_local(async move {
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        match msg {
                            Some((session_notification, tx)) => {
                                let result = conn.session_notification(session_notification).await;
                                if let Err(e) = result { error!(error = ?e, "failed to send session notification"); break; }
                                let _ = tx.send(());
                            }
                            None => break,
                        }
                    }
                    op = client_rx.recv() => {
                        match op {
                            Some(agent::ClientOp::RequestPermission(req, tx)) => {
                                let res = conn.request_permission(req).await;
                                let _ = tx.send(res);
                            }
                            Some(agent::ClientOp::ReadTextFile(mut req, tx)) => {
                                match session_modes.resolve_acp_session_id(&req.session_id) {
                                    Some(resolved_id) => {
                                        req.session_id = resolved_id;
                                        let res = conn.read_text_file(req).await;
                                        let _ = tx.send(res);
                                    }
                                    None => {
                                        let err = Error::invalid_params()
                                            .with_data("unknown session for read_text_file");
                                        let _ = tx.send(Err(err));
                                    }
                                }
                            }
                            Some(agent::ClientOp::WriteTextFile(mut req, tx)) => {
                                match session_modes.resolve_acp_session_id(&req.session_id) {
                                    Some(resolved_id) => {
                                        req.session_id = resolved_id.clone();
                                        if session_modes.is_read_only(&resolved_id) {
                                            let err = Error::invalid_params()
                                                .with_data("write_text_file is disabled while session mode is read-only");
                                            let _ = tx.send(Err(err));
                                        } else {
                                            let res = conn.write_text_file(req).await;
                                            let _ = tx.send(res);
                                        }
                                    }
                                    None => {
                                        let err = Error::invalid_params()
                                            .with_data("unknown session for write_text_file");
                                        let _ = tx.send(Err(err));
                                    }
                                }
                            }
                            None => break,
                        }
                    }
                }
            }
        });

        match handle_io.await {
            Ok(()) => Ok(()),
            Err(e) => Err(anyhow::Error::new(e)),
        }
    }).await
}
