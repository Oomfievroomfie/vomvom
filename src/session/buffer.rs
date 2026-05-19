// In-memory buffer: text content (ropey rope) + undo/redo stack.
//
// Pos::col is a char offset within the line (not byte offset).

use crate::session::db::{OpKind, UndoOp};
use ropey::Rope;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiskStatus {
    Ok,       // file matches what editor last read/wrote
    Diverged, // file exists but mtime differs from when we loaded/saved
    Deleted,  // path is set but file no longer exists
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,  // char offset within line
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self { Pos { line, col } }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum EditKind { Insert, Backspace, DeleteForward }

pub struct Buffer {
    pub id: i64,
    pub path: Option<String>,
    rope: Rope,
    pub cursor: Pos,
    /// Selection anchor. When Some, the selection spans (anchor, cursor) in document order.
    /// Each future cursor in a multicursor setup would carry its own anchor.
    pub anchor: Option<Pos>,
    pub scroll_line: usize,
    pub is_modified: bool,
    pub dirty: bool,
    /// mtime (secs since unix epoch) of the file when last loaded or saved.
    /// None for untitled buffers.
    pub disk_mtime: Option<u64>,
    pub disk_status: DiskStatus,
    pub last_mtime_check: Option<Instant>,

    ops: Vec<UndoOp>,
    undo_head: Option<usize>,
    next_seq: i64,
    next_group: i64,
    last_edit_kind: Option<EditKind>,
    last_edit_time: Option<Instant>,
    last_insert_was_separator: bool,
}

impl Buffer {
    pub fn new(id: i64, path: Option<String>, content: &str) -> Self {
        Buffer {
            id,
            path,
            rope: Rope::from_str(content),
            cursor: Pos::default(),
            anchor: None,
            scroll_line: 0,
            is_modified: false,
            dirty: false,
            disk_mtime: None,
            disk_status: DiskStatus::Ok,
            last_mtime_check: None,
            ops: Vec::new(),
            undo_head: None,
            next_seq: 0,
            next_group: 0,
            last_edit_kind: None,
            last_edit_time: None,
            last_insert_was_separator: false,
        }
    }

    /// Read current file mtime without updating state. Returns None for untitled or missing file.
    pub fn current_disk_mtime(path: &str) -> Option<u64> {
        std::fs::metadata(path).ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
    }

    /// Check disk mtime and update disk_status. Call sparsely.
    pub fn refresh_disk_status(&mut self) {
        self.last_mtime_check = Some(Instant::now());
        let Some(ref path) = self.path else { return };
        match Self::current_disk_mtime(path) {
            None => { self.disk_status = DiskStatus::Deleted; }
            Some(mtime) => {
                self.disk_status = match self.disk_mtime {
                    Some(expected) if expected == mtime => DiskStatus::Ok,
                    _ => DiskStatus::Diverged,
                };
            }
        }
    }

    pub fn restore_undo(&mut self, ops: Vec<UndoOp>, current_seq: i64) {
        self.next_seq = ops.last().map(|o| o.seq + 1).unwrap_or(0);
        self.next_group = ops.iter().map(|o| o.group_id + 1).max().unwrap_or(0);
        self.undo_head = if current_seq < 0 {
            None
        } else {
            ops.iter().rposition(|o| o.seq == current_seq)
        };
        self.ops = ops;
        self.last_edit_kind = None;
        self.last_edit_time = None;
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

    fn is_insert_separator(text: &str) -> bool {
        text.chars().all(|c| c == ' ' || c == '\n' || c == '\t')
    }

    // Returns the group_id for the current edit, advancing next_group when a
    // new group is needed.
    //
    // Insert grouping: spaces/tabs/newlines split from a preceding word but
    // chain with the following word (non-sep→sep = split, sep→non-sep = no split).
    //
    // Backspace/delete grouping: newlines always force a split (each line's
    // worth of deletion is its own group); spaces/tabs do not split.
    fn current_group(&mut self, kind: EditKind, text: &str) -> i64 {
        let now = Instant::now();
        let timed_out = self.last_edit_time.map_or(true, |t| now.duration_since(t).as_secs() >= 4);
        let split = match &self.last_edit_kind {
            None => true,
            Some(k) => {
                *k != kind
                || timed_out
                || match kind {
                    EditKind::Insert => {
                        let incoming_sep = Self::is_insert_separator(text);
                        incoming_sep && !self.last_insert_was_separator
                    }
                    EditKind::Backspace | EditKind::DeleteForward => {
                        text.contains('\n') && !self.last_insert_was_separator
                    }
                }
            }
        };
        if split {
            self.next_group += 1;
        }
        self.last_edit_kind = Some(kind);
        self.last_edit_time = Some(now);
        self.last_insert_was_separator = text.chars().all(|c| matches!(c, ' ' | '\t' | '\n' | '\r'));
        self.next_group
    }

    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let start = self.cursor;
        self.apply_insert(start, text);
        self.truncate_redo();
        let end = self.cursor;
        let seq = self.next_seq;
        self.next_seq += 1;
        let group_id = self.current_group(EditKind::Insert, text);
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Insert,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: text.to_string(),
            group_id,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
    }

    pub fn backspace(&mut self) {
        if self.delete_selection() { return; }
        if self.cursor == Pos::default() { return; }
        let end = self.cursor;
        let start = self.prev_pos(end);
        let deleted = self.char_at(start).to_string();
        self.apply_delete(start, end);
        self.truncate_redo();
        let seq = self.next_seq;
        self.next_seq += 1;
        let group_id = self.current_group(EditKind::Backspace, &deleted);
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Delete,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: deleted,
            group_id,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
    }

    pub fn delete_forward(&mut self) {
        if self.delete_selection() { return; }
        let start = self.cursor;
        let end = self.next_pos(start);
        if start == end { return; }
        let deleted = self.char_at(start).to_string();
        self.apply_delete(start, end);
        self.truncate_redo();
        let seq = self.next_seq;
        self.next_seq += 1;
        let group_id = self.current_group(EditKind::DeleteForward, &deleted);
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Delete,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: deleted,
            group_id,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
    }

    // Break the current undo group so the next edit starts a new group.
    // Call this after cursor moves, undo/redo, or any non-edit action.
    pub fn break_undo_group(&mut self) {
        self.last_edit_kind = None;
        self.last_edit_time = None;
        self.last_insert_was_separator = false;
    }

    pub fn undo(&mut self) -> bool {
        let head = match self.undo_head {
            None => return false,
            Some(h) => h,
        };
        let group = self.ops[head].group_id;
        // Walk backwards applying all ops in this group.
        let mut idx = head;
        loop {
            let op = self.ops[idx].clone();
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
            if idx == 0 || self.ops[idx - 1].group_id != group {
                self.undo_head = idx.checked_sub(1);
                break;
            }
            idx -= 1;
        }
        self.break_undo_group();
        self.dirty = true;
        true
    }

    pub fn redo(&mut self) -> bool {
        let next_idx = match self.undo_head {
            None => 0,
            Some(h) => h + 1,
        };
        if next_idx >= self.ops.len() { return false; }
        let group = self.ops[next_idx].group_id;
        // Walk forwards applying all ops in this group.
        let mut idx = next_idx;
        loop {
            let op = self.ops[idx].clone();
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
            let next = idx + 1;
            if next >= self.ops.len() || self.ops[next].group_id != group {
                self.undo_head = Some(idx);
                break;
            }
            idx += 1;
        }
        self.break_undo_group();
        self.dirty = true;
        true
    }

    /// Returns (start, end) of the current selection in document order, or None if no selection.
    pub fn selection_range(&self) -> Option<(Pos, Pos)> {
        let anchor = self.anchor?;
        if anchor == self.cursor { return None; }
        if anchor < self.cursor { Some((anchor, self.cursor)) } else { Some((self.cursor, anchor)) }
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
    }

    /// Set anchor to current cursor position only if no selection is active.
    pub fn set_anchor_if_none(&mut self) {
        if self.anchor.is_none() {
            self.anchor = Some(self.cursor);
        }
    }

    /// If there is an active selection, delete it and return true.
    pub fn delete_selection(&mut self) -> bool {
        let Some((start, end)) = self.selection_range() else { return false };
        let sc = self.pos_to_char(start);
        let ec = self.pos_to_char(end);
        let deleted: String = self.rope.chars_at(sc).take(ec - sc).collect();
        self.apply_delete(start, end);
        self.truncate_redo();
        let seq = self.next_seq;
        self.next_seq += 1;
        let group_id = self.current_group(EditKind::DeleteForward, &deleted);
        self.ops.push(crate::session::db::UndoOp {
            seq,
            kind: crate::session::db::OpKind::Delete,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: deleted,
            group_id,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
        self.anchor = None;
        true
    }

    /// Selected text as a String, or empty string if no selection.
    pub fn selected_text(&self) -> String {
        let Some((start, end)) = self.selection_range() else { return String::new() };
        let sc = self.pos_to_char(start);
        let ec = self.pos_to_char(end);
        self.rope.chars_at(sc).take(ec - sc).collect()
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

    fn char_class(c: char) -> u8 {
        if c == '\n' { 2 } else if c.is_whitespace() { 0 } else if c.is_alphanumeric() || c == '_' { 1 } else { 2 }
    }

    pub fn word_start_left(&self, pos: Pos) -> Pos {
        let mut ci = self.pos_to_char(pos);
        if ci == 0 { return pos; }
        // step back over whitespace
        while ci > 0 && Self::char_class(self.rope.char(ci - 1)) == 0 { ci -= 1; }
        if ci == 0 { return self.char_to_pos(ci); }
        let cls = Self::char_class(self.rope.char(ci - 1));
        while ci > 0 && Self::char_class(self.rope.char(ci - 1)) == cls { ci -= 1; }
        self.char_to_pos(ci)
    }

    pub fn word_end_right(&self, pos: Pos) -> Pos {
        let len = self.rope.len_chars();
        let mut ci = self.pos_to_char(pos);
        if ci >= len { return pos; }
        // step over whitespace
        while ci < len && Self::char_class(self.rope.char(ci)) == 0 { ci += 1; }
        if ci >= len { return self.char_to_pos(ci); }
        let cls = Self::char_class(self.rope.char(ci));
        while ci < len && Self::char_class(self.rope.char(ci)) == cls { ci += 1; }
        self.char_to_pos(ci)
    }

    pub fn backspace_word(&mut self) {
        if self.delete_selection() { return; }
        let end = self.cursor;
        let start = self.word_start_left(end);
        if start == end { return; }
        let sc = self.pos_to_char(start);
        let ec = self.pos_to_char(end);
        let deleted: String = self.rope.chars_at(sc).take(ec - sc).collect();
        self.apply_delete(start, end);
        self.truncate_redo();
        let seq = self.next_seq;
        self.next_seq += 1;
        let group_id = self.current_group(EditKind::Backspace, &deleted);
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Delete,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: deleted,
            group_id,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
    }

    pub fn delete_forward_word(&mut self) {
        if self.delete_selection() { return; }
        let start = self.cursor;
        let end = self.word_end_right(start);
        if start == end { return; }
        let sc = self.pos_to_char(start);
        let ec = self.pos_to_char(end);
        let deleted: String = self.rope.chars_at(sc).take(ec - sc).collect();
        self.apply_delete(start, end);
        self.truncate_redo();
        let seq = self.next_seq;
        self.next_seq += 1;
        let group_id = self.current_group(EditKind::DeleteForward, &deleted);
        self.ops.push(UndoOp {
            seq,
            kind: OpKind::Delete,
            line_start: start.line as i64,
            col_start: start.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
            text: deleted,
            group_id,
        });
        self.undo_head = Some(self.ops.len() - 1);
        self.is_modified = true;
        self.dirty = true;
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
        // Without explicit group breaks, consecutive same-kind inserts group together.
        let mut b = buf("");
        b.insert("a");
        b.insert("b");
        b.insert("c");
        assert!(b.undo()); assert_eq!(b.content(), "");
        assert!(!b.undo());
    }

    #[test]
    fn test_undo_chain_with_breaks() {
        let mut b = buf("");
        b.insert("a"); b.break_undo_group();
        b.insert("b"); b.break_undo_group();
        b.insert("c"); b.break_undo_group();
        assert!(b.undo()); assert_eq!(b.content(), "ab");
        assert!(b.undo()); assert_eq!(b.content(), "a");
        assert!(b.undo()); assert_eq!(b.content(), "");
        assert!(!b.undo());
    }

    #[test]
    fn test_redo_after_new_edit_clears_branch() {
        let mut b = buf("");
        b.insert("a"); b.break_undo_group();
        b.insert("b");
        b.undo();
        b.insert("c");
        assert!(!b.redo());
        assert_eq!(b.content(), "ac");
    }

    #[test]
    fn test_undo_group_space_splits() {
        let mut b = buf("");
        b.insert("hello");
        b.insert(" ");
        b.insert("world");
        // "hello"=group1, " world"=group2 (space splits from preceding word;
        // "world" chains onto the space since sep→non-sep doesn't split)
        assert!(b.undo()); assert_eq!(b.content(), "hello");
        assert!(b.undo()); assert_eq!(b.content(), "");
    }

    #[test]
    fn test_undo_group_consecutive_spaces() {
        let mut b = buf("");
        b.insert("hello");
        b.insert(" ");
        b.insert(" ");
        b.insert(" ");
        // three spaces chain together as one separator group
        assert!(b.undo()); assert_eq!(b.content(), "hello");
        assert!(b.undo()); assert_eq!(b.content(), "");
    }

    #[test]
    fn test_undo_group_backspace_separate_from_insert() {
        let mut b = buf("hello");
        b.move_cursor(0, 5);
        b.insert("x");
        b.insert("y");
        b.backspace();
        b.backspace();
        // inserts and backspaces are different kinds — each kind is its own group
        assert!(b.undo()); assert_eq!(b.content(), "helloxy");
        assert!(b.undo()); assert_eq!(b.content(), "hello");
    }

    #[test]
    fn test_undo_group_backspace_newline_splits() {
        let mut b = buf("hello\nworld");
        b.move_cursor(1, 5);
        for _ in 0..5 { b.backspace(); } // delete "world"
        b.backspace();                    // delete "\n" — splits (prev was non-whitespace)
        for _ in 0..5 { b.backspace(); } // delete "hello" — chains
        assert!(b.undo()); assert_eq!(b.content(), "hello\n");
        assert!(b.undo()); assert_eq!(b.content(), "hello\nworld");
    }

    #[test]
    fn test_undo_group_backspace_whitespace_only_lines() {
        let mut b = buf("hello\n  \n  \nworld");
        // move to end of "world"
        b.move_cursor(3, 5);
        for _ in 0..5 { b.backspace(); } // delete "world"
        b.backspace(); b.backspace(); b.backspace(); // delete "\n  " (newline + spaces on line 2)
        b.backspace(); b.backspace(); b.backspace(); // delete "\n  " (newline + spaces on line 1... wait)
        // Actually backspacing from end of "world": w,o,r,l,d then \n then spaces then \n then spaces then \n then "hello"
        // All whitespace-only lines crossed without non-whitespace → should all chain as one group after "world" split
        // But wait, "world" itself is non-whitespace, so when we hit the first \n, prev was 'd' (non-ws) → split
        // Then subsequent whitespace chains. Let's just verify the two-group result.
        for _ in 0..5 { b.backspace(); } // delete "hello"
        // "world" = group1, "\n  \n  \nhello" = group2
        assert!(b.undo()); assert_eq!(b.content(), "hello\n  \n  \n");
        assert!(b.undo()); assert_eq!(b.content(), "hello\n  \n  \nworld");
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
        b.break_undo_group();
        b.insert("b"); assert_eq!(b.current_seq(), 1);
        b.undo();      assert_eq!(b.current_seq(), 0);
        b.undo();      assert_eq!(b.current_seq(), -1);
    }

    #[test]
    fn test_restore_undo_roundtrip() {
        let mut b = buf("");
        for c in "hello".chars() { b.insert(&c.to_string()); }
        b.insert(" ");
        for c in "world".chars() { b.insert(&c.to_string()); }
        let ops = b.undo_ops().to_vec();
        let seq = b.current_seq();
        let mut b2 = buf("hello world");
        b2.restore_undo(ops, seq);
        assert!(b2.undo()); assert_eq!(b2.content(), "hello");
        assert!(b2.undo()); assert_eq!(b2.content(), "");
    }
}
