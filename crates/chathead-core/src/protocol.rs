use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const PROTOCOL_VERSION: u16 = 11;

pub const PANEL_ZOOM_LEVELS: [u16; 6] = [80, 90, 100, 110, 125, 150];
pub const PANEL_WIDTH_MIN: u16 = 420;
pub const PANEL_WIDTH_DEFAULT: u16 = 560;
pub const PANEL_WIDTH_MAX: u16 = 960;
pub const PANEL_HEIGHT_MIN: u16 = 460;
pub const PANEL_HEIGHT_DEFAULT: u16 = 460;
pub const PANEL_HEIGHT_MAX: u16 = 800;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct PanelZoom(u16);

impl PanelZoom {
    pub const DEFAULT: Self = Self(100);

    #[must_use]
    pub const fn value(self) -> u16 {
        self.0
    }

    #[must_use]
    pub fn previous(self) -> Self {
        let index = PANEL_ZOOM_LEVELS
            .iter()
            .position(|level| *level == self.0)
            .unwrap_or(2);
        Self(PANEL_ZOOM_LEVELS[index.saturating_sub(1)])
    }

    #[must_use]
    pub fn next(self) -> Self {
        let index = PANEL_ZOOM_LEVELS
            .iter()
            .position(|level| *level == self.0)
            .unwrap_or(2);
        Self(PANEL_ZOOM_LEVELS[(index + 1).min(PANEL_ZOOM_LEVELS.len() - 1)])
    }

    #[must_use]
    pub const fn reset() -> Self {
        Self::DEFAULT
    }
}

impl Default for PanelZoom {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<u16> for PanelZoom {
    type Error = &'static str;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        PANEL_ZOOM_LEVELS
            .contains(&value)
            .then_some(Self(value))
            .ok_or("panel zoom must be one of 80, 90, 100, 110, 125, or 150")
    }
}

impl<'de> Deserialize<'de> for PanelZoom {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::try_from(u16::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PanelSize {
    width: u16,
    height: u16,
}

impl PanelSize {
    pub const DEFAULT: Self = Self {
        width: PANEL_WIDTH_DEFAULT,
        height: PANEL_HEIGHT_DEFAULT,
    };

    pub fn try_new(width: u16, height: u16) -> Result<Self, &'static str> {
        if !(PANEL_WIDTH_MIN..=PANEL_WIDTH_MAX).contains(&width) {
            return Err("panel width must be between 420 and 960");
        }
        if !(PANEL_HEIGHT_MIN..=PANEL_HEIGHT_MAX).contains(&height) {
            return Err("panel height must be between 460 and 800");
        }
        Ok(Self { width, height })
    }

    #[must_use]
    pub const fn width(self) -> u16 {
        self.width
    }

    #[must_use]
    pub const fn height(self) -> u16 {
        self.height
    }
}

impl Default for PanelSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RawPanelSize {
    width: u16,
    height: u16,
}

impl<'de> Deserialize<'de> for PanelSize {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawPanelSize::deserialize(deserializer)?;
        Self::try_new(raw.width, raw.height).map_err(serde::de::Error::custom)
    }
}

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
pub enum LaunchBlocker {
    MissingLaunchProvider,
    DesktopIntegrationRequired,
    DesktopIntegrationUnavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchReadiness {
    pub ready: bool,
    pub blockers: Vec<LaunchBlocker>,
}

impl LaunchReadiness {
    #[must_use]
    pub fn from_blockers(blockers: Vec<LaunchBlocker>) -> Self {
        Self {
            ready: blockers.is_empty(),
            blockers,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopIntegrationKind {
    LayerShell,
    GnomeShell,
    Unsupported,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DesktopIntegrationStatus {
    Ready,
    NotInstalled,
    Disabled,
    Incompatible,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DesktopIntegrationSnapshot {
    pub kind: DesktopIntegrationKind,
    pub status: DesktopIntegrationStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gnome_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl DesktopIntegrationSnapshot {
    #[must_use]
    pub fn layer_shell_ready() -> Self {
        Self {
            kind: DesktopIntegrationKind::LayerShell,
            status: DesktopIntegrationStatus::Ready,
            gnome_version: None,
            message: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VoiceInteractionMode {
    #[default]
    Hold,
    Toggle,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VoiceSubmissionMode {
    #[default]
    InsertOnly,
    InsertAndSend,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VoicePhase {
    #[default]
    Disabled,
    SetupRequired,
    Downloading,
    Loading,
    Ready,
    Listening,
    Transcribing,
    PendingSend,
    Error,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum MicrophoneAccess {
    #[default]
    Unknown,
    Granted,
    Denied,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum VoiceModelState {
    #[default]
    NotInstalled,
    Downloading,
    Installed,
    Invalid,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum VoiceModelId {
    #[default]
    #[serde(rename = "sherpa-onnx-whisper-tiny-int8-multilingual-v1")]
    WhisperTinyInt8,
    #[serde(rename = "sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25")]
    Qwen3Asr06bInt8,
}

impl VoiceModelId {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WhisperTinyInt8 => "sherpa-onnx-whisper-tiny-int8-multilingual-v1",
            Self::Qwen3Asr06bInt8 => "sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceModelSnapshot {
    pub id: VoiceModelId,
    pub name: &'static str,
    pub badges: Vec<&'static str>,
    pub description: &'static str,
    pub languages: Vec<&'static str>,
    pub license: &'static str,
    pub download_size_bytes: u64,
    pub installed_size_bytes: u64,
    pub resource_guidance: &'static str,
    pub state: VoiceModelState,
    pub download_progress_percent: u8,
    pub installed_size_bytes_actual: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceInputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceSnapshot {
    pub enabled: bool,
    pub phase: VoicePhase,
    pub interaction_mode: VoiceInteractionMode,
    pub submission_mode: VoiceSubmissionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_input_device_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_input_device_id: Option<String>,
    pub input_devices: Vec<VoiceInputDevice>,
    pub microphone_access: MicrophoneAccess,
    pub selected_model_id: VoiceModelId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_model_id: Option<VoiceModelId>,
    pub models: Vec<VoiceModelSnapshot>,
    pub microphone_test_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub recoverable: bool,
}

impl Default for VoiceSnapshot {
    fn default() -> Self {
        Self {
            enabled: false,
            phase: VoicePhase::Disabled,
            interaction_mode: VoiceInteractionMode::Hold,
            submission_mode: VoiceSubmissionMode::InsertOnly,
            selected_input_device_id: None,
            default_input_device_id: None,
            input_devices: Vec::new(),
            microphone_access: MicrophoneAccess::Unknown,
            selected_model_id: VoiceModelId::WhisperTinyInt8,
            active_model_id: None,
            models: Vec::new(),
            microphone_test_active: false,
            message: Some(
                "Local voice is off. Enable it in Settings to download the model.".to_owned(),
            ),
            recoverable: true,
        }
    }
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
    pub desktop_integration: DesktopIntegrationSnapshot,
    pub overlay_running: bool,
    pub voice: VoiceSnapshot,
    pub shortcut_status: ShortcutStatus,
    pub panel_shortcut_status: ShortcutStatus,
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
    DesktopIntegrationRequired,
    DesktopIntegrationUnavailable,
    SidecarUnavailable,
    ProtocolMismatch,
    InvalidRequest,
    UnsupportedOperation,
    CodexProtocolError,
    ChatUnavailable,
    ChatBusy,
    VoiceUnavailable,
    VoiceSetupFailed,
    VoiceInvalidState,
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
                launch_readiness: LaunchReadiness::from_blockers(vec![
                    LaunchBlocker::MissingLaunchProvider,
                ]),
                desktop_integration: DesktopIntegrationSnapshot::layer_shell_ready(),
                overlay_running: false,
                voice: VoiceSnapshot::default(),
                shortcut_status: ShortcutStatus::Registering,
                panel_shortcut_status: ShortcutStatus::Registering,
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

    #[test]
    fn voice_snapshot_uses_the_typescript_camel_case_contract() {
        let json = serde_json::to_value(VoiceSnapshot::default()).expect("serialize voice");
        assert_eq!(json["phase"], "disabled");
        assert_eq!(json["interactionMode"], "hold");
        assert_eq!(json["submissionMode"], "insertOnly");
        assert_eq!(
            json["selectedModelId"],
            "sherpa-onnx-whisper-tiny-int8-multilingual-v1"
        );
        assert!(json["models"].is_array());
        assert!(json["inputDevices"].is_array());
    }

    #[test]
    fn panel_zoom_transitions_are_bounded_and_resettable() {
        let levels = PANEL_ZOOM_LEVELS.map(|level| PanelZoom::try_from(level).expect("valid zoom"));
        assert_eq!(levels[0].previous(), levels[0]);
        assert_eq!(levels[5].next(), levels[5]);
        for pair in levels.windows(2) {
            assert_eq!(pair[0].next(), pair[1]);
            assert_eq!(pair[1].previous(), pair[0]);
        }
        assert_eq!(PanelZoom::reset(), PanelZoom::DEFAULT);
    }

    #[test]
    fn panel_zoom_rejects_arbitrary_protocol_values() {
        assert!(PanelZoom::try_from(95).is_err());
        assert!(serde_json::from_str::<PanelZoom>("95").is_err());
        assert_eq!(
            serde_json::from_str::<PanelZoom>("125")
                .expect("valid zoom")
                .value(),
            125
        );
    }

    #[test]
    fn panel_zoom_changed_event_uses_the_current_protocol_contract() {
        let event = IpcEvent {
            protocol_version: PROTOCOL_VERSION,
            event: "panelZoomChanged",
            payload: PanelZoom::try_from(125).expect("valid zoom"),
        };
        let json = serde_json::to_value(event).expect("serialize event");
        assert_eq!(json["protocolVersion"], 11);
        assert_eq!(json["event"], "panelZoomChanged");
        assert_eq!(json["payload"], 125);
    }

    #[test]
    fn panel_size_validates_every_boundary() {
        assert_eq!(
            PanelSize::default(),
            PanelSize::try_new(560, 460).expect("default")
        );
        assert!(PanelSize::try_new(420, 460).is_ok());
        assert!(PanelSize::try_new(960, 800).is_ok());
        assert!(PanelSize::try_new(419, 460).is_err());
        assert!(PanelSize::try_new(961, 460).is_err());
        assert!(PanelSize::try_new(560, 459).is_err());
        assert!(PanelSize::try_new(560, 801).is_err());
    }

    #[test]
    fn panel_size_rejects_invalid_protocol_payloads() {
        assert!(serde_json::from_str::<PanelSize>(r#"{"width":419,"height":460}"#).is_err());
        assert!(
            serde_json::from_str::<PanelSize>(r#"{"width":560,"height":460,"extra":1}"#).is_err()
        );
        let size = serde_json::from_str::<PanelSize>(r#"{"width":720,"height":600}"#)
            .expect("valid panel size");
        assert_eq!((size.width(), size.height()), (720, 600));
    }

    #[test]
    fn panel_size_changed_event_uses_the_current_protocol_contract() {
        let event = IpcEvent {
            protocol_version: PROTOCOL_VERSION,
            event: "panelSizeChanged",
            payload: PanelSize::try_new(720, 600).expect("valid panel size"),
        };
        let json = serde_json::to_value(event).expect("serialize event");
        assert_eq!(json["protocolVersion"], 11);
        assert_eq!(json["event"], "panelSizeChanged");
        assert_eq!(json["payload"]["width"], 720);
        assert_eq!(json["payload"]["height"], 600);
    }
}
