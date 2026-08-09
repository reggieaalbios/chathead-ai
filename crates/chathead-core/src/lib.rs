//! Security-sensitive provider state and the versioned desktop IPC contract.

mod codex_app_server;
mod conversation;
mod protocol;
mod providers;

pub use codex_app_server::{
    AuthenticationState, CodexAppServer, CodexCommand, CodexEvent, CodexServiceError,
};
pub use conversation::{ChatMessage, Conversation, MessageRole, MessageState};

pub use protocol::{
    AuthMethod, BackendSnapshot, ErrorCode, ExperimentalChatSnapshot, ExperimentalChatState,
    IpcError, IpcEvent, IpcRequest, IpcResponse, LaunchReadiness, PROTOCOL_VERSION, ProviderId,
    ProviderKind, ProviderSnapshot, ProviderStatus, ShortcutStatus, VoiceState,
};
pub use providers::{Backend, BackendError};
