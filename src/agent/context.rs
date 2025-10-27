use agent_client_protocol as acp;
use codex_core::{
    protocol::{AskForApproval, SandboxPolicy},
    protocol_config_types::ReasoningEffort,
};
use tokio::sync::oneshot;

/// Context needed for applying turn context overrides.
///
/// This encapsulates the current session state that needs to be preserved
/// or selectively overridden when changing session modes or models.
pub(super) struct SessionContext {
    pub approval: AskForApproval,
    pub sandbox: SandboxPolicy,
    pub model: Option<String>,
    pub effort: Option<ReasoningEffort>,
}

/// Operations that require client interaction.
///
/// These operations are sent to the client handler to request permissions,
/// read files, or write files based on client capabilities.
pub enum ClientOp {
    RequestPermission {
        session_id: acp::SessionId,
        request: acp::RequestPermissionRequest,
        response_tx: oneshot::Sender<Result<acp::RequestPermissionResponse, acp::Error>>,
    },
    ReadTextFile {
        session_id: acp::SessionId,
        request: acp::ReadTextFileRequest,
        response_tx: oneshot::Sender<Result<acp::ReadTextFileResponse, acp::Error>>,
    },
    WriteTextFile {
        session_id: acp::SessionId,
        request: acp::WriteTextFileRequest,
        response_tx: oneshot::Sender<Result<acp::WriteTextFileResponse, acp::Error>>,
    },
}
