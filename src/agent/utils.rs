use std::path::{Path, PathBuf};

use agent_client_protocol as acp;
use codex_core::protocol::McpInvocation;
use codex_protocol::parse_command::ParsedCommand;

/// Formatted summary for a command/tool call used by ACP updates.
#[derive(Clone, Debug)]
pub struct FormatCommandCall {
    pub title: String,
    pub terminal_output: bool,
    pub locations: Vec<acp::ToolCallLocation>,
    pub kind: acp::ToolKind,
}

/// Metadata describing an FS tool call, including a display path and an
/// optional source location line for deep-linking in clients.
#[derive(Clone, Debug)]
pub struct FsToolMetadata {
    pub display_path: String,
    pub location_path: PathBuf,
    pub line: Option<u32>,
}

/// Format a tool/command call for display in the client, summarizing a
/// sequence of parsed commands into a single title, the kind, locations,
/// and whether terminal output should be rendered.
pub fn format_command_call(cwd: &Path, parsed_cmd: &[ParsedCommand]) -> FormatCommandCall {
    let mut titles = Vec::new();
    let mut locations = Vec::new();
    let mut terminal_output = false;
    let mut kind = acp::ToolKind::Execute;

    for cmd in parsed_cmd {
        let mut cmd_path: Option<PathBuf> = None;
        match cmd {
            ParsedCommand::Read { cmd: _, name, path } => {
                titles.push(format!("Read {name}"));
                cmd_path = Some(path.clone());
                kind = acp::ToolKind::Read;
            }
            ParsedCommand::ListFiles { cmd: _, path } => {
                let dir = if let Some(path) = path.as_ref() {
                    cwd.join(path)
                } else {
                    cwd.to_path_buf()
                };
                titles.push(format!("List {}", dir.display()));
                cmd_path = path.as_ref().map(PathBuf::from);
                kind = acp::ToolKind::Search;
            }
            ParsedCommand::Search { cmd, query, path } => {
                let label = match (query, path.as_ref()) {
                    (Some(query), Some(path)) => format!("Search {query} in {path}"),
                    (Some(query), None) => format!("Search {query}"),
                    _ => format!("Search {}", cmd),
                };
                titles.push(label);
                cmd_path = path.as_ref().map(PathBuf::from);
                kind = acp::ToolKind::Search;
            }
            ParsedCommand::Unknown { cmd } => {
                titles.push(format!("Run {cmd}"));
                terminal_output = true;
            }
        }

        if let Some(path) = cmd_path {
            locations.push(acp::ToolCallLocation {
                path: if path.is_relative() {
                    cwd.join(&path)
                } else {
                    path
                },
                line: None,
                meta: None,
            });
        }
    }

    FormatCommandCall {
        title: titles.join(", "),
        terminal_output,
        locations,
        kind,
    }
}

/// Return a user-friendly display path for a raw path string.
/// If `raw_path` is within `cwd`, return a relative path; otherwise, fall back
/// to the file name or the original raw string.
pub fn display_fs_path(cwd: &Path, raw_path: &str) -> String {
    let path = Path::new(raw_path);
    if let Ok(relative) = path.strip_prefix(cwd) {
        let rel_display = relative.display().to_string();
        if !rel_display.is_empty() {
            return rel_display;
        }
    }

    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| raw_path.to_string())
}

/// Extract FS tool metadata from an MCP invocation, when applicable.
/// Only tools from the "acp_fs" server and supported tool names are considered.
pub fn fs_tool_metadata(invocation: &McpInvocation, cwd: &Path) -> Option<FsToolMetadata> {
    if invocation.server != "acp_fs" {
        return None;
    }

    match invocation.tool.as_str() {
        "read_text_file" | "write_text_file" | "edit_text_file" => {}
        _ => return None,
    }

    let args = invocation.arguments.as_ref()?.as_object()?;
    let path = args.get("path")?.as_str()?.to_string();
    let line = args
        .get("line")
        .and_then(|value| value.as_u64())
        .map(|value| value as u32);
    let display_path = display_fs_path(cwd, &path);
    let location_path = PathBuf::from(&path);

    Some(FsToolMetadata {
        display_path,
        location_path,
        line,
    })
}

/// Describe an MCP tool call for ACP by creating a human-friendly title and
/// mapping to zero or more `ToolCallLocation`s. When the invocation is an
/// FS tool, the title includes the display path and a single location entry.
pub fn describe_mcp_tool(
    invocation: &McpInvocation,
    cwd: &Path,
) -> (String, Vec<acp::ToolCallLocation>) {
    if let Some(metadata) = fs_tool_metadata(invocation, cwd) {
        let location = acp::ToolCallLocation {
            path: metadata.location_path,
            line: metadata.line,
            meta: None,
        };
        (
            format!(
                "{}.{} ({})",
                invocation.server, invocation.tool, metadata.display_path
            ),
            vec![location],
        )
    } else {
        (
            format!("{}.{}", invocation.server, invocation.tool),
            Vec::new(),
        )
    }
}
