use rusqlite::{params, Connection, Result as SqlResult};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct ClipEntry {
    pub id: i64,
    pub timestamp: i64,
    pub mime_type: String,
    pub content: Option<String>,
    pub image_path: Option<String>,
    pub content_hash: String,
}

pub struct HistoryDb {
    conn: Connection,
}

impl HistoryDb {
    pub fn open() -> SqlResult<Self> {
        let dir = data_dir();
        std::fs::create_dir_all(&dir).ok();
        let db_path = dir.join("history.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS history (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                timestamp    INTEGER NOT NULL,
                mime_type    TEXT NOT NULL,
                content      TEXT,
                image_path   TEXT,
                content_hash TEXT NOT NULL UNIQUE
            );
            CREATE INDEX IF NOT EXISTS history_ts ON history(timestamp DESC);",
        )?;
        // history.db can contain plaintext secrets copied to the clipboard
        // (passwords, tokens, TOTP codes); restrict it to owner-only, every open.
        restrict_permissions(&db_path);
        Ok(Self { conn })
    }

    pub fn insert_text(&self, text: &str) -> SqlResult<()> {
        let hash = sha256_hex(text.as_bytes());
        let ts = unix_now();
        self.conn.execute(
            "INSERT INTO history (timestamp, mime_type, content, content_hash)
             VALUES (?1, 'text/plain', ?2, ?3)
             ON CONFLICT(content_hash) DO UPDATE SET timestamp = ?1",
            params![ts, text, hash],
        )?;
        self.trim(200, 50)
    }

    pub fn insert_image(&self, png_bytes: &[u8]) -> SqlResult<()> {
        let hash = sha256_hex(png_bytes);
        let images_dir = data_dir().join("images");
        std::fs::create_dir_all(&images_dir).ok();
        // Use first 16 hex chars for the filename (collision-safe for 50 images)
        let path = images_dir.join(format!("{}.png", &hash[..16]));
        if !path.exists() {
            std::fs::write(&path, png_bytes).ok();
            restrict_permissions(&path);
        }
        let path_str = path.to_string_lossy().to_string();
        let ts = unix_now();
        self.conn.execute(
            "INSERT INTO history (timestamp, mime_type, image_path, content_hash)
             VALUES (?1, 'image/png', ?2, ?3)
             ON CONFLICT(content_hash) DO UPDATE SET timestamp = ?1",
            params![ts, path_str, hash],
        )?;
        self.trim(200, 50)
    }

    pub fn list_entries(&self, limit: usize) -> SqlResult<Vec<ClipEntry>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, timestamp, mime_type, content, image_path, content_hash
             FROM history ORDER BY timestamp DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(ClipEntry {
                id: row.get(0)?,
                timestamp: row.get(1)?,
                mime_type: row.get(2)?,
                content: row.get(3)?,
                image_path: row.get(4)?,
                content_hash: row.get(5)?,
            })
        })?
        .collect::<SqlResult<Vec<_>>>();
        rows
    }

    pub fn delete_entry(&self, id: i64) -> SqlResult<()> {
        // Clean up image file if present
        let image_path: Option<String> = self
            .conn
            .query_row(
                "SELECT image_path FROM history WHERE id = ?1",
                [id],
                |row| row.get(0),
            )
            .ok()
            .flatten();
        if let Some(p) = image_path {
            let _ = std::fs::remove_file(p);
        }
        self.conn.execute("DELETE FROM history WHERE id = ?1", [id])?;
        Ok(())
    }

    fn trim(&self, max_text: usize, max_images: usize) -> SqlResult<()> {
        self.conn.execute(
            "DELETE FROM history
             WHERE mime_type = 'text/plain'
               AND id NOT IN (
                   SELECT id FROM history WHERE mime_type = 'text/plain'
                   ORDER BY timestamp DESC LIMIT ?1
               )",
            [max_text as i64],
        )?;
        // Collect old image paths before deleting rows
        let old_paths: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT image_path FROM history
                 WHERE mime_type = 'image/png'
                   AND image_path IS NOT NULL
                   AND id NOT IN (
                       SELECT id FROM history WHERE mime_type = 'image/png'
                       ORDER BY timestamp DESC LIMIT ?1
                   )",
            )?;
            let paths = stmt.query_map([max_images as i64], |row| row.get(0))?
                .collect::<SqlResult<Vec<_>>>()?;
            paths
        };
        for path in old_paths {
            let _ = std::fs::remove_file(path);
        }
        self.conn.execute(
            "DELETE FROM history
             WHERE mime_type = 'image/png'
               AND id NOT IN (
                   SELECT id FROM history WHERE mime_type = 'image/png'
                   ORDER BY timestamp DESC LIMIT ?1
               )",
            [max_images as i64],
        )?;
        Ok(())
    }
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

pub fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("~/.local/share"))
        .join("breadclip")
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
