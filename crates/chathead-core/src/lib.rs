//! Security-sensitive provider state and the versioned desktop IPC contract.

mod codex_app_server;
mod conversation;
mod protocol;
mod providers;

pub use codex_app_server::{
    AuthenticationState, CodexAppServer, CodexCommand, CodexEvent, CodexServiceError,
};
pub use conversation::{
    AgentActivity, AgentActivityKind, AgentQuestion, AgentQuestionField, AgentQuestionOption,
    AgentRequestId, ChatAttachment, ChatMessage, ChatMode, ChatPrompt, Conversation, MessageRole,
    MessageState,
};

pub use protocol::{
    AuthMethod, BackendSnapshot, DesktopIntegrationKind, DesktopIntegrationSnapshot,
    DesktopIntegrationStatus, ErrorCode, ExperimentalChatSnapshot, ExperimentalChatState, IpcError,
    IpcEvent, IpcRequest, IpcResponse, LaunchBlocker, LaunchReadiness, MicrophoneAccess,
    PANEL_HEIGHT_DEFAULT, PANEL_HEIGHT_MAX, PANEL_HEIGHT_MIN, PANEL_WIDTH_DEFAULT, PANEL_WIDTH_MAX,
    PANEL_WIDTH_MIN, PANEL_ZOOM_LEVELS, PROTOCOL_VERSION, PanelSize, PanelZoom, ProviderId,
    ProviderKind, ProviderSnapshot, ProviderStatus, ShortcutAction, ShortcutActionsSnapshot,
    ShortcutBackend, ShortcutBinding, ShortcutCaptureSnapshot, ShortcutConfigFormat,
    ShortcutConflict, ShortcutIntegrationSnapshot, ShortcutState, ShortcutStatus, VoiceInputDevice,
    VoiceInteractionMode, VoiceModelId, VoiceModelSnapshot, VoiceModelState, VoicePhase,
    VoiceSnapshot, VoiceSubmissionMode,
};
pub use providers::{Backend, BackendError};
