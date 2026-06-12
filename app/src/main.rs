//! markdown-delight viewer — open a .md, render it natively (G0a + G0d + CRT-lite).
//!
//! comrak parses CommonMark+GFM ONCE into owned blocks (render.rs); every
//! frame builds elements from that cache — which is what lets the CRT
//! tracking band animate at frame rate without re-parsing. The look mirrors
//! the browser reference: phosphor tube in a glossy purple complement shell,
//! line-counter rail fused to the top bar, scanlines, vignette lit from the
//! top-left, glass glint, and the slow rolling tracking band. All primitives
//! (Track 1 of the visuals plan) — no shaders, no webview.
//!
//!   cargo run                 # shows a built-in sample
//!   cargo run -- README.md    # renders that file

mod render;

use std::{env, fs, time::Duration};

use gpui::{
    Animation, AnimationExt, App, Bounds, Context, FocusHandle, Focusable, SharedString,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, linear_color_stop,
    linear_gradient, prelude::*, px, relative, rgb, rgba, size,
};
use gpui_platform::application;

// hacker palette tokens (src/styles/theme.css)
const BG: u32 = 0x050706;
const TEXT: u32 = 0x86efac;
const ACCENT: u32 = 0x22c55e;

// the complement shell — glossy purple housing (src/styles/theme.css --frame-*)
const FRAME_BG: u32 = 0x1b1026;
const FRAME_TEXT: u32 = 0xcdb8dc;
const FRAME_BORDER: u32 = 0xa86fd2;
const FRAME_FAINT: u32 = 0xb888e5;

const SAMPLE: &str = "\
# markdown-delight

**Rendered natively** — comrak AST → GPUI elements, *no webview*.

- [x] open any `.md` via right-click / double-click
- [x] CRT-lite: scanlines · vignette · glint · tracking band
- [ ] editor core (G0b) — next

    cargo run -- README.md
";

struct MdView {
    focus_handle: FocusHandle,
    path_label: SharedString,
    blocks: Vec<render::Block>,
}

impl MdView {
    fn new(path_label: String, text: String, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            path_label: path_label.into(),
            blocks: render::parse(&text),
        }
    }
}

impl Focusable for MdView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/* ---------------- CRT overlay layers (pane-local, like the web tubes) ---------------- */

fn scanlines() -> impl IntoElement {
    // 1px dark line every 4px — covers up to 1600px of screen, clipped by the pane
    div()
        .absolute()
        .inset_0()
        .overflow_hidden()
        .children((0..400).map(|i| {
            div()
                .absolute()
                .top(px(i as f32 * 4.))
                .left_0()
                .right_0()
                .h(px(1.))
                .bg(rgba(0x00000052))
        }))
}

fn vignette() -> impl IntoElement {
    // curved-glass falloff via edge strips; corners darken where strips overlap.
    // The light sits top-left, so the bottom and right fall off hardest.
    div()
        .absolute()
        .inset_0()
        .child(
            div().absolute().top_0().left_0().right_0().h(px(70.)).bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0x00000038), 1.),
                linear_color_stop(rgba(0x00000000), 0.),
            )),
        )
        .child(
            div().absolute().bottom_0().left_0().right_0().h(px(150.)).bg(linear_gradient(
                0.,
                linear_color_stop(rgba(0x00000075), 1.),
                linear_color_stop(rgba(0x00000000), 0.),
            )),
        )
        .child(
            div().absolute().top_0().bottom_0().left_0().w(px(90.)).bg(linear_gradient(
                90.,
                linear_color_stop(rgba(0x00000042), 1.),
                linear_color_stop(rgba(0x00000000), 0.),
            )),
        )
        .child(
            div().absolute().top_0().bottom_0().right_0().w(px(130.)).bg(linear_gradient(
                270.,
                linear_color_stop(rgba(0x00000066), 1.),
                linear_color_stop(rgba(0x00000000), 0.),
            )),
        )
}

fn glint() -> impl IntoElement {
    // the glass catching the top-left light: a soft elongated smear
    div()
        .absolute()
        .top(relative(0.06))
        .left(relative(0.05))
        .w(relative(0.34))
        .h(px(64.))
        .rounded_full()
        .bg(linear_gradient(
            135.,
            linear_color_stop(rgba(0xffffff12), 0.),
            linear_color_stop(rgba(0xffffff00), 1.),
        ))
}

fn tracking_band() -> impl IntoElement {
    // the slow rolling refresh bar — phosphor wash with a faint white core
    div()
        .absolute()
        .left_0()
        .right_0()
        .h(px(140.))
        .flex()
        .flex_col()
        .child(div().h(px(60.)).w_full().bg(linear_gradient(
            180.,
            linear_color_stop(rgba(0x22c55e17), 1.),
            linear_color_stop(rgba(0x22c55e00), 0.),
        )))
        .child(div().h(px(20.)).w_full().bg(rgba(0xd8ffe60d)))
        .child(div().h(px(60.)).w_full().bg(linear_gradient(
            0.,
            linear_color_stop(rgba(0x22c55e14), 1.),
            linear_color_stop(rgba(0x22c55e00), 0.),
        )))
        .with_animation(
            "crt-tracking",
            Animation::new(Duration::from_secs(7)).repeat(),
            |band, delta| band.top(px(-160. + delta * 1800.)),
        )
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
                // the shell bar — glossy purple housing, lit from the top
                div()
                    .flex()
                    .flex_row()
                    .justify_between()
                    .px_3()
                    .py_1()
                    .bg(linear_gradient(
                        180.,
                        linear_color_stop(rgba(0x2c1c3e_ff), 0.),
                        linear_color_stop(rgba(0x1b1026_ff), 1.),
                    ))
                    .text_color(rgb(FRAME_TEXT))
                    .child(SharedString::from(format!("▸ {}", self.path_label)))
                    .child(div().text_color(rgb(ACCENT)).child("markdown-delight")),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_row()
                    .min_h_0()
                    .child(
                        // the line-counter rail — fused to the bar above (same shell,
                        // no seam): light continues the bar's falloff then settles
                        div()
                            .flex_none()
                            .w(px(44.))
                            .h_full()
                            .overflow_hidden()
                            .bg(linear_gradient(
                                180.,
                                linear_color_stop(rgba(0x1b1026_ff), 0.),
                                linear_color_stop(rgba(0x130a1b_ff), 1.),
                            ))
                            .border_r_1()
                            .border_color(rgba(0xa86fd2_4d))
                            .flex()
                            .flex_col()
                            .pt_2()
                            .children((1..=99).map(|i| {
                                div()
                                    .h(px(21.))
                                    .pr_2()
                                    .text_size(px(11.5))
                                    .text_color(rgba(0xb888e5_6e))
                                    .flex()
                                    .justify_end()
                                    .child(SharedString::from(format!("{i}")))
                            })),
                    )
                    .child(
                        // THE SCREEN — content + the tube's glass layers
                        div()
                            .flex_1()
                            .min_w_0()
                            .relative()
                            .overflow_hidden()
                            .child(
                                div()
                                    .id("doc")
                                    .size_full()
                                    .overflow_y_scroll()
                                    .overflow_x_hidden()
                                    .px_6()
                                    .py_4()
                                    .child(render::document(&self.blocks)),
                            )
                            .child(scanlines())
                            .child(vignette())
                            .child(glint())
                            .child(tracking_band()),
                    ),
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
        let bounds = Bounds::centered(None, size(px(1060.), px(720.)), cx);
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
