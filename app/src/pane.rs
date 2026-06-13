//! pane.rs — one sub-monitor: a view onto a shared Doc (source or preview).
//!
//! Panes in a tab share ONE Doc entity: edit in a source pane and every
//! preview pane of the same document re-renders live (cx.observe). Each pane
//! keeps its own mode, focus, and crt::Fx — its own desynced tube. Default
//! mode for the first pane is SOURCE: right-click → open → start typing.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use gpui::{
    AnyElement, Bounds, Context, Entity, FocusHandle, Focusable, HighlightStyle,
    KeyDownEvent, MouseButton, MouseDownEvent, Pixels, ScrollHandle, SharedString, StyledText,
    Window, canvas, div, linear_color_stop, linear_gradient, point, prelude::*, px,
};

use crate::{crt, editor, render, theme, warp};

const LINE_H: f32 = 21.;
const PAD_X: f32 = 16.;
const PAD_Y: f32 = 12.;
/// JetBrains Mono advance width at 13px — good enough for click→column
const CHAR_W: f32 = 7.8;

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
    /// Optional per-pane theme override (name into theme::ThemeRegistry).
    /// None = follow the global active theme. New/split panes inherit this from
    /// their origin pane; dragged panes carry it with them.
    pub theme: Option<String>,
    /// Optional per-pane SEED colour — folds this pane's tube onto one hue,
    /// layered on top of `theme`. None = the (effective) theme's own colours.
    pub seed: Option<gpui::Hsla>,
    focus_handle: FocusHandle,
    fx: crt::Fx,
    scroll: ScrollHandle,
    tube_bounds: Arc<Mutex<Option<Bounds<Pixels>>>>,
    doc_sub: gpui::Subscription,
}

impl MdPane {
    pub fn new(
        doc: Entity<Doc>,
        mode: Mode,
        seed: u64,
        theme: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        // live preview: repaint when the shared doc changes
        let doc_sub = cx.observe(&doc, |_, _, cx| cx.notify());
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
                        let th = pane.effective_theme(cx);
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
            theme,
            seed: None,
            focus_handle: cx.focus_handle(),
            fx: crt::Fx::new(seed),
            scroll: ScrollHandle::new(),
            tube_bounds: Arc::new(Mutex::new(None)),
            doc_sub,
        }
    }

    /// The theme this pane renders with: its override (else the global active
    /// theme), with its optional seed hue folded on top.
    pub fn effective_theme(&self, cx: &gpui::App) -> Arc<theme::Theme> {
        let base = theme::resolve(cx, self.theme.as_deref());
        match self.seed {
            Some(seed) => Arc::new(theme::recolor(&base, seed)),
            None => base,
        }
    }

    pub fn is_editing(&self) -> bool {
        self.mode == Mode::Source
    }

    /// The pane's effective theme name (for the header chip).
    pub fn theme_name(&self, cx: &gpui::App) -> SharedString {
        self.effective_theme(cx).name.clone().into()
    }

    /// Header status word: editing / modified / live.
    pub fn status_str(&self, cx: &gpui::App) -> &'static str {
        let dirty = self.doc.read(cx).editor.dirty;
        match (self.is_editing(), dirty) {
            (true, true) => "editing · ● modified",
            (true, false) => "editing",
            (false, true) => "● modified",
            (false, false) => "live",
        }
    }

    /// Point this pane at a different Doc (the finder's "open in THIS tab").
    /// Re-subscribes the live-preview observer and rewinds the tube.
    pub fn set_doc(&mut self, doc: Entity<Doc>, cx: &mut Context<Self>) {
        self.doc_sub = cx.observe(&doc, |_, _, cx| cx.notify());
        self.doc = doc;
        self.scroll.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Keep the cursor row inside the visible window after edits — without
    /// this, typing while scrolled away LOOKS like a frozen app.
    fn follow_cursor(&self, cx: &gpui::App) {
        let Some(b) = *self.tube_bounds.lock().unwrap() else {
            return;
        };
        let h = f32::from(b.size.height);
        let line_h = LINE_H * theme::font_scale();
        let (line, _) = self.doc.read(cx).editor.line_col();
        let cursor_y = PAD_Y + line as f32 * line_h;
        let mut off = self.scroll.offset();
        let visible_y = cursor_y + f32::from(off.y);
        if visible_y < line_h {
            off.y = px(-(cursor_y - line_h * 2.).max(0.));
            self.scroll.set_offset(off);
        } else if visible_y > h - line_h * 2. {
            off.y = px(-(cursor_y - h + line_h * 3.));
            self.scroll.set_offset(off);
        }
    }

    /// Click → cursor: map a tube-space click to (line, col).
    fn place_cursor(&mut self, pos: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        let Some(b) = *self.tube_bounds.lock().unwrap() else {
            return;
        };
        let off = self.scroll.offset();
        let sc = theme::font_scale();
        // Follow the glass: a screen point inside a bent tube DISPLAYS content
        // sampled from warped(point), so a click must be pushed through the same
        // barrel curve the shader uses or the cursor lands off-target near the
        // edges. (k matches the per-tube dial in render / theme::apply_warp.)
        let mut sx = f32::from(pos.x) - f32::from(b.origin.x);
        let mut sy = f32::from(pos.y) - f32::from(b.origin.y);
        let th = self.effective_theme(cx);
        let (k1, k2) = (th.curvature * 0.14, th.curvature * 0.06);
        if k1.abs() > 0.0005 || k2.abs() > 0.0005 {
            let bw = f32::from(b.size.width).max(1.);
            let bh = f32::from(b.size.height).max(1.);
            let cu = sx / bw - 0.5;
            let cv = sy / bh - 0.5;
            let r2 = cu * cu + cv * cv;
            let f = 1.0 + k1 * r2 + k2 * r2 * r2;
            sx = (0.5 + cu * f) * bw;
            sy = (0.5 + cv * f) * bh;
        }
        let y = sy - PAD_Y - f32::from(off.y);
        let x = sx - PAD_X - f32::from(off.x);
        self.doc.update(cx, |doc, cx| {
            let e = &mut doc.editor;
            let line = ((y / (LINE_H * sc)).floor().max(0.) as usize).min(e.line_count() - 1);
            let col = ((x / (CHAR_W * sc)).round().max(0.) as usize).min(e.line(line).chars().count());
            e.set_cursor(line, col);
            cx.notify();
        });
        cx.notify();
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
            // ctrl+e toggles mode here; ctrl+s (save / save-as) is handled at the
            // Workspace level so it can name a new notebook + rename its tab.
            if ks.key.as_str() == "e" {
                self.mode = match self.mode {
                    Mode::Preview => Mode::Source,
                    Mode::Source => Mode::Preview,
                };
                cx.notify();
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
            self.follow_cursor(cx);
            cx.notify();
        }
    }

    /// The source tube: every buffer line, block cursor on the active one.
    fn source_lines(&self, th: &theme::Theme, cx: &gpui::App) -> Vec<AnyElement> {
        let doc = self.doc.read(cx);
        let (cur_line, cur_col) = doc.editor.line_col();
        let focused = true; // cursor always drawn; dim later if needed
        let line_h = px(LINE_H * theme::font_scale());
        (0..doc.editor.line_count())
            .map(|i| {
                let mut text = doc.editor.line(i);
                let line = div().h(line_h).whitespace_nowrap();
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

impl Render for MdPane {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = self.effective_theme(cx);
        let sc = theme::font_scale();
        let doc = self.doc.read(cx);
        let editing = self.mode == Mode::Source;
        let line_count = doc.editor.line_count();
        let rail_count = if editing { line_count } else { 99 };
        let jiggle = self.fx.jiggle_px;

        // NOTE: the pane header (drag handle + theme chip) is rendered by
        // render_node in main.rs, which has the Workspace context the drag needs.
        // rail numbers ride the tube's scroll offset so they stay aligned
        let rail_offset = if editing {
            f32::from(self.scroll.offset().y)
        } else {
            0.
        };
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
            .child(
                div()
                    .mt(px(8. + rail_offset))
                    .flex()
                    .flex_col()
                    .children((1..=rail_count.max(1)).map(|i| {
                        div()
                            .h(px(21. * sc))
                            .pr_2()
                            .text_size(px(10.5 * sc))
                            .text_color(th.frame_faint.alpha(0.45))
                            .flex()
                            .justify_end()
                            .child(SharedString::from(format!("{i}")))
                    })),
            );

        let content: AnyElement = if editing {
            div()
                .id("src")
                .size_full()
                .overflow_y_scroll()
                .overflow_x_hidden()
                .track_scroll(&self.scroll)
                .px_4()
                .py_3()
                .text_size(px(13. * sc))
                .whitespace_nowrap()
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

        let tube_store = self.tube_bounds.clone();
        let tube = div()
            .flex_1()
            .min_w_0()
            .relative()
            .overflow_hidden()
            .bg(th.bg)
            .when(editing, |el| {
                el.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|pane, ev: &MouseDownEvent, window, cx| {
                        window.focus(&pane.focus_handle, cx);
                        pane.place_cursor(ev.position, cx);
                    }),
                )
            })
            .child(
                div().absolute().inset_0().child(
                    canvas(
                        // capture the Copy f32, not all of `th` (Arc) — `th` is
                        // reborrowed below for crt::glass()
                        {
                            let glare = th.screen_glare;
                            // per-tube barrel curvature: this pane bends on its
                            // OWN theme (see warp.rs). Same dial as theme::apply_warp.
                            let k1 = th.curvature * 0.14;
                            let k2 = th.curvature * 0.06;
                            move |bounds, window, _| {
                                *tube_store.lock().unwrap() = Some(bounds);
                                let sf = window.scale_factor();
                                warp::register_tube(
                                    [
                                        f32::from(bounds.origin.x) * sf,
                                        f32::from(bounds.origin.y) * sf,
                                        f32::from(bounds.size.width) * sf,
                                        f32::from(bounds.size.height) * sf,
                                    ],
                                    glare,
                                    k1,
                                    k2,
                                );
                            }
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
            // repaint on wheel so the rail tracks the tube's scroll offset
            .on_scroll_wheel(cx.listener(|_, _: &gpui::ScrollWheelEvent, _, cx| cx.notify()))
            .size_full()
            .flex()
            .flex_col()
            .font_family(th.font_family.clone())
            .text_size(px(th.font_size * sc))
            .text_color(th.text)
            .pt(px(jiggle.max(0.)))
            .pb(px((-jiggle).max(0.)))
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
