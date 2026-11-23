mod notes;
mod shortcuts;

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Result, anyhow};
use rusqlite::{
    Connection, OptionalExtension, TransactionBehavior, functions::FunctionFlags, params,
};
use sha2::{Digest, Sha256};
pub use winycast_core::unix_time;
use winycast_core::{ClipKind, ClipboardData, Entry, Row, Settings, Target, Usage, web_address};

const UNPINNED_LIMIT: i64 = 200;

#[derive(Clone)]
pub struct Storage(Arc<Mutex<Connection>>);

impl Storage {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Self::configure(Connection::open(path)?)
    }

    fn configure(mut connection: Connection) -> Result<Self> {
        connection.busy_timeout(Duration::from_millis(700))?;
        connection.execute_batch(
            "PRAGMA journal_mode = DELETE;
             PRAGMA secure_delete = ON;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS settings (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                value TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS usage (
                id TEXT PRIMARY KEY,
                count INTEGER NOT NULL DEFAULT 0,
                pinned INTEGER NOT NULL DEFAULT 0,
                last_used INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS clipboard (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                digest BLOB NOT NULL UNIQUE,
                kind INTEGER NOT NULL CHECK(kind IN (0, 1)),
                text TEXT,
                png BLOB,
                seen INTEGER NOT NULL,
                pinned INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS clipboard_order ON clipboard(pinned DESC, seen DESC, id DESC);
             CREATE TABLE IF NOT EXISTS clipboard_image_ocr (
                clip_id INTEGER PRIMARY KEY REFERENCES clipboard(id) ON DELETE CASCADE,
                search_text TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS shortcuts (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                kind INTEGER NOT NULL CHECK(kind IN (0, 1)),
                title TEXT NOT NULL,
                body TEXT NOT NULL
             );
             CREATE TABLE IF NOT EXISTS notes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                revision INTEGER NOT NULL DEFAULT 1,
                title TEXT NOT NULL,
                markdown TEXT NOT NULL
             );",
        )?;
        let migration = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let has_last_used: bool = migration.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('usage') WHERE name = 'last_used')",
            [],
            |row| row.get(0),
        )?;
        if !has_last_used {
            migration.execute(
                "ALTER TABLE usage ADD COLUMN last_used INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        migration.commit()?;
        connection.create_scalar_function(
            "is_web_address",
            1,
            FunctionFlags::SQLITE_DETERMINISTIC | FunctionFlags::SQLITE_UTF8,
            |context| {
                Ok(context
                    .get_raw(0)
                    .as_str()
                    .is_ok_and(|value| web_address(value).is_some()))
            },
        )?;
        Ok(Self(Arc::new(Mutex::new(connection))))
    }

    fn with<T>(&self, operation: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        operation(
            &mut *self
                .0
                .lock()
                .map_err(|_| anyhow!("SQLite connection lock poisoned"))?,
        )
    }

    pub fn settings(&self) -> Result<Settings> {
        self.with(|connection| {
            let value: Option<String> = connection
                .query_row("SELECT value FROM settings WHERE id = 1", [], |row| {
                    row.get(0)
                })
                .optional()?;
            Ok(match value {
                Some(value) => serde_json::from_str(&value)?,
                None => Settings::default(),
            })
        })
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<()> {
        self.with(|connection| {
            let tx = connection.transaction()?;
            tx.execute(
                "INSERT INTO settings (id, value) VALUES (1, ?1)
                 ON CONFLICT(id) DO UPDATE SET value = excluded.value",
                [serde_json::to_string(settings)?],
            )?;
            if !settings.clipboard_ocr {
                tx.execute("DELETE FROM clipboard_image_ocr", [])?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn usage(&self) -> Result<HashMap<String, Usage>> {
        self.with(|connection| {
            let mut query = connection.prepare("SELECT id, count, pinned, last_used FROM usage")?;
            let rows = query.query_map([], |row| {
                Ok((
                    row.get(0)?,
                    Usage {
                        count: row.get(1)?,
                        pinned: row.get(2)?,
                        last_used: row.get(3)?,
                    },
                ))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn used(&self, id: &str) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "INSERT INTO usage(id, count, last_used) VALUES (?1, 1, ?2)
                 ON CONFLICT(id) DO UPDATE SET count = min(count + 1, 1000000),
                    last_used = max(last_used, excluded.last_used)",
                params![id, unix_time()],
            )?;
            Ok(())
        })
    }

    pub fn toggle_favorite(&self, id: &str) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "INSERT INTO usage(id, pinned) VALUES (?1, 1)
                 ON CONFLICT(id) DO UPDATE SET pinned = 1 - pinned",
                [id],
            )?;
            Ok(())
        })
    }

    pub fn add_clip(&self, data: ClipboardData, now: i64, retention_days: u16) -> Result<()> {
        self.add_clip_if(data, now, retention_days, || true)
    }

    pub fn add_clip_if(
        &self,
        data: ClipboardData,
        now: i64,
        retention_days: u16,
        enabled: impl FnOnce() -> bool,
    ) -> Result<()> {
        let (kind, text, png, bytes) = match &data {
            ClipboardData::Text(text) => (0, Some(text.as_str()), None, text.as_bytes()),
            ClipboardData::Png(png) => (1, None, Some(png.as_slice()), png.as_slice()),
        };
        let mut digest = Sha256::new();
        digest.update([kind as u8]);
        digest.update(bytes);
        self.with(|connection| {
            if !enabled() { return Ok(()); }
            let tx = connection.transaction()?;
            tx.execute(
                "INSERT INTO clipboard(digest, kind, text, png, seen) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(digest) DO UPDATE SET seen = max(seen, excluded.seen)",
                params![digest.finalize().as_slice(), kind, text, png, now],
            )?;
            expire(&tx, now, retention_days)?;
            tx.execute(
                "DELETE FROM clipboard WHERE id IN (
                    SELECT id FROM clipboard WHERE pinned = 0 ORDER BY seen DESC, id DESC LIMIT -1 OFFSET ?1
                 )",
                [UNPINNED_LIMIT],
            )?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn clips(&self, query: &str, cancelled: impl Fn() -> bool) -> Result<Vec<Row>> {
        self.search_clips(query, false, cancelled)
    }

    pub fn clip_links(&self, query: &str, cancelled: impl Fn() -> bool) -> Result<Vec<Row>> {
        self.search_clips(query, true, cancelled)
    }

    fn search_clips(
        &self,
        query: &str,
        links_only: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<Row>> {
        self.with(|connection| {
            let mut stmt = connection.prepare(
                "SELECT id, kind, substr(text, 1, 768), pinned, is_web_address(text)
                 FROM clipboard LEFT JOIN clipboard_image_ocr ON clip_id = id
                 WHERE (?3 = 0 OR (kind = 0 AND is_web_address(text)))
                 AND (?1 = '' OR case kind
                    WHEN 0 THEN instr(lower(text), lower(?1)) > 0
                    ELSE instr('图片 image png', lower(?1)) > 0
                        OR (?2 <> '' AND instr(search_text, ?2) > 0) END)
                 ORDER BY pinned DESC, seen DESC, id DESC LIMIT 500",
            )?;
            let mut rows = stmt.query(params![query.trim(), ocr_search_text(query), links_only])?;
            let mut results = Vec::new();
            while let Some(row) = rows.next()? {
                if cancelled() {
                    return Ok(Vec::new());
                }
                let id: i64 = row.get(0)?;
                let kind: i64 = row.get(1)?;
                let (title, detail, kind) = if kind == 0 {
                    let sample: String = row.get(2)?;
                    let title = sample
                        .lines()
                        .find(|line| !line.trim().is_empty())
                        .unwrap_or("文本")
                        .trim()
                        .chars()
                        .take(120)
                        .collect::<String>();
                    let kind = if row.get::<_, bool>(4)? {
                        ClipKind::Link
                    } else {
                        ClipKind::Text
                    };
                    (title, sample, kind)
                } else {
                    ("图片".into(), "PNG · 剪贴板图片".into(), ClipKind::Image)
                };
                results.push(Row {
                    entry: Entry::display(
                        format!("clip:{id}"),
                        title,
                        detail,
                        Target::Clip(id, kind),
                    ),
                    pinned: row.get(3)?,
                    suggested: false,
                });
            }
            Ok(results)
        })
    }

    pub fn clip(&self, id: i64) -> Result<Option<ClipboardData>> {
        self.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT kind, text, png FROM clipboard WHERE id = ?1",
                    [id],
                    |row| {
                        if row.get::<_, i64>(0)? == 0 {
                            Ok(ClipboardData::Text(row.get(1)?))
                        } else {
                            Ok(ClipboardData::Png(row.get(2)?))
                        }
                    },
                )
                .optional()?)
        })
    }

    pub fn pending_image_ocr(&self) -> Result<Vec<i64>> {
        self.with(|connection| {
            let mut query = connection.prepare(
                "SELECT id FROM clipboard LEFT JOIN clipboard_image_ocr ON clip_id = id
                 WHERE kind = 1 AND clip_id IS NULL
                 ORDER BY pinned DESC, seen DESC, id DESC",
            )?;
            let images = query.query_map([], |row| row.get(0))?;
            Ok(images.collect::<rusqlite::Result<_>>()?)
        })
    }

    pub fn save_image_ocr(
        &self,
        id: i64,
        text: &str,
        cancelled: impl FnOnce() -> bool,
    ) -> Result<bool> {
        let text = ocr_search_text(text);
        self.with(|connection| {
            if cancelled() {
                return Ok(false);
            }
            Ok(connection.execute(
                "INSERT OR IGNORE INTO clipboard_image_ocr (clip_id, search_text)
                 SELECT id, ?2 FROM clipboard
                 WHERE id = ?1 AND kind = 1 AND EXISTS (
                    SELECT 1 FROM settings WHERE id = 1 AND json_extract(value, '$.clipboard_ocr') = 1
                 )",
                params![id, text],
            )? != 0)
        })
    }

    pub fn toggle_clip_pin(&self, id: i64) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "UPDATE clipboard SET pinned = 1 - pinned WHERE id = ?1",
                [id],
            )?;
            Ok(())
        })
    }

    pub fn clear_clips(&self) -> Result<()> {
        self.with(|connection| {
            connection.execute("DELETE FROM clipboard", [])?;
            Ok(())
        })
    }

    pub fn expire_clips(&self, now: i64, days: u16) -> Result<usize> {
        self.with(|connection| expire(connection, now, days))
    }
}

fn ocr_search_text(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_whitespace())
        .flat_map(char::to_lowercase)
        .take(16_384)
        .collect()
}

fn expire(connection: &Connection, now: i64, days: u16) -> Result<usize> {
    Ok(connection.execute(
        "DELETE FROM clipboard WHERE pinned = 0 AND seen < ?1",
        [now.saturating_sub(i64::from(days.max(1)) * 86400)],
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_settings_and_updates_usage() -> Result<()> {
        let storage = Storage::configure(Connection::open_in_memory()?)?;
        let settings = Settings {
            opacity: 85,
            ..Default::default()
        };
        storage.save_settings(&settings)?;
        assert_eq!(storage.settings()?, settings);
        storage.used("app:x")?;
        storage.toggle_favorite("app:x")?;
        assert_eq!(storage.usage()?["app:x"].count, 1);
        assert!(storage.usage()?["app:x"].pinned);
        storage.toggle_favorite("app:x")?;
        assert!(!storage.usage()?["app:x"].pinned);
        Ok(())
    }

    #[test]
    fn migrates_existing_usage_and_records_recent_launches() -> Result<()> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "CREATE TABLE usage (id TEXT PRIMARY KEY, count INTEGER NOT NULL DEFAULT 0, pinned INTEGER NOT NULL DEFAULT 0);
             INSERT INTO usage VALUES ('app:legacy', 7, 1);",
        )?;
        let storage = Storage::configure(connection)?;
        let prior = storage.usage()?["app:legacy"];
        assert_eq!(prior.count, 7);
        assert!(prior.pinned);
        assert_eq!(prior.last_used, 0);
        storage.used("app:legacy")?;
        let current = storage.usage()?["app:legacy"];
        assert_eq!(current.count, 8);
        assert!(current.pinned);
        assert!(current.last_used > 0);
        Ok(())
    }

    #[test]
    fn clipboard_link_filter_keeps_only_web_urls_and_honors_search() -> Result<()> {
        let storage = Storage::configure(Connection::open_in_memory()?)?;
        for (seen, text) in [
            (1, "see https://example.org/"),
            (2, "file:///C:/example.org"),
            (3, "https://example.org/docs"),
            (4, "http://example.com/"),
        ] {
            storage.add_clip(ClipboardData::Text(text.into()), seen, 30)?;
        }
        storage.add_clip(ClipboardData::Png(vec![1, 2]), 5, 30)?;
        let all = storage.clips("", || false)?;
        assert_eq!(all.len(), 5);
        assert!(matches!(
            all[0].entry.target,
            Target::Clip(_, ClipKind::Image)
        ));
        assert_eq!(storage.clip_links("", || false)?.len(), 2);
        let links = storage.clip_links("docs", || false)?;
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].entry.title, "https://example.org/docs");
        assert!(matches!(
            links[0].entry.target,
            Target::Clip(_, ClipKind::Link)
        ));
        assert!(storage.clip_links("", || true)?.is_empty());
        Ok(())
    }

    #[test]
    fn history_deduplicates_preserves_pins_expires_and_clears() -> Result<()> {
        let storage = Storage::configure(Connection::open_in_memory()?)?;
        storage.add_clip(ClipboardData::Text("Example 北京".into()), 1, 30)?;
        storage.add_clip(ClipboardData::Text("Example 北京".into()), 2, 30)?;
        storage.add_clip(ClipboardData::Png(vec![1, 2, 3]), 3, 30)?;
        let rows = storage.clips("北京", || false)?;
        assert_eq!(rows.len(), 1);
        let Target::Clip(id, ClipKind::Text) = rows[0].entry.target else {
            panic!()
        };
        storage.toggle_clip_pin(id)?;
        assert_eq!(storage.clips("", || false)?.len(), 2);
        assert_eq!(storage.expire_clips(172800, 1)?, 1);
        assert!(matches!(storage.clip(id)?, Some(ClipboardData::Text(_))));
        storage.clear_clips()?;
        assert!(storage.clips("", || false)?.is_empty());
        Ok(())
    }

    #[test]
    fn image_ocr_search_requires_opt_in_and_cleans_up_with_history() -> Result<()> {
        let storage = Storage::configure(Connection::open_in_memory()?)?;
        storage.add_clip(ClipboardData::Png(vec![1]), 1, 30)?;
        storage.add_clip(ClipboardData::Png(vec![2]), 2, 30)?;
        let ids = storage.pending_image_ocr()?;
        assert_eq!(ids.len(), 2);
        let (pinned, other) = (ids[0], ids[1]);
        assert!(!storage.save_image_ocr(pinned, "private by default", || false)?);
        assert!(storage.clips("private by default", || false)?.is_empty());

        let enabled = Settings {
            clipboard_ocr: true,
            ..Settings::default()
        };
        storage.save_settings(&enabled)?;
        assert!(!storage.save_image_ocr(pinned, "cancelled", || true)?);
        assert_eq!(storage.pending_image_ocr()?, ids);
        assert!(storage.save_image_ocr(pinned, "中 文 识 别\nWinycast OCR", || false)?);
        assert!(storage.save_image_ocr(other, "expires", || false)?);
        assert!(storage.pending_image_ocr()?.is_empty());
        assert_eq!(
            storage.clips("中文识别", || false)?[0].entry.target,
            Target::Clip(pinned, ClipKind::Image)
        );
        assert_eq!(storage.clips("winycast ocr", || false)?.len(), 1);
        assert_eq!(storage.clips("png", || false)?.len(), 2);

        storage.toggle_clip_pin(pinned)?;
        assert_eq!(storage.expire_clips(172800, 1)?, 1);
        assert!(storage.clips("expires", || false)?.is_empty());
        assert!(!storage.save_image_ocr(other, "stale", || false)?);

        storage.save_settings(&Settings::default())?;
        assert!(storage.clips("中文识别", || false)?.is_empty());
        assert_eq!(storage.clips("png", || false)?.len(), 1);
        assert!(!storage.save_image_ocr(pinned, "stale", || false)?);
        assert_eq!(storage.pending_image_ocr()?, vec![pinned]);

        storage.save_settings(&enabled)?;
        assert!(storage.save_image_ocr(pinned, "before clear", || false)?);
        storage.clear_clips()?;
        assert!(!storage.save_image_ocr(pinned, "stale", || false)?);
        let indexed = storage.with(|connection| {
            Ok(
                connection.query_row("SELECT count(*) FROM clipboard_image_ocr", [], |row| {
                    row.get::<_, u32>(0)
                })?,
            )
        })?;
        assert_eq!(indexed, 0);
        Ok(())
    }
}
