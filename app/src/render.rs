//! render.rs — comrak AST → owned blocks (parse ONCE) → GPUI elements (per frame).
//!
//! The product renderer per docs/PLAN.md §2 D2: parse CommonMark+GFM with
//! comrak (BSD-2) into an owned, animation-friendly Block tree at load time,
//! then build GPUI elements from it on every frame. The split matters: the
//! CRT tracking band animates at frame rate, and re-parsing per frame would
//! burn the snappiness pillar. NO webview — native elements, hacker palette.

use comrak::nodes::{AstNode, ListType, NodeValue};
use comrak::{Arena, Options, parse_document};
use gpui::{
    AnyElement, FontStyle, FontWeight, HighlightStyle, SharedString, StrikethroughStyle,
    StyledText, UnderlineStyle, div, prelude::*, px, rgb,
};
use std::ops::Range;

// hacker palette tokens (src/styles/theme.css)
const SURFACE: u32 = 0x08100d;
const SURFACE_ALT: u32 = 0x0e1a14;
const TEXT: u32 = 0x86efac;
const ACCENT: u32 = 0x22c55e;
const ACCENT_STRONG: u32 = 0x4ade80;
const FAINT: u32 = 0x14401f;
const MUTED: u32 = 0x3f9963;

/* ================= owned document model (parse once) ================= */

pub struct Inline {
    text: SharedString,
    runs: Vec<(Range<usize>, HighlightStyle)>,
}

pub enum Block {
    Heading { level: u8, inline: Inline },
    Paragraph(Inline),
    Code(Vec<SharedString>),
    Quote(Vec<Block>),
    List(Vec<(SharedString, Vec<Block>)>),
    Table(Vec<(bool, Vec<Inline>)>),
    Rule,
    Html(Vec<SharedString>),
}

pub fn parse(text: &str) -> Vec<Block> {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    let root = parse_document(&arena, text, &options);
    root.children().filter_map(to_block).collect()
}

/// Parse, and alongside the render tree emit per-block anchor metadata
/// (fingerprint + plain text) so comment threads can attach and survive edits.
/// `src` is reserved for future source-range bridging (kept 0..0 for now).
pub fn parse_with_meta(text: &str) -> (Vec<Block>, Vec<crate::comments::BlockMeta>) {
    let blocks = parse(text);
    let meta = blocks
        .iter()
        .map(|b| {
            let plain = block_plain(b);
            crate::comments::BlockMeta { fp: crate::comments::fingerprint(&plain), plain, src: 0..0 }
        })
        .collect();
    (blocks, meta)
}

/// Flatten a block to its plain text — the basis for a comment's fingerprint and
/// the magnifier quote. Matches what the reader sees, not the raw markdown.
pub fn block_plain(block: &Block) -> String {
    match block {
        Block::Heading { inline, .. } => inline.text.to_string(),
        Block::Paragraph(inline) => inline.text.to_string(),
        Block::Code(lines) | Block::Html(lines) => {
            lines.iter().map(|l| l.as_ref()).collect::<Vec<_>>().join("\n")
        }
        Block::Quote(blocks) => blocks.iter().map(block_plain).collect::<Vec<_>>().join(" "),
        Block::Rule => "—".to_string(),
        Block::List(items) => items
            .iter()
            .map(|(marker, blocks)| {
                let body = blocks.iter().map(block_plain).collect::<Vec<_>>().join(" ");
                format!("{marker} {body}")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Block::Table(rows) => rows
            .iter()
            .map(|(_, cells)| {
                cells.iter().map(|c| c.text.to_string()).collect::<Vec<_>>().join(" | ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/* ---------------- inline spans ---------------- */

#[derive(Clone, Copy, Default)]
struct InlineFlags {
    bold: bool,
    italic: bool,
    strike: bool,
    code: bool,
    link: bool,
}

impl InlineFlags {
    fn any(&self) -> bool {
        self.bold || self.italic || self.strike || self.code || self.link
    }
    fn style(&self) -> HighlightStyle {
        HighlightStyle {
            color: if self.code {
                Some(rgb(ACCENT_STRONG).into())
            } else if self.link {
                Some(rgb(ACCENT).into())
            } else {
                None
            },
            font_weight: self.bold.then_some(FontWeight::BOLD),
            font_style: self.italic.then_some(FontStyle::Italic),
            background_color: self.code.then(|| rgb(SURFACE_ALT).into()),
            underline: self.link.then(|| UnderlineStyle {
                thickness: px(1.),
                color: Some(rgb(ACCENT).into()),
                wavy: false,
            }),
            strikethrough: self.strike.then(|| StrikethroughStyle {
                thickness: px(1.),
                color: Some(rgb(MUTED).into()),
            }),
            ..Default::default()
        }
    }
}

fn collect_inline<'a>(
    node: &'a AstNode<'a>,
    flags: InlineFlags,
    out: &mut String,
    runs: &mut Vec<(Range<usize>, HighlightStyle)>,
) {
    let mut push = |s: &str, f: InlineFlags| {
        let start = out.len();
        out.push_str(s);
        if f.any() {
            runs.push((start..out.len(), f.style()));
        }
    };
    match &node.data.borrow().value {
        NodeValue::Text(t) => push(t, flags),
        NodeValue::Code(c) => push(&c.literal, InlineFlags { code: true, ..flags }),
        NodeValue::SoftBreak | NodeValue::LineBreak => push(" ", flags),
        NodeValue::HtmlInline(h) => push(h, flags),
        NodeValue::Strong => {
            for c in node.children() {
                collect_inline(c, InlineFlags { bold: true, ..flags }, out, runs);
            }
        }
        NodeValue::Emph => {
            for c in node.children() {
                collect_inline(c, InlineFlags { italic: true, ..flags }, out, runs);
            }
        }
        NodeValue::Strikethrough => {
            for c in node.children() {
                collect_inline(c, InlineFlags { strike: true, ..flags }, out, runs);
            }
        }
        NodeValue::Link(_) | NodeValue::Image(_) => {
            for c in node.children() {
                collect_inline(c, InlineFlags { link: true, ..flags }, out, runs);
            }
        }
        _ => {
            for c in node.children() {
                collect_inline(c, flags, out, runs);
            }
        }
    }
}

fn inline_of<'a>(node: &'a AstNode<'a>) -> Inline {
    let mut text = String::new();
    let mut runs = Vec::new();
    for c in node.children() {
        collect_inline(c, InlineFlags::default(), &mut text, &mut runs);
    }
    if text.is_empty() {
        text.push(' ');
    }
    Inline { text: text.into(), runs }
}

fn lines_of(literal: &str) -> Vec<SharedString> {
    literal
        .trim_end_matches('\n')
        .split('\n')
        .map(|l| SharedString::from(if l.is_empty() { " ".to_string() } else { l.to_string() }))
        .collect()
}

/* ---------------- AST → Block ---------------- */

fn to_block<'a>(node: &'a AstNode<'a>) -> Option<Block> {
    Some(match &node.data.borrow().value {
        NodeValue::Heading(h) => Block::Heading { level: h.level, inline: inline_of(node) },
        NodeValue::Paragraph => Block::Paragraph(inline_of(node)),
        NodeValue::CodeBlock(cb) => Block::Code(lines_of(&cb.literal)),
        NodeValue::BlockQuote => Block::Quote(node.children().filter_map(to_block).collect()),
        NodeValue::ThematicBreak => Block::Rule,
        NodeValue::HtmlBlock(hb) => Block::Html(lines_of(&hb.literal)),
        NodeValue::Table(_) => Block::Table(
            node.children()
                .map(|row| {
                    let header = matches!(&row.data.borrow().value, NodeValue::TableRow(true));
                    (header, row.children().map(inline_of).collect())
                })
                .collect(),
        ),
        NodeValue::List(l) => {
            let ordered = l.list_type == ListType::Ordered;
            let mut n = l.start;
            Block::List(
                node.children()
                    .map(|item| {
                        let marker = item_marker(item, ordered, &mut n);
                        (SharedString::from(marker), item.children().filter_map(to_block).collect())
                    })
                    .collect(),
            )
        }
        _ => Block::Paragraph(inline_of(node)),
    })
}

fn item_marker<'a>(item: &'a AstNode<'a>, ordered: bool, n: &mut usize) -> String {
    if let NodeValue::TaskItem(t) = &item.data.borrow().value {
        return if t.symbol.is_some() { "☑".into() } else { "☐".into() };
    }
    if ordered {
        let m = format!("{n}.");
        *n += 1;
        m
    } else {
        "•".into()
    }
}

/* ================= Block → GPUI elements (per frame) ================= */

pub fn document(blocks: &[Block]) -> AnyElement {
    div()
        .flex()
        .flex_col()
        .gap_2()
        .text_color(rgb(TEXT))
        .children(blocks.iter().map(|b| element(b)))
        .into_any_element()
}

/// Render a single block — used by comment mode to wrap each block in its own
/// clickable, commentable container.
pub fn block_element(block: &Block) -> AnyElement {
    element(block)
}

/// For a paragraph, its text + inline style runs — so comment mode can rebuild
/// the `StyledText` itself (capturing its layout for drag-selection and merging
/// in comment-span highlights). `None` for non-paragraph blocks, which stay
/// whole-block-commentable via `block_element`.
pub fn paragraph_text(block: &Block) -> Option<(SharedString, Vec<(Range<usize>, HighlightStyle)>)> {
    match block {
        Block::Paragraph(inline) => Some((inline.text.clone(), inline.runs.clone())),
        _ => None,
    }
}

fn styled(inline: &Inline) -> AnyElement {
    StyledText::new(inline.text.clone())
        .with_highlights(inline.runs.iter().cloned())
        .into_any_element()
}

fn element(block: &Block) -> AnyElement {
    match block {
        Block::Heading { level, inline } => {
            let (size, top_pad) = match level {
                1 => (px(24.), px(10.)),
                2 => (px(20.), px(8.)),
                3 => (px(17.), px(6.)),
                _ => (px(15.), px(4.)),
            };
            let el = div()
                .pt(top_pad)
                .text_size(size)
                .font_weight(FontWeight::BOLD)
                .text_color(rgb(ACCENT_STRONG))
                .child(styled(inline));
            if *level <= 2 {
                el.pb_1().border_b_1().border_color(rgb(FAINT)).into_any_element()
            } else {
                el.into_any_element()
            }
        }
        Block::Paragraph(inline) => div().child(styled(inline)).into_any_element(),
        Block::Quote(blocks) => div()
            .border_l_2()
            .border_color(rgb(ACCENT))
            .pl_3()
            .py_1()
            .bg(rgb(SURFACE))
            .text_color(rgb(MUTED))
            .flex()
            .flex_col()
            .gap_2()
            .children(blocks.iter().map(element))
            .into_any_element(),
        Block::Code(lines) => div()
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(FAINT))
            .rounded_md()
            .p_3()
            .my_1()
            .flex()
            .flex_col()
            .children(lines.iter().map(|l| div().child(l.clone())))
            .into_any_element(),
        Block::Rule => div().h(px(1.)).my_2().bg(rgb(FAINT)).into_any_element(),
        Block::Html(lines) => div()
            .text_color(rgb(MUTED))
            .flex()
            .flex_col()
            .children(lines.iter().map(|l| div().child(l.clone())))
            .into_any_element(),
        Block::List(items) => div()
            .flex()
            .flex_col()
            .gap_1()
            .pl(px(4.))
            .children(items.iter().map(|(marker, blocks)| {
                div()
                    .flex()
                    .flex_row()
                    .items_start()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .min_w(px(18.))
                            .text_color(rgb(ACCENT))
                            .child(marker.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .children(blocks.iter().map(element)),
                    )
            }))
            .into_any_element(),
        Block::Table(rows) => div()
            .my_1()
            .border_1()
            .border_color(rgb(FAINT))
            .rounded_md()
            .flex()
            .flex_col()
            .children(rows.iter().map(|(header, cells)| {
                let mut r = div().flex().flex_row().border_b_1().border_color(rgb(FAINT));
                if *header {
                    r = r.bg(rgb(SURFACE)).font_weight(FontWeight::BOLD).text_color(rgb(ACCENT_STRONG));
                }
                r.children(cells.iter().map(|cell| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .px_2()
                        .py_1()
                        .border_r_1()
                        .border_color(rgb(FAINT))
                        .child(styled(cell))
                }))
            }))
            .into_any_element(),
    }
}
