use std::{collections::HashMap, env};

use crate::{
    AuthMethod, AuthenticationState, BackendSnapshot, ErrorCode, ExperimentalChatSnapshot,
    ExperimentalChatState, IpcError, LaunchReadiness, ProviderId, ProviderKind, ProviderSnapshot,
    ProviderStatus, ShortcutStatus, VoiceState,
};

const KEYRING_SERVICE: &str = "io.github.chathead_ai.ChatHead";

#[derive(Clone, Copy)]
struct ProviderConfig {
    id: ProviderId,
    name: &'static str,
    description: &'static str,
    kind: ProviderKind,
    api_key_label: &'static str,
    env_var: Option<&'static str>,
}

const PROVIDERS: [ProviderConfig; 5] = [
    ProviderConfig {
        id: ProviderId::ChatGpt,
        name: "ChatGPT",
        description: "OpenAI-compatible LLM provider with API-key or Codex subscription authentication.",
        kind: ProviderKind::LargeLanguageModel,
        api_key_label: "OpenAI API key",
        env_var: Some("OPENAI_API_KEY"),
    },
    ProviderConfig {
        id: ProviderId::Claude,
        name: "Claude",
        description: "Anthropic-backed LLM provider using API-key authentication.",
        kind: ProviderKind::LargeLanguageModel,
        api_key_label: "Anthropic API key",
        env_var: Some("ANTHROPIC_API_KEY"),
    },
    ProviderConfig {
        id: ProviderId::Gemini,
        name: "Gemini",
        description: "Google Gemini LLM provider using API-key authentication.",
        kind: ProviderKind::LargeLanguageModel,
        api_key_label: "Gemini API key",
        env_var: Some("GEMINI_API_KEY"),
    },
    ProviderConfig {
        id: ProviderId::Grok,
        name: "Grok",
        description: "xAI LLM provider using API-key authentication.",
        kind: ProviderKind::LargeLanguageModel,
        api_key_label: "xAI API key",
        env_var: Some("XAI_API_KEY"),
    },
    ProviderConfig {
        id: ProviderId::Zep,
        name: "Zep",
        description: "Memory and context provider. Zep alone cannot launch ChatHead.",
        kind: ProviderKind::MemoryContext,
        api_key_label: "Zep API key",
        env_var: Some("ZEP_API_KEY"),
    },
];

#[derive(Debug, thiserror::Error)]
pub enum BackendError {
    #[error("API key did not pass basic provider format validation")]
    InvalidApiKey,
    #[error("credential store unavailable: {0}")]
    CredentialStoreUnavailable(String),
    #[error("Codex CLI was not found")]
    CodexNotFound,
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    #[error("provider does not support this operation")]
    UnsupportedOperation,
}

impl From<BackendError> for IpcError {
    fn from(error: BackendError) -> Self {
        let code = match error {
            BackendError::InvalidApiKey => ErrorCode::InvalidApiKey,
            BackendError::CredentialStoreUnavailable(_) => ErrorCode::CredentialStoreUnavailable,
            BackendError::CodexNotFound => ErrorCode::CodexNotFound,
            BackendError::AuthFailed(_) => ErrorCode::AuthFailed,
            BackendError::UnsupportedOperation => ErrorCode::UnsupportedOperation,
        };
        Self {
            code,
            message: error.to_string(),
            recoverable: true,
        }
    }
}

pub struct Backend {
    statuses: HashMap<ProviderId, ProviderStatus>,
    overlay_running: bool,
    voice_state: VoiceState,
    shortcut_status: ShortcutStatus,
    experimental_chat: ExperimentalChatSnapshot,
}

impl Default for Backend {
    fn default() -> Self {
        Self::new()
    }
}

impl Backend {
    #[must_use]
    pub fn new() -> Self {
        let statuses = PROVIDERS
            .iter()
            .map(|provider| {
                let configured_by_env = provider
                    .env_var
                    .is_some_and(|name| env::var_os(name).is_some());
                let configured_by_keyring = stored_api_key(*provider).ok().flatten().is_some();
                let status = initial_provider_status(
                    provider.id,
                    configured_by_env || configured_by_keyring,
                    false,
                );
                (provider.id, status)
            })
            .collect();
        Self {
            statuses,
            overlay_running: false,
            voice_state: VoiceState::Idle,
            shortcut_status: ShortcutStatus::Registering,
            experimental_chat: ExperimentalChatSnapshot {
                provider_id: ProviderId::ChatGpt,
                experimental: true,
                state: ExperimentalChatState::Probing,
                message: Some("Checking ChatGPT subscription…".to_owned()),
            },
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> BackendSnapshot {
        let providers = PROVIDERS
            .iter()
            .map(|provider| ProviderSnapshot {
                id: provider.id,
                name: provider.name,
                description: provider.description,
                kind: provider.kind,
                api_key_label: provider.api_key_label,
                supports_subscription: provider.id == ProviderId::ChatGpt,
                status: self
                    .statuses
                    .get(&provider.id)
                    .cloned()
                    .unwrap_or(ProviderStatus::Unconfigured),
            })
            .collect();
        BackendSnapshot {
            providers,
            launch_readiness: self.launch_readiness(),
            overlay_running: self.overlay_running,
            voice_state: self.voice_state,
            shortcut_status: self.shortcut_status.clone(),
            experimental_chat: self.experimental_chat.clone(),
        }
    }

    pub fn save_api_key(
        &mut self,
        provider_id: ProviderId,
        value: &str,
    ) -> Result<(), BackendError> {
        validate_api_key(provider_id, value.trim())?;
        let provider = provider(provider_id);
        let entry = keyring::Entry::new(KEYRING_SERVICE, keyring_account(provider_id))
            .map_err(|error| BackendError::CredentialStoreUnavailable(error.to_string()))?;
        entry
            .set_password(value.trim())
            .map_err(|error| BackendError::CredentialStoreUnavailable(error.to_string()))?;
        self.statuses.insert(
            provider.id,
            ProviderStatus::Authenticated {
                method: AuthMethod::ApiKey,
            },
        );
        Ok(())
    }

    pub fn begin_subscription_login(
        &mut self,
        provider_id: ProviderId,
    ) -> Result<(), BackendError> {
        if provider_id != ProviderId::ChatGpt {
            return Err(BackendError::UnsupportedOperation);
        }
        self.statuses
            .insert(provider_id, ProviderStatus::Authenticating);
        self.experimental_chat.state = ExperimentalChatState::Authenticating;
        self.experimental_chat.message = Some("Complete sign-in in your browser.".to_owned());
        Ok(())
    }

    pub fn disconnect_provider(&mut self, provider_id: ProviderId) -> Result<(), BackendError> {
        if provider_id == ProviderId::ChatGpt
            && matches!(
                self.statuses.get(&provider_id),
                Some(ProviderStatus::Authenticated {
                    method: AuthMethod::SubscriptionLogin
                })
            )
        {
            self.set_subscription_authentication(AuthenticationState::SignedOut);
        } else {
            let entry = keyring::Entry::new(KEYRING_SERVICE, keyring_account(provider_id))
                .map_err(|error| BackendError::CredentialStoreUnavailable(error.to_string()))?;
            match entry.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(error) => {
                    return Err(BackendError::CredentialStoreUnavailable(error.to_string()));
                }
            }
        }
        self.statuses
            .insert(provider_id, ProviderStatus::Unconfigured);
        Ok(())
    }

    #[must_use]
    pub fn launch_readiness(&self) -> LaunchReadiness {
        let ready = PROVIDERS
            .iter()
            .filter(|provider| provider.kind == ProviderKind::LargeLanguageModel)
            .any(|provider| {
                matches!(
                    self.statuses.get(&provider.id),
                    Some(ProviderStatus::Authenticated { .. })
                )
            });
        if ready {
            LaunchReadiness::Ready
        } else {
            LaunchReadiness::MissingLaunchProvider
        }
    }

    pub fn set_overlay_running(&mut self, running: bool) {
        self.overlay_running = running;
    }

    pub fn set_codex_availability(&mut self, available: bool, message: Option<String>) {
        if !available {
            self.experimental_chat.state = ExperimentalChatState::Unavailable;
            self.experimental_chat.message = message;
        }
    }

    pub fn set_subscription_authentication(&mut self, authentication: AuthenticationState) {
        match authentication {
            AuthenticationState::ChatGpt => {
                self.statuses.insert(
                    ProviderId::ChatGpt,
                    ProviderStatus::Authenticated {
                        method: AuthMethod::SubscriptionLogin,
                    },
                );
                self.experimental_chat.state = ExperimentalChatState::Ready;
                self.experimental_chat.message = None;
            }
            AuthenticationState::ApiKey | AuthenticationState::SignedOut => {
                if !matches!(
                    self.statuses.get(&ProviderId::ChatGpt),
                    Some(ProviderStatus::Authenticated {
                        method: AuthMethod::ApiKey
                    })
                ) {
                    self.statuses
                        .insert(ProviderId::ChatGpt, ProviderStatus::Unconfigured);
                }
                self.experimental_chat.state = ExperimentalChatState::Unavailable;
                self.experimental_chat.message = Some(
                    "Connect a ChatGPT subscription in Settings to use Experimental chat."
                        .to_owned(),
                );
            }
        }
    }

    pub fn set_codex_error(&mut self, message: String) {
        self.experimental_chat.state = ExperimentalChatState::Error;
        self.experimental_chat.message = Some(message);
    }
}

fn initial_provider_status(
    provider_id: ProviderId,
    has_api_key: bool,
    has_subscription: bool,
) -> ProviderStatus {
    if has_api_key {
        return ProviderStatus::Authenticated {
            method: AuthMethod::ApiKey,
        };
    }
    if provider_id != ProviderId::ChatGpt {
        return ProviderStatus::Unconfigured;
    }
    if has_subscription {
        ProviderStatus::Authenticated {
            method: AuthMethod::SubscriptionLogin,
        }
    } else {
        ProviderStatus::Unconfigured
    }
}

fn provider(id: ProviderId) -> &'static ProviderConfig {
    PROVIDERS
        .iter()
        .find(|provider| provider.id == id)
        .expect("all ProviderId variants have configuration")
}

fn stored_api_key(provider: ProviderConfig) -> Result<Option<String>, BackendError> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, keyring_account(provider.id))
        .map_err(|error| BackendError::CredentialStoreUnavailable(error.to_string()))?;
    match entry.get_password() {
        Ok(secret) if !secret.trim().is_empty() => Ok(Some(secret)),
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(BackendError::CredentialStoreUnavailable(error.to_string())),
    }
}

fn keyring_account(id: ProviderId) -> &'static str {
    match id {
        ProviderId::ChatGpt => "openai-api-key",
        ProviderId::Claude => "anthropic-api-key",
        ProviderId::Gemini => "gemini-api-key",
        ProviderId::Grok => "xai-api-key",
        ProviderId::Zep => "zep-api-key",
    }
}

fn validate_api_key(provider_id: ProviderId, key: &str) -> Result<(), BackendError> {
    let valid = match provider_id {
        ProviderId::ChatGpt => key.starts_with("sk-") && key.len() >= 20,
        ProviderId::Claude => key.starts_with("sk-ant-") && key.len() >= 24,
        ProviderId::Gemini | ProviderId::Zep => {
            key.len() >= 20 && key.chars().all(|ch| ch.is_ascii_graphic())
        }
        ProviderId::Grok => (key.starts_with("xai-") || key.starts_with("sk-")) && key.len() >= 20,
    };
    valid.then_some(()).ok_or(BackendError::InvalidApiKey)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_provider_specific_key_shapes() {
        assert!(validate_api_key(ProviderId::ChatGpt, "sk-12345678901234567").is_ok());
        assert!(validate_api_key(ProviderId::Claude, "sk-short").is_err());
    }

    #[test]
    fn restores_chatgpt_subscription_status_at_startup() {
        assert_eq!(
            initial_provider_status(ProviderId::ChatGpt, false, true,),
            ProviderStatus::Authenticated {
                method: AuthMethod::SubscriptionLogin,
            }
        );
    }

    #[test]
    fn stored_api_key_takes_precedence_over_subscription_status() {
        assert_eq!(
            initial_provider_status(ProviderId::ChatGpt, true, true,),
            ProviderStatus::Authenticated {
                method: AuthMethod::ApiKey,
            }
        );
    }

    #[test]
    fn zep_is_not_a_launch_provider() {
        let mut backend = Backend {
            statuses: HashMap::new(),
            overlay_running: false,
            voice_state: VoiceState::Idle,
            shortcut_status: ShortcutStatus::Registering,
            experimental_chat: ExperimentalChatSnapshot {
                provider_id: ProviderId::ChatGpt,
                experimental: true,
                state: ExperimentalChatState::Unavailable,
                message: None,
            },
        };
        backend.statuses.insert(
            ProviderId::Zep,
            ProviderStatus::Authenticated {
                method: AuthMethod::ApiKey,
            },
        );
        assert_eq!(
            backend.launch_readiness(),
            LaunchReadiness::MissingLaunchProvider
        );
    }

    #[test]
    fn authenticated_llm_enables_launch() {
        let mut backend = Backend {
            statuses: HashMap::new(),
            overlay_running: false,
            voice_state: VoiceState::Idle,
            shortcut_status: ShortcutStatus::Registering,
            experimental_chat: ExperimentalChatSnapshot {
                provider_id: ProviderId::ChatGpt,
                experimental: true,
                state: ExperimentalChatState::Unavailable,
                message: None,
            },
        };
        backend.statuses.insert(
            ProviderId::Claude,
            ProviderStatus::Authenticated {
                method: AuthMethod::ApiKey,
            },
        );
        assert_eq!(backend.launch_readiness(), LaunchReadiness::Ready);
    }
}
