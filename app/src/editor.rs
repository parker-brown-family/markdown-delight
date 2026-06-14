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
}

impl Editor {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(&text.replace("\r\n", "\n")),
            cursor: 0,
            anchor: None,
            dirty: false,
            goal_col: None,
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
        let (s, e) = if a <= self.cursor { (a, self.cursor) } else { (self.cursor, a) };
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
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.rope.len_chars();
        self.goal_col = None;
    }

    fn char_at(&self, i: usize) -> Option<char> {
        (i < self.rope.len_chars()).then(|| self.rope.char(i))
    }

    fn is_word(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    // ── editing (selection-aware) ─────────────────────────────────────────

    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        self.rope.insert(self.cursor, s);
        self.cursor += s.chars().count();
        self.dirty = true;
        self.goal_col = None;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor > 0 {
            self.rope.remove(self.cursor - 1..self.cursor);
            self.cursor -= 1;
            self.dirty = true;
        }
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        if self.cursor < self.rope.len_chars() {
            self.rope.remove(self.cursor..self.cursor + 1);
            self.dirty = true;
        }
        self.goal_col = None;
    }

    /// Ctrl+Backspace: delete the word to the left (or the selection).
    pub fn delete_word_left(&mut self) {
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
