use std::{
    io::{self, BufRead, BufReader, BufWriter, Write},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use chathead_core::{
    Backend, CodexAppServer, CodexCommand, CodexEvent, ErrorCode, IpcError, IpcEvent, IpcRequest,
    IpcResponse, LaunchReadiness, PROTOCOL_VERSION, PanelSize, PanelZoom, ProviderId,
    VoiceInteractionMode, VoiceModelId, VoiceSubmissionMode,
};
use chathead_voice::{VoiceEvent, VoiceService};
use gtk::{gio, glib, prelude::*};
use serde::Deserialize;
use serde_json::{Value, json};

mod overlay;
mod response_format;
mod response_view;

const APP_ID: &str = "io.github.chathead_ai.ChatHead.Sidecar";
const IPC_POLL_MS: u64 = 20;

enum Input {
    Request(IpcRequest),
    Malformed(String),
    Closed,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProviderParams {
    provider_id: ProviderId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SaveApiKeyParams {
    provider_id: ProviderId,
    api_key: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OverlayThemeParams {
    theme: overlay::OrbTheme,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PanelPositionParams {
    position: overlay::PanelPosition,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PanelZoomParams {
    zoom: PanelZoom,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PanelSizeParams {
    size: PanelSize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoiceEnabledParams {
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoiceInputDeviceParams {
    device_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoiceInteractionModeParams {
    mode: VoiceInteractionMode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoiceSubmissionModeParams {
    mode: VoiceSubmissionMode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VoiceModelParams {
    model_id: VoiceModelId,
}

type Output = Arc<Mutex<BufWriter<io::Stdout>>>;

fn main() -> glib::ExitCode {
    let app = gtk::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::IS_SERVICE)
        .build();
    app.connect_startup(start_sidecar);
    app.run_with_args::<&str>(&[])
}

fn start_sidecar(app: &gtk::Application) {
    overlay::load_native_css();
    let application_hold = app.hold();

    let backend = Arc::new(Mutex::new(Backend::new()));
    let voice = VoiceService::start();
    if let Ok(mut state) = backend.lock() {
        state.set_voice_snapshot(voice.snapshot());
    }
    let codex = CodexAppServer::start();
    let output = Arc::new(Mutex::new(BufWriter::new(io::stdout())));
    let (sender, receiver) = mpsc::channel();
    let (shortcut_status_sender, shortcut_status_receiver) = mpsc::channel();
    start_stdin_reader(sender);

    if let Ok(state) = backend.lock() {
        write_message(
            &output,
            &IpcEvent {
                protocol_version: PROTOCOL_VERSION,
                event: "ready",
                payload: state.snapshot(),
            },
        );
    }

    let app = app.clone();
    glib::timeout_add_local(Duration::from_millis(IPC_POLL_MS), move || {
        let _keep_application_alive = &application_hold;
        while let Ok(input) = receiver.try_recv() {
            match input {
                Input::Request(request) => handle_request(
                    &app,
                    &backend,
                    &codex,
                    &voice,
                    &output,
                    &shortcut_status_sender,
                    request,
                ),
                Input::Malformed(message) => write_message(
                    &output,
                    &IpcResponse::<Value>::failure(
                        String::new(),
                        IpcError {
                            code: ErrorCode::InvalidRequest,
                            message,
                            recoverable: true,
                        },
                    ),
                ),
                Input::Closed => {
                    overlay::stop_native_overlay(&app);
                    app.quit();
                    return glib::ControlFlow::Break;
                }
            }
        }
        while let Some(event) = codex.try_recv() {
            handle_codex_event(&app, &backend, &output, event);
        }
        while let Some(event) = voice.try_recv() {
            handle_voice_event(&app, &backend, &output, event);
        }
        while let Ok(shortcut_status) = shortcut_status_receiver.try_recv() {
            if let Ok(mut state) = backend.lock() {
                match shortcut_status.action {
                    overlay::ShortcutAction::Voice => {
                        state.set_shortcut_status(shortcut_status.status);
                    }
                    overlay::ShortcutAction::Panel => {
                        state.set_panel_shortcut_status(shortcut_status.status);
                    }
                }
                write_snapshot_changed(&output, state.snapshot());
            }
        }
        glib::ControlFlow::Continue
    });
}

fn start_stdin_reader(sender: mpsc::Sender<Input>) {
    if let Err(error) = thread::Builder::new()
        .name("chathead-ipc-reader".to_owned())
        .spawn(move || {
            let stdin = io::stdin();
            for line in BufReader::new(stdin.lock()).lines() {
                let input = match line {
                    Ok(line) => match serde_json::from_str::<IpcRequest>(&line) {
                        Ok(request) => Input::Request(request),
                        Err(error) => Input::Malformed(format!("malformed IPC request: {error}")),
                    },
                    Err(error) => Input::Malformed(format!("could not read IPC request: {error}")),
                };
                if sender.send(input).is_err() {
                    return;
                }
            }
            let _ = sender.send(Input::Closed);
        })
    {
        eprintln!("failed to start IPC input reader: {error}");
    }
}

fn handle_request(
    app: &gtk::Application,
    backend: &Arc<Mutex<Backend>>,
    codex: &CodexAppServer,
    voice: &VoiceService,
    output: &Output,
    shortcut_status_sender: &mpsc::Sender<overlay::ShortcutStatusUpdate>,
    request: IpcRequest,
) {
    if request.protocol_version != PROTOCOL_VERSION {
        write_error(
            output,
            request.id,
            ErrorCode::ProtocolMismatch,
            format!(
                "expected protocol {PROTOCOL_VERSION}, received {}",
                request.protocol_version
            ),
            false,
        );
        return;
    }

    let id = request.id;
    let publish_response_snapshot = publishes_response_snapshot(&request.method);
    let result = match request.method.as_str() {
        "getSnapshot" => backend
            .lock()
            .map(|state| state.snapshot())
            .map_err(lock_error),
        "saveApiKey" => parse::<SaveApiKeyParams>(request.params).and_then(|params| {
            backend
                .lock()
                .map_err(lock_error)?
                .save_api_key(params.provider_id, &params.api_key)
                .map_err(IpcError::from)?;
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "connectSubscription" => parse::<ProviderParams>(request.params).and_then(|params| {
            backend
                .lock()
                .map_err(lock_error)?
                .begin_subscription_login(params.provider_id)
                .map_err(IpcError::from)?;
            codex.send(CodexCommand::Login).map_err(codex_error)?;
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "disconnectProvider" => parse::<ProviderParams>(request.params).and_then(|params| {
            let subscription_logout = backend
                .lock()
                .map_err(lock_error)?
                .snapshot()
                .providers
                .into_iter()
                .find(|provider| provider.id == params.provider_id)
                .is_some_and(|provider| {
                    matches!(
                        provider.status,
                        chathead_core::ProviderStatus::Authenticated {
                            method: chathead_core::AuthMethod::SubscriptionLogin
                        }
                    )
                });
            backend
                .lock()
                .map_err(lock_error)?
                .disconnect_provider(params.provider_id)
                .map_err(IpcError::from)?;
            if subscription_logout {
                codex.send(CodexCommand::Logout).map_err(codex_error)?;
            }
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "launchOverlay" => {
            let snapshot = backend
                .lock()
                .map_err(lock_error)
                .map(|state| state.snapshot());
            match snapshot {
                Ok(snapshot)
                    if snapshot.launch_readiness == LaunchReadiness::MissingLaunchProvider =>
                {
                    Err(IpcError {
                        code: ErrorCode::AuthFailed,
                        message: "authenticate at least one LLM provider before launch".to_owned(),
                        recoverable: true,
                    })
                }
                Ok(snapshot) => {
                    overlay::start_native_overlay(
                        app,
                        codex.clone(),
                        snapshot.experimental_chat,
                        voice.clone(),
                        output.clone(),
                        shortcut_status_sender.clone(),
                    )
                }
                .map_err(|message| IpcError {
                    code: ErrorCode::LayerShellUnsupported,
                    message: message.to_owned(),
                    recoverable: true,
                })
                .and_then(|()| {
                    backend
                        .lock()
                        .map_err(lock_error)?
                        .set_overlay_running(true);
                    backend
                        .lock()
                        .map(|state| state.snapshot())
                        .map_err(lock_error)
                }),
                Err(error) => Err(error),
            }
        }
        "stopOverlay" => {
            overlay::stop_native_overlay(app);
            let _ = codex.send(CodexCommand::NewChat);
            backend.lock().map_err(lock_error).map(|mut state| {
                state.set_overlay_running(false);
                state.snapshot()
            })
        }
        "setOverlayTheme" => parse::<OverlayThemeParams>(request.params).and_then(|params| {
            overlay::set_native_overlay_theme(app, params.theme);
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "setPanelPosition" => parse::<PanelPositionParams>(request.params).and_then(|params| {
            overlay::set_native_panel_position(params.position);
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "setPanelZoom" => parse::<PanelZoomParams>(request.params).and_then(|params| {
            overlay::set_native_panel_zoom(params.zoom);
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "setPanelSize" => parse::<PanelSizeParams>(request.params).and_then(|params| {
            overlay::set_native_panel_size(params.size);
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "setVoiceEnabled" => parse::<VoiceEnabledParams>(request.params).and_then(|params| {
            voice.set_enabled(params.enabled).map_err(voice_error)?;
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "setVoiceInputDevice" => {
            parse::<VoiceInputDeviceParams>(request.params).and_then(|params| {
                voice
                    .set_input_device(params.device_id)
                    .map_err(voice_error)?;
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "setVoiceInteractionMode" => {
            parse::<VoiceInteractionModeParams>(request.params).and_then(|params| {
                voice
                    .set_interaction_mode(params.mode)
                    .map_err(voice_error)?;
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "setVoiceSubmissionMode" => {
            parse::<VoiceSubmissionModeParams>(request.params).and_then(|params| {
                voice
                    .set_submission_mode(params.mode)
                    .map_err(voice_error)?;
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "refreshVoiceDevices" => voice.refresh_devices().map_err(voice_error).and_then(|()| {
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "retryVoiceSetup" => voice.retry_setup().map_err(voice_error).and_then(|()| {
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "setVoiceModel" => parse::<VoiceModelParams>(request.params).and_then(|params| {
            voice.set_model(params.model_id).map_err(voice_error)?;
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "downloadVoiceModel" => parse::<VoiceModelParams>(request.params).and_then(|params| {
            voice.download_model(params.model_id).map_err(voice_error)?;
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "cancelVoiceModelDownload" => {
            parse::<VoiceModelParams>(request.params).and_then(|params| {
                voice
                    .cancel_model_download(params.model_id)
                    .map_err(voice_error)?;
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "removeVoiceModel" => parse::<VoiceModelParams>(request.params).and_then(|params| {
            voice.remove_model(params.model_id).map_err(voice_error)?;
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "startVoiceTest" => voice.start_test().map_err(voice_error).and_then(|()| {
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "stopVoiceTest" => voice.stop_test().map_err(voice_error).and_then(|()| {
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "shutdown" => {
            overlay::stop_native_overlay(app);
            let _ = codex.send(CodexCommand::Shutdown);
            write_message(
                output,
                &IpcResponse::success(id, json!({ "shuttingDown": true })),
            );
            app.quit();
            return;
        }
        _ => Err(IpcError {
            code: ErrorCode::UnsupportedOperation,
            message: "unsupported IPC method".to_owned(),
            recoverable: true,
        }),
    };

    match result {
        Ok(snapshot) => {
            write_message(output, &IpcResponse::success(id, snapshot.clone()));
            if publish_response_snapshot {
                write_message(
                    output,
                    &IpcEvent {
                        protocol_version: PROTOCOL_VERSION,
                        event: "snapshotChanged",
                        payload: snapshot,
                    },
                );
            }
        }
        Err(error) => write_message(output, &IpcResponse::<Value>::failure(id, error)),
    }
}

fn codex_error(_: chathead_core::CodexServiceError) -> IpcError {
    IpcError {
        code: ErrorCode::ChatUnavailable,
        message: "experimental Codex service is unavailable".to_owned(),
        recoverable: true,
    }
}

fn voice_error(error: chathead_voice::VoiceServiceError) -> IpcError {
    IpcError {
        code: ErrorCode::VoiceUnavailable,
        message: error.to_string(),
        recoverable: true,
    }
}

fn handle_voice_event(
    app: &gtk::Application,
    backend: &Arc<Mutex<Backend>>,
    output: &Output,
    event: VoiceEvent,
) {
    if let VoiceEvent::Snapshot(snapshot) = &event
        && let Ok(mut state) = backend.lock()
    {
        state.set_voice_snapshot(snapshot.clone());
        write_snapshot_changed(output, state.snapshot());
    }
    if let VoiceEvent::LevelChanged { level } = event {
        write_message(
            output,
            &IpcEvent {
                protocol_version: PROTOCOL_VERSION,
                event: "voiceLevelChanged",
                payload: json!({ "level": level }),
            },
        );
    } else {
        overlay::handle_voice_event(app, &event);
    }
}

fn handle_codex_event(
    app: &gtk::Application,
    backend: &Arc<Mutex<Backend>>,
    output: &Output,
    event: CodexEvent,
) {
    match &event {
        CodexEvent::AvailabilityChanged { available, message } => {
            if let Ok(mut state) = backend.lock() {
                state.set_codex_availability(*available, message.clone());
                write_snapshot_changed(output, state.snapshot());
            }
        }
        CodexEvent::AuthenticationChanged(authentication) => {
            if let Ok(mut state) = backend.lock() {
                state.set_subscription_authentication(authentication.clone());
                write_snapshot_changed(output, state.snapshot());
            }
        }
        CodexEvent::AuthenticationUrl(url) => write_message(
            output,
            &IpcEvent {
                protocol_version: PROTOCOL_VERSION,
                event: "openExternal",
                payload: json!({ "purpose": "codexLogin", "url": url }),
            },
        ),
        CodexEvent::Failure {
            message_id: None,
            message,
            ..
        } => {
            if let Ok(mut state) = backend.lock() {
                state.set_codex_error(message.clone());
                write_snapshot_changed(output, state.snapshot());
            }
        }
        _ => {}
    }
    overlay::handle_codex_event(app, &event);
}

fn write_snapshot_changed(output: &Output, snapshot: chathead_core::BackendSnapshot) {
    write_message(
        output,
        &IpcEvent {
            protocol_version: PROTOCOL_VERSION,
            event: "snapshotChanged",
            payload: snapshot,
        },
    );
}

fn parse<T: for<'de> Deserialize<'de>>(value: Value) -> Result<T, IpcError> {
    serde_json::from_value(value).map_err(|error| IpcError {
        code: ErrorCode::InvalidRequest,
        message: format!("invalid request parameters: {error}"),
        recoverable: true,
    })
}

fn publishes_response_snapshot(method: &str) -> bool {
    !matches!(
        method,
        "setVoiceEnabled"
            | "setVoiceInputDevice"
            | "setVoiceInteractionMode"
            | "setVoiceSubmissionMode"
            | "refreshVoiceDevices"
            | "retryVoiceSetup"
            | "setVoiceModel"
            | "downloadVoiceModel"
            | "cancelVoiceModelDownload"
            | "removeVoiceModel"
            | "startVoiceTest"
            | "stopVoiceTest"
    )
}

fn lock_error<T>(_: std::sync::PoisonError<T>) -> IpcError {
    IpcError {
        code: ErrorCode::SidecarUnavailable,
        message: "backend state is unavailable".to_owned(),
        recoverable: false,
    }
}

fn write_error(output: &Output, id: String, code: ErrorCode, message: String, recoverable: bool) {
    write_message(
        output,
        &IpcResponse::<Value>::failure(
            id,
            IpcError {
                code,
                message,
                recoverable,
            },
        ),
    );
}

fn write_message<T: serde::Serialize>(output: &Output, message: &T) {
    let Ok(mut writer) = output.lock() else {
        eprintln!("IPC output lock is poisoned");
        return;
    };
    if let Err(error) = serde_json::to_writer(&mut *writer, message)
        .and_then(|()| writer.write_all(b"\n").map_err(serde_json::Error::io))
        .and_then(|()| writer.flush().map_err(serde_json::Error::io))
    {
        eprintln!("failed to write IPC message: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_parameter_fields() {
        let result = parse::<ProviderParams>(
            json!({ "providerId": "chatgpt", "apiKey": "must-not-be-accepted" }),
        );
        assert!(result.is_err());
    }

    #[test]
    fn request_protocol_version_is_mandatory() {
        let request = serde_json::from_str::<IpcRequest>(r#"{"id":"1","method":"getSnapshot"}"#);
        assert!(request.is_err());
    }

    #[test]
    fn asynchronous_voice_commands_do_not_publish_stale_response_snapshots() {
        assert!(!publishes_response_snapshot("setVoiceEnabled"));
        assert!(!publishes_response_snapshot("startVoiceTest"));
        assert!(publishes_response_snapshot("getSnapshot"));
    }

    #[test]
    fn panel_zoom_parameters_reject_values_outside_the_protocol_levels() {
        assert!(parse::<PanelZoomParams>(json!({ "zoom": 125 })).is_ok());
        assert!(parse::<PanelZoomParams>(json!({ "zoom": 95 })).is_err());
    }

    #[test]
    fn panel_size_parameters_reject_values_outside_the_protocol_bounds() {
        assert!(
            parse::<PanelSizeParams>(json!({ "size": { "width": 720, "height": 600 } })).is_ok()
        );
        assert!(
            parse::<PanelSizeParams>(json!({ "size": { "width": 419, "height": 600 } })).is_err()
        );
        assert!(
            parse::<PanelSizeParams>(json!({ "size": { "width": 720, "height": 801 } })).is_err()
        );
    }
}
