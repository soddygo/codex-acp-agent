use std::env;

use agent_client_protocol as acp;
use codex_app_server_protocol::AuthMode;
use tracing::info;

use super::core::CodexAgent;

impl CodexAgent {
    /// Initialize the agent and return supported capabilities and authentication methods.
    pub(super) async fn initialize(
        &self,
        args: acp::InitializeRequest,
    ) -> Result<acp::InitializeResponse, acp::Error> {
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
            protocol_version: acp::V1,
            agent_capabilities,
            auth_methods,
            agent_info: Some(acp::Implementation {
                name: "codex-acp".into(),
                title: Some("Codex ACP".into()),
                version: env!("CARGO_PKG_VERSION").into(),
            }),
            meta: None,
        })
    }

    /// Authenticate the client using the specified authentication method.
    pub(super) async fn authenticate(
        &self,
        args: acp::AuthenticateRequest,
    ) -> Result<acp::AuthenticateResponse, acp::Error> {
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
                Err(acp::Error::auth_required().with_data("Failed to load API key auth"))
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
                Err(acp::Error::auth_required()
                    .with_data("ChatGPT login not found. Run `codex login` to connect your plan."))
            }
            other => {
                Err(acp::Error::invalid_params()
                    .with_data(format!("unknown auth method: {}", other)))
            }
        }
    }
}
