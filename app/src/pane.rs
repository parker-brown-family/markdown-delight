//! pane.rs — one sub-monitor: a view onto a shared Doc (source or preview).
//!
//! Panes in a tab share ONE Doc entity: edit in a source pane and every
//! preview pane of the same document re-renders live (cx.observe). Each pane
//! keeps its own mode, focus, and crt::Fx — its own desynced tube. Default
//! mode for the first pane is SOURCE: right-click → open → start typing.

use std::{
    collections::{BTreeSet, HashMap},
    ops::Range,
    path::PathBuf,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use gpui::{
    canvas, div, hsla, linear_color_stop, linear_gradient, point, prelude::*, px, white,
    AnyElement, Bounds, BoxShadow, ClipboardItem, Context, Entity, FocusHandle, Focusable,
    FontWeight, HighlightStyle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, ScrollHandle, SharedString, StyledText, TextLayout, Window,
};

use crate::{appearance, comment_ui, comments, crt, editor, render, theme, warp};

const LINE_H: f32 = 21.;
const PAD_X: f32 = 16.;
const PAD_Y: f32 = 12.;
/// JetBrains Mono advance width at 13px — good enough for click→column
const CHAR_W: f32 = 7.8;

/* ================= the shared document ================= */

pub struct Doc {
    pub editor: editor::Editor,
    pub blocks: Vec<render::Block>,
    /// per-block anchor metadata (fingerprint + plain text), kept in lockstep
    /// with `blocks` so comment threads can re-anchor after edits
    pub meta: Vec<comments::BlockMeta>,
    pub path: Option<PathBuf>,
    pub label: SharedString,
    /// review comments — loaded lazily on first entry into comment mode so docs
    /// that are never reviewed pay nothing
    pub comments: comments::CommentStore,
    comments_loaded: bool,
}

impl Doc {
    pub fn new(label: String, path: Option<PathBuf>, text: String) -> Self {
        let (blocks, meta) = render::parse_with_meta(&text);
        Self {
            editor: editor::Editor::new(&text),
            blocks,
            meta,
            path,
            label: label.into(),
            comments: comments::CommentStore::default(),
            comments_loaded: false,
        }
    }

    pub fn reparse(&mut self) {
        let (blocks, meta) = render::parse_with_meta(&self.editor.text());
        self.blocks = blocks;
        self.meta = meta;
        // edits may have moved/removed commented text — re-anchor & save
        if self.comments_loaded {
            self.comments.reanchor(&self.meta);
            self.save_comments();
        }
    }

    /// Stable per-doc key for the comment store (canonical path, or scratch id).
    fn comment_key(&self) -> String {
        comments::doc_key(self.path.as_deref(), self.label.as_ref())
    }

    /// Load this doc's comments once (and re-anchor them to the current blocks).
    pub fn ensure_comments_loaded(&mut self) {
        if !self.comments_loaded {
            self.comments = comments::CommentStore::load(&self.comment_key());
            self.comments.reanchor(&self.meta);
            self.comments_loaded = true;
        }
    }

    pub fn save_comments(&self) {
        self.comments.save(&self.comment_key());
    }
}

/* ================= the pane ================= */

#[derive(PartialEq, Clone, Copy)]
pub enum Mode {
    Preview,
    Source,
    /// read-only review surface: click/select a block to comment on it
    Comment,
}

/// An in-progress drag-selection inside one paragraph block (byte offsets into
/// that block's rendered text).
#[derive(Clone)]
struct Sel {
    block: usize,
    anchor: usize,
    head: usize,
}

impl Sel {
    /// normalized (start, end) byte range
    fn range(&self) -> (usize, usize) {
        (self.anchor.min(self.head), self.anchor.max(self.head))
    }
}

/// Transient state for the open comment "device" panel.
struct CommentUi {
    /// what we're commenting on
    anchor: comments::Anchor,
    /// existing thread id, if this block/range already has one
    thread_id: Option<String>,
    /// short label for the titlebar (e.g. "paragraph", "heading")
    kind: &'static str,
    /// draft text in the composer
    composer: String,
}

pub struct MdPane {
    pub doc: Entity<Doc>,
    pub mode: Mode,
    /// mode to restore when leaving comment mode
    prev_mode: Mode,
    /// open comment panel (None = closed)
    comment_ui: Option<CommentUi>,
    /// per-paragraph text layouts captured each render, for drag-select hit-test
    block_layouts: HashMap<usize, TextLayout>,
    /// active/just-finished paragraph drag-selection (comment mode)
    sel: Option<Sel>,
    sel_dragging: bool,
    /// a left-button text drag is in progress in the source editor
    text_dragging: bool,
    /// the "all comments" browser overlay is open
    show_browser: bool,
    /// the keys-&-tips help modal is open (F1 / Ctrl+/)
    show_help: bool,
    /// transient confirmation pill (e.g. after "copy with comments"); cleared by
    /// the fx clock after `toast_ticks` frames
    toast: Option<SharedString>,
    toast_ticks: u16,
    /// comment author for this session (git user.name → $USER → anon)
    author: String,
    pub closed: bool,
    /// Per-pane display configuration: four independently-inheriting groups
    /// (colour / texture / grade / curve), each with a "follow outer" toggle.
    /// New/split panes inherit this from their origin; dragged panes carry it.
    pub appearance: appearance::PaneAppearance,
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
        appearance: appearance::PaneAppearance,
        cx: &mut Context<Self>,
    ) -> Self {
        // live preview: repaint when the shared doc changes
        let doc_sub = cx.observe(&doc, |_, _, cx| cx.notify());
        // fx clock — only notifies when something visibly moved
        cx.spawn(async move |this, cx| loop {
            cx.background_executor()
                .timer(Duration::from_millis(33))
                .await;
            if !this
                .update(cx, |pane: &mut MdPane, cx| {
                    if pane.closed {
                        return false;
                    }
                    let th = pane.effective_theme(cx);
                    if pane.fx.tick(&th) {
                        cx.notify();
                    }
                    // age out a transient toast
                    if pane.toast_ticks > 0 {
                        pane.toast_ticks -= 1;
                        if pane.toast_ticks == 0 {
                            pane.toast = None;
                        }
                        cx.notify();
                    }
                    true
                })
                .unwrap_or(false)
            {
                break;
            }
        })
        .detach();
        Self {
            doc,
            mode,
            prev_mode: Mode::Preview,
            comment_ui: None,
            block_layouts: HashMap::new(),
            sel: None,
            sel_dragging: false,
            text_dragging: false,
            show_browser: false,
            show_help: false,
            toast: None,
            toast_ticks: 0,
            author: comments::default_author(),
            closed: false,
            appearance,
            focus_handle: cx.focus_handle(),
            fx: crt::Fx::new(seed),
            scroll: ScrollHandle::new(),
            tube_bounds: Arc::new(Mutex::new(None)),
            doc_sub,
        }
    }

    /// This pane's appearance, resolved against the workspace outer look —
    /// every group (colour / texture / grade / curve) made concrete.
    pub fn resolved(&self, cx: &gpui::App) -> appearance::Resolved {
        self.appearance.effective(&appearance::outer(cx))
    }

    /// The theme this pane renders with: its four display-config groups composed
    /// into one `Theme` (colour + seed, texture's CRT dials, curve gate, grade
    /// baked into the colours).
    pub fn effective_theme(&self, cx: &gpui::App) -> Arc<theme::Theme> {
        Arc::new(appearance::compose(cx, &self.resolved(cx)))
    }

    /// Effective text scale for this pane: the workspace scrubber × this pane's
    /// grade text-size. Used everywhere `theme::font_scale()` was, so per-pane
    /// zoom stays consistent with click→cursor math.
    pub fn eff_scale(&self, cx: &gpui::App) -> f32 {
        theme::font_scale() * self.resolved(cx).grade.text_scale
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
        let line_h = LINE_H * self.eff_scale(cx);
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

    /// Click → cursor: map a tube-space click to (line, col). `select` (shift
    /// held) extends the current selection to the click instead of clearing it.
    fn place_cursor(&mut self, pos: gpui::Point<Pixels>, select: bool, cx: &mut Context<Self>) {
        let Some(b) = *self.tube_bounds.lock().unwrap() else {
            return;
        };
        let off = self.scroll.offset();
        let sc = self.eff_scale(cx);
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
            let col =
                ((x / (CHAR_W * sc)).round().max(0.) as usize).min(e.line(line).chars().count());
            e.set_cursor(line, col, select);
            cx.notify();
        });
        cx.notify();
    }

    /// Double-click in the source editor → select the word under the pointer.
    fn select_word_click(&mut self, pos: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.place_cursor(pos, false, cx);
        self.doc.update(cx, |doc, cx| {
            doc.editor.select_word_at_cursor();
            cx.notify();
        });
        cx.notify();
    }

    /// Triple-click in the source editor → select the whole line.
    fn select_line_click(&mut self, pos: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        self.place_cursor(pos, false, cx);
        self.doc.update(cx, |doc, cx| {
            doc.editor.select_line_at_cursor();
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
        let key = ks.key.as_str();

        // the comment composer owns the keyboard while the device panel is open
        if self.comment_ui.is_some() {
            self.comment_panel_key(ks, cx);
            return;
        }

        // The F1 / Ctrl+/ toggle is handled at the Workspace level so it works
        // regardless of which pane (if any) holds focus. Here we only enforce the
        // modal: while help is open Esc closes it and every other key is swallowed
        // so nothing edits behind it.
        if self.show_help {
            if key == "escape" {
                self.show_help = false;
                cx.notify();
            }
            return;
        }

        // ctrl+e cycles source/preview here (and leaves comment mode to source);
        // ctrl+s (save / save-as) is owned by the Workspace so it can name a new
        // notebook + rename its tab.
        if m.control && !m.alt && !m.shift && key == "e" {
            self.mode = match self.mode {
                Mode::Source => Mode::Preview,
                _ => Mode::Source,
            };
            cx.notify();
            return;
        }
        // ctrl+shift+c toggles comment mode (mirrors the ▣ comment header chip)
        if m.control && m.shift && !m.alt && key == "c" {
            self.toggle_comment_mode(cx);
            return;
        }
        // ctrl+shift+a opens the all-comments browser (mirrors ≡ comments)
        if m.control && m.shift && !m.alt && key == "a" {
            self.toggle_comment_browser(cx);
            return;
        }
        // ctrl+shift+e → copy the document with comments injected (works in any
        // mode — it reads the doc + its comment store)
        if m.control && m.shift && !m.alt && key == "e" {
            self.copy_with_comments(cx);
            return;
        }
        // only the source editor consumes typing / selection keys
        if self.mode != Mode::Source {
            return;
        }
        // alt chords (alt+arrow pane focus, ctrl+alt splits) belong to the
        // Workspace — never swallow them in the editor.
        if m.alt {
            return;
        }

        // undo / redo: ctrl+z, ctrl+shift+z (redo), ctrl+y (redo)
        if m.control && key == "z" {
            self.undo_redo(m.shift, cx); // shift → redo
            return;
        }
        if m.control && !m.shift && key == "y" {
            self.undo_redo(true, cx);
            return;
        }

        // clipboard + select-all (ctrl, no shift)
        if m.control && !m.shift {
            match key {
                "a" => {
                    self.doc.update(cx, |doc, cx| {
                        doc.editor.select_all();
                        cx.notify();
                    });
                    cx.notify();
                    return;
                }
                "c" => return self.copy(cx),
                "x" => return self.cut(cx),
                "v" => return self.paste(cx),
                _ => {}
            }
        }

        let select = m.shift; // shift held → extend the selection
        let by_unit = m.control; // ctrl held → word / document granularity

        let handled = self.doc.update(cx, |doc, cx| {
            let e = &mut doc.editor;
            let mut edited = false;
            match key {
                "left" => {
                    if by_unit {
                        e.word_left(select)
                    } else {
                        e.left(select)
                    }
                }
                "right" => {
                    if by_unit {
                        e.word_right(select)
                    } else {
                        e.right(select)
                    }
                }
                "up" => e.up(select),
                "down" => e.down(select),
                "home" => {
                    if by_unit {
                        e.doc_start(select)
                    } else {
                        e.home(select)
                    }
                }
                "end" => {
                    if by_unit {
                        e.doc_end(select)
                    } else {
                        e.end(select)
                    }
                }
                "backspace" => {
                    if by_unit {
                        e.delete_word_left()
                    } else {
                        e.backspace()
                    }
                    edited = true;
                }
                "delete" => {
                    if by_unit {
                        e.delete_word_right()
                    } else {
                        e.delete()
                    }
                    edited = true;
                }
                // text entry — never while ctrl is held (those are unbound chords)
                "enter" if !m.control => {
                    e.insert("\n");
                    edited = true;
                }
                "tab" if !m.control => {
                    e.insert("  ");
                    edited = true;
                }
                "space" if !m.control => {
                    e.insert(" ");
                    edited = true;
                }
                _ => match ks.key_char.clone() {
                    Some(ch) if !m.control => {
                        e.insert(&ch);
                        edited = true;
                    }
                    _ => return false,
                },
            }
            if edited {
                doc.reparse(); // live: preview panes of this doc follow every edit
            }
            cx.notify();
            true
        });
        if handled {
            self.follow_cursor(cx);
            cx.notify();
        }
    }

    /// Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y — undo or redo, then re-render the preview.
    fn undo_redo(&mut self, redo: bool, cx: &mut Context<Self>) {
        self.doc.update(cx, |doc, cx| {
            if redo {
                doc.editor.redo();
            } else {
                doc.editor.undo();
            }
            doc.reparse(); // live preview panes follow the undone/redone state
            cx.notify();
        });
        self.follow_cursor(cx);
        cx.notify();
    }

    /// Ctrl+C — copy the selection to the system clipboard (and the X11 PRIMARY
    /// selection, so middle-click paste works too; no-op on Wayland).
    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.doc.read(cx).editor.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
    }

    /// Ctrl+X — copy the selection, then delete it.
    fn cut(&mut self, cx: &mut Context<Self>) {
        let Some(text) = self.doc.read(cx).editor.selected_text() else {
            return;
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        cx.write_to_primary(ClipboardItem::new_string(text));
        self.doc.update(cx, |doc, cx| {
            doc.editor.backspace(); // deletes the active selection
            doc.reparse();
            cx.notify();
        });
        self.follow_cursor(cx);
        cx.notify();
    }

    /// Ctrl+V — insert the clipboard at the cursor (replacing any selection).
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        self.doc.update(cx, |doc, cx| {
            doc.editor.insert(&text.replace("\r\n", "\n"));
            doc.reparse();
            cx.notify();
        });
        self.follow_cursor(cx);
        cx.notify();
    }

    // ── comment mode ─────────────────────────────────────────────────────

    /// Flip this pane into/out of the read-only review surface.
    pub fn toggle_comment_mode(&mut self, cx: &mut Context<Self>) {
        self.sel = None;
        self.sel_dragging = false;
        if self.mode == Mode::Comment {
            self.mode = self.prev_mode;
            self.comment_ui = None;
            self.show_browser = false;
        } else {
            self.prev_mode = self.mode;
            self.mode = Mode::Comment;
            self.comment_ui = None;
            self.doc.update(cx, |doc, _| doc.ensure_comments_loaded());
        }
        cx.notify();
    }

    pub fn is_commenting(&self) -> bool {
        self.mode == Mode::Comment
    }

    /// Open the device panel on the i-th top-level block (whole-block comment).
    fn open_block_comment(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.doc.update(cx, |doc, _| doc.ensure_comments_loaded());
        let opened = {
            let doc = self.doc.read(cx);
            doc.meta.get(i).map(|m| {
                let fp = m.fp;
                let ord = doc.meta[..i].iter().filter(|x| x.fp == fp).count();
                let kind = doc.blocks.get(i).map(block_kind).unwrap_or("block");
                let existing = doc.comments.block_thread(fp, ord).map(|t| t.id.clone());
                CommentUi {
                    anchor: comments::Anchor::whole_block(fp, ord, m.plain.clone()),
                    thread_id: existing,
                    kind,
                    composer: String::new(),
                }
            })
        };
        if let Some(ui) = opened {
            self.sel = None;
            self.comment_ui = Some(ui);
            window.focus(&self.focus_handle, cx);
            cx.notify();
        }
    }

    /// Window-space point with the pane's barrel-warp undone — so a click on the
    /// curved glass maps to where gpui actually laid the text out (mirrors
    /// `place_cursor`). Needed for char-accurate hit-testing in comment mode.
    fn unwarp(&self, pos: gpui::Point<Pixels>, cx: &gpui::App) -> Option<gpui::Point<Pixels>> {
        let b = (*self.tube_bounds.lock().unwrap())?;
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
        Some(point(b.origin.x + px(sx), b.origin.y + px(sy)))
    }

    /// Byte index in block `i`'s paragraph text nearest the (warped) point.
    fn sel_index(&self, i: usize, pos: gpui::Point<Pixels>, cx: &gpui::App) -> Option<usize> {
        let layout = self.block_layouts.get(&i)?;
        let p = self.unwarp(pos, cx)?;
        Some(match layout.index_for_position(p) {
            Ok(ix) => ix,
            Err(ix) => ix,
        })
    }

    fn begin_sel(
        &mut self,
        i: usize,
        pos: gpui::Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus_handle, cx);
        // A press NEVER opens a comment — it only arms a potential selection.
        // end_sel decides on RELEASE ("click off"): a real drag → range comment,
        // a plain click → whole-block comment. Paragraphs range-select via their
        // captured layout; other blocks have no index (→ 0) so any release on
        // them collapses to the whole-block comment.
        let ix = self.sel_index(i, pos, cx).unwrap_or(0);
        self.sel = Some(Sel {
            block: i,
            anchor: ix,
            head: ix,
        });
        self.sel_dragging = true;
        cx.notify();
    }

    fn update_sel(&mut self, pos: gpui::Point<Pixels>, cx: &mut Context<Self>) {
        if !self.sel_dragging {
            return;
        }
        let Some(block) = self.sel.as_ref().map(|s| s.block) else {
            return;
        };
        if let Some(ix) = self.sel_index(block, pos, cx) {
            if let Some(sel) = self.sel.as_mut() {
                sel.head = ix;
            }
            cx.notify();
        }
    }

    fn end_sel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.sel_dragging {
            return;
        }
        self.sel_dragging = false;
        let Some(sel) = self.sel.clone() else { return };
        let (s, e) = sel.range();
        if e > s {
            self.open_range_comment(sel.block, s, e, cx);
        } else {
            // a plain click (no drag) comments on the whole block
            self.open_block_comment(sel.block, window, cx);
        }
    }

    /// Open the device panel on a selected span (byte range) within a paragraph.
    fn open_range_comment(&mut self, i: usize, bs: usize, be: usize, cx: &mut Context<Self>) {
        self.doc.update(cx, |doc, _| doc.ensure_comments_loaded());
        let opened = {
            let doc = self.doc.read(cx);
            doc.meta.get(i).and_then(|m| {
                let text = &m.plain;
                let bs = bs.min(text.len());
                let be = be.min(text.len());
                let quote = text.get(bs..be)?.to_string();
                if quote.trim().is_empty() {
                    return None;
                }
                let fp = m.fp;
                let ord = doc.meta[..i].iter().filter(|x| x.fp == fp).count();
                let cs = text[..bs].chars().count();
                let ce = text[..be].chars().count();
                // reuse an existing identical range thread if there is one
                let existing = doc
                    .comments
                    .threads_for_block(fp, ord)
                    .into_iter()
                    .find(|t| t.anchor.is_range() && t.anchor.quote == quote)
                    .map(|t| t.id.clone());
                Some((fp, ord, cs, ce, quote, existing))
            })
        };
        if let Some((fp, ord, cs, ce, quote, existing)) = opened {
            self.comment_ui = Some(CommentUi {
                anchor: comments::Anchor::span(fp, ord, (cs, ce), quote),
                thread_id: existing,
                kind: "selection",
                composer: String::new(),
            });
            cx.notify();
        }
    }

    /// Save the composer draft — a reply if the thread exists, else a new thread.
    fn commit_comment(&mut self, cx: &mut Context<Self>) {
        let Some(ui) = self.comment_ui.as_ref() else {
            return;
        };
        let body = ui.composer.trim().to_string();
        if body.is_empty() {
            return;
        }
        let anchor = ui.anchor.clone();
        let thread_id = ui.thread_id.clone();
        let author = self.author.clone();
        let id = self.doc.update(cx, |doc, _| {
            doc.ensure_comments_loaded();
            let id = match &thread_id {
                Some(tid) => {
                    doc.comments.reply(tid, author, body);
                    tid.clone()
                }
                None => doc.comments.new_thread(anchor, author, body),
            };
            doc.save_comments();
            id
        });
        if let Some(ui) = self.comment_ui.as_mut() {
            ui.thread_id = Some(id);
            ui.composer.clear();
        }
        // refresh preview/badge observers of this doc
        self.doc.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    fn toggle_resolve(&mut self, cx: &mut Context<Self>) {
        let Some(tid) = self.comment_ui.as_ref().and_then(|u| u.thread_id.clone()) else {
            return;
        };
        self.doc.update(cx, |doc, _| {
            let now = doc
                .comments
                .thread(&tid)
                .map(|t| t.resolved)
                .unwrap_or(false);
            doc.comments.set_resolved(&tid, !now);
            doc.save_comments();
        });
        cx.notify();
    }

    /// Route a keystroke into the composer while the panel is open.
    fn comment_panel_key(&mut self, ks: &gpui::Keystroke, cx: &mut Context<Self>) {
        let m = &ks.modifiers;
        match ks.key.as_str() {
            "escape" => {
                self.comment_ui = None;
                self.sel = None;
            }
            "enter" if m.control => return self.commit_comment(cx),
            "enter" => self.composer_push('\n'),
            "backspace" => self.composer_pop(),
            "space" => self.composer_push(' '),
            _ => {
                if !m.control {
                    if let Some(ch) = ks.key_char.clone() {
                        for c in ch.chars() {
                            self.composer_push(c);
                        }
                    }
                }
            }
        }
        cx.notify();
    }

    fn composer_push(&mut self, c: char) {
        if let Some(ui) = self.comment_ui.as_mut() {
            ui.composer.push(c);
        }
    }

    fn composer_pop(&mut self) {
        if let Some(ui) = self.comment_ui.as_mut() {
            ui.composer.pop();
        }
    }

    /// Close the comment panel and drop any in-progress selection.
    fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.comment_ui = None;
        self.sel = None;
        self.sel_dragging = false;
        cx.notify();
    }

    /// Open/close the "all comments" browser (forces comment mode on).
    pub fn toggle_comment_browser(&mut self, cx: &mut Context<Self>) {
        self.show_browser = !self.show_browser;
        if self.show_browser {
            self.comment_ui = None;
            self.sel = None;
            if self.mode != Mode::Comment {
                self.prev_mode = self.mode;
                self.mode = Mode::Comment;
            }
            self.doc.update(cx, |doc, _| doc.ensure_comments_loaded());
        }
        cx.notify();
    }

    /// Open an existing thread (from the browser) in the device panel.
    fn open_thread(&mut self, id: String, cx: &mut Context<Self>) {
        let ui = {
            let doc = self.doc.read(cx);
            doc.comments.thread(&id).map(|t| {
                let kind = if t.anchor.is_range() {
                    "selection"
                } else {
                    // resolve the block kind label by walking to the fp/ord-th block
                    let (fp, ord) = (t.anchor.block_fp, t.anchor.block_ord);
                    let mut seen = 0usize;
                    let mut k = "block";
                    for (i, m) in doc.meta.iter().enumerate() {
                        if m.fp == fp {
                            if seen == ord {
                                k = doc.blocks.get(i).map(block_kind).unwrap_or("block");
                                break;
                            }
                            seen += 1;
                        }
                    }
                    k
                };
                CommentUi {
                    anchor: t.anchor.clone(),
                    thread_id: Some(t.id.clone()),
                    kind,
                    composer: String::new(),
                }
            })
        };
        if let Some(ui) = ui {
            self.show_browser = false;
            self.sel = None;
            self.comment_ui = Some(ui);
            cx.notify();
        }
    }

    fn delete_thread(&mut self, id: String, cx: &mut Context<Self>) {
        self.doc.update(cx, |doc, _| {
            doc.comments.delete(&id);
            doc.save_comments();
        });
        cx.notify();
    }

    /// Toggle the keys-&-tips help modal (driven by the Workspace-level F1 / Ctrl+/).
    pub fn toggle_help(&mut self, cx: &mut Context<Self>) {
        self.show_help = !self.show_help;
        cx.notify();
    }

    /// Show a transient confirmation pill for ~2.6s (aged out by the fx clock).
    fn flash(&mut self, msg: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast = Some(msg.into());
        self.toast_ticks = 80;
        cx.notify();
    }

    /// THE feature: copy the whole document to the clipboard with every comment
    /// injected as a `> 💬` blockquote right after the section it annotates — the
    /// artifact you hand straight back to your coding agent. See
    /// `comments::review_markdown`.
    fn copy_with_comments(&mut self, cx: &mut Context<Self>) {
        self.doc.update(cx, |doc, _| doc.ensure_comments_loaded());
        let (md, n) = {
            let doc = self.doc.read(cx);
            let src = doc.editor.text();
            let n = doc
                .comments
                .threads
                .iter()
                .filter(|t| !t.deprecated)
                .count();
            (comments::review_markdown(&src, &doc.meta, &doc.comments), n)
        };
        cx.write_to_clipboard(ClipboardItem::new_string(md));
        let msg = match n {
            0 => "Copied — document only (no comments yet)".to_string(),
            1 => "✓ Copied document + 1 comment".to_string(),
            _ => format!("✓ Copied document + {n} comments"),
        };
        self.flash(msg, cx);
    }

    /// Export comments to a sidecar beside the doc, guarded by .git/info/exclude
    /// so they can't be committed. No-op for unsaved scratch buffers.
    fn export_comments(&mut self, cx: &mut Context<Self>) {
        let (path, store) = {
            let doc = self.doc.read(cx);
            (doc.path.clone(), doc.comments.clone())
        };
        if let Some(path) = path {
            match comments::export_sidecar(&path, &store) {
                Ok(p) => eprintln!(
                    "[comments] exported → {} (git-ignored locally)",
                    p.display()
                ),
                Err(e) => eprintln!("[comments] export failed: {e}"),
            }
        } else {
            eprintln!("[comments] save the notebook first to export its comments");
        }
    }

    /// The read-only review document. Paragraphs are rebuilt as `StyledText`
    /// (capturing their layout for drag-select) with comment spans merged into
    /// the inline runs; other blocks render via `block_element` and are
    /// whole-block-commentable. Commented blocks glow + carry a count badge.
    fn comment_document(&mut self, th: &theme::Theme, cx: &mut Context<Self>) -> AnyElement {
        let sel = self.sel.clone();
        // the open panel's target block, only when it's a WHOLE-block thread
        let active_block = self.comment_ui.as_ref().and_then(|u| {
            u.anchor
                .range
                .is_none()
                .then_some((u.anchor.block_fp, u.anchor.block_ord))
        });

        struct Row {
            el: AnyElement,
            layout: Option<TextLayout>,
            badge: usize,
            whole: usize,
            resolved: bool,
            is_active: bool,
        }

        // Phase 1 — gather owned rows (drops the doc borrow before we take the
        // &mut Context that cx.listener needs).
        let rows: Vec<Row> = {
            let doc = self.doc.read(cx);
            doc.blocks
                .iter()
                .enumerate()
                .map(|(i, b)| {
                    let fp = doc.meta[i].fp;
                    let ord = doc.meta[..i].iter().filter(|x| x.fp == fp).count();
                    let threads = doc.comments.threads_for_block(fp, ord);
                    let badge = threads.len();
                    let whole = threads.iter().filter(|t| t.anchor.range.is_none()).count();
                    let resolved = badge > 0 && threads.iter().all(|t| t.resolved);
                    let is_active = active_block == Some((fp, ord));

                    if let Some((text, base_runs)) = render::paragraph_text(b) {
                        // comment spans: the live drag-selection + each range thread
                        let mut spans: Vec<(Range<usize>, SpanKind)> = Vec::new();
                        if let Some(s) = &sel {
                            let (a, e) = s.range();
                            if s.block == i && e > a {
                                spans.push((a..e, SpanKind::Active));
                            }
                        }
                        for t in &threads {
                            if t.anchor.is_range() {
                                if let Some(byte) = text.find(t.anchor.quote.as_str()) {
                                    let kind = if t.resolved {
                                        SpanKind::Resolved
                                    } else {
                                        SpanKind::Range
                                    };
                                    spans.push((byte..byte + t.anchor.quote.len(), kind));
                                }
                            }
                        }
                        let runs = merge_runs(text.len(), &base_runs, &spans, th);
                        let styled = StyledText::new(text).with_highlights(runs);
                        let layout = styled.layout().clone();
                        Row {
                            el: styled.into_any_element(),
                            layout: Some(layout),
                            badge,
                            whole,
                            resolved,
                            is_active,
                        }
                    } else {
                        Row {
                            el: render::block_element(b),
                            layout: None,
                            badge,
                            whole,
                            resolved,
                            is_active,
                        }
                    }
                })
                .collect()
        };

        // Phase 2 — wrap each block; record this frame's paragraph layouts.
        self.block_layouts.clear();
        let children: Vec<AnyElement> = rows
            .into_iter()
            .enumerate()
            .map(|(i, r)| {
                if let Some(l) = r.layout {
                    self.block_layouts.insert(i, l);
                }
                let mut wrap = div()
                    .id(SharedString::from(format!("cblock-{i}")))
                    .relative()
                    .rounded_md()
                    .px_2()
                    .py_1()
                    .cursor_pointer()
                    .hover(|s| s.bg(th.accent.alpha(0.06)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |pane, ev: &MouseDownEvent, window, cx| {
                            pane.begin_sel(i, ev.position, window, cx);
                        }),
                    )
                    .child(r.el);

                // whole-block glow (range comments glow their span instead)
                if r.is_active {
                    wrap = wrap
                        .bg(th.accent.alpha(0.14))
                        .text_color(white().alpha(0.95))
                        .shadow(vec![glow(th.accent.alpha(0.55), 12.)]);
                } else if r.whole > 0 {
                    let a = if r.resolved { 0.16 } else { 0.32 };
                    wrap = wrap
                        .bg(th.accent.alpha(0.05))
                        .text_color(th.accent)
                        .shadow(vec![glow(th.accent.alpha(a), 8.)]);
                }
                if r.badge > 0 {
                    wrap = wrap.child(
                        div()
                            .absolute()
                            .top_0()
                            .right_0()
                            .px_1()
                            .rounded_sm()
                            .text_size(px(9.))
                            .font_weight(FontWeight::BOLD)
                            .bg(if r.resolved {
                                th.frame_faint
                            } else {
                                th.accent
                            })
                            .text_color(th.bg)
                            .child(SharedString::from(format!("● {}", r.badge))),
                    );
                }
                wrap.into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(children)
            .into_any_element()
    }

    /// The floating non-CRT "device" panel for the open thread (if any).
    fn render_comment_panel(
        &self,
        th: &theme::Theme,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let ui = self.comment_ui.as_ref()?;
        let quote = ui.anchor.quote.clone();
        let kind = ui.kind;
        let has_thread = ui.thread_id.is_some();

        // owned snapshot of the thread's comments (drops the doc borrow)
        let (entries, resolved): (Vec<(String, String, String)>, bool) = {
            let doc = self.doc.read(cx);
            match ui
                .thread_id
                .as_deref()
                .and_then(|id| doc.comments.thread(id))
            {
                Some(t) => (
                    t.comments
                        .iter()
                        .map(|c| (c.author.clone(), comment_ui::ago(c.ts), c.body.clone()))
                        .collect(),
                    t.resolved,
                ),
                None => (Vec::new(), false),
            }
        };

        // ── titlebar ──
        let titlebar = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(th.accent.alpha(0.25))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(th.accent)
                            .child("COMMENT"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(th.frame_faint)
                            .child(SharedString::from(format!("// {kind}"))),
                    ),
            )
            .child(
                div()
                    .text_size(px(13.))
                    .cursor_pointer()
                    .text_color(th.frame_faint)
                    .hover(|s| s.text_color(th.accent))
                    .child("✕")
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|pane, _: &MouseDownEvent, _w, cx| pane.close_panel(cx)),
                    ),
            );

        // ── magnifier: the quoted text ──
        let magnifier = div()
            .px_3()
            .pt_3()
            .child(comment_ui::kicker("SELECTION"))
            .child(
                comment_ui::sunken_screen(th)
                    .id("cmt-quote")
                    .mt_1()
                    .p_2()
                    .max_h(px(120.))
                    .overflow_y_scroll()
                    .text_size(px(12.))
                    .child(SharedString::from(quote)),
            );

        // ── thread screen ──
        let mut thread = comment_ui::sunken_screen(th)
            .id("cmt-thread")
            .mx_3()
            .mt_3()
            .p_2()
            .max_h(px(190.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(12.));
        if entries.is_empty() {
            thread = thread.child(
                div()
                    .italic()
                    .text_color(th.frame_faint)
                    .child("No comments yet — type below, then Add."),
            );
        } else {
            for (author, when, body) in &entries {
                thread = thread.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .items_baseline()
                                .child(
                                    div()
                                        .text_color(th.accent)
                                        .font_weight(FontWeight::BOLD)
                                        .child(SharedString::from(author.clone())),
                                )
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(th.frame_faint)
                                        .child(SharedString::from(when.clone())),
                                ),
                        )
                        .child(
                            div()
                                .text_color(white().alpha(0.9))
                                .child(SharedString::from(body.clone())),
                        ),
                );
            }
        }

        // ── composer (keyboard-driven; see comment_panel_key) ──
        let composer = self
            .comment_ui
            .as_ref()
            .map(|u| u.composer.clone())
            .unwrap_or_default();
        let (comp_text, comp_color) = if composer.is_empty() {
            (
                "Add a comment… (Ctrl+Enter to save)".to_string(),
                th.frame_faint,
            )
        } else {
            (format!("{composer}▏"), white().alpha(0.92))
        };
        let composer_box = div().px_3().pt_3().child(
            comment_ui::sunken_screen(th)
                .min_h(px(72.))
                .p_2()
                .text_size(px(12.))
                .text_color(comp_color)
                .child(SharedString::from(comp_text)),
        );

        // ── physical buttons ──
        let mut buttons = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_end()
            .gap_2()
            .p_3();
        if has_thread {
            let label = if resolved { "Unresolve" } else { "Resolve" };
            buttons = buttons.child(comment_ui::device_button(th, label, false).on_mouse_down(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseDownEvent, _w, cx| pane.toggle_resolve(cx)),
            ));
        }
        buttons = buttons
            .child(comment_ui::device_button(th, "Done", false).on_mouse_down(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseDownEvent, _w, cx| pane.close_panel(cx)),
            ))
            .child(comment_ui::device_button(th, "Add", true).on_mouse_down(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseDownEvent, _w, cx| pane.commit_comment(cx)),
            ));

        let panel = comment_ui::device_panel(th)
            .w(px(440.))
            .max_h(px(560.))
            .child(titlebar)
            .child(magnifier)
            .child(thread)
            .child(composer_box)
            .child(buttons)
            // clicking the panel must not fall through to the backdrop-close
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()),
            );

        let overlay = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseDownEvent, _w, cx| pane.close_panel(cx)),
            )
            .child(panel);
        Some(overlay.into_any_element())
    }

    /// The "all comments" browser: every thread for this doc, with a distinct
    /// Deprecated section for orphaned anchors (delete or let them auto-revive
    /// if the text returns). Export lives in its titlebar.
    /// The themed keys-&-tips help modal (F1 / Ctrl+/). Inherits the pane's
    /// effective theme — same scrim + centered device-panel pattern as the browser.
    fn render_help(&self, th: &theme::Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_help {
            return None;
        }
        // one "key — description" line
        let row = |k: &str, d: &str| {
            div()
                .flex()
                .flex_row()
                .gap_3()
                .items_baseline()
                .child(
                    div()
                        .flex_none()
                        .min_w(px(148.))
                        .text_color(th.accent)
                        .text_size(px(11.5))
                        .child(SharedString::from(k.to_string())),
                )
                .child(
                    // flex + min_w_0 so a long description wraps inside the column
                    // instead of overflowing (and clipping) the panel
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_color(th.text.alpha(0.9))
                        .text_size(px(11.5))
                        .child(SharedString::from(d.to_string())),
                )
        };
        let head = |t: &str| {
            div()
                .mt_2()
                .mb_1()
                .text_color(th.frame_faint)
                .text_size(px(10.))
                .child(SharedString::from(format!("// {t}")))
        };

        let col_a = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(head("EDIT"))
            .child(row("Ctrl+Z", "undo"))
            .child(row("Ctrl+Shift+Z · Ctrl+Y", "redo"))
            .child(row("Ctrl+C · X · V", "copy · cut · paste"))
            .child(row("Ctrl+A", "select all"))
            .child(row("Ctrl+⌫ · Ctrl+Del", "delete word left · right"))
            .child(head("SELECT"))
            .child(row("Shift+← → ↑ ↓", "extend selection"))
            .child(row("Ctrl+Shift+← →", "select by word"))
            .child(row("Shift+Home · End", "select to line edge"))
            .child(row("Ctrl+Shift+Home·End", "select to doc top · bottom"))
            .child(row("2× · 3× click", "select word · line"))
            .child(row("shift-click", "extend selection to click"))
            .child(head("MOVE"))
            .child(row("Ctrl+← →", "jump by word"))
            .child(row("Home · End", "line start · end"))
            .child(row("Ctrl+Home · End", "document top · bottom"))
            .child(row("Ctrl+P", "find / open file"));

        let col_b = div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap_0p5()
            .child(head("FILES & TABS"))
            .child(row("Ctrl+S", "save"))
            .child(row("Ctrl+Shift+T", "new tab"))
            .child(row("Ctrl+W", "close tab / pane"))
            .child(row("Ctrl+PgUp · PgDn", "previous · next tab"))
            .child(row("Ctrl+Alt+M", "quick scratch-pad window"))
            .child(head("PANES & VIEW"))
            .child(row("Ctrl+Alt+R · D", "split right · down"))
            .child(row("Alt+← → ↑ ↓", "focus another pane"))
            .child(row("Ctrl+E", "toggle source / preview"))
            .child(head("COMMENTS"))
            .child(row("Ctrl+Shift+C", "comment mode"))
            .child(row("Ctrl+Shift+A", "all-comments browser"))
            .child(row("Ctrl+Shift+E", "★ copy with comments"))
            .child(head("HELP"))
            .child(row("F1 · Ctrl+/", "this panel"));

        let tip = |t: &str| {
            div()
                .text_color(th.text.alpha(0.8))
                .text_size(px(11.))
                .child(SharedString::from(t.to_string()))
        };
        let tips = div()
            .mt_3()
            .pt_2()
            .border_t_1()
            .border_color(th.frame_border.alpha(0.3))
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_color(th.frame_faint)
                    .text_size(px(10.))
                    .child("// TIPS"),
            )
            .child(tip(
                "• In comment mode, drag a phrase to comment on that exact span.",
            ))
            .child(tip(
                "• “Copy with comments” returns the whole doc with your notes inline — paste it straight back to your agent.",
            ))
            .child(tip(
                "• Themes hot-reload from ~/.config/markdown-delight/theme.toml while the app runs.",
            ));

        let titlebar = div()
            .flex()
            .flex_row()
            .justify_between()
            .items_center()
            .child(
                div()
                    .text_color(th.accent)
                    .text_size(px(13.))
                    .child("⌨  markdown-delight — keys & tips"),
            )
            .child(
                div()
                    .text_color(th.frame_faint)
                    .text_size(px(10.))
                    .child("Esc / F1 to close"),
            );

        let panel = comment_ui::device_panel(th)
            .id("help-panel")
            .w(px(760.))
            .max_h(px(600.))
            .p_5()
            .border_2()
            .border_color(th.accent.alpha(0.75))
            .overflow_y_scroll()
            .child(titlebar)
            .child(
                div()
                    .mt_2()
                    .flex()
                    .flex_row()
                    .gap(px(28.))
                    .child(col_a)
                    .child(col_b),
            )
            .child(tips)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()),
            );

        let overlay = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.5))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseDownEvent, _w, cx| {
                    pane.show_help = false;
                    cx.notify();
                }),
            )
            .child(panel);
        Some(overlay.into_any_element())
    }

    fn render_browser(&self, th: &theme::Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.show_browser {
            return None;
        }
        struct BRow {
            id: String,
            deprecated: bool,
            resolved: bool,
            range: bool,
            snippet: String,
            count: usize,
            body: String,
        }
        let rows: Vec<BRow> = {
            let doc = self.doc.read(cx);
            doc.comments
                .threads
                .iter()
                .map(|t| {
                    let q = t.anchor.quote.trim();
                    let snippet = if q.chars().count() > 60 {
                        format!("{}…", q.chars().take(60).collect::<String>())
                    } else {
                        q.to_string()
                    };
                    BRow {
                        id: t.id.clone(),
                        deprecated: t.deprecated,
                        resolved: t.resolved,
                        range: t.anchor.is_range(),
                        snippet,
                        count: t.comments.len(),
                        body: t
                            .comments
                            .first()
                            .map(|c| c.body.clone())
                            .unwrap_or_default(),
                    }
                })
                .collect()
        };
        let total = rows.len();
        let (active, dead): (Vec<BRow>, Vec<BRow>) = rows.into_iter().partition(|r| !r.deprecated);

        let mut list = comment_ui::sunken_screen(th)
            .id("cmt-browser-list")
            .m_3()
            .p_2()
            .max_h(px(380.))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(12.));

        list = list.child(comment_ui::kicker(format!("ACTIVE · {}", active.len())));
        if active.is_empty() {
            list = list.child(
                div()
                    .italic()
                    .text_color(th.frame_faint)
                    .child("No comments yet."),
            );
        }
        for r in active {
            let id = r.id.clone();
            list = list.child(
                div()
                    .id(SharedString::from(format!("br-{}", r.id)))
                    .rounded_md()
                    .p_2()
                    .cursor_pointer()
                    .bg(th.accent.alpha(0.05))
                    .hover(|s| s.bg(th.accent.alpha(0.12)))
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |pane, _: &MouseDownEvent, _w, cx| {
                            pane.open_thread(id.clone(), cx)
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .items_baseline()
                            .text_size(px(9.5))
                            .child(div().text_color(th.accent).child(if r.range {
                                "⌇ span"
                            } else {
                                "▭ block"
                            }))
                            .child(div().text_color(th.frame_faint).child(SharedString::from(
                                format!(
                                    "{} comment{}",
                                    r.count,
                                    if r.count == 1 { "" } else { "s" }
                                ),
                            )))
                            .when(r.resolved, |d| {
                                d.child(div().text_color(th.frame_faint).child("· resolved"))
                            }),
                    )
                    .child(
                        div()
                            .text_color(white().alpha(0.55))
                            .italic()
                            .child(SharedString::from(r.snippet)),
                    )
                    .child(div().text_color(th.text).child(SharedString::from(r.body))),
            );
        }

        if !dead.is_empty() {
            list = list.child(comment_ui::kicker(format!(
                "DEPRECATED · {} (anchor text changed)",
                dead.len()
            )));
            for r in dead {
                let id = r.id.clone();
                list = list.child(
                    div()
                        .rounded_md()
                        .p_2()
                        .bg(hsla(0., 0., 0., 0.25))
                        .border_1()
                        .border_color(th.frame_faint.alpha(0.4))
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_size(px(9.5))
                                        .text_color(th.frame_faint)
                                        .child("orphaned"),
                                )
                                .child(
                                    comment_ui::device_button(th, "Delete", false).on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(move |pane, _: &MouseDownEvent, _w, cx| {
                                            pane.delete_thread(id.clone(), cx)
                                        }),
                                    ),
                                ),
                        )
                        .child(
                            div()
                                .text_color(th.frame_faint.alpha(0.8))
                                .italic()
                                .child(SharedString::from(r.snippet)),
                        )
                        .child(
                            div()
                                .text_color(white().alpha(0.6))
                                .child(SharedString::from(r.body)),
                        ),
                );
            }
        }

        let titlebar = div()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(th.accent.alpha(0.25))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_baseline()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::EXTRA_BOLD)
                            .text_color(th.accent)
                            .child("ALL COMMENTS"),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(th.frame_faint)
                            .child(SharedString::from(format!("// {total}"))),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        comment_ui::device_button(th, "⧉ copy with comments", true).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|pane, _: &MouseDownEvent, _w, cx| {
                                pane.copy_with_comments(cx);
                                pane.show_browser = false;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        comment_ui::device_button(th, "⤓ export", false).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|pane, _: &MouseDownEvent, _w, cx| {
                                pane.export_comments(cx)
                            }),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(13.))
                            .cursor_pointer()
                            .text_color(th.frame_faint)
                            .hover(|s| s.text_color(th.accent))
                            .child("✕")
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|pane, _: &MouseDownEvent, _w, cx| {
                                    pane.show_browser = false;
                                    cx.notify();
                                }),
                            ),
                    ),
            );

        let panel = comment_ui::device_panel(th)
            .w(px(480.))
            .max_h(px(560.))
            .child(titlebar)
            .child(list)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _: &MouseDownEvent, _w, cx| cx.stop_propagation()),
            );

        let overlay = div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(hsla(0., 0., 0., 0.45))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|pane, _: &MouseDownEvent, _w, cx| {
                    pane.show_browser = false;
                    cx.notify();
                }),
            )
            .child(panel);
        Some(overlay.into_any_element())
    }

    /// The source tube: every buffer line, with the selection band drawn across
    /// it and — when nothing is selected — the inverse-video block cursor on the
    /// active line.
    fn source_lines(&self, th: &theme::Theme, cx: &gpui::App) -> Vec<AnyElement> {
        let doc = self.doc.read(cx);
        let e = &doc.editor;
        let (cur_line, cur_col) = e.line_col();
        let sel = e.selection();
        let line_h = px(LINE_H * self.eff_scale(cx));
        let sel_bg = th.accent.alpha(0.30);
        let n_lines = e.line_count();
        (0..n_lines)
            .map(|i| {
                let mut text = e.line(i);
                let line = div().h(line_h).whitespace_nowrap();
                let nchars = text.chars().count();
                let line_start = e.rope.line_to_char(i);
                let has_nl = i + 1 < n_lines;
                let line_end = line_start + nchars + has_nl as usize;

                let mut highlights: Vec<(std::ops::Range<usize>, HighlightStyle)> = Vec::new();

                if let Some(r) = &sel {
                    // the slice of this line that falls inside the selection
                    // (its end may reach past `nchars` to cover the newline)
                    let a = r.start.max(line_start);
                    let b = r.end.min(line_end);
                    if a < b {
                        let cs = a - line_start; // start col (≤ nchars)
                        let mut ce = b - line_start; // end col (nchars+1 ⇒ the \n)
                        let bstart = col_byte(&text, cs);
                        if ce > nchars {
                            // selection runs through the line break — paint a
                            // trailing cell so the wrap reads as selected
                            text.push(' ');
                            ce = nchars + 1;
                        }
                        let bend = col_byte(&text, ce);
                        if bend > bstart {
                            highlights.push((
                                bstart..bend,
                                HighlightStyle {
                                    background_color: Some(sel_bg),
                                    ..Default::default()
                                },
                            ));
                        }
                    }
                } else if i == cur_line {
                    // no selection → inverse-video block cursor on this line
                    let (start, end) = match text.char_indices().nth(cur_col) {
                        Some((b, c)) => (b, b + c.len_utf8()),
                        None => {
                            text.push(' ');
                            (text.len() - 1, text.len())
                        }
                    };
                    highlights.push((
                        start..end,
                        HighlightStyle {
                            color: Some(th.bg),
                            background_color: Some(th.accent),
                            ..Default::default()
                        },
                    ));
                }

                if highlights.is_empty() {
                    return if text.is_empty() {
                        line.into_any_element()
                    } else {
                        line.child(SharedString::from(text)).into_any_element()
                    };
                }
                line.child(StyledText::new(SharedString::from(text)).with_highlights(highlights))
                    .into_any_element()
            })
            .collect()
    }
}

/// Byte offset of the `col`-th char in `s` (its full length if past the last).
fn col_byte(s: &str, col: usize) -> usize {
    s.char_indices().nth(col).map(|(b, _)| b).unwrap_or(s.len())
}

// ── font fallback (portability) ─────────────────────────────────────────────

/// Font families installed on this system, captured once at startup so the
/// editor can fall back deliberately instead of letting gpui pick a silent
/// substitute on a box without JetBrains Mono.
static AVAILABLE_FONTS: OnceLock<Vec<String>> = OnceLock::new();

/// Common monospace families to try, in order, when the requested one is absent.
const MONO_FALLBACKS: &[&str] = &[
    "JetBrains Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Noto Sans Mono",
    "Source Code Pro",
    "Ubuntu Mono",
    "monospace",
];

/// Record the system's available font families. Call once at startup with
/// `cx.text_system().all_font_names()` (see main.rs).
pub fn init_font_registry(names: Vec<String>) {
    let _ = AVAILABLE_FONTS.set(names);
}

fn font_available(name: &str) -> bool {
    match AVAILABLE_FONTS.get() {
        Some(list) => list.iter().any(|n| n.eq_ignore_ascii_case(name)),
        // registry not populated (e.g. unit tests) — assume present, don't rewrite
        None => true,
    }
}

/// Resolve the requested family against what's actually installed, falling back
/// through a chain of common monospace families. Returns the family to request.
pub fn resolve_family(requested: &str) -> String {
    if font_available(requested) {
        return requested.to_string();
    }
    for fb in MONO_FALLBACKS {
        if !fb.eq_ignore_ascii_case(requested) && font_available(fb) {
            return (*fb).to_string();
        }
    }
    // nothing matched; hand back the request and let gpui do its own fallback
    requested.to_string()
}

/// Startup diagnostic: if the ship-default family isn't installed, describe the
/// fallback that will be used (so a silent substitution can't hide). Returns
/// None when the default is present. Call after `init_font_registry`.
pub fn font_diagnostic() -> Option<String> {
    let want = "JetBrains Mono";
    let got = resolve_family(want);
    if got == want {
        return None;
    }
    let n = AVAILABLE_FONTS.get().map(|v| v.len()).unwrap_or(0);
    Some(format!(
        "font '{want}' not installed; falling back to '{got}' ({n} families available). \
         Install JetBrains Mono for the intended look."
    ))
}

/// A centered toast pill floated at the bottom of the tube (transient feedback).
fn toast_pill(th: &theme::Theme, msg: SharedString) -> gpui::Div {
    div()
        .absolute()
        .bottom(px(20.))
        .left_0()
        .right_0()
        .flex()
        .justify_center()
        .child(
            div()
                .px_4()
                .py_2()
                .rounded_lg()
                .bg(th.frame_bg)
                .border_1()
                .border_color(th.accent.alpha(0.6))
                .text_color(th.accent)
                .text_size(px(12.5))
                .shadow(vec![glow(th.accent.alpha(0.5), 18.)])
                .child(msg),
        )
}

/// A soft, spreadless outer glow of the given colour/blur — the "commented" cue.
fn glow(color: gpui::Hsla, blur: f32) -> BoxShadow {
    BoxShadow {
        color,
        offset: point(px(0.), px(0.)),
        blur_radius: px(blur),
        spread_radius: px(0.),
        inset: false,
    }
}

/// A comment highlight overlaid on a paragraph's own inline styling.
#[derive(Clone, Copy)]
enum SpanKind {
    /// the live drag-selection (brightest)
    Active,
    /// a saved range comment
    Range,
    /// a saved, resolved range comment
    Resolved,
}

/// Merge a paragraph's base inline runs with comment spans into a sorted,
/// non-overlapping run list (what `StyledText::with_highlights` requires).
/// Comment spans win on background; the brightest span wins on overlap.
fn merge_runs(
    len: usize,
    base: &[(Range<usize>, HighlightStyle)],
    spans: &[(Range<usize>, SpanKind)],
    th: &theme::Theme,
) -> Vec<(Range<usize>, HighlightStyle)> {
    if base.is_empty() && spans.is_empty() {
        return Vec::new();
    }
    // every run/span boundary becomes a cut point → contiguous segments
    let mut bounds: BTreeSet<usize> = BTreeSet::from([0, len]);
    for (r, _) in base {
        bounds.insert(r.start.min(len));
        bounds.insert(r.end.min(len));
    }
    for (r, _) in spans {
        bounds.insert(r.start.min(len));
        bounds.insert(r.end.min(len));
    }
    let pts: Vec<usize> = bounds.into_iter().collect();
    let mut out = Vec::with_capacity(pts.len());
    for w in pts.windows(2) {
        let (s, e) = (w[0], w[1]);
        if s >= e {
            continue;
        }
        let mut hs = base
            .iter()
            .find(|(r, _)| r.start <= s && e <= r.end)
            .map(|(_, h)| *h)
            .unwrap_or_default();
        // brightest span covering this segment (Active < Range < Resolved)
        let kind = spans
            .iter()
            .filter(|(r, _)| r.start <= s && e <= r.end)
            .map(|(_, k)| *k)
            .min_by_key(|k| match k {
                SpanKind::Active => 0,
                SpanKind::Range => 1,
                SpanKind::Resolved => 2,
            });
        match kind {
            Some(SpanKind::Active) => {
                hs.background_color = Some(th.accent.alpha(0.5));
                hs.color = Some(white().alpha(0.97));
            }
            Some(SpanKind::Range) => hs.background_color = Some(th.accent.alpha(0.3)),
            Some(SpanKind::Resolved) => hs.background_color = Some(th.frame_faint.alpha(0.4)),
            None => {}
        }
        out.push((s..e, hs));
    }
    out
}

/// Human label for a block kind (titlebar of the comment panel).
fn block_kind(b: &render::Block) -> &'static str {
    match b {
        render::Block::Heading { .. } => "heading",
        render::Block::Paragraph(_) => "paragraph",
        render::Block::Code(_) => "code block",
        render::Block::Quote(_) => "quote",
        render::Block::List(_) => "list",
        render::Block::Table(_) => "table",
        render::Block::Rule => "divider",
        render::Block::Html(_) => "html",
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
        // resolve the appearance ONCE per frame, then derive both the composed
        // theme and the text scale from it (avoids a second resolve+clone).
        let r = self.resolved(cx);
        let sc = theme::font_scale() * r.grade.text_scale;
        let th = Arc::new(appearance::compose(cx, &r));
        let editing = self.mode == Mode::Source;
        let line_count = self.doc.read(cx).editor.line_count();
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
            .child(div().mt(px(8. + rail_offset)).flex().flex_col().children(
                (1..=rail_count.max(1)).map(|i| {
                    div()
                        .h(px(21. * sc))
                        .pr_2()
                        .text_size(px(10.5 * sc))
                        .text_color(th.frame_faint.alpha(0.45))
                        .flex()
                        .justify_end()
                        .child(SharedString::from(format!("{i}")))
                }),
            ));

        let content: AnyElement = match self.mode {
            Mode::Source => div()
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
                .into_any_element(),
            Mode::Comment => div()
                .id("cmt")
                .size_full()
                .overflow_y_scroll()
                .overflow_x_hidden()
                .px_5()
                .py_3()
                // drag inside a paragraph to select a span → range comment
                .on_mouse_move(cx.listener(|pane, ev: &MouseMoveEvent, _w, cx| {
                    pane.update_sel(ev.position, cx);
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|pane, _: &MouseUpEvent, window, cx| pane.end_sel(window, cx)),
                )
                .child(self.comment_document(&th, cx))
                .into_any_element(),
            Mode::Preview => div()
                .id("doc")
                .size_full()
                .overflow_y_scroll()
                .overflow_x_hidden()
                .px_5()
                .py_3()
                .child(render::document(&self.doc.read(cx).blocks))
                .into_any_element(),
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
                        match ev.click_count {
                            n if n >= 3 => pane.select_line_click(ev.position, cx),
                            2 => pane.select_word_click(ev.position, cx),
                            _ => {
                                // single click: place caret (shift extends) and
                                // arm a drag-select
                                pane.place_cursor(ev.position, ev.modifiers.shift, cx);
                                pane.text_dragging = true;
                            }
                        }
                    }),
                )
                .on_mouse_move(cx.listener(|pane, ev: &MouseMoveEvent, _w, cx| {
                    // extend the selection to the pointer while dragging
                    if pane.text_dragging && ev.pressed_button == Some(MouseButton::Left) {
                        pane.place_cursor(ev.position, true, cx);
                    }
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|pane, _: &MouseUpEvent, _w, cx| {
                        pane.text_dragging = false;
                        // publish a drag-selection to the X11 PRIMARY (middle-click paste)
                        if let Some(text) = pane.doc.read(cx).editor.selected_text() {
                            cx.write_to_primary(ClipboardItem::new_string(text));
                        }
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

        let root = div()
            .track_focus(&self.focus_handle(cx))
            .on_key_down(cx.listener(Self::on_key))
            // repaint on wheel so the rail tracks the tube's scroll offset
            .on_scroll_wheel(cx.listener(|_, _: &gpui::ScrollWheelEvent, _, cx| cx.notify()))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .font_family(resolve_family(&th.font_family))
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
            // transient confirmation pill (e.g. "✓ Copied document + 3 comments")
            .when_some(self.toast.clone(), |el, msg| el.child(toast_pill(&th, msg)));
        // the help modal floats above everything; then the comment panel / browser
        if let Some(help) = self.render_help(&th, cx) {
            root.child(help)
        } else if let Some(panel) = self.render_comment_panel(&th, cx) {
            root.child(panel)
        } else if let Some(browser) = self.render_browser(&th, cx) {
            root.child(browser)
        } else {
            root
        }
    }
}
