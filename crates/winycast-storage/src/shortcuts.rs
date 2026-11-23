use anyhow::{Result, anyhow, ensure};
use rusqlite::{OptionalExtension, Row, params};
use winycast_core::{Entry, Shortcut, ShortcutDraft, ShortcutKind, shortcut_entry_id};

use crate::{Storage, unix_time};

impl Storage {
    pub fn shortcuts(&self, cancelled: impl Fn() -> bool) -> Result<Vec<Entry>> {
        self.with(|connection| {
            let mut query = connection.prepare(
                "SELECT id, kind, title, substr(body, 1, 240) FROM shortcuts ORDER BY id",
            )?;
            let mut entries = Vec::new();
            for result in query.query_map([], read_shortcut)? {
                if cancelled() {
                    return Ok(Vec::new());
                }
                entries.push(result?.into_entry());
            }
            Ok(entries)
        })
    }

    pub fn shortcut(&self, id: i64) -> Result<Option<Shortcut>> {
        self.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT id, kind, title, body FROM shortcuts WHERE id = ?1",
                    [id],
                    read_shortcut,
                )
                .optional()?)
        })
    }

    pub fn save_shortcut(&self, id: Option<i64>, draft: ShortcutDraft) -> Result<i64> {
        let draft = draft.validated().map_err(|message| anyhow!(message))?;
        self.with(|connection| {
            let values = params![draft.kind as i64, draft.title, draft.body];
            match id {
                Some(id) => {
                    let updated = connection.execute(
                        "UPDATE shortcuts SET kind = ?1, title = ?2, body = ?3 WHERE id = ?4",
                        params![draft.kind as i64, draft.title, draft.body, id],
                    )?;
                    ensure!(updated != 0, "常用项已移除");
                    Ok(id)
                }
                None => Ok(connection.query_row(
                    "INSERT INTO shortcuts (kind, title, body) VALUES (?1, ?2, ?3) RETURNING id",
                    values,
                    |row| row.get(0),
                )?),
            }
        })
    }

    pub fn delete_shortcut(&self, id: i64) -> Result<()> {
        self.with(|connection| {
            let tx = connection.transaction()?;
            tx.execute("DELETE FROM shortcuts WHERE id = ?1", [id])?;
            tx.execute("DELETE FROM usage WHERE id = ?1", [shortcut_entry_id(id)])?;
            tx.commit()?;
            Ok(())
        })
    }

    pub fn record_shortcut_use(&self, id: i64) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "INSERT INTO usage(id, count, last_used) SELECT ?1, 1, ?3 FROM shortcuts WHERE id = ?2
                 ON CONFLICT(id) DO UPDATE SET count = min(count + 1, 1000000),
                    last_used = max(last_used, excluded.last_used)",
                params![shortcut_entry_id(id), id, unix_time()],
            )?;
            Ok(())
        })
    }

    pub fn toggle_shortcut_favorite(&self, id: i64) -> Result<()> {
        self.with(|connection| {
            connection.execute(
                "INSERT INTO usage(id, pinned) SELECT ?1, 1 FROM shortcuts WHERE id = ?2
                 ON CONFLICT(id) DO UPDATE SET pinned = 1 - pinned",
                params![shortcut_entry_id(id), id],
            )?;
            Ok(())
        })
    }
}

fn read_shortcut(row: &Row<'_>) -> rusqlite::Result<Shortcut> {
    let kind = match row.get(1)? {
        0i64 => ShortcutKind::Snippet,
        1 => ShortcutKind::Quicklink,
        value => return Err(rusqlite::Error::IntegralValueOutOfRange(1, value)),
    };
    Ok(Shortcut {
        id: row.get(0)?,
        draft: ShortcutDraft {
            kind,
            title: row.get(2)?,
            body: row.get(3)?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;
    use winycast_core::{Target, rank};

    #[test]
    fn manages_shortcuts_and_keeps_search_and_usage_in_sync() -> Result<()> {
        let storage = Storage::configure(Connection::open_in_memory()?)?;
        let mut draft = ShortcutDraft {
            kind: ShortcutKind::Snippet,
            title: "会议模板".into(),
            body: "第一行\n\t第二行\n".into(),
        };
        let id = storage.save_shortcut(None, draft.clone())?;
        assert_eq!(storage.shortcut(id)?.unwrap().draft, draft);
        let entries = storage.shortcuts(|| false)?;
        let key = shortcut_entry_id(id);
        storage.toggle_shortcut_favorite(id)?;
        storage.record_shortcut_use(id)?;
        let rows = rank(&entries, "hymb", &storage.usage()?, || false);
        assert_eq!(
            rows[0].entry.target,
            Target::Shortcut(id, ShortcutKind::Snippet)
        );
        assert!(rows[0].pinned);
        assert!(storage.shortcuts(|| true)?.is_empty());

        draft.kind = ShortcutKind::Quicklink;
        draft.body = "file:///C:/test.exe".into();
        assert!(storage.save_shortcut(Some(id), draft.clone()).is_err());
        assert_eq!(
            storage.shortcut(id)?.unwrap().draft.kind,
            ShortcutKind::Snippet
        );
        draft.body = "https://example.com/".into();
        assert_eq!(storage.save_shortcut(Some(id), draft.clone())?, id);
        assert_eq!(storage.shortcut(id)?.unwrap().draft, draft);

        storage.delete_shortcut(id)?;
        assert!(storage.shortcut(id)?.is_none());
        assert!(storage.save_shortcut(Some(id), draft).is_err());
        storage.record_shortcut_use(id)?;
        storage.toggle_shortcut_favorite(id)?;
        assert!(!storage.usage()?.contains_key(&key));
        assert!(storage.shortcuts(|| false)?.is_empty());
        Ok(())
    }
}
