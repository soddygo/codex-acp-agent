use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use agent_client_protocol as acp;
use codex_core::protocol::{FileChange, McpInvocation, ReviewDecision};
use codex_protocol::parse_command::ParsedCommand;
use serde_json::json;

use super::utils;

/// Arguments for "Exec Command End" update generation.
pub struct ExecEndArgs {
    pub call_id: String,
    pub exit_code: i32,
    pub aggregated_output: String,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub formatted_output: String,
}

/// Centralized helpers to translate Codex Event data into ACP updates and requests.
///
/// This module does not send updates itself; instead, it produces ACP model
/// structures (`SessionUpdate`, `RequestPermissionRequest`, etc.) that the
/// caller can pass to their transport layer. This makes it easier to unit test
/// the formatting logic and to keep the agent's event loop focused.
pub struct EventHandler {
    cwd: PathBuf,
    support_terminal: bool,
    permission_options: Arc<Vec<acp::PermissionOption>>,
}

impl EventHandler {
    /// Create a new handler with the workspace `cwd` and whether the client supports terminals.
    pub fn new(cwd: PathBuf, support_terminal: bool) -> Self {
        Self {
            cwd,
            support_terminal,
            permission_options: default_permission_options(),
        }
    }
    // ---- MCP tool calls ----

    /// Build a ToolCall update for "MCP Tool Call Begin".
    pub fn on_mcp_tool_call_begin(
        &self,
        call_id: &str,
        invocation: &McpInvocation,
    ) -> acp::SessionUpdate {
        let (title, locations) = utils::describe_mcp_tool(invocation, &self.cwd);
        let tool = acp::ToolCall {
            id: acp::ToolCallId(call_id.into()),
            title,
            kind: acp::ToolKind::Fetch,
            status: acp::ToolCallStatus::InProgress,
            content: Vec::new(),
            locations,
            raw_input: invocation.arguments.clone(),
            raw_output: None,
            meta: None,
        };
        acp::SessionUpdate::ToolCall(tool)
    }

    /// Build a ToolCallUpdate for "MCP Tool Call End".
    pub fn on_mcp_tool_call_end(
        &self,
        call_id: &str,
        invocation: &McpInvocation,
        result: &serde_json::Value,
        success: bool,
    ) -> acp::SessionUpdate {
        let status = if success {
            acp::ToolCallStatus::Completed
        } else {
            acp::ToolCallStatus::Failed
        };
        let raw_output = Some(result.clone());
        let (title, locations) = utils::describe_mcp_tool(invocation, &self.cwd);
        let update = acp::ToolCallUpdate {
            id: acp::ToolCallId(call_id.into()),
            fields: acp::ToolCallUpdateFields {
                status: Some(status),
                title: Some(title),
                locations: if locations.is_empty() {
                    None
                } else {
                    Some(locations)
                },
                raw_output,
                ..Default::default()
            },
            meta: None,
        };
        acp::SessionUpdate::ToolCallUpdate(update)
    }

    // ---- Exec command calls ----

    /// Build a ToolCall for "Exec Command Begin".
    pub fn on_exec_command_begin(
        &self,
        call_id: &str,
        cwd: &Path,
        command: &[String],
        parsed_cmd: &[ParsedCommand],
    ) -> acp::SessionUpdate {
        let utils::FormatCommandCall {
            title,
            locations,
            terminal_output,
            kind,
        } = utils::format_command_call(cwd, parsed_cmd);

        let (content, meta) = if self.support_terminal && terminal_output {
            let content = vec![acp::ToolCallContent::Terminal {
                terminal_id: acp::TerminalId(call_id.into()),
            }];
            let meta = Some(json!({
                "terminal_info": {
                    "terminal_id": call_id,
                    "cwd": cwd
                }
            }));
            (content, meta)
        } else {
            (vec![], None)
        };

        let tool = acp::ToolCall {
            id: acp::ToolCallId(call_id.into()),
            title,
            kind,
            status: acp::ToolCallStatus::InProgress,
            content,
            locations,
            raw_input: Some(json!({
                "command": command,
                "command_string": command.join(" "),
                "cwd": cwd
            })),
            raw_output: None,
            meta,
        };
        acp::SessionUpdate::ToolCall(tool)
    }

    /// Arguments for "Exec Command End" update generation.
    /// Build a ToolCallUpdate for "Exec Command End".
    pub fn on_exec_command_end(&self, end: ExecEndArgs) -> acp::SessionUpdate {
        let status = if end.exit_code == 0 {
            acp::ToolCallStatus::Completed
        } else {
            acp::ToolCallStatus::Failed
        };

        let mut content: Vec<acp::ToolCallContent> = Vec::new();
        if !end.aggregated_output.is_empty() {
            content.push(acp::ToolCallContent::from(end.aggregated_output.clone()));
        } else if !end.stdout.is_empty() || !end.stderr.is_empty() {
            let merged = if !end.stderr.is_empty() {
                format!("{}\n{}", end.stdout, end.stderr)
            } else {
                end.stdout.clone()
            };
            if !merged.is_empty() {
                content.push(acp::ToolCallContent::from(merged));
            }
        }

        let update = acp::ToolCallUpdate {
            id: acp::ToolCallId(end.call_id.into()),
            fields: acp::ToolCallUpdateFields {
                status: Some(status),
                content: if content.is_empty() {
                    None
                } else {
                    Some(content)
                },
                raw_output: Some(json!({
                    "exit_code": end.exit_code,
                    "duration_ms": end.duration_ms,
                    "formatted_output": end.formatted_output,
                })),
                ..Default::default()
            },
            meta: None,
        };

        acp::SessionUpdate::ToolCallUpdate(update)
    }

    /// Build a permission request for an exec approval.
    pub fn on_exec_approval_request(
        &self,
        session_id: &acp::SessionId,
        call_id: &str,
        cwd: &Path,
        parsed_cmd: &[ParsedCommand],
    ) -> acp::RequestPermissionRequest {
        let utils::FormatCommandCall {
            title,
            locations,
            terminal_output: _,
            kind,
        } = utils::format_command_call(cwd, parsed_cmd);

        let update = acp::ToolCallUpdate {
            id: acp::ToolCallId(call_id.into()),
            fields: acp::ToolCallUpdateFields {
                kind: Some(kind),
                status: Some(acp::ToolCallStatus::Pending),
                title: Some(title),
                locations: if locations.is_empty() {
                    None
                } else {
                    Some(locations)
                },
                ..Default::default()
            },
            meta: None,
        };

        acp::RequestPermissionRequest {
            session_id: session_id.clone(),
            tool_call: update,
            options: self.permission_options.as_ref().clone(),
            meta: None,
        }
    }

    // ---- Patch approval ----

    /// Build a permission request for "Apply Patch Approval Request".
    pub fn on_apply_patch_approval_request(
        &self,
        session_id: &acp::SessionId,
        call_id: &str,
        changes: &[(String, FileChange)],
    ) -> acp::RequestPermissionRequest {
        let mut contents: Vec<acp::ToolCallContent> = Vec::new();
        for (path, change) in changes.iter() {
            match change {
                FileChange::Add { content } => {
                    contents.push(acp::ToolCallContent::from(acp::Diff {
                        path: PathBuf::from(path),
                        old_text: None,
                        new_text: content.clone(),
                        meta: None,
                    }));
                }
                FileChange::Delete { content } => {
                    contents.push(acp::ToolCallContent::from(acp::Diff {
                        path: PathBuf::from(path),
                        old_text: Some(content.clone()),
                        new_text: "".into(),
                        meta: None,
                    }));
                }
                FileChange::Update { unified_diff, .. } => {
                    contents.push(acp::ToolCallContent::from(acp::Diff {
                        path: PathBuf::from(path),
                        old_text: Some(unified_diff.into()),
                        new_text: unified_diff.clone(),
                        meta: None,
                    }));
                }
            }
        }

        let title = if changes.len() == 1 {
            "Apply changes".to_string()
        } else {
            format!("Edit {} files", changes.len())
        };

        let update = acp::ToolCallUpdate {
            id: acp::ToolCallId(call_id.into()),
            fields: acp::ToolCallUpdateFields {
                kind: Some(acp::ToolKind::Edit),
                status: Some(acp::ToolCallStatus::Pending),
                title: Some(title),
                content: if contents.is_empty() {
                    None
                } else {
                    Some(contents)
                },
                ..Default::default()
            },
            meta: None,
        };

        acp::RequestPermissionRequest {
            session_id: session_id.clone(),
            tool_call: update,
            options: self.permission_options.as_ref().clone(),
            meta: None,
        }
    }

    /// Build a ToolCallUpdate for "Patch Apply End".
    pub fn on_patch_apply_end(
        &self,
        call_id: &str,
        success: bool,
        raw_event_json: serde_json::Value,
    ) -> acp::SessionUpdate {
        let update = acp::ToolCallUpdate {
            id: acp::ToolCallId(call_id.into()),
            fields: acp::ToolCallUpdateFields {
                status: Some(if success {
                    acp::ToolCallStatus::Completed
                } else {
                    acp::ToolCallStatus::Failed
                }),
                raw_output: Some(raw_event_json),
                ..Default::default()
            },
            meta: None,
        };

        acp::SessionUpdate::ToolCallUpdate(update)
    }
}

/// Map an approval response to the `ReviewDecision` used by Codex operations.
pub fn handle_response_outcome(resp: acp::RequestPermissionResponse) -> ReviewDecision {
    match resp.outcome {
        acp::RequestPermissionOutcome::Selected { option_id } => match option_id.0.as_ref() {
            "approved" => ReviewDecision::Approved,
            "approved-for-session" => ReviewDecision::ApprovedForSession,
            _ => ReviewDecision::Abort,
        },
        acp::RequestPermissionOutcome::Cancelled => ReviewDecision::Abort,
    }
}

/// Build the default permission options set for approval requests.
pub fn default_permission_options() -> Arc<Vec<acp::PermissionOption>> {
    Arc::new(vec![
        acp::PermissionOption {
            id: acp::PermissionOptionId("approved-for-session".into()),
            name: "Approved Always".into(),
            kind: acp::PermissionOptionKind::AllowAlways,
            meta: None,
        },
        acp::PermissionOption {
            id: acp::PermissionOptionId("approved".into()),
            name: "Approved".into(),
            kind: acp::PermissionOptionKind::AllowOnce,
            meta: None,
        },
        acp::PermissionOption {
            id: acp::PermissionOptionId("abort".into()),
            name: "Reject".into(),
            kind: acp::PermissionOptionKind::RejectOnce,
            meta: None,
        },
    ])
}

/// Aggregates reasoning deltas and sections to produce a compact text output.
///
/// This mirrors the logic used by the agent to collate streaming reasoning.
/// It can be used to decouple reasoning accumulation from the main event loop.
pub struct ReasoningAggregator {
    sections: Vec<String>,
    current: String,
}

impl ReasoningAggregator {
    pub fn new() -> Self {
        Self {
            sections: Vec::new(),
            current: String::new(),
        }
    }

    pub fn reset(&mut self) {
        self.sections.clear();
        self.current.clear();
    }

    pub fn append_delta(&mut self, delta: &str) {
        self.current.push_str(delta);
    }

    pub fn section_break(&mut self) {
        if !self.current.is_empty() {
            let chunk = std::mem::take(&mut self.current);
            self.sections.push(chunk);
        }
    }

    /// Returns combined text with double newlines between sections, trimming trailing whitespace.
    pub fn take_text(&mut self) -> Option<String> {
        let mut combined = String::new();
        let mut first = true;

        for section in self.sections.drain(..) {
            if section.trim().is_empty() {
                continue;
            }
            if !first {
                combined.push_str("\n\n");
            }
            combined.push_str(section.trim_end());
            first = false;
        }

        if !self.current.trim().is_empty() {
            if !first {
                combined.push_str("\n\n");
            }
            combined.push_str(self.current.trim_end());
        }

        self.current.clear();

        if combined.is_empty() {
            None
        } else {
            Some(combined)
        }
    }

    /// Given a final reasoning text (if any), choose the longer, non-empty variant
    /// between the aggregated text and the final text.
    pub fn choose_final_text(&mut self, final_text: Option<String>) -> Option<String> {
        let aggregated = self.take_text();
        match (aggregated, final_text) {
            (Some(agg), Some(final_text)) => {
                if final_text.trim().len() > agg.trim().len() {
                    Some(final_text)
                } else {
                    Some(agg)
                }
            }
            (Some(agg), None) => Some(agg),
            (None, Some(final_text)) => Some(final_text),
            (None, None) => None,
        }
    }
}
