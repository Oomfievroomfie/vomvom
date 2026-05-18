// In-memory buffer: text content (ropey rope) + undo/redo stack.
//
// Pos::col is a char offset within the line (not byte offset).

use crate::session::db::{OpKind, UndoOp};
use ropey::Rope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,  // char offset within line
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self { Pos { line, col } }
}

#[derive(Debug, Clone)]
pub struct Buffer {
    pub id: i64,
    pub path: Option<String>,
    rope: Rope,
    pub cursor: Pos,
    pub scroll_line: usize,
    pub is_modified: bool,
    pub dirty: bool,

    ops: Vec<UndoOp>,
    undo_head: Option<usize>,
    next_seq: i64,
}

impl Buffer {
    pub fn new(id: i64, path: Option<String>, content: &str) -> Self {
        Buffer {
            id,
            path,
            rope: Rope::from_str(content),
            cursor: Pos::default(),
            scroll_line: 0,
            is_modified: false,
            dirty: false,
            ops: Vec::new(),
            undo_head: None,
            next_seq: 0,
        }
    }

    pub fn restore_undo(&mut self, ops: Vec<UndoOp>, current_seq: i64) {
        self.next_seq = ops.last().map(|o| o.seq + 1).unwrap_or(0);
        self.undo_head = if current_seq < 0 {
            None
        } else {
            ops.iter().rposition(|o| o.seq == current_seq)
        };
        self.ops = ops;
    }

    pub fn content(&self) -> String {
        self.rope.to_string()
    }

    pub fn line_count(&self) -> usize {
        // ropey counts a trailing newline as starting an extra line; match old behaviour.
        self.rope.len_lines()
    }

    /// Return line `idx` as a String, without the trailing newline.
    pub fn line(&self, idx: usize) -> String {
        let idx = idx.min(self.rope.len_lines().saturating_sub(1));
        let slice = self.rope.line(idx);
        // Strip trailing newline chars (\n or \r\n).
        let s: String = slice.chars().collect();
        s.trim_end_matches('\n').trim_end_matches('\r').to_string()
    }

    // ── Editing ───────────────────────────────────────────────────────────────

    pub fn insert(&mut self, text: &str) {
        let start = self.cursor;
        self.apply_insert(start, text);
        self.truncate_redo();
        let end = self.cursor;
        let seq = self.next_seq;
        self.next_seq += 1;
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Insert,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: text.to_string(),
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
    }

    pub fn backspace(&mut self) {
        if self.cursor == Pos::default() { return; }
        let end = self.cursor;
        let start = self.prev_pos(end);
        let deleted = self.char_at(start).to_string();
        self.apply_delete(start, end);
        self.truncate_redo();
        let seq = self.next_seq;
        self.next_seq += 1;
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Delete,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: deleted,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
    }

    pub fn delete_forward(&mut self) {
        let start = self.cursor;
        let end = self.next_pos(start);
        if start == end { return; }
        let deleted = self.char_at(start).to_string();
        self.apply_delete(start, end);
        self.truncate_redo();
        let seq = self.next_seq;
        self.next_seq += 1;
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Delete,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: deleted,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
    }

    pub fn undo(&mut self) -> bool {
        let head = match self.undo_head {
            None => return false,
            Some(h) => h,
        };
        let op = self.ops[head].clone();
        match op.kind {
            OpKind::Insert => {
                let start = Pos::new(op.line_start as usize, op.col_start as usize);
                let end = Pos::new(op.line_end as usize, op.col_end as usize);
                self.apply_delete(start, end);
                self.cursor = start;
            }
            OpKind::Delete => {
                let start = Pos::new(op.line_start as usize, op.col_start as usize);
                self.apply_insert(start, &op.text.clone());
                self.cursor = Pos::new(op.line_end as usize, op.col_end as usize);
            }
        }
        self.undo_head = head.checked_sub(1);
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let next_idx = match self.undo_head {
            None => 0,
            Some(h) => h + 1,
        };
        if next_idx >= self.ops.len() { return false; }
        let op = self.ops[next_idx].clone();
        match op.kind {
            OpKind::Insert => {
                let start = Pos::new(op.line_start as usize, op.col_start as usize);
                self.apply_insert(start, &op.text.clone());
                self.cursor = Pos::new(op.line_end as usize, op.col_end as usize);
            }
            OpKind::Delete => {
                let start = Pos::new(op.line_start as usize, op.col_start as usize);
                let end = Pos::new(op.line_end as usize, op.col_end as usize);
                self.apply_delete(start, end);
                self.cursor = start;
            }
        }
        self.undo_head = Some(next_idx);
        self.dirty = true;
        true
    }

    pub fn move_cursor(&mut self, line: usize, col: usize) {
        let line = line.min(self.rope.len_lines().saturating_sub(1));
        let line_len = self.rope.line(line).len_chars()
            .saturating_sub(if self.rope.line(line).to_string().ends_with('\n') { 1 } else { 0 });
        let col = col.min(line_len);
        self.cursor = Pos::new(line, col);
    }

    // ── Undo serialization ────────────────────────────────────────────────────

    pub fn undo_ops(&self) -> &[UndoOp] { &self.ops }

    pub fn current_seq(&self) -> i64 {
        match self.undo_head {
            None => -1,
            Some(h) => self.ops[h].seq,
        }
    }

    // ── Internal helpers ──────────────────────────────────────────────────────

    fn pos_to_char(&self, pos: Pos) -> usize {
        let line_start = self.rope.line_to_char(pos.line.min(self.rope.len_lines().saturating_sub(1)));
        let line_len = self.rope.line(pos.line.min(self.rope.len_lines().saturating_sub(1))).len_chars();
        // don't step past the newline at end of line
        let max_col = line_len.saturating_sub(
            if self.rope.line(pos.line.min(self.rope.len_lines().saturating_sub(1))).to_string().ends_with('\n') { 1 } else { 0 }
        );
        line_start + pos.col.min(max_col)
    }

    fn char_to_pos(&self, char_idx: usize) -> Pos {
        let char_idx = char_idx.min(self.rope.len_chars());
        let line = self.rope.char_to_line(char_idx);
        let line_start = self.rope.line_to_char(line);
        Pos::new(line, char_idx - line_start)
    }

    fn truncate_redo(&mut self) {
        if let Some(h) = self.undo_head {
            self.ops.truncate(h + 1);
        } else {
            self.ops.clear();
        }
    }

    fn apply_insert(&mut self, at: Pos, text: &str) {
        let char_idx = self.pos_to_char(at);
        self.rope.insert(char_idx, text);
        let text_chars = text.chars().count();
        self.cursor = self.char_to_pos(char_idx + text_chars);
    }

    fn apply_delete(&mut self, start: Pos, end: Pos) {
        let sc = self.pos_to_char(start);
        let ec = self.pos_to_char(end);
        if sc < ec {
            self.rope.remove(sc..ec);
        }
        self.cursor = start;
    }

    fn char_at(&self, pos: Pos) -> char {
        let ci = self.pos_to_char(pos);
        self.rope.char(ci.min(self.rope.len_chars().saturating_sub(1)))
    }

    fn prev_pos(&self, pos: Pos) -> Pos {
        let ci = self.pos_to_char(pos);
        if ci == 0 { return pos; }
        self.char_to_pos(ci - 1)
    }

    fn next_pos(&self, pos: Pos) -> Pos {
        let ci = self.pos_to_char(pos);
        if ci >= self.rope.len_chars() { return pos; }
        self.char_to_pos(ci + 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(content: &str) -> Buffer { Buffer::new(1, None, content) }

    #[test]
    fn test_insert_simple() {
        let mut b = buf("");
        b.insert("hello");
        assert_eq!(b.content(), "hello");
        assert_eq!(b.cursor, Pos::new(0, 5));
    }

    #[test]
    fn test_insert_newline() {
        let mut b = buf("ab");
        b.move_cursor(0, 1);
        b.insert("\n");
        assert_eq!(b.content(), "a\nb");
        assert_eq!(b.cursor, Pos::new(1, 0));
    }

    #[test]
    fn test_backspace() {
        let mut b = buf("hello");
        b.move_cursor(0, 5);
        b.backspace();
        assert_eq!(b.content(), "hell");
        assert_eq!(b.cursor, Pos::new(0, 4));
    }

    #[test]
    fn test_backspace_across_newline() {
        let mut b = buf("a\nb");
        b.move_cursor(1, 0);
        b.backspace();
        assert_eq!(b.content(), "ab");
        assert_eq!(b.cursor, Pos::new(0, 1));
    }

    #[test]
    fn test_delete_forward() {
        let mut b = buf("hello");
        b.move_cursor(0, 0);
        b.delete_forward();
        assert_eq!(b.content(), "ello");
    }

    #[test]
    fn test_undo_insert() {
        let mut b = buf("");
        b.insert("abc");
        assert!(b.undo());
        assert_eq!(b.content(), "");
        assert_eq!(b.cursor, Pos::new(0, 0));
    }

    #[test]
    fn test_redo_insert() {
        let mut b = buf("");
        b.insert("abc");
        b.undo();
        assert!(b.redo());
        assert_eq!(b.content(), "abc");
        assert_eq!(b.cursor, Pos::new(0, 3));
    }

    #[test]
    fn test_undo_backspace() {
        let mut b = buf("hello");
        b.move_cursor(0, 5);
        b.backspace();
        assert!(b.undo());
        assert_eq!(b.content(), "hello");
        assert_eq!(b.cursor, Pos::new(0, 5));
    }

    #[test]
    fn test_undo_chain() {
        let mut b = buf("");
        b.insert("a");
        b.insert("b");
        b.insert("c");
        assert!(b.undo()); assert_eq!(b.content(), "ab");
        assert!(b.undo()); assert_eq!(b.content(), "a");
        assert!(b.undo()); assert_eq!(b.content(), "");
        assert!(!b.undo());
    }

    #[test]
    fn test_redo_after_new_edit_clears_branch() {
        let mut b = buf("");
        b.insert("a");
        b.insert("b");
        b.undo();
        b.insert("c");
        assert!(!b.redo());
        assert_eq!(b.content(), "ac");
    }

    #[test]
    fn test_undo_nothing() { assert!(!buf("hello").undo()); }

    #[test]
    fn test_redo_nothing() { assert!(!buf("hello").redo()); }

    #[test]
    fn test_multiline_insert() {
        let mut b = buf("ac");
        b.move_cursor(0, 1);
        b.insert("b\n");
        assert_eq!(b.content(), "ab\nc");
        assert_eq!(b.cursor, Pos::new(1, 0));
        b.undo();
        assert_eq!(b.content(), "ac");
    }

    #[test]
    fn test_dirty_flag() {
        let mut b = buf("x");
        assert!(!b.dirty);
        b.move_cursor(0, 1);
        b.insert("y");
        assert!(b.dirty);
        b.dirty = false;
        b.undo();
        assert!(b.dirty);
    }

    #[test]
    fn test_current_seq_tracks_undo() {
        let mut b = buf("");
        assert_eq!(b.current_seq(), -1);
        b.insert("a"); assert_eq!(b.current_seq(), 0);
        b.insert("b"); assert_eq!(b.current_seq(), 1);
        b.undo();      assert_eq!(b.current_seq(), 0);
        b.undo();      assert_eq!(b.current_seq(), -1);
    }

    #[test]
    fn test_restore_undo_roundtrip() {
        let mut b = buf("");
        b.insert("hello");
        b.insert(" world");
        let ops = b.undo_ops().to_vec();
        let seq = b.current_seq();
        let mut b2 = buf("hello world");
        b2.restore_undo(ops, seq);
        assert!(b2.undo()); assert_eq!(b2.content(), "hello");
        assert!(b2.undo()); assert_eq!(b2.content(), "");
    }
}
