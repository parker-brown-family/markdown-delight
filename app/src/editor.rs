//! editor.rs — the G0b editor core: rope buffer + cursor + edits + atomic save.
//!
//! Built clean-room on ropey (MIT) per docs/PLAN.md §2 D1 — Zed's GPL editor
//! crate is study-only and was not consulted. Deliberately small: char-index
//! cursor, line/col navigation, insert/delete, and write-temp-then-rename
//! save. Multi-cursor, selections, undo arrive on top of this seam later.

use std::{fs, io, path::Path};

use ropey::Rope;

pub struct Editor {
    pub rope: Rope,
    /// cursor as a char index into the rope (never inside a CRLF pair — we
    /// normalize to \n on load)
    pub cursor: usize,
    pub dirty: bool,
    /// preferred column for vertical movement (sticky col)
    goal_col: Option<usize>,
}

impl Editor {
    pub fn new(text: &str) -> Self {
        Self {
            rope: Rope::from_str(&text.replace("\r\n", "\n")),
            cursor: 0,
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

    pub fn insert(&mut self, s: &str) {
        self.rope.insert(self.cursor, s);
        self.cursor += s.chars().count();
        self.dirty = true;
        self.goal_col = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.rope.remove(self.cursor - 1..self.cursor);
            self.cursor -= 1;
            self.dirty = true;
        }
        self.goal_col = None;
    }

    pub fn delete(&mut self) {
        if self.cursor < self.rope.len_chars() {
            self.rope.remove(self.cursor..self.cursor + 1);
            self.dirty = true;
        }
        self.goal_col = None;
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
        self.goal_col = None;
    }

    pub fn right(&mut self) {
        if self.cursor < self.rope.len_chars() {
            self.cursor += 1;
        }
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

    pub fn up(&mut self) {
        self.vertical(-1);
    }

    pub fn down(&mut self) {
        self.vertical(1);
    }

    pub fn home(&mut self) {
        let (line, _) = self.line_col();
        self.cursor = self.rope.line_to_char(line);
        self.goal_col = None;
    }

    pub fn end(&mut self) {
        let (line, _) = self.line_col();
        self.cursor = self.rope.line_to_char(line) + self.line_len(line);
        self.goal_col = None;
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
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
