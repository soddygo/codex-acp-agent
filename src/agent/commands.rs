use super::*;
use agent_client_protocol::{AvailableCommand, AvailableCommandInput};
use codex_core::{
    NewConversation,
    protocol::{AskForApproval, Op, ReviewRequest, SandboxPolicy},
};
use std::{fs, io};
use tokio::sync::oneshot;

pub static AVAILABLE_COMMANDS: LazyLock<Vec<AvailableCommand>> = LazyLock::new(built_in_commands);

impl CodexAgent {
    pub async fn handle_slash_command(
        &self,
        session_id: &acp::SessionId,
        name: &str,
        _rest: &str,
    ) -> Result<bool, Error> {
        let conversation = self.get_conversation(session_id).await?;

        // Commands implemented inline (no Codex submission needed)
        match name {
            "new" => {
                // Start a new conversation within the current session
                let conversation_id = match self
                    .conversation_manager
                    .new_conversation(self.config.clone())
                    .await
                {
                    Ok(NewConversation {
                        conversation_id, ..
                    }) => conversation_id,
                    Err(e) => {
                        let (tx, rx) = oneshot::channel();
                        self.send_message_chunk(
                            session_id,
                            format!("Failed to start new conversation: {}", e).into(),
                            tx,
                        )?;
                        rx.await.map_err(Error::into_internal_error)?;
                        return Ok(true);
                    }
                };

                let current_mode = Self::modes(&self.config)
                    .as_ref()
                    .map(|m| m.current_mode_id.clone())
                    .unwrap_or(acp::SessionModeId("auto".into()));

                // Update the session with the new conversation
                self.sessions.borrow_mut().insert(
                    conversation_id.to_string(),
                    SessionState {
                        current_approval: self.config.approval_policy,
                        current_sandbox: self.config.sandbox_policy.clone(),
                        current_mode,
                        token_usage: None,
                        reasoning_sections: Vec::new(),
                        current_reasoning_chunk: String::new(),
                    },
                );

                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(session_id, "✨ Started a new conversation".into(), tx)?;
                rx.await.map_err(Error::into_internal_error)?;
                return Ok(true);
            }
            "init" => {
                // Create AGENTS.md in the current workspace if it doesn't already exist.
                let rest = _rest.trim();
                let force = matches!(rest, "--force" | "-f" | "force");

                let cwd = self.config.cwd.clone();
                // If any AGENTS* file already exists and not forcing, bail out.
                let existing = self.find_agents_files();
                if !existing.is_empty() && !force {
                    let msg = format!(
                        "AGENTS file already exists: {}\nUse /init --force to overwrite.",
                        existing.join(", ")
                    );

                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(session_id, msg.into(), tx)?;
                    rx.await.map_err(Error::into_internal_error)?;
                    return Ok(true);
                }

                let target = cwd.join("AGENTS.md");
                let template = r#"# AGENTS.md

This file gives Codex instructions for working in this repository. Place project-specific tips here so the agent acts consistently with your workflows.

Scope
- The scope of this file is the entire repository (from this folder down).
- Add more AGENTS.md files in subdirectories for overrides; deeper files take precedence.

Coding Conventions
- Keep changes minimal and focused on the task.
- Match the existing code style and structure; avoid wholesale refactors.
- Don't add licenses or headers unless requested.

Workflow
- How to run and test: describe commands (e.g., `cargo test`, `npm test`).
- Any environment variables or secrets required for local runs.
- Where to place new modules, configs, or scripts.

Reviews and Safety
- Point out risky or destructive actions before performing them.
- Prefer root-cause fixes over band-aids.
- When in doubt, ask for confirmation.

Notes for Agents
- Follow instructions in this file for all edits within its scope.
- Files in deeper directories with their own AGENTS.md override these rules.
"#;

                // Try to write the file; on errors, surface a message.
                let result = (|| -> io::Result<()> {
                    // Ensure parent exists (workspace root should exist already).
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(&target, template)
                })();

                let msg = match result {
                    Ok(()) => format!(
                        "Initialized AGENTS.md at {}\nEdit it to customize agent behavior.",
                        self.shorten_home(&target)
                    ),
                    Err(e) => format!(
                        "Failed to create AGENTS.md: {}\nPath: {}",
                        e,
                        self.shorten_home(&target)
                    ),
                };

                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(session_id, msg.into(), tx)?;
                rx.await.map_err(Error::into_internal_error)?;
                return Ok(true);
            }
            "status" => {
                let status_text = self.render_status(session_id.0.as_ref()).await;
                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(session_id, status_text.into(), tx)?;
                rx.await.map_err(Error::into_internal_error)?;
                return Ok(true);
            }
            "model" => {
                let rest = _rest.trim();
                if rest.is_empty() {
                    let msg = format!(
                        "Current model: {}\nUsage: /model <model-slug>",
                        self.config.model,
                    );
                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(session_id, msg.into(), tx)?;
                    rx.await.map_err(Error::into_internal_error)?;
                    return Ok(true);
                }

                // Request Codex to change the model for subsequent turns.
                let op = Op::OverrideTurnContext {
                    cwd: None,
                    approval_policy: None,
                    sandbox_policy: None,
                    model: Some(rest.to_string()),
                    effort: None,
                    summary: None,
                };

                conversation
                    .submit(op)
                    .await
                    .map_err(Error::into_internal_error)?;

                // Provide immediate feedback to the user.
                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(
                    session_id,
                    format!("🧠 Requested model change to: `{}`", rest).into(),
                    tx,
                )?;
                rx.await.map_err(Error::into_internal_error)?;
                return Ok(true);
            }
            "approvals" => {
                let value = _rest.trim().to_lowercase();
                let parsed = match value.as_str() {
                    "" | "show" => None,
                    "on-request" => Some(AskForApproval::OnRequest),
                    "on-failure" => Some(AskForApproval::OnFailure),
                    "never" => Some(AskForApproval::Never),
                    "untrusted" | "unless-trusted" => Some(AskForApproval::UnlessTrusted),
                    _ => {
                        let msg = "Usage: /approvals untrusted|on-request|on-failure|never";
                        let (tx, rx) = oneshot::channel();
                        self.send_message_chunk(session_id, msg.into(), tx)?;
                        rx.await.map_err(Error::into_internal_error)?;
                        return Ok(true);
                    }
                };

                return if let Some(policy) = parsed {
                    let submit_result = conversation
                        .submit(Op::OverrideTurnContext {
                            cwd: None,
                            approval_policy: Some(policy),
                            sandbox_policy: None,
                            model: None,
                            effort: None,
                            summary: None,
                        })
                        .await;

                    if let Err(e) = submit_result {
                        let (tx, rx) = oneshot::channel();
                        self.send_message_chunk(
                            session_id,
                            format!("❌ Failed to set approval policy: {}", e).into(),
                            tx,
                        )?;
                        rx.await.map_err(Error::into_internal_error)?;
                        return Ok(true);
                    }

                    // Persist our local view of the policy for /status
                    self.with_session_state_mut(session_id, |state| {
                        state.current_approval = policy;
                    });

                    let msg = format!(
                        "✅ Approval policy updated to: `{}`. Use `/status` to view current session settings.",
                        value
                    );
                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(session_id, msg.into(), tx)?;
                    rx.await.map_err(Error::into_internal_error)?;
                    Ok(true)
                } else {
                    let msg = "⚠️ Unable to set approval policy: Codex backend not connected.";
                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(session_id, msg.into(), tx)?;
                    rx.await.map_err(Error::into_internal_error)?;
                    Ok(true)
                };
            }
            "quit" => {
                // Say goodbye and submit Shutdown to Codex if available
                let quit_msg = "👋 Codex agent is shutting down. Goodbye!";
                // Request backend shutdown
                if let Err(e) = conversation.submit(Op::Shutdown).await {
                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(
                        session_id,
                        format!("Failed to submit shutdown: {}", e).into(),
                        tx,
                    )?;
                    let _ = rx.await;
                    return Ok(true);
                }
                // Send the goodbye message
                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(session_id, quit_msg.into(), tx)?;
                let _ = rx.await;
                return Ok(true);
            }
            _ => {}
        }

        let mut msg = String::default();
        // Commands forwarded to Codex as protocol Ops
        let op = match name {
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

        if let Some(op) = op {
            let submit_result = conversation.submit(op).await;
            if let Err(e) = submit_result {
                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(
                    session_id,
                    format!("Failed to submit message: {}", e).into(),
                    tx,
                )?;
                rx.await.map_err(Error::into_internal_error)?;
                return Ok(true);
            }

            let (tx, rx) = oneshot::channel();
            self.send_message_chunk(session_id, msg.into(), tx)?;
            rx.await.map_err(Error::into_internal_error)?;

            loop {
                let event = conversation
                    .next_event()
                    .await
                    .map_err(Error::into_internal_error)?;

                match event.msg {
                    EventMsg::ExitedReviewMode(e) => {
                        if let Some(review_output) = e.review_output {
                            let (tx, rx) = oneshot::channel();
                            self.send_message_chunk(
                                session_id,
                                serde_json::to_string_pretty(&review_output)
                                    .unwrap_or_else(|_| "<failed to serialize>".to_string())
                                    .into(),
                                tx,
                            )?;
                            rx.await.map_err(Error::into_internal_error)?;
                        }
                    }
                    EventMsg::TaskComplete(_) | EventMsg::ShutdownComplete => {
                        let (tx, rx) = oneshot::channel();
                        self.send_message_chunk(session_id, "Task completed".into(), tx)?;
                        rx.await.map_err(Error::into_internal_error)?;
                        break;
                    }
                    EventMsg::StreamError(err) => {
                        let (tx, rx) = oneshot::channel();
                        let mut msg = err.message;
                        msg.push_str("\n\n");
                        self.send_message_chunk(session_id, msg.into(), tx)?;
                        rx.await.map_err(Error::into_internal_error)?;
                    }
                    EventMsg::Error(err) => {
                        let (tx, rx) = oneshot::channel();
                        self.send_message_chunk(session_id, err.message.into(), tx)?;
                        rx.await.map_err(Error::into_internal_error)?;
                        break;
                    }
                    _ => {}
                }
            }
            return Ok(true);
        }

        Ok(false)
    }

    async fn render_status(&self, sid_str: &str) -> String {
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
        let agents_files = self.find_agents_files();
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

    fn shorten_home(&self, p: &std::path::Path) -> String {
        let s = p.display().to_string();
        if let Ok(home) = std::env::var("HOME")
            && s.starts_with(&home)
        {
            return s.replacen(&home, "~", 1);
        }
        s
    }

    fn find_agents_files(&self) -> Vec<String> {
        let mut names = Vec::new();
        let candidates = ["AGENTS.md", "Agents.md", "agents.md"];
        for c in candidates.iter() {
            let path = self.config.cwd.join(c);
            if path.exists() {
                names.push(c.to_string());
            }
        }
        names
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
                hint: "untrusted|on-request|on-failure|never".into(),
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
