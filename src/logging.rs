//! Centralized tracing initialization for codex-acp.
//!
//! Features:
//! - RUST_LOG-compatible filtering via `tracing-subscriber`'s `EnvFilter`.
//! - Dual output: stderr + file (if configured).
//! - Optional daily log rotation when a log directory is provided.
//! - Non-blocking file writes with a guard to flush logs on shutdown.
//!
//! Environment variables (from highest to lowest precedence for file output):
//! - CODEX_LOG_FILE: absolute or relative file path to append logs (no rotation).
//! - CODEX_LOG_DIR: directory for daily-rotated logs (file name: "acp.log").
//! - CODEX_LOG_STDERR: "0" or "false" disables stderr logging; otherwise enabled.
//! - RUST_LOG: standard logging filter (e.g., "info", "debug", "codex_acp=trace,rmcp=info").
//!
//! Usage:
//! - Call `init_from_env()` at process startup and hold on to the returned `LoggingGuard`
//!   for the lifetime of the program to ensure logs are flushed on shutdown.
//!
//! Example:
//!     let _logging = codex_acp::logging::init_from_env()?;
//!     // run application...
//!
//! Notes:
//! - Calling initialization more than once is safe; subsequent calls are no-ops.
//! - ANSI color is disabled for file output to keep logs clean.
//! - Parent directories for CODEX_LOG_FILE/CODEX_LOG_DIR are created if needed.

use std::{env, fs, fs::OpenOptions, path::Path};

use anyhow::Result;
use tracing_appender::non_blocking::{self, WorkerGuard};
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

/// A guard that keeps the non-blocking file writer alive until dropped,
/// ensuring logs are flushed on process shutdown.
pub struct LoggingGuard {
    _file_guard: Option<WorkerGuard>,
}

impl LoggingGuard {
    fn none() -> Self {
        Self { _file_guard: None }
    }
    fn with_guard(guard: WorkerGuard) -> Self {
        Self {
            _file_guard: Some(guard),
        }
    }
}

/// Initialize global tracing subscriber from environment variables.
/// - RUST_LOG controls filtering (defaults to "info" if not set or invalid).
/// - CODEX_LOG_FILE selects an explicit file (no rotation).
/// - CODEX_LOG_DIR selects daily-rotated logs in the provided directory.
/// - CODEX_LOG_STDERR disables stderr logging when set to "0" or "false".
///
/// Returns a LoggingGuard that must be kept alive for the duration of the process.
pub fn init_from_env() -> Result<LoggingGuard> {
    // Build EnvFilter from RUST_LOG or default to "info".
    let filter = EnvFilter::try_from_default_env().or_else(|_| EnvFilter::try_new("info"))?;

    // Determine stderr logging behavior.
    let stderr_enabled = env::var("CODEX_LOG_STDERR")
        .map(|v| {
            let v = v.to_ascii_lowercase();
            !(v == "0" || v == "false" || v == "off" || v == "no")
        })
        .unwrap_or(true);

    // Determine file logging behavior.
    let file_path = env::var("CODEX_LOG_FILE").ok();
    let dir_path = env::var("CODEX_LOG_DIR").ok();

    // Build optional layers and a guard in one pass.
    let mut file_guard: Option<WorkerGuard> = None;

    let stderr_layer = if stderr_enabled {
        Some(fmt::layer().with_target(true))
    } else {
        None
    };

    // File layer (non-rotating) takes precedence over directory-based rotation.
    let file_layer = if let Some(file) = file_path {
        let (nb, guard) = non_blocking_writer_for_file(&file)?;
        file_guard = Some(guard);
        Some(
            fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_writer(nb),
        )
    } else if let Some(dir) = dir_path {
        let (nb, guard) = non_blocking_writer_for_daily(dir, "acp.log")?;
        file_guard = Some(guard);
        Some(
            fmt::layer()
                .with_ansi(false)
                .with_target(true)
                .with_writer(nb),
        )
    } else {
        None
    };

    // Chain all layers in a single expression to avoid type-mismatch on reassignment.
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(stderr_layer)
        .with(file_layer);

    // Try init; ignore error if already initialized elsewhere.
    let _ = subscriber.try_init();

    Ok(match file_guard {
        Some(guard) => LoggingGuard::with_guard(guard),
        None => LoggingGuard::none(),
    })
}

/// Build a non-blocking writer for an explicit file path.
/// Ensures parent directories exist. Appends to the file if it exists.
fn non_blocking_writer_for_file<P: AsRef<Path>>(
    path: P,
) -> Result<(non_blocking::NonBlocking, WorkerGuard)> {
    let path = path.as_ref();
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    let file = OpenOptions::new().create(true).append(true).open(path)?;
    Ok(tracing_appender::non_blocking(file))
}

/// Build a non-blocking writer with daily rotation in a directory.
/// Ensures directory exists. Uses `file_name` for the rotated files.
fn non_blocking_writer_for_daily<P: AsRef<Path>>(
    dir: P,
    file_name: &str,
) -> Result<(non_blocking::NonBlocking, WorkerGuard)> {
    let dir = dir.as_ref();
    if !dir.as_os_str().is_empty() {
        fs::create_dir_all(dir)?;
    }
    let file_appender = tracing_appender::rolling::daily(dir, file_name);
    Ok(tracing_appender::non_blocking(file_appender))
}
