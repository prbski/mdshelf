//! The SQLite sidecar.
//!
//! Everything here is disposable (D13/NFR-3). `rules_index` is derived from vault
//! frontmatter and rebuilt on boot; deleting the database costs only live sessions and
//! access history. Nothing in this file is a source of truth for who may read what —
//! that lives in the markdown.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

/// Bumped whenever the schema changes shape. A database written by a newer mdshelf is
/// refused rather than silently misread.
pub const SCHEMA_VERSION: i64 = 1;

/// Outcome recorded for a request in the access log (D27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allow,
    Deny,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Allow => "allow",
            Outcome::Deny => "deny",
        }
    }
}

/// A live session row.
#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub email: String,
    pub created_at: i64,
    pub last_seen_at: i64,
    pub refresh_token_enc: Option<Vec<u8>>,
}

/// One entry from the access log.
#[derive(Debug, Clone)]
pub struct AccessEntry {
    pub email: String,
    pub path: String,
    pub ts: i64,
    pub outcome: String,
}

/// Handle to the sidecar database.
///
/// rusqlite connections are not `Sync`, so a mutex guards a single connection. Requests
/// hold it only for the duration of one statement; if that ever becomes a bottleneck the
/// fix is a connection pool, not a change in ownership.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (creating if needed) the database at `path` and apply the schema.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating database directory {}", parent.display()))?;
        }
        let conn = Connection::open(path)
            .with_context(|| format!("opening database {}", path.display()))?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.apply_schema()?;
        Ok(store)
    }

    /// An in-memory database, used by tests.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self {
            conn: Mutex::new(conn),
        };
        store.apply_schema()?;
        Ok(store)
    }

    fn apply_schema(&self) -> Result<()> {
        let conn = self.lock();
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        let existing: Option<i64> = conn
            .query_row(
                "SELECT value FROM mdshelf_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap_or(None);

        if let Some(version) = existing
            && version > SCHEMA_VERSION
        {
            bail!(
                "database schema version {} is newer than this mdshelf understands ({}). \
                 Upgrade mdshelf, or delete the database to start fresh (this only \
                 discards sessions and access history).",
                version,
                SCHEMA_VERSION
            );
        }

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS mdshelf_meta (
                key   TEXT PRIMARY KEY,
                value INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                id                TEXT PRIMARY KEY,
                email             TEXT NOT NULL,
                created_at        INTEGER NOT NULL,
                last_seen_at      INTEGER NOT NULL,
                refresh_token_enc BLOB
            );
            CREATE INDEX IF NOT EXISTS sessions_email_idx ON sessions(email);

            CREATE TABLE IF NOT EXISTS access_log (
                email   TEXT NOT NULL,
                path    TEXT NOT NULL,
                ts      INTEGER NOT NULL,
                outcome TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS access_log_path_idx  ON access_log(path, ts);
            CREATE INDEX IF NOT EXISTS access_log_email_idx ON access_log(email, ts);
            CREATE INDEX IF NOT EXISTS access_log_ts_idx    ON access_log(ts);

            CREATE TABLE IF NOT EXISTS rules_index (
                site   TEXT NOT NULL,
                path   TEXT NOT NULL,
                level  TEXT NOT NULL,
                effect TEXT NOT NULL,
                email  TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS rules_index_lookup ON rules_index(site, path);
            "#,
        )?;

        conn.execute(
            "INSERT INTO mdshelf_meta(key, value) VALUES ('schema_version', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![SCHEMA_VERSION],
        )?;
        Ok(())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    // ---- sessions -------------------------------------------------------------

    pub fn insert_session(
        &self,
        id: &str,
        email: &str,
        now: i64,
        refresh_token_enc: Option<&[u8]>,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO sessions(id, email, created_at, last_seen_at, refresh_token_enc)
             VALUES (?1, ?2, ?3, ?3, ?4)",
            params![id, email, now, refresh_token_enc],
        )?;
        Ok(())
    }

    pub fn get_session(&self, id: &str) -> Result<Option<SessionRecord>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, email, created_at, last_seen_at, refresh_token_enc
                 FROM sessions WHERE id = ?1",
                params![id],
                |row| {
                    Ok(SessionRecord {
                        id: row.get(0)?,
                        email: row.get(1)?,
                        created_at: row.get(2)?,
                        last_seen_at: row.get(3)?,
                        refresh_token_enc: row.get(4)?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn touch_session(&self, id: &str, now: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE sessions SET last_seen_at = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    pub fn delete_session(&self, id: &str) -> Result<()> {
        self.lock()
            .execute("DELETE FROM sessions WHERE id = ?1", params![id])?;
        Ok(())
    }

    /// Remove every session belonging to an address. Used by erasure and by explicit
    /// force-logout.
    pub fn delete_sessions_for_email(&self, email: &str) -> Result<usize> {
        let removed = self
            .lock()
            .execute("DELETE FROM sessions WHERE email = ?1", params![email])?;
        Ok(removed)
    }

    /// Move a session's timestamps into the past so age- and idle-dependent behaviour
    /// can be tested without sleeping.
    #[cfg(any(test, feature = "test-support"))]
    pub fn backdate_session(&self, id: &str, created_at: i64, last_seen_at: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE sessions SET created_at = ?2, last_seen_at = ?3 WHERE id = ?1",
            params![id, created_at, last_seen_at],
        )?;
        Ok(())
    }

    pub fn count_sessions(&self) -> Result<i64> {
        let conn = self.lock();
        let count = conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))?;
        Ok(count)
    }

    // ---- access log -----------------------------------------------------------

    pub fn log_access(&self, email: &str, path: &str, ts: i64, outcome: Outcome) -> Result<()> {
        self.lock().execute(
            "INSERT INTO access_log(email, path, ts, outcome) VALUES (?1, ?2, ?3, ?4)",
            params![email, path, ts, outcome.as_str()],
        )?;
        Ok(())
    }

    pub fn access_by_path(&self, path: &str) -> Result<Vec<AccessEntry>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT email, path, ts, outcome FROM access_log WHERE path = ?1 ORDER BY ts DESC",
        )?;
        let rows = stmt
            .query_map(params![path], map_access_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn access_by_email(&self, email: &str) -> Result<Vec<AccessEntry>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT email, path, ts, outcome FROM access_log WHERE email = ?1 ORDER BY ts DESC",
        )?;
        let rows = stmt
            .query_map(params![email], map_access_entry)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Addresses that appear in the access log, used by `acl doctor` to spot grants
    /// that nobody has ever exercised.
    pub fn seen_emails(&self) -> Result<Vec<String>> {
        let conn = self.lock();
        let mut stmt = conn.prepare("SELECT DISTINCT email FROM access_log")?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Delete log entries older than `retention` relative to `now`.
    pub fn prune_access_log(&self, now: i64, retention: Duration) -> Result<usize> {
        let cutoff = now - (retention.as_secs() as i64) * 1000;
        let removed = self
            .lock()
            .execute("DELETE FROM access_log WHERE ts < ?1", params![cutoff])?;
        Ok(removed)
    }

    /// GDPR erasure: drop every log entry and session for an address (US-21).
    pub fn forget_email(&self, email: &str) -> Result<(usize, usize)> {
        let conn = self.lock();
        let entries = conn.execute("DELETE FROM access_log WHERE email = ?1", params![email])?;
        let sessions = conn.execute("DELETE FROM sessions WHERE email = ?1", params![email])?;
        Ok((entries, sessions))
    }

    // ---- derived rule index ---------------------------------------------------

    /// Replace the whole derived index for one site. Called on boot and whenever the
    /// watcher reports a change.
    pub fn replace_rules_for_site(&self, site: &str, rows: &[RuleRow]) -> Result<()> {
        let mut conn = self.lock();
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM rules_index WHERE site = ?1", params![site])?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO rules_index(site, path, level, effect, email)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for row in rows {
                stmt.execute(params![site, row.path, row.level, row.effect, row.email])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn count_rules(&self) -> Result<i64> {
        let conn = self.lock();
        let count = conn.query_row("SELECT COUNT(*) FROM rules_index", [], |row| row.get(0))?;
        Ok(count)
    }
}

/// One derived row of the rule index.
#[derive(Debug, Clone)]
pub struct RuleRow {
    pub path: String,
    pub level: String,
    pub effect: String,
    pub email: String,
}

fn map_access_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<AccessEntry> {
    Ok(AccessEntry {
        email: row.get(0)?,
        path: row.get(1)?,
        ts: row.get(2)?,
        outcome: row.get(3)?,
    })
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_schema_and_records_version() {
        let store = Store::open_in_memory().unwrap();
        let conn = store.lock();
        let version: i64 = conn
            .query_row(
                "SELECT value FROM mdshelf_meta WHERE key = 'schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn session_round_trip() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_session("sid-1", "ana@corp.com", 1_000, Some(&[1, 2, 3]))
            .unwrap();

        let found = store.get_session("sid-1").unwrap().unwrap();
        assert_eq!(found.email, "ana@corp.com");
        assert_eq!(found.created_at, 1_000);
        assert_eq!(found.last_seen_at, 1_000);
        assert_eq!(found.refresh_token_enc.as_deref(), Some(&[1u8, 2, 3][..]));

        store.touch_session("sid-1", 2_000).unwrap();
        assert_eq!(
            store.get_session("sid-1").unwrap().unwrap().last_seen_at,
            2_000
        );

        store.delete_session("sid-1").unwrap();
        assert!(store.get_session("sid-1").unwrap().is_none());
    }

    #[test]
    fn access_log_queries_and_pruning() {
        let store = Store::open_in_memory().unwrap();
        let now = 10_000_000_000i64;
        store
            .log_access("ana@corp.com", "/hr/comp", now, Outcome::Allow)
            .unwrap();
        store
            .log_access("bob@corp.com", "/hr/comp", now, Outcome::Deny)
            .unwrap();

        assert_eq!(store.access_by_path("/hr/comp").unwrap().len(), 2);
        assert_eq!(store.access_by_email("ana@corp.com").unwrap().len(), 1);

        // An entry one day old survives a 90-day retention window.
        let removed = store
            .prune_access_log(now + 86_400_000, Duration::from_secs(90 * 86_400))
            .unwrap();
        assert_eq!(removed, 0);

        // The same entry is pruned once the window is shorter than its age.
        let removed = store
            .prune_access_log(now + 86_400_000, Duration::from_secs(60))
            .unwrap();
        assert_eq!(removed, 2);
    }

    #[test]
    fn forget_email_clears_sessions_and_log() {
        let store = Store::open_in_memory().unwrap();
        store
            .insert_session("sid-1", "bob@corp.com", 1_000, None)
            .unwrap();
        store
            .log_access("bob@corp.com", "/a", 1_000, Outcome::Allow)
            .unwrap();

        let (entries, sessions) = store.forget_email("bob@corp.com").unwrap();
        assert_eq!((entries, sessions), (1, 1));
        assert!(store.get_session("sid-1").unwrap().is_none());
        assert!(store.access_by_email("bob@corp.com").unwrap().is_empty());
    }

    #[test]
    fn replacing_rules_is_idempotent_per_site() {
        let store = Store::open_in_memory().unwrap();
        let rows = vec![RuleRow {
            path: "hr/comp.md".into(),
            level: "file".into(),
            effect: "allow".into(),
            email: "ana@corp.com".into(),
        }];
        store.replace_rules_for_site("/docs", &rows).unwrap();
        store.replace_rules_for_site("/docs", &rows).unwrap();
        assert_eq!(store.count_rules().unwrap(), 1);

        store.replace_rules_for_site("/docs", &[]).unwrap();
        assert_eq!(store.count_rules().unwrap(), 0);
    }

    #[test]
    fn rejects_future_schema_version() {
        let dir = std::env::temp_dir().join(format!("mdshelf-db-{}", now_ms()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("mdshelf.db");
        {
            let store = Store::open(&path).unwrap();
            store
                .lock()
                .execute(
                    "UPDATE mdshelf_meta SET value = ?1 WHERE key = 'schema_version'",
                    params![SCHEMA_VERSION + 1],
                )
                .unwrap();
        }
        let err = match Store::open(&path) {
            Ok(_) => panic!("a future schema version must be refused"),
            Err(error) => error.to_string(),
        };
        assert!(
            err.contains("newer than this mdshelf understands"),
            "got: {err}"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
