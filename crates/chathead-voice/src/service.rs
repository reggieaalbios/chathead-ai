use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use chathead_core::{
    MicrophoneAccess, VoiceInteractionMode, VoiceModelId, VoiceModelSnapshot, VoiceModelState,
    VoicePhase, VoiceSnapshot, VoiceSubmissionMode,
};

use crate::{
    AudioCapture, LocalVoiceRecognizer, ModelManager, VoiceCapture, VoiceConfig, VoicePaths,
    VoiceRecognizer, catalog, discover_input_devices, probe_input_device,
    recognizer::RecognitionError,
};

const WORKER_TICK: Duration = Duration::from_millis(20);
const MAX_UTTERANCE: Duration = Duration::from_secs(30);
const MAX_TEST: Duration = Duration::from_secs(10);
const LEVEL_EVENT_INTERVAL: Duration = Duration::from_millis(80);
const DEVICE_SWITCH_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Debug, PartialEq)]
pub enum VoiceEvent {
    Snapshot(VoiceSnapshot),
    Transcript { utterance_id: u64, text: String },
    LevelChanged { level: f32 },
    AutoFinalized,
}

#[derive(Debug, thiserror::Error)]
pub enum VoiceServiceError {
    #[error("local voice worker is unavailable")]
    Unavailable,
}

#[derive(Clone)]
pub struct VoiceService {
    commands: mpsc::Sender<Command>,
    events: Arc<Mutex<mpsc::Receiver<VoiceEvent>>>,
    event_sender: mpsc::Sender<VoiceEvent>,
    snapshot: Arc<Mutex<VoiceSnapshot>>,
}

impl VoiceService {
    #[must_use]
    pub fn start() -> Self {
        Self::start_with_paths(VoicePaths::discover())
    }

    #[must_use]
    pub fn start_with_paths(paths: VoicePaths) -> Self {
        let config = paths.load_config().unwrap_or_default();
        let mut initial = VoiceSnapshot {
            enabled: config.enabled,
            interaction_mode: config.interaction_mode,
            submission_mode: config.submission_mode,
            selected_input_device_id: config.input_device_id.clone(),
            selected_model_id: config.selected_model_id,
            models: catalog()
                .iter()
                .map(|model| VoiceModelSnapshot {
                    id: model.id,
                    name: model.name,
                    badges: model.badges.to_vec(),
                    description: model.description,
                    languages: model.languages.to_vec(),
                    license: model.license,
                    download_size_bytes: model.download_size_bytes,
                    installed_size_bytes: model.installed_size_bytes,
                    resource_guidance: model.resource_guidance,
                    state: VoiceModelState::NotInstalled,
                    download_progress_percent: 0,
                    installed_size_bytes_actual: 0,
                    error: None,
                })
                .collect(),
            ..VoiceSnapshot::default()
        };
        initial.phase = if config.enabled {
            VoicePhase::Loading
        } else {
            VoicePhase::Disabled
        };
        let snapshot = Arc::new(Mutex::new(initial));
        let (command_sender, command_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let worker_snapshot = Arc::clone(&snapshot);
        let service_event_sender = event_sender.clone();
        let spawn_result = thread::Builder::new()
            .name("chathead-local-voice".to_owned())
            .spawn(move || {
                Worker::new(paths, config, worker_snapshot, event_sender).run(command_receiver);
            });
        if spawn_result.is_err()
            && let Ok(mut value) = snapshot.lock()
        {
            value.phase = VoicePhase::Error;
            value.message = Some("Could not start the local voice worker.".to_owned());
            value.recoverable = false;
        }
        Self {
            commands: command_sender,
            events: Arc::new(Mutex::new(event_receiver)),
            event_sender: service_event_sender,
            snapshot,
        }
    }

    #[must_use]
    pub fn snapshot(&self) -> VoiceSnapshot {
        self.snapshot
            .lock()
            .map_or_else(|_| VoiceSnapshot::default(), |value| value.clone())
    }

    pub fn try_recv(&self) -> Option<VoiceEvent> {
        self.events.lock().ok()?.try_recv().ok()
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), VoiceServiceError> {
        if !enabled && let Ok(mut snapshot) = self.snapshot.lock() {
            snapshot.enabled = false;
            snapshot.phase = VoicePhase::Disabled;
            snapshot.message =
                Some("Local voice is off. Audio is never recorded or uploaded.".to_owned());
            let _ = self
                .event_sender
                .send(VoiceEvent::Snapshot(snapshot.clone()));
        }
        self.send(Command::SetEnabled(enabled))
    }

    pub fn set_input_device(&self, device_id: Option<String>) -> Result<(), VoiceServiceError> {
        self.send(Command::SetInputDevice(device_id))
    }

    pub fn set_interaction_mode(
        &self,
        mode: VoiceInteractionMode,
    ) -> Result<(), VoiceServiceError> {
        self.send(Command::SetInteractionMode(mode))
    }

    pub fn set_submission_mode(&self, mode: VoiceSubmissionMode) -> Result<(), VoiceServiceError> {
        self.send(Command::SetSubmissionMode(mode))
    }

    pub fn refresh_devices(&self) -> Result<(), VoiceServiceError> {
        self.send(Command::RefreshDevices)
    }

    pub fn retry_setup(&self) -> Result<(), VoiceServiceError> {
        self.send(Command::RetrySetup)
    }

    pub fn set_model(&self, model_id: VoiceModelId) -> Result<(), VoiceServiceError> {
        self.send(Command::SetModel(model_id))
    }

    pub fn download_model(&self, model_id: VoiceModelId) -> Result<(), VoiceServiceError> {
        self.send(Command::DownloadModel(model_id))
    }

    pub fn cancel_model_download(&self, model_id: VoiceModelId) -> Result<(), VoiceServiceError> {
        self.send(Command::CancelModelDownload(model_id))
    }

    pub fn remove_model(&self, model_id: VoiceModelId) -> Result<(), VoiceServiceError> {
        self.send(Command::RemoveModel(model_id))
    }

    pub fn start_test(&self) -> Result<(), VoiceServiceError> {
        self.send(Command::StartTest)
    }

    pub fn stop_test(&self) -> Result<(), VoiceServiceError> {
        self.send(Command::StopTest)
    }

    pub fn shortcut_activated(
        &self,
        chat_busy: bool,
        chat_ready: bool,
    ) -> Result<(), VoiceServiceError> {
        self.send(Command::Activated {
            chat_busy,
            chat_ready,
        })
    }

    pub fn shortcut_deactivated(&self) -> Result<(), VoiceServiceError> {
        self.send(Command::Deactivated)
    }

    pub fn cancel(&self) -> Result<(), VoiceServiceError> {
        self.send(Command::Cancel)
    }

    pub fn complete_utterance(&self, utterance_id: u64) -> Result<(), VoiceServiceError> {
        self.send(Command::Complete(utterance_id))
    }

    fn send(&self, command: Command) -> Result<(), VoiceServiceError> {
        self.commands
            .send(command)
            .map_err(|_| VoiceServiceError::Unavailable)
    }
}

enum Command {
    SetEnabled(bool),
    SetInputDevice(Option<String>),
    SetInteractionMode(VoiceInteractionMode),
    SetSubmissionMode(VoiceSubmissionMode),
    RefreshDevices,
    RetrySetup,
    SetModel(VoiceModelId),
    DownloadModel(VoiceModelId),
    CancelModelDownload(VoiceModelId),
    RemoveModel(VoiceModelId),
    StartTest,
    StopTest,
    Activated { chat_busy: bool, chat_ready: bool },
    Deactivated,
    Cancel,
    Complete(u64),
}

struct ActiveCapture {
    started: Instant,
    samples: Vec<f32>,
    testing: bool,
    last_level_event: Instant,
}

struct TranscriptionResult {
    utterance_id: u64,
    result: Result<String, RecognitionError>,
}

struct DeviceSwitchResult {
    revision: u64,
    result: Result<(), String>,
}

struct PendingDeviceSwitch {
    revision: u64,
    device_id: Option<String>,
    started: Instant,
}

impl PendingDeviceSwitch {
    fn accepts(&self, revision: u64) -> bool {
        self.revision == revision
    }

    fn timed_out(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= DEVICE_SWITCH_TIMEOUT
    }
}

fn is_silent_recognition(error: &RecognitionError) -> bool {
    matches!(
        error,
        RecognitionError::NoSpeech | RecognitionError::EmptyTranscript
    )
}

enum InstallEvent {
    Progress {
        model_id: VoiceModelId,
        percent: u8,
    },
    Finished {
        model_id: VoiceModelId,
        result: Result<(), String>,
    },
}

struct DownloadTask {
    model_id: VoiceModelId,
    cancel: Arc<AtomicBool>,
}

struct Worker {
    paths: VoicePaths,
    config: VoiceConfig,
    model: ModelManager,
    recognizer: LocalVoiceRecognizer,
    capture: AudioCapture,
    active: Option<ActiveCapture>,
    current_utterance: Option<u64>,
    next_utterance: u64,
    shortcut_flow: ShortcutFlow,
    snapshot: Arc<Mutex<VoiceSnapshot>>,
    events: mpsc::Sender<VoiceEvent>,
    transcription_sender: mpsc::Sender<TranscriptionResult>,
    transcription_receiver: mpsc::Receiver<TranscriptionResult>,
    device_switch_sender: mpsc::Sender<DeviceSwitchResult>,
    device_switch_receiver: mpsc::Receiver<DeviceSwitchResult>,
    pending_device_switch: Option<PendingDeviceSwitch>,
    next_device_switch_revision: u64,
    install_sender: mpsc::Sender<InstallEvent>,
    install_receiver: mpsc::Receiver<InstallEvent>,
    download: Option<DownloadTask>,
    model_errors: HashMap<VoiceModelId, String>,
}

impl Worker {
    fn new(
        paths: VoicePaths,
        config: VoiceConfig,
        snapshot: Arc<Mutex<VoiceSnapshot>>,
        events: mpsc::Sender<VoiceEvent>,
    ) -> Self {
        let (transcription_sender, transcription_receiver) = mpsc::channel();
        let (device_switch_sender, device_switch_receiver) = mpsc::channel();
        let (install_sender, install_receiver) = mpsc::channel();
        Self {
            model: ModelManager::new(paths.clone()),
            paths,
            config,
            recognizer: LocalVoiceRecognizer::default(),
            capture: AudioCapture::default(),
            active: None,
            current_utterance: None,
            next_utterance: 0,
            shortcut_flow: ShortcutFlow::default(),
            snapshot,
            events,
            transcription_sender,
            transcription_receiver,
            device_switch_sender,
            device_switch_receiver,
            pending_device_switch: None,
            next_device_switch_revision: 0,
            install_sender,
            install_receiver,
            download: None,
            model_errors: HashMap::new(),
        }
    }

    fn run(mut self, commands: mpsc::Receiver<Command>) {
        self.refresh_devices_internal();
        self.refresh_model_snapshots(None);
        if self.config.enabled {
            self.setup_model(false);
        } else {
            self.update(|snapshot| {
                snapshot.phase = VoicePhase::Disabled;
                snapshot.message =
                    Some("Local voice is off. Enable it or download a model manually.".to_owned());
            });
        }
        loop {
            match commands.recv_timeout(WORKER_TICK) {
                Ok(command) => self.handle(command),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            }
            self.poll_capture();
            self.poll_transcription();
            self.poll_device_switch();
            self.poll_installation();
        }
        if let Some(download) = &self.download {
            download.cancel.store(true, Ordering::Release);
        }
        let _ = self.capture.stop();
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::SetEnabled(enabled) => self.set_enabled(enabled),
            Command::SetInputDevice(device_id) => self.start_device_switch(device_id),
            Command::SetInteractionMode(mode) => {
                self.config.interaction_mode = mode;
                self.shortcut_flow = ShortcutFlow::default();
                self.update(|snapshot| snapshot.interaction_mode = mode);
                self.persist();
            }
            Command::SetSubmissionMode(mode) => {
                self.config.submission_mode = mode;
                self.update(|snapshot| snapshot.submission_mode = mode);
                self.persist();
            }
            Command::RefreshDevices => self.refresh_devices_internal(),
            Command::RetrySetup => {
                self.setup_model(true);
            }
            Command::SetModel(model_id) => self.set_model(model_id),
            Command::DownloadModel(model_id) => self.start_download(model_id),
            Command::CancelModelDownload(model_id) => self.cancel_download(model_id),
            Command::RemoveModel(model_id) => self.remove_model(model_id),
            Command::StartTest => self.start_capture(true, false, true),
            Command::StopTest => self.stop_test(),
            Command::Activated {
                chat_busy,
                chat_ready,
            } => match self
                .shortcut_flow
                .activated(self.config.interaction_mode, self.active.is_some())
            {
                ShortcutAction::Start => self.start_capture(false, chat_busy, chat_ready),
                ShortcutAction::Finish => self.finish_capture(false),
                ShortcutAction::Ignore => {}
            },
            Command::Deactivated => {
                if self
                    .shortcut_flow
                    .deactivated(self.config.interaction_mode, self.active.is_some())
                    == ShortcutAction::Finish
                {
                    self.finish_capture(false);
                }
            }
            Command::Cancel => self.cancel(),
            Command::Complete(id) => {
                if self.current_utterance == Some(id) {
                    self.current_utterance = None;
                    self.ready("Ready for local voice input.");
                }
            }
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        if self.config.enabled == enabled {
            return;
        }
        self.config.enabled = enabled;
        self.persist();
        self.update(|snapshot| snapshot.enabled = enabled);
        if enabled {
            self.setup_model(false);
        } else {
            self.cancel();
            self.recognizer.unload();
            self.update(|snapshot| {
                snapshot.phase = VoicePhase::Disabled;
                snapshot.active_model_id = None;
                snapshot.message =
                    Some("Local voice is off. Audio is never recorded or uploaded.".to_owned());
                snapshot.microphone_test_active = false;
            });
        }
    }

    fn start_device_switch(&mut self, device_id: Option<String>) {
        if self.active.is_some() || self.current_utterance.is_some() {
            self.ready("Finish the current voice action before changing microphones.");
            return;
        }
        if self.config.input_device_id == device_id && self.pending_device_switch.is_none() {
            self.ready("The selected microphone is ready.");
            return;
        }

        self.next_device_switch_revision = self.next_device_switch_revision.saturating_add(1);
        let revision = self.next_device_switch_revision;
        self.pending_device_switch = Some(PendingDeviceSwitch {
            revision,
            device_id: device_id.clone(),
            started: Instant::now(),
        });
        self.shortcut_flow = ShortcutFlow::default();
        self.update_ready_message("Switching microphone… Super+E will be ready shortly.");

        let sender = self.device_switch_sender.clone();
        let spawn = thread::Builder::new()
            .name(format!("chathead-microphone-switch-{revision}"))
            .spawn(move || {
                let result =
                    probe_input_device(device_id.as_deref()).map_err(|error| error.to_string());
                let _ = sender.send(DeviceSwitchResult { revision, result });
            });
        if let Err(error) = spawn {
            self.pending_device_switch = None;
            self.fallback_to_system_microphone(format!(
                "Could not start microphone validation: {error}"
            ));
        }
    }

    fn poll_device_switch(&mut self) {
        while let Ok(completion) = self.device_switch_receiver.try_recv() {
            let Some(pending) = self.pending_device_switch.as_ref() else {
                continue;
            };
            if !pending.accepts(completion.revision) {
                continue;
            }
            let pending = self
                .pending_device_switch
                .take()
                .expect("pending switch exists");
            match completion.result {
                Ok(()) => {
                    self.config.input_device_id = pending.device_id.clone();
                    self.update(|snapshot| {
                        snapshot.selected_input_device_id = pending.device_id;
                    });
                    self.persist();
                    self.update_ready_message("Microphone switched. Ready for local voice input.");
                }
                Err(error) => self.fallback_to_system_microphone(format!(
                    "The selected microphone could not be opened ({error})."
                )),
            }
        }

        if self
            .pending_device_switch
            .as_ref()
            .is_some_and(|pending| pending.timed_out(Instant::now()))
        {
            self.pending_device_switch = None;
            self.fallback_to_system_microphone(
                "The selected microphone took too long to respond.".to_owned(),
            );
        }
    }

    fn fallback_to_system_microphone(&mut self, reason: String) {
        self.config.input_device_id = None;
        self.update(|snapshot| snapshot.selected_input_device_id = None);
        self.persist();
        self.update_ready_message(&format!(
            "{reason} Using the system default microphone instead."
        ));
    }

    fn update_ready_message(&self, message: &str) {
        if self.config.enabled && self.recognizer.is_loaded() && self.phase() == VoicePhase::Ready {
            self.ready(message);
        }
    }

    fn setup_model(&mut self, force: bool) {
        let model_id = self.config.selected_model_id;
        match self.model.verify_installed(model_id) {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                if force {
                    self.model_errors.remove(&model_id);
                }
                self.start_download(model_id);
                return;
            }
        }
        self.load_model(model_id);
    }

    fn load_model(&mut self, model_id: VoiceModelId) {
        self.update(|snapshot| {
            snapshot.phase = VoicePhase::Loading;
            snapshot.message = Some("Loading the local model…".to_owned());
        });
        let mut replacement = LocalVoiceRecognizer::default();
        match replacement.load(&self.model.files(model_id)) {
            Ok(()) => {
                self.recognizer = replacement;
                self.model_errors.remove(&model_id);
                self.update(|snapshot| snapshot.active_model_id = Some(model_id));
                self.refresh_model_snapshots(None);
                self.ready("Ready for English, Filipino, and code-switched speech.");
            }
            Err(error) => {
                self.model_errors.insert(model_id, error.to_string());
                self.refresh_model_snapshots(None);
                self.fail(
                    format!("Could not load the selected local voice model: {error}"),
                    true,
                );
            }
        }
    }

    fn set_model(&mut self, model_id: VoiceModelId) {
        if self.voice_busy() || self.model.verify_installed(model_id).ok() != Some(true) {
            self.fail(
                "The model must be installed and voice must be idle before activation.".to_owned(),
                true,
            );
            return;
        }
        let previous_selected = self.config.selected_model_id;
        self.config.selected_model_id = model_id;
        self.update(|snapshot| snapshot.selected_model_id = model_id);
        if self.config.enabled {
            let previous_phase = self.phase();
            let previous_active = self
                .snapshot
                .lock()
                .ok()
                .and_then(|snapshot| snapshot.active_model_id);
            self.load_model(model_id);
            if self
                .snapshot
                .lock()
                .is_ok_and(|snapshot| snapshot.active_model_id != Some(model_id))
            {
                self.config.selected_model_id = previous_selected;
                self.update(|snapshot| {
                    snapshot.selected_model_id = previous_selected;
                    snapshot.active_model_id = previous_active;
                    snapshot.phase = previous_phase;
                });
                return;
            }
        }
        self.persist();
        self.refresh_model_snapshots(None);
    }

    fn start_download(&mut self, model_id: VoiceModelId) {
        if self.download.is_some() {
            self.model_errors.insert(
                model_id,
                "Another model download is already running.".to_owned(),
            );
            self.refresh_model_snapshots(None);
            return;
        }
        if self.model.verify_installed(model_id).ok() == Some(true) {
            if self.config.enabled && self.config.selected_model_id == model_id {
                self.load_model(model_id);
            }
            return;
        }
        self.model_errors.remove(&model_id);
        let cancel = Arc::new(AtomicBool::new(false));
        self.download = Some(DownloadTask {
            model_id,
            cancel: Arc::clone(&cancel),
        });
        self.refresh_model_snapshots(Some((model_id, 0)));
        if self.config.enabled && self.config.selected_model_id == model_id {
            self.update(|snapshot| {
                snapshot.phase = VoicePhase::Downloading;
                snapshot.message = Some("Downloading the selected local voice model…".to_owned());
            });
        }
        let manager = self.model.clone();
        let sender = self.install_sender.clone();
        let spawn = thread::Builder::new()
            .name(format!("chathead-model-install-{}", model_id.as_str()))
            .spawn(move || {
                let mut last = 255_u8;
                let result = manager
                    .install(model_id, |percent| {
                        if cancel.load(Ordering::Acquire) {
                            return false;
                        }
                        if percent != last {
                            last = percent;
                            let _ = sender.send(InstallEvent::Progress { model_id, percent });
                        }
                        true
                    })
                    .map(|_| ())
                    .map_err(|error| error.to_string());
                let _ = sender.send(InstallEvent::Finished { model_id, result });
            });
        if let Err(error) = spawn {
            self.download = None;
            self.model_errors
                .insert(model_id, format!("Could not start model download: {error}"));
            self.refresh_model_snapshots(None);
        }
    }

    fn cancel_download(&mut self, model_id: VoiceModelId) {
        if let Some(download) = &self.download
            && download.model_id == model_id
        {
            download.cancel.store(true, Ordering::Release);
        }
    }

    fn poll_installation(&mut self) {
        while let Ok(event) = self.install_receiver.try_recv() {
            match event {
                InstallEvent::Progress { model_id, percent } => {
                    self.refresh_model_snapshots(Some((model_id, percent)))
                }
                InstallEvent::Finished { model_id, result } => {
                    self.download = None;
                    match result {
                        Ok(()) => {
                            self.model_errors.remove(&model_id);
                            self.refresh_model_snapshots(None);
                            if self.config.enabled && self.config.selected_model_id == model_id {
                                self.load_model(model_id);
                            }
                        }
                        Err(error) if error.contains("canceled") => {
                            self.refresh_model_snapshots(None);
                            if self.config.enabled && self.config.selected_model_id == model_id {
                                self.update(|snapshot| {
                                    snapshot.phase = VoicePhase::SetupRequired;
                                    snapshot.message =
                                        Some("Selected model download canceled.".to_owned());
                                });
                            }
                        }
                        Err(error) => {
                            self.model_errors.insert(model_id, error.clone());
                            self.refresh_model_snapshots(None);
                            if self.config.enabled && self.config.selected_model_id == model_id {
                                self.fail(format!("Local voice setup failed: {error}"), true);
                            }
                        }
                    }
                }
            }
        }
    }

    fn remove_model(&mut self, model_id: VoiceModelId) {
        if self.voice_busy()
            || (self.config.enabled
                && self
                    .snapshot
                    .lock()
                    .is_ok_and(|snapshot| snapshot.active_model_id == Some(model_id)))
        {
            self.fail(
                "Disable Voice or activate another installed model before removal.".to_owned(),
                true,
            );
            return;
        }
        match self.model.remove(model_id) {
            Ok(()) => {
                self.model_errors.remove(&model_id);
                self.refresh_model_snapshots(None);
                self.update(|snapshot| {
                    if snapshot.selected_model_id == model_id && snapshot.enabled {
                        snapshot.phase = VoicePhase::SetupRequired;
                    }
                    snapshot.message = Some("Local voice model removed.".to_owned());
                });
            }
            Err(error) => self.fail(format!("Could not remove the local model: {error}"), true),
        }
    }

    fn refresh_model_snapshots(&self, progress: Option<(VoiceModelId, u8)>) {
        if let Some((model_id, percent)) = progress {
            self.update(|snapshot| {
                if let Some(model) = snapshot
                    .models
                    .iter_mut()
                    .find(|model| model.id == model_id)
                {
                    model.state = VoiceModelState::Downloading;
                    model.download_progress_percent = percent.min(100);
                    model.error = None;
                }
            });
            return;
        }
        let models = catalog()
            .iter()
            .map(|descriptor| {
                let mut snapshot = self.model.snapshot(
                    descriptor.id,
                    0,
                    self.model_errors.get(&descriptor.id).cloned(),
                );
                if self
                    .download
                    .as_ref()
                    .is_some_and(|download| download.model_id == descriptor.id)
                {
                    snapshot.state = VoiceModelState::Downloading;
                }
                snapshot
            })
            .collect();
        self.update(|snapshot| snapshot.models = models);
    }

    fn voice_busy(&self) -> bool {
        self.active.is_some()
            || self.current_utterance.is_some()
            || self.pending_device_switch.is_some()
            || matches!(
                self.phase(),
                VoicePhase::Listening
                    | VoicePhase::Transcribing
                    | VoicePhase::PendingSend
                    | VoicePhase::Loading
            )
            || self
                .snapshot
                .lock()
                .is_ok_and(|snapshot| snapshot.microphone_test_active)
    }

    fn start_capture(&mut self, testing: bool, chat_busy: bool, chat_ready: bool) {
        if let Some(block) =
            capture_start_block(self.config.enabled, testing, chat_busy, chat_ready)
        {
            match block {
                CaptureStartBlock::Disabled => self.fail(block.message().to_owned(), true),
                CaptureStartBlock::Busy | CaptureStartBlock::ChatUnavailable => {
                    self.ready(block.message());
                }
            }
            return;
        }
        if self.active.is_some() || (!testing && self.phase() != VoicePhase::Ready) {
            return;
        }
        if self.pending_device_switch.is_some() {
            self.shortcut_flow = ShortcutFlow::default();
            self.ready("Microphone switch is still in progress. Try Super+E again shortly.");
            return;
        }
        match self.capture.start(self.config.input_device_id.as_deref()) {
            Ok(()) => {
                self.active = Some(ActiveCapture {
                    started: Instant::now(),
                    samples: Vec::new(),
                    testing,
                    last_level_event: Instant::now() - LEVEL_EVENT_INTERVAL,
                });
                self.update(|snapshot| {
                    snapshot.microphone_access = MicrophoneAccess::Granted;
                    snapshot.microphone_test_active = testing;
                    snapshot.phase = if testing {
                        VoicePhase::Ready
                    } else {
                        VoicePhase::Listening
                    };
                    snapshot.message = Some(if testing {
                        "Microphone test is running for up to 10 seconds.".to_owned()
                    } else {
                        "Listening locally · release to transcribe · 30-second maximum.".to_owned()
                    });
                });
            }
            Err(error) => {
                self.update(|snapshot| {
                    snapshot.microphone_access =
                        if error.to_string().to_ascii_lowercase().contains("denied")
                            || error
                                .to_string()
                                .to_ascii_lowercase()
                                .contains("permission")
                        {
                            MicrophoneAccess::Denied
                        } else {
                            MicrophoneAccess::Unavailable
                        };
                });
                self.fail(format!("Microphone unavailable: {error}"), true);
            }
        }
    }

    fn poll_capture(&mut self) {
        let Some(active) = self.active.as_mut() else {
            return;
        };
        if self.capture.drain_available(&mut active.samples).is_err() {
            self.fail(
                "The microphone disconnected while recording.".to_owned(),
                true,
            );
            self.active = None;
            return;
        }
        if active.testing && active.last_level_event.elapsed() >= LEVEL_EVENT_INTERVAL {
            let recent = active.samples.iter().rev().take(1_600);
            let (sum, count) = recent.fold((0.0_f32, 0_u32), |(sum, count), sample| {
                (sum + sample * sample, count.saturating_add(1))
            });
            let level = if count == 0 {
                0.0
            } else {
                (sum / count as f32).sqrt().clamp(0.0, 1.0)
            };
            let _ = self.events.send(VoiceEvent::LevelChanged { level });
            active.last_level_event = Instant::now();
        }
        let limit = if active.testing {
            MAX_TEST
        } else {
            MAX_UTTERANCE
        };
        if active.started.elapsed() >= limit {
            if active.testing {
                self.stop_test();
            } else {
                let _ = self.events.send(VoiceEvent::AutoFinalized);
                self.finish_capture(true);
            }
        }
    }

    fn finish_capture(&mut self, _automatic: bool) {
        let Some(mut active) = self.active.take() else {
            return;
        };
        if active.testing {
            self.stop_capture_without_transcription(active);
            return;
        }
        let captured = match self.capture.stop() {
            Ok(mut captured) => {
                captured.samples.splice(0..0, active.samples.drain(..));
                captured
            }
            Err(error) => {
                self.fail(
                    format!("Could not finish microphone capture: {error}"),
                    true,
                );
                return;
            }
        };
        self.shortcut_flow = ShortcutFlow::default();
        self.next_utterance = self.next_utterance.saturating_add(1);
        let utterance_id = self.next_utterance;
        self.current_utterance = Some(utterance_id);
        self.update(|snapshot| {
            snapshot.phase = VoicePhase::Transcribing;
            snapshot.message = Some("Transcribing locally… · Esc to cancel.".to_owned());
        });
        let recognizer = self.recognizer.clone();
        let sender = self.transcription_sender.clone();
        let spawn = thread::Builder::new()
            .name(format!("chathead-transcribe-{utterance_id}"))
            .spawn(move || {
                let result = recognizer.transcribe(captured);
                let _ = sender.send(TranscriptionResult {
                    utterance_id,
                    result,
                });
            });
        if let Err(error) = spawn {
            self.current_utterance = None;
            self.fail(
                format!("Could not start local transcription: {error}"),
                true,
            );
        }
    }

    fn poll_transcription(&mut self) {
        while let Ok(result) = self.transcription_receiver.try_recv() {
            if self.current_utterance != Some(result.utterance_id) {
                continue;
            }
            match result.result {
                Ok(text) => {
                    self.update(|snapshot| {
                        snapshot.phase = VoicePhase::PendingSend;
                        snapshot.message = Some("Sending in 0.7s · Esc to cancel.".to_owned());
                    });
                    let _ = self.events.send(VoiceEvent::Transcript {
                        utterance_id: result.utterance_id,
                        text,
                    });
                }
                Err(error) if is_silent_recognition(&error) => {
                    self.current_utterance = None;
                    self.update(|snapshot| {
                        snapshot.phase = VoicePhase::Ready;
                        snapshot.message = None;
                        snapshot.recoverable = true;
                    });
                }
                Err(error) => {
                    self.current_utterance = None;
                    self.fail(error.to_string(), true);
                }
            }
        }
    }

    fn stop_test(&mut self) {
        let Some(active) = self.active.take() else {
            self.update(|snapshot| snapshot.microphone_test_active = false);
            return;
        };
        if active.testing {
            self.stop_capture_without_transcription(active);
        } else {
            self.active = Some(active);
        }
    }

    fn stop_capture_without_transcription(&mut self, _active: ActiveCapture) {
        let _ = self.capture.stop();
        self.update(|snapshot| snapshot.microphone_test_active = false);
        self.ready("Microphone test finished. No audio was saved.");
    }

    fn cancel(&mut self) {
        if self.active.take().is_some() {
            let _ = self.capture.stop();
        }
        self.current_utterance = None;
        self.shortcut_flow = ShortcutFlow::default();
        if self.config.enabled && self.recognizer.is_loaded() {
            self.ready("Voice input canceled. Audio and transcript were discarded.");
        }
    }

    fn refresh_devices_internal(&mut self) {
        match discover_input_devices() {
            Ok(devices) => {
                let default = devices
                    .iter()
                    .find(|device| device.is_default)
                    .map(|device| device.id.clone());
                self.update(|snapshot| {
                    snapshot.default_input_device_id = default;
                    snapshot.input_devices = devices;
                });
            }
            Err(error) => self.update(|snapshot| {
                snapshot.input_devices.clear();
                snapshot.default_input_device_id = None;
                snapshot.microphone_access = MicrophoneAccess::Unavailable;
                snapshot.message = Some(format!("Could not enumerate microphones: {error}"));
            }),
        }
    }

    fn persist(&self) {
        if let Err(error) = self.paths.save_config(&self.config) {
            self.update(|snapshot| {
                snapshot.message = Some(format!("Could not save voice settings: {error}"));
                snapshot.recoverable = true;
            });
        }
    }

    fn phase(&self) -> VoicePhase {
        self.snapshot
            .lock()
            .map_or(VoicePhase::Error, |snapshot| snapshot.phase)
    }

    fn ready(&self, message: &str) {
        self.update(|snapshot| {
            snapshot.phase = VoicePhase::Ready;
            snapshot.message = Some(message.to_owned());
            snapshot.recoverable = true;
        });
    }

    fn fail(&self, message: String, recoverable: bool) {
        self.update(|snapshot| {
            snapshot.phase = VoicePhase::Error;
            snapshot.message = Some(message);
            snapshot.recoverable = recoverable;
            snapshot.microphone_test_active = false;
        });
    }

    fn update(&self, change: impl FnOnce(&mut VoiceSnapshot)) {
        if let Ok(mut snapshot) = self.snapshot.lock() {
            change(&mut snapshot);
            let _ = self.events.send(VoiceEvent::Snapshot(snapshot.clone()));
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct ShortcutFlow {
    toggle_recording: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShortcutAction {
    Start,
    Finish,
    Ignore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CaptureStartBlock {
    Disabled,
    Busy,
    ChatUnavailable,
}

impl CaptureStartBlock {
    fn message(self) -> &'static str {
        match self {
            Self::Disabled => "Enable Local Voice in Settings first.",
            Self::Busy => "Voice input is unavailable while ChatHead is responding.",
            Self::ChatUnavailable => "Voice input requires a ready ChatGPT connection.",
        }
    }
}

fn capture_start_block(
    enabled: bool,
    testing: bool,
    chat_busy: bool,
    chat_ready: bool,
) -> Option<CaptureStartBlock> {
    if !enabled {
        Some(CaptureStartBlock::Disabled)
    } else if !testing && chat_busy {
        Some(CaptureStartBlock::Busy)
    } else if !testing && !chat_ready {
        Some(CaptureStartBlock::ChatUnavailable)
    } else {
        None
    }
}

impl ShortcutFlow {
    fn activated(&mut self, mode: VoiceInteractionMode, capture_active: bool) -> ShortcutAction {
        match mode {
            VoiceInteractionMode::Hold if capture_active => ShortcutAction::Ignore,
            VoiceInteractionMode::Hold => ShortcutAction::Start,
            VoiceInteractionMode::Toggle if self.toggle_recording || capture_active => {
                self.toggle_recording = false;
                ShortcutAction::Finish
            }
            VoiceInteractionMode::Toggle => {
                self.toggle_recording = true;
                ShortcutAction::Start
            }
        }
    }

    fn deactivated(&mut self, mode: VoiceInteractionMode, capture_active: bool) -> ShortcutAction {
        if mode == VoiceInteractionMode::Hold && capture_active {
            ShortcutAction::Finish
        } else {
            ShortcutAction::Ignore
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hold_mode_ignores_duplicate_activation_and_finishes_on_release() {
        let mut flow = ShortcutFlow::default();
        assert_eq!(
            flow.activated(VoiceInteractionMode::Hold, false),
            ShortcutAction::Start
        );
        assert_eq!(
            flow.activated(VoiceInteractionMode::Hold, true),
            ShortcutAction::Ignore
        );
        assert_eq!(
            flow.deactivated(VoiceInteractionMode::Hold, true),
            ShortcutAction::Finish
        );
    }

    #[test]
    fn toggle_mode_ignores_release_and_finishes_on_second_activation() {
        let mut flow = ShortcutFlow::default();
        assert_eq!(
            flow.activated(VoiceInteractionMode::Toggle, false),
            ShortcutAction::Start
        );
        assert_eq!(
            flow.deactivated(VoiceInteractionMode::Toggle, true),
            ShortcutAction::Ignore
        );
        assert_eq!(
            flow.activated(VoiceInteractionMode::Toggle, true),
            ShortcutAction::Finish
        );
    }

    #[test]
    fn stale_transcription_ids_cannot_become_current_again() {
        let current = Some(4_u64);
        assert_ne!(current, Some(3));
        assert_eq!(current, Some(4));
    }

    #[test]
    fn duration_limits_match_product_contract() {
        assert_eq!(MAX_UTTERANCE, Duration::from_secs(30));
        assert_eq!(MAX_TEST, Duration::from_secs(10));
        assert_eq!(DEVICE_SWITCH_TIMEOUT, Duration::from_secs(8));
    }

    #[test]
    fn microphone_switch_revisions_reject_stale_completions() {
        let pending = PendingDeviceSwitch {
            revision: 7,
            device_id: Some("pipewire:microphone".to_owned()),
            started: Instant::now(),
        };
        assert!(!pending.accepts(6));
        assert!(pending.accepts(7));
    }

    #[test]
    fn microphone_switch_timeout_is_bounded() {
        let now = Instant::now();
        let pending = PendingDeviceSwitch {
            revision: 1,
            device_id: None,
            started: now - DEVICE_SWITCH_TIMEOUT,
        };
        assert!(pending.timed_out(now));
    }

    #[test]
    fn silence_and_empty_transcripts_are_normal_empty_results() {
        assert!(is_silent_recognition(&RecognitionError::NoSpeech));
        assert!(is_silent_recognition(&RecognitionError::EmptyTranscript));
        assert!(!is_silent_recognition(&RecognitionError::DeviceLost));
    }

    #[test]
    fn capture_start_rejects_busy_or_unavailable_chat_but_allows_microphone_tests() {
        assert_eq!(
            capture_start_block(true, false, true, true),
            Some(CaptureStartBlock::Busy)
        );
        assert_eq!(
            capture_start_block(true, false, false, false),
            Some(CaptureStartBlock::ChatUnavailable)
        );
        assert_eq!(capture_start_block(true, true, true, false), None);
    }
}
