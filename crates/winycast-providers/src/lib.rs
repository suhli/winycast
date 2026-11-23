mod clipboard_ocr;

use std::{
    cmp::Reverse,
    collections::HashMap,
    sync::{Arc, RwLock},
};

use anyhow::{Result, anyhow};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use winycast_core::{Entry, Page, Row, Target, Usage, calculate, commands, rank, unix_time};
use winycast_storage::Storage;

pub use clipboard_ocr::index_clipboard_images;

#[derive(Clone)]
pub struct SearchService {
    apps: Arc<RwLock<Arc<[Entry]>>>,
    commands: Arc<[Entry]>,
    storage: Storage,
    file_query_slot: Arc<Mutex<()>>,
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use winycast_core::{NoteDraft, ShortcutDraft, ShortcutKind};

    use super::*;

    #[test]
    fn suggestions_promote_existing_entries_by_recent_use_without_duplicates() {
        let now = 40 * 86400;
        let mut usage = HashMap::new();
        let mut rows = Vec::new();
        for index in 0..8 {
            let id = format!("app:{index}");
            let pinned = index == 7;
            usage.insert(
                id.clone(),
                Usage {
                    count: if index == 0 { 0 } else { 6 },
                    last_used: if index == 6 { now } else { 1 },
                    pinned,
                },
            );
            rows.push(Row {
                entry: Entry::display(id.clone(), id.clone(), "".into(), Target::App(id.into())),
                pinned,
                suggested: false,
            });
        }
        mark_suggestions(&mut rows, &usage, now);
        assert_eq!(rows.len(), 8);
        assert!(rows[0].pinned);
        assert!(!rows[0].suggested);
        assert_eq!(rows[1].entry.id, "app:6");
        assert!(rows[1..6].iter().all(|row| row.suggested));
        assert!(rows[6..].iter().all(|row| !row.suggested));
    }

    #[tokio::test]
    async fn searches_saved_shortcuts_in_both_views() -> Result<()> {
        let storage = Storage::open(Path::new(":memory:"))?;
        let snippet = storage.save_shortcut(
            None,
            ShortcutDraft {
                kind: ShortcutKind::Snippet,
                title: "会议纪要".into(),
                body: "议题\n结论".into(),
            },
        )?;
        let link = storage.save_shortcut(
            None,
            ShortcutDraft {
                kind: ShortcutKind::Quicklink,
                title: "文档链接".into(),
                body: "https://example.com/".into(),
            },
        )?;
        let service = SearchService::new(storage);
        let cancel = CancellationToken::new();
        let rows = service
            .search(Page::Launcher, String::new(), cancel.clone())
            .await?;
        for target in [
            Target::Shortcut(snippet, ShortcutKind::Snippet),
            Target::Shortcut(link, ShortcutKind::Quicklink),
        ] {
            assert!(rows.iter().any(|row| row.entry.target == target));
        }
        let rows = service
            .search(Page::Shortcuts, "wdlj".into(), cancel.clone())
            .await?;
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].entry.target,
            Target::Shortcut(link, ShortcutKind::Quicklink)
        );
        cancel.cancel();
        assert!(
            service
                .search(Page::Shortcuts, String::new(), cancel)
                .await?
                .is_empty()
        );
        Ok(())
    }

    #[tokio::test]
    async fn searches_note_titles_and_full_bodies() -> Result<()> {
        let storage = Storage::open(Path::new(":memory:"))?;
        let note = storage.save_note(
            None,
            NoteDraft {
                title: "项目记录".into(),
                markdown: format!("{}\n独特关键词 MIXEDcase", "前".repeat(300)),
            },
        )?;
        let target = Target::Note(note.version.id);
        let service = SearchService::new(storage.clone());
        let cancel = CancellationToken::new();
        for (page, query) in [
            (Page::Launcher, "xmjl"),
            (Page::Launcher, "独特关键词"),
            (Page::Notes, "项目 mixedcase"),
        ] {
            let results = service.search(page, query.into(), cancel.clone()).await?;
            assert!(
                results.iter().any(|row| row.entry.target == target),
                "{query}"
            );
        }
        assert!(
            service
                .search(Page::Notes, "不存在".into(), cancel.clone())
                .await?
                .is_empty()
        );
        cancel.cancel();
        assert!(
            service
                .search(Page::Notes, "项目".into(), cancel)
                .await?
                .is_empty()
        );
        storage.delete_note(note.version.id)?;
        assert!(
            service
                .search(Page::Notes, String::new(), CancellationToken::new())
                .await?
                .is_empty()
        );
        Ok(())
    }
}

impl SearchService {
    pub fn new(storage: Storage) -> Self {
        Self {
            apps: Arc::default(),
            commands: commands().into(),
            storage,
            file_query_slot: Arc::default(),
        }
    }

    pub async fn rescan(&self, cancel: CancellationToken) -> Result<usize> {
        let apps = self.apps.clone();
        Ok(tokio::task::spawn_blocking(move || {
            let mut entries = winycast_platform_windows::applications(|| cancel.is_cancelled());
            entries.sort_unstable_by(|a, b| a.id.cmp(&b.id));
            entries.dedup_by(|a, b| a.id == b.id);
            let count = entries.len();
            let mut index = apps
                .write()
                .map_err(|_| anyhow!("application index lock poisoned"))?;
            if !cancel.is_cancelled() {
                *index = entries.into();
            }
            Ok::<_, anyhow::Error>(count)
        })
        .await??)
    }

    pub async fn search(
        &self,
        page: Page,
        query: String,
        cancel: CancellationToken,
    ) -> Result<Vec<Row>> {
        let file_slot = if page == Page::Files {
            if query.trim().is_empty() {
                return Ok(Vec::new());
            }
            // Let the current OLE DB call return before starting another file query.
            Some(tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(Vec::new()),
                slot = self.file_query_slot.clone().lock_owned() => slot,
            })
        } else {
            None
        };
        let service = self.clone();
        let search = cancel.clone();
        let job = tokio::task::spawn_blocking(move || {
            let _file_slot = file_slot;
            service.search_blocking(page, &query, &search)
        });
        tokio::select! {
            biased;
            () = cancel.cancelled() => Ok(Vec::new()),
            result = job => result?,
        }
    }

    fn search_blocking(
        &self,
        page: Page,
        query: &str,
        cancel: &CancellationToken,
    ) -> Result<Vec<Row>> {
        if cancel.is_cancelled() || page == Page::Settings {
            return Ok(Vec::new());
        }
        if page == Page::Clipboard {
            return self.storage.clips(query, || cancel.is_cancelled());
        }
        if page == Page::ClipboardLinks {
            return self.storage.clip_links(query, || cancel.is_cancelled());
        }
        let shortcuts = if matches!(page, Page::Launcher | Page::Shortcuts) {
            self.storage.shortcuts(|| cancel.is_cancelled())?
        } else {
            Vec::new()
        };
        if page == Page::Shortcuts {
            return Ok(rank(&shortcuts, query, &self.storage.usage()?, || {
                cancel.is_cancelled()
            }));
        }
        let notes = if matches!(page, Page::Launcher | Page::Notes) {
            self.storage.note_entries(query, || cancel.is_cancelled())?
        } else {
            Vec::new()
        };
        if page == Page::Notes {
            return Ok(rank(&notes, query, &self.storage.usage()?, || {
                cancel.is_cancelled()
            }));
        }
        if page == Page::Files {
            return Ok(
                winycast_platform_windows::search_files(query, || cancel.is_cancelled())?
                    .into_iter()
                    .filter_map(|path| {
                        let title = path.file_name()?.to_string_lossy().into_owned();
                        let detail = path.to_string_lossy().into_owned();
                        Some(Row {
                            entry: Entry::display(
                                format!("file:{detail}"),
                                title,
                                detail,
                                Target::File(path),
                            ),
                            pinned: false,
                            suggested: false,
                        })
                    })
                    .collect(),
            );
        }
        let apps = self
            .apps
            .read()
            .map_err(|_| anyhow!("application index lock poisoned"))?
            .clone();
        let windows = if page == Page::Windows || !query.trim().is_empty() {
            winycast_platform_windows::running_windows(|| cancel.is_cancelled())
        } else {
            Vec::new()
        };
        let usage = self.storage.usage()?;
        let general = (page == Page::Launcher).then_some(self.commands.iter().chain(apps.iter()));
        let mut rows = rank(
            windows
                .iter()
                .chain(general.into_iter().flatten())
                .chain(&shortcuts)
                .chain(&notes),
            query,
            &usage,
            || cancel.is_cancelled(),
        );
        if page == Page::Launcher && query.trim().is_empty() {
            mark_suggestions(&mut rows, &usage, unix_time());
        }
        if page == Page::Launcher
            && !cancel.is_cancelled()
            && let Some(value) = calculate(query)
        {
            rows.insert(
                0,
                Row {
                    entry: Entry::display(
                        "calculator".into(),
                        value.clone(),
                        query.into(),
                        Target::CopyText(value),
                    ),
                    pinned: false,
                    suggested: false,
                },
            );
        }
        Ok(rows)
    }
}

fn mark_suggestions(rows: &mut [Row], usage: &HashMap<String, Usage>, now: i64) {
    let order = |row: &Row| {
        let stats = usage.get(&row.entry.id).copied().unwrap_or_default();
        Reverse((stats.weight(now), stats.last_used))
    };
    let mut candidates: Vec<_> = rows
        .iter_mut()
        .filter(|row| {
            !row.pinned
                && usage
                    .get(&row.entry.id)
                    .is_some_and(|stats| stats.count > 0)
        })
        .collect();
    candidates.sort_by_key(|row| order(row));
    for row in candidates.into_iter().take(5) {
        row.suggested = true;
    }
    rows.sort_by_key(|row| match (row.pinned, row.suggested) {
        (true, _) => (0, Reverse((0, 0))),
        (false, true) => (1, order(row)),
        (false, false) => (2, Reverse((0, 0))),
    });
}
