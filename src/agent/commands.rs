use super::*;
use agent_client_protocol::{AvailableCommand, AvailableCommandInput};
use codex_core::{
    NewConversation,
    protocol::{AskForApproval, Op, ReviewRequest, SandboxPolicy},
};
use std::sync::LazyLock;
use tokio::sync::oneshot;

pub static AVAILABLE_COMMANDS: LazyLock<Vec<AvailableCommand>> = LazyLock::new(built_in_commands);

impl CodexAgent {
    pub async fn handle_slash_command(
        &self,
        session_id: &acp::SessionId,
        name: &str,
        rest: &str,
    ) -> Result<bool, Error> {
        match name {
            "new" => self.handle_new_cmd(session_id).await,
            "status" => self.handle_status_cmd(session_id).await,
            "model" => self.handle_model_cmd(session_id, rest).await,
            "quit" => self.handle_quit_cmd(session_id).await,
            _ => Ok(false),
        }
    }

    async fn handle_new_cmd(&self, session_id: &acp::SessionId) -> Result<bool, Error> {
        let (conversation_id, conversation) = match self
            .conversation_manager
            .new_conversation(self.config.clone())
            .await
        {
            Ok(NewConversation {
                conversation_id,
                conversation,
                ..
            }) => (conversation_id, conversation),
            Err(e) => {
                self.send_message_chunk(
                    session_id,
                    format!("Failed to start new conversation: {}", e).into(),
                )
                .await?;
                return Ok(true);
            }
        };

        let current_mode = Self::modes(&self.config)
            .as_ref()
            .map(|m| m.current_mode_id.clone())
            .unwrap_or(acp::SessionModeId("auto".into()));

        let fs_session_id = Uuid::new_v4().to_string();
        self.sessions.borrow_mut().insert(
            conversation_id.to_string(),
            SessionState {
                fs_session_id,
                conversation: Some(conversation.clone()),
                current_approval: self.config.approval_policy,
                current_sandbox: self.config.sandbox_policy.clone(),
                current_mode,
                token_usage: None,
                reasoning_sections: Vec::new(),
                current_reasoning_chunk: String::new(),
            },
        );

        self.send_message_chunk(session_id, "✨ Started a new conversation".into())
            .await?;
        Ok(true)
    }

    async fn handle_status_cmd(&self, session_id: &acp::SessionId) -> Result<bool, Error> {
        let status_text = self.render_status(session_id).await;
        self.send_message_chunk(session_id, status_text.into())
            .await?;
        Ok(true)
    }

    async fn handle_model_cmd(
        &self,
        session_id: &acp::SessionId,
        rest: &str,
    ) -> Result<bool, Error> {
        let trimmed = rest.trim();
        if trimmed.is_empty() {
            let msg = format!(
                "Current model: {}\nUsage: /model <model-slug>",
                self.config.model,
            );
            self.send_message_chunk(session_id, msg.into()).await?;
            return Ok(true);
        }

        let conversation = self.get_conversation(session_id).await?;
        conversation
            .submit(Op::OverrideTurnContext {
                cwd: None,
                approval_policy: None,
                sandbox_policy: None,
                model: Some(trimmed.to_string()),
                effort: None,
                summary: None,
            })
            .await
            .map_err(Error::into_internal_error)?;

        self.send_message_chunk(
            session_id,
            format!("🧠 Requested model change to: `{}`", trimmed).into(),
        )
        .await?;
        Ok(true)
    }

    async fn handle_quit_cmd(&self, session_id: &acp::SessionId) -> Result<bool, Error> {
        let conversation = self.get_conversation(session_id).await?;
        let mut quit_msg = "👋 Codex agent is shutting down. Goodbye!".to_string();

        if let Err(e) = conversation.submit(Op::Shutdown).await {
            quit_msg = format!("Failed to submit shutdown: {}", e);
        }

        self.send_message_chunk(session_id, quit_msg.into()).await?;
        Ok(true)
    }

    pub async fn handle_background_task_command(
        &self,
        session_id: &acp::SessionId,
        name: &str,
    ) -> Option<Op> {
        if !matches!(name, "init" | "review" | "compact") {
            return None;
        }

        let mut msg = String::default();
        // Commands forwarded to Codex as protocol Ops
        let op = match name {
            "init" => {
                let prompt = include_str!("./prompt_init_command.md");

                msg = "📝 Creating AGENTS.md file with initial instructions...\n\n".into();
                Some(Op::UserInput {
                    items: vec![UserInput::Text {
                        text: prompt.into(),
                    }],
                })
            }
            "compact" => {
                self.with_session_state_mut(session_id, |state| {
                    state.token_usage = None;
                });
                msg = "🧠 Compacting conversation to reduce context size...\n\n".into();
                Some(Op::Compact)
            }
            "review" => {
                msg = "🔍 Asking Codex to review current changes...\n\n".into();
                Some(Op::Review {
                    review_request: ReviewRequest {
                        prompt: "review current changes".to_string(),
                        user_facing_hint: "current changes".to_string(),
                    },
                })
            }
            _ => None,
        };

        if !msg.is_empty() {
            drop(self.send_message_chunk(session_id, msg.into()).await);
        }
        op
    }

    async fn render_status(&self, session_id: &acp::SessionId) -> String {
        let sid_str = session_id.0.as_ref();
        // Session snapshot
        let (approval_mode, sandbox_mode, token_usage) = {
            if let Some(state) = self.sessions.borrow().get(sid_str) {
                (
                    state.current_approval,
                    state.current_sandbox.clone(),
                    state.token_usage.clone(),
                )
            } else {
                (
                    AskForApproval::OnRequest,
                    SandboxPolicy::new_workspace_write_policy(),
                    None,
                )
            }
        };

        // Workspace
        let cwd = self.shorten_home(&self.config.cwd);
        let agents_files = self.find_agents_files(Some(session_id)).await;
        let agents_line = if agents_files.is_empty() {
            "(none)".to_string()
        } else {
            agents_files
                .iter()
                .map(|f| self.shorten_home(&self.config.cwd.join(f)))
                .collect::<Vec<_>>()
                .join(", ")
        };

        // Account
        let (auth_mode, email, plan): (String, String, String) =
            match self.auth_manager.read().ok().and_then(|am| am.auth()) {
                Some(auth) => match auth.get_token_data().await {
                    Ok(td) => {
                        let email = td
                            .id_token
                            .email
                            .clone()
                            .unwrap_or_else(|| "(none)".to_string());
                        let plan = td
                            .id_token
                            .get_chatgpt_plan_type()
                            .unwrap_or_else(|| "(unknown)".to_string());
                        ("ChatGPT".to_string(), email, plan)
                    }
                    Err(_) => (
                        "API key".to_string(),
                        "(none)".to_string(),
                        "(unknown)".to_string(),
                    ),
                },
                None => (
                    "Not signed in".to_string(),
                    "(none)".to_string(),
                    "(unknown)".to_string(),
                ),
            };

        // Model
        let model = &self.config.model;
        let provider = self.title_case(&self.config.model_provider_id);
        let effort = self.title_case(
            format!("{}", self.config.model_reasoning_effort.unwrap_or_default()).as_str(),
        );
        let summary = self.title_case(format!("{}", self.config.model_reasoning_summary).as_str());

        // Tokens
        let (input, output, total) = match token_usage {
            Some(u) => (
                u.input_tokens.to_string(),
                u.output_tokens.to_string(),
                u.total_tokens.to_string(),
            ),
            None => ("0".to_string(), "0".to_string(), "0".to_string()),
        };

        let status = format!(
            r#"
📂 Workspace

    Path:          {cwd}
    Approval Mode: {approval}
    Sandbox:       {sandbox}
    AGENTS files:  {agents}

👤 Account

    Signed in with: {auth_mode}
    Login:          {email}
    Plan:           {plan}

🧠 Model

    Name:                {model}
    Provider:            {provider}
    Reasoning Effort:    {effort}
    Reasoning Summaries: {summary}

📊 Token Usage

    Session ID:     {sid}
    Input:          {input}
    Output:         {output}
    Total:          {total}
"#,
            cwd = cwd,
            approval = approval_mode,
            sandbox = sandbox_mode,
            agents = agents_line,
            auth_mode = auth_mode,
            email = email,
            plan = plan,
            model = model,
            provider = provider,
            effort = effort,
            summary = summary,
            sid = sid_str,
            input = input,
            output = output,
            total = total,
        );
        status
    }

    fn shorten_home(&self, p: &Path) -> String {
        let s = p.display().to_string();
        if let Ok(home) = std::env::var("HOME")
            && s.starts_with(&home)
        {
            return s.replacen(&home, "~", 1);
        }
        s
    }

    async fn find_agents_files(&self, session_id: Option<&acp::SessionId>) -> Vec<String> {
        let mut names = Vec::new();
        let candidates = ["AGENTS.md", "Agents.md", "agents.md"];

        for candidate in candidates.iter() {
            let path = self.config.cwd.join(candidate);
            let mut found = false;

            if let Some(session_id) = session_id
                && self.client_supports_fs_read()
                && self
                    .client_read_text_file(session_id, path.clone(), Some(1), Some(1))
                    .await
                    .is_ok()
            {
                found = true;
            }

            if !found && path.exists() {
                found = true;
            }

            if found {
                names.push((*candidate).to_string());
            }
        }

        names
    }

    fn client_supports_fs_read(&self) -> bool {
        self.client_capabilities.borrow().fs.read_text_file
    }

    async fn client_read_text_file(
        &self,
        session_id: &acp::SessionId,
        path: PathBuf,
        line: Option<u32>,
        limit: Option<u32>,
    ) -> Result<acp::ReadTextFileResponse, Error> {
        let (tx, rx) = oneshot::channel();
        let request = acp::ReadTextFileRequest {
            session_id: session_id.clone(),
            path,
            line,
            limit,
            meta: None,
        };
        self.client_tx
            .send(ClientOp::ReadTextFile(request, tx))
            .map_err(|_| {
                Error::internal_error().with_data("client read_text_file channel closed")
            })?;
        rx.await.map_err(|_| {
            Error::internal_error().with_data("client read_text_file response dropped")
        })?
    }

    fn title_case(&self, s: &str) -> String {
        if s.is_empty() {
            return s.to_string();
        }
        let mut chars = s.chars();
        let first = chars.next().unwrap().to_uppercase().to_string();
        let rest = chars.as_str();
        format!("{}{}", first, rest)
    }
}

fn built_in_commands() -> Vec<AvailableCommand> {
    vec![
        AvailableCommand {
            name: "new".into(),
            description: "start a new chat during a conversation".into(),
            input: None,
            meta: None,
        },
        AvailableCommand {
            name: "init".into(),
            description: "create an AGENTS.md file with instructions for Codex".into(),
            input: None,
            meta: None,
        },
        AvailableCommand {
            name: "compact".into(),
            description: "summarize conversation to prevent hitting the context limit".into(),
            input: None,
            meta: None,
        },
        AvailableCommand {
            name: "review".into(),
            description: "review my current changes and find issues".to_string(),
            input: None,
            meta: None,
        },
        AvailableCommand {
            name: "model".into(),
            description: "choose what model and reasoning effort to use".into(),
            input: Some(AvailableCommandInput::Unstructured {
                hint: "Model slug, e.g., gpt-codex".into(),
            }),
            meta: None,
        },
        AvailableCommand {
            name: "approvals".into(),
            description: "choose what Codex can do without approval".into(),
            input: Some(AvailableCommandInput::Unstructured {
                hint: "read-only|auto|full-access".into(),
            }),
            meta: None,
        },
        AvailableCommand {
            name: "status".into(),
            description: "show current session configuration and token usage".into(),
            input: None,
            meta: None,
        },
        AvailableCommand {
            name: "quit".into(),
            description: "exit Codex".into(),
            input: None,
            meta: None,
        },
    ]
}
