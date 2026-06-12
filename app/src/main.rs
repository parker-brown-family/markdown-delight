//! markdown-delight — tabs · tiling splits · monitor-wrap chrome, ported from
//! terminal-delight's proven workspace (the sibling, MIT).
//!
//! Splits divide ONLY the focused pane's space (true tiling tree). All panes
//! in a tab share ONE document: split a source pane and the new pane opens as
//! a LIVE PREVIEW of the same buffer — it re-renders as you type.
//!
//! ctrl+shift+t / [+]: new tab · ctrl+pgup/pgdn: switch tab · right-click
//! tab: rename · ctrl+alt+r / [▥]: split right · ctrl+alt+d / [▤]: split
//! down · alt+arrows: pane focus · ctrl+w: close pane · ctrl+e: source ↔
//! preview · ctrl+s: save. Opens in SOURCE mode: right-click → open → type.

mod crt;
mod editor;
mod pane;
mod render;
mod theme;
mod warp;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs, path::PathBuf};

use gpui::{
    App, Bounds, BoxShadow, Context, Entity, EntityId, Focusable, Hsla, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString, TitlebarOptions, Window,
    WindowBounds, WindowOptions, canvas, div, hsla, linear_color_stop, linear_gradient, point,
    prelude::*, px, size, white,
};
use gpui_platform::application;
use pane::{Doc, MdPane, Mode};

const MAX_PANES: usize = 8;

const SAMPLE: &str = "\
# markdown-delight

**Rendered natively** — comrak AST → GPUI elements, *no webview*.

- [x] tabs · tiling splits · shared live document
- [x] CRT: scanlines · vignette · tracking · flicker · jiggle · barrel warp
- [ ] selections · undo · find — next

    cargo run -- README.md
";

#[derive(Clone, Copy, PartialEq)]
enum SplitDir {
    Row,
    Col,
}

static SPLIT_IDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
fn next_split_id() -> u64 {
    SPLIT_IDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// The tiling tree: splits divide only the targeted leaf.
enum Node {
    Leaf(Entity<MdPane>),
    Split {
        id: u64,
        dir: SplitDir,
        ratio: f32,
        a: Box<Node>,
        b: Box<Node>,
    },
}

impl Node {
    fn leaves<'a>(&'a self, out: &mut Vec<&'a Entity<MdPane>>) {
        match self {
            Node::Leaf(e) => out.push(e),
            Node::Split { a, b, .. } => {
                a.leaves(out);
                b.leaves(out);
            }
        }
    }

    fn split_leaf(&mut self, target: EntityId, dir: SplitDir, new: Entity<MdPane>) -> bool {
        match self {
            Node::Leaf(e) if e.entity_id() == target => {
                let old = std::mem::replace(self, Node::Leaf(new.clone()));
                *self = Node::Split {
                    id: next_split_id(),
                    dir,
                    ratio: 0.5,
                    a: Box::new(old),
                    b: Box::new(Node::Leaf(new)),
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                if a.split_leaf(target, dir, new.clone()) {
                    true
                } else {
                    b.split_leaf(target, dir, new)
                }
            }
        }
    }

    /// Drop closed leaves; a split with one survivor collapses to it.
    fn reap(self, cx: &App) -> Option<Node> {
        match self {
            Node::Leaf(e) => (!e.read(cx).closed).then_some(Node::Leaf(e)),
            Node::Split { id, dir, ratio, a, b } => match (a.reap(cx), b.reap(cx)) {
                (Some(a), Some(b)) => Some(Node::Split {
                    id,
                    dir,
                    ratio,
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (Some(x), None) | (None, Some(x)) => Some(x),
                (None, None) => None,
            },
        }
    }

    fn dir_of(&self, target: u64) -> Option<SplitDir> {
        match self {
            Node::Leaf(_) => None,
            Node::Split { id, dir, a, b, .. } => {
                if *id == target {
                    Some(*dir)
                } else {
                    a.dir_of(target).or_else(|| b.dir_of(target))
                }
            }
        }
    }

    fn set_ratio(&mut self, target: u64, value: f32) -> bool {
        match self {
            Node::Leaf(_) => false,
            Node::Split { id, ratio, a, b, .. } => {
                if *id == target {
                    *ratio = value.clamp(0.15, 0.85);
                    true
                } else {
                    a.set_ratio(target, value) || b.set_ratio(target, value)
                }
            }
        }
    }
}

struct Tab {
    root: Node,
    name: Option<String>,
}

/// Frame-wide jiggle: the whole device hops ±1px every so often.
struct FrameJiggle {
    started: Instant,
    rng: u64,
    px: f32,
    until: f32,
    next_at: f32,
}

impl FrameJiggle {
    fn new() -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(3);
        Self {
            started: Instant::now(),
            rng: 0x9E3779B97F4A7C15 ^ seed,
            px: 0.,
            until: 0.,
            next_at: 5.0,
        }
    }
    fn rand(&mut self) -> f32 {
        self.rng = self
            .rng
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.rng >> 33) as f32) / (u32::MAX as f32 / 2.0)
    }
    fn tick(&mut self) -> bool {
        let t = self.started.elapsed().as_secs_f32();
        if self.px != 0. && t >= self.until {
            self.px = 0.;
            return true;
        }
        if self.px == 0. && t >= self.next_at {
            self.px = if self.rand() > 1.0 { 1.0 } else { -1.0 };
            self.until = t + 0.07;
            self.next_at = t + 7.0 + self.rand() * 5.0;
            return true;
        }
        false
    }
}

struct Workspace {
    doc: Entity<Doc>,
    tabs: Vec<Tab>,
    active: usize,
    focus_handle: gpui::FocusHandle,
    renaming: Option<(usize, String)>,
    jiggle: FrameJiggle,
    last_action: Instant,
    drag_split: Option<u64>,
    split_bounds: Arc<Mutex<std::collections::HashMap<u64, Bounds<Pixels>>>>,
    pane_seed: u64,
}

impl Workspace {
    fn new(doc: Entity<Doc>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut ws = Self {
            doc,
            tabs: vec![],
            active: 0,
            focus_handle: cx.focus_handle(),
            renaming: None,
            jiggle: FrameJiggle::new(),
            last_action: Instant::now() - Duration::from_secs(1),
            drag_split: None,
            split_bounds: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pane_seed: 0xD0C5,
        };
        // the opening pane: SOURCE mode — right-click → open → start typing
        let pane = ws.make_pane(Mode::Source, cx);
        ws.tabs.push(Tab {
            root: Node::Leaf(pane),
            name: None,
        });
        ws.focus_active(window, cx);
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(60))
                    .await;
                let _ = this.update(cx, |ws: &mut Workspace, cx| {
                    if ws.jiggle.tick() {
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        ws
    }

    fn make_pane(&mut self, mode: Mode, cx: &mut Context<Self>) -> Entity<MdPane> {
        self.pane_seed = self.pane_seed.wrapping_mul(31).wrapping_add(17);
        let doc = self.doc.clone();
        let seed = self.pane_seed;
        cx.new(|cx| MdPane::new(doc, mode, seed, cx))
    }

    fn pane_count(&self) -> usize {
        let mut n = 0;
        for t in &self.tabs {
            let mut v = vec![];
            t.root.leaves(&mut v);
            n += v.len();
        }
        n
    }

    /// Mouse-down can dispatch more than once per physical click (capture +
    /// bubble); structural actions debounce to one per 200ms.
    fn debounced(&mut self) -> bool {
        if self.last_action.elapsed() < Duration::from_millis(200) {
            return false;
        }
        self.last_action = Instant::now();
        true
    }

    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debounced() {
            return;
        }
        let pane = self.make_pane(Mode::Preview, cx);
        self.tabs.push(Tab {
            root: Node::Leaf(pane),
            name: None,
        });
        self.active = self.tabs.len() - 1;
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Split ONLY the focused pane. The new pane opens in the OPPOSITE mode:
    /// split a source pane and you get a live preview beside it.
    fn split(&mut self, dir: SplitDir, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debounced() {
            return;
        }
        if self.pane_count() >= MAX_PANES {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let mut leaves = vec![];
        tab.root.leaves(&mut leaves);
        let focused = leaves
            .iter()
            .find(|p| p.focus_handle(cx).is_focused(window))
            .or_else(|| leaves.first());
        let Some(focused) = focused else { return };
        let target = focused.entity_id();
        let new_mode = match focused.read(cx).mode {
            Mode::Source => Mode::Preview,
            Mode::Preview => Mode::Source,
        };
        let new_pane = self.make_pane(new_mode, cx);
        self.tabs[self.active].root.split_leaf(target, dir, new_pane);
        cx.notify();
    }

    fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debounced() {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let mut leaves = vec![];
        tab.root.leaves(&mut leaves);
        if let Some(p) = leaves
            .iter()
            .find(|p| p.focus_handle(cx).is_focused(window))
        {
            if self.pane_count() > 1 {
                p.update(cx, |pane, _| pane.closed = true);
                cx.notify();
            }
        }
    }

    fn reap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = false;
        let mut i = 0;
        while i < self.tabs.len() {
            let tab = self.tabs.remove(i);
            match tab.root.reap(cx) {
                Some(root) => {
                    self.tabs.insert(
                        i,
                        Tab {
                            root,
                            name: tab.name,
                        },
                    );
                    i += 1;
                }
                None => changed = true,
            }
        }
        if self.tabs.is_empty() {
            let pane = self.make_pane(Mode::Source, cx);
            self.tabs.push(Tab {
                root: Node::Leaf(pane),
                name: None,
            });
            changed = true;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
            changed = true;
        }
        if changed {
            self.focus_active(window, cx);
        }
    }

    fn activate_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i < self.tabs.len() {
            self.active = i;
            self.focus_active(window, cx);
            cx.notify();
        }
    }

    fn focus_active(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(tab) = self.tabs.get(self.active) {
            let mut leaves = vec![];
            tab.root.leaves(&mut leaves);
            if let Some(p) = leaves.first() {
                window.focus(&p.focus_handle(cx), cx);
            }
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        // the inline rename box owns the keyboard while open
        if let Some((tab_i, mut buf)) = self.renaming.take() {
            match ks.key.as_str() {
                "enter" => {
                    if let Some(tab) = self.tabs.get_mut(tab_i) {
                        tab.name = (!buf.trim().is_empty()).then(|| buf.trim().to_string());
                    }
                    self.focus_active(window, cx);
                }
                "escape" => self.focus_active(window, cx),
                "backspace" => {
                    buf.pop();
                    self.renaming = Some((tab_i, buf));
                }
                _ => {
                    if let Some(ch) = ks.key_char.as_ref() {
                        if buf.chars().count() < 18 {
                            buf.push_str(ch);
                        }
                    }
                    self.renaming = Some((tab_i, buf));
                }
            }
            cx.notify();
            return;
        }
        if m.control && m.shift && ks.key.as_str() == "t" {
            self.new_tab(window, cx);
            return;
        }
        if m.control && !m.alt && !m.shift && ks.key.as_str() == "w" {
            self.close_focused(window, cx);
            return;
        }
        if m.control && !m.alt && self.tabs.len() > 1 {
            match ks.key.as_str() {
                "pageup" => {
                    let n = self.tabs.len();
                    self.activate_tab((self.active + n - 1) % n, window, cx);
                    return;
                }
                "pagedown" => {
                    let n = self.tabs.len();
                    self.activate_tab((self.active + 1) % n, window, cx);
                    return;
                }
                _ => {}
            }
        }
        if m.control && m.alt {
            match ks.key.as_str() {
                "r" => self.split(SplitDir::Row, window, cx),
                "d" => self.split(SplitDir::Col, window, cx),
                _ => {}
            }
            return;
        }
        if m.alt && !m.control {
            let Some(tab) = self.tabs.get(self.active) else {
                return;
            };
            let mut leaves = vec![];
            tab.root.leaves(&mut leaves);
            if leaves.len() > 1 {
                let dir: i32 = match ks.key.as_str() {
                    "left" | "up" => -1,
                    "right" | "down" => 1,
                    _ => return,
                };
                let cur = leaves
                    .iter()
                    .position(|p| p.focus_handle(cx).is_focused(window))
                    .unwrap_or(0) as i32;
                let next = (cur + dir).rem_euclid(leaves.len() as i32) as usize;
                window.focus(&leaves[next].focus_handle(cx), cx);
                cx.notify();
            }
        }
    }

    fn on_mouse_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if ev.pressed_button == Some(MouseButton::Left) {
            if let Some(split_id) = self.drag_split {
                let bounds = self.split_bounds.lock().unwrap().get(&split_id).copied();
                if let (Some(b), Some(tab)) = (bounds, self.tabs.get_mut(self.active)) {
                    let rx = ((f32::from(ev.position.x) - f32::from(b.origin.x))
                        / f32::from(b.size.width).max(1.))
                    .clamp(0., 1.);
                    let ry = ((f32::from(ev.position.y) - f32::from(b.origin.y))
                        / f32::from(b.size.height).max(1.))
                    .clamp(0., 1.);
                    let dir = tab.root.dir_of(split_id);
                    let ratio = match dir {
                        Some(SplitDir::Row) => rx,
                        Some(SplitDir::Col) => ry,
                        None => return,
                    };
                    tab.root.set_ratio(split_id, ratio);
                    cx.notify();
                }
            }
        }
    }

    fn on_mouse_up(&mut self, _ev: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.drag_split.take().is_some() {
            cx.notify();
        }
    }

    fn bezel_btn(th: &theme::Theme, label: &str, active: bool) -> gpui::Div {
        let b = div()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .text_size(px(10.5))
            .cursor_pointer();
        if active {
            b.bg(th.frame_border.alpha(0.35))
                .border_color(th.frame_border)
                .text_color(white())
                .child(label.to_string())
        } else {
            b.bg(linear_gradient(
                135.,
                linear_color_stop(brighten(th.frame_bg, 1.7), 0.),
                linear_color_stop(darken(th.frame_bg, 0.7), 1.),
            ))
            .border_color(th.frame_border.alpha(0.4))
            .text_color(th.frame_text)
            .child(label.to_string())
        }
    }
}

fn darken(mut c: Hsla, f: f32) -> Hsla {
    c.l *= f;
    c
}
fn brighten(mut c: Hsla, f: f32) -> Hsla {
    c.l = (c.l * f).min(0.92);
    c
}

fn render_node(
    node: &Node,
    th: &theme::Theme,
    focused: Option<EntityId>,
    dragging: Option<u64>,
    registry: &Arc<Mutex<std::collections::HashMap<u64, Bounds<Pixels>>>>,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    match node {
        Node::Leaf(e) => {
            let is_focused = focused == Some(e.entity_id());
            div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(if is_focused {
                    th.frame_border.alpha(0.7)
                } else {
                    th.frame_border.alpha(0.25)
                })
                .child(e.clone())
        }
        Node::Split {
            id,
            dir,
            ratio,
            a,
            b,
        } => {
            let id = *id;
            let dir = *dir;
            let is_dragging = dragging == Some(id);
            let store = registry.clone();
            let measure = div().absolute().inset_0().child(
                canvas(
                    move |bounds, _, _| {
                        store.lock().unwrap().insert(id, bounds);
                    },
                    |_, _, _, _| {},
                )
                .size_full(),
            );
            let mut handle = div().flex_none().bg(if is_dragging {
                th.frame_border.alpha(0.8)
            } else {
                th.frame_border.alpha(0.25)
            });
            handle = match dir {
                SplitDir::Row => handle.w(px(7.)).h_full().cursor_col_resize(),
                SplitDir::Col => handle.h(px(7.)).w_full().cursor_row_resize(),
            };

            let first = div()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(render_node(a, th, focused, dragging, registry, cx));
            let first = match dir {
                SplitDir::Row => first.h_full().w(gpui::relative(*ratio)),
                SplitDir::Col => first.w_full().h(gpui::relative(*ratio)),
            };
            let second = div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(render_node(b, th, focused, dragging, registry, cx));

            let ratio_now = *ratio;
            let store2 = registry.clone();
            let base = div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .relative()
                .flex()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                        if ws.drag_split.is_some() {
                            return;
                        }
                        let Some(b) = store2.lock().unwrap().get(&id).copied() else {
                            return;
                        };
                        let (along, extent) = match dir {
                            SplitDir::Row => (
                                f32::from(ev.position.x) - f32::from(b.origin.x),
                                f32::from(b.size.width),
                            ),
                            SplitDir::Col => (
                                f32::from(ev.position.y) - f32::from(b.origin.y),
                                f32::from(b.size.height),
                            ),
                        };
                        let strip = ratio_now * extent;
                        if along >= strip - 6. && along <= strip + 13. {
                            ws.drag_split = Some(id);
                            cx.notify();
                        }
                    }),
                );
            let base = match dir {
                SplitDir::Row => base.flex_row(),
                SplitDir::Col => base.flex_col(),
            };
            base.child(measure).child(first).child(handle).child(second)
        }
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.reap(window, cx);
        warp::begin_frame(); // visible panes re-register their tube rects below
        let th = theme::theme(cx);
        let bezel = darken(th.frame_bg, 0.85);
        let tab = &self.tabs[self.active];
        let mut leaves = vec![];
        tab.root.leaves(&mut leaves);
        let focused_id = leaves
            .iter()
            .find(|p| p.focus_handle(cx).is_focused(window))
            .map(|p| p.entity_id());
        let focused_title = leaves
            .iter()
            .find(|p| Some(p.entity_id()) == focused_id)
            .or(leaves.first())
            .map(|p| p.read(cx).title(cx))
            .unwrap_or_default();
        let dirty = self.doc.read(cx).editor.dirty;
        let pane_count = self.pane_count();
        let tab_count = self.tabs.len();
        let jiggle = self.jiggle.px;

        // ---- tabs (right-click renames) ----
        let renaming = self.renaming.clone();
        let mut tab_strip = div().flex().flex_row().gap_1().items_center();
        for i in 0..tab_count {
            let is_active = i == self.active;
            if let Some((_, buf)) = renaming.as_ref().filter(|(ri, _)| *ri == i) {
                tab_strip = tab_strip.child(
                    div()
                        .px_2()
                        .py_0p5()
                        .rounded_sm()
                        .border_1()
                        .border_color(th.accent)
                        .bg(darken(th.bg, 0.8))
                        .text_size(px(11.))
                        .text_color(th.text)
                        .flex()
                        .flex_row()
                        .items_center()
                        .child(buf.clone())
                        .child(div().w(px(6.)).h(px(13.)).bg(th.accent)),
                );
                continue;
            }
            let label = self.tabs[i]
                .name
                .clone()
                .unwrap_or_else(|| format!("{}", i + 1));
            tab_strip = tab_strip.child(
                Self::bezel_btn(&th, &label, is_active)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                            ws.activate_tab(i, window, cx)
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                            let seed = ws.tabs[i].name.clone().unwrap_or_default();
                            ws.renaming = Some((i, seed));
                            window.focus(&ws.focus_handle, cx);
                            cx.notify();
                        }),
                    ),
            );
        }
        tab_strip = tab_strip.child(
            Self::bezel_btn(&th, "+", false).on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseDownEvent, window, cx| ws.new_tab(window, cx)),
            ),
        );

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
                    .items_center()
                    .gap_2()
                    .child(div().text_color(th.accent).child("▸ MARKDOWN-DELIGHT"))
                    .child(div().text_color(th.frame_faint.alpha(0.6)).child("// EDITOR"))
                    .child(tab_strip),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(Self::bezel_btn(&th, "▥ split", false).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                            ws.split(SplitDir::Row, window, cx)
                        }),
                    ))
                    .child(Self::bezel_btn(&th, "▤ split", false).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                            ws.split(SplitDir::Col, window, cx)
                        }),
                    )),
            );

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
            .child(SharedString::from(format!("{} · {}", th.name, focused_title)))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(SharedString::from(format!(
                        "{tab_count} tab{} · {pane_count} pane{}",
                        if tab_count == 1 { "" } else { "s" },
                        if pane_count == 1 { "" } else { "s" }
                    )))
                    .child(div().text_color(th.accent).child(if dirty {
                        "● MODIFIED"
                    } else {
                        "● READY"
                    })),
            );

        let pane_area = div()
            .size_full()
            .flex()
            .p(px(3.))
            .child(render_node(
                &tab.root,
                &th,
                focused_id,
                self.drag_split,
                &self.split_bounds,
                cx,
            ));

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
            .child(pane_area);

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
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
    application().run(move |cx: &mut App| {
        theme::init(cx);
        let bounds = gpui::Bounds::centered(None, size(px(1180.), px(800.)), cx);
        let title: SharedString = format!("{label} — markdown-delight").into();
        let doc = cx.new(|_| Doc::new(label.clone(), path.clone(), text.clone()));
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some(title),
                    ..Default::default()
                }),
                ..Default::default()
            },
            move |window, cx| {
                // match the .desktop StartupWMClass so the dock shows OUR icon
                window.set_app_id("markdown-delight");
                cx.new(|cx| Workspace::new(doc.clone(), window, cx))
            },
        )
        .expect("open window");
        cx.activate(true);
    });
}
