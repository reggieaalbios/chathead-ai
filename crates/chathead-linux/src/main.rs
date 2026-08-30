use std::{
    cell::RefCell,
    io::{self, BufRead, BufReader, BufWriter, Write},
    rc::Rc,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

use chathead_core::{
    Backend, BackendSnapshot, CodexAppServer, CodexCommand, CodexEvent, DesktopIntegrationKind,
    DesktopIntegrationSnapshot, DesktopIntegrationStatus, ErrorCode, IpcError, IpcEvent,
    IpcRequest, IpcResponse, LaunchBlocker, PROTOCOL_VERSION, PanelSize, PanelZoom, ProviderId,
    ShortcutAction, VoiceInteractionMode, VoiceModelId, VoiceSubmissionMode,
};
use chathead_voice::{VoiceEvent, VoiceService};
use gtk::{gio, glib, prelude::*};
use serde::Deserialize;
use serde_json::{Value, json};

mod desktop_integration;
mod overlay;
mod presentation;
mod response_format;
mod response_view;
mod shortcut_integration;

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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ShortcutActionParams {
    action: ShortcutAction,
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
    let shortcuts = Rc::new(RefCell::new(shortcut_integration::ShortcutManager::load()));
    if let Ok(mut state) = backend.lock() {
        state.set_desktop_integration(desktop_integration::detect());
        state.set_shortcut_actions(shortcuts.borrow().states());
        state.set_shortcut_integration(shortcuts.borrow().integration());
    }
    let voice = VoiceService::start();
    if let Ok(mut state) = backend.lock() {
        state.set_voice_snapshot(voice.snapshot());
    }
    let codex = CodexAppServer::start();
    let output = Arc::new(Mutex::new(BufWriter::new(io::stdout())));
    let (sender, receiver) = mpsc::channel();
    let (shortcut_status_sender, shortcut_status_receiver) = mpsc::channel();
    let (global_shortcut_sender, global_shortcut_receiver) = mpsc::channel();
    presentation::export(app);
    watch_gnome_readiness(&backend, &output);
    let shortcut_service = overlay::start_shortcut_service(
        global_shortcut_sender,
        shortcuts.borrow().configured_actions(),
        shortcuts.borrow().integration().backend,
    );
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
                    ShortcutRuntime {
                        status_sender: &shortcut_status_sender,
                        manager: &shortcuts,
                        service: &shortcut_service,
                    },
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
                    presentation::stop();
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
        while shortcut_status_receiver.try_recv().is_ok() {}
        while let Ok(event) = global_shortcut_receiver.try_recv() {
            if matches!(event, overlay::AppEvent::ConfigReloaded) {
                shortcuts.borrow_mut().audit_effective_bindings();
                publish_shortcuts(&backend, &shortcuts, &output);
            }
            if matches!(event, overlay::AppEvent::TogglePanel)
                && backend
                    .lock()
                    .is_ok_and(|state| !state.snapshot().overlay_running)
                && let Err(error) = launch_overlay(
                    &app,
                    &backend,
                    &codex,
                    &voice,
                    &output,
                    &shortcut_status_sender,
                )
            {
                write_message(
                    &output,
                    &IpcEvent {
                        protocol_version: PROTOCOL_VERSION,
                        event: "openSettings",
                        payload: json!({ "message": error.message }),
                    },
                );
            }
            overlay::handle_global_app_event(&event);
            presentation::handle_global_app_event(&event);
        }
        glib::ControlFlow::Continue
    });
}

fn watch_gnome_readiness(backend: &Arc<Mutex<Backend>>, output: &Output) {
    let appeared_backend = backend.clone();
    let appeared_output = output.clone();
    let vanished_backend = backend.clone();
    let vanished_output = output.clone();
    let _readiness_watcher = gio::bus_watch_name(
        gio::BusType::Session,
        desktop_integration::READINESS_BUS_NAME,
        gio::BusNameWatcherFlags::NONE,
        move |_, _, _| {
            if let Ok(mut state) = appeared_backend.lock()
                && state.snapshot().desktop_integration.kind == DesktopIntegrationKind::GnomeShell
            {
                state.set_desktop_integration(desktop_integration::detect());
                write_snapshot_changed(&appeared_output, state.snapshot());
            }
        },
        move |_, _| {
            if let Ok(mut state) = vanished_backend.lock() {
                let snapshot = state.snapshot();
                if snapshot.desktop_integration.kind != DesktopIntegrationKind::GnomeShell {
                    return;
                }
                state.set_desktop_integration(DesktopIntegrationSnapshot {
                    kind: DesktopIntegrationKind::GnomeShell,
                    status: DesktopIntegrationStatus::Disabled,
                    gnome_version: snapshot.desktop_integration.gnome_version,
                    message: Some(
                        "Enable the ChatHead GNOME extension, then log out and back in if GNOME cannot activate it live."
                            .to_owned(),
                    ),
                });
                if snapshot.overlay_running {
                    presentation::stop();
                    state.set_overlay_running(false);
                }
                write_snapshot_changed(&vanished_output, state.snapshot());
            }
        },
    );
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

struct ShortcutRuntime<'a> {
    status_sender: &'a mpsc::Sender<overlay::ShortcutStatusUpdate>,
    manager: &'a Rc<RefCell<shortcut_integration::ShortcutManager>>,
    service: &'a overlay::ShortcutService,
}

fn handle_request(
    app: &gtk::Application,
    backend: &Arc<Mutex<Backend>>,
    codex: &CodexAppServer,
    voice: &VoiceService,
    output: &Output,
    shortcuts: ShortcutRuntime<'_>,
    request: IpcRequest,
) {
    let shortcut_status_sender = shortcuts.status_sender;
    let shortcut_service = shortcuts.service;
    let shortcuts = shortcuts.manager;
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
            launch_overlay(app, backend, codex, voice, output, shortcut_status_sender)
        }
        "stopOverlay" => {
            overlay::stop_native_overlay(app);
            presentation::stop();
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
        "refreshDesktopIntegration" => backend.lock().map_err(lock_error).map(|mut state| {
            state.set_desktop_integration(desktop_integration::detect());
            state.snapshot()
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
        "beginShortcutCapture" => {
            parse::<ShortcutActionParams>(request.params).and_then(|params| {
                shortcuts
                    .borrow_mut()
                    .begin_capture(params.action)
                    .map_err(shortcut_error)?;
                if let Ok(mut state) = backend.lock() {
                    state.set_shortcut_actions(shortcuts.borrow().states());
                    state.set_shortcut_capture(shortcuts.borrow().capture());
                }
                let complete_shortcuts = shortcuts.clone();
                let complete_backend = backend.clone();
                let complete_output = output.clone();
                let complete_service = shortcut_service.clone();
                let cancel_shortcuts = shortcuts.clone();
                let cancel_backend = backend.clone();
                let cancel_output = output.clone();
                let update_shortcuts = shortcuts.clone();
                let update_backend = backend.clone();
                let update_output = output.clone();
                shortcut_integration::show_capture_surface(
                    app,
                    params.action,
                    move |binding| {
                        let _ = complete_shortcuts
                            .borrow_mut()
                            .captured(params.action, binding);
                        configure_shortcut_service(&complete_shortcuts, &complete_service);
                        publish_shortcuts(&complete_backend, &complete_shortcuts, &complete_output);
                    },
                    move || {
                        cancel_shortcuts.borrow_mut().cancel_capture(params.action);
                        publish_shortcuts(&cancel_backend, &cancel_shortcuts, &cancel_output);
                    },
                    move |pressed_keys| {
                        update_shortcuts
                            .borrow_mut()
                            .update_capture_keys(params.action, pressed_keys);
                        publish_shortcuts(&update_backend, &update_shortcuts, &update_output);
                    },
                );
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "cancelShortcutCapture" => {
            parse::<ShortcutActionParams>(request.params).and_then(|params| {
                shortcuts.borrow_mut().cancel_capture(params.action);
                publish_shortcuts(backend, shortcuts, output);
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "confirmShortcutReplacement" => {
            parse::<ShortcutActionParams>(request.params).and_then(|params| {
                shortcuts
                    .borrow_mut()
                    .confirm_replacement(params.action)
                    .map_err(shortcut_error)?;
                configure_shortcut_service(shortcuts, shortcut_service);
                publish_shortcuts(backend, shortcuts, output);
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "clearShortcut" => parse::<ShortcutActionParams>(request.params).and_then(|params| {
            shortcuts
                .borrow_mut()
                .clear(params.action)
                .map_err(shortcut_error)?;
            configure_shortcut_service(shortcuts, shortcut_service);
            publish_shortcuts(backend, shortcuts, output);
            backend
                .lock()
                .map(|state| state.snapshot())
                .map_err(lock_error)
        }),
        "repairShortcutIntegration" => {
            let repair_result = {
                let mut manager = shortcuts.borrow_mut();
                manager.repair().map_err(shortcut_error)
            };
            repair_result.and_then(|()| {
                configure_shortcut_service(shortcuts, shortcut_service);
                publish_shortcuts(backend, shortcuts, output);
                backend
                    .lock()
                    .map(|state| state.snapshot())
                    .map_err(lock_error)
            })
        }
        "shutdown" => {
            overlay::stop_native_overlay(app);
            presentation::stop();
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

fn launch_overlay(
    app: &gtk::Application,
    backend: &Arc<Mutex<Backend>>,
    codex: &CodexAppServer,
    voice: &VoiceService,
    output: &Output,
    shortcut_status_sender: &mpsc::Sender<overlay::ShortcutStatusUpdate>,
) -> Result<BackendSnapshot, IpcError> {
    let snapshot = backend
        .lock()
        .map_err(lock_error)
        .map(|state| state.snapshot())?;
    if !snapshot.launch_readiness.ready {
        let integration_required = snapshot
            .launch_readiness
            .blockers
            .contains(&LaunchBlocker::DesktopIntegrationRequired);
        let integration_unavailable = snapshot
            .launch_readiness
            .blockers
            .contains(&LaunchBlocker::DesktopIntegrationUnavailable);
        return Err(IpcError {
            code: if integration_required {
                ErrorCode::DesktopIntegrationRequired
            } else if integration_unavailable {
                ErrorCode::DesktopIntegrationUnavailable
            } else {
                ErrorCode::AuthFailed
            },
            message: if integration_required {
                "desktop integration must be installed or enabled before launch"
            } else if integration_unavailable {
                "desktop integration is unavailable for this session"
            } else {
                "authenticate at least one LLM provider before launch"
            }
            .to_owned(),
            recoverable: true,
        });
    }

    match snapshot.desktop_integration.kind {
        DesktopIntegrationKind::GnomeShell => {
            presentation::start(
                codex.clone(),
                snapshot.experimental_chat,
                voice.clone(),
                output.clone(),
            );
        }
        DesktopIntegrationKind::LayerShell => overlay::start_native_overlay(
            app,
            codex.clone(),
            snapshot.experimental_chat,
            voice.clone(),
            output.clone(),
            shortcut_status_sender.clone(),
        )
        .map_err(|message| IpcError {
            code: ErrorCode::DesktopIntegrationUnavailable,
            message: message.to_owned(),
            recoverable: true,
        })?,
        DesktopIntegrationKind::Unsupported => {
            return Err(IpcError {
                code: ErrorCode::DesktopIntegrationUnavailable,
                message: "desktop integration is unavailable for this session".to_owned(),
                recoverable: true,
            });
        }
    }
    backend
        .lock()
        .map_err(lock_error)?
        .set_overlay_running(true);
    backend
        .lock()
        .map(|state| state.snapshot())
        .map_err(lock_error)
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
        presentation::handle_voice_event(&event);
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
    presentation::handle_codex_event(&event);
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

fn shortcut_error(message: String) -> IpcError {
    IpcError {
        code: ErrorCode::InvalidRequest,
        message,
        recoverable: true,
    }
}

fn publish_shortcuts(
    backend: &Arc<Mutex<Backend>>,
    shortcuts: &Rc<RefCell<shortcut_integration::ShortcutManager>>,
    output: &Output,
) {
    if let Ok(mut state) = backend.lock() {
        state.set_shortcut_actions(shortcuts.borrow().states());
        state.set_shortcut_integration(shortcuts.borrow().integration());
        state.set_shortcut_capture(shortcuts.borrow().capture());
        write_snapshot_changed(output, state.snapshot());
    }
}

fn configure_shortcut_service(
    shortcuts: &Rc<RefCell<shortcut_integration::ShortcutManager>>,
    service: &overlay::ShortcutService,
) {
    let shortcuts = shortcuts.borrow();
    service.configure(
        shortcuts.configured_actions(),
        shortcuts.integration().backend,
    );
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
        assert!(parse::<PanelZoomParams>(json!({ "zoom": 250 })).is_ok());
        assert!(parse::<PanelZoomParams>(json!({ "zoom": 251 })).is_err());
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
            parse::<PanelSizeParams>(json!({ "size": { "width": 720, "height": 851 } })).is_err()
        );
    }
}
