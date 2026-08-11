use std::{
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
};

use chathead_core::{VoiceInteractionMode, VoiceModelId, VoiceSubmissionMode};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VoiceConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub input_device_id: Option<String>,
    #[serde(default)]
    pub interaction_mode: VoiceInteractionMode,
    #[serde(default)]
    pub submission_mode: VoiceSubmissionMode,
    #[serde(default)]
    pub selected_model_id: VoiceModelId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VoicePaths {
    pub config_file: PathBuf,
    pub models_dir: PathBuf,
}

impl VoicePaths {
    #[must_use]
    pub fn discover() -> Self {
        let home = env::var_os("HOME").map(PathBuf::from);
        let config_root = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".config")))
            .unwrap_or_else(|| PathBuf::from(".config"));
        let data_root = env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| home.map(|path| path.join(".local/share")))
            .unwrap_or_else(|| PathBuf::from(".local/share"));
        Self {
            config_file: config_root.join("chathead-ai/voice.json"),
            models_dir: data_root.join("chathead-ai/models"),
        }
    }

    pub fn load_config(&self) -> io::Result<VoiceConfig> {
        match fs::read(&self.config_file) {
            Ok(bytes) => {
                let mut value: serde_json::Value = serde_json::from_slice(&bytes)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
                let object = value.as_object_mut().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "voice configuration must be an object",
                    )
                })?;
                let known = object
                    .get("selectedModelId")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|id| {
                        id == VoiceModelId::WhisperTinyInt8.as_str()
                            || id == VoiceModelId::Qwen3Asr06bInt8.as_str()
                    });
                if !known {
                    // Accuracy/default promotion remains benchmark-gated; legacy and unknown IDs
                    // therefore migrate to the established lightweight model for now.
                    object.insert(
                        "selectedModelId".to_owned(),
                        serde_json::Value::String(
                            VoiceModelId::WhisperTinyInt8.as_str().to_owned(),
                        ),
                    );
                }
                serde_json::from_value(value)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(VoiceConfig::default()),
            Err(error) => Err(error),
        }
    }

    pub fn save_config(&self, config: &VoiceConfig) -> io::Result<()> {
        let parent = self
            .config_file
            .parent()
            .ok_or_else(|| io::Error::other("voice configuration path has no parent"))?;
        fs::create_dir_all(parent)?;
        let temporary = self.config_file.with_extension("json.part");
        let mut file = fs::File::create(&temporary)?;
        serde_json::to_writer_pretty(&mut file, config).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(temporary, &self.config_file)
    }

    #[must_use]
    pub fn model_dir(&self, id: VoiceModelId) -> PathBuf {
        self.models_dir.join(id.as_str())
    }

    #[must_use]
    pub fn is_safe_model_path(&self, id: VoiceModelId, path: &Path) -> bool {
        path == self.model_dir(id) && path.starts_with(&self.models_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_removal_is_restricted_to_the_owned_model_directory() {
        let paths = VoicePaths {
            config_file: PathBuf::from("/tmp/chathead-test/config/voice.json"),
            models_dir: PathBuf::from("/tmp/chathead-test/data/models"),
        };
        assert!(paths.is_safe_model_path(
            VoiceModelId::WhisperTinyInt8,
            &paths.model_dir(VoiceModelId::WhisperTinyInt8)
        ));
        assert!(!paths.is_safe_model_path(
            VoiceModelId::WhisperTinyInt8,
            Path::new("/tmp/chathead-test")
        ));
        assert!(!paths.is_safe_model_path(VoiceModelId::WhisperTinyInt8, Path::new("/")));
    }

    #[test]
    fn configuration_round_trips_atomically() {
        let root = std::env::temp_dir().join(format!(
            "chathead-voice-config-{}-{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let paths = VoicePaths {
            config_file: root.join("config/voice.json"),
            models_dir: root.join("models"),
        };
        let expected = VoiceConfig {
            enabled: true,
            input_device_id: Some("PipeWire|microphone".to_owned()),
            interaction_mode: VoiceInteractionMode::Toggle,
            submission_mode: VoiceSubmissionMode::InsertAndSend,
            selected_model_id: VoiceModelId::Qwen3Asr06bInt8,
        };
        paths.save_config(&expected).expect("save configuration");
        assert_eq!(paths.load_config().expect("load configuration"), expected);
        assert!(!paths.config_file.with_extension("json.part").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn legacy_enabled_config_and_unknown_model_ids_migrate_without_losing_settings() {
        let root =
            std::env::temp_dir().join(format!("chathead-voice-migration-{}", std::process::id()));
        let paths = VoicePaths {
            config_file: root.join("voice.json"),
            models_dir: root.join("models"),
        };
        fs::create_dir_all(&root).expect("create root");
        fs::write(
            &paths.config_file,
            br#"{"enabled":true,"interactionMode":"toggle","selectedModelId":"removed-model"}"#,
        )
        .expect("write legacy config");
        let loaded = paths.load_config().expect("migrate config");
        assert!(loaded.enabled);
        assert_eq!(loaded.interaction_mode, VoiceInteractionMode::Toggle);
        assert_eq!(loaded.submission_mode, VoiceSubmissionMode::InsertOnly);
        assert_eq!(loaded.selected_model_id, VoiceModelId::WhisperTinyInt8);
        let _ = fs::remove_dir_all(root);
    }
}
