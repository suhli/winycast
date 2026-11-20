use std::sync::{Arc, RwLock};

use anyhow::{Result, anyhow};
use tokio_util::sync::CancellationToken;
use winycast_core::{Entry, Page, Row, Target, calculate, commands, rank};
use winycast_storage::Storage;

#[derive(Clone)]
pub struct SearchService {
    apps: Arc<RwLock<Arc<[Entry]>>>,
    commands: Arc<[Entry]>,
    storage: Storage,
}

impl SearchService {
    pub fn new(storage: Storage) -> Self {
        Self {
            apps: Arc::default(),
            commands: commands().into(),
            storage,
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
        let service = self.clone();
        let search = cancel.clone();
        let job =
            tokio::task::spawn_blocking(move || service.search_blocking(page, &query, &search));
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
            windows.iter().chain(general.into_iter().flatten()),
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
