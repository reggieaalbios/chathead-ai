//! Entirely local voice capture and recognition owned by the native sidecar.

mod capture;
mod config;
mod model;
mod recognizer;
mod service;

pub use capture::{
    AudioCapture, CapturedAudio, VoiceCapture, discover_input_devices, probe_input_device,
};
pub use config::{VoiceConfig, VoicePaths};
pub use model::{ModelFiles, ModelManager, QWEN_MODEL_ID, WHISPER_MODEL_ID, catalog, descriptor};
pub use recognizer::{LocalVoiceRecognizer, VoiceRecognizer, resample_mono};
pub use service::{VoiceEvent, VoiceService, VoiceServiceError};
