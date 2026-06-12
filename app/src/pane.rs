//! pane.rs — one sub-monitor: a view onto a shared Doc (source or preview).
//!
//! Panes in a tab share ONE Doc entity: edit in a source pane and every
//! preview pane of the same document re-renders live (cx.observe). Each pane
//! keeps its own mode, focus, and crt::Fx — its own desynced tube. Default
//! mode for the first pane is SOURCE: right-click → open → start typing.

use std::{path::PathBuf, time::Duration};

use gpui::{
    AnyElement, BoxShadow, Context, Entity, FocusHandle, Focusable, HighlightStyle, KeyDownEvent,
    SharedString, StyledText, Window, canvas, div, linear_color_stop, linear_gradient, point,
    prelude::*, px, white,
};

use crate::{crt, editor, render, theme, warp};

/* ================= the shared document ================= */

pub struct Doc {
    pub editor: editor::Editor,
    pub blocks: Vec<render::Block>,
    pub path: Option<PathBuf>,
    pub label: SharedString,
}

impl Doc {
    pub fn new(label: String, path: Option<PathBuf>, text: String) -> Self {
        Self {
            editor: editor::Editor::new(&text),
            blocks: render::parse(&text),
            path,
            label: label.into(),
        }
    }

    pub fn reparse(&mut self) {
        self.blocks = render::parse(&self.editor.text());
    }
}

/* ================= the pane ================= */

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    Preview,
    Source,
}

pub struct MdPane {
    pub doc: Entity<Doc>,
    pub mode: Mode,
    pub closed: bool,
    focus_handle: FocusHandle,
    fx: crt::Fx,
}

impl MdPane {
    pub fn new(doc: Entity<Doc>, mode: Mode, seed: u64, cx: &mut Context<Self>) -> Self {
        // live preview: repaint when the shared doc changes
        cx.observe(&doc, |_, _, cx| cx.notify()).detach();
        // fx clock — only notifies when something visibly moved
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                if this
                    .update(cx, |pane: &mut MdPane, cx| {
                        if pane.closed {
                            return false;
                        }
                        let th = theme::theme(cx);
                        if pane.fx.tick(&th) {
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false)
                    == false
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            doc,
            mode,
            closed: false,
            focus_handle: cx.focus_handle(),
            fx: crt::Fx::new(seed),
        }
    }

    pub fn title(&self, cx: &gpui::App) -> SharedString {
        self.doc.read(cx).label.clone()
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        // workspace chords bubble up untouched
        if (m.control && m.alt) || (m.control && m.shift) || (m.alt && !m.control) {
            return;
        }
        if m.control {
            match ks.key.as_str() {
                "e" => {
                    self.mode = match self.mode {
                        Mode::Preview => Mode::Source,
                        Mode::Source => Mode::Preview,
                    };
                    cx.notify();
                }
                "s" => {
                    self.doc.update(cx, |doc, cx| {
                        if let Some(path) = doc.path.clone() {
                            if let Err(e) = doc.editor.save(&path) {
                                eprintln!("save failed: {e}");
                            }
                            doc.reparse();
                            cx.notify();
                        }
                    });
                }
                _ => {}
            }
            return;
        }
        if self.mode != Mode::Source {
            return;
        }
        let handled = self.doc.update(cx, |doc, cx| {
            let e = &mut doc.editor;
            match ks.key.as_str() {
                "enter" => e.insert("\n"),
                "backspace" => e.backspace(),
                "delete" => e.delete(),
                "left" => e.left(),
                "right" => e.right(),
                "up" => e.up(),
                "down" => e.down(),
                "home" => e.home(),
                "end" => e.end(),
                "tab" => e.insert("  "),
                "space" => e.insert(" "),
                _ => {
                    if let Some(ch) = ks.key_char.clone() {
                        e.insert(&ch);
                    } else {
                        return false;
                    }
                }
            }
            doc.reparse(); // live: preview panes of this doc follow every edit
            cx.notify();
            true
        });
        if handled {
            cx.notify();
        }
    }

    /// The source tube: every buffer line, block cursor on the active one.
    fn source_lines(&self, th: &theme::Theme, cx: &gpui::App) -> Vec<AnyElement> {
        let doc = self.doc.read(cx);
        let (cur_line, cur_col) = doc.editor.line_col();
        let focused = true; // cursor always drawn; dim later if needed
        (0..doc.editor.line_count())
            .map(|i| {
                let mut text = doc.editor.line(i);
                let line = div().h(px(21.)).whitespace_nowrap();
                if i != cur_line || !focused {
                    return if text.is_empty() {
                        line.into_any_element()
                    } else {
                        line.child(SharedString::from(text)).into_any_element()
                    };
                }
                let (start, end) = match text.char_indices().nth(cur_col) {
                    Some((b, c)) => (b, b + c.len_utf8()),
                    None => {
                        text.push(' ');
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

impl Focusable for MdPane {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn darken(mut c: gpui::Hsla, f: f32) -> gpui::Hsla {
    c.l *= f;
    c
}
fn brighten(mut c: gpui::Hsla, f: f32) -> gpui::Hsla {
    c.l = (c.l * f).min(0.92);
    c
}

impl Render for MdPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = theme::theme(cx);
        let doc = self.doc.read(cx);
        let editing = self.mode == Mode::Source;
        let dirty = doc.editor.dirty;
        let label = doc.label.clone();
        let block_count = doc.blocks.len();
        let line_count = doc.editor.line_count();
        let status = match (editing, dirty) {
            (true, true) => "editing · ● modified",
            (true, false) => "editing",
            (false, true) => "● modified",
            (false, false) => "live",
        };
        let rail_count = if editing { line_count } else { 99 };
        let jiggle = self.fx.jiggle_px;

        let header = div()
            .h(px(24.))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_2()
            .bg(linear_gradient(
                180.,
                linear_color_stop(brighten(th.frame_bg, 1.9), 0.),
                linear_color_stop(th.frame_bg, 1.),
            ))
            .border_b_1()
            .border_color(th.frame_border.alpha(0.5))
            .text_size(px(11.))
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
            .child(SharedString::from(format!(
                "▸ {} · {}",
                if editing { "SRC" } else { "DOC" },
                label
            )))
            .child(SharedString::from(format!("{} · {}", th.name, status)));

        let rail = div()
            .flex_none()
            .w(px(38.))
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
                    .text_size(px(10.5))
                    .text_color(th.frame_faint.alpha(0.45))
                    .flex()
                    .justify_end()
                    .child(SharedString::from(format!("{i}")))
            }));

        let content: AnyElement = if editing {
            div()
                .id("src")
                .size_full()
                .overflow_y_scroll()
                .overflow_x_hidden()
                .px_4()
                .py_3()
                .text_size(px(13.))
                .flex()
                .flex_col()
                .children(self.source_lines(&th, cx))
                .into_any_element()
        } else {
            div()
                .id("doc")
                .size_full()
                .overflow_y_scroll()
                .overflow_x_hidden()
                .px_5()
                .py_3()
                .child(render::document(&doc.blocks))
                .into_any_element()
        };

        let tube = div()
            .flex_1()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .bg(th.bg)
            .child(
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
            .child(content)
            .child(crt::glass(&th, &self.fx));

        div()
            .track_focus(&self.focus_handle(cx))
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .font_family(th.font_family.clone())
            .text_size(px(th.font_size))
            .text_color(th.text)
            .pt(px(jiggle.max(0.)))
            .pb(px((-jiggle).max(0.)))
            .child(header)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_row()
                    .child(rail)
                    .child(tube),
            )
    }
}
