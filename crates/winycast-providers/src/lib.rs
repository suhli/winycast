mod clipboard_ocr;

use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use winycast_core::{Entry, Page, Row, Target, calculate, commands, rank};
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

    use winycast_core::{ShortcutDraft, ShortcutKind};

    use super::*;

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
                .chain(&shortcuts),
            query,
            &usage,
            || cancel.is_cancelled(),
        );
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
                },
            );
        }
        Ok(rows)
    }
}
