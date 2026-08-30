//! Safe, UI-local Markdown parsing and clipboard projections for assistant responses.

use std::fmt::Write as _;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag};
use serde::Serialize;
use url::Url;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ResponseDocument {
    pub(crate) source: String,
    pub(crate) blocks: Vec<ResponseBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "content", rename_all = "camelCase")]
pub(crate) enum ResponseBlock {
    Paragraph(Vec<InlineSpan>),
    Heading {
        level: u8,
        spans: Vec<InlineSpan>,
    },
    Quote(Vec<ResponseBlock>),
    Code {
        language: Option<String>,
        text: String,
        closed: bool,
    },
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    Table {
        alignments: Vec<TableAlignment>,
        header: Vec<Vec<InlineSpan>>,
        rows: Vec<Vec<Vec<InlineSpan>>>,
    },
    Footnote {
        label: String,
        blocks: Vec<ResponseBlock>,
    },
    DefinitionList(Vec<DefinitionItem>),
    Separator,
    DisplayMath(String),
    Literal(String),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ListItem {
    pub(crate) checked: Option<bool>,
    pub(crate) blocks: Vec<ResponseBlock>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DefinitionItem {
    pub(crate) term: Vec<InlineSpan>,
    pub(crate) definitions: Vec<Vec<ResponseBlock>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum TableAlignment {
    None,
    Left,
    Center,
    Right,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", content = "content", rename_all = "camelCase")]
pub(crate) enum InlineSpan {
    Text(String),
    Emphasis(Vec<InlineSpan>),
    Strong(Vec<InlineSpan>),
    Strikethrough(Vec<InlineSpan>),
    Code(String),
    Link {
        label: Vec<InlineSpan>,
        destination: String,
        safe: bool,
    },
    Image {
        description: Vec<InlineSpan>,
        destination: String,
        safe: bool,
    },
    Math(String),
    FootnoteReference(String),
    SoftBreak,
    HardBreak,
}

#[derive(Clone, Debug)]
enum Node {
    Container(Container, Vec<Node>),
    Text(String),
    Code(String),
    InlineMath(String),
    DisplayMath(String),
    Html(String),
    FootnoteReference(String),
    SoftBreak,
    HardBreak,
    Rule,
    Task(bool),
}

#[derive(Clone, Debug)]
enum Container {
    Root,
    Paragraph,
    Heading(u8),
    Quote,
    Code(Option<String>),
    List(Option<u64>),
    Item,
    Footnote(String),
    DefinitionList,
    DefinitionTitle,
    DefinitionValue,
    Table(Vec<TableAlignment>),
    TableHead,
    TableRow,
    TableCell,
    Emphasis,
    Strong,
    Strikethrough,
    Link(String),
    Image(String),
    HtmlBlock,
    Transparent,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct StableMarkdownPrefix {
    pub(crate) stable_len: usize,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct StableMarkdownTracker {
    scanned_len: usize,
    stable_len: usize,
    fence: Option<(char, usize)>,
}

impl StableMarkdownTracker {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn update(&mut self, source: &str) -> StableMarkdownPrefix {
        if self.scanned_len > source.len() || !source.is_char_boundary(self.scanned_len) {
            self.reset();
        }

        let mut offset = self.scanned_len;
        for line in source[self.scanned_len..].split_inclusive('\n') {
            // An unfinished line may receive more bytes in the next delta, so
            // leave it unscanned until its terminating newline arrives.
            if !line.ends_with('\n') {
                break;
            }
            update_stable_markdown_line(line, offset, &mut self.stable_len, &mut self.fence);
            offset += line.len();
            self.scanned_len = offset;
        }

        StableMarkdownPrefix {
            stable_len: self.stable_len,
        }
    }
}

impl ResponseDocument {
    #[must_use]
    pub(crate) fn parse(source: &str, terminal: bool) -> Self {
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_DEFINITION_LIST
            | Options::ENABLE_MATH;
        let normalized_source = normalize_math_delimiters(source);
        let mut stack = vec![(Container::Root, Vec::new())];

        for event in Parser::new_ext(&normalized_source, options) {
            match event {
                Event::Start(tag) => stack.push((container_from_tag(tag), Vec::new())),
                Event::End(_) => close_container(&mut stack),
                Event::Text(text) => push_node(&mut stack, Node::Text(text.into_string())),
                Event::Code(text) => push_node(&mut stack, Node::Code(text.into_string())),
                Event::InlineMath(text) => {
                    push_node(&mut stack, Node::InlineMath(text.into_string()));
                }
                Event::DisplayMath(text) => {
                    push_node(&mut stack, Node::DisplayMath(text.into_string()));
                }
                Event::Html(text) | Event::InlineHtml(text) => {
                    push_node(&mut stack, Node::Html(text.into_string()));
                }
                Event::FootnoteReference(label) => {
                    push_node(&mut stack, Node::FootnoteReference(label.into_string()));
                }
                Event::SoftBreak => push_node(&mut stack, Node::SoftBreak),
                Event::HardBreak => push_node(&mut stack, Node::HardBreak),
                Event::Rule => push_node(&mut stack, Node::Rule),
                Event::TaskListMarker(checked) => push_node(&mut stack, Node::Task(checked)),
            }
        }
        while stack.len() > 1 {
            close_container(&mut stack);
        }

        let nodes = stack.pop().map_or_else(Vec::new, |(_, nodes)| nodes);
        Self {
            source: source.to_owned(),
            blocks: nodes_to_blocks(nodes, source, terminal),
        }
    }

    #[must_use]
    pub(crate) fn plain_text(&self) -> String {
        let mut output = String::new();
        write_blocks_plain(&self.blocks, 0, &mut output);
        output.trim_end().to_owned()
    }
}

/// Converts the alternate TeX delimiters commonly emitted by LLMs into the
/// dollar delimiters understood by pulldown-cmark. Fenced and inline code are
/// deliberately left byte-for-byte unchanged.
fn normalize_math_delimiters(source: &str) -> String {
    let mut normalized = String::with_capacity(source.len());
    let mut fence: Option<(char, usize)> = None;

    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next();
        let run = marker.map_or(0, |character| {
            trimmed
                .chars()
                .take_while(|next| *next == character)
                .count()
        });
        let fence_line = matches!(marker, Some('`' | '~')) && run >= 3;

        if let Some((character, width)) = fence {
            normalized.push_str(line);
            if marker == Some(character) && run >= width && trimmed[run..].trim().is_empty() {
                fence = None;
            }
        } else if fence_line {
            normalized.push_str(line);
            fence = Some((marker.expect("matched fence marker"), run));
        } else {
            normalize_math_line(line, &mut normalized);
        }
    }

    normalized
}

fn normalize_math_line(line: &str, output: &mut String) {
    let bytes = line.as_bytes();
    let mut index = 0;
    let mut code_ticks = None;

    while index < bytes.len() {
        if bytes[index] == b'`' {
            let width = bytes[index..]
                .iter()
                .take_while(|byte| **byte == b'`')
                .count();
            output.push_str(&line[index..index + width]);
            code_ticks = match code_ticks {
                Some(open_width) if open_width == width => None,
                None => Some(width),
                current => current,
            };
            index += width;
            continue;
        }

        if code_ticks.is_none() && bytes[index] == b'\\' && index + 1 < bytes.len() {
            match bytes[index + 1] {
                b'\\' => {
                    output.push_str("\\\\");
                    index += 2;
                    continue;
                }
                b'[' | b']' => {
                    output.push_str("$$");
                    index += 2;
                    continue;
                }
                b'(' | b')' => {
                    output.push('$');
                    index += 2;
                    continue;
                }
                _ => {}
            }
        }

        let character = line[index..]
            .chars()
            .next()
            .expect("index remains on a UTF-8 boundary");
        output.push(character);
        index += character.len_utf8();
    }
}

#[must_use]
#[cfg(test)]
pub(crate) fn stable_markdown_prefix(source: &str) -> StableMarkdownPrefix {
    let mut stable_len = 0;
    let mut offset = 0;
    let mut fence: Option<(char, usize)> = None;

    for line in source.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let marker = trimmed.chars().next();
        let run = marker.map_or(0, |character| {
            trimmed
                .chars()
                .take_while(|next| *next == character)
                .count()
        });
        if let Some((character, width)) = fence {
            if marker == Some(character) && run >= width && trimmed[run..].trim().is_empty() {
                fence = None;
                stable_len = offset + line.len();
            }
        } else if matches!(marker, Some('`' | '~')) && run >= 3 {
            fence = Some((marker.expect("matched fence marker"), run));
        } else if trimmed.trim().is_empty() {
            stable_len = offset + line.len();
        }
        offset += line.len();
    }

    StableMarkdownPrefix { stable_len }
}

fn update_stable_markdown_line(
    line: &str,
    offset: usize,
    stable_len: &mut usize,
    fence: &mut Option<(char, usize)>,
) {
    let trimmed = line.trim_start();
    let marker = trimmed.chars().next();
    let run = marker.map_or(0, |character| {
        trimmed
            .chars()
            .take_while(|next| *next == character)
            .count()
    });
    if let Some((character, width)) = *fence {
        if marker == Some(character) && run >= width && trimmed[run..].trim().is_empty() {
            *fence = None;
            *stable_len = offset + line.len();
        }
    } else if matches!(marker, Some('`' | '~')) && run >= 3 {
        *fence = Some((marker.expect("matched fence marker"), run));
    } else if trimmed.trim().is_empty() {
        *stable_len = offset + line.len();
    }
}

#[must_use]
pub(crate) fn safe_web_uri(destination: &str) -> Option<Url> {
    let parsed = Url::parse(destination).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        return None;
    }
    Some(parsed)
}

#[must_use]
pub(crate) fn inline_plain(spans: &[InlineSpan]) -> String {
    let mut output = String::new();
    write_inline_plain(spans, &mut output);
    output
}

#[must_use]
pub(crate) fn inline_pango_markup(spans: &[InlineSpan]) -> String {
    let mut output = String::new();
    write_inline_markup(spans, &mut output);
    output
}

fn container_from_tag(tag: Tag<'_>) -> Container {
    match tag {
        Tag::Paragraph => Container::Paragraph,
        Tag::Heading { level, .. } => Container::Heading(heading_level(level)),
        Tag::BlockQuote(_) => Container::Quote,
        Tag::CodeBlock(kind) => Container::Code(match kind {
            CodeBlockKind::Indented => None,
            CodeBlockKind::Fenced(info) => normalize_language(&info),
        }),
        Tag::HtmlBlock => Container::HtmlBlock,
        Tag::List(start) => Container::List(start),
        Tag::Item => Container::Item,
        Tag::FootnoteDefinition(label) => Container::Footnote(label.into_string()),
        Tag::DefinitionList => Container::DefinitionList,
        Tag::DefinitionListTitle => Container::DefinitionTitle,
        Tag::DefinitionListDefinition => Container::DefinitionValue,
        Tag::Table(alignments) => {
            Container::Table(alignments.into_iter().map(table_alignment).collect())
        }
        Tag::TableHead => Container::TableHead,
        Tag::TableRow => Container::TableRow,
        Tag::TableCell => Container::TableCell,
        Tag::Emphasis => Container::Emphasis,
        Tag::Strong => Container::Strong,
        Tag::Strikethrough => Container::Strikethrough,
        Tag::Link { dest_url, .. } => Container::Link(dest_url.into_string()),
        Tag::Image { dest_url, .. } => Container::Image(dest_url.into_string()),
        Tag::Superscript | Tag::Subscript | Tag::MetadataBlock(_) => Container::Transparent,
    }
}

fn close_container(stack: &mut Vec<(Container, Vec<Node>)>) {
    if stack.len() <= 1 {
        return;
    }
    let Some((container, nodes)) = stack.pop() else {
        return;
    };
    push_node(stack, Node::Container(container, nodes));
}

fn push_node(stack: &mut [(Container, Vec<Node>)], node: Node) {
    if let Some((_, nodes)) = stack.last_mut() {
        nodes.push(node);
    }
}

fn nodes_to_blocks(nodes: Vec<Node>, source: &str, terminal: bool) -> Vec<ResponseBlock> {
    let mut blocks = Vec::new();
    for node in nodes {
        match node {
            Node::Container(Container::Paragraph, children) => {
                if let [Node::DisplayMath(text)] = children.as_slice() {
                    blocks.push(ResponseBlock::DisplayMath(text.clone()));
                } else {
                    blocks.push(ResponseBlock::Paragraph(nodes_to_inline(children)));
                }
            }
            Node::Container(Container::Heading(level), children) => {
                blocks.push(ResponseBlock::Heading {
                    level,
                    spans: nodes_to_inline(children),
                });
            }
            Node::Container(Container::Quote, children) => blocks.push(ResponseBlock::Quote(
                nodes_to_blocks(children, source, terminal),
            )),
            Node::Container(Container::Code(language), children) => {
                let mut text = String::new();
                collect_literal(&children, &mut text);
                blocks.push(ResponseBlock::Code {
                    language,
                    text,
                    closed: !has_unclosed_fence(source),
                });
            }
            Node::Container(Container::List(start), children) => {
                let items = children
                    .into_iter()
                    .filter_map(|child| match child {
                        Node::Container(Container::Item, mut item_nodes) => {
                            let checked = match item_nodes.first() {
                                Some(Node::Task(value)) => Some(*value),
                                _ => None,
                            };
                            if checked.is_some() {
                                item_nodes.remove(0);
                            }
                            Some(ListItem {
                                checked,
                                blocks: nodes_to_blocks(item_nodes, source, terminal),
                            })
                        }
                        _ => None,
                    })
                    .collect();
                blocks.push(ResponseBlock::List { start, items });
            }
            Node::Container(Container::Table(alignments), children) => {
                let (header, rows) = table_parts(children);
                blocks.push(ResponseBlock::Table {
                    alignments,
                    header,
                    rows,
                });
            }
            Node::Container(Container::Footnote(label), children) => {
                blocks.push(ResponseBlock::Footnote {
                    label,
                    blocks: nodes_to_blocks(children, source, terminal),
                });
            }
            Node::Container(Container::DefinitionList, children) => {
                blocks.push(ResponseBlock::DefinitionList(definition_items(
                    children, source, terminal,
                )));
            }
            Node::Container(Container::HtmlBlock, children) => {
                let mut text = String::new();
                collect_literal(&children, &mut text);
                blocks.push(ResponseBlock::Literal(text));
            }
            Node::DisplayMath(text) => blocks.push(ResponseBlock::DisplayMath(text)),
            Node::Rule => blocks.push(ResponseBlock::Separator),
            Node::Text(text) | Node::Html(text) => blocks.push(ResponseBlock::Literal(text)),
            Node::SoftBreak | Node::HardBreak => blocks.push(ResponseBlock::Literal("\n".into())),
            Node::Container(_, children) => {
                blocks.extend(nodes_to_blocks(children, source, terminal));
            }
            Node::Code(text) | Node::InlineMath(text) | Node::FootnoteReference(text) => {
                blocks.push(ResponseBlock::Literal(text));
            }
            Node::Task(_) => {}
        }
    }
    if blocks.is_empty() && !source.is_empty() {
        blocks.push(ResponseBlock::Literal(source.to_owned()));
    }
    blocks
}

fn nodes_to_inline(nodes: Vec<Node>) -> Vec<InlineSpan> {
    nodes
        .into_iter()
        .map(|node| match node {
            Node::Text(text) | Node::Html(text) => InlineSpan::Text(text),
            Node::Code(text) => InlineSpan::Code(text),
            Node::InlineMath(text) | Node::DisplayMath(text) => InlineSpan::Math(text),
            Node::FootnoteReference(label) => InlineSpan::FootnoteReference(label),
            Node::SoftBreak => InlineSpan::SoftBreak,
            Node::HardBreak => InlineSpan::HardBreak,
            Node::Container(Container::Emphasis, children) => {
                InlineSpan::Emphasis(nodes_to_inline(children))
            }
            Node::Container(Container::Strong, children) => {
                InlineSpan::Strong(nodes_to_inline(children))
            }
            Node::Container(Container::Strikethrough, children) => {
                InlineSpan::Strikethrough(nodes_to_inline(children))
            }
            Node::Container(Container::Link(destination), children) => InlineSpan::Link {
                label: nodes_to_inline(children),
                safe: safe_web_uri(&destination).is_some(),
                destination,
            },
            Node::Container(Container::Image(destination), children) => InlineSpan::Image {
                description: nodes_to_inline(children),
                safe: safe_web_uri(&destination).is_some(),
                destination,
            },
            Node::Container(_, children) => {
                InlineSpan::Text(inline_plain(&nodes_to_inline(children)))
            }
            Node::Rule => InlineSpan::Text("—".into()),
            Node::Task(checked) => InlineSpan::Text(if checked { "☑ " } else { "☐ " }.into()),
        })
        .collect()
}

fn table_parts(children: Vec<Node>) -> (Vec<Vec<InlineSpan>>, Vec<Vec<Vec<InlineSpan>>>) {
    let mut header = Vec::new();
    let mut rows = Vec::new();
    for child in children {
        match child {
            Node::Container(Container::TableHead, head_children) => {
                header = table_cells(head_children);
            }
            Node::Container(Container::TableRow, row_children) => {
                rows.push(table_cells(row_children));
            }
            _ => {}
        }
    }
    (header, rows)
}

fn table_cells(children: Vec<Node>) -> Vec<Vec<InlineSpan>> {
    children
        .into_iter()
        .filter_map(|node| match node {
            Node::Container(Container::TableCell, children) => Some(nodes_to_inline(children)),
            Node::Container(Container::TableRow, children) => Some(nodes_to_inline(children)),
            _ => None,
        })
        .collect()
}

fn definition_items(children: Vec<Node>, source: &str, terminal: bool) -> Vec<DefinitionItem> {
    let mut items = Vec::new();
    for child in children {
        match child {
            Node::Container(Container::DefinitionTitle, children) => {
                items.push(DefinitionItem {
                    term: nodes_to_inline(children),
                    definitions: Vec::new(),
                });
            }
            Node::Container(Container::DefinitionValue, children) => {
                if let Some(item) = items.last_mut() {
                    item.definitions
                        .push(nodes_to_blocks(children, source, terminal));
                }
            }
            _ => {}
        }
    }
    items
}

fn collect_literal(nodes: &[Node], output: &mut String) {
    for node in nodes {
        match node {
            Node::Text(text)
            | Node::Code(text)
            | Node::InlineMath(text)
            | Node::DisplayMath(text)
            | Node::Html(text)
            | Node::FootnoteReference(text) => output.push_str(text),
            Node::SoftBreak | Node::HardBreak => output.push('\n'),
            Node::Container(_, children) => collect_literal(children, output),
            Node::Rule => output.push_str("---"),
            Node::Task(checked) => output.push_str(if *checked { "[x] " } else { "[ ] " }),
        }
    }
}

fn write_blocks_plain(blocks: &[ResponseBlock], depth: usize, output: &mut String) {
    for block in blocks {
        match block {
            ResponseBlock::Paragraph(spans) | ResponseBlock::Heading { spans, .. } => {
                write_inline_plain(spans, output);
                output.push_str("\n\n");
            }
            ResponseBlock::Quote(children) => {
                let mut quote = String::new();
                write_blocks_plain(children, depth + 1, &mut quote);
                for line in quote.trim_end().lines() {
                    let _ = writeln!(output, "> {line}");
                }
                output.push('\n');
            }
            ResponseBlock::Code { text, .. } => {
                output.push_str(text);
                if !text.ends_with('\n') {
                    output.push('\n');
                }
                output.push('\n');
            }
            ResponseBlock::List { start, items } => {
                for (index, item) in items.iter().enumerate() {
                    let indent = "  ".repeat(depth);
                    let marker = start.map_or_else(
                        || "-".to_owned(),
                        |first| format!("{}.", first.saturating_add(index as u64)),
                    );
                    let check = item
                        .checked
                        .map_or("", |checked| if checked { "[x] " } else { "[ ] " });
                    let mut body = String::new();
                    write_blocks_plain(&item.blocks, depth + 1, &mut body);
                    for (line_index, line) in body.trim_end().lines().enumerate() {
                        if line_index == 0 {
                            let _ = writeln!(output, "{indent}{marker} {check}{line}");
                        } else {
                            let _ = writeln!(output, "{indent}  {line}");
                        }
                    }
                }
                output.push('\n');
            }
            ResponseBlock::Table { header, rows, .. } => {
                if !header.is_empty() {
                    write_table_row(header, output);
                }
                for row in rows {
                    write_table_row(row, output);
                }
                output.push('\n');
            }
            ResponseBlock::Footnote { label, blocks } => {
                let _ = write!(output, "[{label}] ");
                write_blocks_plain(blocks, depth, output);
            }
            ResponseBlock::DefinitionList(items) => {
                for item in items {
                    write_inline_plain(&item.term, output);
                    output.push('\n');
                    for definition in &item.definitions {
                        output.push_str(": ");
                        write_blocks_plain(definition, depth + 1, output);
                    }
                }
            }
            ResponseBlock::Separator => output.push_str("---\n\n"),
            ResponseBlock::DisplayMath(text) => {
                let _ = writeln!(output, "$${text}$$\n");
            }
            ResponseBlock::Literal(text) => {
                output.push_str(text);
                output.push_str("\n\n");
            }
        }
    }
}

fn write_table_row(cells: &[Vec<InlineSpan>], output: &mut String) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            output.push('\t');
        }
        write_inline_plain(cell, output);
    }
    output.push('\n');
}

fn write_inline_plain(spans: &[InlineSpan], output: &mut String) {
    for span in spans {
        match span {
            InlineSpan::Text(text) | InlineSpan::Code(text) => output.push_str(text),
            InlineSpan::Emphasis(children)
            | InlineSpan::Strong(children)
            | InlineSpan::Strikethrough(children) => write_inline_plain(children, output),
            InlineSpan::Link {
                label, destination, ..
            } => {
                write_inline_plain(label, output);
                let _ = write!(output, " ({destination})");
            }
            InlineSpan::Image {
                description,
                destination,
                ..
            } => {
                output.push_str("Image: ");
                write_inline_plain(description, output);
                let _ = write!(output, " ({destination})");
            }
            InlineSpan::Math(text) => {
                let _ = write!(output, "${text}$");
            }
            InlineSpan::FootnoteReference(label) => {
                let _ = write!(output, "[{label}]");
            }
            InlineSpan::SoftBreak => output.push(' '),
            InlineSpan::HardBreak => output.push('\n'),
        }
    }
}

fn write_inline_markup(spans: &[InlineSpan], output: &mut String) {
    for span in spans {
        match span {
            InlineSpan::Text(text) => output.push_str(&escape_markup(text)),
            InlineSpan::Emphasis(children) => wrap_markup("i", children, output),
            InlineSpan::Strong(children) => wrap_markup("b", children, output),
            InlineSpan::Strikethrough(children) => wrap_markup("s", children, output),
            InlineSpan::Code(text) => {
                let _ = write!(output, "<tt>{}</tt>", escape_markup(text));
            }
            InlineSpan::Link {
                label,
                destination,
                safe,
            } => {
                if *safe {
                    let _ = write!(output, "<a href=\"{}\">", escape_markup(destination));
                    write_inline_markup(label, output);
                    output.push_str("</a>");
                } else {
                    write_inline_markup(label, output);
                }
            }
            InlineSpan::Image {
                description,
                destination,
                safe,
            } => {
                output.push_str("<i>Image: ");
                write_inline_markup(description, output);
                output.push_str("</i>");
                if *safe {
                    let _ = write!(
                        output,
                        " <a href=\"{}\">link</a>",
                        escape_markup(destination)
                    );
                }
            }
            InlineSpan::Math(text) => {
                let _ = write!(output, "<tt>${}$</tt>", escape_markup(text));
            }
            InlineSpan::FootnoteReference(label) => {
                let _ = write!(output, "<sup>[{}]</sup>", escape_markup(label));
            }
            InlineSpan::SoftBreak => output.push(' '),
            InlineSpan::HardBreak => output.push('\n'),
        }
    }
}

fn wrap_markup(tag: &str, children: &[InlineSpan], output: &mut String) {
    let _ = write!(output, "<{tag}>");
    write_inline_markup(children, output);
    let _ = write!(output, "</{tag}>");
}

fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn table_alignment(alignment: Alignment) -> TableAlignment {
    match alignment {
        Alignment::None => TableAlignment::None,
        Alignment::Left => TableAlignment::Left,
        Alignment::Center => TableAlignment::Center,
        Alignment::Right => TableAlignment::Right,
    }
}

fn normalize_language(info: &str) -> Option<String> {
    let language = info
        .split(|character: char| character.is_ascii_whitespace() || character == ',')
        .next()
        .unwrap_or_default()
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    (!language.is_empty()).then_some(language)
}

fn has_unclosed_fence(source: &str) -> bool {
    let mut fence: Option<(char, usize)> = None;
    for line in source.lines() {
        let trimmed = line.trim_start();
        let Some(character @ ('`' | '~')) = trimmed.chars().next() else {
            continue;
        };
        let width = trimmed
            .chars()
            .take_while(|next| *next == character)
            .count();
        if width < 3 {
            continue;
        }
        match fence {
            Some((open, minimum))
                if open == character && width >= minimum && trimmed[width..].trim().is_empty() =>
            {
                fence = None;
            }
            None => fence = Some((character, width)),
            _ => {}
        }
    }
    fence.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested_lists_tasks_and_unicode() {
        let document = ResponseDocument::parse("- [x] café\n  1. 東京\n  2. **bold**\n", true);
        let ResponseBlock::List { items, .. } = &document.blocks[0] else {
            panic!("expected list");
        };
        assert_eq!(items[0].checked, Some(true));
        assert!(document.plain_text().contains("café"));
        assert!(document.plain_text().contains("東京"));
    }

    #[test]
    fn parses_table_alignment_and_flattens_with_tabs() {
        let document = ResponseDocument::parse("| Left | Right |\n|:--|--:|\n| a | b |", true);
        let ResponseBlock::Table { alignments, .. } = &document.blocks[0] else {
            panic!("expected table");
        };
        assert_eq!(alignments, &[TableAlignment::Left, TableAlignment::Right]);
        assert_eq!(document.plain_text(), "Left\tRight\na\tb");
    }

    #[test]
    fn preserves_raw_html_as_literal_escaped_markup() {
        let document = ResponseDocument::parse("<script>alert('x')</script>", true);
        assert_eq!(document.plain_text(), "<script>alert('x')</script>");
        let markup = inline_pango_markup(&[InlineSpan::Text("<b>unsafe</b>".into())]);
        assert_eq!(markup, "&lt;b&gt;unsafe&lt;/b&gt;");
    }

    #[test]
    fn parses_links_images_math_footnotes_and_definitions() {
        let source = "[safe](https://example.com) [bad](file:///tmp/x) ![plot](https://example.com/p.png) $x^2$[^a]\n\n[^a]: note\n\nTerm\n: meaning";
        let document = ResponseDocument::parse(source, true);
        let plain = document.plain_text();
        assert!(plain.contains("safe (https://example.com)"));
        assert!(plain.contains("Image: plot"));
        assert!(plain.contains("$x^2$"));
        assert!(plain.contains("meaning"));
    }

    #[test]
    fn normalizes_tex_math_delimiters_but_preserves_code() {
        let source = "\\[ 3x + 5 = 20 \\]\n\nInline \\(x^2\\).\n\n`\\(not math\\)`\n\n```tex\n\\[not math\\]\n```";
        let document = ResponseDocument::parse(source, true);
        assert!(matches!(
            &document.blocks[0],
            ResponseBlock::DisplayMath(text) if text.trim() == "3x + 5 = 20"
        ));
        assert!(matches!(
            &document.blocks[1],
            ResponseBlock::Paragraph(spans)
                if spans.iter().any(|span| matches!(span, InlineSpan::Math(text) if text == "x^2"))
        ));
        assert!(document.plain_text().contains(r"\(not math\)"));
        assert!(document.plain_text().contains(r"\[not math\]"));
        assert_eq!(document.source, source);
    }

    #[test]
    fn uri_policy_rejects_credentials_and_unsafe_or_relative_targets() {
        assert!(safe_web_uri("https://example.com/path").is_some());
        assert!(safe_web_uri("http://example.com").is_some());
        assert!(safe_web_uri("https://user:pass@example.com").is_none());
        assert!(safe_web_uri("mailto:a@example.com").is_none());
        assert!(safe_web_uri("/relative").is_none());
        assert!(safe_web_uri("javascript:alert(1)").is_none());
    }

    #[test]
    fn incomplete_fence_is_monochrome_until_terminal() {
        let source = "```rust\nfn main() {}\n";
        let streaming = ResponseDocument::parse(source, false);
        let ResponseBlock::Code { closed, text, .. } = &streaming.blocks[0] else {
            panic!("expected code");
        };
        assert!(!closed);
        assert_eq!(text, "fn main() {}\n");
        let interrupted = ResponseDocument::parse(source, true);
        assert!(matches!(
            interrupted.blocks[0],
            ResponseBlock::Code { closed: false, .. }
        ));
    }

    #[test]
    fn stable_prefix_is_monotonic_for_token_chunks() {
        let source = "First paragraph.\n\n- one\n- two\n\n```rust\nlet x = 1;\n```\n\nFinal";
        let mut previous = 0;
        for boundary in source
            .char_indices()
            .map(|(index, _)| index)
            .skip(1)
            .chain([source.len()])
        {
            let current = stable_markdown_prefix(&source[..boundary]).stable_len;
            assert!(current >= previous, "prefix regressed at byte {boundary}");
            previous = current;
        }
    }

    #[test]
    fn incremental_stable_prefix_matches_complete_line_scans() {
        let source = "First paragraph.\n\n- one\n- two\n\n```rust\nlet x = 1;\n```\n\nFinal";
        let mut tracker = StableMarkdownTracker::default();

        for boundary in source
            .char_indices()
            .map(|(index, _)| index)
            .skip(1)
            .chain([source.len()])
        {
            let chunk = &source[..boundary];
            let tracked = tracker.update(chunk).stable_len;
            let complete_line_len = chunk.rfind('\n').map_or(0, |index| index + 1);
            let expected = stable_markdown_prefix(&chunk[..complete_line_len]).stable_len;
            assert_eq!(tracked, expected, "tracker diverged at byte {boundary}");
        }
    }

    #[test]
    fn final_streamed_parse_equals_one_shot_parse() {
        let source = "# Title\n\n> quote\n\n1. one\n2. two\n";
        let streamed = ResponseDocument::parse(source, true);
        let one_shot = ResponseDocument::parse(source, true);
        assert_eq!(streamed, one_shot);
    }
}
