use agent_client_protocol as acp;
use codex_core::protocol::{ErrorEvent, EventMsg, Op, PatchApplyEndEvent, StreamErrorEvent};
use codex_protocol::{
    plan_tool::{StepStatus, UpdatePlanArgs},
    user_input::UserInput,
};
use serde_json::json;
use tokio::sync::oneshot;
use tracing::info;

use super::{core::CodexAgent, events, session::ClientOp};

impl CodexAgent {
    /// Process a user prompt and stream responses back to the client.
    ///
    /// This handles:
    /// - Slash commands (e.g., /status, /help)
    /// - Text, image, audio, and resource content blocks
    /// - Streaming agent responses, reasoning, and tool calls
    /// - Approval requests for commands and file operations
    pub(super) async fn prompt(
        &self,
        args: acp::PromptRequest,
    ) -> Result<acp::PromptResponse, acp::Error> {
        info!(?args, "Received prompt request");
        let event_handler =
            events::EventHandler::new(self.config.cwd.clone(), self.support_terminal());
        let mut reason = events::ReasoningAggregator::new();
        let conversation = self.get_conversation(&args.session_id).await?;

        let mut op_opt = None;
        // Handle slash commands (e.g., "/status") when the first block is text starting with '/'
        if let Some(acp::ContentBlock::Text(t)) = args.prompt.first() {
            let line = t.text.trim();
            if let Some(cmd) = line.strip_prefix('/') {
                let mut parts = cmd.split_whitespace();
                let name = parts.next().unwrap_or("").to_lowercase();
                match self.handle_slash_command(&args.session_id, &name).await {
                    Some(op) => {
                        op_opt = Some(op);
                    }
                    None => {
                        return Ok(acp::PromptResponse {
                            stop_reason: acp::StopReason::EndTurn,
                            meta: None,
                        });
                    }
                }
            }
        }

        reason.reset();

        // Build user input submission items from prompt content blocks.
        let mut items: Vec<UserInput> = Vec::new();
        for block in &args.prompt {
            match block {
                acp::ContentBlock::Text(t) => {
                    items.push(UserInput::Text {
                        text: t.text.clone(),
                    });
                }
                acp::ContentBlock::Image(img) => {
                    let url = format!("data:{};base64,{}", img.mime_type, img.data);
                    items.push(UserInput::Image { image_url: url });
                }
                acp::ContentBlock::Audio(_a) => {
                    // Not supported by Codex input yet; skip.
                }
                acp::ContentBlock::Resource(res) => {
                    if let acp::EmbeddedResourceResource::TextResourceContents(trc) = &res.resource
                    {
                        items.push(UserInput::Text {
                            text: trc.text.clone(),
                        });
                    }
                }
                acp::ContentBlock::ResourceLink(link) => {
                    items.push(UserInput::Text {
                        text: format!("Resource: {}", link.uri),
                    });
                }
            }
        }

        let op = match op_opt {
            Some(op) => op,
            None => Op::UserInput { items },
        };

        // Enqueue work and then stream corresponding events back as ACP updates.
        let submit_id = conversation
            .submit(op)
            .await
            .map_err(acp::Error::into_internal_error)?;

        let mut saw_message_delta = false;
        let stop_reason = loop {
            let event = conversation
                .next_event()
                .await
                .map_err(acp::Error::into_internal_error)?;
            if event.id != submit_id {
                continue;
            }

            match event.msg {
                EventMsg::AgentMessageDelta(delta) => {
                    saw_message_delta = true;
                    self.send_message_chunk(&args.session_id, delta.delta.into())
                        .await?;
                }
                EventMsg::AgentMessage(msg) => {
                    if saw_message_delta {
                        continue;
                    }
                    self.send_message_chunk(&args.session_id, msg.message.into())
                        .await?;
                }
                EventMsg::AgentReasoningDelta(delta) => {
                    reason.append_delta(&delta.delta);
                }
                EventMsg::AgentReasoningRawContentDelta(delta) => {
                    reason.append_delta(&delta.delta);
                }
                EventMsg::AgentReasoning(reason_ev) => {
                    reason.section_break();
                    let final_text = if reason_ev.text.trim().is_empty() {
                        None
                    } else {
                        Some(reason_ev.text)
                    };
                    if let Some(text) = reason.choose_final_text(final_text)
                        && !text.trim().is_empty()
                    {
                        self.send_thought_chunk(&args.session_id, text.clone().into())
                            .await?;
                    }
                }
                EventMsg::AgentReasoningRawContent(reason_ev) => {
                    reason.section_break();
                    if !reason_ev.text.trim().is_empty() {
                        reason.append_delta(&reason_ev.text);
                    }
                }
                EventMsg::AgentReasoningSectionBreak(_) => {
                    reason.section_break();
                }
                // MCP tool calls → ACP ToolCall/ToolCallUpdate
                EventMsg::McpToolCallBegin(begin) => {
                    let update =
                        event_handler.on_mcp_tool_call_begin(&begin.call_id, &begin.invocation);
                    self.send_session_update(&args.session_id, update).await?;
                }
                EventMsg::McpToolCallEnd(end) => {
                    let result_json =
                        serde_json::to_value(&end.result).unwrap_or(serde_json::json!(null));
                    let update = event_handler.on_mcp_tool_call_end(
                        &end.call_id,
                        &end.invocation,
                        &result_json,
                        end.is_success(),
                    );
                    self.send_session_update(&args.session_id, update).await?;
                }
                // Exec command begin/end → ACP ToolCall/ToolCallUpdate
                EventMsg::ExecCommandBegin(beg) => {
                    let update = event_handler.on_exec_command_begin(
                        &beg.call_id,
                        &beg.cwd,
                        &beg.command,
                        &beg.parsed_cmd,
                    );
                    self.send_session_update(&args.session_id, update).await?;
                }
                EventMsg::ExecCommandEnd(end) => {
                    let exec_end_args = events::ExecEndArgs {
                        call_id: end.call_id.clone(),
                        exit_code: end.exit_code,
                        aggregated_output: end.aggregated_output.clone(),
                        stdout: end.stdout.clone(),
                        stderr: end.stderr.clone(),
                        duration_ms: end.duration.as_millis(),
                        formatted_output: end.formatted_output.clone(),
                    };
                    let update = event_handler.on_exec_command_end(exec_end_args);
                    self.send_session_update(&args.session_id, update).await?;
                }
                EventMsg::ExecApprovalRequest(req) => {
                    let permission_req = event_handler.on_exec_approval_request(
                        &args.session_id,
                        &req.call_id,
                        &req.cwd,
                        &req.parsed_cmd,
                    );

                    let (txp, rxp) = oneshot::channel();
                    let _ = self.client_tx.send(ClientOp::RequestPermission {
                        session_id: args.session_id.clone(),
                        request: permission_req,
                        response_tx: txp,
                    });
                    let outcome: Result<acp::RequestPermissionResponse, acp::Error> =
                        rxp.await.map_err(|_| acp::Error::internal_error())?;
                    if let Ok(resp) = outcome {
                        let decision = events::handle_response_outcome(resp);
                        // Send ExecApproval back to Codex; refer to current event.id
                        conversation
                            .submit(Op::ExecApproval {
                                id: event.id.clone(),
                                decision,
                            })
                            .await
                            .map_err(acp::Error::into_internal_error)?;
                    }
                }
                EventMsg::ApplyPatchApprovalRequest(req) => {
                    // Convert changes to the type expected by EventHandler
                    let changes: Vec<(String, _)> = req
                        .changes
                        .iter()
                        .map(|(p, c)| (p.display().to_string(), c.clone()))
                        .collect();

                    let permission_req = event_handler.on_apply_patch_approval_request(
                        &args.session_id,
                        &req.call_id,
                        &changes,
                    );
                    let (txp, rxp) = oneshot::channel();
                    let _ = self.client_tx.send(ClientOp::RequestPermission {
                        session_id: args.session_id.clone(),
                        request: permission_req,
                        response_tx: txp,
                    });
                    let outcome: Result<acp::RequestPermissionResponse, acp::Error> =
                        rxp.await.map_err(acp::Error::into_internal_error)?;
                    if let Ok(resp) = outcome {
                        let decision = events::handle_response_outcome(resp);
                        conversation
                            .submit(Op::PatchApproval {
                                id: event.id.clone(),
                                decision,
                            })
                            .await
                            .map_err(acp::Error::into_internal_error)?;
                    }
                }
                EventMsg::PatchApplyEnd(event) => {
                    let raw_output = serde_json::json!(&event);
                    let PatchApplyEndEvent {
                        call_id,
                        stdout: _,
                        stderr: _,
                        success,
                    } = event;

                    let update = event_handler.on_patch_apply_end(&call_id, success, raw_output);

                    self.send_session_update(&args.session_id, update).await?;
                }
                EventMsg::TokenCount(tc) => {
                    if let Some(info) = tc.info {
                        self.with_session_state_mut(&args.session_id, |state| {
                            state.token_usage = Some(info.total_token_usage.clone());
                        });
                    }
                }
                EventMsg::PlanUpdate(UpdatePlanArgs { explanation, plan }) => {
                    if let Some(content) = explanation {
                        self.send_message_chunk(&args.session_id, content.into())
                            .await?;
                    }

                    let entries = plan
                        .iter()
                        .map(|item| {
                            let status = match item.status {
                                StepStatus::Pending => acp::PlanEntryStatus::Pending,
                                StepStatus::InProgress => acp::PlanEntryStatus::InProgress,
                                StepStatus::Completed => acp::PlanEntryStatus::Completed,
                            };

                            acp::PlanEntry {
                                content: item.step.clone(),
                                priority: acp::PlanEntryPriority::Medium,
                                status,
                                meta: None,
                            }
                        })
                        .collect();

                    self.send_session_update(
                        &args.session_id,
                        acp::SessionUpdate::Plan(acp::Plan {
                            entries,
                            meta: None,
                        }),
                    )
                    .await?;
                }
                EventMsg::TaskComplete(_) => {
                    break acp::StopReason::EndTurn;
                }
                EventMsg::Error(ErrorEvent { message })
                | EventMsg::StreamError(StreamErrorEvent { message }) => {
                    let mut msg = String::from(&message);
                    msg.push_str("\n\n");
                    self.send_message_chunk(&args.session_id, msg.into())
                        .await?;
                }
                EventMsg::ShutdownComplete | EventMsg::TurnAborted(_) => {
                    break acp::StopReason::Cancelled;
                }
                // Ignore other events for now.
                _ => {}
            }
        };

        if let Some(text) = reason.take_text()
            && !text.trim().is_empty()
        {
            self.send_thought_chunk(&args.session_id, text.into())
                .await?;
        }

        Ok(acp::PromptResponse {
            stop_reason,
            meta: None,
        })
    }

    /// Cancel an ongoing prompt operation.
    pub(super) async fn cancel(&self, args: acp::CancelNotification) -> Result<(), acp::Error> {
        info!(?args, "Received cancel request");
        self.get_conversation(&args.session_id)
            .await?
            .submit(Op::Interrupt)
            .await
            .map_err(|e| acp::Error::from(anyhow::anyhow!("failed to send interrupt: {}", e)))?;
        Ok(())
    }

    /// Handle extension method calls.
    ///
    /// This is a placeholder for future extensions.
    pub(super) async fn ext_method(
        &self,
        args: acp::ExtRequest,
    ) -> Result<acp::ExtResponse, acp::Error> {
        info!(method = %args.method, params = ?args.params, "Received extension method call");
        Ok(serde_json::value::to_raw_value(&json!({"example": "response"}))?.into())
    }

    /// Handle extension notifications.
    ///
    /// This is a placeholder for future extensions.
    pub(super) async fn ext_notification(
        &self,
        args: acp::ExtNotification,
    ) -> Result<(), acp::Error> {
        info!(method = %args.method, params = ?args.params, "Received extension notification call");
        Ok(())
    }
}
