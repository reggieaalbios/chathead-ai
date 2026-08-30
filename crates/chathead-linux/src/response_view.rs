//! Native GTK document view for parsed assistant responses.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fmt::Write as _,
    rc::Rc,
    sync::mpsc::{self, Receiver, SyncSender, TrySendError},
    thread,
    time::Duration,
};

use chathead_core::MessageState;
use gtk::{gio, glib, prelude::*};
use syntect::{
    easy::HighlightLines,
    highlighting::{FontStyle, Style, ThemeSet},
    parsing::SyntaxSet,
    util::LinesWithEndings,
};

use crate::response_format::{
    DefinitionItem, InlineSpan, ListItem, ResponseBlock, ResponseDocument, StableMarkdownTracker,
    TableAlignment, inline_pango_markup,
};

const MAX_HIGHLIGHT_BYTES: usize = 64 * 1024;
const MAX_HIGHLIGHT_LINES: usize = 1_000;

pub(crate) type LinkHandler = Rc<dyn Fn(String)>;
pub(crate) type RetryHandler = Rc<dyn Fn()>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum DocumentTheme {
    Light,
    Dark,
}

#[derive(Clone, Debug)]
pub(crate) struct HighlightResult {
    pub(crate) message_id: String,
    pub(crate) revision: u64,
    pub(crate) block_index: usize,
    pub(crate) markup: Option<String>,
}

#[derive(Clone)]
pub(crate) struct HighlightWorker {
    sender: SyncSender<HighlightJob>,
    receiver: Rc<RefCell<Receiver<HighlightResult>>>,
}

#[derive(Clone, Debug)]
struct HighlightJob {
    message_id: String,
    revision: u64,
    block_index: usize,
    language: Option<String>,
    code: String,
    theme: DocumentTheme,
}

impl HighlightWorker {
    #[must_use]
    pub(crate) fn start() -> Self {
        let (job_sender, job_receiver) = mpsc::sync_channel::<HighlightJob>(16);
        let (result_sender, result_receiver) = mpsc::sync_channel(16);
        if let Err(error) = thread::Builder::new()
            .name("chathead-syntax-highlighter".to_owned())
            .spawn(move || highlight_loop(job_receiver, result_sender))
        {
            eprintln!("failed to start syntax highlighting worker: {error}");
        }
        Self {
            sender: job_sender,
            receiver: Rc::new(RefCell::new(result_receiver)),
        }
    }

    fn submit(&self, job: HighlightJob) {
        match self.sender.try_send(job) {
            Ok(()) | Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {}
        }
    }

    pub(crate) fn drain(&self) -> Vec<HighlightResult> {
        let mut results = Vec::new();
        while let Ok(result) = self.receiver.borrow().try_recv() {
            results.push(result);
        }
        results
    }
}

#[derive(Clone)]
pub(crate) struct AssistantDocument {
    message_id: String,
    root: gtk::Box,
    stable: gtk::Box,
    provisional: gtk::Box,
    provisional_label: gtk::Label,
    actions: gtk::Box,
    source: Rc<RefCell<String>>,
    state: Rc<Cell<MessageState>>,
    revision: Rc<Cell<u64>>,
    stable_len: Rc<Cell<usize>>,
    stable_tracker: Rc<RefCell<StableMarkdownTracker>>,
    stable_blocks: Rc<RefCell<Vec<ResponseBlock>>>,
    theme: Rc<Cell<DocumentTheme>>,
    code_labels: Rc<RefCell<HashMap<usize, gtk::Label>>>,
    worker: HighlightWorker,
    link_handler: LinkHandler,
    retry_handler: RetryHandler,
}

impl AssistantDocument {
    #[must_use]
    pub(crate) fn new(
        message_id: &str,
        source: &str,
        state: MessageState,
        theme: DocumentTheme,
        worker: &HighlightWorker,
        link_handler: LinkHandler,
        retry_handler: RetryHandler,
    ) -> Self {
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(10)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .css_classes(["assistant-document"])
            .build();
        let stable = document_box("assistant-stable");
        let provisional = document_box("assistant-provisional");
        let provisional_label = plain_label("", "response-paragraph");
        provisional.append(&provisional_label);
        stable.set_visible(false);
        provisional.set_visible(false);
        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Start)
            .css_classes(["response-actions"])
            .build();
        actions.set_visible(false);
        root.append(&stable);
        root.append(&provisional);
        root.append(&actions);

        let document = Self {
            message_id: message_id.to_owned(),
            root,
            stable,
            provisional,
            provisional_label,
            actions,
            source: Rc::new(RefCell::new(String::new())),
            state: Rc::new(Cell::new(MessageState::Streaming)),
            revision: Rc::new(Cell::new(0)),
            stable_len: Rc::new(Cell::new(0)),
            stable_tracker: Rc::new(RefCell::new(StableMarkdownTracker::default())),
            stable_blocks: Rc::new(RefCell::new(Vec::new())),
            theme: Rc::new(Cell::new(theme)),
            code_labels: Rc::new(RefCell::new(HashMap::new())),
            worker: worker.clone(),
            link_handler,
            retry_handler,
        };
        document.update(source, state, theme);
        document
    }

    #[must_use]
    pub(crate) fn widget(&self) -> gtk::Box {
        self.root.clone()
    }

    pub(crate) fn update_theme(&self, theme: DocumentTheme) {
        if self.theme.get() == theme {
            return;
        }
        let source = self.source.borrow().clone();
        self.update(&source, self.state.get(), theme);
    }

    pub(crate) fn update(&self, source: &str, state: MessageState, theme: DocumentTheme) {
        if self.source.borrow().as_str() == source
            && self.state.get() == state
            && self.theme.get() == theme
        {
            return;
        }

        let previous_state = self.state.get();
        let theme_changed = self.theme.replace(theme) != theme;
        let mut stored_source = self.source.borrow_mut();
        let appended = source.starts_with(stored_source.as_str());
        if appended {
            let previous_len = stored_source.len();
            stored_source.push_str(&source[previous_len..]);
        } else {
            stored_source.clear();
            stored_source.push_str(source);
        }
        drop(stored_source);
        if !appended {
            self.stable_tracker.borrow_mut().reset();
        }
        self.state.set(state);

        let terminal = state != MessageState::Streaming;
        if theme_changed {
            self.reset_rendered_document();
            if terminal {
                self.rebuild_complete(source);
            } else {
                self.reconcile_stream(source);
            }
        } else if terminal && previous_state == MessageState::Streaming {
            self.reconcile_stream(source);
            self.finalize_stream(source);
        } else if terminal {
            self.reset_rendered_document();
            self.rebuild_complete(source);
        } else {
            self.reconcile_stream(source);
        }
        if terminal {
            self.rebuild_actions();
        }
    }

    fn reset_rendered_document(&self) {
        self.revision.set(self.revision.get().saturating_add(1));
        self.code_labels.borrow_mut().clear();
        clear_box(&self.stable);
        self.stable_blocks.borrow_mut().clear();
        self.stable_len.set(0);
    }

    fn rebuild_complete(&self, source: &str) {
        clear_box(&self.stable);
        self.provisional_label.set_label("");
        self.provisional.set_visible(false);
        let document = ResponseDocument::parse(source, true);
        let mut block_index = 0;
        render_blocks(
            &document.blocks,
            &self.stable,
            &mut block_index,
            &RenderContext::from_document(self),
        );
        self.stable.set_visible(self.stable.first_child().is_some());
        self.stable_blocks.replace(document.blocks);
        self.stable_len.set(source.len());
    }

    fn finalize_stream(&self, source: &str) {
        let stable_len = self.stable_len.get();
        let tail = &source[stable_len..];
        if !tail.is_empty() {
            let tail_document = ResponseDocument::parse(tail, true);
            let mut block_index = count_code_blocks(&self.stable_blocks.borrow());
            render_blocks(
                &tail_document.blocks,
                &self.stable,
                &mut block_index,
                &RenderContext::from_document(self),
            );
            self.stable_blocks.borrow_mut().extend(tail_document.blocks);
        }
        self.stable_len.set(source.len());
        self.stable.set_visible(self.stable.first_child().is_some());
        self.provisional_label.set_label("");
        self.provisional.set_visible(false);
    }

    pub(crate) fn apply_highlight(&self, result: &HighlightResult) -> bool {
        if result.message_id != self.message_id || result.revision != self.revision.get() {
            return false;
        }
        let Some(label) = self.code_labels.borrow().get(&result.block_index).cloned() else {
            return false;
        };
        if let Some(markup) = &result.markup {
            label.set_markup(markup);
        }
        true
    }

    fn reconcile_stream(&self, source: &str) {
        let stable_len = self.stable_tracker.borrow_mut().update(source).stable_len;
        let previous_stable_len = self.stable_len.get();

        if stable_len != previous_stable_len {
            let stable_document = ResponseDocument::parse(&source[..stable_len], false);
            let previous = self.stable_blocks.borrow();
            let preserves_prefix = stable_document.blocks.starts_with(previous.as_slice());
            let old_count = previous.len();
            drop(previous);

            if stable_len < previous_stable_len || !preserves_prefix {
                self.revision.set(self.revision.get().saturating_add(1));
                self.code_labels.borrow_mut().clear();
                clear_box(&self.stable);
                let mut block_index = 0;
                render_blocks(
                    &stable_document.blocks,
                    &self.stable,
                    &mut block_index,
                    &RenderContext::from_document(self),
                );
            } else if stable_document.blocks.len() > old_count {
                let mut block_index = count_code_blocks(&stable_document.blocks[..old_count]);
                render_blocks(
                    &stable_document.blocks[old_count..],
                    &self.stable,
                    &mut block_index,
                    &RenderContext::from_document(self),
                );
            }
            self.stable_blocks.replace(stable_document.blocks);
            self.stable_len.set(stable_len);
            self.stable.set_visible(self.stable.first_child().is_some());
        }

        let tail = &source[stable_len..];
        self.provisional_label.set_label(tail);
        self.provisional.set_visible(!tail.is_empty());
    }

    fn rebuild_actions(&self) {
        clear_box(&self.actions);
        self.actions.set_visible(true);
        let copy = gtk::Button::builder()
            .icon_name("edit-copy-symbolic")
            .tooltip_text("Copy response")
            .focusable(true)
            .css_classes(["response-action"])
            .build();
        let source = Rc::clone(&self.source);
        copy.connect_clicked(move |button| {
            let source = source.borrow();
            let plain = ResponseDocument::parse(&source, true).plain_text();
            copy_response_to_clipboard(button, &plain, &source);
        });
        self.actions.append(&copy);

        let retry = gtk::Button::builder()
            .icon_name("view-refresh-symbolic")
            .tooltip_text("Retry this prompt")
            .focusable(true)
            .css_classes(["response-action"])
            .build();
        let retry_handler = Rc::clone(&self.retry_handler);
        retry.connect_clicked(move |_| retry_handler());
        self.actions.append(&retry);
    }
}

struct RenderContext<'a> {
    message_id: &'a str,
    revision: u64,
    theme: DocumentTheme,
    worker: &'a HighlightWorker,
    link_handler: &'a LinkHandler,
    code_labels: &'a Rc<RefCell<HashMap<usize, gtk::Label>>>,
}

impl<'a> RenderContext<'a> {
    fn from_document(document: &'a AssistantDocument) -> Self {
        Self {
            message_id: &document.message_id,
            revision: document.revision.get(),
            theme: document.theme.get(),
            worker: &document.worker,
            link_handler: &document.link_handler,
            code_labels: &document.code_labels,
        }
    }
}

fn render_blocks(
    blocks: &[ResponseBlock],
    container: &gtk::Box,
    block_index: &mut usize,
    context: &RenderContext<'_>,
) {
    for block in blocks {
        match block {
            ResponseBlock::Paragraph(spans) => {
                container.append(&inline_label(spans, "response-paragraph", context));
            }
            ResponseBlock::Heading { level, spans } => {
                container.append(&inline_label(spans, &format!("response-h{level}"), context));
            }
            ResponseBlock::Quote(children) => {
                let quote = gtk::Box::builder()
                    .orientation(gtk::Orientation::Vertical)
                    .spacing(10)
                    .hexpand(true)
                    .css_classes(["response-quote"])
                    .build();
                render_blocks(children, &quote, block_index, context);
                container.append(&quote);
            }
            ResponseBlock::Code {
                language,
                text,
                closed,
            } => {
                let index = *block_index;
                *block_index = block_index.saturating_add(1);
                container.append(&code_block(
                    index,
                    language.as_deref(),
                    text,
                    *closed,
                    context,
                ));
            }
            ResponseBlock::List { start, items } => {
                container.append(&list_block(*start, items, block_index, context));
            }
            ResponseBlock::Table {
                alignments,
                header,
                rows,
            } => container.append(&table_block(alignments, header, rows, context)),
            ResponseBlock::Footnote { label, blocks } => {
                let row = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .spacing(6)
                    .hexpand(true)
                    .css_classes(["response-footnote"])
                    .build();
                row.append(&plain_label(&format!("[{label}]"), "footnote-marker"));
                let body = document_box("footnote-body");
                render_blocks(blocks, &body, block_index, context);
                row.append(&body);
                container.append(&row);
            }
            ResponseBlock::DefinitionList(items) => {
                container.append(&definition_list(items, block_index, context));
            }
            ResponseBlock::Separator => {
                container.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
            }
            ResponseBlock::DisplayMath(text) => {
                container.append(&plain_label(text.trim(), "response-math"));
            }
            ResponseBlock::Literal(text) => {
                container.append(&plain_label(text, "response-paragraph"));
            }
        }
    }
}

fn inline_label(spans: &[InlineSpan], class: &str, context: &RenderContext<'_>) -> gtk::Label {
    let label = gtk::Label::builder()
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .use_markup(true)
        .xalign(0.0)
        .yalign(0.0)
        .hexpand(true)
        .css_classes([class])
        .build();
    label.set_markup(&inline_pango_markup(spans));
    let handler = context.link_handler.clone();
    label.connect_activate_link(move |_, destination| {
        handler(destination.to_owned());
        glib::Propagation::Stop
    });
    label
}

fn plain_label(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .yalign(0.0)
        .hexpand(true)
        .css_classes([class])
        .build()
}

fn code_block(
    index: usize,
    language: Option<&str>,
    text: &str,
    closed: bool,
    context: &RenderContext<'_>,
) -> gtk::Box {
    let block = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .hexpand(true)
        .css_classes(["response-code"])
        .build();
    if compact_code_block(text) {
        block.add_css_class("response-code-compact");
    }
    let header = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .css_classes(["code-header"])
        .build();
    let language_label = gtk::Label::builder()
        .label(language.unwrap_or("plain text"))
        .xalign(0.0)
        .hexpand(true)
        .selectable(true)
        .css_classes(["code-language"])
        .build();
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy exact code")
        .focusable(true)
        .css_classes(["code-copy"])
        .build();
    let code = text.to_owned();
    copy.connect_clicked(move |button| {
        copy_text_to_clipboard(button, &code);
    });
    header.append(&language_label);
    header.append(&copy);
    block.append(&header);

    let label = gtk::Label::builder()
        .label(text)
        .selectable(true)
        .xalign(0.0)
        .yalign(0.0)
        .css_classes(["code-content"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .child(&label)
        .build();
    block.append(&scroller);
    context.code_labels.borrow_mut().insert(index, label);

    if closed && highlightable(text) {
        context.worker.submit(HighlightJob {
            message_id: context.message_id.to_owned(),
            revision: context.revision,
            block_index: index,
            language: language.map(str::to_owned),
            code: text.to_owned(),
            theme: context.theme,
        });
    }
    block
}

fn list_block(
    start: Option<u64>,
    items: &[ListItem],
    block_index: &mut usize,
    context: &RenderContext<'_>,
) -> gtk::Box {
    let list = document_box("response-list");
    for (index, item) in items.iter().enumerate() {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(7)
            .hexpand(true)
            .css_classes(["response-list-row"])
            .build();
        let marker = item.checked.map_or_else(
            || {
                start.map_or_else(
                    || "•".to_owned(),
                    |first| format!("{}.", first.saturating_add(index as u64)),
                )
            },
            |checked| {
                if checked {
                    "☑".to_owned()
                } else {
                    "☐".to_owned()
                }
            },
        );
        row.append(&marker_label(&marker, "list-marker"));
        let body = document_box("list-body");
        body.set_halign(gtk::Align::Fill);
        render_blocks(&item.blocks, &body, block_index, context);
        row.append(&body);
        list.append(&row);
    }
    list
}

fn table_block(
    alignments: &[TableAlignment],
    header: &[Vec<InlineSpan>],
    rows: &[Vec<Vec<InlineSpan>>],
    context: &RenderContext<'_>,
) -> gtk::ScrolledWindow {
    let grid = gtk::Grid::builder()
        .row_spacing(0)
        .column_spacing(0)
        .css_classes(["response-table"])
        .build();
    if !header.is_empty() {
        attach_table_row(&grid, 0, header, alignments, true, context);
    }
    for (index, row) in rows.iter().enumerate() {
        attach_table_row(
            &grid,
            i32::try_from(index + 1).unwrap_or(i32::MAX),
            row,
            alignments,
            false,
            context,
        );
    }
    gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Never)
        .hexpand(true)
        .css_classes(["response-table-scroll"])
        .child(&grid)
        .build()
}

fn attach_table_row(
    grid: &gtk::Grid,
    row: i32,
    cells: &[Vec<InlineSpan>],
    alignments: &[TableAlignment],
    header: bool,
    context: &RenderContext<'_>,
) {
    for (column, spans) in cells.iter().enumerate() {
        let label = inline_label(
            spans,
            if header { "table-header" } else { "table-cell" },
            context,
        );
        label.set_xalign(
            match alignments
                .get(column)
                .copied()
                .unwrap_or(TableAlignment::None)
            {
                TableAlignment::Center => 0.5,
                TableAlignment::Right => 1.0,
                TableAlignment::None | TableAlignment::Left => 0.0,
            },
        );
        grid.attach(&label, i32::try_from(column).unwrap_or(i32::MAX), row, 1, 1);
    }
}

fn definition_list(
    items: &[DefinitionItem],
    block_index: &mut usize,
    context: &RenderContext<'_>,
) -> gtk::Box {
    let list = document_box("definition-list");
    for item in items {
        list.append(&inline_label(&item.term, "definition-term", context));
        for definition in &item.definitions {
            let body = document_box("definition-value");
            render_blocks(definition, &body, block_index, context);
            list.append(&body);
        }
    }
    list
}

fn copy_response_to_clipboard(button: &gtk::Button, plain: &str, markdown: &str) {
    copy_text_to_clipboard(button, response_clipboard_text(plain, markdown));
}

fn response_clipboard_text<'a>(plain: &'a str, markdown: &'a str) -> &'a str {
    if plain.is_empty() { markdown } else { plain }
}

pub(crate) fn copy_text_to_clipboard(button: &gtk::Button, text: &str) {
    button.display().clipboard().set_text(text);
    show_copied_feedback(button);
}

fn show_copied_feedback(button: &gtk::Button) {
    button.add_css_class("response-action-copied");
    button.set_tooltip_text(Some("Copied"));
    let weak_button = button.downgrade();
    glib::timeout_add_local_once(Duration::from_millis(1_400), move || {
        if let Some(button) = weak_button.upgrade() {
            button.remove_css_class("response-action-copied");
            if button.has_css_class("code-copy") {
                button.set_tooltip_text(Some("Copy exact code"));
            } else if button.has_css_class("user-copy-action") {
                button.set_tooltip_text(Some("Copy prompt"));
            } else {
                button.set_tooltip_text(Some("Copy response"));
            }
        }
    });
}

fn document_box(class: &str) -> gtk::Box {
    gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(10)
        .hexpand(true)
        .css_classes([class])
        .build()
}

fn marker_label(text: &str, class: &str) -> gtk::Label {
    gtk::Label::builder()
        .label(text)
        .selectable(true)
        .xalign(0.0)
        .yalign(0.0)
        .halign(gtk::Align::Start)
        .hexpand(false)
        .css_classes([class])
        .build()
}

fn compact_code_block(code: &str) -> bool {
    code.trim_end_matches(['\r', '\n']).lines().count() <= 1
}

fn clear_box(container: &gtk::Box) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
}

fn count_code_blocks(blocks: &[ResponseBlock]) -> usize {
    blocks.iter().map(count_code_block).sum()
}

fn count_code_block(block: &ResponseBlock) -> usize {
    match block {
        ResponseBlock::Code { .. } => 1,
        ResponseBlock::Quote(blocks) | ResponseBlock::Footnote { blocks, .. } => {
            count_code_blocks(blocks)
        }
        ResponseBlock::List { items, .. } => items
            .iter()
            .map(|item| count_code_blocks(&item.blocks))
            .sum(),
        ResponseBlock::DefinitionList(items) => items
            .iter()
            .flat_map(|item| &item.definitions)
            .map(|blocks| count_code_blocks(blocks))
            .sum(),
        _ => 0,
    }
}

fn highlightable(code: &str) -> bool {
    code.len() <= MAX_HIGHLIGHT_BYTES && code.lines().count() <= MAX_HIGHLIGHT_LINES
}

fn highlight_loop(receiver: Receiver<HighlightJob>, sender: SyncSender<HighlightResult>) {
    let syntax_set = SyntaxSet::load_defaults_newlines();
    let theme_set = ThemeSet::load_defaults();
    while let Ok(job) = receiver.recv() {
        let markup = highlight_job(&job, &syntax_set, &theme_set);
        if sender
            .send(HighlightResult {
                message_id: job.message_id,
                revision: job.revision,
                block_index: job.block_index,
                markup,
            })
            .is_err()
        {
            return;
        }
    }
}

fn highlight_job(
    job: &HighlightJob,
    syntax_set: &SyntaxSet,
    theme_set: &ThemeSet,
) -> Option<String> {
    if !highlightable(&job.code) {
        return None;
    }
    let syntax = job
        .language
        .as_deref()
        .and_then(|language| syntax_set.find_syntax_by_token(language))
        .unwrap_or_else(|| syntax_set.find_syntax_plain_text());
    let theme_name = match job.theme {
        DocumentTheme::Light => "InspiredGitHub",
        DocumentTheme::Dark => "base16-ocean.dark",
    };
    let theme = theme_set.themes.get(theme_name)?;
    let mut highlighter = HighlightLines::new(syntax, theme);
    let mut markup = String::with_capacity(job.code.len().saturating_add(job.code.len() / 2));
    for line in LinesWithEndings::from(&job.code) {
        let ranges = highlighter.highlight_line(line, syntax_set).ok()?;
        for (style, text) in ranges {
            push_highlighted_span(&mut markup, style, text);
        }
    }
    Some(markup)
}

fn push_highlighted_span(output: &mut String, style: Style, text: &str) {
    let escaped = escape_markup(text);
    let mut attributes = format!(
        "foreground=\"#{:02x}{:02x}{:02x}\"",
        style.foreground.r, style.foreground.g, style.foreground.b
    );
    if style.font_style.contains(FontStyle::BOLD) {
        attributes.push_str(" weight=\"bold\"");
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        attributes.push_str(" style=\"italic\"");
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        attributes.push_str(" underline=\"single\"");
    }
    let _ = write!(output, "<span {attributes}>{escaped}</span>");
}

fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn open_confirmed_uri(destination: &str) -> Result<(), glib::Error> {
    gio::AppInfo::launch_default_for_uri(destination, gio::AppLaunchContext::NONE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response_format::stable_markdown_prefix;

    #[test]
    fn large_code_is_not_submitted_for_highlighting() {
        let code = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
        assert!(!highlightable(&code));
        assert!(!highlightable(&"x\n".repeat(MAX_HIGHLIGHT_LINES + 1)));
    }

    #[test]
    fn unknown_language_uses_plain_text_syntax_and_both_themes_exist() {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        for theme in [DocumentTheme::Light, DocumentTheme::Dark] {
            let job = HighlightJob {
                message_id: "m".into(),
                revision: 1,
                block_index: 0,
                language: Some("definitely-unknown".into()),
                code: "hello <world>\n".into(),
                theme,
            };
            let markup = highlight_job(&job, &syntax_set, &themes).expect("highlight markup");
            assert!(markup.contains("&lt;world&gt;"));
        }
    }

    #[test]
    fn code_block_counter_includes_nested_blocks() {
        let blocks = vec![ResponseBlock::Quote(vec![ResponseBlock::Code {
            language: None,
            text: "x".into(),
            closed: true,
        }])];
        assert_eq!(count_code_blocks(&blocks), 1);
    }

    #[test]
    fn stale_highlight_identity_is_revision_tagged() {
        let result = HighlightResult {
            message_id: "assistant-1".into(),
            revision: 2,
            block_index: 0,
            markup: Some("x".into()),
        };
        assert_ne!(result.revision, 3);
    }

    #[test]
    fn only_single_line_code_uses_compact_layout() {
        assert!(compact_code_block("free -h\n"));
        assert!(compact_code_block(""));
        assert!(!compact_code_block("free -h\nfree -m\n"));
    }

    #[test]
    fn inline_plain_remains_available_for_table_and_accessibility_labels() {
        assert_eq!(
            crate::response_format::inline_plain(&[InlineSpan::Strong(vec![InlineSpan::Text(
                "x".into()
            )])]),
            "x"
        );
    }

    #[test]
    fn response_clipboard_uses_source_when_rendered_text_is_empty() {
        assert_eq!(
            response_clipboard_text("Rendered", "**Rendered**"),
            "Rendered"
        );
        assert_eq!(response_clipboard_text("", "**Fallback**"), "**Fallback**");
    }

    #[test]
    fn finalizing_only_the_unstable_tail_matches_a_complete_parse() {
        let responses = [
            "# Title\n\nFirst **paragraph**.\n\nFinal paragraph.",
            "> quoted text\n\n1. one\n2. two\n\n- final item\n",
            "| Left | Right |\n|:--|--:|\n| a | b |\n\n```rust\nfn main() {}\n```\n",
            "Before math.\n\n\\[x^2 + y^2\\]\n\nAfter math.",
        ];

        for source in responses {
            let stable_len = stable_markdown_prefix(source).stable_len;
            let mut streamed = ResponseDocument::parse(&source[..stable_len], false).blocks;
            streamed.extend(ResponseDocument::parse(&source[stable_len..], true).blocks);

            assert_eq!(streamed, ResponseDocument::parse(source, true).blocks);
        }
    }
}
