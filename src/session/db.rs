// SQLite-backed session persistence.
//
// Crash safety: all writes happen inside transactions. The DB is never left
// in a partially-written state — if the process dies mid-sync, the previous
// committed state is intact and loads cleanly on next boot.
//
// Schema version is stored in user_version PRAGMA. Migrations run forward-only.

use rusqlite::{Connection, params};

pub const SCHEMA_VERSION: u32 = 1;

pub fn open(path: &str) -> rusqlite::Result<Connection> {
    let conn = Connection::open(path)?;
    // WAL mode: readers don't block writers and vice versa; also survives crashes better.
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")?;
    migrate(&conn)?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version < 1 {
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS buffers (
                id          INTEGER PRIMARY KEY,
                path        TEXT,           -- NULL for untitled buffers
                content     TEXT NOT NULL DEFAULT '',
                cursor_line INTEGER NOT NULL DEFAULT 0,
                cursor_col  INTEGER NOT NULL DEFAULT 0,
                scroll_line INTEGER NOT NULL DEFAULT 0,
                is_modified INTEGER NOT NULL DEFAULT 0,  -- unsaved vs disk
                created_at  INTEGER NOT NULL DEFAULT (strftime('%s','now')),
                updated_at  INTEGER NOT NULL DEFAULT (strftime('%s','now'))
            );

            CREATE TABLE IF NOT EXISTS undo_ops (
                id          INTEGER PRIMARY KEY,
                buffer_id   INTEGER NOT NULL REFERENCES buffers(id) ON DELETE CASCADE,
                seq         INTEGER NOT NULL,   -- monotonically increasing per buffer
                kind        TEXT NOT NULL,      -- 'insert' | 'delete'
                line_start  INTEGER NOT NULL,
                col_start   INTEGER NOT NULL,
                line_end    INTEGER NOT NULL,
                col_end     INTEGER NOT NULL,
                text        TEXT NOT NULL,
                UNIQUE(buffer_id, seq)
            );

            -- current undo position per buffer (head of undo stack)
            CREATE TABLE IF NOT EXISTS undo_state (
                buffer_id   INTEGER PRIMARY KEY REFERENCES buffers(id) ON DELETE CASCADE,
                current_seq INTEGER NOT NULL DEFAULT -1  -- -1 = nothing undoable
            );

            CREATE TABLE IF NOT EXISTS session (
                id          INTEGER PRIMARY KEY CHECK (id = 1),
                active_buffer_id INTEGER REFERENCES buffers(id),
                last_sync_at INTEGER NOT NULL DEFAULT 0
            );

            INSERT OR IGNORE INTO session (id, last_sync_at) VALUES (1, 0);

            CREATE INDEX IF NOT EXISTS idx_undo_ops_buffer ON undo_ops(buffer_id, seq);

            PRAGMA user_version = 1;
        ")?;
    }
    Ok(())
}

// ── Buffer CRUD ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct BufferRow {
    pub id: i64,
    pub path: Option<String>,
    pub content: String,
    pub cursor_line: i64,
    pub cursor_col: i64,
    pub scroll_line: i64,
    pub is_modified: bool,
}

pub fn load_all_buffers(conn: &Connection) -> rusqlite::Result<Vec<BufferRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, path, content, cursor_line, cursor_col, scroll_line, is_modified
         FROM buffers ORDER BY id"
    )?;
    let rows = stmt.query_map([], |r| Ok(BufferRow {
        id: r.get(0)?,
        path: r.get(1)?,
        content: r.get(2)?,
        cursor_line: r.get(3)?,
        cursor_col: r.get(4)?,
        scroll_line: r.get(5)?,
        is_modified: r.get::<_, i64>(6)? != 0,
    }))?;
    rows.collect()
}

pub fn insert_buffer(conn: &Connection, path: Option<&str>, content: &str) -> rusqlite::Result<i64> {
    conn.execute(
        "INSERT INTO buffers (path, content) VALUES (?1, ?2)",
        params![path, content],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Sync a dirty buffer's content and cursor state. Called inside a transaction.
pub fn sync_buffer(conn: &Connection, id: i64, content: &str, cursor_line: i64, cursor_col: i64, scroll_line: i64, is_modified: bool) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE buffers SET content=?1, cursor_line=?2, cursor_col=?3,
         scroll_line=?4, is_modified=?5,
         updated_at=strftime('%s','now')
         WHERE id=?6",
        params![content, cursor_line, cursor_col, scroll_line, is_modified as i64, id],
    )?;
    Ok(())
}

pub fn delete_buffer(conn: &Connection, id: i64) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM buffers WHERE id=?1", params![id])?;
    Ok(())
}

// ── Undo/redo ops ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub enum OpKind { Insert, Delete }

#[derive(Debug, Clone)]
pub struct UndoOp {
    pub seq: i64,
    pub kind: OpKind,
    pub line_start: i64,
    pub col_start: i64,
    pub line_end: i64,
    pub col_end: i64,
    pub text: String,
}

pub fn load_undo_ops(conn: &Connection, buffer_id: i64) -> rusqlite::Result<Vec<UndoOp>> {
    let mut stmt = conn.prepare(
        "SELECT seq, kind, line_start, col_start, line_end, col_end, text
         FROM undo_ops WHERE buffer_id=?1 ORDER BY seq"
    )?;
    let rows = stmt.query_map(params![buffer_id], |r| {
        let kind_str: String = r.get(1)?;
        Ok(UndoOp {
            seq: r.get(0)?,
            kind: if kind_str == "insert" { OpKind::Insert } else { OpKind::Delete },
            line_start: r.get(2)?,
            col_start: r.get(3)?,
            line_end: r.get(4)?,
            col_end: r.get(5)?,
            text: r.get(6)?,
        })
    })?;
    rows.collect()
}

pub fn load_undo_state(conn: &Connection, buffer_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT current_seq FROM undo_state WHERE buffer_id=?1",
        params![buffer_id],
        |r| r.get(0),
    ).or(Ok(-1))
}

/// Replace the undo history for a buffer. Called inside a transaction.
/// Deletes ops beyond current_seq (the redo branch) then appends new ops.
pub fn sync_undo_ops(
    conn: &Connection,
    buffer_id: i64,
    ops: &[UndoOp],
    current_seq: i64,
) -> rusqlite::Result<()> {
    // Wipe and rewrite — simplest crash-safe approach for a 5-minute sync cadence.
    conn.execute("DELETE FROM undo_ops WHERE buffer_id=?1", params![buffer_id])?;
    conn.execute("DELETE FROM undo_state WHERE buffer_id=?1", params![buffer_id])?;

    let mut stmt = conn.prepare(
        "INSERT INTO undo_ops (buffer_id, seq, kind, line_start, col_start, line_end, col_end, text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
    )?;
    for op in ops {
        stmt.execute(params![
            buffer_id,
            op.seq,
            if op.kind == OpKind::Insert { "insert" } else { "delete" },
            op.line_start, op.col_start, op.line_end, op.col_end,
            op.text,
        ])?;
    }

    conn.execute(
        "INSERT INTO undo_state (buffer_id, current_seq) VALUES (?1, ?2)",
        params![buffer_id, current_seq],
    )?;
    Ok(())
}

// ── Session metadata ─────────────────────────────────────────────────────────

pub fn get_active_buffer_id(conn: &Connection) -> rusqlite::Result<Option<i64>> {
    conn.query_row(
        "SELECT active_buffer_id FROM session WHERE id=1",
        [],
        |r| r.get(0),
    )
}

pub fn set_active_buffer_id(conn: &Connection, id: Option<i64>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE session SET active_buffer_id=?1 WHERE id=1",
        params![id],
    )?;
    Ok(())
}

pub fn touch_last_sync(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE session SET last_sync_at=strftime('%s','now') WHERE id=1",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn test_schema_creates_cleanly() {
        let conn = mem_db();
        // Should be able to run migrate twice without error (idempotent).
        migrate(&conn).unwrap();
    }

    #[test]
    fn test_insert_and_load_buffer() {
        let conn = mem_db();
        let id = insert_buffer(&conn, Some("/tmp/foo.rs"), "fn main() {}").unwrap();
        let buffers = load_all_buffers(&conn).unwrap();
        assert_eq!(buffers.len(), 1);
        assert_eq!(buffers[0].id, id);
        assert_eq!(buffers[0].path.as_deref(), Some("/tmp/foo.rs"));
        assert_eq!(buffers[0].content, "fn main() {}");
        assert!(!buffers[0].is_modified);
    }

    #[test]
    fn test_insert_untitled_buffer() {
        let conn = mem_db();
        let id = insert_buffer(&conn, None, "hello").unwrap();
        let buffers = load_all_buffers(&conn).unwrap();
        assert_eq!(buffers[0].path, None);
        assert_eq!(buffers[0].id, id);
    }

    #[test]
    fn test_sync_buffer() {
        let conn = mem_db();
        let id = insert_buffer(&conn, Some("/a.rs"), "old").unwrap();
        sync_buffer(&conn, id, "new content", 5, 10, 2, true).unwrap();
        let buffers = load_all_buffers(&conn).unwrap();
        assert_eq!(buffers[0].content, "new content");
        assert_eq!(buffers[0].cursor_line, 5);
        assert_eq!(buffers[0].cursor_col, 10);
        assert_eq!(buffers[0].scroll_line, 2);
        assert!(buffers[0].is_modified);
    }

    #[test]
    fn test_delete_buffer_cascades_undo() {
        let conn = mem_db();
        let id = insert_buffer(&conn, None, "x").unwrap();
        let ops = vec![UndoOp {
            seq: 0, kind: OpKind::Insert,
            line_start: 0, col_start: 0, line_end: 0, col_end: 1,
            text: "x".into(),
        }];
        sync_undo_ops(&conn, id, &ops, 0).unwrap();
        delete_buffer(&conn, id).unwrap();
        // undo_ops and undo_state should be gone
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM undo_ops WHERE buffer_id=?1", params![id], |r| r.get(0)
        ).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn test_undo_ops_round_trip() {
        let conn = mem_db();
        let id = insert_buffer(&conn, None, "abc").unwrap();
        let ops = vec![
            UndoOp { seq: 0, kind: OpKind::Insert, line_start: 0, col_start: 0, line_end: 0, col_end: 3, text: "abc".into() },
            UndoOp { seq: 1, kind: OpKind::Delete, line_start: 0, col_start: 1, line_end: 0, col_end: 2, text: "b".into() },
        ];
        sync_undo_ops(&conn, id, &ops, 1).unwrap();
        let loaded = load_undo_ops(&conn, id).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].kind, OpKind::Insert);
        assert_eq!(loaded[0].text, "abc");
        assert_eq!(loaded[1].kind, OpKind::Delete);
        assert_eq!(loaded[1].seq, 1);
        let cur = load_undo_state(&conn, id).unwrap();
        assert_eq!(cur, 1);
    }

    #[test]
    fn test_undo_ops_wipe_on_resync() {
        let conn = mem_db();
        let id = insert_buffer(&conn, None, "").unwrap();
        let ops1 = vec![
            UndoOp { seq: 0, kind: OpKind::Insert, line_start: 0, col_start: 0, line_end: 0, col_end: 1, text: "a".into() },
            UndoOp { seq: 1, kind: OpKind::Insert, line_start: 0, col_start: 1, line_end: 0, col_end: 2, text: "b".into() },
        ];
        sync_undo_ops(&conn, id, &ops1, 1).unwrap();
        // Now undo once and resync with only op[0]
        let ops2 = vec![
            UndoOp { seq: 0, kind: OpKind::Insert, line_start: 0, col_start: 0, line_end: 0, col_end: 1, text: "a".into() },
        ];
        sync_undo_ops(&conn, id, &ops2, 0).unwrap();
        let loaded = load_undo_ops(&conn, id).unwrap();
        assert_eq!(loaded.len(), 1);
        let cur = load_undo_state(&conn, id).unwrap();
        assert_eq!(cur, 0);
    }

    #[test]
    fn test_session_active_buffer() {
        let conn = mem_db();
        let id = insert_buffer(&conn, Some("/x.rs"), "").unwrap();
        set_active_buffer_id(&conn, Some(id)).unwrap();
        let active = get_active_buffer_id(&conn).unwrap();
        assert_eq!(active, Some(id));
        set_active_buffer_id(&conn, None).unwrap();
        let active = get_active_buffer_id(&conn).unwrap();
        assert_eq!(active, None);
    }

    #[test]
    fn test_multiple_buffers_ordering() {
        let conn = mem_db();
        insert_buffer(&conn, Some("/a.rs"), "a").unwrap();
        insert_buffer(&conn, Some("/b.rs"), "b").unwrap();
        insert_buffer(&conn, None, "c").unwrap();
        let buffers = load_all_buffers(&conn).unwrap();
        assert_eq!(buffers.len(), 3);
        assert_eq!(buffers[0].path.as_deref(), Some("/a.rs"));
        assert_eq!(buffers[2].path, None);
    }

    #[test]
    fn test_touch_last_sync() {
        let conn = mem_db();
        touch_last_sync(&conn).unwrap();
        let ts: i64 = conn.query_row(
            "SELECT last_sync_at FROM session WHERE id=1", [], |r| r.get(0)
        ).unwrap();
        assert!(ts > 0);
    }
}
