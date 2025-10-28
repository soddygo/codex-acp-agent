use std::env;

use agent_client_protocol::{
    AgentCapabilities, AuthMethod, AuthMethodId, AuthenticateRequest, AuthenticateResponse, Error,
    Implementation, InitializeRequest, InitializeResponse, McpCapabilities, PromptCapabilities, V1,
};
use codex_app_server_protocol::AuthMode;
use tracing::info;

use super::{core::CodexAgent, session};

impl CodexAgent {
    /// Initialize the agent and return supported capabilities and authentication methods.
    pub(super) async fn initialize(
        &self,
        args: InitializeRequest,
    ) -> Result<InitializeResponse, Error> {
        info!(?args, "Received initialize request");

        // Advertise supported auth methods based on the configured provider
        let mut auth_methods = vec![
            AuthMethod {
                id: AuthMethodId("chatgpt".into()),
                name: "ChatGPT".into(),
                description: Some("Sign in with ChatGPT to use your plan".into()),
                meta: None,
            },
            AuthMethod {
                id: AuthMethodId("apikey".into()),
                name: "OpenAI API Key".into(),
                description: Some("Use OPENAI_API_KEY from environment or auth.json".into()),
                meta: None,
            },
        ];

        // Add custom provider auth method if using a custom provider
        if session::is_custom_provider(&self.config.model_provider_id) {
            auth_methods.push(AuthMethod {
                id: AuthMethodId(self.config.model_provider_id.clone().into()),
                name: self.config.model_provider.name.clone(),
                description: Some(format!(
                    "Authenticate with custom provider: {}",
                    self.config.model_provider_id
                )),
                meta: None,
            });
        }

        self.client_capabilities.replace(args.client_capabilities);

        let agent_capabilities = AgentCapabilities {
            load_session: false,
            prompt_capabilities: PromptCapabilities {
                image: true,
                audio: false,
                embedded_context: true,
                meta: None,
            },
            mcp_capabilities: McpCapabilities {
                http: true,
                sse: true,
                meta: None,
            },
            meta: None,
        };

        Ok(InitializeResponse {
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

    /// Authenticate the client using the specified authentication method.
    pub(super) async fn authenticate(
        &self,
        args: AuthenticateRequest,
    ) -> Result<AuthenticateResponse, Error> {
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
            "custom_provider" => {
                // For custom providers, check if the provider is configured
                if !session::is_custom_provider(&self.config.model_provider_id) {
                    return Err(Error::invalid_params().with_data(
                        "Custom provider auth method is only available for custom providers",
                    ));
                }

                // Verify the custom provider is properly configured in model_providers
                if !self
                    .config
                    .model_providers
                    .contains_key(&self.config.model_provider_id)
                {
                    return Err(Error::auth_required().with_data(format!(
                        "Custom provider '{}' is not configured in model_providers",
                        self.config.model_provider_id
                    )));
                }

                // For custom providers, we assume authentication is handled via the provider's
                // configuration (e.g., API keys in the provider settings). If auth_manager
                // has valid auth, accept it; otherwise require configuration.
                if let Ok(am) = self.auth_manager.write() {
                    am.reload();
                    if am.auth().is_some() {
                        return Ok(Default::default());
                    }
                }

                Err(Error::auth_required().with_data(format!(
                    "Custom provider '{}' requires authentication. Please configure API credentials in your Codex config.",
                    self.config.model_provider_id
                )))
            }
            other => {
                Err(Error::invalid_params().with_data(format!("unknown auth method: {}", other)))
            }
        }
    }
}
