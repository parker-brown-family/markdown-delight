//! markdown-delight viewer — open a .md, render it natively (G0a + G0d).
//! comrak parses CommonMark+GFM; render.rs paints the AST as GPUI elements
//! in the hacker palette. Read-only; the editor core (rope + cursor) is G0b.
//!
//!   cargo run                 # shows a built-in sample
//!   cargo run -- README.md    # renders that file

mod render;

use std::{env, fs};

use gpui::{
    App, Bounds, Context, FocusHandle, Focusable, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions, div, prelude::*, px, rgb, size,
};
use gpui_platform::application;

// hacker palette tokens (from the browser reference, src/styles/theme.css)
const BG: u32 = 0x050706;
const SURFACE: u32 = 0x08100d;
const TEXT: u32 = 0x86efac;
const ACCENT: u32 = 0x22c55e;
const FAINT: u32 = 0x14401f;

const SAMPLE: &str = "\
# markdown-delight

**Rendered natively** — comrak AST → GPUI elements, *no webview*.

- [x] open any `.md` via right-click / double-click
- [ ] editor core (G0b) — next

    cargo run -- README.md
";

struct MdView {
    focus_handle: FocusHandle,
    path_label: SharedString,
    text: String,
}

impl MdView {
    fn new(path_label: String, text: String, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            path_label: path_label.into(),
            text,
        }
    }
}

impl Focusable for MdView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MdView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .bg(rgb(BG))
            .flex()
            .flex_col()
            .font_family("JetBrains Mono")
            .text_size(px(14.))
            .text_color(rgb(TEXT))
            .child(
                // pane header, echoing the chrome from the browser reference
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .bg(rgb(SURFACE))
                    .border_b_1()
                    .border_color(rgb(FAINT))
                    .text_color(rgb(ACCENT))
                    .child(SharedString::from(format!("▸ {}", self.path_label)))
                    .child("markdown-delight · g0a"),
            )
            .child(
                div()
                    .id("doc")
                    .flex_1()
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .px_6()
                    .py_4()
                    .child(render::markdown(&self.text)),
            )
    }
}

fn load() -> (String, String) {
    match env::args().nth(1) {
        Some(path) => match fs::read_to_string(&path) {
            Ok(text) => (path, text),
            Err(e) => (format!("{path} (error)"), format!("could not read {path}:\n{e}")),
        },
        None => ("sample.md".to_string(), SAMPLE.to_string()),
    }
}

fn main() {
    let (label, text) = load();
    application().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.), px(680.)), cx);
        let title: SharedString = format!("{label} — markdown-delight").into();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| MdView::new(label.clone(), text.clone(), cx));
                window.focus(&view.focus_handle(cx), cx);
                view
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}
