//! Session result persistence (`ARCHITECTURE.md` §4/§6, D-0010). Two tiers:
//!
//! - An **in-memory map** of completed (and in-flight) sessions, always populated, so the
//!   authoritative result of any session in this process survives a WS disconnect and is
//!   retrievable via `GET /v1/sessions/{id}` regardless of the opt-in history flag.
//! - **Opt-in SQLite** (rusqlite, D-0010): when history is enabled, finalized records are
//!   also written to a local file so they survive a daemon restart.
//!
//! `rusqlite::Connection` is `!Send`/`!Sync`, so this store never holds one open: the worker
//! thread opens a short-lived connection per write, and reads open their own — SQLite allows
//! multiple connections to one file. Records are stored as a JSON blob keyed by id, which keeps
//! the schema forward-compatible with the evolving [`SessionRecord`] contract.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use rusqlite::Connection;

use super::api::SessionRecord;

/// In-memory + optional-SQLite session store.
pub struct SessionStore {
    mem: Mutex<HashMap<String, SessionRecord>>,
    /// `Some` when opt-in history is enabled (D-0010); the path of the SQLite file.
    db_path: Option<PathBuf>,
}

impl SessionStore {
    /// Build a store. When `db_path` is `Some`, create the parent dir and the schema up front
    /// (fail fast if the location is unwritable) so the first write cannot silently drop data.
    pub fn new(db_path: Option<PathBuf>) -> Result<Self, String> {
        if let Some(path) = &db_path {
            if let Some(parent) = path.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("create history dir {}: {e}", parent.display()))?;
                }
            }
            let conn = Connection::open(path)
                .map_err(|e| format!("open history db {}: {e}", path.display()))?;
            conn.execute(
                "CREATE TABLE IF NOT EXISTS sessions (
                     id TEXT PRIMARY KEY,
                     created_at_ms INTEGER NOT NULL,
                     json TEXT NOT NULL
                 )",
                [],
            )
            .map_err(|e| format!("create schema: {e}"))?;
        }
        Ok(Self {
            mem: Mutex::new(HashMap::new()),
            db_path,
        })
    }

    /// Whether opt-in history persistence is active.
    pub fn history_enabled(&self) -> bool {
        self.db_path.is_some()
    }

    /// Upsert a record into the in-memory map (always) and SQLite (when history is enabled).
    /// The in-memory upsert is infallible; a SQLite write error is returned so the caller can
    /// log it without losing the in-memory copy.
    pub fn upsert(&self, record: &SessionRecord) -> Result<(), String> {
        self.mem
            .lock()
            .expect("session mem mutex")
            .insert(record.session_id.clone(), record.clone());

        if let Some(path) = &self.db_path {
            let json = serde_json::to_string(record).map_err(|e| e.to_string())?;
            let conn = Connection::open(path).map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT INTO sessions (id, created_at_ms, json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET json = excluded.json,
                                               created_at_ms = excluded.created_at_ms",
                rusqlite::params![record.session_id, record.created_at_ms as i64, json],
            )
            .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Fetch a record: in-memory first (covers this process's lifetime), then SQLite (covers a
    /// prior run when history is enabled).
    pub fn get(&self, id: &str) -> Option<SessionRecord> {
        if let Some(rec) = self.mem.lock().expect("session mem mutex").get(id).cloned() {
            return Some(rec);
        }
        let path = self.db_path.as_ref()?;
        let conn = Connection::open(path).ok()?;
        let json: String = conn
            .query_row(
                "SELECT json FROM sessions WHERE id = ?1",
                rusqlite::params![id],
                |row| row.get(0),
            )
            .ok()?;
        serde_json::from_str(&json).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::api::{CleanupInfo, LatencyInfo, SessionStatusDto};

    fn rec(id: &str, text: &str) -> SessionRecord {
        SessionRecord {
            session_id: id.to_string(),
            status: SessionStatusDto::Completed,
            text: text.to_string(),
            lang: "cs".to_string(),
            cleanup: CleanupInfo {
                applied: false,
                skip_reason: Some("gate_clean".to_string()),
            },
            latency: LatencyInfo::default(),
            error: None,
            created_at_ms: 42,
        }
    }

    #[test]
    fn in_memory_get_returns_czech_text_byte_exact() {
        let store = SessionStore::new(None).unwrap();
        let text = "Příliš žluťoučký kůň úpěl ďábelské ódy.";
        store.upsert(&rec("s1", text)).unwrap();
        assert_eq!(store.get("s1").unwrap().text, text);
        assert!(store.get("missing").is_none());
    }

    #[test]
    fn sqlite_persists_across_store_instances() {
        let dir = std::env::temp_dir().join(format!("whisperd-store-test-{}", std::process::id()));
        let db = dir.join("history.db");
        let _ = std::fs::remove_file(&db);
        let text = "Ďábelské ódy a žluťoučký kůň.";
        {
            let store = SessionStore::new(Some(db.clone())).unwrap();
            store.upsert(&rec("s2", text)).unwrap();
        }
        // A fresh store (simulating a restart) reads the record back from disk.
        let store2 = SessionStore::new(Some(db.clone())).unwrap();
        assert_eq!(store2.get("s2").unwrap().text, text);
        let _ = std::fs::remove_file(&db);
    }
}
