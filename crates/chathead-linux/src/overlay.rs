//! GTK4 layer-shell orb, panel, input regions, drag handling, and shortcut service.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        atomic::{AtomicU8, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use ashpd::desktop::{
    CreateSessionOptions,
    global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut},
};
use chathead_core::{
    ChatMessage, CodexAppServer, CodexCommand, CodexEvent, Conversation, ExperimentalChatSnapshot,
    ExperimentalChatState, MessageRole, MessageState,
};
use futures_util::StreamExt;
use gtk::{cairo, gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde::Deserialize;

const VOICE_TOGGLE_ID: &str = "voice_toggle";
const VOICE_TOGGLE_TRIGGER: &str = "LOGO+SHIFT+v";
const VOICE_TOGGLE_LABEL: &str = "Super+Shift+V";
const CHATHEAD_SIZE: i32 = 84;
const PANEL_WIDTH: i32 = 560;
const PANEL_HEIGHT: i32 = 460;
const PANEL_GAP: i32 = 10;
const EDGE_PADDING: i32 = 12;
const CLICK_THRESHOLD: f64 = 5.0;
const ACTION_POLL_MS: u64 = 40;
const ANIMATION_FRAME_MS: u64 = 33;
const SHORTCUT_DEBOUNCE_MS: u64 = 450;
const WAKE_ANIMATION_SECONDS: f64 = 0.36;
static ORB_THEME: AtomicU8 = AtomicU8::new(0);
static OVERLAY_POSITION: AtomicU8 = AtomicU8::new(0);

thread_local! {
    static PANEL_RUNTIME: RefCell<Option<PanelRuntime>> = const { RefCell::new(None) };
    static POSITION_RUNTIME: RefCell<Option<PositionRuntime>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OrbTheme {
    Light,
    Dark,
}

impl OrbTheme {
    const fn value(self) -> u8 {
        match self {
            Self::Light => 0,
            Self::Dark => 1,
        }
    }

    fn current() -> Self {
        if ORB_THEME.load(Ordering::Relaxed) == Self::Dark.value() {
            Self::Dark
        } else {
            Self::Light
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum OverlayPosition {
    Left,
    Right,
}

impl OverlayPosition {
    const fn value(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    fn current() -> Self {
        if OVERLAY_POSITION.load(Ordering::Relaxed) == Self::Right.value() {
            Self::Right
        } else {
            Self::Left
        }
    }
}

pub(crate) fn set_native_overlay_theme(app: &gtk::Application, theme: OrbTheme) {
    ORB_THEME.store(theme.value(), Ordering::Relaxed);
    for window in app.windows() {
        window.remove_css_class("theme-light");
        window.remove_css_class("theme-dark");
        window.add_css_class(match theme {
            OrbTheme::Light => "theme-light",
            OrbTheme::Dark => "theme-dark",
        });
        if let Some(root) = window.child() {
            queue_chathead_draw(&root);
        }
    }
}

pub(crate) fn set_native_overlay_position(position: OverlayPosition) {
    OVERLAY_POSITION.store(position.value(), Ordering::Relaxed);
    POSITION_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            apply_preferred_position(runtime, position);
        }
    });
}

fn queue_chathead_draw(widget: &gtk::Widget) {
    if widget.has_css_class("chathead") {
        widget.queue_draw();
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        queue_chathead_draw(&current);
        child = current.next_sibling();
    }
}

pub(crate) fn load_native_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_data(include_str!("style.css"));

    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VoiceState {
    Idle,
    Listening,
}

enum AppEvent {
    StopVoice,
    ToggleVoice,
    ShortcutStatus(ShortcutStatus),
}

#[derive(Clone)]
enum ShortcutStatus {
    Registering,
    Ready(String),
    ConflictPossible(String),
    Unavailable(String),
}

#[derive(Clone)]
struct OverlayState {
    x: Rc<Cell<f64>>,
    y: Rc<Cell<f64>>,
    drag_start_x: Rc<Cell<f64>>,
    drag_start_y: Rc<Cell<f64>>,
    dragging_chathead: Rc<Cell<bool>>,
    panel_open: Rc<Cell<bool>>,
    voice_state: Rc<Cell<VoiceState>>,
    voice_changed_at: Rc<Cell<Instant>>,
    shortcut_status: Rc<RefCell<ShortcutStatus>>,
    animation_source: Rc<RefCell<Option<glib::SourceId>>>,
}

impl OverlayState {
    fn new() -> Self {
        Self {
            x: Rc::new(Cell::new(100.0)),
            y: Rc::new(Cell::new(100.0)),
            drag_start_x: Rc::new(Cell::new(100.0)),
            drag_start_y: Rc::new(Cell::new(100.0)),
            dragging_chathead: Rc::new(Cell::new(false)),
            panel_open: Rc::new(Cell::new(false)),
            voice_state: Rc::new(Cell::new(VoiceState::Idle)),
            voice_changed_at: Rc::new(Cell::new(Instant::now())),
            shortcut_status: Rc::new(RefCell::new(ShortcutStatus::Registering)),
            animation_source: Rc::new(RefCell::new(None)),
        }
    }
}

#[derive(Clone)]
struct PanelWidgets {
    container: gtk::Box,
    status: gtk::Label,
    message: gtk::Label,
    chat_status: gtk::Label,
    transcript: gtk::Box,
    transcript_scroll: gtk::ScrolledWindow,
    composer: gtk::TextView,
    composer_placeholder: gtk::Label,
    send: gtk::Button,
    retry: gtk::Button,
    failure: gtk::Label,
    info: gtk::Label,
}

#[derive(Clone)]
struct PanelRuntime {
    widgets: PanelWidgets,
    conversation: Rc<RefCell<Conversation>>,
    codex: CodexAppServer,
    chat_state: Rc<Cell<ExperimentalChatState>>,
    chat_message: Rc<RefCell<Option<String>>>,
    failure: Rc<RefCell<Option<String>>>,
}

#[derive(Clone)]
struct PositionRuntime {
    window: gtk::ApplicationWindow,
    canvas: gtk::Fixed,
    chathead: gtk::DrawingArea,
    panel: gtk::Box,
    state: OverlayState,
}

pub(crate) fn start_native_overlay(
    app: &gtk::Application,
    codex: CodexAppServer,
    chat: ExperimentalChatSnapshot,
) -> Result<(), &'static str> {
    if let Some(existing) = app
        .windows()
        .into_iter()
        .find(|window| window.has_css_class("overlay-window"))
    {
        existing.present();
        return Ok(());
    }

    if !gtk4_layer_shell::is_supported() {
        return Err("layer shell is not supported by the active compositor");
    }

    let state = OverlayState::new();
    let (event_sender, event_receiver) = mpsc::channel();
    let window = gtk::ApplicationWindow::builder()
        .application(app)
        .title("ChatHead AI")
        .decorated(false)
        .resizable(false)
        .css_classes(["overlay-window"])
        .build();
    window.add_css_class(match OrbTheme::current() {
        OrbTheme::Light => "theme-light",
        OrbTheme::Dark => "theme-dark",
    });

    window.init_layer_shell();
    window.set_namespace(Some("chathead-ai"));
    window.set_layer(Layer::Overlay);
    window.set_keyboard_mode(KeyboardMode::None);
    window.set_exclusive_zone(-1);
    for edge in [Edge::Top, Edge::Right, Edge::Bottom, Edge::Left] {
        window.set_anchor(edge, true);
    }
    target_first_monitor(&window);

    let canvas = gtk::Fixed::new();
    canvas.set_hexpand(true);
    canvas.set_vexpand(true);

    let chathead = gtk::DrawingArea::builder()
        .width_request(CHATHEAD_SIZE)
        .height_request(CHATHEAD_SIZE)
        .tooltip_text("ChatHead AI")
        .css_classes(["chathead"])
        .build();
    let state_for_draw = state.clone();
    chathead.set_draw_func(move |_, context, width, height| {
        draw_companion_orb(
            context,
            width,
            height,
            state_for_draw.voice_state.get(),
            state_for_draw.voice_changed_at.get().elapsed(),
            OrbTheme::current(),
        );
    });

    let panel_widgets = build_panel();
    let panel_runtime = PanelRuntime {
        widgets: panel_widgets.clone(),
        conversation: Rc::new(RefCell::new(Conversation::default())),
        codex,
        chat_state: Rc::new(Cell::new(chat.state)),
        chat_message: Rc::new(RefCell::new(chat.message)),
        failure: Rc::new(RefCell::new(None)),
    };
    wire_chat_controls(&panel_runtime);
    render_chat(&panel_runtime);
    PANEL_RUNTIME.with(|stored| stored.replace(Some(panel_runtime)));
    let panel = panel_widgets.container.clone();
    panel.set_visible(false);
    canvas.put(&panel, 0.0, 0.0);
    canvas.put(&chathead, state.x.get(), state.y.get());
    window.set_child(Some(&canvas));

    attach_drag_controller(&window, &canvas, &chathead, &panel, &state);
    attach_local_key_controller(&panel, event_sender.clone());

    let window_for_realize = window.clone();
    let state_for_realize = state.clone();
    window.connect_realize(move |_| {
        apply_idle_input_region(&window_for_realize, &state_for_realize);
    });

    window.present();

    POSITION_RUNTIME.with(|stored| {
        stored.replace(Some(PositionRuntime {
            window: window.clone(),
            canvas: canvas.clone(),
            chathead: chathead.clone(),
            panel: panel.clone(),
            state: state.clone(),
        }));
    });

    let window_for_idle = window.clone();
    let canvas_for_idle = canvas.clone();
    let chathead_for_idle = chathead.clone();
    let panel_for_idle = panel.clone();
    let state_for_idle = state.clone();
    glib::idle_add_local_once(move || {
        apply_preferred_position(
            &PositionRuntime {
                window: window_for_idle,
                canvas: canvas_for_idle,
                chathead: chathead_for_idle,
                panel: panel_for_idle,
                state: state_for_idle,
            },
            OverlayPosition::current(),
        );
    });

    attach_app_event_pump(
        event_receiver,
        &chathead,
        &panel_widgets.status,
        &panel_widgets.message,
        &state,
    );
    start_shortcut_service(event_sender);
    Ok(())
}

pub(crate) fn stop_native_overlay(app: &gtk::Application) {
    PANEL_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.take() {
            runtime.conversation.borrow_mut().new_chat();
            let _ = runtime.codex.send(CodexCommand::NewChat);
        }
    });
    POSITION_RUNTIME.with(|stored| {
        stored.take();
    });
    for window in app.windows() {
        if window.has_css_class("overlay-window") {
            window.close();
        }
    }
}

fn target_first_monitor(window: &gtk::ApplicationWindow) {
    let Some(display) = gdk::Display::default() else {
        return;
    };
    let Some(monitor) = display
        .monitors()
        .item(0)
        .and_then(|item| item.downcast::<gdk::Monitor>().ok())
    else {
        return;
    };

    let geometry = monitor.geometry();
    window.set_monitor(Some(&monitor));
    window.set_default_size(geometry.width(), geometry.height());
}

fn build_panel() -> PanelWidgets {
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(PANEL_WIDTH)
        .height_request(PANEL_HEIGHT)
        .spacing(12)
        .css_classes(["chat-panel"])
        .build();

    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let title = gtk::Label::builder()
        .label("ChatHead AI")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["panel-title"])
        .build();
    let experimental = gtk::Label::builder()
        .label("Experimental")
        .css_classes(["experimental-badge"])
        .build();
    let chat_status = gtk::Label::builder()
        .label("Checking…")
        .css_classes(["status"])
        .build();
    let new_chat = gtk::Button::builder()
        .label("New Chat")
        .css_classes(["new-chat"])
        .build();
    header.append(&title);
    header.append(&experimental);
    header.append(&chat_status);
    header.append(&new_chat);

    let status = gtk::Label::builder()
        .label("Voice shortcut…")
        .xalign(0.0)
        .css_classes(["voice-status"])
        .build();

    let message = gtk::Label::builder()
        .label(format!(
            "Voice toggle is registering through the XDG portal. Preferred shortcut: {VOICE_TOGGLE_LABEL}."
        ))
        .wrap(true)
        .xalign(0.0)
        .yalign(0.0)
        .css_classes(["voice-message"])
        .build();

    let transcript = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .valign(gtk::Align::End)
        .build();
    let transcript_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .css_classes(["transcript-scroll"])
        .build();
    transcript_scroll.set_child(Some(&transcript));

    let info = gtk::Label::builder()
        .label("Experimental chat uses your authenticated ChatGPT subscription through Codex.")
        .wrap(true)
        .xalign(0.0)
        .css_classes(["chat-info"])
        .build();

    let failure_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let failure = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["chat-failure"])
        .build();
    let retry = gtk::Button::builder().label("Retry").build();
    failure_row.append(&failure);
    failure_row.append(&retry);

    let composer_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    let composer = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .hexpand(true)
        .height_request(58)
        .css_classes(["prompt-input"])
        .build();
    let composer_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .height_request(64)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .css_classes(["prompt-frame"])
        .child(&composer)
        .build();
    let composer_placeholder = gtk::Label::builder()
        .label("Message ChatHead…")
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start(9)
        .margin_top(8)
        .css_classes(["prompt-placeholder"])
        .build();
    composer_placeholder.set_can_target(false);
    let composer_overlay = gtk::Overlay::new();
    composer_overlay.set_hexpand(true);
    composer_overlay.set_child(Some(&composer_scroll));
    composer_overlay.add_overlay(&composer_placeholder);
    let send = gtk::Button::builder()
        .label("Send")
        .valign(gtk::Align::Fill)
        .css_classes(["send-button"])
        .build();
    composer_row.append(&composer_overlay);
    composer_row.append(&send);

    panel.append(&header);
    panel.append(&status);
    panel.append(&message);
    panel.append(&info);
    panel.append(&transcript_scroll);
    panel.append(&failure_row);
    panel.append(&composer_row);
    new_chat.connect_clicked(|_| {
        PANEL_RUNTIME.with(|stored| {
            if let Some(runtime) = stored.borrow().as_ref() {
                runtime.conversation.borrow_mut().new_chat();
                runtime.failure.replace(None);
                let _ = runtime.codex.send(CodexCommand::NewChat);
                render_chat(runtime);
            }
        })
    });
    PanelWidgets {
        container: panel,
        status,
        message,
        chat_status,
        transcript,
        transcript_scroll,
        composer,
        composer_placeholder,
        send,
        retry,
        failure,
        info,
    }
}

fn wire_chat_controls(runtime: &PanelRuntime) {
    let runtime_for_send = runtime.clone();
    runtime.widgets.send.connect_clicked(move |_| {
        if runtime_for_send.conversation.borrow().is_busy() {
            let _ = runtime_for_send.codex.send(CodexCommand::Interrupt);
        } else {
            submit_composer(&runtime_for_send);
        }
    });

    let runtime_for_retry = runtime.clone();
    runtime.widgets.retry.connect_clicked(move |_| {
        let prompt = runtime_for_retry
            .conversation
            .borrow()
            .last_prompt()
            .map(str::to_owned);
        if let Some(prompt) = prompt {
            submit_text(&runtime_for_retry, &prompt);
        }
    });

    let key = gtk::EventControllerKey::new();
    let runtime_for_key = runtime.clone();
    key.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::Return && !modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
            submit_composer(&runtime_for_key);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    runtime.widgets.composer.add_controller(key);

    let placeholder = runtime.widgets.composer_placeholder.clone();
    runtime
        .widgets
        .composer
        .buffer()
        .connect_changed(move |buffer| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
            placeholder.set_visible(text.is_empty());
        });
}

fn submit_composer(runtime: &PanelRuntime) {
    let buffer = runtime.widgets.composer.buffer();
    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
    submit_text(runtime, &text);
    if runtime.conversation.borrow().is_busy() {
        buffer.set_text("");
    }
}

fn submit_text(runtime: &PanelRuntime, text: &str) {
    if text.trim().is_empty() {
        runtime
            .failure
            .replace(Some("Type a message before sending.".to_owned()));
        render_chat(runtime);
        runtime.widgets.composer.grab_focus();
        return;
    }
    if runtime.chat_state.get() != ExperimentalChatState::Ready {
        runtime.failure.replace(Some(
            "Connect a ChatGPT subscription in Settings before sending.".to_owned(),
        ));
        render_chat(runtime);
        return;
    }
    let send_result = runtime.conversation.borrow_mut().send(text);
    let message_id = match send_result {
        Ok(message_id) => message_id,
        Err(error) => {
            runtime.failure.replace(Some(error.to_string()));
            render_chat(runtime);
            return;
        }
    };
    runtime.failure.replace(None);
    if runtime
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
        runtime.conversation.borrow_mut().apply(&event);
        runtime.failure.replace(Some(
            "Codex is unavailable. Start a new chat to retry.".to_owned(),
        ));
    }
    render_chat(runtime);
}

pub(crate) fn handle_codex_event(_app: &gtk::Application, event: &CodexEvent) {
    PANEL_RUNTIME.with(|stored| {
        let runtime_ref = stored.borrow();
        let Some(runtime) = runtime_ref.as_ref() else {
            return;
        };
        match event {
            CodexEvent::AvailabilityChanged { available, message } => {
                if !available {
                    runtime.chat_state.set(ExperimentalChatState::Unavailable);
                    runtime.chat_message.replace(message.clone());
                }
            }
            CodexEvent::AuthenticationChanged(authentication) => {
                if *authentication == chathead_core::AuthenticationState::ChatGpt {
                    runtime.chat_state.set(ExperimentalChatState::Ready);
                    runtime.chat_message.replace(None);
                } else {
                    runtime.chat_state.set(ExperimentalChatState::Unavailable);
                    runtime.chat_message.replace(Some(
                        "Connect a ChatGPT subscription in Settings to use Experimental chat."
                            .to_owned(),
                    ));
                }
            }
            CodexEvent::Failure { message, .. } => {
                runtime.failure.replace(Some(message.clone()));
                runtime.conversation.borrow_mut().apply(event);
            }
            _ => runtime.conversation.borrow_mut().apply(event),
        }
        render_chat(runtime);
    });
}

fn render_chat(runtime: &PanelRuntime) {
    let adjustment = runtime.widgets.transcript_scroll.vadjustment();
    let near_bottom = adjustment.value() + adjustment.page_size() >= adjustment.upper() - 28.0;

    while let Some(child) = runtime.widgets.transcript.first_child() {
        runtime.widgets.transcript.remove(&child);
    }
    let conversation = runtime.conversation.borrow();
    for message in conversation.messages() {
        runtime.widgets.transcript.append(&message_bubble(message));
    }

    let busy = conversation.is_busy();
    let ready = runtime.chat_state.get() == ExperimentalChatState::Ready;
    runtime.widgets.chat_status.set_label(if busy {
        "Thinking"
    } else if ready {
        "Ready"
    } else {
        "Unavailable"
    });
    runtime.widgets.chat_status.set_css_classes(if ready {
        &["status"]
    } else {
        &["status", "status-error"]
    });
    runtime
        .widgets
        .send
        .set_label(if busy { "Stop" } else { "Send" });
    runtime.widgets.composer.set_sensitive(ready && !busy);
    let composer_buffer = runtime.widgets.composer.buffer();
    let composer_text = composer_buffer.text(
        &composer_buffer.start_iter(),
        &composer_buffer.end_iter(),
        true,
    );
    runtime
        .widgets
        .composer_placeholder
        .set_visible(ready && !busy && composer_text.is_empty());
    runtime.widgets.send.set_sensitive(ready);

    let info = runtime.chat_message.borrow().clone().unwrap_or_else(|| {
        "Experimental chat uses your authenticated ChatGPT subscription through Codex.".to_owned()
    });
    runtime.widgets.info.set_label(&info);
    runtime
        .widgets
        .info
        .set_visible(conversation.messages().is_empty() || !ready);

    let failure = runtime.failure.borrow().clone();
    runtime
        .widgets
        .failure
        .set_label(failure.as_deref().unwrap_or(""));
    runtime.widgets.failure.set_visible(failure.is_some());
    runtime.widgets.retry.set_visible(
        failure.is_some() && conversation.last_prompt().is_some() && !conversation.is_busy(),
    );
    drop(conversation);

    if near_bottom {
        let adjustment = adjustment.clone();
        glib::idle_add_local_once(move || {
            adjustment.set_value((adjustment.upper() - adjustment.page_size()).max(0.0));
        });
    }
}

fn message_bubble(message: &ChatMessage) -> gtk::Box {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_halign(match message.role {
        MessageRole::User => gtk::Align::End,
        MessageRole::Assistant => gtk::Align::Start,
    });
    let text = if message.text.is_empty() && message.state == MessageState::Streaming {
        "…"
    } else {
        &message.text
    };
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .max_width_chars(38)
        .css_classes(match message.role {
            MessageRole::User => ["chat-bubble", "user-bubble"],
            MessageRole::Assistant => ["chat-bubble", "assistant-bubble"],
        })
        .build();
    row.append(&label);
    row
}

fn attach_app_event_pump(
    receiver: mpsc::Receiver<AppEvent>,
    chathead: &gtk::DrawingArea,
    status: &gtk::Label,
    message: &gtk::Label,
    state: &OverlayState,
) {
    let chathead = chathead.clone();
    let status = status.clone();
    let message = message.clone();
    let state = state.clone();

    glib::timeout_add_local(Duration::from_millis(ACTION_POLL_MS), move || {
        while let Ok(event) = receiver.try_recv() {
            handle_app_event(event, &chathead, &status, &message, &state);
        }

        glib::ControlFlow::Continue
    });
}

fn handle_app_event(
    event: AppEvent,
    chathead: &gtk::DrawingArea,
    status: &gtk::Label,
    message: &gtk::Label,
    state: &OverlayState,
) {
    match event {
        AppEvent::StopVoice => stop_voice_state(chathead, status, message, state),
        AppEvent::ToggleVoice => toggle_voice_state(chathead, status, message, state),
        AppEvent::ShortcutStatus(shortcut_status) => {
            state.shortcut_status.replace(shortcut_status);
            update_status_widgets(status, message, state);
        }
    }
}

fn toggle_voice_state(
    chathead: &gtk::DrawingArea,
    status: &gtk::Label,
    message: &gtk::Label,
    state: &OverlayState,
) {
    let next = match state.voice_state.get() {
        VoiceState::Idle => VoiceState::Listening,
        VoiceState::Listening => VoiceState::Idle,
    };

    state.voice_state.set(next);
    state.voice_changed_at.set(Instant::now());
    update_status_widgets(status, message, state);
    start_or_continue_animation(chathead, state);
}

fn stop_voice_state(
    chathead: &gtk::DrawingArea,
    status: &gtk::Label,
    message: &gtk::Label,
    state: &OverlayState,
) {
    if state.voice_state.get() == VoiceState::Idle {
        return;
    }

    state.voice_state.set(VoiceState::Idle);
    state.voice_changed_at.set(Instant::now());
    update_status_widgets(status, message, state);
    start_or_continue_animation(chathead, state);
}

fn update_status_widgets(status: &gtk::Label, message: &gtk::Label, state: &OverlayState) {
    match state.voice_state.get() {
        VoiceState::Listening => {
            status.set_label("Listening");
            status.set_css_classes(&["status", "status-listening"]);
            message.set_label(
                "Listening mode is active. Press the voice shortcut again, or press Esc in this panel to stop.",
            );
        }
        VoiceState::Idle => match &*state.shortcut_status.borrow() {
            ShortcutStatus::Registering => {
                status.set_label("Shortcut...");
                status.set_css_classes(&["status"]);
                message.set_label(&format!(
                    "Voice toggle is registering through the XDG portal. Preferred shortcut: {VOICE_TOGGLE_LABEL}."
                ));
            }
            ShortcutStatus::Ready(trigger) => {
                status.set_label("Ready");
                status.set_css_classes(&["status"]);
                message.set_label(&format!(
                    "Voice toggle: {trigger}. Press it to wake listening mode."
                ));
            }
            ShortcutStatus::ConflictPossible(details) => {
                status.set_label("Shortcut warning");
                status.set_css_classes(&["status", "status-warning"]);
                message.set_label(details);
            }
            ShortcutStatus::Unavailable(details) => {
                status.set_label("Shortcut off");
                status.set_css_classes(&["status", "status-error"]);
                message.set_label(details);
            }
        },
    }
}

fn start_or_continue_animation(chathead: &gtk::DrawingArea, state: &OverlayState) {
    if state.animation_source.borrow().is_some() {
        chathead.queue_draw();
        return;
    }

    let chathead = chathead.clone();
    let state_for_tick = state.clone();
    let animation_source = state.animation_source.clone();
    let source = glib::timeout_add_local(Duration::from_millis(ANIMATION_FRAME_MS), move || {
        chathead.queue_draw();

        let transition_active = state_for_tick
            .voice_changed_at
            .get()
            .elapsed()
            .as_secs_f64()
            < WAKE_ANIMATION_SECONDS;
        if state_for_tick.voice_state.get() == VoiceState::Listening || transition_active {
            glib::ControlFlow::Continue
        } else {
            animation_source.replace(None);
            glib::ControlFlow::Break
        }
    });

    state.animation_source.replace(Some(source));
}

fn attach_local_key_controller(panel: &gtk::Box, sender: mpsc::Sender<AppEvent>) {
    let key = gtk::EventControllerKey::new();
    key.connect_key_pressed(move |_, key, _, _| {
        if key == gdk::Key::Escape {
            let _ = sender.send(AppEvent::StopVoice);
            return glib::Propagation::Stop;
        }

        glib::Propagation::Proceed
    });
    panel.add_controller(key);
}

fn attach_drag_controller(
    window: &gtk::ApplicationWindow,
    canvas: &gtk::Fixed,
    chathead: &gtk::DrawingArea,
    panel: &gtk::Box,
    state: &OverlayState,
) {
    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    let window_for_begin = window.clone();
    let state_for_begin = state.clone();
    drag.connect_drag_begin(move |_, start_x, start_y| {
        if !point_is_in_chathead(start_x, start_y, &state_for_begin) {
            state_for_begin.dragging_chathead.set(false);
            return;
        }

        state_for_begin.dragging_chathead.set(true);
        state_for_begin.drag_start_x.set(state_for_begin.x.get());
        state_for_begin.drag_start_y.set(state_for_begin.y.get());
        set_full_input_region(&window_for_begin);
    });

    let canvas_for_update = canvas.clone();
    let chathead_for_update = chathead.clone();
    let panel_for_update = panel.clone();
    let state_for_update = state.clone();
    drag.connect_drag_update(move |_, offset_x, offset_y| {
        if !state_for_update.dragging_chathead.get() {
            return;
        }

        state_for_update
            .x
            .set(state_for_update.drag_start_x.get() + offset_x);
        state_for_update
            .y
            .set(state_for_update.drag_start_y.get() + offset_y);
        clamp_position(&canvas_for_update, &state_for_update);
        position_widgets(
            &canvas_for_update,
            &chathead_for_update,
            &panel_for_update,
            &state_for_update,
        );
    });

    let window_for_end = window.clone();
    let canvas_for_end = canvas.clone();
    let chathead_for_end = chathead.clone();
    let panel_for_end = panel.clone();
    let state_for_end = state.clone();
    drag.connect_drag_end(move |_, offset_x, offset_y| {
        if !state_for_end.dragging_chathead.replace(false) {
            return;
        }

        let distance = offset_x.abs() + offset_y.abs();
        if distance < CLICK_THRESHOLD {
            let open = !state_for_end.panel_open.get();
            state_for_end.panel_open.set(open);
            panel_for_end.set_visible(open);
            window_for_end.set_keyboard_mode(if open {
                KeyboardMode::OnDemand
            } else {
                KeyboardMode::None
            });
        }

        clamp_position(&canvas_for_end, &state_for_end);
        position_widgets(
            &canvas_for_end,
            &chathead_for_end,
            &panel_for_end,
            &state_for_end,
        );
        apply_idle_input_region(&window_for_end, &state_for_end);
    });

    let window_for_cancel = window.clone();
    let state_for_cancel = state.clone();
    drag.connect_cancel(move |_, _| {
        if state_for_cancel.dragging_chathead.replace(false) {
            apply_idle_input_region(&window_for_cancel, &state_for_cancel);
        }
    });

    canvas.add_controller(drag);
    chathead.set_can_focus(false);
}

fn start_shortcut_service(sender: mpsc::Sender<AppEvent>) {
    if let Err(error) = thread::Builder::new()
        .name("chathead-shortcuts".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build();

            match runtime {
                Ok(runtime) => runtime.block_on(run_shortcut_service(sender)),
                Err(error) => {
                    let _ = sender.send(AppEvent::ShortcutStatus(ShortcutStatus::Unavailable(
                        format!("Global shortcut runtime failed: {error}."),
                    )));
                }
            }
        })
    {
        eprintln!("failed to start shortcut service: {error}");
    }
}

async fn run_shortcut_service(sender: mpsc::Sender<AppEvent>) {
    if sender
        .send(AppEvent::ShortcutStatus(ShortcutStatus::Registering))
        .is_err()
    {
        return;
    }

    let result = register_and_listen_for_shortcuts(sender.clone()).await;
    if let Err(error) = result {
        let _ = sender.send(AppEvent::ShortcutStatus(ShortcutStatus::Unavailable(
            format!("Global shortcut unavailable: {error}."),
        )));
    }
}

async fn register_and_listen_for_shortcuts(
    sender: mpsc::Sender<AppEvent>,
) -> Result<(), ashpd::Error> {
    let portal = GlobalShortcuts::new().await?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await?;
    let activated = portal.receive_activated().await?;
    let shortcuts = [NewShortcut::new(VOICE_TOGGLE_ID, "Toggle voice listening")
        .preferred_trigger(Some(VOICE_TOGGLE_TRIGGER))];

    let request = portal
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await?;
    let response = request.response()?;
    let trigger = response
        .shortcuts()
        .iter()
        .find(|shortcut| shortcut.id() == VOICE_TOGGLE_ID)
        .map(|shortcut| shortcut.trigger_description().trim())
        .filter(|trigger| !trigger.is_empty())
        .unwrap_or(VOICE_TOGGLE_LABEL);

    if response
        .shortcuts()
        .iter()
        .any(|shortcut| shortcut.id() == VOICE_TOGGLE_ID)
    {
        if sender
            .send(AppEvent::ShortcutStatus(ShortcutStatus::Ready(
                trigger.to_owned(),
            )))
            .is_err()
        {
            return Ok(());
        }
    } else if sender
        .send(AppEvent::ShortcutStatus(ShortcutStatus::ConflictPossible(
            "The voice shortcut was not bound by the desktop portal. Choose another shortcut or configure your compositor to route it to ChatHead AI.".to_owned(),
        )))
        .is_err()
    {
        return Ok(());
    }

    futures_util::pin_mut!(activated);
    let mut last_activation = Instant::now() - Duration::from_millis(SHORTCUT_DEBOUNCE_MS);
    while let Some(event) = activated.next().await {
        if event.shortcut_id() != VOICE_TOGGLE_ID {
            continue;
        }

        let now = Instant::now();
        if now.duration_since(last_activation) < Duration::from_millis(SHORTCUT_DEBOUNCE_MS) {
            continue;
        }
        last_activation = now;

        if sender.send(AppEvent::ToggleVoice).is_err() {
            break;
        }
    }

    Ok(())
}

fn point_is_in_chathead(x: f64, y: f64, state: &OverlayState) -> bool {
    let chathead_x = state.x.get();
    let chathead_y = state.y.get();
    x >= chathead_x
        && x <= chathead_x + CHATHEAD_SIZE as f64
        && y >= chathead_y
        && y <= chathead_y + CHATHEAD_SIZE as f64
}

fn clamp_position(canvas: &gtk::Fixed, state: &OverlayState) {
    let max_x = (canvas.allocated_width() - CHATHEAD_SIZE).max(0) as f64;
    let max_y = (canvas.allocated_height() - CHATHEAD_SIZE).max(0) as f64;
    state.x.set(state.x.get().clamp(0.0, max_x));
    state.y.set(state.y.get().clamp(0.0, max_y));
}

fn position_widgets(
    canvas: &gtk::Fixed,
    chathead: &gtk::DrawingArea,
    panel: &gtk::Box,
    state: &OverlayState,
) {
    let (panel_x, panel_y) =
        panel_position(canvas.allocated_width(), canvas.allocated_height(), state);
    canvas.move_(panel, panel_x, panel_y);
    canvas.move_(chathead, state.x.get(), state.y.get());
}

fn preferred_x(width: i32, position: OverlayPosition) -> f64 {
    let max_x = (width - CHATHEAD_SIZE).max(0);
    match position {
        OverlayPosition::Left => EDGE_PADDING.min(max_x),
        OverlayPosition::Right => (max_x - EDGE_PADDING).max(0),
    }
    .into()
}

fn apply_preferred_position(runtime: &PositionRuntime, position: OverlayPosition) {
    runtime
        .state
        .x
        .set(preferred_x(runtime.canvas.allocated_width(), position));
    clamp_position(&runtime.canvas, &runtime.state);
    position_widgets(
        &runtime.canvas,
        &runtime.chathead,
        &runtime.panel,
        &runtime.state,
    );
    apply_idle_input_region(&runtime.window, &runtime.state);
}

fn draw_companion_orb(
    context: &cairo::Context,
    width: i32,
    height: i32,
    voice_state: VoiceState,
    elapsed: Duration,
    theme: OrbTheme,
) {
    let size = f64::from(width.min(height));
    let center = size / 2.0;
    let radius = size * 0.34;
    let seconds = elapsed.as_secs_f64();
    let listening = voice_state == VoiceState::Listening;
    let pulse = if listening {
        ((seconds * std::f64::consts::TAU * 1.35).sin() + 1.0) * 0.5
    } else {
        0.0
    };
    let wake = (1.0 - (seconds / WAKE_ANIMATION_SECONDS)).clamp(0.0, 1.0);

    context.set_operator(cairo::Operator::Over);
    draw_ambient_glow(context, center, size, listening, pulse, wake, theme);
    draw_orb_shadow(context, center, size, theme);

    let scale = 1.0 + wake * 0.05 + if listening { pulse * 0.018 } else { 0.0 };
    let _ = context.save();
    context.translate(center, center);
    context.scale(scale, scale);
    context.translate(-center, -center);
    draw_orb_shell(context, center, radius, theme);
    draw_face_plate(context, center, size, listening, pulse, theme);
    let _ = context.restore();
}

fn draw_ambient_glow(
    context: &cairo::Context,
    center: f64,
    size: f64,
    listening: bool,
    pulse: f64,
    wake: f64,
    theme: OrbTheme,
) {
    let glow_radius = size * (0.52 + pulse * 0.08 + wake * 0.12);
    let glow = cairo::RadialGradient::new(center, center, size * 0.18, center, center, glow_radius);
    let intensity = if listening {
        0.24 + pulse * 0.1
    } else if theme == OrbTheme::Dark {
        0.22
    } else {
        0.15
    } + wake * 0.14;
    glow.add_color_stop_rgba(0.0, 0.43, 0.55, 1.0, intensity);
    glow.add_color_stop_rgba(0.58, 0.43, 0.55, 1.0, intensity * 0.45);
    glow.add_color_stop_rgba(1.0, 0.43, 0.55, 1.0, 0.0);

    let _ = context.set_source(&glow);
    context.arc(center, center, glow_radius, 0.0, std::f64::consts::TAU);
    let _ = context.fill();

    if listening {
        context.set_line_width(1.2 + pulse * 1.2);
        context.set_source_rgba(0.55, 0.68, 1.0, 0.18 + pulse * 0.14);
        context.arc(
            center,
            center,
            size * (0.43 + pulse * 0.06),
            0.0,
            std::f64::consts::TAU,
        );
        let _ = context.stroke();
    }
}

fn draw_orb_shadow(context: &cairo::Context, center: f64, size: f64, theme: OrbTheme) {
    let shadow = cairo::RadialGradient::new(
        center + size * 0.02,
        center + size * 0.18,
        size * 0.06,
        center + size * 0.02,
        center + size * 0.2,
        size * 0.42,
    );
    let shadow_alpha = if theme == OrbTheme::Dark { 0.48 } else { 0.26 };
    shadow.add_color_stop_rgba(0.0, 0.08, 0.18, 0.42, shadow_alpha);
    shadow.add_color_stop_rgba(0.55, 0.08, 0.18, 0.42, shadow_alpha * 0.46);
    shadow.add_color_stop_rgba(1.0, 0.24, 0.33, 0.72, 0.0);

    let _ = context.set_source(&shadow);
    context.arc(
        center,
        center + size * 0.15,
        size * 0.41,
        0.0,
        std::f64::consts::TAU,
    );
    let _ = context.fill();
}

fn draw_orb_shell(context: &cairo::Context, center: f64, radius: f64, theme: OrbTheme) {
    let shell = cairo::RadialGradient::new(
        center - radius * 0.34,
        center - radius * 0.42,
        radius * 0.05,
        center,
        center + radius * 0.08,
        radius * 1.08,
    );
    if theme == OrbTheme::Dark {
        shell.add_color_stop_rgba(0.0, 0.29, 0.46, 0.72, 1.0);
        shell.add_color_stop_rgba(0.42, 0.10, 0.20, 0.36, 1.0);
        shell.add_color_stop_rgba(0.8, 0.035, 0.08, 0.17, 1.0);
        shell.add_color_stop_rgba(1.0, 0.015, 0.04, 0.10, 1.0);
    } else {
        shell.add_color_stop_rgba(0.0, 1.0, 1.0, 1.0, 1.0);
        shell.add_color_stop_rgba(0.48, 0.96, 0.98, 1.0, 1.0);
        shell.add_color_stop_rgba(0.82, 0.86, 0.91, 1.0, 1.0);
        shell.add_color_stop_rgba(1.0, 0.72, 0.80, 0.95, 1.0);
    }

    let _ = context.set_source(&shell);
    context.arc(center, center, radius, 0.0, std::f64::consts::TAU);
    let _ = context.fill();

    let inner_shade = cairo::RadialGradient::new(
        center,
        center + radius * 0.2,
        radius * 0.2,
        center,
        center + radius * 0.2,
        radius * 0.95,
    );
    inner_shade.add_color_stop_rgba(0.0, 0.36, 0.46, 0.8, 0.0);
    inner_shade.add_color_stop_rgba(
        1.0,
        0.36,
        0.46,
        0.8,
        if theme == OrbTheme::Dark { 0.28 } else { 0.12 },
    );
    let _ = context.set_source(&inner_shade);
    context.arc(center, center, radius * 0.95, 0.0, std::f64::consts::TAU);
    let _ = context.fill();

    context.set_line_width(1.4);
    context.set_source_rgba(
        0.72,
        0.86,
        1.0,
        if theme == OrbTheme::Dark { 0.66 } else { 0.78 },
    );
    context.arc(
        center - radius * 0.08,
        center - radius * 0.18,
        radius * 0.72,
        3.85,
        5.55,
    );
    let _ = context.stroke();
}

fn draw_face_plate(
    context: &cairo::Context,
    center: f64,
    size: f64,
    listening: bool,
    pulse: f64,
    theme: OrbTheme,
) {
    let alert = if listening { 1.0 } else { 0.0 };
    let plate_width = size * (0.36 + alert * 0.035);
    let plate_height = size * (0.25 + alert * 0.02);
    let plate_x = center - plate_width / 2.0;
    let plate_y = center - plate_height / 2.0 + size * 0.01;
    let plate_radius = plate_height * 0.48;

    let plate_shadow = cairo::RadialGradient::new(
        center,
        center + size * 0.05,
        size * 0.05,
        center,
        center + size * 0.05,
        size * 0.22,
    );
    plate_shadow.add_color_stop_rgba(0.0, 0.2, 0.35, 0.95, 0.2);
    plate_shadow.add_color_stop_rgba(1.0, 0.2, 0.35, 0.95, 0.0);
    let _ = context.set_source(&plate_shadow);
    rounded_rect(
        context,
        plate_x - size * 0.03,
        plate_y + size * 0.03,
        plate_width + size * 0.06,
        plate_height + size * 0.05,
        plate_radius + size * 0.02,
    );
    let _ = context.fill();

    let plate = cairo::LinearGradient::new(plate_x, plate_y, plate_x + plate_width, plate_y);
    plate.add_color_stop_rgba(0.0, 0.37, 0.52, 1.0, 1.0);
    plate.add_color_stop_rgba(0.55, 0.25, 0.37, 0.97, 1.0);
    plate.add_color_stop_rgba(1.0, 0.22, 0.47, 1.0, 1.0);

    let _ = context.set_source(&plate);
    rounded_rect(
        context,
        plate_x,
        plate_y,
        plate_width,
        plate_height,
        plate_radius,
    );
    let _ = context.fill();

    if listening {
        context.set_line_width(1.0);
        context.set_source_rgba(0.74, 0.83, 1.0, 0.32 + pulse * 0.22);
        rounded_rect(
            context,
            plate_x - size * 0.015,
            plate_y - size * 0.015,
            plate_width + size * 0.03,
            plate_height + size * 0.03,
            plate_radius + size * 0.012,
        );
        let _ = context.stroke();
    }

    let face_width = plate_width * 0.62;
    let face_height = plate_height * 0.68;
    let face_x = center - face_width / 2.0;
    let face_y = center - face_height / 2.0 + size * 0.01;
    if theme == OrbTheme::Dark {
        context.set_source_rgba(0.08, 0.14, 0.25, 0.98);
    } else {
        context.set_source_rgba(0.97, 0.99, 1.0, 0.96);
    }
    rounded_rect(
        context,
        face_x,
        face_y,
        face_width,
        face_height,
        face_height * 0.48,
    );
    let _ = context.fill();

    let eye_y = center + size * 0.015;
    let eye_offset = size * (0.055 + alert * 0.008);
    let eye_radius = size * (0.025 + alert * 0.004 + pulse * 0.004);

    if theme == OrbTheme::Dark {
        context.set_source_rgba(0.45, 0.72, 1.0, 1.0);
    } else {
        context.set_source_rgba(0.25, 0.36, 0.94, 1.0);
    }
    for eye_x in [center - eye_offset, center + eye_offset] {
        context.arc(eye_x, eye_y, eye_radius, 0.0, std::f64::consts::TAU);
        let _ = context.fill();
    }

    if listening {
        context.set_source_rgba(0.78, 0.86, 1.0, 0.45 + pulse * 0.25);
        context.arc(
            center + eye_offset * 0.35,
            eye_y - eye_radius * 0.38,
            eye_radius * 0.32,
            0.0,
            std::f64::consts::TAU,
        );
        let _ = context.fill();
    }
}

fn rounded_rect(context: &cairo::Context, x: f64, y: f64, width: f64, height: f64, radius: f64) {
    let right = x + width;
    let bottom = y + height;

    context.new_sub_path();
    context.arc(
        right - radius,
        y + radius,
        radius,
        -std::f64::consts::FRAC_PI_2,
        0.0,
    );
    context.arc(
        right - radius,
        bottom - radius,
        radius,
        0.0,
        std::f64::consts::FRAC_PI_2,
    );
    context.arc(
        x + radius,
        bottom - radius,
        radius,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
    );
    context.arc(
        x + radius,
        y + radius,
        radius,
        std::f64::consts::PI,
        std::f64::consts::PI * 1.5,
    );
    context.close_path();
}

fn panel_position(width: i32, height: i32, state: &OverlayState) -> (f64, f64) {
    let width = width.max(PANEL_WIDTH + EDGE_PADDING * 2);
    let height = height.max(PANEL_HEIGHT + EDGE_PADDING * 2);
    let x = if state.x.get() + PANEL_WIDTH as f64 <= width as f64 {
        state.x.get()
    } else {
        state.x.get() + CHATHEAD_SIZE as f64 - PANEL_WIDTH as f64
    };
    let y = if state.y.get() + CHATHEAD_SIZE as f64 + PANEL_GAP as f64 + PANEL_HEIGHT as f64
        <= height as f64
    {
        state.y.get() + CHATHEAD_SIZE as f64 + PANEL_GAP as f64
    } else {
        state.y.get() - PANEL_HEIGHT as f64 - PANEL_GAP as f64
    };

    (
        x.clamp(
            EDGE_PADDING as f64,
            (width - PANEL_WIDTH - EDGE_PADDING).max(0) as f64,
        ),
        y.clamp(
            EDGE_PADDING as f64,
            (height - PANEL_HEIGHT - EDGE_PADDING).max(0) as f64,
        ),
    )
}

fn set_full_input_region(window: &gtk::ApplicationWindow) {
    if let Some(surface) = window.surface() {
        surface.set_input_region(None);
    }
}

fn apply_idle_input_region(window: &gtk::ApplicationWindow, state: &OverlayState) {
    let Some(surface) = window.surface() else {
        return;
    };

    let region = cairo::Region::create();
    let chathead_rect = cairo::RectangleInt::new(
        state.x.get().round() as i32,
        state.y.get().round() as i32,
        CHATHEAD_SIZE,
        CHATHEAD_SIZE,
    );
    let _ = region.union_rectangle(&chathead_rect);

    if state.panel_open.get() {
        let (panel_x, panel_y) = panel_position(surface.width(), surface.height(), state);
        let panel_rect = cairo::RectangleInt::new(
            panel_x.round() as i32,
            panel_y.round() as i32,
            PANEL_WIDTH,
            PANEL_HEIGHT,
        );
        let _ = region.union_rectangle(&panel_rect);
    }

    surface.set_input_region(Some(&region));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_stays_within_output_edges() {
        let state = OverlayState::new();
        state.x.set(1900.0);
        state.y.set(1000.0);
        let (x, y) = panel_position(1920, 1080, &state);
        assert!(x >= f64::from(EDGE_PADDING));
        assert!(y >= f64::from(EDGE_PADDING));
        assert!(x + f64::from(PANEL_WIDTH) <= f64::from(1920 - EDGE_PADDING));
        assert!(y + f64::from(PANEL_HEIGHT) <= f64::from(1080 - EDGE_PADDING));
    }

    #[test]
    fn chathead_hit_testing_uses_current_position() {
        let state = OverlayState::new();
        state.x.set(40.0);
        state.y.set(50.0);
        assert!(point_is_in_chathead(60.0, 70.0, &state));
        assert!(!point_is_in_chathead(10.0, 10.0, &state));
    }

    #[test]
    fn preferred_position_respects_both_output_edges() {
        assert_eq!(preferred_x(1920, OverlayPosition::Left), 12.0);
        assert_eq!(preferred_x(1920, OverlayPosition::Right), 1824.0);
    }

    #[test]
    fn preferred_position_handles_outputs_smaller_than_the_chathead() {
        assert_eq!(preferred_x(60, OverlayPosition::Left), 0.0);
        assert_eq!(preferred_x(60, OverlayPosition::Right), 0.0);
    }
}
