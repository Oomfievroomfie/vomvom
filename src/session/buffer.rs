// In-memory buffer: text content + undo/redo stack.

use crate::session::db::{OpKind, UndoOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct Pos {
    pub line: usize,
    pub col: usize,
}

impl Pos {
    pub fn new(line: usize, col: usize) -> Self { Pos { line, col } }
}

#[derive(Debug, Clone)]
pub struct Buffer {
    pub id: i64,
    pub path: Option<String>,
    /// Lines of the document. Always at least one element.
    lines: Vec<String>,
    pub cursor: Pos,
    pub scroll_line: usize,
    /// True if content differs from what's on disk.
    pub is_modified: bool,
    /// True if this buffer has been modified since the last DB sync.
    pub dirty: bool,

    // Undo stack: ops[0..=undo_head] are undoable; ops[undo_head+1..] are redoable.
    // undo_head = usize::MAX means nothing is undoable.
    ops: Vec<UndoOp>,
    undo_head: Option<usize>,  // index into ops of the last applied op
    next_seq: i64,
}

impl Buffer {
    pub fn new(id: i64, path: Option<String>, content: &str) -> Self {
        let lines = content_to_lines(content);
        Buffer {
            id,
            path,
            lines,
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
        // Rebuild undo_head from current_seq
        self.next_seq = ops.last().map(|o| o.seq + 1).unwrap_or(0);
        self.undo_head = if current_seq < 0 {
            None
        } else {
            ops.iter().rposition(|o| o.seq == current_seq)
        };
        self.ops = ops;
    }

    pub fn content(&self) -> String {
        self.lines.join("\n")
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn line(&self, idx: usize) -> &str {
        &self.lines[idx.min(self.lines.len().saturating_sub(1))]
    }

    // ── Editing ──────────────────────────────────────────────────────────────

    /// Insert text at the current cursor position.
    pub fn insert(&mut self, text: &str) {
        let start = self.cursor;
        self.apply_insert(start, text);
        // Record op — truncate any redo branch first
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

    /// Delete the character before the cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor == (Pos::default()) {
            return;
        }
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

    /// Delete the character at the cursor (delete key).
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
                // Undo an insert = delete the inserted text
                let start = Pos::new(op.line_start as usize, op.col_start as usize);
                let end = Pos::new(op.line_end as usize, op.col_end as usize);
                self.apply_delete(start, end);
                self.cursor = start;
            }
            OpKind::Delete => {
                // Undo a delete = re-insert the deleted text
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
        let line = line.min(self.lines.len().saturating_sub(1));
        let col = col.min(self.lines[line].len());
        self.cursor = Pos::new(line, col);
    }

    // ── Undo serialization ───────────────────────────────────────────────────

    pub fn undo_ops(&self) -> &[UndoOp] {
        &self.ops
    }

    pub fn current_seq(&self) -> i64 {
        match self.undo_head {
            None => -1,
            Some(h) => self.ops[h].seq,
        }
    }

    // ── Internal helpers ─────────────────────────────────────────────────────

    fn truncate_redo(&mut self) {
        if let Some(h) = self.undo_head {
            self.ops.truncate(h + 1);
        } else {
            self.ops.clear();
        }
    }

    fn apply_insert(&mut self, at: Pos, text: &str) {
        let new_lines = content_to_lines(text);
        let tail = self.lines[at.line].split_off(at.col);

        if new_lines.len() == 1 {
            self.lines[at.line].push_str(&new_lines[0]);
            self.lines[at.line].push_str(&tail);
            self.cursor = Pos::new(at.line, at.col + new_lines[0].len());
        } else {
            self.lines[at.line].push_str(&new_lines[0]);
            let last_new = new_lines.last().unwrap().clone() + &tail;
            let extra: Vec<String> = new_lines[1..new_lines.len()-1].to_vec()
                .into_iter().chain(std::iter::once(last_new)).collect();
            let insert_at = at.line + 1;
            for (i, l) in extra.into_iter().enumerate() {
                self.lines.insert(insert_at + i, l);
            }
            let last_line = at.line + new_lines.len() - 1;
            let last_col = new_lines.last().unwrap().len();
            self.cursor = Pos::new(last_line, last_col);
        }
    }

    fn apply_delete(&mut self, start: Pos, end: Pos) {
        if start == end { return; }
        if start.line == end.line {
            self.lines[start.line].drain(start.col..end.col);
        } else {
            let tail = self.lines[end.line][end.col..].to_string();
            self.lines[start.line].truncate(start.col);
            self.lines[start.line].push_str(&tail);
            self.lines.drain(start.line + 1..=end.line);
        }
        self.cursor = start;
    }

    fn char_at(&self, pos: Pos) -> char {
        if pos.line < self.lines.len() {
            let line = &self.lines[pos.line];
            if pos.col < line.len() {
                return line[pos.col..].chars().next().unwrap_or('\n');
            }
        }
        '\n'
    }

    fn prev_pos(&self, pos: Pos) -> Pos {
        if pos.col > 0 {
            // step back one char
            let line = &self.lines[pos.line];
            let byte_idx = line[..pos.col].char_indices().next_back().map(|(i,_)| i).unwrap_or(0);
            Pos::new(pos.line, byte_idx)
        } else if pos.line > 0 {
            let prev_line = pos.line - 1;
            Pos::new(prev_line, self.lines[prev_line].len())
        } else {
            pos
        }
    }

    fn next_pos(&self, pos: Pos) -> Pos {
        if pos.line < self.lines.len() {
            let line = &self.lines[pos.line];
            if pos.col < line.len() {
                let next_col = pos.col + line[pos.col..].chars().next().map(|c| c.len_utf8()).unwrap_or(1);
                Pos::new(pos.line, next_col)
            } else if pos.line + 1 < self.lines.len() {
                Pos::new(pos.line + 1, 0)
            } else {
                pos
            }
        } else {
            pos
        }
    }
}

fn content_to_lines(content: &str) -> Vec<String> {
    if content.is_empty() {
        return vec![String::new()];
    }
    content.split('\n').map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(content: &str) -> Buffer {
        Buffer::new(1, None, content)
    }

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
        assert!(b.undo());
        assert_eq!(b.content(), "ab");
        assert!(b.undo());
        assert_eq!(b.content(), "a");
        assert!(b.undo());
        assert_eq!(b.content(), "");
        assert!(!b.undo()); // nothing left to undo
    }

    #[test]
    fn test_redo_after_new_edit_clears_branch() {
        let mut b = buf("");
        b.insert("a");
        b.insert("b");
        b.undo();
        b.insert("c"); // this should clear the redo branch
        assert!(!b.redo());
        assert_eq!(b.content(), "ac");
    }

    #[test]
    fn test_undo_nothing() {
        let mut b = buf("hello");
        assert!(!b.undo());
    }

    #[test]
    fn test_redo_nothing() {
        let mut b = buf("hello");
        assert!(!b.redo());
    }

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
        b.insert("a");
        assert_eq!(b.current_seq(), 0);
        b.insert("b");
        assert_eq!(b.current_seq(), 1);
        b.undo();
        assert_eq!(b.current_seq(), 0);
        b.undo();
        assert_eq!(b.current_seq(), -1);
    }

    #[test]
    fn test_restore_undo_roundtrip() {
        let mut b = buf("");
        b.insert("hello");
        b.insert(" world");
        let ops: Vec<UndoOp> = b.undo_ops().to_vec();
        let seq = b.current_seq();

        let mut b2 = buf("hello world");
        b2.restore_undo(ops, seq);
        assert!(b2.undo());
        assert_eq!(b2.content(), "hello");
        assert!(b2.undo());
        assert_eq!(b2.content(), "");
    }
}
