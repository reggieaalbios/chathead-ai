//! GTK4 layer-shell orb, panel, input regions, drag handling, and shortcut service.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        atomic::{AtomicU8, AtomicU16, Ordering},
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
    ExperimentalChatState, IpcEvent, MessageRole, MessageState, PANEL_HEIGHT_DEFAULT,
    PANEL_HEIGHT_MAX, PANEL_HEIGHT_MIN, PANEL_WIDTH_DEFAULT, PANEL_WIDTH_MAX, PANEL_WIDTH_MIN,
    PROTOCOL_VERSION, PanelSize, PanelZoom, ShortcutStatus as ProtocolShortcutStatus, VoicePhase,
    VoiceSnapshot, VoiceSubmissionMode,
};
use chathead_voice::{VoiceEvent, VoiceService};
use futures_util::StreamExt;
use gtk::{cairo, gdk, glib, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde::Deserialize;

use crate::response_view::{
    AssistantDocument, DocumentTheme, HighlightWorker, LinkHandler, RetryHandler,
    copy_text_to_clipboard, open_confirmed_uri,
};

const VOICE_TOGGLE_ID: &str = "voice_toggle";
const VOICE_TOGGLE_TRIGGER: &str = "LOGO+e";
const VOICE_TOGGLE_LABEL: &str = "Super+E";
const PANEL_TOGGLE_ID: &str = "panel_toggle";
const PANEL_TOGGLE_TRIGGER: &str = "LOGO+w";
const PANEL_TOGGLE_LABEL: &str = "Super+W";
const CHATHEAD_SIZE: i32 = 84;
const PANEL_GAP: i32 = 10;
const EDGE_PADDING: i32 = 16;
const RESIZE_EDGE_HIT_ZONE: f64 = 12.0;
const RESIZE_CORNER_HIT_ZONE: f64 = 24.0;
const CLICK_THRESHOLD: f64 = 5.0;
const ACTION_POLL_MS: u64 = 40;
const ANIMATION_FRAME_MS: u64 = 33;
const STREAM_RENDER_INTERVAL_MS: u64 = 33;
const VOICE_SEND_DELAY: Duration = Duration::from_millis(700);
const WAKE_ANIMATION_SECONDS: f64 = 0.36;
static ORB_THEME: AtomicU8 = AtomicU8::new(0);
static PANEL_POSITION: AtomicU8 = AtomicU8::new(1);
static PANEL_ZOOM: AtomicU8 = AtomicU8::new(100);
static PANEL_WIDTH: AtomicU16 = AtomicU16::new(PANEL_WIDTH_DEFAULT);
static PANEL_HEIGHT: AtomicU16 = AtomicU16::new(PANEL_HEIGHT_DEFAULT);

thread_local! {
    static PANEL_ZOOM_CSS: RefCell<Option<gtk::CssProvider>> = const { RefCell::new(None) };
    static PANEL_RUNTIME: RefCell<Option<PanelRuntime>> = const { RefCell::new(None) };
    static OVERLAY_RUNTIME: RefCell<Option<OverlayRuntime>> = const { RefCell::new(None) };
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PanelMetrics {
    panel_padding: i32,
    panel_spacing: i32,
    row_spacing: i32,
    transcript_spacing: i32,
    composer_height: i32,
    composer_view_height: i32,
    compact_target: i32,
    send_target: i32,
    title_font: i32,
    body_font: i32,
    small_font: i32,
}

impl PanelMetrics {
    fn for_zoom(zoom: PanelZoom) -> Self {
        let scale = f64::from(zoom.value()) / 100.0;
        let scaled = |base: i32| (f64::from(base) * scale).round() as i32;
        Self {
            panel_padding: scaled(14),
            panel_spacing: scaled(12).max(8),
            row_spacing: scaled(8).max(6),
            transcript_spacing: scaled(18).max(12),
            composer_height: scaled(46).max(36),
            composer_view_height: scaled(42).max(32),
            compact_target: scaled(28).max(28),
            send_target: scaled(36).max(28),
            title_font: scaled(14).max(10),
            body_font: scaled(13).max(10),
            small_font: scaled(10).max(10),
        }
    }
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
pub(crate) enum PanelPosition {
    Left,
    Right,
}

impl PanelPosition {
    const fn value(self) -> u8 {
        match self {
            Self::Left => 0,
            Self::Right => 1,
        }
    }

    const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
        }
    }

    fn current() -> Self {
        if PANEL_POSITION.load(Ordering::Relaxed) == Self::Right.value() {
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
    PANEL_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            let anchor = TranscriptAnchor::capture(&runtime.widgets.transcript_scroll);
            for rendered in runtime.rendered_messages.borrow().iter() {
                if let Some(document) = &rendered.document {
                    document.update(
                        &rendered.rendered_text,
                        rendered.rendered_state,
                        theme.into(),
                    );
                }
            }
            let adjustment = runtime.widgets.transcript_scroll.vadjustment();
            glib::idle_add_local_once(move || anchor.restore(&adjustment));
        }
    });
}

impl From<OrbTheme> for DocumentTheme {
    fn from(theme: OrbTheme) -> Self {
        match theme {
            OrbTheme::Light => Self::Light,
            OrbTheme::Dark => Self::Dark,
        }
    }
}

pub(crate) fn set_native_panel_position(position: PanelPosition) {
    PANEL_POSITION.store(position.value(), Ordering::Relaxed);
    OVERLAY_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            runtime.state.panel_rect_override.set(None);
            apply_panel_position(runtime);
        }
    });
}

pub(crate) fn set_native_panel_zoom(zoom: PanelZoom) {
    let zoom_value = u8::try_from(zoom.value()).expect("validated panel zoom fits in u8");
    let previous = PANEL_ZOOM.swap(zoom_value, Ordering::Relaxed);
    if previous == zoom_value {
        update_panel_zoom_css(zoom);
        return;
    }
    PANEL_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            apply_panel_metrics(runtime, zoom);
        }
    });
    update_panel_zoom_css(zoom);
}

fn current_panel_zoom() -> PanelZoom {
    PanelZoom::try_from(u16::from(PANEL_ZOOM.load(Ordering::Relaxed))).unwrap_or(PanelZoom::DEFAULT)
}

pub(crate) fn set_native_panel_size(size: PanelSize) {
    if size == current_panel_size() {
        return;
    }
    store_panel_size(size);
    OVERLAY_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            let effective = effective_panel_size(
                size,
                runtime.canvas.allocated_width(),
                runtime.canvas.allocated_height(),
            );
            store_panel_size(effective);
            runtime.state.panel_size.set(effective);
            runtime.state.panel_rect_override.set(None);
            PANEL_RUNTIME.with(|panel_stored| {
                if let Some(panel_runtime) = panel_stored.borrow().as_ref() {
                    apply_panel_dimensions(panel_runtime, effective);
                }
            });
            apply_panel_position(runtime);
            if effective != size {
                emit_panel_size_changed(effective);
            }
        }
    });
}

fn current_panel_size() -> PanelSize {
    PanelSize::try_new(
        PANEL_WIDTH.load(Ordering::Relaxed),
        PANEL_HEIGHT.load(Ordering::Relaxed),
    )
    .unwrap_or_default()
}

fn store_panel_size(size: PanelSize) {
    PANEL_WIDTH.store(size.width(), Ordering::Relaxed);
    PANEL_HEIGHT.store(size.height(), Ordering::Relaxed);
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
        let zoom_provider = gtk::CssProvider::new();
        gtk::style_context_add_provider_for_display(
            &display,
            &zoom_provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION + 1,
        );
        PANEL_ZOOM_CSS.with(|stored| stored.replace(Some(zoom_provider)));
        update_panel_zoom_css(current_panel_zoom());
    }
}

fn update_panel_zoom_css(zoom: PanelZoom) {
    let metrics = PanelMetrics::for_zoom(zoom);
    let scale = f64::from(zoom.value()) / 100.0;
    let px = |base: i32| ((f64::from(base) * scale).round() as i32).max(10);
    let css = format!(
        ".chat-panel {{ padding: {}px; }}\n\
         .chat-panel .panel-title {{ font-size: {}px; }}\n\
         .chat-panel .provider-status, .chat-panel .voice-message {{ font-size: {}px; }}\n\
         .chat-panel .thinking-label, .chat-panel .chat-failure, .chat-panel .prompt-placeholder {{ font-size: {}px; }}\n\
         .chat-panel .chat-info {{ font-size: {}px; }}\n\
         .chat-panel .chat-bubble {{ padding: {}px {}px; border-radius: {}px; font-size: {}px; }}\n\
         .chat-panel .thinking-bubble {{ padding: {}px {}px; }}\n\
         .chat-panel .thinking-dot {{ font-size: {}px; }}\n\
         .chat-panel .response-paragraph, .chat-panel .response-table label, .chat-panel .response-list label, .chat-panel .response-quote label, .chat-panel .response-footnote label, .chat-panel .definition-list label {{ font-size: {}px; }}\n\
         .chat-panel .response-h1 {{ font-size: {}px; }}\n\
         .chat-panel .response-h2 {{ font-size: {}px; }}\n\
         .chat-panel .response-h3 {{ font-size: {}px; }}\n\
         .chat-panel .response-h4, .chat-panel .response-h5, .chat-panel .response-h6 {{ font-size: {}px; }}\n\
         .chat-panel .code-content, .chat-panel .code-language {{ font-size: {}px; }}\n\
         .chat-panel .assistant-document > box > label, .chat-panel .assistant-document > box > box, .chat-panel .assistant-document > box > scrolledwindow, .chat-panel .assistant-document > box > separator {{ margin-bottom: {}px; }}\n\
         .chat-panel .composer-bar {{ min-height: {}px; padding: {}px; border-radius: {}px; }}\n\
         .chat-panel .prompt-frame {{ min-height: {}px; }}\n\
         .chat-panel .prompt-input, .chat-panel .prompt-input text {{ min-height: {}px; padding: {}px {}px; font-size: {}px; }}\n\
         .chat-panel .new-chat {{ min-width: {}px; min-height: {}px; font-size: {}px; }}\n\
         .chat-panel .send-button {{ min-width: {}px; min-height: {}px; font-size: {}px; }}\n\
         .chat-panel button:not(.new-chat):not(.send-button):not(.response-action) {{ min-height: {}px; padding-left: {}px; padding-right: {}px; font-size: {}px; }}",
        metrics.panel_padding,
        metrics.title_font,
        metrics.small_font,
        px(11),
        px(12),
        (9.0 * scale).round() as i32,
        (11.0 * scale).round() as i32,
        (8.0 * scale).round() as i32,
        metrics.body_font,
        (7.0 * scale).round() as i32,
        (10.0 * scale).round() as i32,
        metrics.small_font,
        px(14),
        px(18),
        px(16),
        px(15),
        px(14),
        px(12),
        (10.0 * scale).round() as i32,
        metrics.composer_height,
        (5.0 * scale).round() as i32,
        (11.0 * scale).round() as i32,
        metrics.composer_height - 2,
        metrics.composer_view_height,
        (5.0 * scale).round() as i32,
        (6.0 * scale).round() as i32,
        metrics.body_font,
        metrics.compact_target,
        metrics.compact_target,
        px(15),
        metrics.send_target,
        metrics.send_target,
        px(14),
        metrics.compact_target,
        (8.0 * scale).round() as i32,
        (8.0 * scale).round() as i32,
        px(11),
    );
    PANEL_ZOOM_CSS.with(|stored| {
        if let Some(provider) = stored.borrow().as_ref() {
            provider.load_from_data(&css);
        }
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum VoiceState {
    Idle,
    Listening,
}

enum AppEvent {
    CancelVoice,
    VoiceShortcutActivated,
    VoiceShortcutDeactivated,
    TogglePanel,
    ShortcutStatus(ShortcutAction, ShortcutStatus),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ShortcutAction {
    Voice,
    Panel,
}

pub(crate) struct ShortcutStatusUpdate {
    pub(crate) action: ShortcutAction,
    pub(crate) status: ProtocolShortcutStatus,
}

#[derive(Clone)]
enum ShortcutStatus {
    Registering,
    Ready(String),
    ConflictPossible(String),
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PanelRect {
    x: f64,
    y: f64,
    width: i32,
    height: i32,
}

impl PanelRect {
    fn size(self) -> PanelSize {
        PanelSize::try_new(
            u16::try_from(self.width).expect("bounded panel width fits in u16"),
            u16::try_from(self.height).expect("bounded panel height fits in u16"),
        )
        .expect("panel rectangle is always bounded")
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ResizeEdge {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

impl ResizeEdge {
    const fn moves_left(self) -> bool {
        matches!(self, Self::West | Self::NorthWest | Self::SouthWest)
    }

    const fn moves_right(self) -> bool {
        matches!(self, Self::East | Self::NorthEast | Self::SouthEast)
    }

    const fn moves_top(self) -> bool {
        matches!(self, Self::North | Self::NorthEast | Self::NorthWest)
    }

    const fn moves_bottom(self) -> bool {
        matches!(self, Self::South | Self::SouthEast | Self::SouthWest)
    }

    const fn cursor_name(self) -> &'static str {
        match self {
            Self::North => "n-resize",
            Self::NorthEast => "ne-resize",
            Self::East => "e-resize",
            Self::SouthEast => "se-resize",
            Self::South => "s-resize",
            Self::SouthWest => "sw-resize",
            Self::West => "w-resize",
            Self::NorthWest => "nw-resize",
        }
    }
}

#[derive(Clone)]
struct OverlayState {
    x: Rc<Cell<f64>>,
    y: Rc<Cell<f64>>,
    drag_start_x: Rc<Cell<f64>>,
    drag_start_y: Rc<Cell<f64>>,
    dragging_chathead: Rc<Cell<bool>>,
    panel_open: Rc<Cell<bool>>,
    panel_keyboard_captured: Rc<Cell<bool>>,
    panel_size: Rc<Cell<PanelSize>>,
    panel_rect_override: Rc<Cell<Option<PanelRect>>>,
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
            panel_keyboard_captured: Rc::new(Cell::new(false)),
            panel_size: Rc::new(Cell::new(current_panel_size())),
            panel_rect_override: Rc::new(Cell::new(None)),
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
    header: gtk::Box,
    message: gtk::Label,
    chat_status: gtk::Label,
    transcript: gtk::Box,
    transcript_scroll: gtk::ScrolledWindow,
    failure_row: gtk::Box,
    link_confirmation: gtk::Box,
    link_destination: gtk::Label,
    link_cancel: gtk::Button,
    link_open: gtk::Button,
    composer_row: gtk::Box,
    composer_scroll: gtk::ScrolledWindow,
    composer: gtk::TextView,
    composer_placeholder: gtk::Label,
    send: gtk::Button,
    retry: gtk::Button,
    failure: gtk::Label,
    info: gtk::Label,
    open_settings: gtk::Button,
}

#[derive(Clone)]
struct PanelRuntime {
    widgets: PanelWidgets,
    conversation: Rc<RefCell<Conversation>>,
    codex: CodexAppServer,
    chat_state: Rc<Cell<ExperimentalChatState>>,
    chat_message: Rc<RefCell<Option<String>>>,
    failure: Rc<RefCell<Option<String>>>,
    rendered_messages: Rc<RefCell<Vec<RenderedMessage>>>,
    stream_render_source: Rc<RefCell<Option<glib::SourceId>>>,
    highlight_worker: HighlightWorker,
    pending_link: Rc<RefCell<Option<String>>>,
    voice: VoiceService,
    pending_voice: Rc<RefCell<PendingVoice>>,
    output: super::Output,
}

#[derive(Clone)]
struct RenderedMessage {
    id: String,
    rendered_text: String,
    rendered_state: MessageState,
    revision: u64,
    row: gtk::Box,
    label: Option<gtk::Label>,
    document: Option<AssistantDocument>,
}

#[derive(Default)]
struct PendingVoice {
    utterance_id: Option<u64>,
}

impl PendingVoice {
    fn arm(&mut self, utterance_id: u64) {
        self.utterance_id = Some(utterance_id);
    }

    fn cancel(&mut self) -> bool {
        self.utterance_id.take().is_some()
    }

    fn consume(&mut self, utterance_id: u64) -> bool {
        if self.utterance_id == Some(utterance_id) {
            self.utterance_id = None;
            true
        } else {
            false
        }
    }
}

#[derive(Clone)]
struct OverlayRuntime {
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
    voice: VoiceService,
    output: super::Output,
    shortcut_status_sender: mpsc::Sender<ShortcutStatusUpdate>,
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
    chathead.set_cursor_from_name(Some("pointer"));
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

    let panel_widgets = build_panel(&output);
    let panel_runtime = PanelRuntime {
        widgets: panel_widgets.clone(),
        conversation: Rc::new(RefCell::new(Conversation::default())),
        codex,
        chat_state: Rc::new(Cell::new(chat.state)),
        chat_message: Rc::new(RefCell::new(chat.message)),
        failure: Rc::new(RefCell::new(None)),
        rendered_messages: Rc::new(RefCell::new(Vec::new())),
        stream_render_source: Rc::new(RefCell::new(None)),
        highlight_worker: HighlightWorker::start(),
        pending_link: Rc::new(RefCell::new(None)),
        voice: voice.clone(),
        pending_voice: Rc::new(RefCell::new(PendingVoice::default())),
        output: output.clone(),
    };
    apply_panel_metrics(&panel_runtime, current_panel_zoom());
    apply_panel_dimensions(&panel_runtime, current_panel_size());
    wire_chat_controls(&panel_runtime);
    attach_panel_zoom_controllers(&panel_runtime);
    render_chat(&panel_runtime);
    PANEL_RUNTIME.with(|stored| stored.replace(Some(panel_runtime.clone())));
    start_highlight_result_pump();
    let panel = panel_widgets.container.clone();
    panel.set_visible(false);
    canvas.put(&panel, 0.0, 0.0);
    canvas.put(&chathead, state.x.get(), state.y.get());
    window.set_child(Some(&canvas));

    attach_hover_cursor(&canvas, &state);
    attach_drag_controller(&window, &canvas, &chathead, &panel, &state);
    attach_focus_dismiss_controller(&window, &canvas, &state);
    attach_panel_resize_controller(&window, &canvas, &panel_runtime, &state);
    attach_local_key_controller(&panel, event_sender.clone());

    let window_for_realize = window.clone();
    let state_for_realize = state.clone();
    window.connect_realize(move |_| {
        apply_idle_input_region(&window_for_realize, &state_for_realize);
    });

    window.present();

    OVERLAY_RUNTIME.with(|stored| {
        stored.replace(Some(OverlayRuntime {
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
    let panel_runtime_for_idle = panel_runtime.clone();
    glib::idle_add_local_once(move || {
        clamp_position(&canvas_for_idle, &state_for_idle);
        let requested = state_for_idle.panel_size.get();
        let effective = effective_panel_size(
            requested,
            canvas_for_idle.allocated_width(),
            canvas_for_idle.allocated_height(),
        );
        if effective != requested {
            state_for_idle.panel_size.set(effective);
            store_panel_size(effective);
            apply_panel_dimensions(&panel_runtime_for_idle, effective);
            emit_panel_size_changed(effective);
        }
        position_widgets(
            &canvas_for_idle,
            &chathead_for_idle,
            &panel_for_idle,
            &state_for_idle,
        );
        apply_idle_input_region(&window_for_idle, &state_for_idle);
    });

    attach_app_event_pump(
        event_receiver,
        &panel_widgets.message,
        &state,
        shortcut_status_sender,
    );
    start_shortcut_service(event_sender);
    handle_voice_event(app, &VoiceEvent::Snapshot(voice.snapshot()));
    Ok(())
}

pub(crate) fn stop_native_overlay(app: &gtk::Application) {
    PANEL_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.take() {
            runtime.conversation.borrow_mut().new_chat();
            let _ = runtime.codex.send(CodexCommand::NewChat);
        }
    });
    OVERLAY_RUNTIME.with(|stored| {
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

fn build_panel(output: &super::Output) -> PanelWidgets {
    let panel_size = current_panel_size();
    let panel = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .width_request(i32::from(panel_size.width()))
        .height_request(i32::from(panel_size.height()))
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
    let chat_status = gtk::Label::builder()
        .label("● Checking · ChatGPT")
        .css_classes(["provider-status"])
        .build();
    let new_chat = gtk::Button::builder()
        .label("＋")
        .tooltip_text("New chat")
        .css_classes(["new-chat"])
        .build();
    header.append(&title);
    header.append(&chat_status);
    header.append(&new_chat);

    let message = gtk::Label::builder()
        .label(format!(
            "Voice shortcut is registering through the XDG portal. Preferred shortcut: {VOICE_TOGGLE_LABEL}."
        ))
        .wrap(true)
        .xalign(0.0)
        .yalign(0.0)
        .css_classes(["voice-message"])
        .build();
    let open_settings = gtk::Button::builder()
        .label("Open Settings")
        .halign(gtk::Align::Start)
        .visible(false)
        .build();
    let output_for_settings = output.clone();
    open_settings.connect_clicked(move |_| {
        super::write_message(
            &output_for_settings,
            &IpcEvent {
                protocol_version: PROTOCOL_VERSION,
                event: "openSettings",
                payload: serde_json::json!({ "section": "localVoice" }),
            },
        );
    });

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
        .label("Start a conversation with ChatHead.")
        .wrap(true)
        .xalign(0.5)
        .vexpand(true)
        .valign(gtk::Align::Center)
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

    let link_confirmation = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .visible(false)
        .css_classes(["link-confirmation"])
        .build();
    let link_destination = gtk::Label::builder()
        .wrap(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .max_width_chars(48)
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .css_classes(["link-destination"])
        .build();
    let link_cancel = gtk::Button::builder()
        .label("Cancel")
        .focusable(true)
        .build();
    let link_open = gtk::Button::builder()
        .label("Open")
        .focusable(true)
        .css_classes(["suggested-action"])
        .build();
    link_confirmation.append(&link_destination);
    link_confirmation.append(&link_cancel);
    link_confirmation.append(&link_open);

    let composer_row = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["composer-bar"])
        .build();
    let composer = gtk::TextView::builder()
        .wrap_mode(gtk::WrapMode::WordChar)
        .accepts_tab(false)
        .hexpand(true)
        .height_request(42)
        .css_classes(["prompt-input"])
        .build();
    let composer_scroll = gtk::ScrolledWindow::builder()
        .hexpand(true)
        .height_request(46)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .css_classes(["prompt-frame"])
        .child(&composer)
        .build();
    let composer_placeholder = gtk::Label::builder()
        .label("Message ChatHead…")
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .margin_start(8)
        .margin_top(12)
        .css_classes(["prompt-placeholder"])
        .build();
    composer_placeholder.set_can_target(false);
    let composer_overlay = gtk::Overlay::new();
    composer_overlay.set_hexpand(true);
    composer_overlay.set_child(Some(&composer_scroll));
    composer_overlay.add_overlay(&composer_placeholder);
    let send = gtk::Button::builder()
        .label("↑")
        .tooltip_text("Send message")
        .valign(gtk::Align::Center)
        .sensitive(false)
        .css_classes(["send-button"])
        .build();
    composer_row.append(&composer_overlay);
    composer_row.append(&send);

    panel.append(&header);
    panel.append(&message);
    panel.append(&open_settings);
    panel.append(&info);
    panel.append(&transcript_scroll);
    panel.append(&failure_row);
    panel.append(&link_confirmation);
    panel.append(&composer_row);
    new_chat.connect_clicked(|_| {
        PANEL_RUNTIME.with(|stored| {
            if let Some(runtime) = stored.borrow().as_ref() {
                runtime.conversation.borrow_mut().new_chat();
                runtime.failure.replace(None);
                runtime.pending_link.replace(None);
                runtime.widgets.link_confirmation.set_visible(false);
                let _ = runtime.codex.send(CodexCommand::NewChat);
                render_chat(runtime);
            }
        })
    });
    PanelWidgets {
        container: panel,
        header,
        message,
        chat_status,
        transcript,
        transcript_scroll,
        failure_row,
        link_confirmation,
        link_destination,
        link_cancel,
        link_open,
        composer_row,
        composer_scroll,
        composer,
        composer_placeholder,
        send,
        retry,
        failure,
        info,
        open_settings,
    }
}

#[derive(Clone, Copy)]
enum TranscriptAnchor {
    Bottom,
    Normalized(f64),
}

impl TranscriptAnchor {
    fn capture(scroll: &gtk::ScrolledWindow) -> Self {
        let adjustment = scroll.vadjustment();
        let range = (adjustment.upper() - adjustment.page_size()).max(0.0);
        if range <= 0.0 || adjustment.value() >= range - 28.0 {
            Self::Bottom
        } else {
            Self::Normalized((adjustment.value() / range).clamp(0.0, 1.0))
        }
    }

    fn restore(self, adjustment: &gtk::Adjustment) {
        let range = (adjustment.upper() - adjustment.page_size()).max(0.0);
        adjustment.set_value(match self {
            Self::Bottom => range,
            Self::Normalized(position) => range * position,
        });
    }
}

fn apply_panel_metrics(runtime: &PanelRuntime, zoom: PanelZoom) {
    let anchor = TranscriptAnchor::capture(&runtime.widgets.transcript_scroll);
    let metrics = PanelMetrics::for_zoom(zoom);
    runtime.widgets.container.set_spacing(metrics.panel_spacing);
    runtime.widgets.header.set_spacing(metrics.row_spacing);
    runtime.widgets.failure_row.set_spacing(metrics.row_spacing);
    runtime
        .widgets
        .composer_row
        .set_spacing(metrics.row_spacing);
    runtime
        .widgets
        .transcript
        .set_spacing(metrics.transcript_spacing);
    runtime
        .widgets
        .composer_scroll
        .set_height_request(metrics.composer_height);
    runtime
        .widgets
        .composer
        .set_height_request(metrics.composer_view_height);
    runtime.widgets.container.set_size_request(
        i32::from(current_panel_size().width()),
        i32::from(current_panel_size().height()),
    );

    let max_width_chars = panel_bubble_width_chars(current_panel_size(), zoom);
    for rendered in runtime.rendered_messages.borrow().iter() {
        if let Some(label) = &rendered.label {
            label.set_max_width_chars(max_width_chars);
        }
    }

    runtime.widgets.container.queue_resize();
    let adjustment = runtime.widgets.transcript_scroll.vadjustment();
    glib::idle_add_local_once(move || anchor.restore(&adjustment));
}

fn apply_panel_dimensions(runtime: &PanelRuntime, size: PanelSize) {
    runtime
        .widgets
        .container
        .set_size_request(i32::from(size.width()), i32::from(size.height()));
    for class in ["panel-compact", "panel-standard", "panel-expanded"] {
        runtime.widgets.container.remove_css_class(class);
    }
    let compact = size.width() < 560;
    runtime.widgets.container.add_css_class(if compact {
        "panel-compact"
    } else if size.width() < 720 {
        "panel-standard"
    } else {
        "panel-expanded"
    });
    runtime.widgets.chat_status.set_ellipsize(if compact {
        gtk::pango::EllipsizeMode::End
    } else {
        gtk::pango::EllipsizeMode::None
    });
    runtime
        .widgets
        .chat_status
        .set_max_width_chars(if compact { 18 } else { 30 });
    let max_width_chars = panel_bubble_width_chars(size, current_panel_zoom());
    for rendered in runtime.rendered_messages.borrow().iter() {
        if let Some(label) = &rendered.label {
            label.set_max_width_chars(max_width_chars);
        }
    }
    runtime.widgets.container.queue_resize();
}

fn panel_bubble_width_chars(size: PanelSize, zoom: PanelZoom) -> i32 {
    let base = if size.width() < 560 {
        32_u16
    } else if size.width() < 720 {
        44
    } else {
        72
    };
    i32::from(base.saturating_mul(100) / zoom.value()).max(22)
}

fn change_panel_zoom(runtime: &PanelRuntime, zoom: PanelZoom) {
    if zoom == current_panel_zoom() {
        return;
    }
    set_native_panel_zoom(zoom);
    super::write_message(
        &runtime.output,
        &IpcEvent {
            protocol_version: PROTOCOL_VERSION,
            event: "panelZoomChanged",
            payload: zoom,
        },
    );
}

fn attach_panel_zoom_controllers(runtime: &PanelRuntime) {
    let scroll = gtk::EventControllerScroll::new(
        gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE,
    );
    scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
    let runtime_for_scroll = runtime.clone();
    scroll.connect_scroll(move |controller, _, dy| {
        if !controller
            .current_event_state()
            .contains(gdk::ModifierType::CONTROL_MASK)
        {
            return glib::Propagation::Proceed;
        }
        let current = current_panel_zoom();
        if dy < 0.0 {
            change_panel_zoom(&runtime_for_scroll, current.next());
        } else if dy > 0.0 {
            change_panel_zoom(&runtime_for_scroll, current.previous());
        }
        glib::Propagation::Stop
    });
    runtime.widgets.container.add_controller(scroll);

    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let runtime_for_key = runtime.clone();
    key.connect_key_pressed(move |_, key, _, modifiers| {
        if !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
            return glib::Propagation::Proceed;
        }
        let current = current_panel_zoom();
        let next = if matches!(key, gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add) {
            Some(current.next())
        } else if matches!(key, gdk::Key::minus | gdk::Key::KP_Subtract) {
            Some(current.previous())
        } else if matches!(key, gdk::Key::_0 | gdk::Key::KP_0) {
            Some(PanelZoom::reset())
        } else {
            None
        };
        if let Some(next) = next {
            change_panel_zoom(&runtime_for_key, next);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    runtime.widgets.container.add_controller(key);
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

    let runtime_for_link_cancel = runtime.clone();
    runtime.widgets.link_cancel.connect_clicked(move |_| {
        runtime_for_link_cancel.pending_link.replace(None);
        runtime_for_link_cancel
            .widgets
            .link_confirmation
            .set_visible(false);
        runtime_for_link_cancel.widgets.composer.grab_focus();
    });

    let runtime_for_link_open = runtime.clone();
    runtime.widgets.link_open.connect_clicked(move |_| {
        let destination = runtime_for_link_open.pending_link.borrow().clone();
        let Some(destination) = destination else {
            return;
        };
        match open_confirmed_uri(&destination) {
            Ok(()) => {
                runtime_for_link_open.pending_link.replace(None);
                runtime_for_link_open
                    .widgets
                    .link_confirmation
                    .set_visible(false);
            }
            Err(error) => {
                runtime_for_link_open
                    .widgets
                    .link_destination
                    .set_label(&format!("Could not open {destination}: {error}"));
            }
        }
    });

    let key = gtk::EventControllerKey::new();
    let runtime_for_key = runtime.clone();
    key.connect_key_pressed(move |_, key, _, modifiers| {
        if key == gdk::Key::Return && runtime_for_key.conversation.borrow().is_busy() {
            return glib::Propagation::Stop;
        }
        if key == gdk::Key::Return && !modifiers.contains(gdk::ModifierType::SHIFT_MASK) {
            submit_composer(&runtime_for_key);
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    runtime.widgets.composer.add_controller(key);

    let placeholder = runtime.widgets.composer_placeholder.clone();
    let send = runtime.widgets.send.clone();
    let chat_state = runtime.chat_state.clone();
    let conversation = runtime.conversation.clone();
    runtime
        .widgets
        .composer
        .buffer()
        .connect_changed(move |buffer| {
            let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
            placeholder.set_visible(text.is_empty());
            let ready = chat_state.get() == ExperimentalChatState::Ready;
            send.set_sensitive(ready && (conversation.borrow().is_busy() || !text.is_empty()));
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
                        "Connect a ChatGPT subscription in Settings to use ChatHead.".to_owned(),
                    ));
                }
            }
            CodexEvent::Failure { message, .. } => {
                runtime.failure.replace(Some(message.clone()));
                runtime.conversation.borrow_mut().apply(event);
            }
            _ => runtime.conversation.borrow_mut().apply(event),
        }
        if matches!(event, CodexEvent::AssistantTextDelta { .. }) {
            schedule_stream_render(runtime);
        } else {
            cancel_stream_render(runtime);
            render_chat(runtime);
        }
    });
}

fn schedule_stream_render(runtime: &PanelRuntime) {
    if runtime.stream_render_source.borrow().is_some() {
        return;
    }

    let runtime_for_render = runtime.clone();
    let source = glib::timeout_add_local_once(
        Duration::from_millis(STREAM_RENDER_INTERVAL_MS),
        move || {
            runtime_for_render.stream_render_source.replace(None);
            render_chat(&runtime_for_render);
        },
    );
    runtime.stream_render_source.replace(Some(source));
}

fn cancel_stream_render(runtime: &PanelRuntime) {
    if let Some(source) = runtime.stream_render_source.take() {
        source.remove();
    }
}

fn start_highlight_result_pump() {
    glib::timeout_add_local(Duration::from_millis(ACTION_POLL_MS), move || {
        let mut keep_running = false;
        PANEL_RUNTIME.with(|stored| {
            let runtime_ref = stored.borrow();
            let Some(runtime) = runtime_ref.as_ref() else {
                return;
            };
            keep_running = true;
            let results = runtime.highlight_worker.drain();
            if results.is_empty() {
                return;
            }
            let anchor = TranscriptAnchor::capture(&runtime.widgets.transcript_scroll);
            let rendered = runtime.rendered_messages.borrow();
            let mut applied = false;
            for result in &results {
                if let Some(message) = rendered
                    .iter()
                    .find(|message| message.id == result.message_id)
                    && let Some(document) = &message.document
                {
                    applied |= document.apply_highlight(result);
                }
            }
            if applied {
                runtime.widgets.transcript.queue_resize();
                let adjustment = runtime.widgets.transcript_scroll.vadjustment();
                glib::idle_add_local_once(move || anchor.restore(&adjustment));
            }
        });
        if keep_running {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });
}

fn render_chat(runtime: &PanelRuntime) {
    let adjustment = runtime.widgets.transcript_scroll.vadjustment();
    let near_bottom = adjustment.value() + adjustment.page_size() >= adjustment.upper() - 28.0;

    let conversation = runtime.conversation.borrow();
    sync_transcript(runtime, conversation.messages());

    let busy = conversation.is_busy();
    let ready = runtime.chat_state.get() == ExperimentalChatState::Ready;
    runtime.widgets.chat_status.set_label(if busy {
        "● Thinking · ChatGPT"
    } else if ready {
        "● Ready · ChatGPT"
    } else {
        "● Unavailable · ChatGPT"
    });
    runtime.widgets.chat_status.set_css_classes(if busy {
        &["provider-status", "provider-status-busy"]
    } else if ready {
        &["provider-status"]
    } else {
        &["provider-status", "provider-status-error"]
    });
    runtime.widgets.send.set_label(if busy { "■" } else { "↑" });
    runtime.widgets.send.set_tooltip_text(Some(if busy {
        "Stop response"
    } else {
        "Send message"
    }));
    runtime.widgets.composer.set_sensitive(ready);
    runtime.widgets.composer.set_editable(ready && !busy);
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
    runtime
        .widgets
        .send
        .set_sensitive(ready && (busy || !composer_text.is_empty()));

    let info = runtime
        .chat_message
        .borrow()
        .clone()
        .unwrap_or_else(|| "Start a conversation with ChatHead.".to_owned());
    runtime.widgets.info.set_label(&info);
    runtime
        .widgets
        .info
        .set_visible(conversation.messages().is_empty() || !ready);
    runtime
        .widgets
        .transcript_scroll
        .set_visible(!conversation.messages().is_empty());

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

fn sync_transcript(runtime: &PanelRuntime, messages: &[ChatMessage]) {
    let mut rendered = runtime.rendered_messages.borrow_mut();
    let needs_reset = rendered.len() > messages.len()
        || rendered
            .iter()
            .zip(messages)
            .any(|(widget, message)| widget.id != message.id);

    if needs_reset {
        while let Some(child) = runtime.widgets.transcript.first_child() {
            runtime.widgets.transcript.remove(&child);
        }
        rendered.clear();
    }

    for message in messages.iter().skip(rendered.len()) {
        let widget = message_widget(message, &runtime.highlight_worker);
        runtime.widgets.transcript.append(&widget.row);
        rendered.push(widget);
    }

    for (widget, message) in rendered.iter_mut().zip(messages) {
        if widget.rendered_text == message.text && widget.rendered_state == message.state {
            continue;
        }

        match message.role {
            MessageRole::User => {
                if let Some(label) = &widget.label {
                    label.set_label(&message.text);
                }
            }
            MessageRole::Assistant if message.text.is_empty() => {}
            MessageRole::Assistant => {
                if let Some(document) = &widget.document {
                    document.update(&message.text, message.state, OrbTheme::current().into());
                } else {
                    while let Some(child) = widget.row.first_child() {
                        widget.row.remove(&child);
                    }
                    let document = assistant_document(message, &runtime.highlight_worker);
                    widget.row.append(&document.widget());
                    widget.document = Some(document);
                }
            }
        }
        widget.rendered_text.clone_from(&message.text);
        widget.rendered_state = message.state;
        widget.revision = widget.revision.saturating_add(1);
    }
}

fn message_widget(message: &ChatMessage, worker: &HighlightWorker) -> RenderedMessage {
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.set_hexpand(message.role == MessageRole::Assistant);
    row.set_halign(match message.role {
        MessageRole::User => gtk::Align::End,
        MessageRole::Assistant => gtk::Align::Fill,
    });

    if message.text.is_empty() && message.state == MessageState::Streaming {
        row.append(&thinking_bubble());
        return RenderedMessage {
            id: message.id.clone(),
            rendered_text: String::new(),
            rendered_state: message.state,
            revision: 0,
            row,
            label: None,
            document: None,
        };
    }

    match message.role {
        MessageRole::User => {
            let label = user_message_label(message);
            row.append(&user_message_widget(message, &label));
            RenderedMessage {
                id: message.id.clone(),
                rendered_text: message.text.clone(),
                rendered_state: message.state,
                revision: 1,
                row,
                label: Some(label),
                document: None,
            }
        }
        MessageRole::Assistant => {
            let document = assistant_document(message, worker);
            row.append(&document.widget());
            RenderedMessage {
                id: message.id.clone(),
                rendered_text: message.text.clone(),
                rendered_state: message.state,
                revision: 1,
                row,
                label: None,
                document: Some(document),
            }
        }
    }
}

fn user_message_label(message: &ChatMessage) -> gtk::Label {
    let max_width_chars = panel_bubble_width_chars(current_panel_size(), current_panel_zoom());
    gtk::Label::builder()
        .label(&message.text)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .max_width_chars(max_width_chars)
        .css_classes(["chat-bubble", "user-bubble"])
        .build()
}

fn user_message_widget(message: &ChatMessage, label: &gtk::Label) -> gtk::Box {
    let container = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .halign(gtk::Align::End)
        .css_classes(["user-message"])
        .build();
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .halign(gtk::Align::End)
        .css_classes(["user-message-actions"])
        .build();
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy prompt")
        .focusable(true)
        .css_classes(["response-action", "user-copy-action"])
        .build();
    let prompt = message.text.clone();
    copy.connect_clicked(move |button| copy_text_to_clipboard(button, &prompt));
    actions.append(&copy);
    container.append(label);
    container.append(&actions);
    container
}

fn assistant_document(message: &ChatMessage, worker: &HighlightWorker) -> AssistantDocument {
    let link_handler: LinkHandler = Rc::new(request_link_confirmation);
    let assistant_message_id = message.id.clone();
    let retry_handler: RetryHandler = Rc::new(move || {
        retry_assistant_response(&assistant_message_id);
    });
    AssistantDocument::new(
        &message.id,
        &message.text,
        message.state,
        OrbTheme::current().into(),
        worker,
        link_handler,
        retry_handler,
    )
}

fn retry_assistant_response(assistant_message_id: &str) {
    PANEL_RUNTIME.with(|stored| {
        let runtime_ref = stored.borrow();
        let Some(runtime) = runtime_ref.as_ref() else {
            return;
        };
        let prompt = runtime
            .conversation
            .borrow()
            .prompt_for_assistant(assistant_message_id)
            .map(str::to_owned);
        if let Some(prompt) = prompt {
            submit_text(runtime, &prompt);
        }
    });
}

fn request_link_confirmation(destination: String) {
    PANEL_RUNTIME.with(|stored| {
        let runtime_ref = stored.borrow();
        let Some(runtime) = runtime_ref.as_ref() else {
            return;
        };
        runtime.pending_link.replace(Some(destination.clone()));
        runtime
            .widgets
            .link_destination
            .set_label(&format!("Open this link? {destination}"));
        runtime.widgets.link_confirmation.set_visible(true);
        runtime.widgets.link_cancel.grab_focus();
    });
}

fn thinking_bubble() -> gtk::Box {
    let bubble = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .css_classes(["chat-bubble", "assistant-bubble", "thinking-bubble"])
        .build();
    let dots = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(3)
        .build();
    let dot_labels = (0..3)
        .map(|index| {
            let dot = gtk::Label::builder()
                .label("●")
                .css_classes(["thinking-dot"])
                .build();
            dot.set_opacity(if index == 0 { 1.0 } else { 0.32 });
            dots.append(&dot);
            dot
        })
        .collect::<Vec<_>>();
    let label = gtk::Label::builder()
        .label("Thinking")
        .css_classes(["thinking-label"])
        .build();
    bubble.append(&dots);
    bubble.append(&label);

    let weak_dots = dot_labels
        .iter()
        .map(glib::object::ObjectExt::downgrade)
        .collect::<Vec<_>>();
    let phase = Rc::new(Cell::new(0_usize));
    glib::timeout_add_local(Duration::from_millis(240), move || {
        let next_phase = (phase.get() + 1) % weak_dots.len();
        phase.set(next_phase);
        let mut is_visible = false;
        for (index, weak_dot) in weak_dots.iter().enumerate() {
            if let Some(dot) = weak_dot.upgrade() {
                dot.set_opacity(if index == next_phase { 1.0 } else { 0.32 });
                is_visible = true;
            }
        }
        if is_visible {
            glib::ControlFlow::Continue
        } else {
            glib::ControlFlow::Break
        }
    });

    bubble
}

fn attach_app_event_pump(
    receiver: mpsc::Receiver<AppEvent>,
    message: &gtk::Label,
    state: &OverlayState,
    shortcut_status_sender: mpsc::Sender<ShortcutStatusUpdate>,
) {
    let message = message.clone();
    let state = state.clone();

    glib::timeout_add_local(Duration::from_millis(ACTION_POLL_MS), move || {
        while let Ok(event) = receiver.try_recv() {
            handle_app_event(event, &message, &state, &shortcut_status_sender);
        }

        glib::ControlFlow::Continue
    });
}

fn handle_app_event(
    event: AppEvent,
    message: &gtk::Label,
    state: &OverlayState,
    shortcut_status_sender: &mpsc::Sender<ShortcutStatusUpdate>,
) {
    match event {
        AppEvent::CancelVoice => cancel_voice(),
        AppEvent::VoiceShortcutActivated => {
            reveal_panel();
            PANEL_RUNTIME.with(|stored| {
                if let Some(runtime) = stored.borrow().as_ref() {
                    let busy = runtime.conversation.borrow().is_busy();
                    let ready = runtime.chat_state.get() == ExperimentalChatState::Ready;
                    let _ = runtime.voice.shortcut_activated(busy, ready);
                }
            });
        }
        AppEvent::VoiceShortcutDeactivated => {
            PANEL_RUNTIME.with(|stored| {
                if let Some(runtime) = stored.borrow().as_ref() {
                    let _ = runtime.voice.shortcut_deactivated();
                }
            });
        }
        AppEvent::TogglePanel => toggle_panel(),
        AppEvent::ShortcutStatus(action, shortcut_status) => {
            let protocol_status = match &shortcut_status {
                ShortcutStatus::Registering => ProtocolShortcutStatus::Registering,
                ShortcutStatus::Ready(trigger) => ProtocolShortcutStatus::Ready {
                    trigger: trigger.clone(),
                },
                ShortcutStatus::ConflictPossible(details) => {
                    ProtocolShortcutStatus::ConflictPossible {
                        details: details.clone(),
                    }
                }
                ShortcutStatus::Unavailable(details) => ProtocolShortcutStatus::Unavailable {
                    details: details.clone(),
                },
            };
            let _ = shortcut_status_sender.send(ShortcutStatusUpdate {
                action,
                status: protocol_status,
            });
            if action == ShortcutAction::Panel {
                return;
            }
            state.shortcut_status.replace(shortcut_status);
            let voice_snapshot = PANEL_RUNTIME.with(|stored| {
                stored
                    .borrow()
                    .as_ref()
                    .map(|runtime| runtime.voice.snapshot())
            });
            if let Some(snapshot) =
                voice_snapshot.filter(|snapshot| snapshot.phase != VoicePhase::Ready)
            {
                render_voice_snapshot(&snapshot);
            } else {
                update_shortcut_message(message, state);
            }
        }
    }
}

fn cancel_voice() {
    PANEL_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            if runtime.pending_voice.borrow_mut().cancel() {
                runtime.widgets.composer.buffer().set_text("");
            }
            let _ = runtime.voice.cancel();
            runtime.widgets.composer.grab_focus();
        }
    });
}

fn reveal_panel() {
    set_panel_open(true);
}

fn toggle_panel() {
    let open = OVERLAY_RUNTIME.with(|stored| {
        let runtime_ref = stored.borrow();
        let Some(runtime) = runtime_ref.as_ref() else {
            return false;
        };
        !runtime.state.panel_open.get()
    });
    set_panel_open(open);
}

fn set_panel_open(open: bool) {
    let changed = OVERLAY_RUNTIME.with(|stored| {
        let runtime_ref = stored.borrow();
        let Some(runtime) = runtime_ref.as_ref() else {
            return false;
        };
        runtime.state.panel_open.set(open);
        runtime.state.panel_keyboard_captured.set(open);
        runtime.state.panel_rect_override.set(None);
        runtime.panel.set_visible(open);
        runtime.window.set_keyboard_mode(if open {
            KeyboardMode::Exclusive
        } else {
            KeyboardMode::None
        });
        position_widgets(
            &runtime.canvas,
            &runtime.chathead,
            &runtime.panel,
            &runtime.state,
        );
        apply_idle_input_region(&runtime.window, &runtime.state);
        if open {
            runtime.window.present();
        }
        true
    });
    if changed && open {
        PANEL_RUNTIME.with(|stored| {
            if let Some(runtime) = stored.borrow().as_ref() {
                runtime.widgets.composer.grab_focus();
                focus_composer_when_mapped(&runtime.widgets.composer);
            }
        });
    }
}

pub(crate) fn handle_voice_event(_app: &gtk::Application, event: &VoiceEvent) {
    match event {
        VoiceEvent::Snapshot(snapshot) => render_voice_snapshot(snapshot),
        VoiceEvent::Transcript { utterance_id, text } => {
            let utterance_id = *utterance_id;
            PANEL_RUNTIME.with(|stored| {
                let runtime_ref = stored.borrow();
                let Some(runtime) = runtime_ref.as_ref() else {
                    return;
                };
                runtime.widgets.composer.buffer().set_text(text);
                runtime.widgets.composer.grab_focus();
                if !should_auto_send_voice(runtime.voice.snapshot().submission_mode) {
                    let _ = runtime.voice.complete_utterance(utterance_id);
                    return;
                }
                runtime.pending_voice.borrow_mut().arm(utterance_id);
                let runtime_for_send = runtime.clone();
                glib::timeout_add_local_once(VOICE_SEND_DELAY, move || {
                    if runtime_for_send.voice.snapshot().phase != VoicePhase::PendingSend
                        || !runtime_for_send
                            .pending_voice
                            .borrow_mut()
                            .consume(utterance_id)
                    {
                        return;
                    }
                    if runtime_for_send.conversation.borrow().is_busy() {
                        runtime_for_send.failure.replace(Some(
                            "Voice message was not sent because ChatHead is already responding."
                                .to_owned(),
                        ));
                        let _ = runtime_for_send.voice.cancel();
                        render_chat(&runtime_for_send);
                        return;
                    }
                    let buffer = runtime_for_send.widgets.composer.buffer();
                    let text = buffer.text(&buffer.start_iter(), &buffer.end_iter(), true);
                    submit_text(&runtime_for_send, &text);
                    if runtime_for_send.conversation.borrow().is_busy() {
                        buffer.set_text("");
                    }
                    let _ = runtime_for_send.voice.complete_utterance(utterance_id);
                    runtime_for_send.widgets.composer.grab_focus();
                });
            });
        }
        VoiceEvent::AutoFinalized => PANEL_RUNTIME.with(|stored| {
            if let Some(runtime) = stored.borrow().as_ref() {
                runtime
                    .widgets
                    .message
                    .set_label("30-second limit reached. Transcribing locally…");
            }
        }),
        VoiceEvent::LevelChanged { .. } => {}
    }
}

fn should_auto_send_voice(mode: VoiceSubmissionMode) -> bool {
    mode == VoiceSubmissionMode::InsertAndSend
}

fn render_voice_snapshot(snapshot: &VoiceSnapshot) {
    OVERLAY_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            let next = if snapshot.phase == VoicePhase::Listening {
                VoiceState::Listening
            } else {
                VoiceState::Idle
            };
            if runtime.state.voice_state.replace(next) != next {
                runtime.state.voice_changed_at.set(Instant::now());
                start_or_continue_animation(&runtime.chathead, &runtime.state);
            }
        }
    });
    PANEL_RUNTIME.with(|stored| {
        let runtime_ref = stored.borrow();
        let Some(runtime) = runtime_ref.as_ref() else {
            return;
        };
        let show_message = matches!(
            snapshot.phase,
            VoicePhase::Disabled
                | VoicePhase::SetupRequired
                | VoicePhase::Downloading
                | VoicePhase::Loading
                | VoicePhase::Error
        );
        runtime
            .widgets
            .message
            .set_label(snapshot.message.as_deref().unwrap_or(""));
        runtime.widgets.message.set_visible(show_message);
        runtime.widgets.open_settings.set_visible(matches!(
            snapshot.phase,
            VoicePhase::Disabled | VoicePhase::SetupRequired | VoicePhase::Error
        ));
    });
}

fn update_shortcut_message(message: &gtk::Label, state: &OverlayState) {
    match state.voice_state.get() {
        VoiceState::Listening => {
            message.set_label("");
            message.set_visible(false);
        }
        VoiceState::Idle => match &*state.shortcut_status.borrow() {
            ShortcutStatus::Registering => {
                message.set_label("");
                message.set_visible(false);
            }
            ShortcutStatus::Ready(_) => {
                message.set_label("");
                message.set_visible(false);
            }
            ShortcutStatus::ConflictPossible(details) => {
                message.set_label(details);
                message.set_visible(true);
            }
            ShortcutStatus::Unavailable(details) => {
                message.set_label(details);
                message.set_visible(true);
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
            let _ = sender.send(AppEvent::CancelVoice);
            return glib::Propagation::Stop;
        }

        glib::Propagation::Proceed
    });
    panel.add_controller(key);
}

fn attach_focus_dismiss_controller(
    window: &gtk::ApplicationWindow,
    canvas: &gtk::Fixed,
    state: &OverlayState,
) {
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);

    let window_for_press = window.clone();
    let state_for_press = state.clone();
    click.connect_pressed(move |_, _, x, y| {
        let panel_hit = window_for_press.surface().is_some_and(|surface| {
            point_is_in_panel(x, y, surface.width(), surface.height(), &state_for_press)
        });
        if !state_for_press.panel_open.get()
            || !state_for_press.panel_keyboard_captured.get()
            || point_is_in_chathead(x, y, &state_for_press)
            || panel_hit
        {
            return;
        }

        state_for_press.panel_keyboard_captured.set(false);
        window_for_press.set_keyboard_mode(KeyboardMode::OnDemand);
        apply_idle_input_region(&window_for_press, &state_for_press);
    });

    canvas.add_controller(click);
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
    let canvas_for_begin = canvas.clone();
    let chathead_for_begin = chathead.clone();
    let state_for_begin = state.clone();
    drag.connect_drag_begin(move |_, start_x, start_y| {
        if !point_is_in_chathead(start_x, start_y, &state_for_begin) {
            state_for_begin.dragging_chathead.set(false);
            return;
        }

        state_for_begin.dragging_chathead.set(true);
        state_for_begin.panel_rect_override.set(None);
        state_for_begin.drag_start_x.set(state_for_begin.x.get());
        state_for_begin.drag_start_y.set(state_for_begin.y.get());
        canvas_for_begin.set_cursor_from_name(Some("grabbing"));
        chathead_for_begin.set_cursor_from_name(Some("grabbing"));
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
            toggle_panel();
        }

        clamp_position(&canvas_for_end, &state_for_end);
        position_widgets(
            &canvas_for_end,
            &chathead_for_end,
            &panel_for_end,
            &state_for_end,
        );
        canvas_for_end.set_cursor(None);
        chathead_for_end.set_cursor_from_name(Some("pointer"));
        apply_idle_input_region(&window_for_end, &state_for_end);
    });

    let window_for_cancel = window.clone();
    let canvas_for_cancel = canvas.clone();
    let chathead_for_cancel = chathead.clone();
    let state_for_cancel = state.clone();
    drag.connect_cancel(move |_, _| {
        if state_for_cancel.dragging_chathead.replace(false) {
            canvas_for_cancel.set_cursor(None);
            chathead_for_cancel.set_cursor_from_name(Some("pointer"));
            apply_idle_input_region(&window_for_cancel, &state_for_cancel);
        }
    });

    canvas.add_controller(drag);
    chathead.set_can_focus(false);
}

#[derive(Clone, Copy)]
struct ResizeSession {
    edge: ResizeEdge,
    start: PanelRect,
    anchor: TranscriptAnchor,
}

fn attach_panel_resize_controller(
    window: &gtk::ApplicationWindow,
    canvas: &gtk::Fixed,
    runtime: &PanelRuntime,
    state: &OverlayState,
) {
    let session = Rc::new(Cell::new(None::<ResizeSession>));
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    let panel_for_motion = runtime.widgets.container.clone();
    let session_for_motion = session.clone();
    motion.connect_motion(move |_, x, y| {
        if session_for_motion.get().is_some() {
            return;
        }
        let edge = resize_edge_at(
            x,
            y,
            panel_for_motion.allocated_width(),
            panel_for_motion.allocated_height(),
        );
        panel_for_motion.set_cursor_from_name(edge.map(ResizeEdge::cursor_name));
        if edge.is_some() {
            panel_for_motion.add_css_class("resize-ready");
        } else {
            panel_for_motion.remove_css_class("resize-ready");
        }
    });
    let panel_for_leave = runtime.widgets.container.clone();
    let session_for_leave = session.clone();
    motion.connect_leave(move |_| {
        if session_for_leave.get().is_none() {
            panel_for_leave.set_cursor(None);
            panel_for_leave.remove_css_class("resize-ready");
        }
    });
    runtime.widgets.container.add_controller(motion);

    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    drag.set_propagation_phase(gtk::PropagationPhase::Capture);

    let window_for_begin = window.clone();
    let canvas_for_begin = canvas.clone();
    let runtime_for_begin = runtime.clone();
    let state_for_begin = state.clone();
    let session_for_begin = session.clone();
    drag.connect_drag_begin(move |gesture, start_x, start_y| {
        let Some(edge) = resize_edge_at(
            start_x,
            start_y,
            runtime_for_begin.widgets.container.allocated_width(),
            runtime_for_begin.widgets.container.allocated_height(),
        ) else {
            return;
        };
        let _ = gesture.set_state(gtk::EventSequenceState::Claimed);
        let start = visible_panel_rect(
            canvas_for_begin.allocated_width(),
            canvas_for_begin.allocated_height(),
            &state_for_begin,
        );
        state_for_begin.panel_rect_override.set(Some(start));
        session_for_begin.set(Some(ResizeSession {
            edge,
            start,
            anchor: TranscriptAnchor::capture(&runtime_for_begin.widgets.transcript_scroll),
        }));
        runtime_for_begin
            .widgets
            .container
            .add_css_class("resizing");
        runtime_for_begin
            .widgets
            .container
            .set_cursor_from_name(Some(edge.cursor_name()));
        set_full_input_region(&window_for_begin);
    });

    let canvas_for_update = canvas.clone();
    let runtime_for_update = runtime.clone();
    let state_for_update = state.clone();
    let session_for_update = session.clone();
    drag.connect_drag_update(move |_, offset_x, offset_y| {
        let Some(active) = session_for_update.get() else {
            return;
        };
        let rect = resized_panel_rect(
            active.start,
            active.edge,
            offset_x,
            offset_y,
            canvas_for_update.allocated_width(),
            canvas_for_update.allocated_height(),
        );
        apply_resize_rect(
            &canvas_for_update,
            &runtime_for_update,
            &state_for_update,
            rect,
        );
    });

    let window_for_end = window.clone();
    let canvas_for_end = canvas.clone();
    let runtime_for_end = runtime.clone();
    let state_for_end = state.clone();
    let session_for_end = session.clone();
    drag.connect_drag_end(move |_, offset_x, offset_y| {
        let Some(active) = session_for_end.take() else {
            return;
        };
        let rect = resized_panel_rect(
            active.start,
            active.edge,
            offset_x,
            offset_y,
            canvas_for_end.allocated_width(),
            canvas_for_end.allocated_height(),
        );
        apply_resize_rect(&canvas_for_end, &runtime_for_end, &state_for_end, rect);
        runtime_for_end
            .widgets
            .container
            .remove_css_class("resizing");
        runtime_for_end
            .widgets
            .container
            .remove_css_class("resize-ready");
        runtime_for_end.widgets.container.set_cursor(None);
        restore_transcript_anchor(active.anchor, &runtime_for_end.widgets.transcript_scroll);
        apply_idle_input_region(&window_for_end, &state_for_end);
        emit_panel_size_changed(rect.size());
    });

    let window_for_cancel = window.clone();
    let canvas_for_cancel = canvas.clone();
    let runtime_for_cancel = runtime.clone();
    let state_for_cancel = state.clone();
    let session_for_cancel = session;
    drag.connect_cancel(move |_, _| {
        let Some(active) = session_for_cancel.take() else {
            return;
        };
        apply_resize_rect(
            &canvas_for_cancel,
            &runtime_for_cancel,
            &state_for_cancel,
            active.start,
        );
        runtime_for_cancel
            .widgets
            .container
            .remove_css_class("resizing");
        runtime_for_cancel
            .widgets
            .container
            .remove_css_class("resize-ready");
        runtime_for_cancel.widgets.container.set_cursor(None);
        restore_transcript_anchor(active.anchor, &runtime_for_cancel.widgets.transcript_scroll);
        apply_idle_input_region(&window_for_cancel, &state_for_cancel);
    });

    runtime.widgets.container.add_controller(drag);
}

fn apply_resize_rect(
    canvas: &gtk::Fixed,
    runtime: &PanelRuntime,
    state: &OverlayState,
    rect: PanelRect,
) {
    let size = rect.size();
    state.panel_size.set(size);
    state.panel_rect_override.set(Some(rect));
    store_panel_size(size);
    apply_panel_dimensions(runtime, size);
    canvas.move_(&runtime.widgets.container, rect.x, rect.y);
}

fn restore_transcript_anchor(anchor: TranscriptAnchor, scroll: &gtk::ScrolledWindow) {
    let adjustment = scroll.vadjustment();
    glib::idle_add_local_once(move || anchor.restore(&adjustment));
}

fn emit_panel_size_changed(size: PanelSize) {
    PANEL_RUNTIME.with(|stored| {
        if let Some(runtime) = stored.borrow().as_ref() {
            super::write_message(
                &runtime.output,
                &IpcEvent {
                    protocol_version: PROTOCOL_VERSION,
                    event: "panelSizeChanged",
                    payload: size,
                },
            );
        }
    });
}

fn resize_edge_at(x: f64, y: f64, width: i32, height: i32) -> Option<ResizeEdge> {
    let width = f64::from(width);
    let height = f64::from(height);
    let left_corner = x <= RESIZE_CORNER_HIT_ZONE;
    let right_corner = x >= width - RESIZE_CORNER_HIT_ZONE;
    let top_corner = y <= RESIZE_CORNER_HIT_ZONE;
    let bottom_corner = y >= height - RESIZE_CORNER_HIT_ZONE;
    if left_corner && top_corner {
        return Some(ResizeEdge::NorthWest);
    }
    if right_corner && top_corner {
        return Some(ResizeEdge::NorthEast);
    }
    if right_corner && bottom_corner {
        return Some(ResizeEdge::SouthEast);
    }
    if left_corner && bottom_corner {
        return Some(ResizeEdge::SouthWest);
    }
    if y <= RESIZE_EDGE_HIT_ZONE {
        Some(ResizeEdge::North)
    } else if x >= width - RESIZE_EDGE_HIT_ZONE {
        Some(ResizeEdge::East)
    } else if y >= height - RESIZE_EDGE_HIT_ZONE {
        Some(ResizeEdge::South)
    } else if x <= RESIZE_EDGE_HIT_ZONE {
        Some(ResizeEdge::West)
    } else {
        None
    }
}

fn effective_panel_size(requested: PanelSize, output_width: i32, output_height: i32) -> PanelSize {
    let width_cap = (output_width - EDGE_PADDING * 2)
        .clamp(i32::from(PANEL_WIDTH_MIN), i32::from(PANEL_WIDTH_MAX));
    let height_cap = (output_height - EDGE_PADDING * 2)
        .clamp(i32::from(PANEL_HEIGHT_MIN), i32::from(PANEL_HEIGHT_MAX));
    PanelSize::try_new(
        requested
            .width()
            .min(u16::try_from(width_cap).expect("positive width cap")),
        requested
            .height()
            .min(u16::try_from(height_cap).expect("positive height cap")),
    )
    .expect("effective panel size stays within validated bounds")
}

fn resized_panel_rect(
    start: PanelRect,
    edge: ResizeEdge,
    offset_x: f64,
    offset_y: f64,
    output_width: i32,
    output_height: i32,
) -> PanelRect {
    let maximum = effective_panel_size(
        PanelSize::try_new(PANEL_WIDTH_MAX, PANEL_HEIGHT_MAX).expect("maximum panel size"),
        output_width,
        output_height,
    );
    let min_width = f64::from(PANEL_WIDTH_MIN);
    let min_height = f64::from(PANEL_HEIGHT_MIN);
    let max_width = f64::from(maximum.width());
    let max_height = f64::from(maximum.height());
    let mut left = start.x;
    let mut right = start.x + f64::from(start.width);
    let mut top = start.y;
    let mut bottom = start.y + f64::from(start.height);

    if edge.moves_left() {
        let upper = right - min_width;
        let lower = f64::from(EDGE_PADDING).max(right - max_width).min(upper);
        left = (start.x + offset_x).clamp(lower, upper);
    } else if edge.moves_right() {
        let lower = left + min_width;
        let upper = f64::from(output_width - EDGE_PADDING)
            .min(left + max_width)
            .max(lower);
        right = (start.x + f64::from(start.width) + offset_x).clamp(lower, upper);
    }
    if edge.moves_top() {
        let upper = bottom - min_height;
        let lower = f64::from(EDGE_PADDING).max(bottom - max_height).min(upper);
        top = (start.y + offset_y).clamp(lower, upper);
    } else if edge.moves_bottom() {
        let lower = top + min_height;
        let upper = f64::from(output_height - EDGE_PADDING)
            .min(top + max_height)
            .max(lower);
        bottom = (start.y + f64::from(start.height) + offset_y).clamp(lower, upper);
    }

    PanelRect {
        x: left.round(),
        y: top.round(),
        width: (right - left).round() as i32,
        height: (bottom - top).round() as i32,
    }
}

fn attach_hover_cursor(canvas: &gtk::Fixed, state: &OverlayState) {
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);

    let canvas_for_motion = canvas.clone();
    let state_for_motion = state.clone();
    motion.connect_motion(move |_, x, y| {
        if state_for_motion.dragging_chathead.get() {
            canvas_for_motion.set_cursor_from_name(Some("grabbing"));
        } else if point_is_in_chathead(x, y, &state_for_motion) {
            canvas_for_motion.set_cursor_from_name(Some("pointer"));
        } else {
            canvas_for_motion.set_cursor(None);
        }
    });

    let canvas_for_leave = canvas.clone();
    let state_for_leave = state.clone();
    motion.connect_leave(move |_| {
        if !state_for_leave.dragging_chathead.get() {
            canvas_for_leave.set_cursor(None);
        }
    });

    canvas.add_controller(motion);
}

fn focus_composer_when_mapped(composer: &gtk::TextView) {
    let weak_composer = glib::object::ObjectExt::downgrade(composer);
    glib::idle_add_local_once(move || {
        if let Some(composer) = weak_composer.upgrade() {
            composer.grab_focus();
        }
    });
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
                    send_all_shortcut_statuses(
                        &sender,
                        ShortcutStatus::Unavailable(format!(
                            "Global shortcut runtime failed: {error}."
                        )),
                    );
                }
            }
        })
    {
        eprintln!("failed to start shortcut service: {error}");
    }
}

async fn run_shortcut_service(sender: mpsc::Sender<AppEvent>) {
    if !send_all_shortcut_statuses(&sender, ShortcutStatus::Registering) {
        return;
    }

    let result = register_and_listen_for_shortcuts(sender.clone()).await;
    if let Err(error) = result {
        send_all_shortcut_statuses(
            &sender,
            ShortcutStatus::Unavailable(format!("Global shortcut unavailable: {error}.")),
        );
    }
}

fn send_all_shortcut_statuses(sender: &mpsc::Sender<AppEvent>, status: ShortcutStatus) -> bool {
    [ShortcutAction::Voice, ShortcutAction::Panel]
        .into_iter()
        .all(|action| {
            sender
                .send(AppEvent::ShortcutStatus(action, status.clone()))
                .is_ok()
        })
}

async fn register_and_listen_for_shortcuts(
    sender: mpsc::Sender<AppEvent>,
) -> Result<(), ashpd::Error> {
    let portal = GlobalShortcuts::new().await?;
    let session = portal
        .create_session(CreateSessionOptions::default())
        .await?;
    let activated = portal.receive_activated().await?;
    let deactivated = portal.receive_deactivated().await?;
    let shortcuts = [
        NewShortcut::new(VOICE_TOGGLE_ID, "Start or stop local voice input")
            .preferred_trigger(Some(VOICE_TOGGLE_TRIGGER)),
        NewShortcut::new(PANEL_TOGGLE_ID, "Toggle the chat panel")
            .preferred_trigger(Some(PANEL_TOGGLE_TRIGGER)),
    ];

    let request = portal
        .bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default())
        .await?;
    let response = request.response()?;
    for (action, id, fallback_label, action_label) in [
        (
            ShortcutAction::Voice,
            VOICE_TOGGLE_ID,
            VOICE_TOGGLE_LABEL,
            "voice shortcut",
        ),
        (
            ShortcutAction::Panel,
            PANEL_TOGGLE_ID,
            PANEL_TOGGLE_LABEL,
            "panel shortcut",
        ),
    ] {
        let status = response
            .shortcuts()
            .iter()
            .find(|shortcut| shortcut.id() == id)
            .map_or_else(
                || {
                    ShortcutStatus::ConflictPossible(format!(
                        "The {action_label} was not bound by the desktop portal. Configure the compositor to route it to ChatHead AI."
                    ))
                },
                |shortcut| {
                    let trigger = shortcut.trigger_description().trim();
                    ShortcutStatus::Ready(if trigger.is_empty() {
                        fallback_label.to_owned()
                    } else {
                        trigger.to_owned()
                    })
                },
            );
        if sender
            .send(AppEvent::ShortcutStatus(action, status))
            .is_err()
        {
            return Ok(());
        }
    }

    let activated = activated.fuse();
    let deactivated = deactivated.fuse();
    futures_util::pin_mut!(activated, deactivated);
    loop {
        futures_util::select! {
            event = activated.next() => {
                let Some(event) = event else { break };
                match event.shortcut_id() {
                    VOICE_TOGGLE_ID => {
                        if sender.send(AppEvent::VoiceShortcutActivated).is_err() {
                            break;
                        }
                    }
                    PANEL_TOGGLE_ID => {
                        if sender.send(AppEvent::TogglePanel).is_err() {
                            break;
                        }
                    }
                    _ => {}
                }
            },
            event = deactivated.next() => {
                let Some(event) = event else { break };
                if event.shortcut_id() == VOICE_TOGGLE_ID
                    && sender.send(AppEvent::VoiceShortcutDeactivated).is_err()
                {
                    break;
                }
            },
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
    let rect = visible_panel_rect(canvas.allocated_width(), canvas.allocated_height(), state);
    canvas.move_(panel, rect.x, rect.y);
    canvas.move_(chathead, state.x.get(), state.y.get());
}

fn apply_panel_position(runtime: &OverlayRuntime) {
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

fn visible_panel_rect(width: i32, height: i32, state: &OverlayState) -> PanelRect {
    state.panel_rect_override.get().unwrap_or_else(|| {
        let size = state.panel_size.get();
        let (x, y) = panel_position_for(width, height, state, PanelPosition::current());
        PanelRect {
            x,
            y,
            width: i32::from(size.width()),
            height: i32::from(size.height()),
        }
    })
}

fn panel_position_for(
    width: i32,
    height: i32,
    state: &OverlayState,
    preferred_position: PanelPosition,
) -> (f64, f64) {
    let size = state.panel_size.get();
    let panel_width = i32::from(size.width());
    let panel_height = i32::from(size.height());
    let width = width.max(panel_width + EDGE_PADDING * 2);
    let height = height.max(panel_height + EDGE_PADDING * 2);
    let preferred_x = panel_x_for(state.x.get(), preferred_position, panel_width);
    let x = if panel_fits_horizontally(preferred_x, width, panel_width) {
        preferred_x
    } else {
        panel_x_for(state.x.get(), preferred_position.opposite(), panel_width)
    };
    let y = if state.y.get() + CHATHEAD_SIZE as f64 + PANEL_GAP as f64 + f64::from(panel_height)
        <= height as f64
    {
        state.y.get() + CHATHEAD_SIZE as f64 + PANEL_GAP as f64
    } else {
        state.y.get() - f64::from(panel_height) - PANEL_GAP as f64
    };

    (
        x.clamp(
            EDGE_PADDING as f64,
            (width - panel_width - EDGE_PADDING).max(0) as f64,
        ),
        y.clamp(
            EDGE_PADDING as f64,
            (height - panel_height - EDGE_PADDING).max(0) as f64,
        ),
    )
}

fn panel_x_for(chathead_x: f64, position: PanelPosition, panel_width: i32) -> f64 {
    match position {
        PanelPosition::Left => chathead_x + f64::from(CHATHEAD_SIZE - panel_width),
        PanelPosition::Right => chathead_x,
    }
}

fn panel_fits_horizontally(panel_x: f64, width: i32, panel_width: i32) -> bool {
    panel_x >= f64::from(EDGE_PADDING)
        && panel_x + f64::from(panel_width) <= f64::from(width - EDGE_PADDING)
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

    if state.panel_open.get() && state.panel_keyboard_captured.get() {
        surface.set_input_region(None);
        return;
    }

    let region = cairo::Region::create();
    let chathead_rect = cairo::RectangleInt::new(
        state.x.get().round() as i32,
        state.y.get().round() as i32,
        CHATHEAD_SIZE,
        CHATHEAD_SIZE,
    );
    let _ = region.union_rectangle(&chathead_rect);

    if state.panel_open.get() {
        let panel = visible_panel_rect(surface.width(), surface.height(), state);
        let panel_rect = cairo::RectangleInt::new(
            panel.x.round() as i32,
            panel.y.round() as i32,
            panel.width,
            panel.height,
        );
        let _ = region.union_rectangle(&panel_rect);
    }

    surface.set_input_region(Some(&region));
}

fn point_is_in_panel(x: f64, y: f64, width: i32, height: i32, state: &OverlayState) -> bool {
    let panel = visible_panel_rect(width, height, state);
    x >= panel.x
        && x <= panel.x + f64::from(panel.width)
        && y >= panel.y
        && y <= panel.y + f64::from(panel.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn panel_stays_within_output_edges() {
        let state = OverlayState::new();
        state.x.set(1900.0);
        state.y.set(1000.0);
        let (x, y) = panel_position_for(1920, 1080, &state, PanelPosition::Right);
        let size = state.panel_size.get();
        assert!(x >= f64::from(EDGE_PADDING));
        assert!(y >= f64::from(EDGE_PADDING));
        assert!(x + f64::from(size.width()) <= f64::from(1920 - EDGE_PADDING));
        assert!(y + f64::from(size.height()) <= f64::from(1080 - EDGE_PADDING));
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
    fn panel_position_places_the_panel_on_the_selected_side() {
        let state = OverlayState::new();
        state.x.set(800.0);

        let (left_x, _) = panel_position_for(1920, 1080, &state, PanelPosition::Left);
        let (right_x, _) = panel_position_for(1920, 1080, &state, PanelPosition::Right);

        assert_eq!(left_x, 324.0);
        assert_eq!(right_x, 800.0);
        assert_eq!(state.x.get(), 800.0);
    }

    #[test]
    fn panel_position_flips_away_from_horizontal_edges() {
        let state = OverlayState::new();

        state.x.set(50.0);
        let (left_x, _) = panel_position_for(1920, 1080, &state, PanelPosition::Left);
        assert_eq!(left_x, 50.0);

        state.x.set(1_700.0);
        let (right_x, _) = panel_position_for(1920, 1080, &state, PanelPosition::Right);
        assert_eq!(right_x, 1_224.0);
    }

    #[test]
    fn panel_position_clamps_when_neither_side_fully_fits() {
        let state = OverlayState::new();
        state.x.set(1_900.0);

        let (x, _) = panel_position_for(1920, 1080, &state, PanelPosition::Right);

        assert_eq!(x, 1_344.0);
    }

    #[test]
    fn pending_voice_submission_is_exactly_once() {
        let mut pending = PendingVoice::default();
        pending.arm(7);
        assert!(pending.consume(7));
        assert!(!pending.consume(7));
    }

    #[test]
    fn canceled_or_stale_voice_submission_cannot_send() {
        let mut pending = PendingVoice::default();
        pending.arm(8);
        assert!(!pending.consume(7));
        assert!(pending.cancel());
        assert!(!pending.consume(8));
    }

    #[test]
    fn voice_send_delay_is_fixed_at_seven_hundred_milliseconds() {
        assert_eq!(VOICE_SEND_DELAY, Duration::from_millis(700));
    }

    #[test]
    fn voice_submission_mode_controls_automatic_send() {
        assert!(!should_auto_send_voice(VoiceSubmissionMode::InsertOnly));
        assert!(should_auto_send_voice(VoiceSubmissionMode::InsertAndSend));
    }

    #[test]
    fn panel_metrics_scale_at_every_zoom_level() {
        let metrics = chathead_core::PANEL_ZOOM_LEVELS.map(|level| {
            PanelMetrics::for_zoom(PanelZoom::try_from(level).expect("valid panel zoom"))
        });
        for pair in metrics.windows(2) {
            assert!(pair[0].panel_padding <= pair[1].panel_padding);
            assert!(pair[0].body_font <= pair[1].body_font);
            assert!(pair[0].composer_height <= pair[1].composer_height);
        }
        assert!(metrics[0].panel_padding < metrics[2].panel_padding);
        assert!(metrics[5].panel_padding > metrics[2].panel_padding);
    }

    #[test]
    fn panel_metrics_enforce_text_and_target_floors() {
        let metrics = PanelMetrics::for_zoom(PanelZoom::try_from(80).expect("valid panel zoom"));
        assert!(metrics.title_font >= 10);
        assert!(metrics.body_font >= 10);
        assert!(metrics.small_font >= 10);
        assert!(metrics.compact_target >= 28);
        assert!(metrics.send_target >= 28);
    }

    #[test]
    fn panel_size_caps_follow_output_and_preserve_tiny_output_minimums() {
        let maximum = PanelSize::try_new(960, 800).expect("maximum size");
        assert_eq!(effective_panel_size(maximum, 1920, 1080), maximum);
        assert_eq!(
            effective_panel_size(maximum, 800, 600),
            PanelSize::try_new(768, 568).expect("monitor-capped size")
        );
        assert_eq!(
            effective_panel_size(maximum, 400, 440),
            PanelSize::try_new(420, 460).expect("minimum size")
        );
    }

    #[test]
    fn resize_hit_testing_covers_every_edge_and_corner() {
        let cases = [
            ((1.0, 1.0), ResizeEdge::NorthWest),
            ((280.0, 1.0), ResizeEdge::North),
            ((559.0, 1.0), ResizeEdge::NorthEast),
            ((559.0, 230.0), ResizeEdge::East),
            ((559.0, 459.0), ResizeEdge::SouthEast),
            ((280.0, 459.0), ResizeEdge::South),
            ((1.0, 459.0), ResizeEdge::SouthWest),
            ((1.0, 230.0), ResizeEdge::West),
        ];
        for (point, expected) in cases {
            assert_eq!(resize_edge_at(point.0, point.1, 560, 460), Some(expected));
        }
        assert_eq!(resize_edge_at(280.0, 230.0, 560, 460), None);
    }

    #[test]
    fn resizing_keeps_the_opposite_edges_fixed() {
        let start = PanelRect {
            x: 100.0,
            y: 100.0,
            width: 560,
            height: 460,
        };
        let west = resized_panel_rect(start, ResizeEdge::West, -50.0, 0.0, 1920, 1080);
        assert_eq!(west.x, 50.0);
        assert_eq!(west.width, 610);
        assert_eq!(west.x + f64::from(west.width), 660.0);

        let north = resized_panel_rect(start, ResizeEdge::North, 0.0, -50.0, 1920, 1080);
        assert_eq!(north.y, 50.0);
        assert_eq!(north.height, 510);
        assert_eq!(north.y + f64::from(north.height), 560.0);
    }

    #[test]
    fn resizing_clamps_at_minimum_maximum_and_output_edges() {
        let start = PanelRect {
            x: 100.0,
            y: 100.0,
            width: 560,
            height: 460,
        };
        let minimum = resized_panel_rect(start, ResizeEdge::West, 500.0, 0.0, 1920, 1080);
        assert_eq!(minimum.width, 420);
        assert_eq!(minimum.x + f64::from(minimum.width), 660.0);

        let maximum =
            resized_panel_rect(start, ResizeEdge::SouthEast, 2_000.0, 2_000.0, 1920, 1080);
        assert_eq!((maximum.width, maximum.height), (960, 800));
        assert!(maximum.x + f64::from(maximum.width) <= f64::from(1920 - EDGE_PADDING));
        assert!(maximum.y + f64::from(maximum.height) <= f64::from(1080 - EDGE_PADDING));
    }
}
