//! render.rs — comrak AST → native GPUI elements (G0d pulled forward).
//!
//! The product renderer per docs/PLAN.md §2 D2: parse CommonMark+GFM with
//! comrak (BSD-2), walk the AST, and paint blocks as GPUI divs and inline
//! spans as StyledText highlight runs. NO webview, no HTML — the preview is
//! native elements wearing the hacker palette, mirroring src/styles/
//! workspace.css `.preview` in the browser reference.

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

/// Parse `text` and build the document's element tree.
pub fn markdown(text: &str) -> AnyElement {
    let arena = Arena::new();
    let mut options = Options::default();
    options.extension.table = true;
    options.extension.strikethrough = true;
    options.extension.tasklist = true;
    options.extension.autolink = true;
    let root = parse_document(&arena, text, &options);

    div()
        .flex()
        .flex_col()
        .gap_2()
        .text_color(rgb(TEXT))
        .children(root.children().map(|n| block(n, 0)))
        .into_any_element()
}

/* ---------------- inline spans → one StyledText with highlight runs ---------------- */

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

fn inline_element<'a>(node: &'a AstNode<'a>) -> AnyElement {
    let mut text = String::new();
    let mut runs = Vec::new();
    for c in node.children() {
        collect_inline(c, InlineFlags::default(), &mut text, &mut runs);
    }
    if text.is_empty() {
        text.push(' ');
    }
    StyledText::new(SharedString::from(text))
        .with_highlights(runs)
        .into_any_element()
}

/* ---------------- blocks ---------------- */

fn block<'a>(node: &'a AstNode<'a>, depth: usize) -> AnyElement {
    match &node.data.borrow().value {
        NodeValue::Heading(h) => {
            let (size, top_pad) = match h.level {
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
                .child(inline_element(node));
            if h.level <= 2 {
                el.pb_1().border_b_1().border_color(rgb(FAINT)).into_any_element()
            } else {
                el.into_any_element()
            }
        }
        NodeValue::Paragraph => div().child(inline_element(node)).into_any_element(),
        NodeValue::BlockQuote => div()
            .border_l_2()
            .border_color(rgb(ACCENT))
            .pl_3()
            .py_1()
            .bg(rgb(SURFACE))
            .text_color(rgb(MUTED))
            .flex()
            .flex_col()
            .gap_2()
            .children(node.children().map(|c| block(c, depth)))
            .into_any_element(),
        NodeValue::CodeBlock(cb) => div()
            .bg(rgb(SURFACE))
            .border_1()
            .border_color(rgb(FAINT))
            .rounded_md()
            .p_3()
            .my_1()
            .flex()
            .flex_col()
            .children(cb.literal.trim_end_matches('\n').split('\n').map(|l| {
                let l = if l.is_empty() { " " } else { l };
                div().child(SharedString::from(l.to_string()))
            }))
            .into_any_element(),
        NodeValue::ThematicBreak => div()
            .h(px(1.))
            .my_2()
            .bg(rgb(FAINT))
            .into_any_element(),
        NodeValue::List(l) => {
            let ordered = l.list_type == ListType::Ordered;
            let mut n = l.start;
            div()
                .flex()
                .flex_col()
                .gap_1()
                .pl(px(if depth == 0 { 4. } else { 18. }))
                .children(node.children().map(|item| {
                    let marker = item_marker(item, ordered, &mut n);
                    list_item(item, marker, depth)
                }))
                .into_any_element()
        }
        // top-level fallthrough: front matter, raw HTML blocks, tables, etc.
        NodeValue::Table(_) => table(node),
        NodeValue::HtmlBlock(hb) => div()
            .text_color(rgb(MUTED))
            .flex()
            .flex_col()
            .children(hb.literal.trim_end_matches('\n').split('\n').map(|l| {
                div().child(SharedString::from(l.to_string()))
            }))
            .into_any_element(),
        _ => div().child(inline_element(node)).into_any_element(),
    }
}

fn item_marker<'a>(item: &'a AstNode<'a>, ordered: bool, n: &mut usize) -> String {
    let value = &item.data.borrow().value;
    if let NodeValue::TaskItem(t) = value {
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

fn list_item<'a>(item: &'a AstNode<'a>, marker: String, depth: usize) -> AnyElement {
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
                .child(SharedString::from(marker)),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .children(item.children().map(|c| block(c, depth + 1))),
        )
        .into_any_element()
}

fn table<'a>(node: &'a AstNode<'a>) -> AnyElement {
    div()
        .my_1()
        .border_1()
        .border_color(rgb(FAINT))
        .rounded_md()
        .flex()
        .flex_col()
        .children(node.children().map(|row| {
            let header = matches!(&row.data.borrow().value, NodeValue::TableRow(true));
            let mut r = div().flex().flex_row().border_b_1().border_color(rgb(FAINT));
            if header {
                r = r.bg(rgb(SURFACE)).font_weight(FontWeight::BOLD).text_color(rgb(ACCENT_STRONG));
            }
            r.children(row.children().map(|cell| {
                div()
                    .flex_1()
                    .min_w_0()
                    .px_2()
                    .py_1()
                    .border_r_1()
                    .border_color(rgb(FAINT))
                    .child(inline_element(cell))
            }))
        }))
        .into_any_element()
}
