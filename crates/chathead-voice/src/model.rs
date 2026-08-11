use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use bzip2::read::BzDecoder;
use chathead_core::{VoiceModelId, VoiceModelSnapshot, VoiceModelState};
use sha2::{Digest, Sha256};

use crate::VoicePaths;

pub const WHISPER_MODEL_ID: VoiceModelId = VoiceModelId::WhisperTinyInt8;
pub const QWEN_MODEL_ID: VoiceModelId = VoiceModelId::Qwen3Asr06bInt8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecognizerBackend {
    Whisper,
    Qwen3Asr,
}

#[derive(Clone, Copy, Debug)]
pub struct ModelDescriptor {
    pub id: VoiceModelId,
    pub name: &'static str,
    pub badges: &'static [&'static str],
    pub description: &'static str,
    pub languages: &'static [&'static str],
    pub license: &'static str,
    pub download_size_bytes: u64,
    pub installed_size_bytes: u64,
    pub resource_guidance: &'static str,
    pub backend: RecognizerBackend,
    archive: Artifact,
    required_files: &'static [RequiredFile],
}

#[derive(Clone, Copy, Debug)]
struct Artifact {
    file_name: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

#[derive(Clone, Copy, Debug)]
struct RequiredFile {
    archive_path: Option<&'static str>,
    installed_path: &'static str,
    size: u64,
    sha256: &'static str,
}

const SILERO_MODEL: Artifact = Artifact {
    file_name: "silero_vad.onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/silero_vad.onnx",
    size: 643_854,
    sha256: "9e2449e1087496d8d4caba907f23e0bd3f78d91fa552479bb9c23ac09cbb1fd6",
};

const WHISPER_ARCHIVE: Artifact = Artifact {
    file_name: "sherpa-onnx-whisper-tiny.tar.bz2",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-whisper-tiny.tar.bz2",
    size: 116_204_861,
    sha256: "c46116994e539aa165266d96b325252728429c12535eb9d8b6a2b10f129e66b1",
};

const QWEN_ARCHIVE: Artifact = Artifact {
    file_name: "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2",
    size: 878_702_423,
    sha256: "393f8a14e2f5fb96746aaab342997a40641001fbd5bf9592a080a8329178ee96",
};

const WHISPER_FILES: [RequiredFile; 4] = [
    RequiredFile {
        archive_path: Some("sherpa-onnx-whisper-tiny/tiny-encoder.int8.onnx"),
        installed_path: "tiny-encoder.int8.onnx",
        size: 12_937_772,
        sha256: "d24fb083ae3b1041fc24e97971d60e280c9342201fbb67b0ab428a8b4a51a434",
    },
    RequiredFile {
        archive_path: Some("sherpa-onnx-whisper-tiny/tiny-decoder.int8.onnx"),
        installed_path: "tiny-decoder.int8.onnx",
        size: 89_855_401,
        sha256: "d2fece8dd42771f1df975c6c0445770d0c292bf7547c2cae04a6c0cc57540925",
    },
    RequiredFile {
        archive_path: Some("sherpa-onnx-whisper-tiny/tiny-tokens.txt"),
        installed_path: "tiny-tokens.txt",
        size: 816_730,
        sha256: "b34b360dbb493e781e479794586d661700670d65564001f23024971d1f2fa126",
    },
    RequiredFile {
        archive_path: None,
        installed_path: "silero_vad.onnx",
        size: SILERO_MODEL.size,
        sha256: SILERO_MODEL.sha256,
    },
];

#[cfg(test)]
const QWEN_PREFIX: &str = "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25";
const QWEN_FILES: [RequiredFile; 7] = [
    RequiredFile {
        archive_path: Some("sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/conv_frontend.onnx"),
        installed_path: "conv_frontend.onnx",
        size: 44_148_281,
        sha256: "d22dc4423e0940e49884e903d2ea2f7e5567c14fc1aed97e4e26d6b8f208ef9e",
    },
    RequiredFile {
        archive_path: Some("sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/encoder.int8.onnx"),
        installed_path: "encoder.int8.onnx",
        size: 182_491_662,
        sha256: "60748d3e6744a57c9c91e1b17424a6c2990567e8adceb0783940c03ed98fa9d9",
    },
    RequiredFile {
        archive_path: Some("sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/decoder.int8.onnx"),
        installed_path: "decoder.int8.onnx",
        size: 755_914_231,
        sha256: "4f6885be5959ae26af3089d38ee7972c5fafbeeb1cf8d5e76eab6d8b61ca5771",
    },
    RequiredFile {
        archive_path: Some("sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/tokenizer/merges.txt"),
        installed_path: "tokenizer/merges.txt",
        size: 1_671_853,
        sha256: "8831e4f1a044471340f7c0a83d7bd71306a5b867e95fd870f74d0c5308a904d5",
    },
    RequiredFile {
        archive_path: Some(
            "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/tokenizer/tokenizer_config.json",
        ),
        installed_path: "tokenizer/tokenizer_config.json",
        size: 12_487,
        sha256: "4942d005604266809309cabc9f4e9cb89ce855d59b14681fdc0e1cc62ea26c4c",
    },
    RequiredFile {
        archive_path: Some("sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25/tokenizer/vocab.json"),
        installed_path: "tokenizer/vocab.json",
        size: 2_776_833,
        sha256: "ca10d7e9fb3ed18575dd1e277a2579c16d108e32f27439684afa0e10b1440910",
    },
    RequiredFile {
        archive_path: None,
        installed_path: "silero_vad.onnx",
        size: SILERO_MODEL.size,
        sha256: SILERO_MODEL.sha256,
    },
];

const CATALOG: [ModelDescriptor; 2] = [
    ModelDescriptor {
        id: WHISPER_MODEL_ID,
        name: "Whisper Tiny multilingual INT8",
        badges: &["Lightweight", "Fastest"],
        description: "Lower resource use for older hardware",
        languages: &["English", "Filipino", "Multilingual"],
        license: "MIT (model); Apache-2.0 (sherpa-onnx); MIT (Silero VAD)",
        download_size_bytes: WHISPER_ARCHIVE.size + SILERO_MODEL.size,
        installed_size_bytes: 104_253_757,
        resource_guidance: "Best for older or resource-constrained hardware",
        backend: RecognizerBackend::Whisper,
        archive: WHISPER_ARCHIVE,
        required_files: &WHISPER_FILES,
    },
    ModelDescriptor {
        id: QWEN_MODEL_ID,
        name: "Qwen3-ASR 0.6B INT8",
        // The planned accuracy labels remain intentionally gated on the benchmark suite.
        badges: &["Benchmark pending"],
        description: "English, Filipino, and code-switching candidate",
        languages: &["English", "Filipino"],
        license: "Apache-2.0 (Qwen3-ASR and sherpa-onnx); MIT (Silero VAD)",
        download_size_bytes: QWEN_ARCHIVE.size + SILERO_MODEL.size,
        installed_size_bytes: 987_659_201,
        resource_guidance: "Higher CPU and RAM use; target four cores and 8 GB RAM",
        backend: RecognizerBackend::Qwen3Asr,
        archive: QWEN_ARCHIVE,
        required_files: &QWEN_FILES,
    },
];

#[must_use]
pub const fn catalog() -> &'static [ModelDescriptor] {
    &CATALOG
}

#[must_use]
pub fn descriptor(id: VoiceModelId) -> &'static ModelDescriptor {
    CATALOG
        .iter()
        .find(|model| model.id == id)
        .unwrap_or(&CATALOG[0])
}

#[derive(Clone, Debug)]
pub struct ModelFiles {
    pub id: VoiceModelId,
    pub backend: RecognizerBackend,
    pub root: PathBuf,
    pub silero_vad: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model storage failed: {0}")]
    Io(#[from] io::Error),
    #[error("model download failed: {0}")]
    Download(String),
    #[error("model download was canceled")]
    Canceled,
    #[error("model artifact {name} has the wrong size or SHA-256")]
    Integrity { name: String },
    #[error("model archive did not contain required file {0}")]
    MissingArtifact(&'static str),
    #[error("refused to access a path outside ChatHead's model directory")]
    UnsafePath,
}

#[derive(Clone, Debug)]
pub struct ModelManager {
    paths: VoicePaths,
}

impl ModelManager {
    #[must_use]
    pub fn new(paths: VoicePaths) -> Self {
        Self { paths }
    }

    #[must_use]
    pub fn files(&self, id: VoiceModelId) -> ModelFiles {
        let root = self.paths.model_dir(id);
        ModelFiles {
            id,
            backend: descriptor(id).backend,
            silero_vad: root.join("silero_vad.onnx"),
            root,
        }
    }

    pub fn verify_installed(&self, id: VoiceModelId) -> Result<bool, ModelError> {
        let root = self.paths.model_dir(id);
        if !root.is_dir() {
            return Ok(false);
        }
        for required in descriptor(id).required_files {
            verify_file(
                &root.join(required.installed_path),
                required.size,
                required.sha256,
            )?;
        }
        Ok(true)
    }

    #[must_use]
    pub fn installed_size(&self, id: VoiceModelId) -> u64 {
        descriptor(id)
            .required_files
            .iter()
            .filter_map(|file| {
                fs::metadata(self.paths.model_dir(id).join(file.installed_path)).ok()
            })
            .map(|metadata| metadata.len())
            .sum()
    }

    #[must_use]
    pub fn snapshot(
        &self,
        id: VoiceModelId,
        progress: u8,
        error: Option<String>,
    ) -> VoiceModelSnapshot {
        let model = descriptor(id);
        let installed = self.verify_installed(id);
        let state = match installed {
            Ok(true) => VoiceModelState::Installed,
            Ok(false) => {
                if progress > 0 {
                    VoiceModelState::Downloading
                } else {
                    VoiceModelState::NotInstalled
                }
            }
            Err(_) => VoiceModelState::Invalid,
        };
        VoiceModelSnapshot {
            id,
            name: model.name,
            badges: model.badges.to_vec(),
            description: model.description,
            languages: model.languages.to_vec(),
            license: model.license,
            download_size_bytes: model.download_size_bytes,
            installed_size_bytes: model.installed_size_bytes,
            resource_guidance: model.resource_guidance,
            state,
            download_progress_percent: progress.min(100),
            installed_size_bytes_actual: self.installed_size(id),
            error,
        }
    }

    pub fn install(
        &self,
        id: VoiceModelId,
        mut progress: impl FnMut(u8) -> bool,
    ) -> Result<ModelFiles, ModelError> {
        fs::create_dir_all(&self.paths.models_dir)?;
        let model = descriptor(id);
        let archive_part =
            self.paths
                .models_dir
                .join(format!("{}.{}.part", id.as_str(), model.archive.file_name));
        let silero_part =
            self.paths
                .models_dir
                .join(format!("{}.{}.part", id.as_str(), SILERO_MODEL.file_name));
        if let Err(error) = download(&model.archive, &archive_part, 0, 99, &mut progress) {
            let _ = fs::remove_file(&archive_part);
            let _ = fs::remove_file(&silero_part);
            return Err(error);
        }
        if let Err(error) = download(&SILERO_MODEL, &silero_part, 99, 100, &mut progress) {
            let _ = fs::remove_file(&archive_part);
            let _ = fs::remove_file(&silero_part);
            return Err(error);
        }

        let installing = self
            .paths
            .models_dir
            .join(format!("{}.installing", id.as_str()));
        if installing.exists() {
            fs::remove_dir_all(&installing)?;
        }
        fs::create_dir_all(&installing)?;
        if let Err(error) = extract_required(model, &archive_part, &installing) {
            let _ = fs::remove_dir_all(&installing);
            let _ = fs::remove_file(&archive_part);
            let _ = fs::remove_file(&silero_part);
            return Err(error);
        }
        fs::rename(&silero_part, installing.join(SILERO_MODEL.file_name))?;
        for required in model.required_files {
            if let Err(error) = verify_file(
                &installing.join(required.installed_path),
                required.size,
                required.sha256,
            ) {
                let _ = fs::remove_dir_all(&installing);
                let _ = fs::remove_file(&archive_part);
                return Err(error);
            }
        }
        if !progress(100) {
            let _ = fs::remove_dir_all(&installing);
            let _ = fs::remove_file(&archive_part);
            return Err(ModelError::Canceled);
        }
        let final_dir = self.paths.model_dir(id);
        if final_dir.exists() {
            if !self.paths.is_safe_model_path(id, &final_dir) {
                return Err(ModelError::UnsafePath);
            }
            fs::remove_dir_all(&final_dir)?;
        }
        fs::rename(&installing, &final_dir)?;
        fs::remove_file(archive_part)?;
        Ok(self.files(id))
    }

    pub fn remove(&self, id: VoiceModelId) -> Result<(), ModelError> {
        let model_dir = self.paths.model_dir(id);
        if !self.paths.is_safe_model_path(id, &model_dir) {
            return Err(ModelError::UnsafePath);
        }
        match fs::remove_dir_all(model_dir) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn download(
    artifact: &Artifact,
    destination: &Path,
    start: u8,
    end: u8,
    progress: &mut impl FnMut(u8) -> bool,
) -> Result<(), ModelError> {
    let mut response = ureq::get(artifact.url)
        .call()
        .map_err(|error| ModelError::Download(error.to_string()))?;
    let mut reader = response.body_mut().as_reader();
    let mut file = fs::File::create(destination)?;
    let mut hasher = Sha256::new();
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        downloaded = downloaded.saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        let span = end.saturating_sub(start);
        let percent = start.saturating_add(
            u8::try_from(downloaded.saturating_mul(u64::from(span)) / artifact.size.max(1))
                .unwrap_or(span)
                .min(span),
        );
        if !progress(percent) {
            drop(file);
            let _ = fs::remove_file(destination);
            return Err(ModelError::Canceled);
        }
    }
    file.sync_all()?;
    if downloaded != artifact.size || format!("{:x}", hasher.finalize()) != artifact.sha256 {
        let _ = fs::remove_file(destination);
        return Err(ModelError::Integrity {
            name: artifact.file_name.to_owned(),
        });
    }
    Ok(())
}

fn extract_required(
    model: &ModelDescriptor,
    archive_path: &Path,
    destination: &Path,
) -> Result<(), ModelError> {
    let file = fs::File::open(archive_path)?;
    let mut archive = tar::Archive::new(BzDecoder::new(file));
    let archived: Vec<_> = model
        .required_files
        .iter()
        .filter(|file| file.archive_path.is_some())
        .collect();
    let mut found = vec![false; archived.len()];
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        for (index, required) in archived.iter().enumerate() {
            if path == Path::new(required.archive_path.unwrap_or_default()) {
                let output = destination.join(required.installed_path);
                if let Some(parent) = output.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut file = fs::File::create(output)?;
                io::copy(&mut entry, &mut file)?;
                file.sync_all()?;
                found[index] = true;
                break;
            }
        }
    }
    for (index, was_found) in found.into_iter().enumerate() {
        if !was_found {
            return Err(ModelError::MissingArtifact(archived[index].installed_path));
        }
    }
    Ok(())
}

fn verify_file(path: &Path, size: u64, sha256: &str) -> Result<(), ModelError> {
    let metadata = fs::metadata(path).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            ModelError::Integrity {
                name: path.display().to_string(),
            }
        } else {
            error.into()
        }
    })?;
    if !metadata.is_file() || metadata.len() != size {
        return Err(ModelError::Integrity {
            name: path.display().to_string(),
        });
    }
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    io::copy(&mut file, &mut hasher)?;
    if format!("{:x}", hasher.finalize()) != sha256 {
        return Err(ModelError::Integrity {
            name: path.display().to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_ids_and_paths_are_unique_and_safe() {
        let ids: HashSet<_> = catalog().iter().map(|model| model.id).collect();
        assert_eq!(ids.len(), catalog().len());
        for model in catalog() {
            assert!(model.archive.url.starts_with("https://"));
            assert_eq!(model.archive.sha256.len(), 64);
            let paths = VoicePaths {
                config_file: PathBuf::from("/tmp/config"),
                models_dir: PathBuf::from("/tmp/chathead-models"),
            };
            assert!(paths.is_safe_model_path(model.id, &paths.model_dir(model.id)));
            assert!(model.required_files.iter().all(|file| {
                Path::new(file.installed_path)
                    .components()
                    .all(|component| matches!(component, std::path::Component::Normal(_)))
            }));
        }
        assert!(
            QWEN_FILES
                .iter()
                .any(|file| file.installed_path == "tokenizer/vocab.json")
        );
    }

    #[test]
    fn qwen_archive_manifest_is_exactly_pinned() {
        assert_eq!(
            QWEN_ARCHIVE.file_name,
            "sherpa-onnx-qwen3-asr-0.6B-int8-2026-03-25.tar.bz2"
        );
        assert_eq!(QWEN_ARCHIVE.size, 878_702_423);
        assert_eq!(
            QWEN_ARCHIVE.sha256,
            "393f8a14e2f5fb96746aaab342997a40641001fbd5bf9592a080a8329178ee96"
        );
        assert_eq!(QWEN_FILES.len(), 7);
    }

    #[test]
    fn rejects_a_hash_mismatch() {
        let root = std::env::temp_dir().join(format!("chathead-hash-test-{}", std::process::id()));
        fs::create_dir_all(&root).expect("create test directory");
        let file = root.join("artifact");
        fs::write(&file, b"wrong").expect("write test artifact");
        assert!(matches!(
            verify_file(
                &file,
                5,
                "0000000000000000000000000000000000000000000000000000000000000000"
            ),
            Err(ModelError::Integrity { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn partial_download_is_never_considered_installed() {
        let root = std::env::temp_dir().join(format!("chathead-part-test-{}", std::process::id()));
        let paths = VoicePaths {
            config_file: root.join("config.json"),
            models_dir: root.join("models"),
        };
        fs::create_dir_all(&paths.models_dir).expect("create models directory");
        fs::write(paths.models_dir.join("model.part"), b"partial").expect("write part");
        assert!(
            !ModelManager::new(paths)
                .verify_installed(WHISPER_MODEL_ID)
                .expect("verify")
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn qwen_prefix_matches_the_pinned_archive_layout() {
        assert!(
            QWEN_FILES
                .iter()
                .filter_map(|file| file.archive_path)
                .all(|path| path.starts_with(QWEN_PREFIX))
        );
    }
}
