use anyhow::{Context, Result, anyhow};
use rusqlite::{OptionalExtension, params};
use winycast_core::{Entry, Note, NoteDraft, NoteVersion, Target, note_entry_id};

use crate::Storage;

impl Storage {
    pub fn note(&self, id: i64) -> Result<Option<Note>> {
        self.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT revision, title, markdown FROM notes WHERE id = ?1",
                    [id],
                    |row| {
                        Ok(Note {
                            version: NoteVersion { id, revision: row.get(0)? },
                            draft: NoteDraft { title: row.get(1)?, markdown: row.get(2)? },
                        })
                    },
                )
                .optional()?)
        })
    }

    pub fn save_note(&self, version: Option<NoteVersion>, draft: NoteDraft) -> Result<Note> {
        let draft = draft.validated().map_err(|message| anyhow!(message))?;
        let version = self.with(|connection| {
            if let Some(version) = version {
                let revision = connection
                    .query_row(
                        "UPDATE notes SET title = ?1, markdown = ?2, revision = revision + 1
                         WHERE id = ?3 AND revision = ?4 RETURNING revision",
                        params![draft.title, draft.markdown, version.id, version.revision],
                        |row| row.get(0),
                    )
                    .optional()?
                    .context("笔记已被更改或删除，无法覆盖保存")?;
                Ok(NoteVersion { revision, ..version })
            } else {
                let id = connection.query_row(
                    "INSERT INTO notes (title, markdown) VALUES (?1, ?2) RETURNING id",
                    params![draft.title, draft.markdown],
                    |row| row.get(0),
                )?;
                Ok(NoteVersion { id, revision: 1 })
            }
        })?;
        Ok(Note { version, draft })
    }

    pub fn delete_note(&self, id: i64) -> Result<()> {
        self.with(|connection| {
            connection.execute("DELETE FROM notes WHERE id = ?1", [id])?;
            Ok(())
        })
    }

    pub fn note_entries(&self, query: &str, cancelled: impl Fn() -> bool) -> Result<Vec<Entry>> {
        if cancelled() {
            return Ok(Vec::new());
        }
        let query = query.to_lowercase();
        let terms: Vec<_> = query.split_whitespace().collect();
        self.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, title, substr(markdown, 1, 240), CASE WHEN ?1 THEN markdown END
                 FROM notes ORDER BY id DESC",
            )?;
            let mut rows = statement.query([!terms.is_empty()])?;
            let mut entries = Vec::new();
            while let Some(row) = rows.next()? {
                if cancelled() {
                    return Ok(Vec::new());
                }
                let id = row.get(0)?;
                let title: String = row.get(1)?;
                let excerpt: String = row.get(2)?;
                let markdown: Option<String> = row.get(3)?;
                let matched = markdown.is_some_and(|markdown| {
                    let body = markdown.to_lowercase();
                    let title = title.to_lowercase();
                    terms.iter().all(|term| body.contains(term) || title.contains(term))
                });
                entries.push(
                    Entry::new(note_entry_id(id), title, excerpt, "Markdown 笔记", Target::Note(id))
                        .with_content_match(matched),
                );
            }
            Ok(entries)
        })
    }
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    #[test]
    fn saves_verbatim_body_and_rejects_conflicting_updates() -> Result<()> {
        let storage = Storage::configure(Connection::open_in_memory()?)?;
        let draft = NoteDraft {
            title: " 项目记录 ".into(),
            markdown: "  正文\n\n    indented code\n".into(),
        };
        let note = storage.save_note(None, draft.clone())?;
        assert_eq!(note.draft.title, "项目记录");
        assert_eq!(note.draft.markdown, draft.markdown);
        assert_eq!(storage.note(note.version.id)?, Some(note.clone()));
        let revised = storage.save_note(
            Some(note.version),
            NoteDraft { markdown: "修订".into(), ..draft.clone() },
        )?;
        assert!(storage.save_note(Some(note.version), draft.clone()).is_err());
        assert_eq!(storage.note(note.version.id)?, Some(revised.clone()));
        storage.delete_note(note.version.id)?;
        assert!(storage.save_note(Some(revised.version), draft).is_err());
        assert!(storage.note(note.version.id)?.is_none());
        Ok(())
    }
}
