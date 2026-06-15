//! editor.rs — the G0b editor core: rope buffer + cursor + selection + edits +
//! atomic save.
//!
//! Built clean-room on ropey (MIT) per docs/PLAN.md §2 D1 — Zed's GPL editor
//! crate is study-only and was not consulted. Deliberately small: char-index
//! cursor, an optional selection anchor, line/col + word navigation,
//! selection-aware insert/delete, and write-temp-then-rename save. Every
//! navigation method takes `select: bool` (shift held) so the IDE-style
//! Shift+Arrow / Shift+Ctrl+Home family all flow through one seam.

use std::{fs, io, ops::Range, path::Path};

use ropey::Rope;

/// Cap on retained undo steps — the oldest drop off the bottom.
const MAX_UNDO: usize = 256;

#[derive(Clone, Copy, PartialEq)]
enum EditKind {
    /// single-char typing — coalesces into one undo step
    Insert,
    /// backspace/delete of single chars — coalesces into one undo step
    Delete,
    /// a discrete edit (paste, newline, word-delete, selection-replace, a
    /// cursor move, undo/redo) that never coalesces with its neighbours
    Other,
}

/// A point-in-time buffer state for undo/redo. Cloning a `Rope` is cheap — it
/// shares structure via Arc — so snapshots cost next to nothing.
#[derive(Clone)]
struct Snapshot {
    rope: Rope,
    cursor: usize,
    anchor: Option<usize>,
}

pub struct Editor {
    pub rope: Rope,
    /// cursor as a char index into the rope (never inside a CRLF pair — we
    /// normalize to \n on load). This is the *moving* end of a selection.
    pub cursor: usize,
    /// selection origin: when `Some`, text between `anchor` and `cursor` is
    /// selected (the two may be equal → empty selection, treated as none).
    pub anchor: Option<usize>,
    pub dirty: bool,
    /// preferred column for vertical movement (sticky col)
    goal_col: Option<usize>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    /// kind of the last mutating edit, for undo coalescing
    last_edit: EditKind,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(&text.replace("\r\n", "\n")),
            cursor: 0,
            anchor: None,
            dirty: false,
            goal_col: None,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: EditKind::Other,
        }
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines()
    }

    /// (line, col) of the cursor, both 0-based
    pub fn line_col(&self) -> (usize, usize) {
        let line = self.rope.char_to_line(self.cursor);
        (line, self.cursor - self.rope.line_to_char(line))
    }

    pub fn line(&self, idx: usize) -> String {
        let l = self.rope.line(idx);
        // strip the trailing newline for display
        let s = l.to_string();
        s.strip_suffix('\n').map(|t| t.to_string()).unwrap_or(s)
    }

    // ── selection ────────────────────────────────────────────────────────

    /// The normalized selection as a half-open char range, or `None` when there
    /// is no (or an empty) selection.
    pub fn selection(&self) -> Option<Range<usize>> {
        let a = self.anchor?;
        let (s, e) = if a <= self.cursor {
            (a, self.cursor)
        } else {
            (self.cursor, a)
        };
        (s != e).then_some(s..e)
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection().map(|r| self.rope.slice(r).to_string())
    }

    /// If text is selected, remove it, collapse the cursor to its start, and
    /// report `true`. Otherwise just clear a stale (empty) anchor and report
    /// `false`. Callers run this before an insert/delete so typing replaces a
    /// selection like every editor.
    fn delete_selection(&mut self) -> bool {
        if let Some(r) = self.selection() {
            let start = r.start;
            self.rope.remove(r);
            self.cursor = start;
            self.anchor = None;
            self.dirty = true;
            self.goal_col = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    /// Begin/extend or clear the selection ahead of a cursor move.
    fn pre_move(&mut self, select: bool) {
        if select {
            if self.anchor.is_none() {
                self.anchor = Some(self.cursor);
            }
        } else {
            self.anchor = None;
        }
        // a cursor move ends the current typing/delete run, so the next edit
        // starts a fresh undo step
        self.last_edit = EditKind::Other;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.rope.len_chars();
        self.goal_col = None;
    }

    /// Select the word under the cursor (double-click). Falls back to the single
    /// char if the cursor isn't sitting on a word character.
    pub fn select_word_at_cursor(&mut self) {
        let len = self.rope.len_chars();
        let mut s = self.cursor;
        while s > 0 && self.char_at(s - 1).is_some_and(Self::is_word) {
            s -= 1;
        }
        let mut e = self.cursor;
        while e < len && self.char_at(e).is_some_and(Self::is_word) {
            e += 1;
        }
        if s == e {
            // not on a word char → select the single char under the cursor
            e = (self.cursor + 1).min(len);
            s = self.cursor.min(e);
        }
        self.anchor = Some(s);
        self.cursor = e;
        self.goal_col = None;
        self.last_edit = EditKind::Other;
    }

    /// Select the whole line under the cursor, including its trailing newline
    /// (triple-click).
    pub fn select_line_at_cursor(&mut self) {
        let (line, _) = self.line_col();
        let start = self.rope.line_to_char(line);
        let end = if line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(line + 1)
        } else {
            self.rope.len_chars()
        };
        self.anchor = Some(start);
        self.cursor = end;
        self.goal_col = None;
        self.last_edit = EditKind::Other;
    }

    // ── undo / redo ────────────────────────────────────────────────────────

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            rope: self.rope.clone(),
            cursor: self.cursor,
            anchor: self.anchor,
        }
    }

    /// Record the pre-edit state for undo. Consecutive edits of the same
    /// coalescing kind (a run of typing, or a run of deletes) fold into one undo
    /// step; `Other` always starts a fresh step. Any edit clears the redo stack.
    fn checkpoint(&mut self, kind: EditKind) {
        let coalesce = kind != EditKind::Other && self.last_edit == kind && !self.undo.is_empty();
        if !coalesce {
            self.undo.push(self.snapshot());
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
            }
        }
        self.redo.clear();
        self.last_edit = kind;
    }

    pub fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(self.snapshot());
            self.restore(prev);
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(self.snapshot());
            self.restore(next);
        }
    }

    fn restore(&mut self, s: Snapshot) {
        self.rope = s.rope;
        self.cursor = s.cursor;
        self.anchor = s.anchor;
        self.dirty = true;
        self.goal_col = None;
        self.last_edit = EditKind::Other;
    }

    fn char_at(&self, i: usize) -> Option<char> {
        (i < self.rope.len_chars()).then(|| self.rope.char(i))
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    // ── editing (selection-aware) ─────────────────────────────────────────

    pub fn insert(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        // single-char typing coalesces; replacing a selection, newlines, and
        // multi-char inserts (paste) are each their own undo step
        let kind = if self.selection().is_some() || s == "\n" || s.chars().count() > 1 {
            EditKind::Other
        } else {
            EditKind::Insert
        };
        self.checkpoint(kind);
        self.delete_selection();
        self.rope.insert(self.cursor, s);
        self.cursor += s.chars().count();
        self.dirty = true;
        self.goal_col = None;
    }

    pub fn backspace(&mut self) {
        let has_sel = self.selection().is_some();
        if !has_sel && self.cursor == 0 {
            return; // nothing to delete → no undo step
        }
        self.checkpoint(if has_sel {
            EditKind::Other
        } else {
            EditKind::Delete
        });
        if self.delete_selection() {
            return;
        }
        self.rope.remove(self.cursor - 1..self.cursor);
        self.cursor -= 1;
        self.dirty = true;
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        let has_sel = self.selection().is_some();
        if !has_sel && self.cursor >= self.rope.len_chars() {
            return; // nothing to delete → no undo step
        }
        self.checkpoint(if has_sel {
            EditKind::Other
        } else {
            EditKind::Delete
        });
        if self.delete_selection() {
            return;
        }
        self.rope.remove(self.cursor..self.cursor + 1);
        self.dirty = true;
        self.goal_col = None;
    }

    /// Ctrl+Backspace: delete the word to the left (or the selection).
    pub fn delete_word_left(&mut self) {
        if self.selection().is_none() && self.cursor == 0 {
            return;
        }
        self.checkpoint(EditKind::Other);
        if self.delete_selection() {
            return;
        }
        let end = self.cursor;
        self.word_left(false);
        if self.cursor < end {
            self.rope.remove(self.cursor..end);
            self.dirty = true;
        }
        self.goal_col = None;
    }

    /// Ctrl+Delete: delete the word to the right (or the selection).
    pub fn delete_word_right(&mut self) {
        if self.selection().is_none() && self.cursor >= self.rope.len_chars() {
            return;
        }
        self.checkpoint(EditKind::Other);
        if self.delete_selection() {
            return;
        }
        let start = self.cursor;
        self.word_right(false);
        if self.cursor > start {
            self.rope.remove(start..self.cursor);
            self.cursor = start;
            self.dirty = true;
        }
        self.goal_col = None;
    }

    // ── navigation ────────────────────────────────────────────────────────

    pub fn left(&mut self, select: bool) {
        // a plain Left over a selection collapses to its left edge
        if !select {
            if let Some(r) = self.selection() {
                self.cursor = r.start;
                self.anchor = None;
                self.goal_col = None;
                return;
            }
        }
        self.pre_move(select);
        self.cursor = self.cursor.saturating_sub(1);
        self.goal_col = None;
    }

    pub fn right(&mut self, select: bool) {
        if !select {
            if let Some(r) = self.selection() {
                self.cursor = r.end;
                self.anchor = None;
                self.goal_col = None;
                return;
            }
        }
        self.pre_move(select);
        if self.cursor < self.rope.len_chars() {
            self.cursor += 1;
        }
        self.goal_col = None;
    }

    pub fn up(&mut self, select: bool) {
        self.pre_move(select);
        self.vertical(-1);
    }

    pub fn down(&mut self, select: bool) {
        self.pre_move(select);
        self.vertical(1);
    }

    pub fn home(&mut self, select: bool) {
        self.pre_move(select);
        let (line, _) = self.line_col();
        self.cursor = self.rope.line_to_char(line);
        self.goal_col = None;
    }

    pub fn end(&mut self, select: bool) {
        self.pre_move(select);
        let (line, _) = self.line_col();
        self.cursor = self.rope.line_to_char(line) + self.line_len(line);
        self.goal_col = None;
    }

    /// Ctrl+Home — top of the buffer.
    pub fn doc_start(&mut self, select: bool) {
        self.pre_move(select);
        self.cursor = 0;
        self.goal_col = None;
    }

    /// Ctrl+End — end of the buffer.
    pub fn doc_end(&mut self, select: bool) {
        self.pre_move(select);
        self.cursor = self.rope.len_chars();
        self.goal_col = None;
    }

    /// Ctrl+Left — to the start of the previous word.
    pub fn word_left(&mut self, select: bool) {
        self.pre_move(select);
        let mut i = self.cursor;
        // skip whitespace immediately to the left
        while i > 0 && self.char_at(i - 1).is_some_and(char::is_whitespace) {
            i -= 1;
        }
        // then consume a run of the same class (word chars OR punctuation)
        if i > 0 {
            let word = Self::is_word(self.char_at(i - 1).unwrap());
            while i > 0 {
                let c = self.char_at(i - 1).unwrap();
                if c.is_whitespace() || Self::is_word(c) != word {
                    break;
                }
                i -= 1;
            }
        }
        self.cursor = i;
        self.goal_col = None;
    }

    /// Ctrl+Right — to the start of the next word.
    pub fn word_right(&mut self, select: bool) {
        self.pre_move(select);
        let len = self.rope.len_chars();
        let mut i = self.cursor;
        // consume the current run of same-class, non-whitespace chars
        if let Some(c) = self.char_at(i).filter(|c| !c.is_whitespace()) {
            let word = Self::is_word(c);
            while let Some(c) = self.char_at(i) {
                if c.is_whitespace() || Self::is_word(c) != word {
                    break;
                }
                i += 1;
            }
        }
        // then skip trailing whitespace to land on the next word's first char
        while i < len && self.char_at(i).is_some_and(char::is_whitespace) {
            i += 1;
        }
        self.cursor = i;
        self.goal_col = None;
    }

    fn line_len(&self, line: usize) -> usize {
        let start = self.rope.line_to_char(line);
        let end = if line + 1 < self.rope.len_lines() {
            self.rope.line_to_char(line + 1) - 1 // before the \n
        } else {
            self.rope.len_chars()
        };
        end - start
    }

    fn vertical(&mut self, delta: isize) {
        let (line, col) = self.line_col();
        let goal = *self.goal_col.get_or_insert(col);
        let target = line as isize + delta;
        if target < 0 || target as usize >= self.rope.len_lines() {
            return;
        }
        let target = target as usize;
        let col = goal.min(self.line_len(target));
        self.cursor = self.rope.line_to_char(target) + col;
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    /// Place the cursor at (line, col), clamped to the buffer. With `select`,
    /// extend the current selection (shift-click) instead of clearing it.
    pub fn set_cursor(&mut self, line: usize, col: usize, select: bool) {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let pos = self.rope.line_to_char(line) + col.min(self.line_len(line));
        self.pre_move(select);
        self.cursor = pos;
        self.goal_col = None;
    }

    /// Atomic save: write a sibling temp file, then rename over the target —
    /// a crash mid-write can never destroy the original (PLAN.md D4).
    pub fn save(&mut self, path: &Path) -> io::Result<()> {
        let tmp = path.with_extension("md.tmp~");
        fs::write(&tmp, self.text())?;
        fs::rename(&tmp, path)?;
        self.dirty = false;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shift_arrows_build_a_selection() {
        let mut e = Editor::new("hello world");
        assert!(e.selection().is_none());
        e.right(true); // shift-right ×3 → select "hel"
        e.right(true);
        e.right(true);
        assert_eq!(e.selection(), Some(0..3));
        assert_eq!(e.selected_text().as_deref(), Some("hel"));
    }

    #[test]
    fn plain_left_collapses_selection_to_its_start() {
        let mut e = Editor::new("hello");
        e.select_all();
        assert_eq!(e.selection(), Some(0..5));
        e.left(false); // plain Left over a selection → caret at left edge
        assert!(e.selection().is_none());
        assert_eq!(e.cursor, 0);
    }

    #[test]
    fn typing_replaces_the_selection() {
        let mut e = Editor::new("hello world");
        e.select_all();
        e.insert("hi"); // replaces the whole buffer
        assert_eq!(e.text(), "hi");
        assert!(e.selection().is_none());
        assert!(e.dirty);
    }

    #[test]
    fn word_motion_lands_on_word_starts() {
        let mut e = Editor::new("the quick fox");
        e.word_right(false);
        assert_eq!(e.cursor, 4, "after 'the ' → start of 'quick'");
        e.word_right(false);
        assert_eq!(e.cursor, 10, "→ start of 'fox'");
        e.word_left(false);
        assert_eq!(e.cursor, 4, "back to start of 'quick'");
    }

    #[test]
    fn shift_word_right_selects_a_word() {
        let mut e = Editor::new("alpha beta");
        e.word_right(true);
        assert_eq!(e.selected_text().as_deref(), Some("alpha "));
    }

    #[test]
    fn delete_word_left_removes_the_prior_word() {
        let mut e = Editor::new("one two");
        e.end(false); // caret after "two"
        e.delete_word_left();
        assert_eq!(e.text(), "one ");
    }

    #[test]
    fn doc_start_end_jump_to_buffer_bounds() {
        let mut e = Editor::new("a\nb\nc");
        e.doc_end(false);
        assert_eq!(e.cursor, e.rope.len_chars());
        e.doc_start(true); // select to top
        assert_eq!(e.selection(), Some(0..5));
    }

    #[test]
    fn undo_redo_roundtrips_typing() {
        let mut e = Editor::new("");
        e.insert("a");
        e.insert("b");
        e.insert("c"); // a run of typing → one undo step
        assert_eq!(e.text(), "abc");
        e.undo();
        assert_eq!(e.text(), "", "one undo removes the whole typing run");
        e.redo();
        assert_eq!(e.text(), "abc", "redo restores it");
    }

    #[test]
    fn cursor_move_splits_undo_runs() {
        let mut e = Editor::new("");
        e.insert("foo");
        e.left(false); // a move ends the run
        e.insert("X");
        assert_eq!(e.text(), "foXo");
        e.undo();
        assert_eq!(e.text(), "foo", "first undo drops only the post-move edit");
        e.undo();
        assert_eq!(e.text(), "", "second undo drops the earlier run");
    }

    #[test]
    fn undo_restores_a_replaced_selection() {
        let mut e = Editor::new("hello world");
        e.select_all();
        e.insert("hi");
        assert_eq!(e.text(), "hi");
        e.undo();
        assert_eq!(e.text(), "hello world", "the replaced text comes back");
    }

    #[test]
    fn redo_stack_clears_on_new_edit() {
        let mut e = Editor::new("");
        e.insert("a");
        e.undo();
        assert_eq!(e.text(), "");
        e.insert("b"); // a fresh edit invalidates the redo of "a"
        e.redo();
        assert_eq!(e.text(), "b", "redo is a no-op after a new edit");
    }

    #[test]
    fn double_click_selects_the_word() {
        let mut e = Editor::new("foo bar baz");
        e.set_cursor(0, 5, false); // inside "bar"
        e.select_word_at_cursor();
        assert_eq!(e.selected_text().as_deref(), Some("bar"));
    }

    #[test]
    fn triple_click_selects_the_line() {
        let mut e = Editor::new("one\ntwo\nthree");
        e.set_cursor(1, 1, false); // on the "two" line
        e.select_line_at_cursor();
        assert_eq!(e.selected_text().as_deref(), Some("two\n"));
    }
}
