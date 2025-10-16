use std::cell::RefCell;
use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, LazyLock, RwLock};
use std::time::Duration;

use agent_client_protocol::{self as acp, Agent, Error, McpServer, PlanEntry, V1};
use codex_app_server_protocol::AuthMode;
use codex_common::approval_presets::{ApprovalPreset, builtin_approval_presets};
use codex_core::{
    AuthManager, CodexConversation, ConversationManager, NewConversation,
    config::Config as CodexConfig,
    config_types::{McpServerConfig, McpServerTransportConfig},
    protocol::{
        AskForApproval, ErrorEvent, EventMsg, InputItem, McpInvocation, Op, PatchApplyEndEvent,
        ReviewDecision, SandboxPolicy, SessionSource, StreamErrorEvent, TokenUsage,
    },
};
use codex_protocol::{
    ConversationId,
    parse_command::ParsedCommand,
    plan_tool::{StepStatus, UpdatePlanArgs},
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot, oneshot::Sender};
use tokio::task;
use tracing::{info, warn};
use uuid::Uuid;

use crate::fs::FsBridge;

mod commands;

pub static APPROVAL_PRESETS: LazyLock<Vec<ApprovalPreset>> =
    LazyLock::new(builtin_approval_presets);

// Placeholder for per-session state. Holds the Codex conversation
// handle, its id (for status/reporting), and bookkeeping for streaming.
#[derive(Clone)]
struct SessionState {
    fs_session_id: String,
    conversation: Option<Arc<CodexConversation>>,
    current_approval: AskForApproval,
    current_sandbox: SandboxPolicy,
    current_mode: acp::SessionModeId,
    token_usage: Option<TokenUsage>,
    reasoning_sections: Vec<String>,
    current_reasoning_chunk: String,
}

#[derive(Clone)]
pub struct SessionModeLookup {
    inner: Rc<RefCell<HashMap<String, SessionState>>>,
}

impl From<&CodexAgent> for SessionModeLookup {
    fn from(agent: &CodexAgent) -> Self {
        Self {
            inner: agent.sessions.clone(),
        }
    }
}

impl SessionModeLookup {
    pub fn current_mode(&self, session_id: &acp::SessionId) -> Option<acp::SessionModeId> {
        let sessions = self.inner.borrow();
        if let Some(state) = sessions.get(session_id.0.as_ref()) {
            return Some(state.current_mode.clone());
        }

        sessions
            .values()
            .find(|state| state.fs_session_id == session_id.0.as_ref())
            .map(|state| state.current_mode.clone())
    }

    pub fn is_read_only(&self, session_id: &acp::SessionId) -> bool {
        self.current_mode(session_id)
            .map(|mode| mode.0.as_ref() == "read-only")
            .unwrap_or(false)
    }

    pub fn resolve_acp_session_id(&self, session_id: &acp::SessionId) -> Option<acp::SessionId> {
        let sessions = self.inner.borrow();
        if sessions.contains_key(session_id.0.as_ref()) {
            return Some(session_id.clone());
        }

        sessions.iter().find_map(|(key, state)| {
            if state.fs_session_id == session_id.0.as_ref() {
                Some(acp::SessionId(key.clone().into()))
            } else {
                None
            }
        })
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
        })
    }

    fn extract_bearer_token(headers: &[acp::HttpHeader]) -> Option<String> {
        headers.iter().find_map(|header| {
            header
                .name
                .eq_ignore_ascii_case("Authorization")
                .then(|| {
                    header
                        .value
                        .trim()
                        .strip_prefix("Bearer ")
                        .map(|token| token.trim().to_owned())
                })
                .flatten()
        })
    }

    fn build_streamable_http_server(
        name: String,
        url: String,
        headers: Vec<acp::HttpHeader>,
        startup_timeout: Option<Duration>,
        tool_timeout: Option<Duration>,
    ) -> (String, McpServerConfig) {
        let bearer_token = Self::extract_bearer_token(&headers);
        let http_headers = headers
            .iter()
            .map(|header| (header.name.clone(), header.value.clone()))
            .collect::<HashMap<_, _>>();
        (
            name,
            McpServerConfig {
                transport: McpServerTransportConfig::StreamableHttp {
                    url,
                    bearer_token_env_var: bearer_token,
                    http_headers: Some(http_headers),
                    env_http_headers: None,
                },
                enabled: true,
                startup_timeout_sec: startup_timeout,
                tool_timeout_sec: tool_timeout,
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
        let fs_guidance = include_str!("./prompt_fs_guidance.md");

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
        let current_mode_id = APPROVAL_PRESETS
            .iter()
            .find(|preset| {
                preset.approval == config.approval_policy && preset.sandbox == config.sandbox_policy
            })
            .map(|preset| acp::SessionModeId(preset.id.into()))?;

        Some(acp::SessionModeState {
            current_mode_id,
            available_modes: APPROVAL_PRESETS
                .iter()
                .map(|preset| acp::SessionMode {
                    id: acp::SessionModeId(preset.id.into()),
                    name: preset.label.to_owned(),
                    description: Some(preset.description.to_owned()),
                    meta: None,
                })
                .collect(),
            meta: None,
        })
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
        self.session_update_tx
            .send((
                acp::SessionNotification {
                    session_id: session_id.clone(),
                    update,
                    meta: None,
                },
                tx,
            ))
            .map_err(Error::into_internal_error)?;
        rx.await.map_err(Error::into_internal_error)
    }

    pub async fn send_message_chunk(
        &self,
        session_id: &acp::SessionId,
        content: acp::ContentBlock,
    ) -> Result<(), Error> {
        self.send_session_update(
            session_id,
            acp::SessionUpdate::AgentMessageChunk { content },
        )
        .await
    }

    pub async fn send_thought_chunk(
        &self,
        session_id: &acp::SessionId,
        content: acp::ContentBlock,
    ) -> Result<(), Error> {
        self.send_session_update(
            session_id,
            acp::SessionUpdate::AgentThoughtChunk { content },
        )
        .await
    }

    fn handle_response_outcome(&self, resp: acp::RequestPermissionResponse) -> ReviewDecision {
        match resp.outcome {
            acp::RequestPermissionOutcome::Selected { option_id } => match option_id.0.as_ref() {
                "approved" => ReviewDecision::Approved,
                "approved-for-session" => ReviewDecision::ApprovedForSession,
                _ => ReviewDecision::Abort,
            },
            acp::RequestPermissionOutcome::Cancelled => ReviewDecision::Abort,
        }
    }

    fn with_session_state_mut<R, F>(&self, session_id: &acp::SessionId, f: F) -> Option<R>
    where
        F: FnOnce(&mut SessionState) -> R,
    {
        let mut sessions = self.sessions.borrow_mut();
        let key: &str = session_id.0.as_ref();
        sessions.get_mut(key).map(f)
    }

    fn reset_reasoning_tracking(&self, session_id: &acp::SessionId) {
        self.with_session_state_mut(session_id, |state| {
            state.reasoning_sections.clear();
            state.current_reasoning_chunk.clear();
        });
    }

    fn append_reasoning_delta(&self, session_id: &acp::SessionId, delta: &str) {
        self.with_session_state_mut(session_id, |state| {
            state.current_reasoning_chunk.push_str(delta);
        });
    }

    fn finish_current_reasoning_section(&self, session_id: &acp::SessionId) {
        self.with_session_state_mut(session_id, |state| {
            if !state.current_reasoning_chunk.is_empty() {
                let chunk = std::mem::take(&mut state.current_reasoning_chunk);
                state.reasoning_sections.push(chunk);
            }
        });
    }

    fn take_reasoning_text(&self, session_id: &acp::SessionId) -> Option<String> {
        self.with_session_state_mut(session_id, |state| {
            let mut combined = String::new();
            let mut first = true;

            for section in state.reasoning_sections.drain(..) {
                if section.trim().is_empty() {
                    continue;
                }
                if !first {
                    combined.push_str("\n\n");
                }
                combined.push_str(section.trim_end());
                first = false;
            }

            if !state.current_reasoning_chunk.trim().is_empty() {
                if !first {
                    combined.push_str("\n\n");
                }
                combined.push_str(state.current_reasoning_chunk.trim_end());
            }

            state.current_reasoning_chunk.clear();

            if combined.is_empty() {
                None
            } else {
                Some(combined)
            }
        })
        .flatten()
    }

    fn describe_mcp_tool(
        &self,
        invocation: &McpInvocation,
    ) -> (String, Vec<acp::ToolCallLocation>) {
        if let Some(metadata) = self.fs_tool_metadata(invocation) {
            let FsToolMetadata {
                display_path,
                location_path,
                line,
            } = metadata;
            let location = acp::ToolCallLocation {
                path: location_path,
                line,
                meta: None,
            };
            (
                format!("{}.{} ({display_path})", invocation.server, invocation.tool),
                vec![location],
            )
        } else {
            (
                format!("{}.{}", invocation.server, invocation.tool),
                Vec::new(),
            )
        }
    }

    fn fs_tool_metadata(&self, invocation: &McpInvocation) -> Option<FsToolMetadata> {
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
        let display_path = self.display_fs_path(&path);
        let location_path = PathBuf::from(&path);

        Some(FsToolMetadata {
            display_path,
            location_path,
            line,
        })
    }

    fn display_fs_path(&self, raw_path: &str) -> String {
        let path = Path::new(raw_path);
        if let Ok(relative) = path.strip_prefix(&self.config.cwd) {
            let rel_display = relative.display().to_string();
            if !rel_display.is_empty() {
                return rel_display;
            }
        }

        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| raw_path.to_string())
    }

    fn format_command_call(cwd: &Path, parsed_cmd: &[ParsedCommand]) -> FormatCommandCall {
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
                        &cwd.join(path)
                    } else {
                        cwd
                    };
                    titles.push(format!("List {}", dir.display()));
                    cmd_path = path.as_ref().map(PathBuf::from);
                    kind = acp::ToolKind::Search;
                }
                ParsedCommand::Search { cmd, query, path } => {
                    titles.push(match (query, path.as_ref()) {
                        (Some(query), Some(path)) => format!("Search {query} in {path}"),
                        (Some(query), None) => format!("Search {query}"),
                        _ => format!("Search {}", cmd),
                    });
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

    fn support_terminal(&self) -> bool {
        self.client_capabilities.borrow().terminal
    }
}

struct FormatCommandCall {
    title: String,
    terminal_output: bool,
    locations: Vec<acp::ToolCallLocation>,
    kind: acp::ToolKind,
}

struct FsToolMetadata {
    display_path: String,
    location_path: PathBuf,
    line: Option<u32>,
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
        let capacities = acp::AgentCapabilities {
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
            agent_capabilities: capacities,
            auth_methods,
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
                reasoning_sections: Vec::new(),
                current_reasoning_chunk: String::new(),
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
                        update: acp::SessionUpdate::AvailableCommandsUpdate { available_commands },
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
                available_modes: APPROVAL_PRESETS
                    .iter()
                    .map(|preset| acp::SessionMode {
                        id: acp::SessionModeId(preset.id.into()),
                        name: preset.label.to_owned(),
                        description: Some(preset.description.to_owned()),
                        meta: None,
                    })
                    .collect(),
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
        let preset = APPROVAL_PRESETS
            .iter()
            .find(|preset| args.mode_id.0.as_ref() == preset.id)
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
        let conversation = self.get_conversation(&args.session_id).await?;

        let mut op_opt = None;
        // Handle slash commands (e.g., "/status") when the first block is text starting with '/'
        if let Some(acp::ContentBlock::Text(t)) = args.prompt.first() {
            let line = t.text.trim();
            if let Some(cmd) = line.strip_prefix('/') {
                let mut parts = cmd.split_whitespace();
                let name = parts.next().unwrap_or("").to_lowercase();
                let rest = parts.collect::<Vec<_>>().join(" ");
                if self
                    .handle_slash_command(&args.session_id, &name, &rest)
                    .await?
                {
                    return Ok(acp::PromptResponse {
                        stop_reason: acp::StopReason::EndTurn,
                        meta: None,
                    });
                }

                op_opt = self
                    .handle_background_task_command(&args.session_id, &name)
                    .await;
            }
        }

        self.reset_reasoning_tracking(&args.session_id);

        // Build user input submission items from prompt content blocks.
        let mut items: Vec<InputItem> = Vec::new();
        for block in &args.prompt {
            match block {
                acp::ContentBlock::Text(t) => {
                    items.push(InputItem::Text {
                        text: t.text.clone(),
                    });
                }
                acp::ContentBlock::Image(img) => {
                    let url = format!("data:{};base64,{}", img.mime_type, img.data);
                    items.push(InputItem::Image { image_url: url });
                }
                acp::ContentBlock::Audio(_a) => {
                    // Not supported by Codex input yet; skip.
                }
                acp::ContentBlock::Resource(res) => {
                    if let acp::EmbeddedResourceResource::TextResourceContents(trc) = &res.resource
                    {
                        items.push(InputItem::Text {
                            text: trc.text.clone(),
                        });
                    }
                }
                acp::ContentBlock::ResourceLink(link) => {
                    items.push(InputItem::Text {
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

        let permission_opts = Arc::new(vec![
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
        ]);

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
                    self.append_reasoning_delta(&args.session_id, &delta.delta);
                }
                EventMsg::AgentReasoningRawContentDelta(delta) => {
                    self.append_reasoning_delta(&args.session_id, &delta.delta);
                }
                EventMsg::AgentReasoning(reason) => {
                    self.finish_current_reasoning_section(&args.session_id);
                    let aggregated = self.take_reasoning_text(&args.session_id);
                    let normalized_final = if reason.text.trim().is_empty() {
                        None
                    } else {
                        Some(reason.text)
                    };

                    let content = match (aggregated, normalized_final) {
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
                    };
                    if let Some(text) = content
                        && !text.trim().is_empty()
                    {
                        self.send_thought_chunk(&args.session_id, text.clone().into())
                            .await?;
                    }
                }
                EventMsg::AgentReasoningRawContent(reason) => {
                    self.finish_current_reasoning_section(&args.session_id);
                    if !reason.text.trim().is_empty() {
                        self.append_reasoning_delta(&args.session_id, &reason.text);
                    }
                }
                EventMsg::AgentReasoningSectionBreak(_) => {
                    self.finish_current_reasoning_section(&args.session_id);
                }
                // MCP tool calls → ACP ToolCall/ToolCallUpdate
                EventMsg::McpToolCallBegin(begin) => {
                    let (title, locations) = self.describe_mcp_tool(&begin.invocation);
                    let tool = acp::ToolCall {
                        id: acp::ToolCallId(begin.call_id.clone().into()),
                        title,
                        kind: acp::ToolKind::Fetch,
                        status: acp::ToolCallStatus::InProgress,
                        content: Vec::new(),
                        locations,
                        raw_input: begin.invocation.arguments,
                        raw_output: None,
                        meta: None,
                    };
                    self.send_session_update(&args.session_id, acp::SessionUpdate::ToolCall(tool))
                        .await?;
                }
                EventMsg::McpToolCallEnd(end) => {
                    // status and optional output
                    let status = if end.is_success() {
                        acp::ToolCallStatus::Completed
                    } else {
                        acp::ToolCallStatus::Failed
                    };
                    let raw_output = serde_json::to_value(&end.result).ok();
                    let (title, locations) = self.describe_mcp_tool(&end.invocation);
                    let update = acp::ToolCallUpdate {
                        id: acp::ToolCallId(end.call_id.clone().into()),
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
                    self.send_session_update(
                        &args.session_id,
                        acp::SessionUpdate::ToolCallUpdate(update),
                    )
                    .await?;
                }
                // Exec command begin/end → ACP ToolCall/ToolCallUpdate
                EventMsg::ExecCommandBegin(beg) => {
                    let FormatCommandCall {
                        title,
                        locations,
                        terminal_output,
                        kind,
                    } = Self::format_command_call(&beg.cwd, &beg.parsed_cmd);

                    let (content, meta) = if self.support_terminal() && terminal_output {
                        let content = vec![acp::ToolCallContent::Terminal {
                            terminal_id: acp::TerminalId(beg.call_id.clone().into()),
                        }];
                        let meta = Some(serde_json::json!({
                            "terminal_info": {
                                "terminal_id": beg.call_id,
                                "cwd": beg.cwd
                            }
                        }));
                        (content, meta)
                    } else {
                        (vec![], None)
                    };

                    let tool = acp::ToolCall {
                        id: acp::ToolCallId(beg.call_id.clone().into()),
                        title,
                        kind,
                        status: acp::ToolCallStatus::InProgress,
                        content,
                        locations,
                        raw_input: Some(json!({"command": beg.command, "cwd": beg.cwd})),
                        raw_output: None,
                        meta,
                    };
                    self.send_session_update(&args.session_id, acp::SessionUpdate::ToolCall(tool))
                        .await?;
                }
                EventMsg::ExecCommandEnd(end) => {
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
                        id: acp::ToolCallId(end.call_id.clone().into()),
                        fields: acp::ToolCallUpdateFields {
                            status: Some(status),
                            content: if content.is_empty() {
                                None
                            } else {
                                Some(content)
                            },
                            raw_output: Some(json!({
                                "exit_code": end.exit_code,
                                "duration_ms": end.duration.as_millis(),
                                "formatted_output": end.formatted_output,
                            })),
                            ..Default::default()
                        },
                        meta: None,
                    };
                    self.send_session_update(
                        &args.session_id,
                        acp::SessionUpdate::ToolCallUpdate(update),
                    )
                    .await?;
                }
                EventMsg::ExecApprovalRequest(req) => {
                    // Build a ToolCallUpdate describing the pending exec
                    let FormatCommandCall {
                        title,
                        locations,
                        terminal_output: _,
                        kind,
                    } = Self::format_command_call(&req.cwd, &req.parsed_cmd);

                    let update = acp::ToolCallUpdate {
                        id: acp::ToolCallId(req.call_id.clone().into()),
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

                    let permission_req = acp::RequestPermissionRequest {
                        session_id: args.session_id.clone(),
                        tool_call: update,
                        options: permission_opts.as_ref().clone(),
                        meta: None,
                    };

                    let (txp, rxp) = oneshot::channel();
                    let _ = self
                        .client_tx
                        .send(ClientOp::RequestPermission(permission_req, txp));
                    let outcome = rxp.await.map_err(|_| Error::internal_error())?;
                    if let Ok(resp) = outcome {
                        let decision = self.handle_response_outcome(resp);
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
                    // Summarize patch as content lines
                    let mut contents = Vec::new();
                    for (path, change) in req.changes.iter() {
                        use codex_core::protocol::FileChange as FC;
                        match change {
                            FC::Add { content } => {
                                contents.push(acp::ToolCallContent::from(acp::Diff {
                                    path: path.clone(),
                                    old_text: None,
                                    new_text: content.clone(),
                                    meta: None,
                                }));
                            }
                            FC::Delete { content } => {
                                contents.push(acp::ToolCallContent::from(acp::Diff {
                                    path: path.clone(),
                                    old_text: Some(content.clone()),
                                    new_text: "".into(),
                                    meta: None,
                                }));
                            }
                            FC::Update { unified_diff, .. } => {
                                contents.push(acp::ToolCallContent::from(acp::Diff {
                                    path: path.clone(),
                                    old_text: Some(unified_diff.into()),
                                    new_text: unified_diff.clone(),
                                    meta: None,
                                }));
                            }
                        };
                    }

                    let title = if req.changes.len() == 1 {
                        "Apply changes".to_string()
                    } else {
                        format!("Edit {} files", req.changes.len())
                    };
                    let update = acp::ToolCallUpdate {
                        id: acp::ToolCallId(req.call_id.clone().into()),
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

                    let permission_req = acp::RequestPermissionRequest {
                        session_id: args.session_id.clone(),
                        tool_call: update,
                        options: permission_opts.as_ref().clone(),
                        meta: None,
                    };
                    let (txp, rxp) = oneshot::channel();
                    let _ = self
                        .client_tx
                        .send(ClientOp::RequestPermission(permission_req, txp));
                    if let Ok(resp) = rxp.await.map_err(Error::into_internal_error)? {
                        let decision = self.handle_response_outcome(resp);
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

                    let update = acp::ToolCallUpdate {
                        id: acp::ToolCallId(call_id.into()),
                        fields: acp::ToolCallUpdateFields {
                            status: Some(if success {
                                acp::ToolCallStatus::Completed
                            } else {
                                acp::ToolCallStatus::Failed
                            }),
                            raw_output: Some(raw_output),
                            ..Default::default()
                        },
                        meta: None,
                    };

                    self.send_session_update(
                        &args.session_id,
                        acp::SessionUpdate::ToolCallUpdate(update),
                    )
                    .await?;
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

                    let entries: Vec<PlanEntry> = plan
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

        if let Some(text) = self.take_reasoning_text(&args.session_id)
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
