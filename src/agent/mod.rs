use std::{
    cell::RefCell,
    collections::HashMap,
    env,
    rc::Rc,
    sync::{Arc, RwLock},
    time::Duration,
};

use crate::agent::session::SessionState;
use crate::fs::FsBridge;
use agent_client_protocol::{self as acp, Agent, Error, Implementation, McpServer, V1};
use codex_app_server_protocol::AuthMode;
use codex_core::{
    AuthManager, CodexConversation, ConversationManager, NewConversation,
    config::Config as CodexConfig,
    config_types::{McpServerConfig, McpServerTransportConfig},
    protocol::{
        ErrorEvent, EventMsg, FileChange, Op, PatchApplyEndEvent, SessionSource, StreamErrorEvent,
    },
};
use codex_protocol::{
    ConversationId,
    plan_tool::{StepStatus, UpdatePlanArgs},
    user_input::UserInput,
};
use serde_json::json;
use tokio::{
    sync::{mpsc, oneshot, oneshot::Sender},
    task,
};
use tracing::{info, warn};
use uuid::Uuid;

mod commands;
mod events;
mod modes;
mod session;
mod utils;

pub use session::SessionModeLookup;

impl From<&CodexAgent> for SessionModeLookup {
    fn from(agent: &CodexAgent) -> Self {
        Self {
            inner: agent.sessions.clone(),
        }
    }
}

pub struct CodexAgent {
    session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, Sender<()>)>,
    sessions: Rc<RefCell<HashMap<String, SessionState>>>,
    config: CodexConfig,
    conversation_manager: ConversationManager,
    auth_manager: Arc<RwLock<Arc<AuthManager>>>,
    client_tx: mpsc::UnboundedSender<ClientOp>,
    client_capabilities: RefCell<acp::ClientCapabilities>,
    fs_bridge: Option<Arc<FsBridge>>,
}

impl CodexAgent {
    pub fn with_config(
        session_update_tx: mpsc::UnboundedSender<(acp::SessionNotification, Sender<()>)>,
        client_tx: mpsc::UnboundedSender<ClientOp>,
        config: CodexConfig,
        fs_bridge: Option<Arc<FsBridge>>,
    ) -> Self {
        let auth = AuthManager::shared(config.codex_home.clone(), false);
        let conversation_manager = ConversationManager::new(auth.clone(), SessionSource::Unknown);

        Self {
            session_update_tx,
            sessions: Rc::new(RefCell::new(HashMap::new())),
            config,
            conversation_manager,
            auth_manager: Arc::new(RwLock::new(auth)),
            client_tx,
            client_capabilities: RefCell::new(Default::default()),
            fs_bridge,
        }
    }

    fn prepare_fs_mcp_server_config(
        &self,
        session_id: &str,
        bridge: &FsBridge,
    ) -> Result<McpServerConfig, Error> {
        let exe_path = env::current_exe().map_err(|err| {
            Error::internal_error().with_data(format!("failed to locate agent binary: {err}"))
        })?;

        let mut env = HashMap::new();
        env.insert(
            "ACP_FS_BRIDGE_ADDR".to_string(),
            bridge.address().to_string(),
        );
        env.insert("ACP_FS_SESSION_ID".to_string(), session_id.to_string());

        Ok(McpServerConfig {
            transport: McpServerTransportConfig::Stdio {
                command: exe_path.to_string_lossy().into_owned(),
                args: vec!["--acp-fs-mcp".to_string()],
                env: Some(env),
                env_vars: vec![],
                cwd: None,
            },
            enabled: true,
            startup_timeout_sec: Some(Duration::from_secs(5)),
            tool_timeout_sec: Some(Duration::from_secs(30)),
            enabled_tools: None,
            disabled_tools: {
                let caps = self.client_capabilities.borrow();
                let mut v: Vec<String> = Vec::new();
                if !caps.fs.read_text_file {
                    v.push("read_text_file".to_string());
                }
                if !caps.fs.write_text_file {
                    v.push("write_text_file".to_string());
                    v.push("edit_text_file".to_string());
                    v.push("multi_edit_text_file".to_string());
                }
                if v.is_empty() { None } else { Some(v) }
            },
        })
    }

    fn build_streamable_http_server(
        name: String,
        url: String,
        headers: Vec<acp::HttpHeader>,
        startup_timeout: Option<Duration>,
        tool_timeout: Option<Duration>,
    ) -> (String, McpServerConfig) {
        let http_headers = headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect::<HashMap<_, _>>();
        (
            name,
            McpServerConfig {
                transport: McpServerTransportConfig::StreamableHttp {
                    url,
                    http_headers: Some(http_headers),
                    bearer_token_env_var: None,
                    env_http_headers: None,
                },
                enabled: true,
                startup_timeout_sec: startup_timeout,
                tool_timeout_sec: tool_timeout,
                enabled_tools: None,
                disabled_tools: None,
            },
        )
    }

    fn build_mcp_server(
        &self,
        server: McpServer,
        startup_timeout: Option<Duration>,
        tool_timeout: Option<Duration>,
    ) -> Option<(String, McpServerConfig)> {
        match server {
            McpServer::Http { name, url, headers } | McpServer::Sse { name, url, headers } => {
                Some(Self::build_streamable_http_server(
                    name,
                    url.to_string(),
                    headers,
                    startup_timeout,
                    tool_timeout,
                ))
            }
            McpServer::Stdio {
                name,
                command,
                args,
                env,
            } => {
                let env = if env.is_empty() {
                    None
                } else {
                    Some(
                        env.into_iter()
                            .map(|var| (var.name, var.value))
                            .collect::<HashMap<_, _>>(),
                    )
                };
                Some((
                    name,
                    McpServerConfig {
                        transport: McpServerTransportConfig::Stdio {
                            command: command.to_string_lossy().into_owned(),
                            args,
                            env,
                            env_vars: vec![],
                            cwd: None,
                        },
                        enabled: true,
                        startup_timeout_sec: startup_timeout,
                        tool_timeout_sec: tool_timeout,
                        enabled_tools: None,
                        disabled_tools: None,
                    },
                ))
            }
        }
    }

    fn build_session_config(
        &self,
        session_id: &str,
        mcp_servers: Vec<McpServer>,
    ) -> Result<CodexConfig, Error> {
        let mut session_config = self.config.clone();
        let fs_guidance = include_str!("prompt_fs_guidance.md");

        if let Some(mut base) = session_config.base_instructions.take() {
            if !base.contains("acp_fs") {
                if !base.trim_end().is_empty() {
                    base.push_str("\n\n");
                }
                base.push_str(fs_guidance);
            }
            session_config.base_instructions = Some(base);
        } else {
            session_config.user_instructions = match session_config.user_instructions.take() {
                Some(mut existing) => {
                    if !existing.contains("acp_fs") {
                        if !existing.trim_end().is_empty() {
                            existing.push_str("\n\n");
                        }
                        existing.push_str(fs_guidance);
                    }
                    Some(existing)
                }
                None => Some(fs_guidance.to_string()),
            };
        }

        let startup_timeout = Some(Duration::from_secs(5));
        let tool_timeout = Some(Duration::from_secs(30));

        session_config.mcp_servers.extend(
            mcp_servers
                .into_iter()
                .filter_map(|srv| self.build_mcp_server(srv, startup_timeout, tool_timeout)),
        );

        if let Some(bridge) = &self.fs_bridge {
            let server_config = self.prepare_fs_mcp_server_config(session_id, bridge.as_ref())?;
            session_config
                .mcp_servers
                .insert("acp_fs".to_string(), server_config);
        }

        Ok(session_config)
    }

    fn modes(config: &CodexConfig) -> Option<acp::SessionModeState> {
        modes::session_modes_for_config(config)
    }

    async fn get_conversation(
        &self,
        session_id: &acp::SessionId,
    ) -> Result<Arc<CodexConversation>, Error> {
        let conversation_opt = {
            let sessions = self.sessions.borrow();
            let state = sessions
                .get(session_id.0.as_ref())
                .ok_or_else(|| Error::invalid_params().with_data("session not found"))?;
            state.conversation.clone()
        };

        if let Some(conversation) = conversation_opt {
            return Ok(conversation);
        }

        let conversation_id = ConversationId::from_string(session_id.0.as_ref())
            .map_err(|e| Error::from(anyhow::anyhow!(e)))?;

        let conversation = self
            .conversation_manager
            .get_conversation(conversation_id)
            .await
            .map_err(|e| Error::from(anyhow::anyhow!(e)))?;

        self.with_session_state_mut(session_id, |state| {
            state.conversation = Some(conversation.clone());
        });
        Ok(conversation)
    }

    pub async fn send_session_update(
        &self,
        session_id: &acp::SessionId,
        update: acp::SessionUpdate,
    ) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        let notification = acp::SessionNotification {
            session_id: session_id.clone(),
            update,
            meta: None,
        };
        self.session_update_tx
            .send((notification, tx))
            .map_err(Error::into_internal_error)?;
        rx.await.map_err(Error::into_internal_error)
    }

    pub async fn send_message_chunk(
        &self,
        session_id: &acp::SessionId,
        content: acp::ContentBlock,
    ) -> Result<(), Error> {
        let chunk = acp::SessionUpdate::AgentMessageChunk(acp::ContentChunk {
            content,
            meta: None,
        });
        self.send_session_update(session_id, chunk).await
    }

    pub async fn send_thought_chunk(
        &self,
        session_id: &acp::SessionId,
        content: acp::ContentBlock,
    ) -> Result<(), Error> {
        let chunk = acp::SessionUpdate::AgentThoughtChunk(acp::ContentChunk {
            content,
            meta: None,
        });
        self.send_session_update(session_id, chunk).await
    }

    fn with_session_state_mut<R, F>(&self, session_id: &acp::SessionId, f: F) -> Option<R>
    where
        F: FnOnce(&mut SessionState) -> R,
    {
        let mut sessions = self.sessions.borrow_mut();
        let key: &str = session_id.0.as_ref();
        sessions.get_mut(key).map(f)
    }

    fn support_terminal(&self) -> bool {
        self.client_capabilities.borrow().terminal
    }
}

#[derive(Debug)]
pub enum ClientOp {
    RequestPermission(
        acp::RequestPermissionRequest,
        Sender<Result<acp::RequestPermissionResponse, Error>>,
    ),
    ReadTextFile(
        acp::ReadTextFileRequest,
        Sender<Result<acp::ReadTextFileResponse, Error>>,
    ),
    WriteTextFile(
        acp::WriteTextFileRequest,
        Sender<Result<acp::WriteTextFileResponse, Error>>,
    ),
}

#[async_trait::async_trait(?Send)]
impl Agent for CodexAgent {
    async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, Error> {
        info!(?args, "Received initialize request");
        // Advertise supported auth methods. We surface both ChatGPT and API key.
        let auth_methods = vec![
            acp::AuthMethod {
                id: acp::AuthMethodId("chatgpt".into()),
                name: "ChatGPT".into(),
                description: Some("Sign in with ChatGPT to use your plan".into()),
                meta: None,
            },
            acp::AuthMethod {
                id: acp::AuthMethodId("apikey".into()),
                name: "OpenAI API Key".into(),
                description: Some("Use OPENAI_API_KEY from environment or auth.json".into()),
                meta: None,
            },
        ];

        self.client_capabilities.replace(args.client_capabilities);
        let agent_capabilities = acp::AgentCapabilities {
            load_session: false,
            prompt_capabilities: acp::PromptCapabilities {
                image: true,
                audio: false,
                embedded_context: true,
                meta: None,
            },
            mcp_capabilities: acp::McpCapabilities {
                http: true,
                sse: true,
                meta: None,
            },
            meta: None,
        };

        Ok(acp::InitializeResponse {
            protocol_version: V1,
            agent_capabilities,
            auth_methods,
            agent_info: Some(Implementation {
                name: "codex-acp".into(),
                title: Some("Codex ACP".into()),
                version: env!("CARGO_PKG_VERSION").into(),
            }),
            meta: None,
        })
    }

    async fn authenticate(
        &self,
        args: acp::AuthenticateRequest,
    ) -> Result<acp::AuthenticateResponse, Error> {
        info!(?args, "Received authenticate request");
        let method = args.method_id.0.as_ref();
        match method {
            "apikey" => {
                if let Ok(am) = self.auth_manager.write() {
                    // Persisting the API key is handled by Codex core when reloading;
                    // here we simply reload and check.
                    am.reload();
                    if am.auth().is_some() {
                        return Ok(Default::default());
                    }
                }
                Err(Error::auth_required().with_data("Failed to load API key auth"))
            }
            "chatgpt" => {
                if let Ok(am) = self.auth_manager.write() {
                    am.reload();
                    if let Some(auth) = am.auth()
                        && auth.mode == AuthMode::ChatGPT
                    {
                        return Ok(Default::default());
                    }
                }
                Err(Error::auth_required()
                    .with_data("ChatGPT login not found. Run `codex login` to connect your plan."))
            }
            other => {
                Err(Error::invalid_params().with_data(format!("unknown auth method: {}", other)))
            }
        }
    }

    async fn new_session(
        &self,
        args: acp::NewSessionRequest,
    ) -> Result<acp::NewSessionResponse, Error> {
        info!(?args, "Received new session request");
        let fs_session_id = Uuid::new_v4().to_string();

        let modes = Self::modes(&self.config);
        let current_mode = modes
            .as_ref()
            .map(|m| m.current_mode_id.clone())
            .unwrap_or(acp::SessionModeId("auto".into()));

        let session_config = self.build_session_config(&fs_session_id, args.mcp_servers)?;

        let new_conv = self
            .conversation_manager
            .new_conversation(session_config)
            .await;

        let (conversation, conversation_id) = match new_conv {
            Ok(NewConversation {
                conversation,
                conversation_id,
                ..
            }) => (conversation, conversation_id),
            Err(e) => {
                warn!(error = %e, "Failed to create Codex conversation");
                return Err(Error::into_internal_error(e));
            }
        };

        let acp_session_id = conversation_id.to_string();

        self.sessions.borrow_mut().insert(
            acp_session_id.clone(),
            SessionState {
                fs_session_id: fs_session_id.clone(),
                conversation: Some(conversation.clone()),
                current_approval: self.config.approval_policy,
                current_sandbox: self.config.sandbox_policy.clone(),
                current_mode: current_mode.clone(),
                token_usage: None,
            },
        );

        // Advertise available slash commands to the client right after
        // the session is created. Send it asynchronously to avoid racing
        // with the NewSessionResponse delivery.
        {
            let session_id = acp_session_id.clone();
            let available_commands = commands::AVAILABLE_COMMANDS.to_vec();
            let tx_updates = self.session_update_tx.clone();
            task::spawn_local(async move {
                let (tx, rx) = oneshot::channel();
                let _ = tx_updates.send((
                    acp::SessionNotification {
                        session_id: acp::SessionId(session_id.clone().into()),
                        update: acp::SessionUpdate::AvailableCommandsUpdate(
                            acp::AvailableCommandsUpdate {
                                available_commands,
                                meta: None,
                            },
                        ),
                        meta: None,
                    },
                    tx,
                ));
                let _ = rx.await;
            });
        }

        Ok(acp::NewSessionResponse {
            session_id: acp::SessionId(acp_session_id.clone().into()),
            modes,
            meta: None,
        })
    }

    async fn load_session(
        &self,
        args: acp::LoadSessionRequest,
    ) -> Result<acp::LoadSessionResponse, Error> {
        info!(?args, "Received load session request");
        let current_mode = {
            let sessions = self.sessions.borrow();
            let state = sessions
                .get(args.session_id.0.as_ref())
                .ok_or_else(|| Error::invalid_params().with_data("session not found"))?;
            state.current_mode.clone()
        };

        Ok(acp::LoadSessionResponse {
            modes: Some(acp::SessionModeState {
                current_mode_id: current_mode,
                available_modes: modes::available_modes(),
                meta: None,
            }),
            meta: None,
        })
    }

    async fn set_session_mode(
        &self,
        args: acp::SetSessionModeRequest,
    ) -> Result<acp::SetSessionModeResponse, Error> {
        info!(?args, "Received set session mode request");
        let preset = modes::find_preset_by_mode_id(&args.mode_id)
            .ok_or_else(|| Error::invalid_params().with_data("invalid mode id"))?;

        self.get_conversation(&args.session_id)
            .await?
            .submit(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: Some(preset.approval),
                sandbox_policy: Some(preset.sandbox.clone()),
                model: None,
                effort: None,
                summary: None,
            })
            .await
            .map_err(|e| Error::from(anyhow::anyhow!(e)))?;

        self.with_session_state_mut(&args.session_id, |state| {
            state.current_approval = preset.approval;
            state.current_sandbox = preset.sandbox.clone();
            state.current_mode = args.mode_id.clone();
        });

        Ok(acp::SetSessionModeResponse::default())
    }

    async fn prompt(&self, args: acp::PromptRequest) -> Result<acp::PromptResponse, Error> {
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
            .map_err(Error::into_internal_error)?;

        let mut saw_message_delta = false;
        let stop_reason = loop {
            let event = conversation
                .next_event()
                .await
                .map_err(Error::into_internal_error)?;
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
                    let _ = self
                        .client_tx
                        .send(ClientOp::RequestPermission(permission_req, txp));
                    let outcome = rxp.await.map_err(|_| Error::internal_error())?;
                    if let Ok(resp) = outcome {
                        let decision = events::handle_response_outcome(resp);
                        // Send ExecApproval back to Codex; refer to current event.id
                        conversation
                            .submit(Op::ExecApproval {
                                id: event.id.clone(),
                                decision,
                            })
                            .await
                            .map_err(Error::into_internal_error)?;
                    }
                }
                EventMsg::ApplyPatchApprovalRequest(req) => {
                    // Convert changes to the type expected by EventHandler
                    let changes: Vec<(String, FileChange)> = req
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
                    let _ = self
                        .client_tx
                        .send(ClientOp::RequestPermission(permission_req, txp));
                    if let Ok(resp) = rxp.await.map_err(Error::into_internal_error)? {
                        let decision = events::handle_response_outcome(resp);
                        conversation
                            .submit(Op::PatchApproval {
                                id: event.id.clone(),
                                decision,
                            })
                            .await
                            .map_err(Error::into_internal_error)?;
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

    async fn cancel(&self, args: acp::CancelNotification) -> Result<(), Error> {
        info!(?args, "Received cancel request");
        self.get_conversation(&args.session_id)
            .await?
            .submit(Op::Interrupt)
            .await
            .map_err(|e| Error::from(anyhow::anyhow!("failed to send interrupt: {}", e)))?;
        Ok(())
    }

    async fn ext_method(&self, args: acp::ExtRequest) -> Result<acp::ExtResponse, Error> {
        info!(method = %args.method, params = ?args.params, "Received extension method call");
        Ok(serde_json::value::to_raw_value(&json!({"example": "response"}))?.into())
    }

    async fn ext_notification(&self, args: acp::ExtNotification) -> Result<(), Error> {
        info!(method = %args.method, params = ?args.params, "Received extension notification call");
        Ok(())
    }
}
