//! Renderer-neutral overlay controller and GNOME presentation D-Bus bridge.

use std::{cell::RefCell, rc::Rc};

use chathead_core::{
    ChatMessage, CodexAppServer, CodexCommand, CodexEvent, Conversation, ExperimentalChatSnapshot,
    ExperimentalChatState, IpcEvent, MessageRole, MessageState, PROTOCOL_VERSION, PanelSize,
    PanelZoom, ShortcutStatus, VoicePhase, VoiceSnapshot, VoiceSubmissionMode,
};
use chathead_voice::{VoiceEvent, VoiceService};
use gtk::{gdk, gio, glib, prelude::*};
use serde::Serialize;
use serde_json::json;

use crate::{
    Output,
    overlay::{OrbTheme, PanelPosition},
    response_format::ResponseDocument,
};

pub(crate) const PRESENTATION_PROTOCOL_VERSION: u16 = 1;
const OBJECT_PATH: &str = "/io/github/chathead_ai/ChatHead/Presentation";
const INTERFACE: &str = "io.github.chathead_ai.ChatHead.Presentation1";
const INTROSPECTION_XML: &str = r#"
<node><interface name="io.github.chathead_ai.ChatHead.Presentation1">
  <method name="GetPresentationSnapshot"><arg type="s" name="snapshot" direction="out"/></method>
  <method name="TogglePanel"/><method name="Send"><arg type="s" name="text" direction="in"/></method>
  <method name="StopResponse"/><method name="Retry"><arg type="s" name="message_id" direction="in"/></method>
  <method name="NewChat"/><method name="ActivateVoice"/><method name="CancelVoice"/>
  <method name="OpenSettings"/><method name="ConfirmLink"><arg type="b" name="open" direction="in"/></method>
  <method name="RequestLink"><arg type="s" name="destination" direction="in"/></method>
  <method name="CopyResponse"><arg type="s" name="message_id" direction="in"/><arg type="s" name="format" direction="in"/></method>
  <method name="StopOverlay"/>
  <signal name="PresentationChanged"><arg type="t" name="revision"/><arg type="s" name="patch"/></signal>
</interface></node>"#;

thread_local! {
    static CONTROLLER: RefCell<Option<Rc<RefCell<OverlayController>>>> = const { RefCell::new(None) };
    static CONNECTION: RefCell<Option<gio::DBusConnection>> = const { RefCell::new(None) };
    static REGISTRATION: RefCell<Option<gio::RegistrationId>> = const { RefCell::new(None) };
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PresentationMessage {
    id: String,
    role: MessageRole,
    state: MessageState,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    document: Option<ResponseDocument>,
    plain_text: String,
    markdown: String,
}

impl From<&ChatMessage> for PresentationMessage {
    fn from(message: &ChatMessage) -> Self {
        let document = (message.role == MessageRole::Assistant).then(|| {
            ResponseDocument::parse(&message.text, message.state != MessageState::Streaming)
        });
        let plain_text = document
            .as_ref()
            .map_or_else(|| message.text.clone(), ResponseDocument::plain_text);
        Self {
            id: message.id.clone(),
            role: message.role,
            state: message.state,
            text: message.text.clone(),
            document,
            plain_text,
            markdown: message.text.clone(),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct PresentationSnapshot {
    protocol_version: u16,
    revision: u64,
    visible: bool,
    panel_open: bool,
    appearance: OrbTheme,
    panel_position: PanelPosition,
    panel_zoom: PanelZoom,
    dimensions: PanelSize,
    conversation: Vec<PresentationMessage>,
    busy: bool,
    chat_state: ExperimentalChatState,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    voice: VoiceSnapshot,
    shortcut_status: ShortcutStatus,
    panel_shortcut_status: ShortcutStatus,
    composer_text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_link_confirmation: Option<String>,
}

/// Owns the state and operations consumed by the GNOME renderer.
pub(crate) struct OverlayController {
    revision: u64,
    visible: bool,
    panel_open: bool,
    appearance: OrbTheme,
    panel_position: PanelPosition,
    panel_zoom: PanelZoom,
    dimensions: PanelSize,
    conversation: Conversation,
    chat_state: ExperimentalChatState,
    chat_message: Option<String>,
    failure: Option<String>,
    pending_link: Option<String>,
    codex: CodexAppServer,
    voice: VoiceService,
    shortcut_status: ShortcutStatus,
    panel_shortcut_status: ShortcutStatus,
    composer_text: String,
    pending_voice: Option<u64>,
    stream_publish_source: Option<glib::SourceId>,
    output: Output,
}

impl OverlayController {
    fn new(
        codex: CodexAppServer,
        chat: ExperimentalChatSnapshot,
        voice: VoiceService,
        output: Output,
    ) -> Self {
        Self {
            revision: 1,
            visible: true,
            panel_open: false,
            appearance: OrbTheme::current(),
            panel_position: PanelPosition::current(),
            panel_zoom: crate::overlay::current_panel_zoom(),
            dimensions: crate::overlay::current_panel_size(),
            conversation: Conversation::default(),
            chat_state: chat.state,
            chat_message: chat.message,
            failure: None,
            pending_link: None,
            codex,
            voice,
            shortcut_status: ShortcutStatus::Registering,
            panel_shortcut_status: ShortcutStatus::Registering,
            composer_text: String::new(),
            pending_voice: None,
            stream_publish_source: None,
            output,
        }
    }

    fn snapshot(&self) -> PresentationSnapshot {
        PresentationSnapshot {
            protocol_version: PRESENTATION_PROTOCOL_VERSION,
            revision: self.revision,
            visible: self.visible,
            panel_open: self.panel_open,
            appearance: self.appearance,
            panel_position: self.panel_position,
            panel_zoom: self.panel_zoom,
            dimensions: self.dimensions,
            conversation: self
                .conversation
                .messages()
                .iter()
                .map(PresentationMessage::from)
                .collect(),
            busy: self.conversation.is_busy(),
            chat_state: self.chat_state,
            message: self.chat_message.clone(),
            voice: self.voice.snapshot(),
            shortcut_status: self.shortcut_status.clone(),
            panel_shortcut_status: self.panel_shortcut_status.clone(),
            composer_text: self.composer_text.clone(),
            failure: self.failure.clone(),
            pending_link_confirmation: self.pending_link.clone(),
        }
    }

    fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
        publish_snapshot(self);
    }

    fn send(&mut self, text: &str) {
        match self.conversation.send(text) {
            Ok(message_id) => {
                self.failure = None;
                if self
                    .codex
                    .send(CodexCommand::SendMessage {
                        message_id: message_id.clone(),
                        text: text.trim().to_owned(),
                    })
                    .is_err()
                {
                    let event = CodexEvent::Failure {
                        message_id: Some(message_id),
                        message: "Codex is unavailable. Start a new chat to retry.".to_owned(),
                        fatal: true,
                    };
                    self.conversation.apply(&event);
                    self.failure =
                        Some("Codex is unavailable. Start a new chat to retry.".to_owned());
                }
            }
            Err(error) => self.failure = Some(error.to_string()),
        }
        self.changed();
    }

    fn apply_codex(&mut self, event: &CodexEvent) {
        match event {
            CodexEvent::AvailabilityChanged {
                available: false,
                message,
            } => {
                self.chat_state = ExperimentalChatState::Unavailable;
                self.chat_message.clone_from(message);
            }
            CodexEvent::AuthenticationChanged(authentication) => {
                self.chat_state = if *authentication == chathead_core::AuthenticationState::ChatGpt
                {
                    ExperimentalChatState::Ready
                } else {
                    ExperimentalChatState::Unavailable
                };
                self.chat_message = (self.chat_state != ExperimentalChatState::Ready).then(|| {
                    "Connect a ChatGPT subscription in Settings to use ChatHead.".to_owned()
                });
            }
            CodexEvent::Failure { message, .. } => {
                self.failure = Some(message.clone());
                self.conversation.apply(event);
            }
            _ => self.conversation.apply(event),
        }
        self.changed();
    }
}

pub(crate) fn export(app: &gtk::Application) {
    let Some(connection) = app.dbus_connection() else {
        eprintln!("presentation D-Bus bridge unavailable: application has no session bus");
        return;
    };
    let interface = match gio::DBusNodeInfo::for_xml(INTROSPECTION_XML)
        .ok()
        .and_then(|node| node.lookup_interface(INTERFACE))
    {
        Some(interface) => interface,
        None => {
            eprintln!("presentation D-Bus introspection failed");
            return;
        }
    };
    match connection
        .register_object(OBJECT_PATH, &interface)
        .method_call(|_, _, _, _, method, parameters, invocation| {
            handle_method(method, &parameters, invocation)
        })
        .build()
    {
        Ok(registration) => {
            CONNECTION.with(|stored| stored.replace(Some(connection)));
            REGISTRATION.with(|stored| stored.replace(Some(registration)));
        }
        Err(error) => eprintln!("presentation D-Bus export failed: {error}"),
    }
}

pub(crate) fn start(
    codex: CodexAppServer,
    chat: ExperimentalChatSnapshot,
    voice: VoiceService,
    output: Output,
) {
    let controller = Rc::new(RefCell::new(OverlayController::new(
        codex, chat, voice, output,
    )));
    publish_snapshot(&controller.borrow());
    CONTROLLER.with(|stored| stored.replace(Some(controller)));
}

pub(crate) fn stop() {
    CONTROLLER.with(|stored| {
        if let Some(controller) = stored.take() {
            let mut controller = controller.borrow_mut();
            controller.visible = false;
            controller.panel_open = false;
            controller.conversation.new_chat();
            let _ = controller.codex.send(CodexCommand::NewChat);
            controller.changed();
        }
    });
}

pub(crate) fn handle_codex_event(event: &CodexEvent) {
    if matches!(event, CodexEvent::AssistantTextDelta { .. }) {
        with_controller_mut(|state| {
            state.conversation.apply(event);
            if state.stream_publish_source.is_none() {
                state.stream_publish_source = Some(glib::timeout_add_local_once(
                    std::time::Duration::from_millis(33),
                    || {
                        with_controller_mut(|state| {
                            state.stream_publish_source = None;
                            state.changed();
                        });
                    },
                ));
            }
        });
    } else {
        with_controller_mut(|state| {
            if let Some(source) = state.stream_publish_source.take() {
                source.remove();
            }
            state.apply_codex(event);
        });
    }
}
pub(crate) fn handle_voice_event(event: &VoiceEvent) {
    match event {
        VoiceEvent::Snapshot(_) | VoiceEvent::AutoFinalized => {
            with_controller_mut(OverlayController::changed);
        }
        VoiceEvent::Transcript { utterance_id, text } => {
            let utterance_id = *utterance_id;
            with_controller_mut(|state| {
                state.composer_text.clone_from(text);
                if state.voice.snapshot().submission_mode == VoiceSubmissionMode::InsertAndSend {
                    state.pending_voice = Some(utterance_id);
                } else {
                    let _ = state.voice.complete_utterance(utterance_id);
                }
                state.changed();
            });
            glib::timeout_add_local_once(std::time::Duration::from_millis(700), move || {
                with_controller_mut(|state| {
                    if state.pending_voice == Some(utterance_id)
                        && state.voice.snapshot().phase == VoicePhase::PendingSend
                    {
                        state.pending_voice = None;
                        let text = std::mem::take(&mut state.composer_text);
                        state.send(&text);
                        let _ = state.voice.complete_utterance(utterance_id);
                    }
                });
            });
        }
        VoiceEvent::LevelChanged { .. } => {}
    }
}

pub(crate) fn handle_global_app_event(event: &crate::overlay::AppEvent) {
    if let Some(update) = event.shortcut_status_update() {
        with_controller_mut(|state| {
            match update.action {
                crate::overlay::ShortcutAction::Voice => state.shortcut_status = update.status,
                crate::overlay::ShortcutAction::Panel => {
                    state.panel_shortcut_status = update.status;
                }
            }
            state.changed();
        });
        return;
    }
    match event {
        crate::overlay::AppEvent::CancelVoice => with_controller_mut(|state| {
            let _ = state.voice.cancel();
            state.pending_voice = None;
            state.changed();
        }),
        crate::overlay::AppEvent::VoiceShortcutActivated => with_controller_mut(|state| {
            state.panel_open = true;
            let _ = state.voice.shortcut_activated(
                state.conversation.is_busy(),
                state.chat_state == ExperimentalChatState::Ready,
            );
            state.changed();
        }),
        crate::overlay::AppEvent::VoiceShortcutDeactivated => with_controller_mut(|state| {
            let _ = state.voice.shortcut_deactivated();
            state.changed();
        }),
        crate::overlay::AppEvent::TogglePanel => with_controller_mut(|state| {
            state.panel_open = !state.panel_open;
            state.changed();
        }),
        crate::overlay::AppEvent::ShortcutStatus(_, _) => {}
    }
}
pub(crate) fn set_appearance(value: OrbTheme) {
    with_controller_mut(|state| {
        state.appearance = value;
        state.changed();
    });
}
pub(crate) fn set_position(value: PanelPosition) {
    with_controller_mut(|state| {
        state.panel_position = value;
        state.changed();
    });
}
pub(crate) fn set_zoom(value: PanelZoom) {
    with_controller_mut(|state| {
        state.panel_zoom = value;
        state.changed();
    });
}
pub(crate) fn set_size(value: PanelSize) {
    with_controller_mut(|state| {
        state.dimensions = value;
        state.changed();
    });
}

fn with_controller_mut(action: impl FnOnce(&mut OverlayController)) {
    CONTROLLER.with(|stored| {
        if let Some(controller) = stored.borrow().as_ref() {
            action(&mut controller.borrow_mut());
        }
    });
}

fn snapshot_json() -> String {
    CONTROLLER.with(|stored| {
        stored
            .borrow()
            .as_ref()
            .and_then(|controller| serde_json::to_string(&controller.borrow().snapshot()).ok())
            .unwrap_or_else(|| {
                json!({ "protocolVersion": 1, "revision": 0, "visible": false, "panelOpen": false })
                    .to_string()
            })
    })
}

fn publish_snapshot(controller: &OverlayController) {
    let Ok(snapshot) = serde_json::to_value(controller.snapshot()) else {
        return;
    };
    let patch = json!({ "kind": "snapshot", "snapshot": snapshot }).to_string();
    CONNECTION.with(|stored| {
        if let Some(connection) = stored.borrow().as_ref() {
            let _ = connection.emit_signal(
                None::<&str>,
                OBJECT_PATH,
                INTERFACE,
                "PresentationChanged",
                Some(&(controller.revision, patch).to_variant()),
            );
        }
    });
}

fn handle_method(method: &str, parameters: &glib::Variant, invocation: gio::DBusMethodInvocation) {
    match method {
        "GetPresentationSnapshot" => {
            invocation.return_value(Some(&(snapshot_json(),).to_variant()));
            return;
        }
        "TogglePanel" => with_controller_mut(|state| {
            state.panel_open = !state.panel_open;
            state.changed();
        }),
        "Send" => with_controller_mut(|state| state.send(&parameters.child_get::<String>(0))),
        "StopResponse" => with_controller_mut(|state| {
            let _ = state.codex.send(CodexCommand::Interrupt);
            state.changed();
        }),
        "Retry" => with_controller_mut(|state| {
            let id = parameters.child_get::<String>(0);
            if let Some(prompt) = state
                .conversation
                .prompt_for_assistant(&id)
                .map(str::to_owned)
            {
                state.send(&prompt);
            }
        }),
        "NewChat" => with_controller_mut(|state| {
            state.conversation.new_chat();
            state.failure = None;
            let _ = state.codex.send(CodexCommand::NewChat);
            state.changed();
        }),
        "ActivateVoice" => with_controller_mut(|state| {
            let _ = state.voice.shortcut_activated(
                state.conversation.is_busy(),
                state.chat_state == ExperimentalChatState::Ready,
            );
            state.changed();
        }),
        "CancelVoice" => with_controller_mut(|state| {
            let _ = state.voice.cancel();
            state.pending_voice = None;
            state.changed();
        }),
        "OpenSettings" => with_controller_mut(|state| {
            super::write_message(
                &state.output,
                &IpcEvent {
                    protocol_version: PROTOCOL_VERSION,
                    event: "openSettings",
                    payload: json!({}),
                },
            )
        }),
        "RequestLink" => with_controller_mut(|state| {
            let destination = parameters.child_get::<String>(0);
            state.pending_link =
                crate::response_format::safe_web_uri(&destination).map(|uri| uri.to_string());
            if state.pending_link.is_none() {
                state.failure = Some("ChatHead blocked an unsafe or unsupported link.".to_owned());
            }
            state.changed();
        }),
        "ConfirmLink" => with_controller_mut(|state| {
            let open = parameters.child_get::<bool>(0);
            if open
                && let Some(destination) = state.pending_link.take()
                && let Some(uri) = crate::response_format::safe_web_uri(&destination)
            {
                let _ =
                    gio::AppInfo::launch_default_for_uri(uri.as_str(), gio::AppLaunchContext::NONE);
            }
            state.pending_link = None;
            state.changed();
        }),
        "CopyResponse" => with_controller_mut(|state| {
            let id = parameters.child_get::<String>(0);
            let format = parameters.child_get::<String>(1);
            if let Some(message) = state
                .conversation
                .messages()
                .iter()
                .find(|message| message.id == id)
                && let Some(display) = gdk::Display::default()
            {
                let text = if format == "markdown" {
                    message.text.clone()
                } else {
                    ResponseDocument::parse(&message.text, true).plain_text()
                };
                display.clipboard().set_text(&text);
            }
        }),
        "StopOverlay" => stop(),
        _ => {
            invocation.return_dbus_error(
                "io.github.chathead_ai.ChatHead.Error.UnknownMethod",
                "unknown presentation method",
            );
            return;
        }
    }
    invocation.return_value(None);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_protocol_is_independent_from_desktop_ipc() {
        assert_eq!(PRESENTATION_PROTOCOL_VERSION, 1);
        assert_ne!(PRESENTATION_PROTOCOL_VERSION, PROTOCOL_VERSION);
    }
    #[test]
    fn assistant_projection_contains_safe_document_and_copy_formats() {
        let message = ChatMessage {
            id: "assistant-1".to_owned(),
            role: MessageRole::Assistant,
            text: "**Hello** [site](https://example.com)".to_owned(),
            state: MessageState::Complete,
        };
        let projection = PresentationMessage::from(&message);
        assert_eq!(projection.plain_text, "Hello site (https://example.com)");
        assert!(projection.document.is_some());
        assert_eq!(projection.markdown, message.text);
    }
}
