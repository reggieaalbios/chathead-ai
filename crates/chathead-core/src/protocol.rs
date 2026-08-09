use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u16 = 3;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExperimentalChatSnapshot {
    pub provider_id: ProviderId,
    pub experimental: bool,
    pub state: ExperimentalChatState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ExperimentalChatState {
    Probing,
    Authenticating,
    Ready,
    Unavailable,
    Error,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum ProviderId {
    #[serde(rename = "chatgpt")]
    ChatGpt,
    #[serde(rename = "claude")]
    Claude,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "grok")]
    Grok,
    #[serde(rename = "zep")]
    Zep,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    LargeLanguageModel,
    MemoryContext,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum AuthMethod {
    ApiKey,
    SubscriptionLogin,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ProviderStatus {
    Unconfigured,
    Authenticating,
    Authenticated { method: AuthMethod },
    Error { message: String },
    Unavailable { message: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LaunchReadiness {
    Ready,
    MissingLaunchProvider,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VoiceState {
    #[default]
    Idle,
    Listening,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ShortcutStatus {
    Registering,
    Ready { trigger: String },
    ConflictPossible { details: String },
    Unavailable { details: String },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSnapshot {
    pub id: ProviderId,
    pub name: &'static str,
    pub description: &'static str,
    pub kind: ProviderKind,
    pub api_key_label: &'static str,
    pub supports_subscription: bool,
    pub status: ProviderStatus,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendSnapshot {
    pub providers: Vec<ProviderSnapshot>,
    pub launch_readiness: LaunchReadiness,
    pub overlay_running: bool,
    pub voice_state: VoiceState,
    pub shortcut_status: ShortcutStatus,
    pub experimental_chat: ExperimentalChatSnapshot,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcRequest {
    pub protocol_version: u16,
    pub id: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    InvalidApiKey,
    CredentialStoreUnavailable,
    CodexNotFound,
    AuthFailed,
    LayerShellUnsupported,
    SidecarUnavailable,
    ProtocolMismatch,
    InvalidRequest,
    UnsupportedOperation,
    CodexProtocolError,
    ChatUnavailable,
    ChatBusy,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    pub code: ErrorCode,
    pub message: String,
    pub recoverable: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcResponse<T: Serialize> {
    pub protocol_version: u16,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<T>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<IpcError>,
}

impl<T: Serialize> IpcResponse<T> {
    pub fn success(id: String, result: T) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: String, error: IpcError) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            id,
            result: None,
            error: Some(error),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpcEvent<T: Serialize> {
    pub protocol_version: u16,
    pub event: &'static str,
    pub payload: T,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_ids_are_stable_lowercase_values() {
        assert_eq!(
            serde_json::to_string(&ProviderId::ChatGpt).expect("serialize"),
            "\"chatgpt\""
        );
        assert_eq!(
            serde_json::to_string(&ProviderId::Zep).expect("serialize"),
            "\"zep\""
        );
    }

    #[test]
    fn response_shape_never_has_a_credential_field() {
        let json = serde_json::to_string(&IpcResponse::success(
            "request-1".to_owned(),
            BackendSnapshot {
                providers: Vec::new(),
                launch_readiness: LaunchReadiness::MissingLaunchProvider,
                overlay_running: false,
                voice_state: VoiceState::Idle,
                shortcut_status: ShortcutStatus::Registering,
                experimental_chat: ExperimentalChatSnapshot {
                    provider_id: ProviderId::ChatGpt,
                    experimental: true,
                    state: ExperimentalChatState::Unavailable,
                    message: None,
                },
            },
        ))
        .expect("serialize response");
        assert!(!json.to_ascii_lowercase().contains("apikey"));
        assert!(!json.contains("secret"));
    }
}
