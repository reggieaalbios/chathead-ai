//! Local-only ASR benchmark for private WAV recordings and reference transcripts.

use std::{error::Error, fs, time::Instant};

use chathead_core::VoiceModelId;
use chathead_voice::{
    CapturedAudio, LocalVoiceRecognizer, ModelManager, VoicePaths, VoiceRecognizer,
};
use sherpa_onnx::Wave;

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let model_id = match args.next().as_deref() {
        Some("sherpa-onnx-whisper-tiny-int8-multilingual-v1") => VoiceModelId::WhisperTinyInt8,
        Some("sherpa-onnx-qwen3-asr-0.6b-int8-2026-03-25") => VoiceModelId::Qwen3Asr06bInt8,
        _ => return Err("usage: voice-benchmark <model-id> <manifest.tsv>\nmanifest rows: /absolute/audio.wav<TAB>reference transcript".into()),
    };
    let manifest_path = args.next().ok_or("missing manifest.tsv path")?;
    if args.next().is_some() {
        return Err("unexpected extra arguments".into());
    }

    let manager = ModelManager::new(VoicePaths::discover());
    if !manager.verify_installed(model_id)? {
        return Err("the selected model is not installed and verified".into());
    }
    let mut recognizer = LocalVoiceRecognizer::default();
    recognizer.load(&manager.files(model_id))?;

    let manifest = fs::read_to_string(manifest_path)?;
    let mut edits = 0_usize;
    let mut reference_words = 0_usize;
    let mut audio_seconds = 0.0_f64;
    let started = Instant::now();
    for (index, line) in manifest.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let (audio_path, reference) = line
            .split_once('\t')
            .ok_or_else(|| format!("manifest line {} is not tab-separated", index + 1))?;
        let wave = Wave::read(audio_path)
            .ok_or_else(|| format!("could not read mono WAV {audio_path}"))?;
        let sample_rate =
            u32::try_from(wave.sample_rate()).map_err(|_| "invalid WAV sample rate")?;
        audio_seconds += wave.samples().len() as f64 / f64::from(sample_rate);
        let transcript = recognizer.transcribe(CapturedAudio {
            samples: wave.samples().to_vec(),
            sample_rate,
            overflowed: false,
            device_lost: false,
        })?;
        let expected = words(reference);
        let actual = words(&transcript);
        edits = edits.saturating_add(edit_distance(&expected, &actual));
        reference_words = reference_words.saturating_add(expected.len());
        println!("{}\t{}", audio_path, transcript);
    }
    if reference_words == 0 || audio_seconds == 0.0 {
        return Err("manifest contained no benchmark speech".into());
    }
    let elapsed = started.elapsed().as_secs_f64();
    println!(
        "WER={:.4}\tRTF={:.4}\twords={}\taudio_seconds={:.2}",
        edits as f64 / reference_words as f64,
        elapsed / audio_seconds,
        reference_words,
        audio_seconds
    );
    Ok(())
}

fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.trim_matches(|character: char| !character.is_alphanumeric())
                .to_lowercase()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

fn edit_distance(expected: &[String], actual: &[String]) -> usize {
    let mut previous: Vec<_> = (0..=actual.len()).collect();
    let mut current = vec![0; actual.len() + 1];
    for (row, expected_word) in expected.iter().enumerate() {
        current[0] = row + 1;
        for (column, actual_word) in actual.iter().enumerate() {
            current[column + 1] = (previous[column + 1] + 1)
                .min(current[column] + 1)
                .min(previous[column] + usize::from(expected_word != actual_word));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[actual.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_error_distance_counts_insertions_deletions_and_substitutions() {
        assert_eq!(
            edit_distance(
                &words("hello filipino world"),
                &words("hello pinoy new world")
            ),
            2
        );
    }
}
