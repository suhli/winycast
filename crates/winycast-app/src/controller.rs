use std::{
    ffi::OsStr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Result, anyhow};
use async_channel::Sender;
use gpui::{Context, FocusHandle, Image, ImageFormat, Window};
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;
use winycast_core::{ClipboardData, Command, Page, Row, Settings, Shortcut, ShortcutKind, Target};
use winycast_platform_windows::{self as native, DesktopEvent, DesktopHost, Panel};
use winycast_providers::{SearchService, index_clipboard_images};
use winycast_storage::{Storage, unix_time};
use winycast_ui::{COMPACT_HEIGHT, CommandPalette, Intent, PANEL_HEIGHT, PANEL_WIDTH, Preview};

pub(crate) enum Event {
    Desktop(DesktopEvent),
    Ui(Intent),
    Worker(WorkerEvent),
}

pub(crate) enum WorkerEvent {
    Search(u64, Page, String, Result<Vec<Row>>),
    Indexed(u64, bool, Result<usize>),
    Preview(u64, i64, Result<Option<Preview>>),
    Settings(Settings, Result<()>),
    Refresh {
        result: Result<()>,
        message: Option<&'static str>,
    },
    Completed(Result<()>, Option<&'static str>, bool),
    ClipboardChanged(Result<ClipboardChange>),
    ClipboardCleared(Result<()>),
    ClipboardOcr(CancellationToken, Result<usize>),
    Paste(u64, FocusHandle, Result<Option<String>>),
    ShortcutLoaded(u64, Result<Shortcut>),
    ShortcutSaved(u64, Result<i64>),
    Hide(u64),
    Resize(u64, i32),
    Expire,
}

pub(crate) enum ClipboardChange {
    None,
    History,
    Image,
}

pub(crate) struct Controller {
    panel: Panel,
    host: DesktopHost,
    storage: Storage,
    settings: Settings,
    runtime: Handle,
    events: Sender<WorkerEvent>,
    index: SearchService,
    search_seq: u64,
    search_cancel: CancellationToken,
    index_seq: u64,
    index_cancel: CancellationToken,
    preview_seq: u64,
    visibility_seq: u64,
    resize_seq: u64,
    clipboard_epoch: Arc<AtomicU64>,
    clipboard_watching: bool,
    clipboard_clearing: bool,
    last_clipboard_seq: Option<u32>,
    ocr_requests: watch::Sender<Option<CancellationToken>>,
    ocr_cancel: Option<CancellationToken>,
    ocr_error_reported: bool,
    closing: bool,
    initial_notice: Option<String>,
    trace: bool,
}

impl Controller {
    pub(crate) fn new(
        panel: Panel,
        host: DesktopHost,
        storage: Storage,
        settings: Settings,
        runtime: Handle,
        events: Sender<WorkerEvent>,
        trace: bool,
    ) -> Self {
        let index = SearchService::new(storage.clone());
        let clipboard_watching = settings.clipboard_enabled;
        let (ocr_requests, ocr_jobs) = watch::channel(None);
        runtime.spawn(run_clipboard_ocr(storage.clone(), ocr_jobs, events.clone()));
        Self {
            panel,
            host,
            storage,
            settings,
            runtime,
            events,
            index,
            search_seq: 0,
            search_cancel: CancellationToken::new(),
            index_seq: 0,
            index_cancel: CancellationToken::new(),
            preview_seq: 0,
            visibility_seq: 0,
            resize_seq: 0,
            clipboard_epoch: Arc::new(AtomicU64::new(0)),
            clipboard_watching,
            clipboard_clearing: false,
            last_clipboard_seq: None,
            ocr_requests,
            ocr_cancel: None,
            ocr_error_reported: false,
            closing: false,
            initial_notice: None,
            trace,
        }
    }

    pub(crate) fn initialize(&mut self) {
        if self.settings.clipboard_enabled
            && let Err(error) = self.host.watch_clipboard(true)
        {
            self.settings.clipboard_enabled = false;
            self.clipboard_watching = false;
            self.initial_notice = Some(format!("剪贴板监听未启用：{error}"));
        }
        self.set_clipboard_ocr(self.settings.clipboard_ocr);
        self.index(false);
        if self.trace {
            eprintln!("resident: {}", !self.panel.visible());
        }
        let events = self.events.clone();
        self.runtime.spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                if events.send(WorkerEvent::Expire).await.is_err() {
                    break;
                }
            }
        });
    }

    pub(crate) fn handle(
        &mut self,
        event: Event,
        ui: &mut CommandPalette,
        window: &mut Window,
        cx: &mut Context<CommandPalette>,
    ) {
        match event {
            Event::Desktop(event) => {
                if self.trace && !matches!(event, DesktopEvent::Clipboard(_)) {
                    eprintln!("desktop: {event:?}");
                }
                match event {
                    DesktopEvent::Toggle
                        if self.panel.visible() && self.panel.active() && !self.closing =>
                    {
                        self.hide(ui, cx)
                    }
                    DesktopEvent::Show | DesktopEvent::Toggle => self.show(None, ui, window, cx),
                    DesktopEvent::Settings => self.show(Some(Page::Settings), ui, window, cx),
                    DesktopEvent::Deactivated(id)
                        if id == self.panel.id()
                            && self.panel.visible()
                            && !self.panel.active() =>
                    {
                        self.hide_now(false, ui, cx);
                    }
                    DesktopEvent::CompositionEnded(id) if id == self.panel.id() => {
                        ui.cancel_composition(cx)
                    }
                    DesktopEvent::PanelClose(id) if id == self.panel.id() => self.hide(ui, cx),
                    DesktopEvent::HotkeyUnavailable => {
                        self.initial_notice = Some("热键已被占用，请在设置中更换。".into())
                    }
                    DesktopEvent::Clipboard(sequence) => self.capture(sequence),
                    _ => {}
                }
            }
            Event::Ui(intent) => match intent {
                Intent::Hide => self.hide(ui, cx),
                Intent::Search(page, query) if self.panel.visible() && !self.closing => {
                    self.search(page, query)
                }
                Intent::Search(..) => {}
                Intent::Run { id, target } => self.run(id, target, ui, window, cx),
                Intent::Pin { id, target } => self.pin(id, target),
                Intent::Preview(id) => self.preview(id),
                Intent::Settings(settings) => self.apply_settings(settings, ui, window, cx),
                Intent::ClearClipboard => self.clear_clipboard(),
                Intent::Resize(compact) => self.resize(compact),
                Intent::CopyInput(text) => self.copy(ClipboardData::Text(text), None, false),
                Intent::PasteInput(focus) => {
                    let seq = self.visibility_seq;
                    self.background(move || WorkerEvent::Paste(seq, focus, native::read_text()));
                }
                Intent::LoadShortcut { request, id } => {
                    let storage = self.storage.clone();
                    self.background(move || {
                        WorkerEvent::ShortcutLoaded(
                            request,
                            storage.shortcut(id).and_then(|shortcut| {
                                shortcut.ok_or_else(|| anyhow!("常用项已移除"))
                            }),
                        )
                    });
                }
                Intent::SaveShortcut { request, id, draft } => {
                    let storage = self.storage.clone();
                    self.background(move || {
                        WorkerEvent::ShortcutSaved(request, storage.save_shortcut(id, draft))
                    });
                }
                Intent::DeleteShortcut(id) => {
                    let storage = self.storage.clone();
                    self.background(move || WorkerEvent::Refresh {
                        result: storage.delete_shortcut(id),
                        message: Some("已删除常用项"),
                    });
                }
            },
            Event::Worker(event) => match event {
                WorkerEvent::Search(seq, page, query, result) if seq == self.search_seq => {
                    match result {
                        Ok(rows) => {
                            if self.trace {
                                eprintln!("search: {page:?}, {} results", rows.len());
                            }
                            ui.results(page, &query, Some(rows), cx);
                        }
                        Err(error) => {
                            if ui.results(page, &query, None, cx) {
                                ui.toast(format!("搜索失败：{error}"), window, cx);
                            }
                        }
                    }
                }
                WorkerEvent::Indexed(seq, announce, result) if seq == self.index_seq => {
                    match result {
                        Ok(count) => {
                            if self.panel.visible() {
                                ui.refresh(cx);
                            }
                            if announce {
                                ui.toast(format!("已索引 {count} 个应用"), window, cx);
                            }
                            if self.trace {
                                eprintln!("indexed: {count} apps");
                            }
                        }
                        Err(error) => ui.toast(format!("应用索引失败：{error}"), window, cx),
                    }
                }
                WorkerEvent::Preview(seq, id, result) if seq == self.preview_seq => {
                    ui.show_preview(id, result.unwrap_or(None), cx);
                }
                WorkerEvent::Settings(requested, result) => {
                    if let Err(error) = result {
                        let _ = self.host.set_hotkey(self.settings.hotkey);
                        self.clipboard_watching = self
                            .host
                            .watch_clipboard(self.settings.clipboard_enabled)
                            .is_ok()
                            && self.settings.clipboard_enabled;
                        ui.apply_settings(self.settings.clone(), cx);
                        ui.toast(format!("无法保存设置：{error}"), window, cx);
                    } else {
                        let ocr_changed = self.settings.clipboard_ocr != requested.clipboard_ocr;
                        self.settings = requested;
                        self.clipboard_watching = self.settings.clipboard_enabled;
                        if self.trace {
                            eprintln!("settings: saved");
                        }
                        ui.apply_settings(self.settings.clone(), cx);
                        if ocr_changed && self.panel.visible() {
                            ui.refresh_clipboard(cx);
                        }
                        self.expire();
                    }
                    self.set_clipboard_ocr(self.settings.clipboard_ocr);
                }
                WorkerEvent::Refresh { result, message } => match result {
                    Ok(()) => {
                        if self.panel.visible() {
                            ui.refresh(cx);
                        }
                        if let Some(message) = message {
                            ui.toast(message, window, cx);
                        }
                    }
                    Err(error) => ui.toast(format!("操作失败：{error}"), window, cx),
                },
                WorkerEvent::Completed(result, message, hide) => match result {
                    Ok(()) => {
                        if self.trace {
                            eprintln!("action: succeeded");
                        }
                        if hide && self.panel.active() {
                            self.hide(ui, cx);
                        } else if let Some(message) = message {
                            ui.toast(message, window, cx);
                        }
                    }
                    Err(error) => ui.toast(format!("操作失败：{error}"), window, cx),
                },
                WorkerEvent::ClipboardChanged(result) => match result {
                    Ok(change @ (ClipboardChange::History | ClipboardChange::Image)) => {
                        if matches!(change, ClipboardChange::Image) {
                            self.queue_clipboard_ocr();
                        }
                        if self.panel.visible() {
                            ui.refresh_clipboard(cx);
                        }
                    }
                    Err(error) if self.trace => eprintln!("clipboard update failed: {error}"),
                    _ => {}
                },
                WorkerEvent::ClipboardOcr(cancel, result) if !cancel.is_cancelled() => match result
                {
                    Ok(count) if count > 0 => {
                        self.ocr_error_reported = false;
                        if self.trace {
                            eprintln!("clipboard OCR: indexed {count} images");
                        }
                        if self.panel.visible() {
                            ui.refresh_clipboard(cx);
                        }
                    }
                    Err(error) if !self.ocr_error_reported => {
                        self.ocr_error_reported = true;
                        let message = format!("剪贴板图片文字识别失败：{error}");
                        if self.panel.visible() {
                            ui.toast(message, window, cx);
                        } else if self.initial_notice.is_none() {
                            self.initial_notice = Some(message);
                        }
                    }
                    _ => {}
                },
                WorkerEvent::ClipboardCleared(result) => {
                    self.clipboard_clearing = false;
                    match result {
                        Ok(()) => {
                            ui.refresh(cx);
                            ui.toast("剪贴板历史已清空", window, cx);
                        }
                        Err(error) => ui.toast(format!("清空失败：{error}"), window, cx),
                    }
                }
                WorkerEvent::Paste(seq, focus, result)
                    if seq == self.visibility_seq && self.panel.active() =>
                {
                    match result {
                        Ok(Some(text)) => ui.paste_input(&focus, text, window, cx),
                        Ok(None) => {}
                        Err(error) => ui.toast(format!("无法读取剪贴板：{error}"), window, cx),
                    }
                }
                WorkerEvent::ShortcutLoaded(request, result) if self.panel.active() => {
                    ui.shortcut_loaded(
                        request,
                        result.map_err(|error| error.to_string()),
                        window,
                        cx,
                    );
                }
                WorkerEvent::ShortcutSaved(request, result) => {
                    ui.shortcut_saved(
                        request,
                        result.map_err(|error| error.to_string()),
                        window,
                        cx,
                    );
                }
                WorkerEvent::Hide(seq) if seq == self.visibility_seq && self.closing => {
                    self.hide_now(true, ui, cx)
                }
                WorkerEvent::Resize(seq, height)
                    if seq == self.resize_seq && self.panel.visible() =>
                {
                    if let Err(error) = self.panel.resize(height) {
                        ui.toast(format!("调整面板失败：{error}"), window, cx);
                    }
                }
                WorkerEvent::Expire => self.expire(),
                _ => {}
            },
        }
    }

    fn background(&self, work: impl FnOnce() -> WorkerEvent + Send + 'static) {
        let events = self.events.clone();
        self.runtime.spawn_blocking(move || {
            let _ = events.try_send(work());
        });
    }

    fn show(
        &mut self,
        page: Option<Page>,
        ui: &mut CommandPalette,
        window: &mut Window,
        cx: &mut Context<CommandPalette>,
    ) {
        self.visibility_seq = self.visibility_seq.wrapping_add(1);
        self.resize_seq = self.resize_seq.wrapping_add(1);
        self.closing = false;
        let start = Instant::now();
        let height = if ui.compact() && page != Some(Page::Settings) {
            COMPACT_HEIGHT
        } else {
            PANEL_HEIGHT
        };
        match self
            .panel
            .show(PANEL_WIDTH, height, self.settings.allow_fullscreen)
        {
            Ok(true) => {
                ui.show(page, window, cx);
                if let Some(message) = self.initial_notice.take() {
                    ui.toast(message, window, cx);
                }
                if self.trace {
                    eprintln!(
                        "visible: {}, active: {}, activation: {} ms",
                        self.panel.visible(),
                        self.panel.active(),
                        start.elapsed().as_millis()
                    );
                }
            }
            Ok(false) => {
                if self.trace {
                    eprintln!("show: suppressed by fullscreen policy");
                }
            }
            Err(error) => eprintln!("show: {error:#}"),
        }
    }

    fn hide(&mut self, ui: &mut CommandPalette, cx: &mut Context<CommandPalette>) {
        if !self.panel.visible() || self.closing {
            return;
        }
        if !self.settings.animations {
            self.hide_now(true, ui, cx);
            return;
        }
        self.closing = true;
        self.visibility_seq = self.visibility_seq.wrapping_add(1);
        ui.begin_hide(cx);
        let seq = self.visibility_seq;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(Duration::from_millis(105)).await;
            let _ = events.send(WorkerEvent::Hide(seq)).await;
        });
    }

    fn hide_now(
        &mut self,
        restore: bool,
        ui: &mut CommandPalette,
        cx: &mut Context<CommandPalette>,
    ) {
        self.closing = false;
        self.visibility_seq = self.visibility_seq.wrapping_add(1);
        self.resize_seq = self.resize_seq.wrapping_add(1);
        self.search_seq = self.search_seq.wrapping_add(1);
        self.preview_seq = self.preview_seq.wrapping_add(1);
        self.search_cancel.cancel();
        self.panel.hide(restore);
        ui.hidden(cx);
        if self.trace {
            eprintln!(
                "visible: {}, active: {}, restore: {restore}",
                self.panel.visible(),
                self.panel.active()
            );
        }
    }

    fn resize(&mut self, compact: bool) {
        if !self.panel.visible() {
            return;
        }
        self.resize_seq = self.resize_seq.wrapping_add(1);
        let goal = if compact {
            COMPACT_HEIGHT
        } else {
            PANEL_HEIGHT
        };
        let current = self.panel.height();
        if !self.settings.animations || current == 0 {
            let _ = self.panel.resize(goal);
            return;
        }
        let seq = self.resize_seq;
        let events = self.events.clone();
        self.runtime.spawn(async move {
            let start = tokio::time::Instant::now();
            for step in 1..=7u64 {
                tokio::time::sleep_until(start + Duration::from_millis(step * 16)).await;
                let progress = 1. - (1. - step as f32 / 7.).powi(3);
                let height = current + ((goal - current) as f32 * progress).round() as i32;
                if events.send(WorkerEvent::Resize(seq, height)).await.is_err() {
                    break;
                }
            }
        });
    }

    fn search(&mut self, page: Page, query: String) {
        self.search_cancel.cancel();
        self.search_cancel = CancellationToken::new();
        self.search_seq = self.search_seq.wrapping_add(1);
        let seq = self.search_seq;
        let cancel = self.search_cancel.clone();
        let events = self.events.clone();
        let index = self.index.clone();
        self.runtime.spawn(async move {
            if !query.is_empty() {
                let delay = if page == Page::Files { 120 } else { 19 };
                tokio::select! {
                    () = cancel.cancelled() => return,
                    () = tokio::time::sleep(Duration::from_millis(delay)) => {}
                }
            }
            let result = index.search(page, query.clone(), cancel.clone()).await;
            if !cancel.is_cancelled() {
                let _ = events
                    .send(WorkerEvent::Search(seq, page, query, result))
                    .await;
            }
        });
    }

    fn index(&mut self, announce: bool) {
        self.index_cancel.cancel();
        self.index_cancel = CancellationToken::new();
        self.index_seq = self.index_seq.wrapping_add(1);
        let seq = self.index_seq;
        let events = self.events.clone();
        let cancel = self.index_cancel.clone();
        let search = self.index.clone();
        self.runtime.spawn(async move {
            let result = search.rescan(cancel).await;
            let _ = events
                .send(WorkerEvent::Indexed(seq, announce, result))
                .await;
        });
    }

    fn preview(&mut self, id: i64) {
        self.preview_seq = self.preview_seq.wrapping_add(1);
        let seq = self.preview_seq;
        let storage = self.storage.clone();
        self.background(move || {
            WorkerEvent::Preview(
                seq,
                id,
                (|| {
                    let Some(ClipboardData::Png(png)) = storage.clip(id)? else {
                        return Ok(None);
                    };
                    let bytes = native::thumbnail(&png)?;
                    Ok(Some(Arc::new(Image::from_bytes(ImageFormat::Png, bytes))))
                })(),
            )
        });
    }

    fn capture(&mut self, sequence: u32) {
        if !self.clipboard_watching
            || self.clipboard_clearing
            || self.last_clipboard_seq == Some(sequence)
        {
            return;
        }
        self.last_clipboard_seq = Some(sequence);
        let epoch = self.clipboard_epoch.clone();
        let accepted = epoch.load(Ordering::Acquire);
        let valid = move || epoch.load(Ordering::Acquire) == accepted;
        let storage = self.storage.clone();
        let days = self.settings.clipboard_days;
        self.background(move || {
            WorkerEvent::ClipboardChanged((|| {
                if !valid() {
                    return Ok(ClipboardChange::None);
                }
                if let Some(data) = native::capture(sequence)? {
                    let change = if matches!(&data, ClipboardData::Png(_)) {
                        ClipboardChange::Image
                    } else {
                        ClipboardChange::History
                    };
                    storage.add_clip_if(data, unix_time(), days, &valid)?;
                    if valid() {
                        return Ok(change);
                    }
                }
                Ok(ClipboardChange::None)
            })())
        });
    }

    fn set_clipboard_ocr(&mut self, enabled: bool) {
        if enabled && self.ocr_cancel.is_none() {
            self.ocr_cancel = Some(CancellationToken::new());
            self.ocr_error_reported = false;
            self.queue_clipboard_ocr();
        } else if !enabled && let Some(cancel) = self.ocr_cancel.take() {
            cancel.cancel();
            self.ocr_requests.send_replace(None);
        }
    }

    fn queue_clipboard_ocr(&self) {
        if let Some(cancel) = &self.ocr_cancel {
            self.ocr_requests.send_replace(Some(cancel.clone()));
        }
    }

    fn clear_clipboard(&mut self) {
        if self.clipboard_clearing {
            return;
        }
        self.clipboard_clearing = true;
        self.clipboard_epoch.fetch_add(1, Ordering::AcqRel);
        let storage = self.storage.clone();
        self.background(move || WorkerEvent::ClipboardCleared(storage.clear_clips()));
    }

    fn expire(&self) {
        let storage = self.storage.clone();
        let days = self.settings.clipboard_days;
        self.background(move || {
            WorkerEvent::ClipboardChanged(storage.expire_clips(unix_time(), days).map(|count| {
                if count > 0 {
                    ClipboardChange::History
                } else {
                    ClipboardChange::None
                }
            }))
        });
    }

    fn copy(&self, data: ClipboardData, message: Option<&'static str>, hide: bool) {
        let writer = self.host.clipboard_writer();
        self.background(move || WorkerEvent::Completed(writer.write(data), message, hide));
    }

    fn run(
        &mut self,
        id: String,
        target: Target,
        ui: &mut CommandPalette,
        window: &mut Window,
        cx: &mut Context<CommandPalette>,
    ) {
        if !self.panel.active() || self.closing {
            return;
        }
        match target {
            Target::Command(Command::Page(page)) => {
                ui.open_page(page, window, cx);
                self.record_use(id);
            }
            Target::Command(Command::Rescan) => {
                self.index(true);
                self.record_use(id);
                ui.toast("正在扫描应用…", window, cx);
            }
            Target::Window(target) => match native::activate(target) {
                Ok(()) => {
                    self.hide_now(false, ui, cx);
                    if self.trace {
                        eprintln!("window switch: succeeded");
                    }
                }
                Err(error) => ui.toast(format!("切换失败：{error}"), window, cx),
            },
            Target::Clip(id) => {
                let storage = self.storage.clone();
                let writer = self.host.clipboard_writer();
                self.background(move || {
                    WorkerEvent::Completed(
                        (|| {
                            let data = storage
                                .clip(id)?
                                .ok_or_else(|| anyhow!("剪贴板条目已移除"))?;
                            writer.write(data)
                        })(),
                        Some("已复制到剪贴板"),
                        true,
                    )
                });
            }
            Target::CopyText(text) => {
                self.copy(ClipboardData::Text(text), Some("计算结果已复制"), false)
            }
            Target::Shortcut(shortcut, kind) => {
                let storage = self.storage.clone();
                let writer = self.host.clipboard_writer();
                self.background(move || {
                    let result = (|| {
                        let draft = storage
                            .shortcut(shortcut)?
                            .ok_or_else(|| anyhow!("常用项已移除"))?
                            .draft;
                        anyhow::ensure!(draft.kind == kind, "常用项类型已更改，请重新选择");
                        let draft = draft.validated().map_err(|message| anyhow!(message))?;
                        match kind {
                            ShortcutKind::Snippet => {
                                writer.write(ClipboardData::Text(draft.body))?
                            }
                            ShortcutKind::Quicklink => native::launch(OsStr::new(&draft.body))?,
                        }
                        let _ = storage.record_shortcut_use(shortcut);
                        Ok(())
                    })();
                    WorkerEvent::Completed(
                        result,
                        (kind == ShortcutKind::Snippet).then_some("已复制到剪贴板"),
                        true,
                    )
                });
            }
            Target::File(path) => self.background(move || {
                WorkerEvent::Completed(native::launch(path.as_os_str()), None, true)
            }),
            Target::App(_) | Target::Command(Command::System(_)) => {
                let storage = self.storage.clone();
                self.background(move || {
                    WorkerEvent::Completed(
                        (|| {
                            match &target {
                                Target::App(location) => native::launch(location)?,
                                Target::Command(Command::System(command)) => {
                                    native::run_system(*command)?
                                }
                                _ => unreachable!(),
                            }
                            let _ = storage.used(&id);
                            Ok(())
                        })(),
                        None,
                        true,
                    )
                });
            }
        }
    }

    fn record_use(&self, id: String) {
        let storage = self.storage.clone();
        self.background(move || WorkerEvent::Refresh {
            result: storage.used(&id),
            message: None,
        });
    }

    fn pin(&self, id: String, target: Target) {
        let storage = self.storage.clone();
        self.background(move || WorkerEvent::Refresh {
            result: match target {
                Target::Clip(id) => storage.toggle_clip_pin(id),
                Target::Shortcut(id, _) => storage.toggle_shortcut_favorite(id),
                _ => storage.toggle_favorite(&id),
            },
            message: None,
        });
    }

    fn apply_settings(
        &mut self,
        requested: Settings,
        ui: &mut CommandPalette,
        window: &mut Window,
        cx: &mut Context<CommandPalette>,
    ) {
        if let Err(error) = self.host.set_hotkey(requested.hotkey) {
            ui.apply_settings(self.settings.clone(), cx);
            ui.toast(format!("该热键不可用：{error}"), window, cx);
            return;
        }
        if let Err(error) = self.host.watch_clipboard(requested.clipboard_enabled) {
            let _ = self.host.set_hotkey(self.settings.hotkey);
            ui.apply_settings(self.settings.clone(), cx);
            ui.toast(format!("无法更新剪贴板监听：{error}"), window, cx);
            return;
        }
        if requested.clipboard_enabled != self.settings.clipboard_enabled
            || requested.clipboard_days != self.settings.clipboard_days
        {
            self.clipboard_epoch.fetch_add(1, Ordering::AcqRel);
            self.last_clipboard_seq = None;
        }
        self.clipboard_watching &= requested.clipboard_enabled;
        if !requested.clipboard_ocr {
            self.set_clipboard_ocr(false);
        }
        let storage = self.storage.clone();
        let old_startup = self.settings.startup;
        self.background(move || {
            let result = (|| {
                if old_startup != requested.startup {
                    native::set_startup(requested.startup)?;
                }
                if let Err(error) = storage.save_settings(&requested) {
                    if old_startup != requested.startup {
                        let _ = native::set_startup(old_startup);
                    }
                    return Err(error);
                }
                Ok(())
            })();
            WorkerEvent::Settings(requested, result)
        });
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.search_cancel.cancel();
        self.index_cancel.cancel();
        self.set_clipboard_ocr(false);
        self.clipboard_epoch.fetch_add(1, Ordering::AcqRel);
    }
}

async fn run_clipboard_ocr(
    storage: Storage,
    mut requests: watch::Receiver<Option<CancellationToken>>,
    events: Sender<WorkerEvent>,
) {
    while requests.changed().await.is_ok() {
        let request = requests.borrow_and_update().clone();
        let Some(cancel) = request.filter(|cancel| !cancel.is_cancelled()) else {
            continue;
        };
        let storage = storage.clone();
        let worker_cancel = cancel.clone();
        let result =
            tokio::task::spawn_blocking(move || index_clipboard_images(&storage, &worker_cancel))
                .await
                .map_err(anyhow::Error::from)
                .and_then(|result| result);
        if !cancel.is_cancelled()
            && events
                .send(WorkerEvent::ClipboardOcr(cancel, result))
                .await
                .is_err()
        {
            break;
        }
    }
}
