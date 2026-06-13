//! markdown-delight — tabs · tiling splits · monitor-wrap chrome, ported from
//! terminal-delight's proven workspace (the sibling, MIT).
//!
//! Splits divide ONLY the focused pane's space (true tiling tree). All panes
//! in a tab share ONE document: split a source pane and the new pane opens as
//! a LIVE PREVIEW of the same buffer — it re-renders as you type.
//!
//! ctrl+shift+t / [+]: new tab · per-tab [✕]: close tab · ctrl+pgup/pgdn:
//! switch tab · right-click tab: rename · ctrl+alt+r / [▮│ split]: split right
//! · ctrl+alt+d / [≣ split]: split down · alt+arrows: pane focus · ctrl+w:
//! close pane · ctrl+e: source ↔ preview · ctrl+s: save · bezel A–A scrubber:
//! live text zoom. Opens in SOURCE mode: right-click → open → type.

mod crt;
mod editor;
mod finder;
mod ipc;
mod pane;
mod render;
mod session;
mod theme;
mod warp;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use std::{env, fs, path::PathBuf};

use gpui::{
    App, Bounds, BoxShadow, Context, Entity, EntityId, Focusable, HighlightStyle, Hsla,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, SharedString,
    StyledText, TitlebarOptions, Window, WindowBounds, WindowOptions, canvas, div, hsla,
    linear_color_stop, linear_gradient, point, prelude::*, px, size, white,
};
use gpui_platform::application;
use pane::{Doc, MdPane, Mode};

const MAX_PANES: usize = 8;

/// Process start, for the MD_TIMING first-frame stamp from inside render.
static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static FIRST_FRAME: AtomicBool = AtomicBool::new(true);

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

    /// Like `split_leaf`, but grafts an EXISTING pane and controls which side it
    /// lands on: `dragged_first` puts the new pane in slot `a` (left/top), else
    /// `b` (right/bottom). Used by drag-to-split drops.
    fn split_leaf_with(
        &mut self,
        target: EntityId,
        dir: SplitDir,
        pane: Entity<MdPane>,
        dragged_first: bool,
    ) -> bool {
        match self {
            Node::Leaf(e) if e.entity_id() == target => {
                let old = std::mem::replace(self, Node::Leaf(pane.clone()));
                let dragged = Box::new(Node::Leaf(pane));
                let existing = Box::new(old);
                let (a, b) = if dragged_first {
                    (dragged, existing)
                } else {
                    (existing, dragged)
                };
                *self = Node::Split {
                    id: next_split_id(),
                    dir,
                    ratio: 0.5,
                    a,
                    b,
                };
                true
            }
            Node::Leaf(_) => false,
            Node::Split { a, b, .. } => {
                a.split_leaf_with(target, dir, pane.clone(), dragged_first)
                    || b.split_leaf_with(target, dir, pane, dragged_first)
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
    /// THIS tab's document — panes in a tab share it (live preview); the
    /// finder swaps it per-tab, so different tabs hold different files.
    doc: Entity<Doc>,
}

/// Which edge of a pane a drag is hovering — decides the split when dropped.
/// Left/Right ⇒ vertical split (SplitDir::Row); Top/Bottom ⇒ horizontal (Col).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DropZone {
    Left,
    Right,
    Top,
    Bottom,
}

/// A pending close awaiting confirmation (the doc has unsaved edits that would
/// be lost). Drives the themed "unsaved changes" modal.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CloseRequest {
    Tab(usize),
    Pane(EntityId),
}

/// Which scope the open theme tray is editing — the whole window (the global
/// "outer" theme) or one pane's override. Ported from terminal-delight's
/// outer/per-pane theme selector.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MenuScope {
    Outer,
    Pane(EntityId),
}

/// Seed-colour presets for the theme tray — fold the whole tube onto this hue.
/// (None = the theme's own colours.) Mirrors terminal-delight's SEED COLOUR row.
const SEED_SWATCHES: &[&str] = &[
    "#2f6fdd", "#31d7ff", "#22c55e", "#ff8a3d", "#f5d442", "#872d73", "#d6336c", "#828282",
];

/// The payload carried while a pane is being dragged. `source` is the Workspace
/// (window) it came from — entities are app-global, so a drop in ANY window can
/// reach back and detach it from its origin. The pane keeps its own Doc.
#[derive(Clone)]
struct DraggedPane {
    pane: Entity<MdPane>,
    source: Entity<Workspace>,
    pane_id: EntityId,
}

/// App-global mirror of the in-flight pane drag, so the window-root mouse-up can
/// tell a drop-on-the-desktop (tear-off → new window) from an internal drop.
/// `consumed` is set true by any pane/tab/`+` drop that handled the pane.
struct ActivePaneDrag {
    pane: Entity<MdPane>,
    source: Entity<Workspace>,
    pane_id: EntityId,
    consumed: bool,
}
impl gpui::Global for ActivePaneDrag {}

/// Mark the in-flight drag as handled internally (skip tear-off on release).
fn mark_drag_consumed(cx: &mut App) {
    if cx.try_global::<ActivePaneDrag>().is_some() {
        cx.global_mut::<ActivePaneDrag>().consumed = true;
    }
}

/// Open a fresh window holding a single existing pane (tear-off).
fn open_pane_window(pane: Entity<MdPane>, cx: &mut App) {
    let bounds = gpui::Bounds::centered(None, size(px(900.), px(680.)), cx);
    let title: SharedString = format!("{} — markdown-delight", pane.read(cx).title(cx)).into();
    let _ = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(title),
                ..Default::default()
            }),
            ..Default::default()
        },
        move |window, cx| {
            window.set_app_id("markdown-delight");
            cx.new(|cx| Workspace::from_pane(pane.clone(), window, cx))
        },
    );
}

/// The little ghost that follows the cursor during a pane drag.
struct DragGhost {
    label: SharedString,
    accent: Hsla,
    frame_bg: Hsla,
}

impl Render for DragGhost {
    fn render(&mut self, _w: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .border_color(self.accent)
            .bg(self.frame_bg.alpha(0.92))
            .text_size(px(11.))
            .text_color(self.accent)
            .child(SharedString::from(format!("▸ {}", self.label)))
    }
}

/// A filesystem-safe slug (≤40 chars) from arbitrary text — used to name a
/// scratch notebook after its first line. Empty if nothing usable.
fn slugify(s: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in s.trim().trim_start_matches('#').trim().chars() {
        if ch.is_alphanumeric() {
            for c in ch.to_lowercase() {
                out.push(c);
            }
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
        if out.chars().count() >= 40 {
            break;
        }
    }
    out.trim_matches('-').to_string()
}

/// `~/markdown-delight-notebooks` (created on demand) — where scratch notebooks
/// are saved.
fn notebook_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let dir = PathBuf::from(home).join("markdown-delight-notebooks");
    fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

/// A non-colliding `<base>.md` (then `-2`, `-3`, …) under the notebook dir.
fn unique_notebook_path(base: &str) -> Option<PathBuf> {
    let dir = notebook_dir()?;
    let first = dir.join(format!("{base}.md"));
    if !first.exists() {
        return Some(first);
    }
    (2..=9999)
        .map(|n| dir.join(format!("{base}-{n}.md")))
        .find(|p| !p.exists())
}

/// Clamp a label to `max` characters (char-safe), appending `…` if truncated.
fn truncate_label(label: &str, max: usize) -> String {
    if label.chars().count() <= max {
        return label.to_string();
    }
    let mut s: String = label.chars().take(max.saturating_sub(1)).collect();
    s.push('…');
    s
}

/// Nearest edge of a `w`×`h` box to the point `(x, y)` — used to pick the
/// split zone as the cursor moves around a drop-target pane.
fn nearest_zone(x: f32, y: f32, w: f32, h: f32) -> DropZone {
    let d_left = x;
    let d_right = (w - x).max(0.);
    let d_top = y;
    let d_bottom = (h - y).max(0.);
    let mut zone = DropZone::Left;
    let mut best = d_left;
    if d_right < best {
        best = d_right;
        zone = DropZone::Right;
    }
    if d_top < best {
        best = d_top;
        zone = DropZone::Top;
    }
    if d_bottom < best {
        zone = DropZone::Bottom;
    }
    zone
}

/// How many rows the finder shows.
const FINDER_MAX: usize = 12;

/// The open Ctrl+P fuzzy finder (anchored to `target`, the pane it opens
/// into). `hits` is CACHED — recomputed only when the query changes or the
/// background index grows, never per render frame (the CRT loop repaints
/// ~30fps and a full fuzzy scan of $HOME each frame would be brutal).
struct FinderState {
    target: EntityId,
    query: String,
    selected: usize,
    hits: Vec<finder::Hit>,
    indexing: bool,
    total: usize,
}

impl FinderState {
    fn recompute(&mut self, index: &finder::FileIndex) {
        let (hits, indexing, total) = index.hits(&self.query, FINDER_MAX);
        self.selected = self.selected.min(hits.len().saturating_sub(1));
        self.hits = hits;
        self.indexing = indexing;
        self.total = total;
    }
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
    /// While a pane is dragged over one of THIS window's panes: which pane and
    /// which edge — drives the drop-zone overlay and the split on release.
    drop_target: Option<(EntityId, DropZone)>,
    /// A close awaiting "save / discard / cancel" because of unsaved edits.
    confirm_close: Option<CloseRequest>,
    /// The open theme tray, if any, and the window-space point to anchor it at
    /// (a pane chip click). None anchor = the fixed top-right outer-button slot.
    theme_menu: Option<MenuScope>,
    menu_at: Option<gpui::Point<Pixels>>,
    /// The current outer (window-wide) theme choice: (theme name, optional seed
    /// hue). Applied to the global ActiveTheme; panes with no override follow it.
    outer: (String, Option<Hsla>),
    /// Live text-zoom scrubber: true while the knob is held; `font_track` holds
    /// the slider's painted bounds so a drag maps cursor-x → scale.
    font_drag: bool,
    font_track: Arc<Mutex<Option<Bounds<Pixels>>>>,
    pane_seed: u64,
    finder: Option<FinderState>,
    index: finder::FileIndex,
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
            drop_target: None,
            confirm_close: None,
            theme_menu: None,
            menu_at: None,
            outer: (theme::theme(cx).name.clone(), None),
            font_drag: false,
            font_track: Arc::new(Mutex::new(None)),
            pane_seed: 0xD0C5,
            finder: None,
            index: finder::FileIndex::spawn(),
        };
        // the opening pane: SOURCE mode — right-click → open → start typing
        let pane = ws.make_pane(Mode::Source, None, cx);
        let doc = ws.doc.clone();
        ws.tabs.push(Tab {
            root: Node::Leaf(pane),
            name: None,
            doc,
        });
        // reopen the files/notebooks that were open last launch (appended after
        // the launched file's tab, which stays active)
        ws.restore_session(cx);
        ws.focus_active(window, cx);
        ws.start_jiggle(cx);
        ws.start_session_autosave(cx);
        ws
    }

    /// A new window seeded with ONE existing pane (tear-off). The pane keeps its
    /// own doc and theme override; the window shows it as a single tab.
    fn from_pane(pane: Entity<MdPane>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let doc = pane.read(cx).doc.clone();
        let mut ws = Self {
            doc: doc.clone(),
            tabs: vec![],
            active: 0,
            focus_handle: cx.focus_handle(),
            renaming: None,
            jiggle: FrameJiggle::new(),
            last_action: Instant::now() - Duration::from_secs(1),
            drag_split: None,
            split_bounds: Arc::new(Mutex::new(std::collections::HashMap::new())),
            drop_target: None,
            confirm_close: None,
            theme_menu: None,
            menu_at: None,
            outer: (theme::theme(cx).name.clone(), None),
            font_drag: false,
            font_track: Arc::new(Mutex::new(None)),
            pane_seed: 0xD0C5,
            finder: None,
            index: finder::FileIndex::spawn(),
        };
        ws.tabs.push(Tab {
            root: Node::Leaf(pane),
            name: None,
            doc,
        });
        ws.focus_active(window, cx);
        ws.start_jiggle(cx);
        ws
    }

    /// The frame-wide jiggle ticker (shared by `new` and `from_pane`).
    fn start_jiggle(&self, cx: &mut Context<Self>) {
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
    }

    fn make_pane(
        &mut self,
        mode: Mode,
        theme: Option<String>,
        cx: &mut Context<Self>,
    ) -> Entity<MdPane> {
        self.pane_seed = self.pane_seed.wrapping_mul(31).wrapping_add(17);
        // new panes join the ACTIVE tab's document (splits = live preview)
        let doc = self
            .tabs
            .get(self.active)
            .map(|t| t.doc.clone())
            .unwrap_or_else(|| self.doc.clone());
        let seed = self.pane_seed;
        cx.new(|cx| MdPane::new(doc, mode, seed, theme, cx))
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

    /// `+` → a brand-new tab with a BLANK, FRESH markdown doc (a scratch
    /// notebook), opened in SOURCE mode so you can start writing immediately.
    /// (Note: NOT `make_pane`, which would reuse the active tab's shared doc.)
    fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debounced() {
            return;
        }
        let doc = cx.new(|_| Doc::new("untitled".to_string(), None, String::new()));
        self.pane_seed = self.pane_seed.wrapping_mul(31).wrapping_add(17);
        let seed = self.pane_seed;
        let pane = cx.new(|cx| MdPane::new(doc.clone(), Mode::Source, seed, None, cx));
        self.tabs.push(Tab {
            root: Node::Leaf(pane),
            name: Some("untitled".to_string()),
            doc,
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
        let focused_read = focused.read(cx);
        let new_mode = match focused_read.mode {
            Mode::Source => Mode::Preview,
            Mode::Preview => Mode::Source,
        };
        // the split inherits the focused pane's theme override (name + seed)
        let inherit_theme = focused_read.theme.clone();
        let inherit_seed = focused_read.seed;
        let new_pane = self.make_pane(new_mode, inherit_theme, cx);
        new_pane.update(cx, |p, cx| {
            p.seed = inherit_seed;
            cx.notify();
        });
        self.tabs[self.active].root.split_leaf(target, dir, new_pane);
        cx.notify();
    }

    /* ---------------- pane drag-and-drop ---------------- */

    fn find_pane(&self, pane_id: EntityId) -> Option<Entity<MdPane>> {
        for t in &self.tabs {
            let mut leaves = vec![];
            t.root.leaves(&mut leaves);
            if let Some(p) = leaves.iter().find(|p| p.entity_id() == pane_id) {
                return Some((*p).clone());
            }
        }
        None
    }

    /// Remove a pane from THIS window's tree (used by every move/tear-off). The
    /// entity is NOT destroyed — it's re-grafted elsewhere — so we toggle the
    /// `closed` flag only long enough to reuse `reap`'s tree-pruning. Never
    /// leaves zero tabs (would panic the renderer).
    fn detach_pane(&mut self, pane_id: EntityId, cx: &mut Context<Self>) {
        let Some(pane) = self.find_pane(pane_id) else {
            return;
        };
        pane.update(cx, |p, _| p.closed = true);
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
                            doc: tab.doc,
                        },
                    );
                    i += 1;
                }
                None => {}
            }
        }
        pane.update(cx, |p, _| p.closed = false);
        if self.tabs.is_empty() {
            let fresh = self.make_pane(Mode::Source, None, cx);
            let doc = fresh.read(cx).doc.clone();
            self.tabs.push(Tab {
                root: Node::Leaf(fresh),
                name: None,
                doc,
            });
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len() - 1;
        }
        cx.notify();
    }

    /// Detach the dragged pane from its origin window (cross-window aware).
    fn detach_from_source(&mut self, d: &DraggedPane, cx: &mut Context<Self>) {
        // an internal drop handled the pane → don't also tear it off on release
        mark_drag_consumed(cx);
        if d.source.entity_id() == cx.entity_id() {
            self.detach_pane(d.pane_id, cx);
        } else {
            let pid = d.pane_id;
            d.source.update(cx, |src, cx| src.detach_pane(pid, cx));
        }
    }

    /// Drop a dragged pane onto another pane → split that pane positionally and
    /// land the dragged pane in the new slot. Keeps the dragged pane's own doc.
    fn accept_pane_drop(
        &mut self,
        d: &DraggedPane,
        target_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let zone = self
            .drop_target
            .take()
            .filter(|(t, _)| *t == target_id)
            .map(|(_, z)| z)
            .unwrap_or(DropZone::Right);
        if d.pane_id == target_id {
            cx.notify();
            return;
        }
        let cross = d.source.entity_id() != cx.entity_id();
        if cross && self.pane_count() >= MAX_PANES {
            cx.notify();
            return;
        }
        self.detach_from_source(d, cx);
        let (dir, dragged_first) = match zone {
            DropZone::Left => (SplitDir::Row, true),
            DropZone::Right => (SplitDir::Row, false),
            DropZone::Top => (SplitDir::Col, true),
            DropZone::Bottom => (SplitDir::Col, false),
        };
        for (ti, t) in self.tabs.iter_mut().enumerate() {
            let mut leaves = vec![];
            t.root.leaves(&mut leaves);
            if leaves.iter().any(|p| p.entity_id() == target_id) {
                if t.root.split_leaf_with(target_id, dir, d.pane.clone(), dragged_first) {
                    self.active = ti;
                    window.focus(&d.pane.focus_handle(cx), cx);
                }
                break;
            }
        }
        cx.notify();
    }

    /// Drop a dragged pane onto a tab button → move it into that tab (split the
    /// tab's first pane, dragged on the right).
    fn move_pane_to_tab(
        &mut self,
        d: &DraggedPane,
        ti: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.drop_target = None;
        if ti >= self.tabs.len() {
            cx.notify();
            return;
        }
        let target_leaf_id = {
            let mut l = vec![];
            self.tabs[ti].root.leaves(&mut l);
            l.first().map(|p| p.entity_id())
        };
        let Some(target_leaf_id) = target_leaf_id else {
            cx.notify();
            return;
        };
        if target_leaf_id == d.pane_id {
            cx.notify();
            return;
        }
        let cross = d.source.entity_id() != cx.entity_id();
        if cross && self.pane_count() >= MAX_PANES {
            cx.notify();
            return;
        }
        self.detach_from_source(d, cx);
        // tab indices may have shifted on detach — relocate by the stable leaf id
        for (i, t) in self.tabs.iter_mut().enumerate() {
            let mut l = vec![];
            t.root.leaves(&mut l);
            if l.iter().any(|p| p.entity_id() == target_leaf_id) {
                t.root
                    .split_leaf_with(target_leaf_id, SplitDir::Row, d.pane.clone(), false);
                self.active = i;
                break;
            }
        }
        window.focus(&d.pane.focus_handle(cx), cx);
        cx.notify();
    }

    /// Drop a dragged pane onto the `+`/empty tab strip → move it into a new tab.
    fn new_tab_with(&mut self, d: &DraggedPane, window: &mut Window, cx: &mut Context<Self>) {
        self.drop_target = None;
        self.detach_from_source(d, cx);
        let doc = d.pane.read(cx).doc.clone();
        self.tabs.push(Tab {
            root: Node::Leaf(d.pane.clone()),
            name: None,
            doc,
        });
        self.active = self.tabs.len() - 1;
        window.focus(&d.pane.focus_handle(cx), cx);
        cx.notify();
    }

    /// Tear a pane off into a brand-new OS window (the header ⧉ button).
    fn pop_out_pane(&mut self, pane_id: EntityId, cx: &mut Context<Self>) {
        let Some(pane) = self.find_pane(pane_id) else {
            return;
        };
        self.detach_pane(pane_id, cx);
        open_pane_window(pane, cx);
        cx.notify();
    }

    /// On drag release: if a pane was dropped OUTSIDE this window (onto the
    /// desktop) and no internal drop claimed it, tear it into a new window.
    /// Always clears the app-global drag mirror.
    fn end_pane_drag(&mut self, outside: bool, cx: &mut Context<Self>) {
        let Some(drag) = cx.try_global::<ActivePaneDrag>() else {
            return;
        };
        let (consumed, pane, source, pane_id) = (
            drag.consumed,
            drag.pane.clone(),
            drag.source.clone(),
            drag.pane_id,
        );
        cx.remove_global::<ActivePaneDrag>();
        if consumed || !outside {
            return;
        }
        // detach from origin window, then open a fresh window holding the pane
        if source.entity_id() == cx.entity_id() {
            self.detach_pane(pane_id, cx);
        } else {
            source.update(cx, |src, cx| src.detach_pane(pane_id, cx));
        }
        open_pane_window(pane, cx);
        cx.notify();
    }

    /* ---------------- theme tray (outer + per-pane selector) ---------------- */

    /// Open the theme tray for a scope, anchored at `at` (None = outer slot).
    fn open_theme_menu(
        &mut self,
        scope: MenuScope,
        at: Option<gpui::Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        self.theme_menu = Some(scope);
        self.menu_at = at;
        cx.notify();
    }

    fn close_theme_menu(&mut self, cx: &mut Context<Self>) {
        if self.theme_menu.take().is_some() {
            self.menu_at = None;
            cx.notify();
        }
    }

    /// The (theme name, seed) currently in effect for the open tray's scope.
    fn menu_choice(&self, cx: &App) -> (String, Option<Hsla>) {
        match self.theme_menu {
            Some(MenuScope::Pane(id)) => match self.find_pane(id) {
                Some(p) => {
                    let p = p.read(cx);
                    let name = p
                        .theme
                        .clone()
                        .unwrap_or_else(|| self.outer.0.clone());
                    (name, p.seed)
                }
                None => self.outer.clone(),
            },
            _ => self.outer.clone(),
        }
    }

    /// Re-derive the global ActiveTheme from the current outer choice (theme +
    /// seed). Panes with no override follow it; the bezel/chrome adopt it too.
    fn rebuild_outer(&self, cx: &mut Context<Self>) {
        let (name, seed) = &self.outer;
        let base = theme::resolve(cx, Some(name));
        let t = match seed {
            Some(s) => theme::recolor(&base, *s),
            None => (*base).clone(),
        };
        cx.set_global(theme::ActiveTheme(Arc::new(t)));
        theme::apply_warp_theme(cx);
    }

    /// Pick a theme in the open tray (keeps the current seed).
    fn set_menu_theme(&mut self, name: String, cx: &mut Context<Self>) {
        match self.theme_menu {
            Some(MenuScope::Pane(id)) => {
                if let Some(pane) = self.find_pane(id) {
                    pane.update(cx, |p, cx| {
                        p.theme = Some(name);
                        cx.notify();
                    });
                }
            }
            _ => {
                self.outer.0 = name;
                self.rebuild_outer(cx);
            }
        }
        cx.notify();
    }

    /// Pick a seed hue in the open tray (keeps the current theme).
    fn set_menu_seed(&mut self, seed: Option<Hsla>, cx: &mut Context<Self>) {
        match self.theme_menu {
            Some(MenuScope::Pane(id)) => {
                if let Some(pane) = self.find_pane(id) {
                    pane.update(cx, |p, cx| {
                        p.seed = seed;
                        cx.notify();
                    });
                }
            }
            _ => {
                self.outer.1 = seed;
                self.rebuild_outer(cx);
            }
        }
        cx.notify();
    }

    /// "Follow outer" — clear a pane's override entirely (theme + seed).
    fn clear_pane_override(&mut self, cx: &mut Context<Self>) {
        if let Some(MenuScope::Pane(id)) = self.theme_menu {
            if let Some(pane) = self.find_pane(id) {
                pane.update(cx, |p, cx| {
                    p.theme = None;
                    p.seed = None;
                    cx.notify();
                });
            }
        }
        cx.notify();
    }

    /* ---------------- closing (with unsaved-changes guard) ---------------- */

    /// How many panes (across all tabs) currently view `doc`.
    fn doc_view_count(&self, doc: &Entity<Doc>, cx: &Context<Self>) -> usize {
        let id = doc.entity_id();
        let mut n = 0;
        for t in &self.tabs {
            let mut l = vec![];
            t.root.leaves(&mut l);
            n += l.iter().filter(|p| p.read(cx).doc.entity_id() == id).count();
        }
        n
    }

    /// Unique dirty docs shown in tab `i`.
    fn tab_dirty_docs(&self, i: usize, cx: &Context<Self>) -> Vec<Entity<Doc>> {
        let Some(t) = self.tabs.get(i) else {
            return vec![];
        };
        let mut l = vec![];
        t.root.leaves(&mut l);
        let mut seen = std::collections::HashSet::new();
        let mut out = vec![];
        for p in l {
            let d = p.read(cx).doc.clone();
            if d.read(cx).editor.dirty && seen.insert(d.entity_id()) {
                out.push(d);
            }
        }
        out
    }

    /// Cheap (allocation-free) "does this tab hold any unsaved edits?" — called
    /// per-tab every frame for the tab dirty-dot, so it short-circuits.
    fn tab_is_dirty(&self, i: usize, cx: &Context<Self>) -> bool {
        let Some(t) = self.tabs.get(i) else {
            return false;
        };
        let mut l = vec![];
        t.root.leaves(&mut l);
        l.iter().any(|p| p.read(cx).doc.read(cx).editor.dirty)
    }

    /// True if closing tab `i` would lose unsaved edits not shown anywhere else.
    fn tab_would_lose_unsaved(&self, i: usize, cx: &Context<Self>) -> bool {
        let Some(t) = self.tabs.get(i) else {
            return false;
        };
        for d in self.tab_dirty_docs(i, cx) {
            let mut l = vec![];
            t.root.leaves(&mut l);
            let in_tab = l
                .iter()
                .filter(|p| p.read(cx).doc.entity_id() == d.entity_id())
                .count();
            if self.doc_view_count(&d, cx) == in_tab {
                return true; // every view of this dirty doc is inside the tab
            }
        }
        false
    }

    /// True if closing pane `id` removes the last view of an unsaved doc.
    fn pane_would_lose_unsaved(&self, id: EntityId, cx: &Context<Self>) -> bool {
        let Some(p) = self.find_pane(id) else {
            return false;
        };
        let d = p.read(cx).doc.clone();
        d.read(cx).editor.dirty && self.doc_view_count(&d, cx) == 1
    }

    /// Ctrl+W — close the focused pane (guarded).
    fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debounced() {
            return;
        }
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let mut leaves = vec![];
        tab.root.leaves(&mut leaves);
        let id = leaves
            .iter()
            .find(|p| p.focus_handle(cx).is_focused(window))
            .map(|p| p.entity_id());
        if let Some(id) = id {
            self.request_close_pane(id, window, cx);
        }
    }

    /// Tab ✕ clicked → confirm if it would lose unsaved work, else close now.
    fn request_close_tab(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if !self.debounced() {
            return;
        }
        if self.tab_would_lose_unsaved(i, cx) {
            self.confirm_close = Some(CloseRequest::Tab(i));
            cx.notify();
        } else {
            self.close_tab_now(i, window, cx);
        }
    }

    /// Pane ✕ clicked → confirm if it would lose unsaved work, else close now.
    fn request_close_pane(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if self.pane_would_lose_unsaved(id, cx) {
            self.confirm_close = Some(CloseRequest::Pane(id));
            cx.notify();
        } else {
            self.close_pane_now(id, window, cx);
        }
    }

    /// Close a single tab by index. Always leaves one tab alive — closing the
    /// last one drops in a fresh blank, same as `reap`'s floor.
    fn close_tab_now(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.tabs.len() {
            return;
        }
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            let pane = self.make_pane(Mode::Source, None, cx);
            let doc = pane.read(cx).doc.clone();
            self.tabs.push(Tab {
                root: Node::Leaf(pane),
                name: None,
                doc,
            });
        }
        // keep `active` pointing at the same tab it did before the removal
        if self.active > i || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.focus_active(window, cx);
        cx.notify();
    }

    /// Close a specific pane by id (its ✕). reap prunes the tree / empty tabs.
    fn close_pane_now(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.find_pane(id) {
            p.update(cx, |pane, _| pane.closed = true);
            self.reap(window, cx);
            cx.notify();
        }
    }

    /* ---------------- session restore (reopen on launch) ---------------- */

    /// Reopen the files/notebooks that were open last launch. Appended after the
    /// launched file's tab (tab 0), which stays active. Only path-bearing docs
    /// that still exist are restored; already-open files are skipped (dedupe).
    fn restore_session(&mut self, cx: &mut Context<Self>) {
        let s = session::load();
        for e in s.tabs {
            let p = PathBuf::from(&e.path);
            if !p.exists() {
                continue;
            }
            let already = self
                .tabs
                .iter()
                .any(|t| t.doc.read(cx).path.as_deref() == Some(p.as_path()));
            if already {
                continue;
            }
            let Ok(text) = fs::read_to_string(&p) else {
                continue;
            };
            let label = p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| e.path.clone());
            let doc = cx.new(|_| Doc::new(label.clone(), Some(p.clone()), text));
            self.pane_seed = self.pane_seed.wrapping_mul(31).wrapping_add(17);
            let seed = self.pane_seed;
            let pane = cx.new(|cx| MdPane::new(doc.clone(), Mode::Preview, seed, None, cx));
            let name = e.name.or_else(|| Some(truncate_label(&label, 40)));
            self.tabs.push(Tab {
                root: Node::Leaf(pane),
                name,
                doc,
            });
        }
    }

    /// A cheap fingerprint of the open-file set + active tab — autosave only
    /// writes the session when this changes.
    fn session_signature(&self, cx: &App) -> String {
        let mut s = format!("{}|", self.active);
        for t in &self.tabs {
            if let Some(p) = t.doc.read(cx).path.as_ref() {
                s.push_str(&p.to_string_lossy());
                s.push('\n');
            }
        }
        s
    }

    /// Persist the open path-bearing tabs (with the active index mapped into the
    /// filtered list).
    fn persist_session(&self, cx: &App) {
        let mut tabs = vec![];
        let mut active = 0;
        for (i, t) in self.tabs.iter().enumerate() {
            if let Some(p) = t.doc.read(cx).path.as_ref() {
                if i == self.active {
                    active = tabs.len();
                }
                tabs.push(session::SessionTab {
                    path: p.to_string_lossy().into_owned(),
                    name: t.name.clone(),
                });
            }
        }
        session::save(&session::Session { active, tabs });
    }

    /// Background loop: every 2s, persist the session iff the open-file set
    /// changed. One place owns persistence → no scattered save calls to drift.
    fn start_session_autosave(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut last = String::new();
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(2))
                    .await;
                let res = this.update(cx, |ws, cx| {
                    let sig = ws.session_signature(cx);
                    if sig != last {
                        ws.persist_session(cx);
                    }
                    sig
                });
                match res {
                    Ok(sig) => last = sig,
                    Err(_) => break, // window closed
                }
            }
        })
        .detach();
    }

    /// Ctrl+S — save the focused pane's doc. A doc with a path saves in place;
    /// a pathless scratch notebook is saved-as into ~/markdown-delight-notebooks
    /// (named after its first line) and the containing tab is renamed to match.
    fn save_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let mut leaves = vec![];
        tab.root.leaves(&mut leaves);
        let pane = leaves
            .iter()
            .find(|p| p.focus_handle(cx).is_focused(window))
            .or(leaves.first())
            .cloned();
        let Some(pane) = pane else {
            return;
        };
        let doc = pane.read(cx).doc.clone();

        // in-place save when the doc already has a path
        if doc.read(cx).path.is_some() {
            doc.update(cx, |d, cx| {
                if let Some(p) = d.path.clone() {
                    if let Err(e) = d.editor.save(&p) {
                        eprintln!("save failed: {e}");
                    }
                    d.reparse();
                    cx.notify();
                }
            });
            cx.notify();
            return;
        }

        // save-as for a scratch notebook → derive a name from its first line
        let base = {
            let s = slugify(&doc.read(cx).editor.line(0));
            if s.is_empty() {
                "untitled".to_string()
            } else {
                s
            }
        };
        let Some(path) = unique_notebook_path(&base) else {
            eprintln!("could not allocate a notebook path");
            return;
        };
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("{base}.md"));
        let ok = doc.update(cx, |d, cx| match d.editor.save(&path) {
            Ok(()) => {
                d.path = Some(path.clone());
                d.label = label.clone().into();
                d.reparse();
                cx.notify();
                true
            }
            Err(e) => {
                eprintln!("save failed: {e}");
                false
            }
        });
        if ok {
            if let Some(t) = self.tabs.get_mut(self.active) {
                t.name = Some(truncate_label(&label, 40));
            }
            cx.notify();
        }
    }

    /// Modal "Save & Close": persist the target's dirty docs (those with a path)
    /// then close. Pathless scratch buffers can't be saved — they close anyway.
    fn confirm_save_and_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(req) = self.confirm_close.take() else {
            return;
        };
        let docs = match req {
            CloseRequest::Tab(i) => self.tab_dirty_docs(i, cx),
            CloseRequest::Pane(id) => self
                .find_pane(id)
                .map(|p| vec![p.read(cx).doc.clone()])
                .unwrap_or_default(),
        };
        for d in docs {
            d.update(cx, |doc, cx| {
                if let Some(path) = doc.path.clone() {
                    if let Err(e) = doc.editor.save(&path) {
                        eprintln!("save failed: {e}");
                    }
                    doc.reparse();
                    cx.notify();
                }
            });
        }
        match req {
            CloseRequest::Tab(i) => self.close_tab_now(i, window, cx),
            CloseRequest::Pane(id) => self.close_pane_now(id, window, cx),
        }
        cx.notify();
    }

    /// Modal "Close without saving".
    fn confirm_discard_and_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(req) = self.confirm_close.take() else {
            return;
        };
        match req {
            CloseRequest::Tab(i) => self.close_tab_now(i, window, cx),
            CloseRequest::Pane(id) => self.close_pane_now(id, window, cx),
        }
        cx.notify();
    }

    /// Modal "Cancel".
    fn confirm_cancel(&mut self, cx: &mut Context<Self>) {
        if self.confirm_close.take().is_some() {
            cx.notify();
        }
    }

    /// The effective theme + dirty file names for the close-confirm modal.
    fn confirm_context(&self, req: CloseRequest, cx: &Context<Self>) -> (Arc<theme::Theme>, Vec<String>) {
        match req {
            CloseRequest::Pane(id) => {
                let th = self
                    .find_pane(id)
                    .map(|p| p.read(cx).effective_theme(cx))
                    .unwrap_or_else(|| theme::theme(cx));
                let names = self
                    .find_pane(id)
                    .map(|p| vec![p.read(cx).title(cx).to_string()])
                    .unwrap_or_default();
                (th, names)
            }
            CloseRequest::Tab(i) => {
                // theme of the tab's first pane; names of its dirty docs
                let th = self
                    .tabs
                    .get(i)
                    .and_then(|t| {
                        let mut l = vec![];
                        t.root.leaves(&mut l);
                        l.first().map(|p| p.read(cx).effective_theme(cx))
                    })
                    .unwrap_or_else(|| theme::theme(cx));
                let names = self
                    .tab_dirty_docs(i, cx)
                    .iter()
                    .map(|d| d.read(cx).label.to_string())
                    .collect();
                (th, names)
            }
        }
    }

    /// Map a cursor x (window space) onto the scrubber track → font scale.
    fn set_font_from_x(&mut self, x_px: f32, cx: &mut Context<Self>) {
        let Some(b) = *self.font_track.lock().unwrap() else {
            return;
        };
        let w = f32::from(b.size.width).max(1.);
        let t = ((x_px - f32::from(b.origin.x)) / w).clamp(0., 1.);
        theme::set_font_scale(theme::FONT_SCALE_MIN + t * (theme::FONT_SCALE_MAX - theme::FONT_SCALE_MIN));
        // the panes read font_scale() at render time — nudge them to repaint now
        if let Some(tab) = self.tabs.get(self.active) {
            let mut leaves = vec![];
            tab.root.leaves(&mut leaves);
            for p in leaves {
                p.update(cx, |_, cx| cx.notify());
            }
        }
        cx.notify();
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
                            doc: tab.doc,
                        },
                    );
                    i += 1;
                }
                None => changed = true,
            }
        }
        if self.tabs.is_empty() {
            let pane = self.make_pane(Mode::Source, None, cx);
            let doc = pane.read(cx).doc.clone();
            self.tabs.push(Tab {
                root: Node::Leaf(pane),
                name: None,
                doc,
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

    /// Open the finder anchored to the focused pane (the one it'll open into).
    fn open_finder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.get(self.active) else {
            return;
        };
        let mut leaves = vec![];
        tab.root.leaves(&mut leaves);
        let target = leaves
            .iter()
            .find(|p| p.focus_handle(cx).is_focused(window))
            .or_else(|| leaves.first())
            .map(|p| p.entity_id());
        let Some(target) = target else { return };
        let mut fs = FinderState {
            target,
            query: String::new(),
            selected: 0,
            hits: vec![],
            indexing: false,
            total: 0,
        };
        fs.recompute(&self.index);
        self.finder = Some(fs);
        // workspace owns the keyboard while the finder is open
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Open `path` into the finder's target pane — "in THIS tab/pane". The
    /// whole active tab shares one Doc, so swapping it updates every pane.
    fn finder_open(&mut self, idx: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(fs) = self.finder.take() else { return };
        let Some(hit) = fs.hits.get(idx) else {
            // nothing matched — just close
            self.focus_active(window, cx);
            cx.notify();
            return;
        };
        let path = PathBuf::from(&hit.path);
        let (label, text) = match fs::read_to_string(&path) {
            Ok(t) => (
                path.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| hit.display.clone()),
                t,
            ),
            Err(e) => (format!("{} (error)", hit.display), format!("could not read:\n{e}")),
        };
        // auto-name the tab after the file it now holds (≤ 40 chars)
        let tab_name = truncate_label(&label, 40);
        let new_doc = cx.new(|_| Doc::new(label, Some(path), text));
        // every pane in the active tab follows the tab's shared doc
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.doc = new_doc.clone();
            tab.name = Some(tab_name);
            let mut leaves = vec![];
            tab.root.leaves(&mut leaves);
            let leaves: Vec<Entity<MdPane>> = leaves.into_iter().cloned().collect();
            for p in leaves {
                p.update(cx, |pane, cx| pane.set_doc(new_doc.clone(), cx));
            }
        }
        // refocus the pane the finder targeted, so you can type immediately
        let mut leaves = vec![];
        if let Some(tab) = self.tabs.get(self.active) {
            tab.root.leaves(&mut leaves);
        }
        if let Some(p) = leaves.iter().find(|p| p.entity_id() == fs.target).or(leaves.first()) {
            window.focus(&p.focus_handle(cx), cx);
        }
        cx.notify();
    }

    /// Keystrokes while the finder is open. Returns true if it consumed the key.
    fn finder_key(&mut self, ks: &gpui::Keystroke, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.finder.is_none() {
            return false;
        }
        match ks.key.as_str() {
            "escape" => {
                self.finder = None;
                self.focus_active(window, cx);
                cx.notify();
            }
            "enter" => {
                let sel = self.finder.as_ref().map(|f| f.selected).unwrap_or(0);
                self.finder_open(sel, window, cx);
            }
            "down" | "up" => {
                if let Some(f) = self.finder.as_mut() {
                    let n = f.hits.len().max(1);
                    let step = if ks.key == "down" { 1 } else { n - 1 };
                    f.selected = (f.selected + step) % n;
                    cx.notify();
                }
            }
            "backspace" => {
                if let Some(f) = self.finder.as_mut() {
                    f.query.pop();
                    f.selected = 0;
                }
                self.refresh_finder(cx);
            }
            _ => {
                if let Some(ch) = ks.key_char.as_ref() {
                    if let Some(f) = self.finder.as_mut() {
                        f.query.push_str(ch);
                        f.selected = 0;
                    }
                    self.refresh_finder(cx);
                }
            }
        }
        true
    }

    /// Recompute the finder's cached hits (after a query edit, or when the
    /// background walk has grown the index). Disjoint borrows of self.
    fn refresh_finder(&mut self, cx: &mut Context<Self>) {
        let index = &self.index;
        if let Some(f) = self.finder.as_mut() {
            f.recompute(index);
            cx.notify();
        }
    }

    fn on_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        // Esc dismisses the unsaved-changes modal (buttons handle save/discard)
        if self.confirm_close.is_some() && ks.key.as_str() == "escape" {
            self.confirm_cancel(cx);
            return;
        }
        // the finder owns the keyboard while open
        if self.finder_key(ks, window, cx) {
            return;
        }
        if m.control && !m.alt && !m.shift && ks.key.as_str() == "p" {
            self.open_finder(window, cx);
            return;
        }
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
        if m.control && !m.alt && !m.shift && ks.key.as_str() == "s" {
            self.save_focused(window, cx);
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
            if self.font_drag {
                self.set_font_from_x(f32::from(ev.position.x), cx);
                return;
            }
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

    fn on_mouse_up(&mut self, ev: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        let mut changed = self.drag_split.take().is_some();
        if self.font_drag {
            self.font_drag = false;
            changed = true;
        }
        // clear a stale drop-zone overlay left by a drag released over nothing
        if self.drop_target.take().is_some() {
            changed = true;
        }
        // tear-off: a pane released outside this window's content → new window
        let vp = window.viewport_size();
        let p = ev.position;
        let outside = p.x < px(0.) || p.y < px(0.) || p.x > vp.width || p.y > vp.height;
        self.end_pane_drag(outside, cx);
        if changed {
            cx.notify();
        }
    }

    fn bezel_btn(th: &theme::Theme, label: &str, active: bool) -> gpui::Div {
        let glint = BoxShadow {
            color: white().alpha(0.22),
            offset: point(px(1.), px(1.)),
            blur_radius: px(0.),
            spread_radius: px(0.),
            inset: true,
        };
        let seat = BoxShadow {
            color: hsla(0., 0., 0., 0.55),
            offset: point(px(2.), px(2.)),
            blur_radius: px(3.),
            spread_radius: px(0.),
            inset: false,
        };
        let b = div()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .text_size(px(10.5))
            .cursor_pointer();
        if active {
            // EXTRA-BOLD accent highlight + outer glow — the active tab pops
            let glow = BoxShadow {
                color: th.accent.alpha(0.45),
                offset: point(px(0.), px(0.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            };
            b.bg(linear_gradient(
                135.,
                linear_color_stop(th.accent.alpha(0.42), 0.),
                linear_color_stop(th.accent.alpha(0.12), 1.),
            ))
            .border_color(th.accent)
            .text_color(white().alpha(0.95))
            .font_weight(gpui::FontWeight::EXTRA_BOLD)
            .shadow(vec![glint, seat, glow])
            .child(label.to_string())
        } else {
            b.bg(linear_gradient(
                135.,
                linear_color_stop(brighten(th.frame_bg, 1.7), 0.),
                linear_color_stop(darken(th.frame_bg, 0.7), 1.),
            ))
            .border_color(th.frame_border.alpha(0.4))
            .text_color(th.frame_text)
            .shadow(vec![glint, seat])
            .child(label.to_string())
        }
    }

    /// A 13×11 glyph that DRAWS the split orientation (no font dependence):
    /// Row = two side-by-side cells, LEFT filled; Col = two stacked cells,
    /// BOTTOM filled. The filled box is what tells the two buttons apart.
    fn split_icon(th: &theme::Theme, dir: SplitDir) -> gpui::Div {
        let on = th.accent;
        let off = th.frame_faint.alpha(0.35);
        let divider = th.frame_text.alpha(0.8);
        let base = div()
            .w(px(13.))
            .h(px(11.))
            .flex_none()
            .overflow_hidden()
            .rounded_sm()
            .border_1()
            .border_color(divider)
            .flex();
        match dir {
            SplitDir::Row => base
                .flex_row()
                .child(div().flex_1().h_full().bg(on))
                .child(div().w(px(1.)).h_full().bg(divider))
                .child(div().flex_1().h_full().bg(off)),
            SplitDir::Col => base
                .flex_col()
                .child(div().w_full().flex_1().bg(off))
                .child(div().w_full().h(px(1.)).bg(divider))
                .child(div().w_full().flex_1().bg(on)),
        }
    }

    /// A bezel button that leads with a DRAWN icon instead of a glyph, so
    /// "split right" vs "split down" are unmistakable.
    fn split_btn(th: &theme::Theme, dir: SplitDir) -> gpui::Div {
        div()
            .px_2()
            .py_0p5()
            .rounded_sm()
            .border_1()
            .text_size(px(10.5))
            .cursor_pointer()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .bg(linear_gradient(
                135.,
                linear_color_stop(brighten(th.frame_bg, 1.7), 0.),
                linear_color_stop(darken(th.frame_bg, 0.7), 1.),
            ))
            .border_color(th.frame_border.alpha(0.4))
            .text_color(th.frame_text)
            .child(Self::split_icon(th, dir))
            .child("split")
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

/// Highlight the fuzzy-matched characters of `display` (indices into chars).
fn highlight_hit(display: &str, indices: &[u32], th: &theme::Theme) -> StyledText {
    let mut ranges = Vec::new();
    for &ci in indices {
        if let Some((b, c)) = display.char_indices().nth(ci as usize) {
            ranges.push((
                b..b + c.len_utf8(),
                HighlightStyle {
                    color: Some(th.accent),
                    ..Default::default()
                },
            ));
        }
    }
    StyledText::new(SharedString::from(display.to_string())).with_highlights(ranges)
}

/// The floating Ctrl+P palette: query box + ranked, highlighted, clickable rows.
fn finder_overlay(ui: &FinderState, th: &theme::Theme, cx: &mut Context<Workspace>) -> gpui::Div {
    let cursor = div().w(px(6.)).h(px(15.)).bg(th.accent);
    let query_row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_3()
        .py_2()
        .border_b_1()
        .border_color(th.frame_border.alpha(0.4))
        .text_size(px(13.))
        .text_color(th.text)
        .child(div().text_color(th.accent).child("⌕"))
        .child(SharedString::from(ui.query.clone()))
        .child(cursor);

    let status = if ui.indexing {
        format!("indexing… {} files", ui.total)
    } else if ui.hits.is_empty() {
        "no matches".to_string()
    } else {
        format!("{} files", ui.total)
    };

    let mut list = div().flex().flex_col().py_1();
    for (i, hit) in ui.hits.iter().enumerate() {
        let selected = i == ui.selected;
        // split "~/dir/sub/name.md" into dim path + bright filename
        let (dir, name) = match hit.display.rfind('/') {
            Some(p) => (&hit.display[..=p], &hit.display[p + 1..]),
            None => ("", hit.display.as_str()),
        };
        let name_indices: Vec<u32> = hit
            .indices
            .iter()
            .filter(|&&ci| ci as usize >= dir.chars().count())
            .map(|&ci| ci - dir.chars().count() as u32)
            .collect();
        let row = div()
            .flex()
            .flex_row()
            .items_baseline()
            .gap_0p5()
            .px_3()
            .py_0p5()
            .text_size(px(12.5))
            .when(selected, |d| d.bg(th.accent.alpha(0.18)))
            .child(
                div()
                    .text_color(th.text)
                    .child(highlight_hit(name, &name_indices, th)),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(th.frame_faint.alpha(0.55))
                    .child(SharedString::from(dir.to_string())),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                    ws.finder_open(i, window, cx)
                }),
            );
        list = list.child(row);
    }

    let panel = div()
        .w(px(560.))
        .max_h(px(420.))
        .overflow_hidden()
        .rounded(px(8.))
        .border_1()
        .border_color(th.frame_border.alpha(0.8))
        .bg(darken(th.bg, 0.75))
        .shadow(
            vec![BoxShadow {
                color: th.accent.alpha(0.18 * th.glow),
                offset: point(px(0.), px(4.)),
                blur_radius: px(24.),
                spread_radius: px(1.),
                inset: false,
            }]
            .into(),
        )
        .child(query_row)
        .child(list)
        .child(
            div()
                .px_3()
                .py_1()
                .border_t_1()
                .border_color(th.frame_border.alpha(0.3))
                .text_size(px(10.))
                .text_color(th.frame_faint.alpha(0.6))
                .child(SharedString::from(format!("{status} · ↑↓ select · ⏎ open · esc cancel"))),
        );

    // dim scrim over the pane + top-centered panel
    div()
        .absolute()
        .inset_0()
        .flex()
        .flex_col()
        .items_center()
        .pt(px(40.))
        .bg(hsla(0., 0., 0., 0.35))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                ws.finder = None;
                ws.focus_active(window, cx);
                cx.notify();
            }),
        )
        .child(panel)
}

/// The accent half-pane highlight shown while a pane is dragged over this one.
fn drop_overlay(zone: DropZone, th: &theme::Theme) -> gpui::Div {
    let base = div()
        .absolute()
        .bg(th.accent.alpha(0.22))
        .border_2()
        .border_color(th.accent);
    match zone {
        DropZone::Left => base.top_0().left_0().h_full().w(gpui::relative(0.5)),
        DropZone::Right => base.top_0().right_0().h_full().w(gpui::relative(0.5)),
        DropZone::Top => base.top_0().left_0().w_full().h(gpui::relative(0.5)),
        DropZone::Bottom => base.bottom_0().left_0().w_full().h(gpui::relative(0.5)),
    }
}

/// The themed "unsaved changes" modal shown before a destructive close. Styled
/// to the theme of the tab/pane being closed.
fn confirm_overlay(
    req: CloseRequest,
    th: &theme::Theme,
    names: &[String],
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let what = match req {
        CloseRequest::Tab(_) => "this tab",
        CloseRequest::Pane(_) => "this pane",
    };
    let list = if names.is_empty() {
        "unsaved changes".to_string()
    } else {
        names.join(" · ")
    };
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(hsla(0., 0., 0., 0.55))
        // click the scrim to cancel
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|ws, _: &MouseDownEvent, _w, cx| ws.confirm_cancel(cx)),
        )
        .child(
            div()
                .w(px(440.))
                .rounded(px(10.))
                .border_2()
                .border_color(th.accent)
                .bg(darken(th.bg, 0.7))
                .shadow(
                    vec![BoxShadow {
                        color: th.accent.alpha(0.35),
                        offset: point(px(0.), px(6.)),
                        blur_radius: px(34.),
                        spread_radius: px(2.),
                        inset: false,
                    }]
                    .into(),
                )
                .p(px(18.))
                .flex()
                .flex_col()
                .gap_3()
                // clicks inside the card must NOT fall through to the scrim
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()),
                )
                .child(
                    div()
                        .text_size(px(15.))
                        .text_color(th.accent)
                        .child("● Unsaved changes"),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(th.text.alpha(0.85))
                        .child(SharedString::from(format!(
                            "Closing {what} will discard unsaved edits in:"
                        ))),
                )
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .rounded_sm()
                        .bg(darken(th.bg, 0.5))
                        .border_1()
                        .border_color(th.frame_border.alpha(0.4))
                        .text_size(px(12.5))
                        .text_color(th.text)
                        .child(SharedString::from(list)),
                )
                .child(
                    div()
                        .mt_1()
                        .flex()
                        .flex_row()
                        .gap_2()
                        .justify_end()
                        .child(
                            Workspace::bezel_btn(th, "Save & Close", true).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    ws.confirm_save_and_close(window, cx);
                                }),
                            ),
                        )
                        .child(
                            Workspace::bezel_btn(th, "Close without saving", false).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    ws.confirm_discard_and_close(window, cx);
                                }),
                            ),
                        )
                        .child(
                            Workspace::bezel_btn(th, "Cancel", false).on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _: &MouseDownEvent, _w, cx| {
                                    cx.stop_propagation();
                                    ws.confirm_cancel(cx);
                                }),
                            ),
                        ),
                ),
        )
}

/// One THEME-row button: a glyph over the theme name, accent-lit when active.
fn theme_icon_btn(th: &theme::Theme, glyph: &str, caption: &str, active: bool) -> gpui::Div {
    let inner = div()
        .flex()
        .flex_col()
        .items_center()
        .gap_0()
        .child(div().text_size(px(15.)).child(glyph.to_string()))
        .child(div().text_size(px(8.)).child(caption.to_string()));
    let b = div()
        .w(px(54.))
        .h(px(40.))
        .flex()
        .items_center()
        .justify_center()
        .rounded_md()
        .border_1()
        .cursor_pointer();
    if active {
        b.bg(linear_gradient(
            135.,
            linear_color_stop(th.accent.alpha(0.45), 0.),
            linear_color_stop(th.accent.alpha(0.15), 1.),
        ))
        .border_color(th.accent)
        .text_color(white().alpha(0.95))
        .child(inner)
    } else {
        b.bg(darken(th.frame_bg, 0.8))
            .border_color(th.accent.alpha(0.35))
            .text_color(th.frame_text)
            .child(inner)
    }
}

/// One SEED-COLOUR swatch. `None` = the theme's own colours (rainbow chip).
fn seed_swatch(color: Option<Hsla>, active: bool) -> gpui::Div {
    let b = div().w(px(20.)).h(px(20.)).rounded_full().cursor_pointer().border_2();
    let b = match color {
        Some(c) => b.bg(c),
        None => b.bg(linear_gradient(
            135.,
            linear_color_stop(hsla(0.0, 0.9, 0.6, 1.0), 0.),
            linear_color_stop(hsla(0.75, 0.9, 0.6, 1.0), 1.),
        )),
    };
    if active {
        b.border_color(white().alpha(0.92))
    } else {
        b.border_color(hsla(0., 0., 0., 0.45))
    }
}

/// The theme tray popover (ported from terminal-delight): a THEME row of icon
/// buttons + a SEED COLOUR row of swatches, anchored at the click. A full-screen
/// scrim closes it. `is_pane` adds a "follow outer" reset.
#[allow(clippy::too_many_arguments)]
fn theme_menu_overlay(
    is_pane: bool,
    cur: (String, Option<Hsla>),
    at: Option<gpui::Point<Pixels>>,
    th: &theme::Theme,
    themes: &[(String, &'static str)],
    has_override: bool,
    window: &Window,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    let (cur_name, cur_seed) = cur;

    // ---- THEME row ----
    let mut theme_row = div().flex().flex_row().gap_2();
    for (name, glyph) in themes {
        let active = *name == cur_name;
        let pick = name.clone();
        theme_row = theme_row.child(theme_icon_btn(th, glyph, name, active).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |ws, _: &MouseDownEvent, _w, cx| {
                cx.stop_propagation();
                ws.set_menu_theme(pick.clone(), cx);
            }),
        ));
    }

    // ---- SEED COLOUR row: theme default (None) + presets ----
    let mut seed_row = div().flex().flex_row().items_center().gap_2();
    seed_row = seed_row.child(seed_swatch(None, cur_seed.is_none()).on_mouse_down(
        MouseButton::Left,
        cx.listener(|ws, _: &MouseDownEvent, _w, cx| {
            cx.stop_propagation();
            ws.set_menu_seed(None, cx);
        }),
    ));
    for &hex in SEED_SWATCHES {
        let color = theme::parse_hex(hex);
        let active = cur_seed.is_some() && color.map(|c| c.h) == cur_seed.map(|c| c.h);
        seed_row = seed_row.child(seed_swatch(color, active).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |ws, _: &MouseDownEvent, _w, cx| {
                cx.stop_propagation();
                ws.set_menu_seed(color, cx);
            }),
        ));
    }

    let label = |s: &str| {
        div()
            .text_size(px(9.))
            .text_color(th.text.alpha(0.55))
            .child(s.to_string())
    };

    const PANEL_W: f32 = 286.;
    const PANEL_H_EST: f32 = 188.;
    let mut panel = div().absolute().w(px(PANEL_W));
    panel = match at {
        Some(p) => {
            let vp = window.viewport_size();
            let (vw, vh) = (f32::from(vp.width), f32::from(vp.height));
            let right = (vw - f32::from(p.x)).clamp(8., (vw - PANEL_W - 8.).max(8.));
            let top = (f32::from(p.y) + 6.).clamp(8., (vh - PANEL_H_EST - 8.).max(8.));
            panel.right(px(right)).top(px(top))
        }
        None => panel.top(px(38.)).right(px(14.)),
    };

    panel = panel
        .p_3()
        .rounded_md()
        .border_1()
        .border_color(th.accent.alpha(0.55))
        .bg(darken(th.frame_bg, 0.6))
        .shadow(
            vec![BoxShadow {
                color: hsla(0., 0., 0., 0.6),
                offset: point(px(4.), px(6.)),
                blur_radius: px(18.),
                spread_radius: px(0.),
                inset: false,
            }]
            .into(),
        )
        .flex()
        .flex_col()
        .gap_2()
        .text_size(px(10.))
        .text_color(th.text)
        // clicks inside the tray must not fall through to the closing scrim
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()),
        )
        .child(label(if is_pane { "THEME — THIS PANE" } else { "THEME — OUTER" }))
        .child(theme_row)
        .child(label("SEED COLOUR"))
        .child(seed_row);

    if is_pane {
        panel = panel.child(
            Workspace::bezel_btn(th, "follow outer", !has_override).on_mouse_down(
                MouseButton::Left,
                cx.listener(|ws, _: &MouseDownEvent, _w, cx| {
                    cx.stop_propagation();
                    ws.clear_pane_override(cx);
                }),
            ),
        );
    }

    div()
        .absolute()
        .inset_0()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|ws, _: &MouseDownEvent, _w, cx| ws.close_theme_menu(cx)),
        )
        .child(panel)
}

fn render_node(
    node: &Node,
    th: &theme::Theme,
    focused: Option<EntityId>,
    dragging: Option<u64>,
    registry: &Arc<Mutex<std::collections::HashMap<u64, Bounds<Pixels>>>>,
    finder: Option<&FinderState>,
    drop_target: Option<(EntityId, DropZone)>,
    cx: &mut Context<Workspace>,
) -> gpui::Div {
    match node {
        Node::Leaf(e) => {
            let pane_id = e.entity_id();
            let is_focused = focused == Some(pane_id);
            // each pane renders in its own (optional) theme; chrome follows it
            let pth = e.read(cx).effective_theme(cx);
            let editing = e.read(cx).is_editing();
            let title = e.read(cx).title(cx);
            let theme_name = e.read(cx).theme_name(cx);
            let status = e.read(cx).status_str(cx);

            // ---- header: drag handle + theme chip + pop-out ----
            let ghost_label = title.clone();
            let ghost_accent = pth.accent;
            let ghost_frame = pth.frame_bg;
            let header = div()
                .id(SharedString::from(format!("pane-header-{pane_id:?}")))
                .h(px(24.))
                .flex_none()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_2()
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(brighten(pth.frame_bg, 1.9), 0.),
                    linear_color_stop(pth.frame_bg, 1.),
                ))
                .border_b_1()
                .border_color(pth.frame_border.alpha(0.5))
                .text_size(px(11.))
                .text_color(pth.frame_text)
                .cursor_pointer()
                // grab the header to drag the whole pane (split / tab / tear-off)
                .on_drag(
                    DraggedPane {
                        pane: e.clone(),
                        source: cx.entity(),
                        pane_id,
                    },
                    move |d, _off, _w, cx| {
                        // mirror the drag app-globally so a release on the desktop
                        // (outside any window) can tear the pane into a new window
                        cx.set_global(ActivePaneDrag {
                            pane: d.pane.clone(),
                            source: d.source.clone(),
                            pane_id: d.pane_id,
                            consumed: false,
                        });
                        cx.new(|_| DragGhost {
                            label: ghost_label.clone(),
                            accent: ghost_accent,
                            frame_bg: ghost_frame,
                        })
                    },
                )
                .child(SharedString::from(format!(
                    "▸ {} · {}",
                    if editing { "SRC" } else { "DOC" },
                    title
                )))
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        // theme chip — click opens the per-pane theme tray at the
                        // cursor; right-click is a quick "follow outer" reset.
                        .child(
                            div()
                                .cursor_pointer()
                                .text_color(pth.frame_faint)
                                .hover(|s| s.text_color(pth.accent))
                                .child(SharedString::from(format!("◧ {theme_name}")))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                                        cx.stop_propagation();
                                        ws.open_theme_menu(
                                            MenuScope::Pane(pane_id),
                                            Some(ev.position),
                                            cx,
                                        );
                                    }),
                                )
                                .on_mouse_down(
                                    MouseButton::Right,
                                    cx.listener(move |ws, _: &MouseDownEvent, _w, cx| {
                                        cx.stop_propagation();
                                        ws.theme_menu = Some(MenuScope::Pane(pane_id));
                                        ws.clear_pane_override(cx);
                                        ws.theme_menu = None;
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .text_color(pth.frame_faint.alpha(0.7))
                                .child(SharedString::from(status)),
                        )
                        // pop-out ⧉ — tear this pane into a new window
                        .child(
                            div()
                                .cursor_pointer()
                                .text_color(pth.frame_faint)
                                .hover(|s| s.text_color(pth.accent))
                                .child("⧉")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |ws, _: &MouseDownEvent, _w, cx| {
                                        cx.stop_propagation();
                                        ws.pop_out_pane(pane_id, cx);
                                    }),
                                ),
                        )
                        // close ✕ — guarded by the unsaved-changes modal
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_center()
                                .w(px(16.))
                                .h(px(16.))
                                .rounded_sm()
                                .cursor_pointer()
                                .text_size(px(13.))
                                .text_color(pth.frame_faint.alpha(0.8))
                                .hover(|s| {
                                    s.bg(hsla(0.0, 0.75, 0.55, 0.9)).text_color(white())
                                })
                                .child("✕")
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                                        cx.stop_propagation();
                                        ws.request_close_pane(pane_id, window, cx);
                                    }),
                                ),
                        ),
                );

            // ---- the leaf: per-pane border + glow wraps header + body ----
            let mut leaf = div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .relative()
                .overflow_hidden()
                .rounded_md()
                .border_1()
                .border_color(if is_focused {
                    pth.frame_border.alpha(0.7)
                } else {
                    pth.frame_border.alpha(0.25)
                })
                // edge phosphor glow — wraps all 4 sides (offset 0 inset radiates
                // inward uniformly). pth.glow is 0 outside glow themes.
                .shadow(
                    vec![BoxShadow {
                        color: pth.accent.alpha((if is_focused { 0.22 } else { 0.11 }) * pth.glow),
                        offset: point(px(0.), px(0.)),
                        blur_radius: px(11.),
                        spread_radius: px(0.),
                        inset: true,
                    }]
                    .into(),
                )
                .flex()
                .flex_col()
                .child(header)
                .child(div().flex_1().min_h_0().child(e.clone()))
                // dropping a pane ANYWHERE on this one splits by the nearest edge
                .on_drag_move::<DraggedPane>(cx.listener(
                    move |ws, ev: &gpui::DragMoveEvent<DraggedPane>, _w, cx| {
                        let b = ev.bounds;
                        let x = f32::from(ev.event.position.x) - f32::from(b.origin.x);
                        let y = f32::from(ev.event.position.y) - f32::from(b.origin.y);
                        let zone =
                            nearest_zone(x, y, f32::from(b.size.width), f32::from(b.size.height));
                        if ws.drop_target != Some((pane_id, zone)) {
                            ws.drop_target = Some((pane_id, zone));
                            cx.notify();
                        }
                    },
                ))
                .on_drop::<DraggedPane>(cx.listener(
                    move |ws, d: &DraggedPane, window, cx| {
                        ws.accept_pane_drop(d, pane_id, window, cx);
                    },
                ));

            // drop-zone overlay (only on the pane currently hovered)
            if let Some((tid, zone)) = drop_target {
                if tid == pane_id {
                    leaf = leaf.child(drop_overlay(zone, &pth));
                }
            }
            // the finder floats inside the pane it targets
            if let Some(ui) = finder.filter(|u| u.target == pane_id) {
                leaf = leaf.child(finder_overlay(ui, &pth, cx));
            }
            leaf
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
                .child(render_node(a, th, focused, dragging, registry, finder, drop_target, cx));
            let first = match dir {
                SplitDir::Row => first.h_full().w(gpui::relative(*ratio)),
                SplitDir::Col => first.w_full().h(gpui::relative(*ratio)),
            };
            let second = div()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .flex()
                .child(render_node(b, th, focused, dragging, registry, finder, drop_target, cx));

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
        if FIRST_FRAME.swap(false, Ordering::Relaxed) {
            if let Some(t0) = START.get() {
                stamp(*t0, "FIRST FRAME painted (interactive)");
            }
        }
        self.reap(window, cx);
        warp::begin_frame(); // visible panes re-register their tube rects below
        // flatten the glass while an overlay is up so its hit-testing is honest
        // (the warp is a post-process; a panel over a bent tube would bow with it)
        warp::set_suppressed(self.theme_menu.is_some() || self.confirm_close.is_some());
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
        // dirty/status reflect the ACTIVE tab's document, not the initial one
        let dirty = self
            .tabs
            .get(self.active)
            .map(|t| t.doc.read(cx).editor.dirty)
            .unwrap_or(false);
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
            let tab_accent = th.accent;
            // a dot when the tab holds unsaved edits — at-a-glance for notebooks
            let tab_dirty = self.tab_is_dirty(i, cx);
            let dot_color = th.accent;
            let x_color = if is_active {
                white().alpha(0.85)
            } else {
                th.frame_faint.alpha(0.7)
            };
            tab_strip = tab_strip.child(
                Self::bezel_btn(&th, &label, is_active)
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_3()
                    // drop a dragged pane here → move it into THIS tab
                    .drag_over::<DraggedPane>(move |s, _d: &DraggedPane, _w, _cx| {
                        s.border_color(tab_accent)
                    })
                    .on_drop::<DraggedPane>(cx.listener(
                        move |ws, d: &DraggedPane, window, cx| ws.move_pane_to_tab(d, i, window, cx),
                    ))
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
                    )
                    // unsaved-edits dot (notebooks / modified files)
                    .when(tab_dirty, |d| {
                        d.child(
                            div()
                                .text_size(px(9.))
                                .text_color(dot_color)
                                .child("●"),
                        )
                    })
                    // the tab's big, distinct ✕ — a circular danger badge that
                    // lights red on hover. stop_propagation so it never activates.
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(18.))
                            .h(px(18.))
                            .rounded_full()
                            .border_1()
                            .border_color(x_color.alpha(0.45))
                            .text_size(px(13.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .text_color(x_color)
                            .cursor_pointer()
                            .hover(move |s| {
                                s.bg(hsla(0.0, 0.78, 0.55, 0.95))
                                    .border_color(hsla(0.0, 0.78, 0.65, 1.0))
                                    .text_color(white())
                            })
                            .child("✕")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |ws, _: &MouseDownEvent, window, cx| {
                                    cx.stop_propagation();
                                    ws.request_close_tab(i, window, cx);
                                }),
                            ),
                    ),
            );
        }
        let plus_accent = th.accent;
        tab_strip = tab_strip.child(
            Self::bezel_btn(&th, "+", false)
                // drop a dragged pane on `+` → move it into a brand-new tab
                .drag_over::<DraggedPane>(move |s, _d: &DraggedPane, _w, _cx| {
                    s.border_color(plus_accent)
                })
                .on_drop::<DraggedPane>(cx.listener(
                    |ws, d: &DraggedPane, window, cx| ws.new_tab_with(d, window, cx),
                ))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|ws, _: &MouseDownEvent, window, cx| ws.new_tab(window, cx)),
                ),
        );

        // ---- live text-zoom scrubber (small A ⟶ big A · %) ----
        let scale = theme::font_scale();
        let pct = ((scale - theme::FONT_SCALE_MIN)
            / (theme::FONT_SCALE_MAX - theme::FONT_SCALE_MIN))
            .clamp(0., 1.);
        let track_w = 66.0_f32;
        let knob = 10.0_f32;
        let font_store = self.font_track.clone();
        let font_scrubber = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .child(div().text_size(px(8.5)).text_color(th.frame_faint).child("A"))
            .child(
                div()
                    .id("font-track")
                    .relative()
                    .w(px(track_w))
                    .h(px(12.))
                    .cursor_pointer()
                    // capture the painted bounds so a drag maps x → scale
                    .child(div().absolute().inset_0().child(
                        canvas(
                            move |bounds, _, _| {
                                *font_store.lock().unwrap() = Some(bounds);
                            },
                            |_, _, _, _| {},
                        )
                        .size_full(),
                    ))
                    // groove
                    .child(
                        div()
                            .absolute()
                            .top(px(5.))
                            .left_0()
                            .right_0()
                            .h(px(2.))
                            .rounded_full()
                            .bg(th.frame_faint.alpha(0.4)),
                    )
                    // fill
                    .child(
                        div()
                            .absolute()
                            .top(px(5.))
                            .left_0()
                            .w(px(pct * track_w))
                            .h(px(2.))
                            .rounded_full()
                            .bg(th.accent),
                    )
                    // knob
                    .child(
                        div()
                            .absolute()
                            .top(px(1.))
                            .left(px(pct * (track_w - knob)))
                            .w(px(knob))
                            .h(px(knob))
                            .rounded_full()
                            .bg(th.accent)
                            .border_1()
                            .border_color(brighten(th.frame_bg, 1.8)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |ws, ev: &MouseDownEvent, _w, cx| {
                            ws.font_drag = true;
                            ws.set_font_from_x(f32::from(ev.position.x), cx);
                        }),
                    ),
            )
            .child(div().text_size(px(13.)).text_color(th.frame_faint).child("A"))
            .child(
                div()
                    .text_size(px(9.))
                    .text_color(th.accent)
                    .child(SharedString::from(format!("{}%", (scale * 100.).round() as i32))),
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
                    .child(Self::bezel_btn(&th, "⌕ open", false).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                            ws.open_finder(window, cx)
                        }),
                    ))
                    .child(tab_strip),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(font_scrubber)
                    // outer (window-wide) theme tray
                    .child(
                        Self::bezel_btn(&th, "◧ theme", self.theme_menu == Some(MenuScope::Outer))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|ws, _: &MouseDownEvent, _w, cx| {
                                    cx.stop_propagation();
                                    ws.open_theme_menu(MenuScope::Outer, None, cx);
                                }),
                            ),
                    )
                    .child(Self::split_btn(&th, SplitDir::Row).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|ws, _: &MouseDownEvent, window, cx| {
                            ws.split(SplitDir::Row, window, cx)
                        }),
                    ))
                    .child(Self::split_btn(&th, SplitDir::Col).on_mouse_down(
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

        // finder hits are cached; refresh only if the background walk grew the
        // index while the finder is open (cheap len check, not a fuzzy scan)
        if self
            .finder
            .as_ref()
            .is_some_and(|f| f.indexing && f.total != self.index.total())
        {
            self.refresh_finder(cx);
        }
        // re-borrow tab after the &mut self refresh above
        let tab = &self.tabs[self.active];
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
                self.finder.as_ref(),
                self.drop_target,
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

        // themed "unsaved changes" modal for the pending close (if any)
        let confirm = self.confirm_close.map(|req| {
            let (cth, names) = self.confirm_context(req, cx);
            (req, cth, names)
        });

        // the theme tray (outer or per-pane), themed to its scope
        let theme_tray = self.theme_menu.map(|scope| {
            let is_pane = matches!(scope, MenuScope::Pane(_));
            let tray_th = match scope {
                MenuScope::Pane(id) => self
                    .find_pane(id)
                    .map(|p| (*p.read(cx).effective_theme(cx)).clone())
                    .unwrap_or_else(|| (*th).clone()),
                MenuScope::Outer => (*th).clone(),
            };
            let has_override = match scope {
                MenuScope::Pane(id) => self
                    .find_pane(id)
                    .map(|p| {
                        let p = p.read(cx);
                        p.theme.is_some() || p.seed.is_some()
                    })
                    .unwrap_or(false),
                MenuScope::Outer => false,
            };
            let cur = self.menu_choice(cx);
            let themes = theme::registry_list(cx);
            let at = self.menu_at;
            theme_menu_overlay(is_pane, cur, at, &tray_th, &themes, has_override, window, cx)
        });

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(Self::on_key))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .size_full()
            .relative()
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
            .children(
                confirm.map(|(req, cth, names)| confirm_overlay(req, &cth, &names, cx)),
            )
            .children(theme_tray)
    }
}

/// Read one file into `(label, path, text)`, degrading gracefully to an
/// error buffer if it can't be read. Shared by initial load and forwarded opens.
fn read_file(path: &PathBuf) -> (String, Option<PathBuf>, String) {
    let disp = path.to_string_lossy().to_string();
    match fs::read_to_string(path) {
        Ok(text) => (disp, Some(path.clone()), text),
        Err(e) => (
            format!("{disp} (error)"),
            None,
            format!("could not read {disp}:\n{e}"),
        ),
    }
}

fn load() -> (String, Option<PathBuf>, String) {
    match env::args().nth(1) {
        Some(path) => read_file(&PathBuf::from(path)),
        None => ("sample.md".to_string(), None, SAMPLE.to_string()),
    }
}

/// Set MD_TIMING=1 to print startup milestones to stderr — tells us exactly
/// where the cold-start seconds go (window create vs. first GPU frame).
fn stamp(t0: Instant, what: &str) {
    if std::env::var_os("MD_TIMING").is_some() {
        eprintln!("[timing] {:>7.0}ms  {what}", t0.elapsed().as_secs_f64() * 1000.);
    }
}

/// Open one editor window for `(label, path, text)`. Shared by the first launch
/// and by every forwarded open from a sibling process (see `ipc`).
fn open_doc_window(label: String, path: Option<PathBuf>, text: String, cx: &mut App) {
    let bounds = gpui::Bounds::centered(None, size(px(1180.), px(800.)), cx);
    let title: SharedString = format!("{label} — markdown-delight").into();
    let doc = cx.new(|_| Doc::new(label, path, text));
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
}

/// Handle a launch request forwarded from a sibling process: open the file(s)
/// it carried (or, for a bare click with none, just raise the existing window),
/// then pull our window to the front so the click feels instant.
fn handle_forwarded(req: ipc::OpenRequest, cx: &mut App) {
    for path in req {
        let (label, p, text) = read_file(&path);
        open_doc_window(label, p, text, cx);
    }
    // A bare request with no windows yet (rare: every window was closed but the
    // process lingered) still deserves a fresh blank one.
    if cx.windows().is_empty() {
        let (label, path, text) = ("sample.md".to_string(), None, SAMPLE.to_string());
        open_doc_window(label, path, text, cx);
    }
    cx.activate(true);
}

fn main() {
    let t0 = Instant::now();
    let _ = START.set(t0);

    // Single-instance: if a primary is already up, hand it our file and bail
    // BEFORE touching the GPU — that is what makes a second click snap open.
    let forward: Vec<PathBuf> = env::args().skip(1).map(PathBuf::from).collect();
    if ipc::try_forward(&forward) {
        return;
    }
    // We are the primary. Listen for siblings' forwarded opens.
    let server = ipc::start_server();

    let (label, path, text) = load();
    stamp(t0, "file loaded");
    application().run(move |cx: &mut App| {
        stamp(t0, "app callback (gpu/window subsystem up)");
        theme::init(cx);
        open_doc_window(label, path, text, cx);
        stamp(t0, "window opened (pre first frame)");
        cx.activate(true);

        // Drain forwarded launch requests on the main loop. A cheap timer poll
        // (mpsc has no async recv) — invisible at 80ms, no extra dependency.
        if let Some(rx) = server {
            cx.spawn(async move |cx| loop {
                cx.background_executor()
                    .timer(Duration::from_millis(80))
                    .await;
                let mut reqs: Vec<ipc::OpenRequest> = Vec::new();
                while let Ok(req) = rx.try_recv() {
                    reqs.push(req);
                }
                if reqs.is_empty() {
                    continue;
                }
                cx.update(|cx| {
                    for req in reqs {
                        handle_forwarded(req, cx);
                    }
                });
            })
            .detach();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{nearest_zone, slugify, truncate_label, DropZone};

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("# Hello World"), "hello-world");
        assert_eq!(slugify("  Multiple   spaces!  "), "multiple-spaces");
        assert_eq!(slugify("path/to\\thing"), "path-to-thing");
    }

    #[test]
    fn slugify_empty_and_symbols() {
        assert_eq!(slugify(""), "");
        assert_eq!(slugify("###"), "");
        assert_eq!(slugify("!!!"), "");
        // collapses runs of separators, trims leading/trailing dashes
        assert_eq!(slugify("--a -- b--"), "a-b");
    }

    #[test]
    fn slugify_caps_length() {
        let long = "a".repeat(100);
        assert!(slugify(&long).chars().count() <= 40);
    }

    #[test]
    fn truncate_label_keeps_short() {
        assert_eq!(truncate_label("hi", 40), "hi");
        assert_eq!(truncate_label("exactly-ten", 11), "exactly-ten");
    }

    #[test]
    fn truncate_label_clamps_with_ellipsis() {
        let out = truncate_label("0123456789", 5);
        assert_eq!(out.chars().count(), 5);
        assert!(out.ends_with('…'));
        assert_eq!(out, "0123…");
    }

    #[test]
    fn truncate_label_is_char_safe() {
        // multibyte chars must not be split mid-byte
        let out = truncate_label("ααααααααα", 4); // 9 alphas
        assert_eq!(out.chars().count(), 4);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn nearest_zone_picks_closest_edge() {
        assert_eq!(nearest_zone(2.0, 50.0, 100.0, 100.0), DropZone::Left);
        assert_eq!(nearest_zone(98.0, 50.0, 100.0, 100.0), DropZone::Right);
        assert_eq!(nearest_zone(50.0, 2.0, 100.0, 100.0), DropZone::Top);
        assert_eq!(nearest_zone(50.0, 98.0, 100.0, 100.0), DropZone::Bottom);
    }
}
