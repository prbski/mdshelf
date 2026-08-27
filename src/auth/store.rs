//! The SQLite sidecar.
//!
//! Most of this is disposable (D13). `rules_index` is derived from vault frontmatter
//! and rebuilt on boot, and nothing here is a source of truth for who may read what —
//! that lives in the markdown.
//!
//! The exception is `links`. Share links are minted here and nowhere else, and their
//! tokens are stored only as hashes, so a deleted database destroys every live share
//! and no amount of re-scanning brings one back. That reverses the original NFR-3, and
//! every message about deleting this file has to say so.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

/// Bumped whenever the schema changes shape. A database written by a newer mdshelf is
/// refused rather than silently misread.
pub const SCHEMA_VERSION: i64 = 2;

/// Outcome recorded for a request in the access log (D27).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Allow,
    Deny,
    /// A request under the link prefix whose token matched no row (S15).
    ///
    /// Separate from `Deny` because these rows are written by unauthenticated
    /// strangers, so they get their own, much shorter retention window (R6).
    BadLink,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Allow => "allow",
            Outcome::Deny => "deny",
            Outcome::BadLink => BAD_LINK_OUTCOME,
        }
    }
}

/// The access-log `outcome` value for an unknown token.
pub const BAD_LINK_OUTCOME: &str = "bad-link";

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

/// One share link, as everything outside this module sees it.
///
/// Carries no token and no token hash: SEC-2 keeps both inside the store, so no caller
/// can accidentally format one into a log line.
#[derive(Debug, Clone)]
pub struct LinkRecord {
    /// Short public id, also the access-log pseudonym (S14).
    pub id: String,
    /// Canonicalised site root path, stable across mount renames.
    pub site: String,
    /// Site-relative path of the shared page.
    pub path: String,
    pub expires_at: i64,
    pub created_at: i64,
    pub issued_by: String,
    pub revoked_at: Option<i64>,
}

impl LinkRecord {
    /// Whether the link is still servable on its own terms.
    ///
    /// Says nothing about the issuer's access, which is revalidated separately on every
    /// request (S29) and is the other half of the answer.
    pub fn is_live(&self, now: i64) -> bool {
        self.revoked_at.is_none() && self.expires_at > now
    }

    /// One word for a listing: `live`, `revoked` or `expired`.
    pub fn state(&self, now: i64) -> &'static str {
        if self.revoked_at.is_some() {
            "revoked"
        } else if self.expires_at <= now {
            "expired"
        } else {
            "live"
        }
    }
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

    /// Open an existing database without applying the schema.
    ///
    /// Used when auth is off (US-13): the server still wants to warn that live links
    /// exist, but must not write to — or upgrade — a database it is not otherwise
    /// using, because NFR-1 promises that build behaves exactly as it did before.
    pub fn open_read_only(path: &Path) -> Result<Self> {
        use rusqlite::OpenFlags;
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(|| format!("opening database {} read-only", path.display()))?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
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
                 Upgrade mdshelf. Deleting the database is not a free reset: it discards \
                 sessions, access history, and every share link, and share links cannot \
                 be recreated because only their hashes are stored. Run `mdshelf share \
                 list --json` with a build that understands this schema first if you \
                 need an inventory of what would be lost.",
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

            CREATE TABLE IF NOT EXISTS links (
                id          TEXT PRIMARY KEY,
                token_hash  BLOB NOT NULL,
                site        TEXT NOT NULL,
                path        TEXT NOT NULL,
                expires_at  INTEGER NOT NULL,
                created_at  INTEGER NOT NULL,
                issued_by   TEXT NOT NULL,
                revoked_at  INTEGER
            );
            CREATE UNIQUE INDEX IF NOT EXISTS links_token_hash_idx ON links(token_hash);
            CREATE INDEX IF NOT EXISTS links_issued_by_idx ON links(issued_by);
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

    /// Delete `bad-link` rows older than `retention` (S15/R6).
    ///
    /// Separate from [`Store::prune_access_log`] because these rows are written by
    /// unauthenticated strangers: a scanner may not be allowed to fill the disk with
    /// ninety days of noise just because it can reach the prefix.
    pub fn prune_bad_links(&self, now: i64, retention: Duration) -> Result<usize> {
        let cutoff = now - (retention.as_secs() as i64) * 1000;
        let removed = self.lock().execute(
            "DELETE FROM access_log WHERE outcome = ?1 AND ts < ?2",
            params![BAD_LINK_OUTCOME, cutoff],
        )?;
        Ok(removed)
    }

    // ---- share links ----------------------------------------------------------

    /// Record a new link. `token_hash` is `sha256(token)`; the token itself never
    /// reaches this function (SEC-1/SEC-2).
    ///
    /// The column list is the argument list; a wrapper struct would only move the same
    /// seven values one indirection away.
    #[allow(clippy::too_many_arguments)]
    pub fn insert_link(
        &self,
        id: &str,
        token_hash: &[u8],
        site: &str,
        path: &str,
        expires_at: i64,
        created_at: i64,
        issued_by: &str,
    ) -> Result<()> {
        self.lock().execute(
            "INSERT INTO links(id, token_hash, site, path, expires_at, created_at, issued_by)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id, token_hash, site, path, expires_at, created_at, issued_by
            ],
        )?;
        Ok(())
    }

    /// The single indexed lookup every link request makes (S21).
    ///
    /// Deliberately not cached: a revoke has to take effect on the very next request,
    /// and a cache is one more thing that can be wrong about who may read what.
    pub fn link_by_token_hash(&self, token_hash: &[u8]) -> Result<Option<LinkRecord>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, site, path, expires_at, created_at, issued_by, revoked_at
                 FROM links WHERE token_hash = ?1",
                params![token_hash],
                map_link_record,
            )
            .optional()?;
        Ok(row)
    }

    pub fn link_by_id(&self, id: &str) -> Result<Option<LinkRecord>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                "SELECT id, site, path, expires_at, created_at, issued_by, revoked_at
                 FROM links WHERE id = ?1",
                params![id],
                map_link_record,
            )
            .optional()?;
        Ok(row)
    }

    /// Links, newest first. `issued_by` narrows to one issuer; `include_dead` adds
    /// revoked and expired rows (US-3).
    pub fn list_links(
        &self,
        now: i64,
        include_dead: bool,
        issued_by: Option<&str>,
    ) -> Result<Vec<LinkRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, site, path, expires_at, created_at, issued_by, revoked_at
             FROM links ORDER BY created_at DESC, id ASC",
        )?;
        let rows = stmt
            .query_map([], map_link_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|link| include_dead || link.is_live(now))
            .filter(|link| issued_by.is_none_or(|email| link.issued_by == email))
            .collect())
    }

    /// Live links one issuer made for one page, newest first (US-16).
    ///
    /// Indexed on `issued_by`, so the popover costs one narrow query rather than a scan
    /// of every link in the vault.
    pub fn links_for_page(
        &self,
        now: i64,
        issued_by: &str,
        site: &str,
        path: &str,
    ) -> Result<Vec<LinkRecord>> {
        let conn = self.lock();
        let mut stmt = conn.prepare(
            "SELECT id, site, path, expires_at, created_at, issued_by, revoked_at
             FROM links
             WHERE issued_by = ?1 AND site = ?2 AND path = ?3
               AND revoked_at IS NULL AND expires_at > ?4
             ORDER BY created_at DESC, id ASC",
        )?;
        let rows = stmt
            .query_map(params![issued_by, site, path, now], map_link_record)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Mark one link revoked. Returns `false` when no such id exists.
    ///
    /// Revoking an already-revoked link leaves the original timestamp in place, so the
    /// record still says when access actually ended (US-4).
    pub fn revoke_link(&self, id: &str, now: i64) -> Result<bool> {
        let conn = self.lock();
        let exists: bool = conn
            .query_row("SELECT 1 FROM links WHERE id = ?1", params![id], |_| Ok(()))
            .optional()?
            .is_some();
        if !exists {
            return Ok(false);
        }
        conn.execute(
            "UPDATE links SET revoked_at = ?2 WHERE id = ?1 AND revoked_at IS NULL",
            params![id, now],
        )?;
        Ok(true)
    }

    /// Revoke every link that is still live, returning how many were affected.
    pub fn revoke_all_links(&self, now: i64) -> Result<usize> {
        let changed = self.lock().execute(
            "UPDATE links SET revoked_at = ?1 WHERE revoked_at IS NULL AND expires_at > ?1",
            params![now],
        )?;
        Ok(changed)
    }

    /// Revoke a link only if `issuer` created it (S11: the interface is issuer-only).
    ///
    /// Returns `false` for both "no such link" and "not yours", so the popover cannot
    /// be used to probe for links other people made.
    pub fn revoke_link_for_issuer(&self, id: &str, issuer: &str, now: i64) -> Result<bool> {
        let conn = self.lock();
        let owned: bool = conn
            .query_row(
                "SELECT 1 FROM links WHERE id = ?1 AND issued_by = ?2",
                params![id, issuer],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        if !owned {
            return Ok(false);
        }
        conn.execute(
            "UPDATE links SET revoked_at = ?3 WHERE id = ?1 AND issued_by = ?2 AND revoked_at IS NULL",
            params![id, issuer, now],
        )?;
        Ok(true)
    }

    /// Delete links that have been dead longer than `retention` (US-5).
    pub fn prune_links(&self, now: i64, retention: Duration) -> Result<usize> {
        let cutoff = now - (retention.as_secs() as i64) * 1000;
        let removed = self.lock().execute(
            "DELETE FROM links
             WHERE (revoked_at IS NOT NULL AND revoked_at < ?1)
                OR (revoked_at IS NULL AND expires_at < ?1)",
            params![cutoff],
        )?;
        Ok(removed)
    }

    /// How many links would still serve. Used by the auth-off startup warning (US-13).
    pub fn count_live_links(&self, now: i64) -> Result<i64> {
        let conn = self.lock();
        let count = conn.query_row(
            "SELECT COUNT(*) FROM links WHERE revoked_at IS NULL AND expires_at > ?1",
            params![now],
            |row| row.get(0),
        )?;
        Ok(count)
    }

    /// Move a link's timestamps so expiry and retention can be tested without sleeping.
    #[cfg(any(test, feature = "test-support"))]
    pub fn backdate_link(&self, id: &str, created_at: i64, expires_at: i64) -> Result<()> {
        self.lock().execute(
            "UPDATE links SET created_at = ?2, expires_at = ?3 WHERE id = ?1",
            params![id, created_at, expires_at],
        )?;
        Ok(())
    }

    /// Fold the write-ahead log back into the main file.
    ///
    /// Needed by the US-2 test that greps the database for a freshly minted token:
    /// without a checkpoint the row may still live only in `mdshelf.db-wal`, and the
    /// search would pass by looking in the wrong file.
    #[cfg(any(test, feature = "test-support"))]
    pub fn checkpoint(&self) -> Result<()> {
        self.lock()
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")?;
        Ok(())
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

fn map_link_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<LinkRecord> {
    Ok(LinkRecord {
        id: row.get(0)?,
        site: row.get(1)?,
        path: row.get(2)?,
        expires_at: row.get(3)?,
        created_at: row.get(4)?,
        issued_by: row.get(5)?,
        revoked_at: row.get(6)?,
    })
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

    /// A minted link is found by its hash, is live, and carries back exactly what was
    /// stored.
    #[test]
    fn link_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let now = 1_000_000i64;
        let hash = [9u8; 32];
        store
            .insert_link(
                "ab12cd",
                &hash,
                "/vault/docs",
                "hr/comp.md",
                now + 86_400_000,
                now,
                "ana@corp.com",
            )
            .unwrap();

        let found = store.link_by_token_hash(&hash).unwrap().unwrap();
        assert_eq!(found.id, "ab12cd");
        assert_eq!(found.site, "/vault/docs");
        assert_eq!(found.path, "hr/comp.md");
        assert_eq!(found.issued_by, "ana@corp.com");
        assert!(found.is_live(now));
        assert_eq!(found.state(now), "live");

        assert!(store.link_by_token_hash(&[0u8; 32]).unwrap().is_none());
        assert_eq!(store.link_by_id("ab12cd").unwrap().unwrap().id, "ab12cd");
    }

    #[test]
    fn a_duplicate_token_hash_is_refused() {
        let store = Store::open_in_memory().unwrap();
        let hash = [4u8; 32];
        store
            .insert_link("aaa111", &hash, "/v", "a.md", 10, 1, "a@b.com")
            .unwrap();
        assert!(
            store
                .insert_link("bbb222", &hash, "/v", "b.md", 10, 1, "a@b.com")
                .is_err(),
            "two links may never share a token"
        );
    }

    #[test]
    fn revoking_is_idempotent_and_keeps_the_first_timestamp() {
        let store = Store::open_in_memory().unwrap();
        let now = 1_000_000i64;
        store
            .insert_link(
                "ab12cd",
                &[1u8; 32],
                "/v",
                "a.md",
                now + 1000,
                now,
                "a@b.com",
            )
            .unwrap();

        assert!(store.revoke_link("ab12cd", now + 10).unwrap());
        let first = store.link_by_id("ab12cd").unwrap().unwrap();
        assert_eq!(first.revoked_at, Some(now + 10));
        assert!(!first.is_live(now + 20));
        assert_eq!(first.state(now + 20), "revoked");

        // US-4: revoking again succeeds and changes nothing.
        assert!(store.revoke_link("ab12cd", now + 99).unwrap());
        assert_eq!(
            store.link_by_id("ab12cd").unwrap().unwrap().revoked_at,
            Some(now + 10)
        );

        assert!(!store.revoke_link("nosuch", now).unwrap());
    }

    #[test]
    fn revoke_all_touches_only_live_links() {
        let store = Store::open_in_memory().unwrap();
        let now = 1_000_000i64;
        store
            .insert_link(
                "live01",
                &[1u8; 32],
                "/v",
                "a.md",
                now + 1000,
                now,
                "a@b.com",
            )
            .unwrap();
        store
            .insert_link(
                "gone02",
                &[2u8; 32],
                "/v",
                "b.md",
                now - 1000,
                now,
                "a@b.com",
            )
            .unwrap();
        store
            .insert_link(
                "done03",
                &[3u8; 32],
                "/v",
                "c.md",
                now + 1000,
                now,
                "a@b.com",
            )
            .unwrap();
        store.revoke_link("done03", now).unwrap();

        assert_eq!(store.revoke_all_links(now).unwrap(), 1);
        assert_eq!(store.count_live_links(now).unwrap(), 0);
        // The already-expired row is left exactly as it was.
        assert!(
            store
                .link_by_id("gone02")
                .unwrap()
                .unwrap()
                .revoked_at
                .is_none()
        );
    }

    #[test]
    fn issuer_scoped_revocation_refuses_other_peoples_links() {
        let store = Store::open_in_memory().unwrap();
        let now = 1_000_000i64;
        store
            .insert_link(
                "mine01",
                &[1u8; 32],
                "/v",
                "a.md",
                now + 1000,
                now,
                "ana@corp.com",
            )
            .unwrap();
        store
            .insert_link(
                "their1",
                &[2u8; 32],
                "/v",
                "b.md",
                now + 1000,
                now,
                "bob@corp.com",
            )
            .unwrap();

        assert!(
            !store
                .revoke_link_for_issuer("their1", "ana@corp.com", now)
                .unwrap()
        );
        assert!(store.link_by_id("their1").unwrap().unwrap().is_live(now));
        assert!(
            store
                .revoke_link_for_issuer("mine01", "ana@corp.com", now)
                .unwrap()
        );
        assert!(!store.link_by_id("mine01").unwrap().unwrap().is_live(now));
    }

    #[test]
    fn listing_filters_by_liveness_and_issuer() {
        let store = Store::open_in_memory().unwrap();
        let now = 1_000_000i64;
        store
            .insert_link(
                "live01",
                &[1u8; 32],
                "/v",
                "a.md",
                now + 1000,
                now,
                "ana@corp.com",
            )
            .unwrap();
        store
            .insert_link(
                "dead02",
                &[2u8; 32],
                "/v",
                "b.md",
                now - 1000,
                now,
                "ana@corp.com",
            )
            .unwrap();
        store
            .insert_link(
                "other3",
                &[3u8; 32],
                "/v",
                "c.md",
                now + 1000,
                now,
                "bob@corp.com",
            )
            .unwrap();

        let live = store.list_links(now, false, None).unwrap();
        assert_eq!(live.len(), 2, "only the two unexpired rows are live");
        assert!(live.iter().all(|link| link.is_live(now)));

        let all = store.list_links(now, true, None).unwrap();
        assert_eq!(all.len(), 3);
        assert!(all.iter().any(|link| link.state(now) == "expired"));

        let anas = store.list_links(now, true, Some("ana@corp.com")).unwrap();
        assert_eq!(anas.len(), 2);
        assert!(anas.iter().all(|link| link.issued_by == "ana@corp.com"));
    }

    /// US-5: dead longer than the window goes; live rows and recently dead rows stay.
    #[test]
    fn link_retention_deletes_only_long_dead_rows() {
        let store = Store::open_in_memory().unwrap();
        let now = 10_000_000_000i64;
        let window = Duration::from_secs(90 * 86_400);
        let long_ago = now - 100 * 86_400_000;

        store
            .insert_link(
                "live01",
                &[1u8; 32],
                "/v",
                "a.md",
                now + 1000,
                now,
                "a@b.com",
            )
            .unwrap();
        store
            .insert_link(
                "old002", &[2u8; 32], "/v", "b.md", long_ago, long_ago, "a@b.com",
            )
            .unwrap();
        store
            .insert_link(
                "recent3",
                &[3u8; 32],
                "/v",
                "c.md",
                now - 1000,
                now,
                "a@b.com",
            )
            .unwrap();
        store
            .insert_link(
                "oldrev4",
                &[4u8; 32],
                "/v",
                "d.md",
                now + 1000,
                long_ago,
                "a@b.com",
            )
            .unwrap();
        store.revoke_link("oldrev4", long_ago).unwrap();

        assert_eq!(store.prune_links(now, window).unwrap(), 2);
        assert!(store.link_by_id("live01").unwrap().is_some());
        assert!(store.link_by_id("recent3").unwrap().is_some());
        assert!(store.link_by_id("old002").unwrap().is_none());
        assert!(store.link_by_id("oldrev4").unwrap().is_none());
    }

    /// US-5: bad-link rows have their own, shorter window; ordinary reads are untouched.
    #[test]
    fn bad_link_retention_is_independent_of_the_log_retention() {
        let store = Store::open_in_memory().unwrap();
        let now = 10_000_000_000i64;
        let ten_days_ago = now - 10 * 86_400_000;

        store
            .log_access("link:unknown", "/s", ten_days_ago, Outcome::BadLink)
            .unwrap();
        store
            .log_access("link:ab12cd", "/docs/a", ten_days_ago, Outcome::Allow)
            .unwrap();

        let removed = store
            .prune_bad_links(now, Duration::from_secs(7 * 86_400))
            .unwrap();
        assert_eq!(removed, 1, "the bad-link row is past its window");
        let survivors = store.access_by_path("/docs/a").unwrap();
        assert_eq!(survivors.len(), 1, "a read inside log_retention survives");
        assert!(store.access_by_path("/s").unwrap().is_empty());
    }

    #[test]
    fn the_bad_link_outcome_round_trips_through_the_log() {
        let store = Store::open_in_memory().unwrap();
        store
            .log_access("link:unknown", "/s", 1_000, Outcome::BadLink)
            .unwrap();
        let entries = store.access_by_path("/s").unwrap();
        assert_eq!(entries[0].outcome, "bad-link");
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
        // R3: the message may no longer claim a deletion costs only sessions and
        // history, because share links cannot be recreated.
        assert!(err.contains("share link"), "got: {err}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
