pub mod db;
pub mod buffer;

use std::time::{Duration, Instant};
use rusqlite::Connection;
use crate::session::buffer::{Buffer, DiskStatus};
use crate::session::db::{
    load_all_buffers, load_undo_ops, load_undo_state,
    insert_buffer, sync_buffer, sync_undo_ops,
    get_active_buffer_id, set_active_buffer_id, touch_last_sync,
};

pub const SYNC_INTERVAL: Duration = Duration::from_secs(5 * 60);
const MTIME_CHECK_INTERVAL: Duration = Duration::from_secs(5);

pub struct Session {
    // None for demo/screenshot sessions that must not touch SQLite.
    conn: Option<Connection>,
    pub buffers: Vec<Buffer>,
    pub active_idx: usize,
    last_sync: Instant,
}

impl Session {
    /// Open (or create) the session DB, load all previous state.
    pub fn open(db_path: &str) -> rusqlite::Result<Self> {
        let conn = db::open(db_path)?;
        let rows = load_all_buffers(&conn)?;
        let active_id = get_active_buffer_id(&conn)?;

        let mut buffers: Vec<Buffer> = Vec::with_capacity(rows.len());
        for row in rows {
            let mut buf = Buffer::new(row.id, row.path, &row.content);
            buf.is_modified = row.is_modified;
            buf.cursor = crate::session::buffer::Pos::new(row.cursor_line as usize, row.cursor_col as usize);
            buf.scroll_line = row.scroll_line as usize;
            buf.disk_mtime = row.disk_mtime;

            let ops = load_undo_ops(&conn, row.id)?;
            let cur_seq = load_undo_state(&conn, row.id)?;
            buf.restore_undo(ops, cur_seq);

            // Check disk status immediately on load so icons are correct from the start.
            buf.refresh_disk_status();

            buffers.push(buf);
        }

        // If no buffers exist, create a blank untitled one.
        let active_idx = if buffers.is_empty() {
            let id = insert_buffer(&conn, None, "")?;
            buffers.push(Buffer::new(id, None, ""));
            0
        } else {
            // Find the active buffer by id, default to 0.
            active_id
                .and_then(|aid| buffers.iter().position(|b| b.id == aid))
                .unwrap_or(0)
        };

        Ok(Session {
            conn: Some(conn),
            buffers,
            active_idx,
            last_sync: Instant::now(),
        })
    }

    /// Create a purely in-memory session with no SQLite connection.
    /// Safe to use in screenshot/demo contexts where no persistence is wanted.
    /// Access the underlying DB connection (None for demo sessions).
    pub fn conn(&self) -> Option<&Connection> {
        self.conn.as_ref()
    }

    pub fn new_demo() -> Self {
        Session {
            conn: None,
            buffers: vec![Buffer::new(0, None, "")],
            active_idx: 0,
            last_sync: Instant::now(),
        }
    }

    pub fn active(&self) -> &Buffer {
        &self.buffers[self.active_idx]
    }

    pub fn active_mut(&mut self) -> &mut Buffer {
        &mut self.buffers[self.active_idx]
    }

    pub fn set_active(&mut self, idx: usize) {
        if idx < self.buffers.len() {
            self.active_idx = idx;
        }
    }

    /// Open a new buffer for a file path. Returns its index.
    pub fn open_file(&mut self, path: &str) -> rusqlite::Result<usize> {
        // Check if already open.
        if let Some(idx) = self.buffers.iter().position(|b| b.path.as_deref() == Some(path)) {
            return Ok(idx);
        }
        let content = std::fs::read_to_string(path).unwrap_or_default();
        let id = if let Some(conn) = &self.conn {
            insert_buffer(conn, Some(path), &content)?
        } else {
            self.buffers.len() as i64
        };
        let mut buf = Buffer::new(id, Some(path.to_string()), &content);
        buf.disk_mtime = Buffer::current_disk_mtime(path);
        buf.disk_status = DiskStatus::Ok;
        self.buffers.push(buf);
        Ok(self.buffers.len() - 1)
    }

    /// Call this on every frame/tick. Syncs dirty buffers if the interval has elapsed.
    /// Also sparsely refreshes disk status for the active buffer.
    pub fn tick(&mut self) -> rusqlite::Result<()> {
        if self.conn.is_some() && self.last_sync.elapsed() >= SYNC_INTERVAL {
            self.sync_now()?;
        }
        let buf = &mut self.buffers[self.active_idx];
        let needs_check = buf.path.is_some() && buf.last_mtime_check
            .map_or(true, |t| t.elapsed() >= MTIME_CHECK_INTERVAL);
        if needs_check {
            buf.refresh_disk_status();
        }
        Ok(())
    }

    /// Force an immediate sync of all dirty buffers.
    pub fn sync_now(&mut self) -> rusqlite::Result<()> {
        let conn = match &mut self.conn {
            Some(c) => c,
            None => return Ok(()), // no-op for demo sessions
        };
        let tx = conn.transaction()?;
        let active_id = self.buffers.get(self.active_idx).map(|b| b.id);

        for buf in &mut self.buffers {
            if !buf.dirty { continue; }
            sync_buffer(
                &tx, buf.id,
                &buf.content(),
                buf.cursor.line as i64,
                buf.cursor.col as i64,
                buf.scroll_line as i64,
                buf.is_modified,
                buf.disk_mtime,
            )?;
            sync_undo_ops(&tx, buf.id, buf.undo_ops(), buf.current_seq())?;
            buf.dirty = false;
        }

        set_active_buffer_id(&tx, active_id)?;
        touch_last_sync(&tx)?;
        tx.commit()?;
        self.last_sync = Instant::now();
        Ok(())
    }

    /// Write a file's content to disk and clear its modified flag.
    pub fn save_active(&mut self) -> std::io::Result<()> {
        let buf = self.active_mut();
        let path = buf.path.clone().ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "buffer has no path")
        })?;
        std::fs::write(&path, buf.content())?;
        buf.disk_mtime = Buffer::current_disk_mtime(&path);
        buf.disk_status = DiskStatus::Ok;
        buf.is_modified = false;
        buf.dirty = true; // need to persist the is_modified=false and disk_mtime
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db() -> String {
        // Use a unique in-memory path per test via a temp file name
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("vomvom_test_{}.db", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()));
        tmp.to_string_lossy().into_owned()
    }

    #[test]
    fn test_session_creates_blank_buffer_on_first_open() {
        let path = temp_db();
        let session = Session::open(&path).unwrap();
        assert_eq!(session.buffers.len(), 1);
        assert_eq!(session.active().content(), "");
        assert_eq!(session.active_idx, 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_session_persists_across_open() {
        let path = temp_db();
        {
            let mut session = Session::open(&path).unwrap();
            session.active_mut().insert("hello world");
            session.sync_now().unwrap();
        }
        {
            let session = Session::open(&path).unwrap();
            assert_eq!(session.active().content(), "hello world");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_session_undo_persists() {
        let path = temp_db();
        {
            let mut session = Session::open(&path).unwrap();
            session.active_mut().insert("abc");
            session.active_mut().break_undo_group();
            session.active_mut().insert("def");
            session.active_mut().undo();
            session.sync_now().unwrap();
        }
        {
            let mut session = Session::open(&path).unwrap();
            assert_eq!(session.active().content(), "abc");
            // Should be able to redo
            assert!(session.active_mut().redo());
            assert_eq!(session.active().content(), "abcdef");
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_session_active_buffer_persists() {
        let path = temp_db();
        {
            let mut session = Session::open(&path).unwrap();
            // Open a second buffer
            let conn = session.conn().unwrap();
            let id2 = db::insert_buffer(conn, Some("/tmp/other.rs"), "x").unwrap();
            let buf2 = Buffer::new(id2, Some("/tmp/other.rs".into()), "x");
            session.buffers.push(buf2);
            session.set_active(1);
            session.sync_now().unwrap();
        }
        {
            let session = Session::open(&path).unwrap();
            assert_eq!(session.active_idx, 1);
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_sync_only_dirty_buffers() {
        let path = temp_db();
        let mut session = Session::open(&path).unwrap();
        session.active_mut().insert("x");
        assert!(session.active().dirty);
        session.sync_now().unwrap();
        assert!(!session.active().dirty);
        // Sync again — should be a no-op (nothing dirty)
        session.sync_now().unwrap();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_tick_does_not_sync_before_interval() {
        let path = temp_db();
        let mut session = Session::open(&path).unwrap();
        session.active_mut().insert("y");
        // tick immediately — interval hasn't elapsed, should not sync
        session.tick().unwrap();
        assert!(session.active().dirty); // still dirty
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn test_multiple_buffers_persist() {
        let path = temp_db();
        {
            let mut session = Session::open(&path).unwrap();
            session.active_mut().insert("buffer one");
            let id2 = db::insert_buffer(session.conn().unwrap(), Some("/b.rs"), "buffer two").unwrap();
            session.buffers.push(Buffer::new(id2, Some("/b.rs".into()), "buffer two"));
            session.sync_now().unwrap();
        }
        {
            let session = Session::open(&path).unwrap();
            assert_eq!(session.buffers.len(), 2);
            assert_eq!(session.buffers[0].content(), "buffer one");
            assert_eq!(session.buffers[1].content(), "buffer two");
        }
        let _ = std::fs::remove_file(&path);
    }
}
