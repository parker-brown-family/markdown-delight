//! markdown-delight viewer — the monitor-wrap build (parity with
//! terminal-delight's native chrome): a MASTER monitor frame around the
//! workspace, the document screen in its own little sub-monitor frame,
//! canvas-painted scanlines + occasional tracking sweeps + flicker bursts +
//! vertical-hold jiggle (crt::Fx), real inset-shadow curved-glass vignette,
//! barrel warp via the shared td-crt-pass renderer patch, and a hot-reloaded
//! theme.toml (edit ~/.config/markdown-delight/theme.toml live).
//!
//! comrak parses ONCE into owned blocks (render.rs); frames rebuild elements
//! from the cache, so none of the animation re-parses the document.
//!
//!   cargo run                 # built-in sample
//!   cargo run -- README.md    # renders that file

mod crt;
mod editor;
mod render;
mod theme;
mod warp;

use std::{env, fs, path::PathBuf, time::Duration};

use gpui::{
    BoxShadow, Context, FocusHandle, Focusable, HighlightStyle, Hsla, KeyDownEvent, SharedString,
    StyledText, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, hsla,
    linear_color_stop, linear_gradient, point, prelude::*, px, size, white,
};
use gpui_platform::application;

const SAMPLE: &str = "\
# markdown-delight

**Rendered natively** — comrak AST → GPUI elements, *no webview*.

- [x] open any `.md` via right-click / double-click
- [x] CRT: scanlines · vignette · tracking · flicker · jiggle · barrel warp
- [x] master monitor frame + per-screen sub-frames
- [ ] editor core (G0b) — next

    cargo run -- README.md
";

fn darken(mut c: Hsla, f: f32) -> Hsla {
    c.l *= f;
    c
}
fn brighten(mut c: Hsla, f: f32) -> Hsla {
    c.l = (c.l * f).min(0.92);
    c
}

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Preview,
    Source,
}

struct MdView {
    focus_handle: FocusHandle,
    path_label: SharedString,
    path: Option<PathBuf>,
    blocks: Vec<render::Block>,
    editor: editor::Editor,
    mode: Mode,
    fx: crt::Fx,
}

impl MdView {
    fn new(path_label: String, path: Option<PathBuf>, text: String, cx: &mut Context<Self>) -> Self {
        // fx clock: cheap idle poll; only notifies when something visibly moved
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let _ = this.update(cx, |view: &mut MdView, cx| {
                    let th = theme::theme(cx);
                    if view.fx.tick(&th) {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        Self {
            focus_handle: cx.focus_handle(),
            path_label: path_label.into(),
            path,
            blocks: render::parse(&text),
            editor: editor::Editor::new(&text),
            mode: Mode::Preview,
            fx: crt::Fx::new(0xD0C5),
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        // global chords
        if ks.modifiers.control {
            match ks.key.as_str() {
                "e" => {
                    self.mode = match self.mode {
                        Mode::Preview => Mode::Source,
                        Mode::Source => {
                            // re-render the document from the edited buffer
                            self.blocks = render::parse(&self.editor.text());
                            Mode::Preview
                        }
                    };
                    cx.notify();
                }
                "s" => {
                    if let Some(path) = &self.path {
                        if let Err(e) = self.editor.save(path) {
                            eprintln!("save failed: {e}");
                        }
                        self.blocks = render::parse(&self.editor.text());
                        cx.notify();
                    }
                }
                _ => {}
            }
            return;
        }
        if self.mode != Mode::Source {
            return;
        }
        match ks.key.as_str() {
            "enter" => self.editor.insert("\n"),
            "backspace" => self.editor.backspace(),
            "delete" => self.editor.delete(),
            "left" => self.editor.left(),
            "right" => self.editor.right(),
            "up" => self.editor.up(),
            "down" => self.editor.down(),
            "home" => self.editor.home(),
            "end" => self.editor.end(),
            "tab" => self.editor.insert("  "),
            "space" => self.editor.insert(" "),
            _ => {
                if let Some(ch) = ks.key_char.as_ref() {
                    self.editor.insert(ch);
                } else {
                    return;
                }
            }
        }
        cx.notify();
    }

    /// The source tube: every buffer line, block cursor on the active one.
    fn source_lines(&self, th: &theme::Theme) -> Vec<gpui::AnyElement> {
        let (cur_line, cur_col) = self.editor.line_col();
        (0..self.editor.line_count())
            .map(|i| {
                let mut text = self.editor.line(i);
                let line = div().h(px(21.)).whitespace_nowrap();
                if i != cur_line {
                    return if text.is_empty() {
                        line.into_any_element()
                    } else {
                        line.child(SharedString::from(text)).into_any_element()
                    };
                }
                // cursor line: highlight the char under a block cursor
                let (start, end) = match text.char_indices().nth(cur_col) {
                    Some((b, c)) => (b, b + c.len_utf8()),
                    None => {
                        text.push(' '); // cursor sits past EOL
                        (text.len() - 1, text.len())
                    }
                };
                line.child(
                    StyledText::new(SharedString::from(text)).with_highlights([(
                        start..end,
                        HighlightStyle {
                            color: Some(th.bg),
                            background_color: Some(th.accent),
                            ..Default::default()
                        },
                    )]),
                )
                .into_any_element()
            })
            .collect()
    }
}

impl Focusable for MdView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for MdView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        warp::begin_frame(); // the tube re-registers its rect below
        let th = theme::theme(cx);
        let bezel = darken(th.frame_bg, 0.85);
        let jiggle = self.fx.jiggle_px;
        let block_count = self.blocks.len();
        let editing = self.mode == Mode::Source;
        let dirty = self.editor.dirty;
        let status = match (editing, dirty) {
            (true, true) => "editing · ● modified",
            (true, false) => "editing",
            (false, true) => "● modified",
            (false, false) => "live",
        };
        let rail_count = if editing { self.editor.line_count() } else { 99 };

        // ---- the sub-monitor: the document tube in its own little frame ----
        let pane_header = div()
            .h(px(26.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .bg(linear_gradient(
                180.,
                linear_color_stop(brighten(th.frame_bg, 1.9), 0.),
                linear_color_stop(th.frame_bg, 1.),
            ))
            .border_b_1()
            .border_color(th.frame_border.alpha(0.5))
            .text_size(px(11.5))
            .text_color(th.frame_text)
            .shadow(
                vec![BoxShadow {
                    color: white().alpha(0.16),
                    offset: point(px(1.), px(1.)),
                    blur_radius: px(0.),
                    spread_radius: px(0.),
                    inset: true,
                }]
                .into(),
            )
            .child(SharedString::from(format!("▸ {}", self.path_label)))
            .child(SharedString::from(format!(
                "{} blocks · {} · {}",
                block_count, th.name, status
            )));

        let rail = div()
            .flex_none()
            .w(px(40.))
            .h_full()
            .overflow_hidden()
            .bg(linear_gradient(
                180.,
                linear_color_stop(th.frame_bg, 0.),
                linear_color_stop(darken(th.frame_bg, 0.7), 1.),
            ))
            .border_r_1()
            .border_color(th.frame_border.alpha(0.3))
            .flex()
            .flex_col()
            .pt_2()
            .children((1..=rail_count.max(1)).map(|i| {
                div()
                    .h(px(21.))
                    .pr_2()
                    .text_size(px(11.))
                    .text_color(th.frame_faint.alpha(0.45))
                    .flex()
                    .justify_end()
                    .child(SharedString::from(format!("{i}")))
            }));

        let tube = div()
            .flex_1()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .bg(th.bg)
            .child(
                // register this rect for the renderer's barrel-warp pass
                div().absolute().inset_0().child(
                    canvas(
                        move |bounds, window, _| {
                            let sf = window.scale_factor();
                            warp::register([
                                f32::from(bounds.origin.x) * sf,
                                f32::from(bounds.origin.y) * sf,
                                f32::from(bounds.size.width) * sf,
                                f32::from(bounds.size.height) * sf,
                            ]);
                        },
                        |_, _, _, _| {},
                    )
                    .size_full(),
                ),
            )
            .child(if editing {
                div()
                    .id("src")
                    .size_full()
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .px_4()
                    .py_4()
                    .text_size(px(13.))
                    .flex()
                    .flex_col()
                    .children(self.source_lines(&th))
                    .into_any_element()
            } else {
                div()
                    .id("doc")
                    .size_full()
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .px_6()
                    .py_4()
                    .child(render::document(&self.blocks))
                    .into_any_element()
            })
            .child(crt::glass(&th, &self.fx));

        let sub_monitor = div()
            .size_full()
            .flex()
            .flex_col()
            .rounded_md()
            .overflow_hidden()
            .border_1()
            .border_color(th.frame_border.alpha(0.45))
            .child(pane_header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(rail)
                    .child(tube),
            );

        // ---- master monitor: bezel top strip, screen well, footer ----
        let bezel_top = div()
            .h(px(30.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .text_size(px(11.5))
            .text_color(th.frame_text)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_2()
                    .child(div().text_color(th.accent).child("▸ MARKDOWN-DELIGHT"))
                    .child(div().text_color(th.frame_faint.alpha(0.6)).child("// VIEWER")),
            )
            .child(SharedString::from(format!("{} · live", th.name)));

        let bezel_bottom = div()
            .h(px(24.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .text_size(px(10.5))
            .text_color(th.frame_faint.alpha(0.7))
            .child(SharedString::from(format!("{} · {}", th.name, self.path_label)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(SharedString::from(if editing {
                        "ctrl+e preview · ctrl+s save".to_string()
                    } else {
                        "ctrl+e edit".to_string()
                    }))
                    .child(div().text_color(th.accent).child(if dirty {
                        "● MODIFIED"
                    } else {
                        "● READY"
                    })),
            );

        let screen_well = div()
            .flex_1()
            .min_h_0()
            .relative()
            .rounded(px(10.))
            .overflow_hidden()
            .bg(darken(th.bg, 0.6))
            .border_1()
            .border_color(darken(th.frame_bg, 0.5))
            .mx_2()
            .child(div().size_full().flex().p(px(4.)).child(sub_monitor));

        div()
            .track_focus(&self.focus_handle(cx))
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .bg(darken(bezel, 0.5))
            .px(px(5.))
            .pt(px(5. + jiggle.max(0.)))
            .pb(px(5. + (-jiggle).max(0.)))
            .font_family(th.font_family.clone())
            .text_size(px(th.font_size))
            .text_color(th.text)
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .rounded(px(14.))
                    .bg(linear_gradient(
                        135.,
                        linear_color_stop(brighten(bezel, 1.6), 0.),
                        linear_color_stop(darken(bezel, 0.8), 1.),
                    ))
                    .border_2()
                    .border_color(th.frame_border.alpha(0.45))
                    .shadow(
                        vec![
                            // upper-left light source: glint biased to (1,1)
                            BoxShadow {
                                color: white().alpha(0.14),
                                offset: point(px(1.), px(1.)),
                                blur_radius: px(0.),
                                spread_radius: px(0.),
                                inset: true,
                            },
                            BoxShadow {
                                color: hsla(0., 0., 0., 0.5),
                                offset: point(px(-2.), px(-2.)),
                                blur_radius: px(3.),
                                spread_radius: px(0.),
                                inset: true,
                            },
                            BoxShadow {
                                color: th.accent.alpha(0.10 * th.glow),
                                offset: point(px(0.), px(0.)),
                                blur_radius: px(30.),
                                spread_radius: px(2.),
                                inset: false,
                            },
                        ]
                        .into(),
                    )
                    .child(bezel_top)
                    .child(screen_well)
                    .child(bezel_bottom),
            )
    }
}

fn load() -> (String, Option<PathBuf>, String) {
    match env::args().nth(1) {
        Some(path) => match fs::read_to_string(&path) {
            Ok(text) => (path.clone(), Some(PathBuf::from(path)), text),
            Err(e) => (
                format!("{path} (error)"),
                None,
                format!("could not read {path}:\n{e}"),
            ),
        },
        None => ("sample.md".to_string(), None, SAMPLE.to_string()),
    }
}

fn main() {
    let (label, path, text) = load();
    application().run(move |cx: &mut gpui::App| {
        theme::init(cx);
        let bounds = gpui::Bounds::centered(None, size(px(1100.), px(760.)), cx);
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
                // match the .desktop StartupWMClass so the dock shows OUR
                // CRT icon instead of the generic gear
                window.set_app_id("markdown-delight");
                let view =
                    cx.new(|cx| MdView::new(label.clone(), path.clone(), text.clone(), cx));
                window.focus(&view.focus_handle(cx), cx);
                view
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}
