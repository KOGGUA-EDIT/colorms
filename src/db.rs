//! SQLite persistence for Colorms.
//!
//! The database lives in `%APPDATA%\Colorms\colorms.db`, uses WAL so a UI
//! autosave never blocks the window, and keeps two tables:
//!
//! * `colors` — recently used colours (unique by hex), pruned to a small list.
//! * `settings` — JSON blobs such as the last picked colour and window size.

use crate::{color::PickerColor, color::Rgb8};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// How many recent colours the picker keeps around.
pub const RECENT_LIMIT: usize = 24;

pub const KEY_LAST_COLOR: &str = "last_color";
pub const KEY_WINDOW_SIZE: &str = "window_size";

#[derive(Debug, Error)]
pub enum DbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredColor {
    pub hex: String,
    pub color: PickerColor,
    pub updated_at_ms: i64,
}

pub struct Db {
    conn: Connection,
    path: Option<PathBuf>,
}

impl Db {
    /// Open (and migrate) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, DbError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        let db = Self {
            conn,
            path: Some(path.to_path_buf()),
        };
        db.migrate()?;
        Ok(db)
    }

    /// Fallback database used when the app-data directory cannot be written.
    pub fn in_memory() -> Result<Self, DbError> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn, path: None };
        db.migrate()?;
        Ok(db)
    }

    /// Open the standard per-user database.
    pub fn open_default() -> Result<Self, DbError> {
        Self::open(default_db_path())
    }

    fn migrate(&self) -> Result<(), DbError> {
        self.conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS colors (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                hex         TEXT    NOT NULL UNIQUE,
                hue         REAL    NOT NULL,
                saturation  REAL    NOT NULL,
                value       REAL    NOT NULL,
                created_at  INTEGER NOT NULL,
                updated_at  INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_colors_updated
                ON colors(updated_at DESC, id DESC);
            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY NOT NULL,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(())
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn is_in_memory(&self) -> bool {
        self.path.is_none()
    }

    /// Insert or update a colour and move it to the front of the recent list.
    pub fn save_color(&self, color: PickerColor) -> Result<(), DbError> {
        let color = color.clamped();
        let now = now_ms();
        let hex = color.hex();
        self.conn.execute(
            r#"
            INSERT INTO colors (hex, hue, saturation, value, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?5)
            ON CONFLICT(hex) DO UPDATE SET
                hue = excluded.hue,
                saturation = excluded.saturation,
                value = excluded.value,
                updated_at = excluded.updated_at
            "#,
            params![hex, color.hue, color.saturation, color.value, now],
        )?;
        self.trim_recent()?;
        Ok(())
    }

    fn trim_recent(&self) -> Result<(), DbError> {
        self.conn.execute(
            r#"
            DELETE FROM colors
            WHERE id NOT IN (
                SELECT id FROM colors
                ORDER BY updated_at DESC, id DESC
                LIMIT ?1
            )
            "#,
            params![RECENT_LIMIT as i64],
        )?;
        Ok(())
    }

    pub fn recent_colors(&self, limit: usize) -> Result<Vec<StoredColor>, DbError> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT hex, hue, saturation, value, updated_at
            FROM colors
            ORDER BY updated_at DESC, id DESC
            LIMIT ?1
            "#,
        )?;
        let rows = stmt.query_map(params![limit.max(1) as i64], |row| {
            Ok(StoredColor {
                hex: row.get(0)?,
                color: PickerColor {
                    hue: row.get(1)?,
                    saturation: row.get(2)?,
                    value: row.get(3)?,
                },
                updated_at_ms: row.get(4)?,
            })
        })?;
        let mut colors = Vec::new();
        for row in rows {
            colors.push(row?);
        }
        Ok(colors)
    }

    /// Returns `true` when a row was actually removed.
    pub fn delete_color(&self, hex: &str) -> Result<bool, DbError> {
        let changed = self
            .conn
            .execute("DELETE FROM colors WHERE hex = ?1", params![hex])?;
        Ok(changed > 0)
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, DbError> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params![key],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.conn.execute(
            r#"
            INSERT INTO settings (key, value) VALUES (?1, ?2)
            ON CONFLICT(key) DO UPDATE SET value = excluded.value
            "#,
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_json<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, DbError> {
        match self.get_setting(key)? {
            Some(raw) => Ok(Some(serde_json::from_str(&raw)?)),
            None => Ok(None),
        }
    }

    pub fn set_json<T: Serialize>(&self, key: &str, value: &T) -> Result<(), DbError> {
        let raw = serde_json::to_string(value)?;
        self.set_setting(key, &raw)
    }

    pub fn load_last_color(&self) -> Result<Option<PickerColor>, DbError> {
        Ok(self
            .get_json::<PickerColor>(KEY_LAST_COLOR)?
            .map(PickerColor::clamped))
    }

    pub fn save_last_color(&self, color: PickerColor) -> Result<(), DbError> {
        self.set_json(KEY_LAST_COLOR, &color.clamped())
    }

    pub fn load_window_size(&self) -> Result<Option<[f32; 2]>, DbError> {
        let size = self.get_json::<[f32; 2]>(KEY_WINDOW_SIZE)?;
        Ok(size.filter(|[w, h]| w.is_finite() && h.is_finite()))
    }

    pub fn save_window_size(&self, width: f32, height: f32) -> Result<(), DbError> {
        self.set_json(KEY_WINDOW_SIZE, &[width, height])
    }

    #[cfg(test)]
    pub(crate) fn count_colors(&self) -> Result<i64, DbError> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM colors", [], |row| row.get(0))?)
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `%APPDATA%\Colorms\colorms.db`, with sensible fallbacks.
pub fn default_db_path() -> PathBuf {
    config_dir().join("colorms.db")
}

/// Directory for logs and config. Created lazily by [`Db::open`].
pub fn config_dir() -> PathBuf {
    if let Some(appdata) = std::env::var_os("APPDATA") {
        let candidate = PathBuf::from(appdata).join("Colorms");
        if !candidate.as_os_str().is_empty() {
            return candidate;
        }
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(profile)
            .join("AppData")
            .join("Roaming")
            .join("Colorms");
    }
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".colorms")
}

/// Convenience used by the picker's footer/palette preview.
pub fn swatch_hex(color: PickerColor) -> String {
    let rgb: Rgb8 = color.rgb();
    rgb.to_hex()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(dir.path().join("nested").join("colorms.db")).unwrap();
        (dir, db)
    }

    #[test]
    fn creates_directory_and_wal_mode() {
        let (_dir, db) = tmp_db();
        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");
        assert!(!db.is_in_memory());
    }

    #[test]
    fn saves_updates_and_orders_recent_colors() {
        let (_dir, db) = tmp_db();
        let red = PickerColor::new(0.0, 1.0, 1.0);
        db.save_color(red).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(3));
        let green = PickerColor::new(120.0, 1.0, 1.0);
        db.save_color(green).unwrap();
        let recent = db.recent_colors(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].hex, "#00FF00");
        assert_eq!(recent[0].color, green);

        // Upserting a colour moves it back to the front and keeps one row.
        std::thread::sleep(std::time::Duration::from_millis(3));
        db.save_color(red).unwrap();
        let recent = db.recent_colors(10).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].hex, "#FF0000");
        assert_eq!(db.count_colors().unwrap(), 2);

        // Values survive a round-trip through SQLite.
        assert!((recent[0].color.saturation - 1.0).abs() < 1e-6);
        assert!((recent[0].color.value - 1.0).abs() < 1e-6);
    }

    #[test]
    fn prunes_to_recent_limit() {
        let (_dir, db) = tmp_db();
        for i in 0..(RECENT_LIMIT + 8) {
            let c = PickerColor::new((i * 17) as f32 % 360.0, 0.8, 0.9);
            db.save_color(c).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(db.count_colors().unwrap(), RECENT_LIMIT as i64);
        assert_eq!(db.recent_colors(100).unwrap().len(), RECENT_LIMIT);
    }

    #[test]
    fn deletes_colors() {
        let (_dir, db) = tmp_db();
        db.save_color(PickerColor::new(30.0, 0.5, 0.5)).unwrap();
        let hex = PickerColor::new(30.0, 0.5, 0.5).hex();
        assert!(db.delete_color(&hex).unwrap());
        assert!(!db.delete_color(&hex).unwrap());
        assert!(db.recent_colors(5).unwrap().is_empty());
    }

    #[test]
    fn settings_and_json_roundtrip() {
        let (_dir, db) = tmp_db();
        assert_eq!(db.get_setting("missing").unwrap(), None);
        db.set_setting("theme", "dark").unwrap();
        assert_eq!(db.get_setting("theme").unwrap().as_deref(), Some("dark"));

        let color = PickerColor::new(200.0, 0.3, 0.7);
        db.save_last_color(color).unwrap();
        assert_eq!(db.load_last_color().unwrap(), Some(color));

        db.save_window_size(800.0, 600.0).unwrap();
        assert_eq!(db.load_window_size().unwrap(), Some([800.0, 600.0]));
        assert_eq!(
            db.get_json::<PickerColor>(KEY_LAST_COLOR).unwrap(),
            Some(color)
        );
    }

    #[test]
    fn in_memory_fallback_works() {
        let db = Db::in_memory().unwrap();
        assert!(db.is_in_memory());
        let color = PickerColor::new(310.0, 0.8, 0.6);
        db.save_color(color).unwrap();
        assert_eq!(db.recent_colors(5).unwrap()[0].color, color);
    }
}
