pub mod config;

use rusqlite::{params, Connection};
use sha2::{Digest, Sha256};
use std::fmt;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How many non-pinned text and image entries history keeps by default. The
/// popup fetches everything (see its `FETCH_ALL`), so entries the daemon
/// persists are always reachable from the UI; pinned entries are exempt from
/// trimming entirely and can exceed these caps.
pub const MAX_TEXT_ENTRIES: usize = 200;
pub const MAX_IMAGE_ENTRIES: usize = 50;

/// Retention caps for non-pinned entries. `0` is meaningful: it means
/// "keep no (unpinned) entries of this kind".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    pub text: usize,
    pub images: usize,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            text: MAX_TEXT_ENTRIES,
            images: MAX_IMAGE_ENTRIES,
        }
    }
}

/// Where a clipboard entry came from: the regular clipboard (Ctrl+C / copy)
/// or the middle-click primary selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureSource {
    Clipboard,
    Primary,
}

impl CaptureSource {
    pub fn is_primary(self) -> bool {
        matches!(self, CaptureSource::Primary)
    }
}

#[derive(Debug, Clone)]
pub struct ClipEntry {
    pub id: i64,
    pub timestamp: i64,
    pub mime_type: String,
    pub content: Option<String>,
    pub image_path: Option<String>,
    pub content_hash: String,
    pub pinned: bool,
    pub is_primary: bool,
}

/// Unified error type for `HistoryDb` operations: SQLite failures plus the
/// filesystem work the database implicitly depends on (creating the data
/// directory, writing image files). A bare `rusqlite::Error` can't represent
/// "disk full while writing the thumbnail file", and swallowing that failure
/// is what used to leave broken rows behind (an `image_path` pointing at a
/// file that was never written).
#[derive(Debug)]
pub enum HistoryError {
    Sql(rusqlite::Error),
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

impl fmt::Display for HistoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HistoryError::Sql(e) => write!(f, "database error: {e}"),
            HistoryError::Io {
                action,
                path,
                source,
            } => {
                write!(f, "failed to {action} {}: {source}", path.display())
            }
        }
    }
}

impl std::error::Error for HistoryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HistoryError::Sql(e) => Some(e),
            HistoryError::Io { source, .. } => Some(source),
        }
    }
}

impl From<rusqlite::Error> for HistoryError {
    fn from(e: rusqlite::Error) -> Self {
        HistoryError::Sql(e)
    }
}

pub struct HistoryDb {
    conn: Connection,
    retention: Retention,
}

impl HistoryDb {
    /// Open with the default retention caps.
    pub fn open() -> Result<Self, HistoryError> {
        Self::open_with(Retention::default())
    }

    pub fn open_with(retention: Retention) -> Result<Self, HistoryError> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir).map_err(|source| HistoryError::Io {
            action: "create data directory",
            path: dir.clone(),
            source,
        })?;
        // The data dir holds clipboard secrets (history.db is plaintext).
        // A 0700 parent also blocks traversal to whatever SQLite's
        // -wal/-shm side files look like, regardless of their own mode.
        restrict_dir(&dir, 0o700);

        let db_path = dir.join("history.db");
        let conn = Connection::open(&db_path)?;
        // Clipboard events can spawn overlapping `--capture-once` processes
        // and the popup opens the same DB: without a busy timeout a writer
        // can hit SQLITE_BUSY and silently drop a capture.
        conn.busy_timeout(Duration::from_secs(5))?;
        // WAL: concurrent readers (popup) and the single writer (daemon) no
        // longer block each other, and readers see a consistent snapshot.
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp    INTEGER NOT NULL,
                mime_type    TEXT NOT NULL,
                content      TEXT,
                image_path   TEXT,
                content_hash TEXT NOT NULL UNIQUE,
                pinned       INTEGER NOT NULL DEFAULT 0,
                is_primary   INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS history_ts ON history(timestamp DESC, id DESC);",
        )?;
        // Migrate databases created before `pinned`/`is_primary` existed.
        ensure_column(
            &conn,
            "pinned",
            "ALTER TABLE history ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &conn,
            "is_primary",
            "ALTER TABLE history ADD COLUMN is_primary INTEGER NOT NULL DEFAULT 0",
        )?;
        // history.db can contain plaintext secrets copied to the clipboard
        // (passwords, tokens, TOTP codes); restrict it to owner-only, every open.
        restrict_permissions(&db_path);
        let db = Self { conn, retention };
        db.gc_orphaned_images()?;
        Ok(db)
    }

    pub fn insert_text(&self, text: &str, source: CaptureSource) -> Result<(), HistoryError> {
        let hash = sha256_hex(text.as_bytes());
        let ts = unix_now();
        self.conn.execute(
            "INSERT INTO history (timestamp, mime_type, content, content_hash, is_primary)
             VALUES (?1, 'text/plain', ?2, ?3, ?4)
             ON CONFLICT(content_hash) DO UPDATE SET timestamp = ?1, is_primary = ?4",
            params![ts, text, hash, source.is_primary() as i64],
        )?;
        self.trim()
    }

    /// Store an image. `mime_type` is persisted with the row and drives the
    /// file extension, so a JPEG capture is stored as a `.jpg` offered as
    /// `image/jpeg`, not silently re-encoded/relabeled as PNG.
    pub fn insert_image(
        &self,
        bytes: &[u8],
        mime_type: &str,
        source: CaptureSource,
    ) -> Result<(), HistoryError> {
        let hash = sha256_hex(bytes);
        let images_dir = data_dir().join("images");
        std::fs::create_dir_all(&images_dir).map_err(|source| HistoryError::Io {
            action: "create images directory",
            path: images_dir.clone(),
            source,
        })?;
        restrict_dir(&images_dir, 0o700);

        let ext = match mime_type {
            "image/jpeg" => "jpg",
            _ => "png",
        };
        // First 16 hex chars of the hash: collision-safe for the few dozen
        // images history keeps.
        let path = images_dir.join(format!("{}.{}", &hash[..16], ext));
        if !path.exists() {
            write_image_file(&path, bytes)?;
        }
        let path_str = path.to_string_lossy().to_string();
        let ts = unix_now();
        self.conn.execute(
            "INSERT INTO history (timestamp, mime_type, image_path, content_hash, is_primary)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(content_hash) DO UPDATE SET timestamp = ?1, is_primary = ?5",
            params![ts, mime_type, path_str, hash, source.is_primary() as i64],
        )?;
        self.trim()
    }

    /// Pin (or unpin) an entry. Pinned entries are exempt from trimming and
    /// sort to the top of the history list.
    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<(), HistoryError> {
        self.conn.execute(
            "UPDATE history SET pinned = ?1 WHERE id = ?2",
            params![pinned as i64, id],
        )?;
        Ok(())
    }

    pub fn list_entries(&self, limit: usize) -> Result<Vec<ClipEntry>, HistoryError> {
        // Pinned rows first, then `timestamp DESC, id DESC`: many copies
        // land within the same second, and the `id` tiebreaker keeps that
        // order (and therefore which rows trim keeps) deterministic.
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, mime_type, content, image_path, content_hash, pinned, is_primary
             FROM history ORDER BY pinned DESC, timestamp DESC, id DESC LIMIT ?1",
        )?;
        let rows = stmt
            .query_map([limit as i64], |row| {
                Ok(ClipEntry {
                    id: row.get(0)?,
                    timestamp: row.get(1)?,
                    mime_type: row.get(2)?,
                    content: row.get(3)?,
                    image_path: row.get(4)?,
                    content_hash: row.get(5)?,
                    pinned: row.get(6)?,
                    is_primary: row.get(7)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Deletes every history entry and every stored image file: a hard
    /// reset that clears pinned entries too. Used by the
    /// `bread.command.clip.clear` handler (see breadclipd's bread-client
    /// subscription) as well as anything else that wants a full wipe.
    pub fn clear_all(&self) -> Result<(), HistoryError> {
        let image_paths: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT image_path FROM history WHERE image_path IS NOT NULL")?;
            let paths = stmt
                .query_map([], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            paths
        };
        // Delete rows first, then files: the reverse order would leave rows
        // pointing at already-removed files if the DELETE failed halfway.
        self.conn.execute("DELETE FROM history", [])?;
        for path in image_paths {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }

    pub fn delete_entry(&self, id: i64) -> Result<(), HistoryError> {
        // Clean up image file if present, after the row is gone, so a
        // failed DELETE never leaves a row pointing at a deleted file.
        let image_path: Option<String> = self
            .conn
            .query_row(
                "SELECT image_path FROM history WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        self.conn
            .execute("DELETE FROM history WHERE id = ?1", [id])?;
        if let Some(p) = image_path {
            let _ = std::fs::remove_file(p);
        }
        Ok(())
    }

    /// Sweep image files on disk that no history row references anymore
    /// (e.g. left behind by a crash between writing the file and inserting
    /// the row). Files written in the last hour are left alone so a
    /// concurrent `--capture-once` that is mid-insert (file on disk, row not
    /// yet committed) is never deleted out from under itself.
    pub fn gc_orphaned_images(&self) -> Result<(), HistoryError> {
        let referenced: std::collections::HashSet<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT image_path FROM history WHERE image_path IS NOT NULL")?;
            let paths = stmt
                .query_map([], |row| row.get::<_, Option<String>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            paths.into_iter().flatten().collect()
        };
        let images_dir = data_dir().join("images");
        let Ok(entries) = std::fs::read_dir(&images_dir) else {
            return Ok(()); // no images dir yet: nothing to sweep
        };
        let cutoff = std::time::SystemTime::now()
            .checked_sub(Duration::from_secs(3600))
            .unwrap_or(std::time::UNIX_EPOCH);
        for entry in entries.flatten() {
            let path = entry.path();
            if referenced.contains(&path.to_string_lossy().to_string()) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if modified < cutoff {
                let _ = std::fs::remove_file(&path);
            }
        }
        Ok(())
    }

    fn trim(&self) -> Result<(), HistoryError> {
        // Oldest non-pinned text rows beyond the cap. Pinned rows are never
        // trimmed; that's the point of pinning them.
        self.conn.execute(
            "DELETE FROM history
             WHERE mime_type = 'text/plain' AND pinned = 0
               AND id NOT IN (
                   SELECT id FROM history WHERE mime_type = 'text/plain' AND pinned = 0
                   ORDER BY timestamp DESC, id DESC LIMIT ?1
               )",
            [self.retention.text as i64],
        )?;
        // Collect the image files that are about to be trimmed, delete the
        // rows first, then the files, so a failed DELETE never leaves rows
        // pointing at already-removed files. `LIKE 'image/%'` covers every
        // image mime type (png, jpeg, and anything added later).
        let old_paths: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT image_path FROM history
                 WHERE mime_type LIKE 'image/%'
                   AND pinned = 0
                   AND image_path IS NOT NULL
                   AND id NOT IN (
                       SELECT id FROM history WHERE mime_type LIKE 'image/%' AND pinned = 0
                       ORDER BY timestamp DESC, id DESC LIMIT ?1
                   )",
            )?;
            let paths = stmt
                .query_map([self.retention.images as i64], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            paths
        };
        self.conn.execute(
            "DELETE FROM history
             WHERE mime_type LIKE 'image/%'
               AND pinned = 0
               AND id NOT IN (
                   SELECT id FROM history WHERE mime_type LIKE 'image/%' AND pinned = 0
                   ORDER BY timestamp DESC, id DESC LIMIT ?1
               )",
            [self.retention.images as i64],
        )?;
        for path in old_paths {
            let _ = std::fs::remove_file(path);
        }
        Ok(())
    }
}

/// Add `column` to the history table if it isn't there (migration for
/// databases created by older versions). Table/column names are our own
/// constants, never user input.
fn ensure_column(conn: &Connection, column: &str, add_ddl: &str) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(history)")?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name? == column {
            return Ok(());
        }
    }
    conn.execute_batch(add_ddl)
}

/// Restrict a file to owner-only read/write (0600). Clipboard history can
/// contain passwords and other secrets, so this must not be world/group
/// readable regardless of the process umask.
fn restrict_permissions(path: &Path) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o600);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// Restrict a directory to owner-only (0700). Used for the data dir and the
/// images dir, both of which contain clipboard secrets.
fn restrict_dir(path: &Path, mode: u32) {
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(mode);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// Write an image file, created 0600 from the very first syscall: no window
/// where a clipboard image (possibly a screenshot with sensitive pixels) is
/// world-readable before a chmod lands, and no dependence on the umask.
fn write_image_file(path: &Path, bytes: &[u8]) -> Result<(), HistoryError> {
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut f) => f.write_all(bytes).map_err(|source| HistoryError::Io {
            action: "write image file",
            path: path.to_path_buf(),
            source,
        }),
        // Same hash filename means identical content: a concurrent
        // `--capture-once` already wrote it.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(source) => Err(HistoryError::Io {
            action: "create image file",
            path: path.to_path_buf(),
            source,
        }),
    }
}

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

pub fn data_dir() -> PathBuf {
    // Was `dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("~/.local/share"))`:
    // PathBuf/std::fs never expand `~`, so on the rare box where `dirs`
    // can't resolve a home directory, that fallback silently resolved to a
    // directory literally named `~` under the current working directory
    // instead of the user's actual home. bread_utils::xdg resolves a real
    // $HOME before ever falling back.
    bread_utils::xdg::data_dir("breadclip")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Serializes tests that redirect `$XDG_DATA_HOME`/`$XDG_CONFIG_HOME`
/// (process-global env vars) against each other: `cargo test` runs tests
/// in parallel threads within one process.
#[cfg(test)]
pub(crate) fn env_test_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open_test_db() -> (
        std::sync::MutexGuard<'static, ()>,
        tempfile::TempDir,
        HistoryDb,
    ) {
        let guard = env_test_lock().lock().unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("XDG_DATA_HOME", dir.path());
        let db = HistoryDb::open().expect("open history db");
        (guard, dir, db)
    }

    #[test]
    fn clear_all_removes_every_entry_and_image_file() {
        let (_guard, _dir, db) = open_test_db();
        db.insert_text("first", CaptureSource::Clipboard).unwrap();
        db.insert_text("second", CaptureSource::Clipboard).unwrap();
        db.insert_image(
            b"not really a png, just bytes for the test",
            "image/png",
            CaptureSource::Clipboard,
        )
        .unwrap();

        let before = db.list_entries(10).unwrap();
        assert_eq!(before.len(), 3);
        let image_path = before
            .iter()
            .find_map(|e| e.image_path.clone())
            .expect("one entry should be the image");
        assert!(Path::new(&image_path).exists());

        db.clear_all().unwrap();

        let after = db.list_entries(10).unwrap();
        assert!(after.is_empty(), "expected no entries after clear_all");
        assert!(
            !Path::new(&image_path).exists(),
            "image file should be removed by clear_all"
        );
    }

    #[test]
    fn clear_all_on_empty_history_is_a_harmless_no_op() {
        let (_guard, _dir, db) = open_test_db();
        db.clear_all().unwrap();
        assert!(db.list_entries(10).unwrap().is_empty());
    }

    #[test]
    fn open_uses_wal_journal_mode() {
        let (_guard, _dir, db) = open_test_db();
        let mode: String = db
            .conn
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn entries_with_equal_timestamps_order_by_id_desc() {
        let (_guard, _dir, db) = open_test_db();
        db.insert_text("first", CaptureSource::Clipboard).unwrap();
        db.insert_text("second", CaptureSource::Clipboard).unwrap();
        let entries = db.list_entries(10).unwrap();
        assert_eq!(entries[0].content.as_deref(), Some("second"));
        assert_eq!(entries[1].content.as_deref(), Some("first"));
    }

    #[test]
    fn trim_caps_text_and_images_at_retention_limits() {
        let (_guard, _dir, db) = open_test_db();
        for i in 0..210 {
            db.insert_text(&format!("text-{i}"), CaptureSource::Clipboard)
                .unwrap();
        }
        for i in 0..55 {
            db.insert_image(
                format!("img-{i}").as_bytes(),
                "image/png",
                CaptureSource::Clipboard,
            )
            .unwrap();
        }
        let entries = db.list_entries(1000).unwrap();
        let texts = entries
            .iter()
            .filter(|e| e.mime_type == "text/plain")
            .count();
        let images = entries
            .iter()
            .filter(|e| e.mime_type.starts_with("image/"))
            .count();
        assert_eq!(texts, MAX_TEXT_ENTRIES);
        assert_eq!(images, MAX_IMAGE_ENTRIES);
    }

    #[test]
    fn trim_respects_configured_retention() {
        let (_guard, _dir, _db) = open_test_db(); // holds the env lock
                                                  // Reopen with a custom retention on a fresh dir.
        let dir = tempfile::tempdir().expect("tempdir");
        std::env::set_var("XDG_DATA_HOME", dir.path());
        let db = HistoryDb::open_with(Retention { text: 3, images: 2 }).expect("open db");
        for i in 0..10 {
            db.insert_text(&format!("t{i}"), CaptureSource::Clipboard)
                .unwrap();
        }
        for i in 0..5 {
            db.insert_image(
                format!("img-{i}").as_bytes(),
                "image/png",
                CaptureSource::Clipboard,
            )
            .unwrap();
        }
        let entries = db.list_entries(100).unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.mime_type == "text/plain")
                .count(),
            3
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.mime_type.starts_with("image/"))
                .count(),
            2
        );
    }

    #[test]
    fn pinned_entries_survive_trim() {
        let (_guard, _dir, db) = open_test_db();
        for i in 0..205 {
            db.insert_text(&format!("t{i}"), CaptureSource::Clipboard)
                .unwrap();
        }
        let entries = db.list_entries(1000).unwrap();
        let oldest_id = entries.last().unwrap().id;
        let oldest_text = entries.last().unwrap().content.clone().unwrap();
        db.set_pinned(oldest_id, true).unwrap();

        // Push past the cap: the pinned entry must survive while the rest
        // stay capped.
        for i in 0..10 {
            db.insert_text(&format!("more{i}"), CaptureSource::Clipboard)
                .unwrap();
        }
        let entries = db.list_entries(1000).unwrap();
        assert!(
            entries.iter().any(|e| e.id == oldest_id && e.pinned),
            "a pinned entry must never be trimmed"
        );
        assert_eq!(
            entries
                .iter()
                .filter(|e| !e.pinned && e.mime_type == "text/plain")
                .count(),
            MAX_TEXT_ENTRIES
        );

        // Unpinning lets it get trimmed again like any other entry.
        db.set_pinned(oldest_id, false).unwrap();
        for i in 0..10 {
            db.insert_text(&format!("final{i}"), CaptureSource::Clipboard)
                .unwrap();
        }
        let entries = db.list_entries(1000).unwrap();
        assert!(
            !entries
                .iter()
                .any(|e| e.content.as_deref() == Some(oldest_text.as_str())),
            "an unpinned entry falls back under normal trimming"
        );
    }

    #[test]
    fn pinned_entries_sort_first() {
        let (_guard, _dir, db) = open_test_db();
        db.insert_text("a", CaptureSource::Clipboard).unwrap();
        db.insert_text("b", CaptureSource::Clipboard).unwrap();
        let second_id = db.list_entries(10).unwrap()[1].id;

        db.set_pinned(second_id, true).unwrap();

        let entries = db.list_entries(10).unwrap();
        assert_eq!(entries[0].id, second_id, "pinned rows sort before unpinned");
        assert!(entries[0].pinned);
    }

    #[test]
    fn primary_source_is_recorded() {
        let (_guard, _dir, db) = open_test_db();
        db.insert_text("primary text", CaptureSource::Primary)
            .unwrap();
        db.insert_image(b"png", "image/png", CaptureSource::Clipboard)
            .unwrap();

        let entries = db.list_entries(10).unwrap();
        let text = entries
            .iter()
            .find(|e| e.mime_type == "text/plain")
            .unwrap();
        assert!(text.is_primary, "primary-selection text is tagged");
        let img = entries
            .iter()
            .find(|e| e.mime_type.starts_with("image/"))
            .unwrap();
        assert!(!img.is_primary, "clipboard image is not tagged primary");
    }

    #[test]
    fn insert_image_jpeg_stores_jpg_file_and_trims_with_other_images() {
        let (_guard, _dir, db) = open_test_db();
        db.insert_image(
            b"\xff\xd8\xff fake jpeg",
            "image/jpeg",
            CaptureSource::Clipboard,
        )
        .unwrap();
        let jpeg_path = db
            .list_entries(10)
            .unwrap()
            .into_iter()
            .find(|e| e.mime_type == "image/jpeg")
            .expect("jpeg entry")
            .image_path
            .unwrap();
        assert!(jpeg_path.ends_with(".jpg"));
        assert!(Path::new(&jpeg_path).exists());

        // 49 more images: the jpeg is still within the newest 50.
        for i in 0..49 {
            db.insert_image(
                format!("img-{i}").as_bytes(),
                "image/png",
                CaptureSource::Clipboard,
            )
            .unwrap();
        }
        let entries = db.list_entries(100).unwrap();
        assert_eq!(
            entries
                .iter()
                .filter(|e| e.mime_type.starts_with("image/"))
                .count(),
            MAX_IMAGE_ENTRIES
        );
        assert!(
            entries.iter().any(|e| e.mime_type == "image/jpeg"),
            "jpeg is among the newest 50 images, so it must survive trim"
        );

        // Push it past the cap: the jpeg row and its file both go.
        for i in 0..10 {
            db.insert_image(
                format!("late-{i}").as_bytes(),
                "image/png",
                CaptureSource::Clipboard,
            )
            .unwrap();
        }
        let entries = db.list_entries(100).unwrap();
        assert!(!entries.iter().any(|e| e.mime_type == "image/jpeg"));
        assert!(
            !Path::new(&jpeg_path).exists(),
            "trim must remove the file of a trimmed image row"
        );
    }

    #[test]
    fn insert_image_propagates_write_failure_without_leaving_a_row() {
        let (_guard, _dir, db) = open_test_db();
        // Replace the images dir with a regular file so create_dir_all fails.
        let images_dir = data_dir().join("images");
        std::fs::create_dir_all(&images_dir).unwrap();
        std::fs::remove_dir_all(&images_dir).unwrap();
        std::fs::write(&images_dir, b"not a directory").unwrap();

        let result = db.insert_image(b"png bytes", "image/png", CaptureSource::Clipboard);
        assert!(
            result.is_err(),
            "a failed image write must surface as an error"
        );
        assert!(
            db.list_entries(10).unwrap().is_empty(),
            "a failed image write must not leave a broken row behind"
        );

        std::fs::remove_file(&images_dir).unwrap();
    }

    #[test]
    fn gc_orphaned_images_removes_unreferenced_but_keeps_referenced_files() {
        let (_guard, _dir, db) = open_test_db();
        db.insert_image(b"png-a", "image/png", CaptureSource::Clipboard)
            .unwrap();
        let referenced = db
            .list_entries(10)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
            .image_path
            .unwrap();

        // An orphan: an image file no history row references. Fake an old
        // mtime so the age guard (which protects concurrent in-flight
        // writes) doesn't skip it.
        let orphan = data_dir().join("images/orphan.png");
        std::fs::write(&orphan, b"orphan").unwrap();
        let file = std::fs::File::options().write(true).open(&orphan).unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(7200);
        file.set_modified(old).unwrap();

        db.gc_orphaned_images().unwrap();

        assert!(
            Path::new(&referenced).exists(),
            "a referenced image must survive GC"
        );
        assert!(!orphan.exists(), "an unreferenced image must be swept");
    }
}
