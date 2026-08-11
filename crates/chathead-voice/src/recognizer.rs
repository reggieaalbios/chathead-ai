use std::sync::Arc;

use rubato::{Fft, FixedSync, Resampler, audioadapter_buffers::direct::InterleavedSlice};
use sherpa_onnx::{
    OfflineQwen3ASRModelConfig, OfflineRecognizer, OfflineRecognizerConfig,
    OfflineWhisperModelConfig, SileroVadModelConfig, VadModelConfig, VoiceActivityDetector,
};

use crate::{CapturedAudio, ModelFiles, model::RecognizerBackend};

const TARGET_SAMPLE_RATE: u32 = 16_000;
const MAX_SPEECH_SAMPLES: usize = 30 * TARGET_SAMPLE_RATE as usize;
const VAD_WINDOW_SIZE: usize = 512;
const VAD_CONTEXT_SAMPLES: usize = TARGET_SAMPLE_RATE as usize / 4;

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum RecognitionError {
    #[error("voice model is not loaded")]
    NotLoaded,
    #[error("captured audio exceeded the 30-second limit")]
    Overflow,
    #[error("the microphone disconnected while recording")]
    DeviceLost,
    #[error("audio resampling failed")]
    Resampling,
    #[error("voice activity detection could not be loaded")]
    VadLoad,
    #[error("no speech was detected")]
    NoSpeech,
    #[error("local transcription failed")]
    Transcription,
    #[error("the transcription was empty")]
    EmptyTranscript,
}

pub trait VoiceRecognizer {
    fn load(&mut self, files: &ModelFiles) -> Result<(), RecognitionError>;
    fn unload(&mut self);
    fn transcribe(&self, audio: CapturedAudio) -> Result<String, RecognitionError>;
    fn is_loaded(&self) -> bool;
}

#[derive(Clone, Default)]
pub struct LocalVoiceRecognizer {
    recognizer: Option<Arc<OfflineRecognizer>>,
    silero_vad_path: Option<String>,
}

impl VoiceRecognizer for LocalVoiceRecognizer {
    fn load(&mut self, files: &ModelFiles) -> Result<(), RecognitionError> {
        let mut config = recognizer_config(files);
        config.model_config.provider = Some("cpu".to_owned());
        config.model_config.num_threads = inference_threads();
        config.model_config.debug = false;
        let recognizer =
            OfflineRecognizer::create(&config).ok_or(RecognitionError::Transcription)?;
        self.recognizer = Some(Arc::new(recognizer));
        self.silero_vad_path = Some(files.silero_vad.to_string_lossy().into_owned());
        Ok(())
    }

    fn unload(&mut self) {
        self.recognizer = None;
        self.silero_vad_path = None;
    }

    fn transcribe(&self, audio: CapturedAudio) -> Result<String, RecognitionError> {
        if audio.overflowed {
            return Err(RecognitionError::Overflow);
        }
        if audio.device_lost {
            return Err(RecognitionError::DeviceLost);
        }
        let recognizer = self
            .recognizer
            .as_ref()
            .ok_or(RecognitionError::NotLoaded)?;
        let vad_path = self
            .silero_vad_path
            .as_ref()
            .ok_or(RecognitionError::NotLoaded)?;
        let resampled = resample_mono(&audio.samples, audio.sample_rate, TARGET_SAMPLE_RATE)?;
        if resampled.len() > MAX_SPEECH_SAMPLES {
            return Err(RecognitionError::Overflow);
        }
        let speech = trim_outer_silence(&resampled, vad_path)?;
        let stream = recognizer.create_stream();
        stream.accept_waveform(TARGET_SAMPLE_RATE as i32, &speech);
        recognizer.decode(&stream);
        let result = stream.get_result().ok_or(RecognitionError::Transcription)?;
        let text = result.text.trim();
        if text.is_empty() {
            Err(RecognitionError::EmptyTranscript)
        } else {
            Ok(text.to_owned())
        }
    }

    fn is_loaded(&self) -> bool {
        self.recognizer.is_some()
    }
}

fn recognizer_config(files: &ModelFiles) -> OfflineRecognizerConfig {
    let mut config = OfflineRecognizerConfig::default();
    match files.backend {
        RecognizerBackend::Whisper => {
            config.model_config.whisper = OfflineWhisperModelConfig {
                encoder: Some(
                    files
                        .root
                        .join("tiny-encoder.int8.onnx")
                        .to_string_lossy()
                        .into_owned(),
                ),
                decoder: Some(
                    files
                        .root
                        .join("tiny-decoder.int8.onnx")
                        .to_string_lossy()
                        .into_owned(),
                ),
                language: Some(String::new()),
                task: Some("transcribe".to_owned()),
                tail_paddings: 0,
                enable_token_timestamps: false,
                enable_segment_timestamps: false,
            };
            config.model_config.tokens = Some(
                files
                    .root
                    .join("tiny-tokens.txt")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        RecognizerBackend::Qwen3Asr => {
            config.model_config.qwen3_asr = OfflineQwen3ASRModelConfig {
                conv_frontend: Some(
                    files
                        .root
                        .join("conv_frontend.onnx")
                        .to_string_lossy()
                        .into_owned(),
                ),
                encoder: Some(
                    files
                        .root
                        .join("encoder.int8.onnx")
                        .to_string_lossy()
                        .into_owned(),
                ),
                decoder: Some(
                    files
                        .root
                        .join("decoder.int8.onnx")
                        .to_string_lossy()
                        .into_owned(),
                ),
                tokenizer: Some(files.root.join("tokenizer").to_string_lossy().into_owned()),
                max_new_tokens: 512,
                ..OfflineQwen3ASRModelConfig::default()
            };
        }
    }
    config
}

fn trim_outer_silence(samples: &[f32], model: &str) -> Result<Vec<f32>, RecognitionError> {
    let silero_vad = SileroVadModelConfig {
        model: Some(model.to_owned()),
        threshold: 0.5,
        min_silence_duration: 0.25,
        min_speech_duration: 0.2,
        max_speech_duration: 30.0,
        window_size: VAD_WINDOW_SIZE as i32,
    };
    let config = VadModelConfig {
        silero_vad,
        ten_vad: Default::default(),
        sample_rate: TARGET_SAMPLE_RATE as i32,
        num_threads: 1,
        provider: Some("cpu".to_owned()),
        debug: false,
    };
    let vad = VoiceActivityDetector::create(&config, 30.0).ok_or(RecognitionError::VadLoad)?;
    let mut first = None;
    let mut last = None;
    for chunk in samples.chunks(VAD_WINDOW_SIZE) {
        vad.accept_waveform(chunk);
        drain_vad_bounds(&vad, &mut first, &mut last);
    }
    vad.flush();
    drain_vad_bounds(&vad, &mut first, &mut last);
    let (start, end) =
        context_bounds(first, last, samples.len()).ok_or(RecognitionError::NoSpeech)?;
    Ok(samples[start..end].to_vec())
}

fn drain_vad_bounds(
    vad: &VoiceActivityDetector,
    first: &mut Option<usize>,
    last: &mut Option<usize>,
) {
    while let Some(segment) = vad.front() {
        let start = usize::try_from(segment.start()).unwrap_or(0);
        let end = start.saturating_add(segment.samples().len());
        first.get_or_insert(start);
        *last = Some(last.map_or(end, |current| current.max(end)));
        vad.pop();
    }
}

fn context_bounds(
    first: Option<usize>,
    last: Option<usize>,
    total: usize,
) -> Option<(usize, usize)> {
    let first = first?;
    let last = last?;
    Some((
        first.saturating_sub(VAD_CONTEXT_SAMPLES),
        last.saturating_add(VAD_CONTEXT_SAMPLES).min(total),
    ))
}

pub fn resample_mono(
    input: &[f32],
    input_rate: u32,
    output_rate: u32,
) -> Result<Vec<f32>, RecognitionError> {
    if input.is_empty() || input_rate == 0 || output_rate == 0 {
        return Ok(Vec::new());
    }
    if input_rate == output_rate {
        return Ok(input.to_vec());
    }
    let input_rate = usize::try_from(input_rate).map_err(|_| RecognitionError::Resampling)?;
    let output_rate = usize::try_from(output_rate).map_err(|_| RecognitionError::Resampling)?;
    let mut resampler = Fft::<f32>::new(input_rate, output_rate, 1_024, 1, FixedSync::Both)
        .map_err(|_| RecognitionError::Resampling)?;
    let adapter =
        InterleavedSlice::new(input, 1, input.len()).map_err(|_| RecognitionError::Resampling)?;
    let output = resampler
        .process_all(&adapter, input.len(), None)
        .map_err(|_| RecognitionError::Resampling)?;
    Ok(output.take_data())
}

fn inference_threads() -> i32 {
    let available = std::thread::available_parallelism().map_or(1, usize::from);
    i32::try_from(available.saturating_sub(1).clamp(1, 4)).unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chathead_core::VoiceModelId;

    fn files(backend: RecognizerBackend) -> ModelFiles {
        ModelFiles {
            id: if backend == RecognizerBackend::Whisper {
                VoiceModelId::WhisperTinyInt8
            } else {
                VoiceModelId::Qwen3Asr06bInt8
            },
            backend,
            root: "/models/test".into(),
            silero_vad: "/models/test/silero_vad.onnx".into(),
        }
    }

    #[test]
    fn configures_both_supported_recognizer_backends() {
        let whisper = recognizer_config(&files(RecognizerBackend::Whisper));
        assert!(
            whisper
                .model_config
                .whisper
                .encoder
                .as_deref()
                .unwrap_or_default()
                .ends_with("tiny-encoder.int8.onnx")
        );
        let qwen = recognizer_config(&files(RecognizerBackend::Qwen3Asr));
        assert!(
            qwen.model_config
                .qwen3_asr
                .tokenizer
                .as_deref()
                .unwrap_or_default()
                .ends_with("tokenizer")
        );
        assert_eq!(qwen.model_config.qwen3_asr.max_new_tokens, 512);
    }

    #[test]
    fn resampling_preserves_duration() {
        let input: Vec<_> = (0..48_000)
            .map(|index| (index as f32 * 0.01).sin())
            .collect();
        let output = resample_mono(&input, 48_000, 16_000).expect("resample");
        assert!(output.len().abs_diff(16_000) <= 1);
    }

    #[test]
    fn band_limited_downsampling_rejects_aliases() {
        let input: Vec<_> = (0..48_000)
            .map(|index| (2.0 * std::f32::consts::PI * 12_000.0 * index as f32 / 48_000.0).sin())
            .collect();
        let output = resample_mono(&input, 48_000, 16_000).expect("resample");
        let rms =
            (output.iter().map(|sample| sample * sample).sum::<f32>() / output.len() as f32).sqrt();
        assert!(rms < 0.1, "out-of-band RMS was {rms}");
    }

    #[test]
    fn outer_trim_preserves_internal_pauses_and_context() {
        let total = 20_000;
        assert_eq!(
            context_bounds(Some(5_000), Some(12_000), total),
            Some((1_000, 16_000))
        );
    }

    #[test]
    fn empty_and_invalid_rates_are_safe() {
        assert!(
            resample_mono(&[], 48_000, 16_000)
                .expect("empty")
                .is_empty()
        );
        assert!(
            resample_mono(&[1.0], 0, 16_000)
                .expect("invalid rate")
                .is_empty()
        );
    }
}
