use super::*;
use codex_core::NewConversation;
use agent_client_protocol::{AvailableCommand, AvailableCommandInput};
use codex_core::protocol::{AskForApproval, Op, ReviewRequest, SandboxPolicy};
use std::{fs, io};
use tokio::sync::oneshot;

impl CodexAgent {
    pub async fn handle_slash_command(
        &self,
        session_id: &SessionId,
        name: &str,
        _rest: &str,
    ) -> Result<bool, Error> {
        let sid_str = session_id.0.to_string();
        let mut session = match self.sessions.borrow().get(&sid_str) {
            Some(s) => s.clone(),
            None => return Err(Error::invalid_params()),
        };

        // Commands implemented inline (no Codex submission needed)
        match name {
            "new" => {
                // Start a new conversation within the current session
                let (conversation_id, conversation_opt, _session_configured) = match self
                    .conversation_manager
                    .new_conversation(self.config.clone())
                    .await
                {
                    Ok(NewConversation {
                        conversation_id,
                        conversation,
                        session_configured,
                    }) => (conversation_id, Some(conversation), session_configured),
                    Err(e) => {
                        let msg = format!("Failed to start new conversation: {}", e);
                        let (tx, rx) = oneshot::channel();
                        self.send_message_chunk(session_id, msg.into(), tx)?;
                        let _ = rx.await;
                        return Ok(true);
                    }
                };

                // Update the session with the new conversation
                session.conversation_id = conversation_id.to_string();
                session.conversation = conversation_opt;
                self.sessions.borrow_mut().insert(sid_str, session);

                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(session_id, "✨ Started a new conversation".into(), tx)?;
                let _ = rx.await;
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
                    let _ = rx.await;
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
                let _ = rx.await;
                return Ok(true);
            }
            "status" => {
                let status_text = self.render_status(&sid_str).await;
                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(session_id, status_text.into(), tx)?;
                let _ = rx.await;
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
                    let _ = rx.await;
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
                if let Some(conv) = session.conversation.as_ref() {
                    conv.submit(op).await.map_err(Error::into_internal_error)?;
                } else {
                    let msg = "/model not available without Codex backend";
                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(session_id, msg.into(), tx)?;
                    let _ = rx.await;
                    return Ok(true);
                }

                // Provide immediate feedback to the user.
                let ack = format!("Requested model change to: {}", rest);
                let (tx, rx) = oneshot::channel();
                self.send_message_chunk(session_id, ack.into(), tx)?;
                let _ = rx.await;
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
                        let _ = rx.await;
                        return Ok(true);
                    }
                };

                if let Some(policy) = parsed {
                    let op = Op::OverrideTurnContext {
                        cwd: None,
                        approval_policy: Some(policy),
                        sandbox_policy: None,
                        model: None,
                        effort: None,
                        summary: None,
                    };
                    if let Some(conv) = session.conversation.as_ref() {
                        conv.submit(op).await.map_err(Error::into_internal_error)?;
                    } else {
                        let msg = "Dev mock mode: /approvals requires Codex backend";
                        let (tx, rx) = oneshot::channel();
                        self.send_message_chunk(session_id, msg.into(), tx)?;
                        let _ = rx.await;
                        return Ok(true);
                    }
                    // Persist our local view of the policy for /status
                    if let Ok(mut map) = self.sessions.try_borrow_mut()
                        && let Some(state) = map.get_mut(&sid_str)
                    {
                        state.current_approval = policy;
                    }
                    let msg = format!("Approval policy set to: {}", value);
                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(session_id, msg.into(), tx)?;
                    let _ = rx.await;
                } else {
                    // show current (best-effort from config)
                    let msg = "Current approval policy: configured per session. Use /approvals <policy> to set.";
                    let (tx, rx) = oneshot::channel();
                    self.send_message_chunk(session_id, msg.into(), tx)?;
                    let _ = rx.await;
                }
                return Ok(true);
            }
            "compact" => session.token_usage = None,
            _ => {}
        }

        // Commands forwarded to Codex as protocol Ops
        let op = match name {
            "compact" => Some(Op::Compact),
            "review" => Some(Op::Review {
                review_request: ReviewRequest {
                    prompt: "review current changes".to_string(),
                    user_facing_hint: "current changes".to_string(),
                },
            }),
            "quit" => Some(Op::Shutdown),
            _ => None,
        };

        if let Some(op) = op {
            if let Some(conv) = session.conversation.as_ref() {
                conv.submit(op).await.map_err(Error::into_internal_error)?;
            }

            // Stream events for this submission using the same loop as in prompt
            return Ok(true);
        }
        Ok(false)
    }

    async fn render_status(&self, sid_str: &str) -> String {
        // Session snapshot
        let (approval_mode, sandbox_mode, token_usage, session_uuid) = {
            let map = self.sessions.borrow();
            if let Some(state) = map.get(sid_str) {
                (
                    state.current_approval,
                    state.current_sandbox.clone(),
                    state.token_usage.clone(),
                    state.conversation_id.to_string(),
                )
            } else {
                (
                    AskForApproval::OnRequest,
                    SandboxPolicy::new_workspace_write_policy(),
                    None,
                    String::new(),
                )
            }
        };

        // Workspace
        let cwd = self.shorten_home(&self.config.cwd);
        let agents_files = self.find_agents_files();
        let agents_line = if agents_files.is_empty() {
            "(none)".to_string()
        } else {
            agents_files.join(", ")
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
        let effort = format!("{:?}", self.config.model_reasoning_effort);
        let summary = format!("{}", self.config.model_reasoning_summary);

        // Tokens
        let (input, output, total) = match token_usage {
            Some(u) => (u.input_tokens, u.output_tokens, u.total_tokens),
            None => (0, 0, 0),
        };

        format!(
            "📂 Workspace\n  • Path: {cwd}\n  • Approval Mode: {approval}\n  • Sandbox: {sandbox}\n  • AGENTS files: {agents}\n\n👤 Account\n  • Signed in with {auth_mode}\n  • Login: {email}\n  • Plan: {plan}\n\n🧠 Model\n  • Name: {model}\n  • Provider: {provider}\n  • Reasoning Effort: {effort}\n  • Reasoning Summaries: {summary}\n\n📊 Token Usage\n  • Session ID: {sid}\n  • Input: {input}\n  • Output: {output}\n  • Total: {total}",
            cwd = cwd,
            approval = approval_mode,
            sandbox = sandbox_mode,
            agents = agents_line,
            auth_mode = auth_mode,
            email = email,
            plan = plan,
            model = model,
            provider = provider,
            effort = self.title_case(&effort),
            summary = self.title_case(&summary),
            sid = session_uuid,
            input = input,
            output = output,
            total = total,
        )
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

pub fn built_in_commands() -> Vec<AvailableCommand> {
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
