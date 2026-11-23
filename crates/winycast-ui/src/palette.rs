use std::{sync::Arc, time::Duration};

use async_channel::Sender;
use gpui::prelude::FluentBuilder;
use gpui::*;
use winycast_core::{
    Command, Hotkey, Page, Row, Settings, Shortcut, ShortcutKind, SystemCommand, Target, Theme,
};

use crate::{
    Intent, Preview,
    input::{QueryChanged, SearchInput},
    settings::SettingsWindow,
    shortcuts::ShortcutEditor,
};

const SEARCH_TABS: [(&str, &str, Page); 5] = [
    ("launcher-tab", "应用", Page::Launcher),
    ("windows-tab", "窗口", Page::Windows),
    ("clipboard-tab", "剪贴板", Page::Clipboard),
    ("files-tab", "文件", Page::Files),
    ("shortcuts-tab", "常用", Page::Shortcuts),
];

#[derive(Clone, Copy)]
pub(crate) struct Colors {
    pub surface: Hsla,
    pub card: Hsla,
    pub text: Hsla,
    pub muted: Hsla,
    pub accent: Hsla,
    pub line: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
}

impl Colors {
    fn new(settings: &Settings, appearance: WindowAppearance) -> Self {
        let dark = match settings.theme {
            Theme::System => matches!(
                appearance,
                WindowAppearance::Dark | WindowAppearance::VibrantDark
            ),
            Theme::Dark => true,
            Theme::Light => false,
        };
        let opacity = f32::from(settings.opacity.clamp(65, 100)) / 100.;
        let accent: Hsla = rgb(settings.accent & 0xff_ffff).into();
        if dark {
            Self {
                surface: Hsla::from(rgb(0x181f26)).opacity(opacity),
                card: rgb(0x242e37).into(),
                text: rgb(0xeff5f6).into(),
                muted: rgb(0xa4b4bd).into(),
                accent: hsla(accent.h, accent.s.min(0.72), 0.73, 1.),
                line: rgba(0xb7c9e42d).into(),
                hover: rgba(0xa2bcc41a).into(),
                selected: hsla(accent.h, 0.45, 0.47, 0.23),
            }
        } else {
            Self {
                surface: Hsla::from(rgb(0xf7fbf9)).opacity(opacity),
                card: rgb(0xe9f1ee).into(),
                text: rgb(0x172b2a).into(),
                muted: rgb(0x506663).into(),
                accent,
                line: rgba(0x4067622a).into(),
                hover: rgba(0x577a7216).into(),
                selected: hsla(accent.h, 0.6, 0.5, 0.15),
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Overlay {
    Actions,
    ConfirmSystem,
    ConfirmClear,
    ConfirmDeleteShortcut(i64),
}

#[derive(Clone, Copy)]
enum ResultAction {
    Run,
    Pin,
    EditShortcut,
    DeleteShortcut,
}

fn result_actions(target: &Target) -> &'static [ResultAction] {
    use ResultAction::*;
    match target {
        Target::Shortcut(..) => &[Run, EditShortcut, Pin, DeleteShortcut],
        _ if can_pin(target) => &[Run, Pin],
        _ => &[Run],
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Setting {
    Theme,
    Accent,
    Opacity,
    Animations,
    Hotkey,
    Fullscreen,
    Startup,
    Clipboard,
    Ocr,
    Retention,
    Clear,
}

struct ResultList {
    rows: Arc<[Row]>,
    index: usize,
    scroll: UniformListScrollHandle,
    pending: bool,
    failed: bool,
}

pub struct CommandPalette {
    input: Entity<SearchInput>,
    input_focused: FocusHandle,
    controls: FocusHandle,
    intents: Sender<Intent>,
    settings: Settings,
    settings_index: usize,
    settings_scroll: ScrollHandle,
    settings_pending: bool,
    capturing_hotkey: bool,
    page: Page,
    query: String,
    list: ResultList,
    overlay: Option<Overlay>,
    menu_index: usize,
    preview: Option<(i64, Preview)>,
    shortcut_request: u64,
    loading_shortcut: Option<u64>,
    shortcut_editor: Option<ShortcutEditor>,
    compact: bool,
    opening: usize,
    closing: bool,
    scene: usize,
    toast: Option<(usize, SharedString)>,
    toast_id: usize,
    _query: Subscription,
}

impl CommandPalette {
    pub fn new(intents: Sender<Intent>, settings: Settings, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| SearchInput::new(intents.clone(), cx));
        let input_focused = input.read(cx).focus.clone();
        let controls = cx.focus_handle();
        let _query = cx.subscribe(&input, |this: &mut Self, _, event: &QueryChanged, cx| {
            this.query = event.0.clone();
            this.list.index = 0;
            this.overlay = None;
            this.request_search();
            cx.notify();
        });
        Self {
            input,
            input_focused,
            controls,
            intents,
            settings,
            settings_index: 0,
            settings_scroll: ScrollHandle::new(),
            settings_pending: false,
            capturing_hotkey: false,
            page: Page::Launcher,
            query: String::new(),
            list: ResultList {
                rows: Arc::from([]),
                index: 0,
                scroll: Default::default(),
                pending: true,
                failed: false,
            },
            overlay: None,
            menu_index: 0,
            preview: None,
            shortcut_request: 0,
            loading_shortcut: None,
            shortcut_editor: None,
            compact: false,
            opening: 0,
            closing: false,
            scene: 0,
            toast: None,
            toast_id: 0,
            _query,
        }
    }

    pub fn show(&mut self, page: Option<Page>, window: &mut Window, cx: &mut Context<Self>) {
        self.closing = false;
        self.opening = self.opening.wrapping_add(1);
        if let Some(page) = page {
            self.change_page(page, window, cx);
        }
        self.focus(window, cx);
        self.request_search();
        cx.notify();
    }

    fn focus(&self, window: &mut Window, cx: &App) {
        if let Some(editor) = &self.shortcut_editor {
            editor.title.read(cx).focus.focus(window);
        } else if self.page == Page::Settings {
            self.controls.focus(window);
        } else {
            self.input_focused.focus(window);
        }
    }

    pub fn begin_hide(&mut self, cx: &mut Context<Self>) {
        self.closing = true;
        self.opening = self.opening.wrapping_add(1);
        cx.notify();
    }

    pub fn hidden(&mut self, cx: &mut Context<Self>) {
        self.closing = false;
        self.overlay = None;
        self.capturing_hotkey = false;
        self.loading_shortcut = None;
        self.cancel_composition(cx);
        cx.notify();
    }

    pub fn compact(&self) -> bool {
        self.compact
    }

    pub fn open_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        self.change_page(page, window, cx);
    }

    pub fn cancel_composition(&mut self, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.cancel_composition(cx));
        if let Some(editor) = &self.shortcut_editor {
            for field in editor.fields() {
                field.update(cx, |field, cx| field.cancel_composition(cx));
            }
        }
    }

    pub fn paste_input(
        &mut self,
        focus: &FocusHandle,
        text: String,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        if self.page == Page::Settings || self.closing || !focus.is_focused(window) {
            return;
        }
        let field = match &self.shortcut_editor {
            Some(editor) => editor
                .fields()
                .into_iter()
                .find(|field| &field.read(cx).focus == focus),
            None => (&self.input_focused == focus).then_some(&self.input),
        };
        if let Some(field) = field {
            field.update(cx, |input, cx| input.paste_text(&text, cx));
        }
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        self.request_search();
        cx.notify();
    }

    pub fn refresh_clipboard(&mut self, cx: &mut Context<Self>) {
        if self.page == Page::Clipboard {
            self.refresh(cx);
        }
    }

    pub fn results(
        &mut self,
        page: Page,
        query: &str,
        rows: Option<Vec<Row>>,
        cx: &mut Context<Self>,
    ) -> bool {
        if page != self.page || query != self.query {
            return false;
        }
        self.list.failed = rows.is_none();
        let rows = rows.unwrap_or_default();
        let old_id = self
            .list
            .rows
            .get(self.list.index)
            .map(|row| row.entry.id.as_str());
        self.list.index = old_id
            .and_then(|id| rows.iter().position(|row| row.entry.id == id))
            .unwrap_or(0);
        self.list.pending = false;
        self.list.rows = rows.into();
        self.request_preview();
        cx.notify();
        true
    }

    pub fn apply_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        self.settings_pending = false;
        self.settings = settings;
        cx.notify();
    }

    pub fn show_preview(&mut self, id: i64, image: Option<Preview>, cx: &mut Context<Self>) {
        if self.page == Page::Clipboard
            && self
                .selected()
                .is_some_and(|row| row.entry.target == Target::Clip(id))
        {
            self.preview = image.map(|image| (id, image));
            cx.notify();
        }
    }

    pub fn toast(
        &mut self,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let id = self.toast_id.wrapping_add(1);
        self.toast_id = id;
        self.toast = Some((id, message.into()));
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(3000))
                .await;
            let _ = this.update_in(cx, |palette, _, cx| {
                if palette
                    .toast
                    .as_ref()
                    .is_some_and(|(current, _)| *current == id)
                {
                    palette.toast = None;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    fn send(&self, intent: Intent) {
        let _ = self.intents.try_send(intent);
    }

    fn request_search(&mut self) {
        if self.page == Page::Settings || self.shortcut_editor.is_some() {
            return;
        }
        self.list.pending = true;
        self.list.failed = false;
        self.send(Intent::Search(self.page, self.query.clone()));
    }

    fn request_preview(&mut self) {
        if let Some(Target::Clip(id)) = self.selected().map(|row| &row.entry.target)
            && self
                .selected()
                .is_some_and(|row| row.entry.detail == "PNG · 剪贴板图片")
        {
            if self
                .preview
                .as_ref()
                .is_some_and(|(current, _)| current == id)
            {
                return;
            }
            self.send(Intent::Preview(*id));
        }
        self.preview = None;
    }

    fn change_page(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if page == self.page {
            return;
        }
        self.cancel_composition(cx);
        self.shortcut_editor = None;
        self.loading_shortcut = None;
        self.page = page;
        self.query.clear();
        self.overlay = None;
        self.preview = None;
        self.capturing_hotkey = false;
        self.scene = self.scene.wrapping_add(1);
        self.list.index = 0;
        self.list.rows = Arc::from([]);
        self.list.scroll = Default::default();
        self.input
            .update(cx, |input, cx| input.set_text(String::new(), cx));
        if matches!(page, Page::Settings | Page::Files | Page::Shortcuts) && self.compact {
            self.set_compact(false, cx);
        }
        self.focus(window, cx);
        self.request_search();
        cx.notify();
    }

    fn set_compact(&mut self, compact: bool, cx: &mut Context<Self>) {
        if self.compact != compact {
            self.compact = compact;
            self.overlay = None;
            self.scene = self.scene.wrapping_add(1);
            self.send(Intent::Resize(compact));
            cx.notify();
        }
    }

    fn selected(&self) -> Option<&Row> {
        self.list.rows.get(self.list.index)
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.list.rows.len() || self.list.pending || self.overlay.is_some() {
            return;
        }
        if index != self.list.index {
            self.list.index = index;
            self.request_preview();
        }
        self.list
            .scroll
            .scroll_to_item(index, ScrollStrategy::Center);
        cx.notify();
    }

    fn run_selected(&mut self, confirmed: bool, cx: &mut Context<Self>) {
        if self.list.pending {
            return;
        }
        let Some(row) = self.selected() else {
            return;
        };
        if !confirmed
            && matches!(
                row.entry.target,
                Target::Command(Command::System(SystemCommand::Lock | SystemCommand::Sleep))
            )
        {
            self.overlay = Some(Overlay::ConfirmSystem);
            cx.notify();
            return;
        }
        self.send(Intent::Run {
            id: row.entry.id.clone(),
            target: row.entry.target.clone(),
        });
        self.overlay = None;
        cx.notify();
    }

    fn toggle_pin(&mut self, cx: &mut Context<Self>) {
        if self.list.pending {
            return;
        }
        if let Some(row) = self.selected().filter(|row| can_pin(&row.entry.target)) {
            self.send(Intent::Pin {
                id: row.entry.id.clone(),
                target: row.entry.target.clone(),
            });
            self.overlay = None;
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.shortcut_editor.is_some() {
            self.shortcut_editor_key(event, window, cx);
            return;
        }
        if self.input.read(cx).composing() || self.closing {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if key == "enter" && event.is_held {
            cx.stop_propagation();
            return;
        }
        if self.capturing_hotkey {
            if key == "escape" {
                self.capturing_hotkey = false;
            } else if let Some(hotkey) = Hotkey::from_key(
                key,
                modifiers.control,
                modifiers.alt,
                modifiers.shift,
                modifiers.platform,
            ) {
                let mut settings = self.settings.clone();
                settings.hotkey = hotkey;
                self.capturing_hotkey = false;
                self.save_settings(settings, cx);
            } else {
                return;
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if let Some(overlay) = self.overlay {
            match key {
                "escape" => self.overlay = None,
                "up" | "down" if overlay == Overlay::Actions => {
                    if let Some(row) = self.selected() {
                        let count = result_actions(&row.entry.target).len();
                        let offset = if key == "down" { 1 } else { count - 1 };
                        self.menu_index = (self.menu_index + offset) % count;
                    }
                }
                "enter" => self.apply_overlay(overlay, self.menu_index, cx),
                _ => return,
            }
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match key {
            "escape" if self.page != Page::Launcher => self.change_page(Page::Launcher, window, cx),
            "escape" => self.send(Intent::Hide),
            "," if modifiers.control => self.change_page(Page::Settings, window, cx),
            "f" if modifiers.control => self.change_page(Page::Files, window, cx),
            "n" if modifiers.control && self.page == Page::Shortcuts => {
                self.new_shortcut(window, cx);
            }
            "up" | "down" if self.page == Page::Settings => {
                self.settings_index = if key == "down" {
                    (self.settings_index + 1).min(SettingsWindow::ITEMS.len() - 1)
                } else {
                    self.settings_index.saturating_sub(1)
                };
                self.settings_scroll.scroll_to_item(self.settings_index);
            }
            "left" | "right" | "enter" if self.page == Page::Settings => {
                self.edit_setting(self.settings_index, if key == "left" { -1 } else { 1 }, cx);
            }
            "tab" if self.page != Page::Settings => {
                let index = SEARCH_TABS
                    .iter()
                    .position(|(_, _, page)| *page == self.page)
                    .unwrap_or(0);
                let offset = if modifiers.shift {
                    SEARCH_TABS.len() - 1
                } else {
                    1
                };
                self.change_page(
                    SEARCH_TABS[(index + offset) % SEARCH_TABS.len()].2,
                    window,
                    cx,
                );
            }
            "space" if modifiers.control && self.page != Page::Settings => {
                self.set_compact(!self.compact, cx)
            }
            "down" if self.compact => self.set_compact(false, cx),
            "up" if self.page != Page::Settings => {
                self.select(self.list.index.saturating_sub(1), cx)
            }
            "down" if self.page != Page::Settings => self.select(
                (self.list.index + 1).min(self.list.rows.len().saturating_sub(1)),
                cx,
            ),
            "pageup" if self.page != Page::Settings => {
                self.select(self.list.index.saturating_sub(6), cx)
            }
            "pagedown" if self.page != Page::Settings => self.select(
                (self.list.index + 6).min(self.list.rows.len().saturating_sub(1)),
                cx,
            ),
            "enter" if self.page != Page::Settings => self.run_selected(false, cx),
            "k" if modifiers.control && self.page != Page::Settings => self.open_actions(cx),
            "d" if modifiers.control && self.page != Page::Settings => self.toggle_pin(cx),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn shortcut_editor_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = &self.shortcut_editor else {
            return;
        };
        if self.closing
            || editor.saving()
            || editor
                .fields()
                .iter()
                .any(|field| field.read(cx).composing())
        {
            return;
        }
        let key = event.keystroke.key.as_str();
        let mods = event.keystroke.modifiers;
        match key {
            "escape" => self.close_shortcut_editor(window, cx),
            "enter" if mods.control && !event.is_held => self.save_shortcut(cx),
            "tab" if mods.control => {
                let next = match editor.kind {
                    ShortcutKind::Snippet => ShortcutKind::Quicklink,
                    ShortcutKind::Quicklink => ShortcutKind::Snippet,
                };
                self.set_shortcut_kind(next, cx);
            }
            "tab" => {
                if editor.title.read(cx).focus.is_focused(window) {
                    editor.body.read(cx).focus.focus(window);
                } else {
                    editor.title.read(cx).focus.focus(window);
                }
            }
            "enter" if !event.is_held && editor.title.read(cx).focus.is_focused(window) => {
                editor.body.read(cx).focus.focus(window);
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    fn new_shortcut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.shortcut_request = self.shortcut_request.wrapping_add(1);
        self.open_shortcut_editor(self.shortcut_request, None, window, cx);
    }

    fn open_shortcut_editor(
        &mut self,
        request: u64,
        shortcut: Option<Shortcut>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_page(Page::Shortcuts, window, cx);
        self.cancel_composition(cx);
        self.set_compact(false, cx);
        self.overlay = None;
        self.loading_shortcut = None;
        self.shortcut_editor = Some(ShortcutEditor::new(request, shortcut, &self.intents, cx));
        self.focus(window, cx);
        cx.notify();
    }

    pub fn shortcut_loaded(
        &mut self,
        request: u64,
        result: Result<Shortcut, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading_shortcut != Some(request) {
            return;
        }
        self.loading_shortcut = None;
        match result {
            Ok(shortcut) => self.open_shortcut_editor(request, Some(shortcut), window, cx),
            Err(error) => self.toast(error, window, cx),
        }
    }

    pub(crate) fn set_shortcut_kind(&mut self, kind: ShortcutKind, cx: &mut Context<Self>) {
        if let Some(editor) = &mut self.shortcut_editor
            && !editor.saving()
        {
            editor.kind = kind;
            editor.error = None;
            cx.notify();
        }
    }

    fn save_shortcut(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.shortcut_editor else {
            return;
        };
        if editor.saving()
            || editor
                .fields()
                .iter()
                .any(|field| field.read(cx).composing())
        {
            return;
        }
        let draft = editor.draft(cx);
        let intent = Intent::SaveShortcut {
            request: editor.request,
            id: editor.id,
            draft: draft.clone(),
        };
        if self.intents.try_send(intent).is_ok() {
            editor.pending = Some(draft);
            editor.error = None;
            cx.notify();
        }
    }

    fn close_shortcut_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .shortcut_editor
            .as_ref()
            .is_some_and(|editor| !editor.saving())
        {
            self.cancel_composition(cx);
            self.shortcut_editor = None;
            self.focus(window, cx);
            cx.notify();
        }
    }

    pub fn shortcut_saved(
        &mut self,
        request: u64,
        result: Result<i64, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.shortcut_editor
            && editor.request == request
        {
            let submitted = editor.pending.take();
            match &result {
                Ok(id) => {
                    editor.id = Some(*id);
                    if submitted.is_some_and(|draft| draft != editor.draft(cx)) {
                        self.toast("已保存，表单还有未保存的改动", window, cx);
                        return;
                    }
                }
                Err(error) => {
                    editor.error = Some(error.clone().into());
                    cx.notify();
                    return;
                }
            }
            self.close_shortcut_editor(window, cx);
        }
        if result.is_ok() {
            self.refresh(cx);
            self.toast("已保存", window, cx);
        }
    }

    fn open_actions(&mut self, cx: &mut Context<Self>) {
        if self.selected().is_some() && !self.list.pending {
            self.set_compact(false, cx);
            self.overlay = Some(Overlay::Actions);
            self.menu_index = 0;
            cx.notify();
        }
    }

    fn apply_overlay(&mut self, overlay: Overlay, index: usize, cx: &mut Context<Self>) {
        match overlay {
            Overlay::Actions => {
                let Some(row) = self.selected().filter(|_| !self.list.pending) else {
                    return;
                };
                let target = &row.entry.target;
                match (result_actions(target).get(index), target) {
                    (Some(ResultAction::Run), _) => self.run_selected(false, cx),
                    (Some(ResultAction::Pin), _) => self.toggle_pin(cx),
                    (Some(ResultAction::EditShortcut), Target::Shortcut(id, _)) => {
                        let id = *id;
                        self.overlay = None;
                        self.shortcut_request = self.shortcut_request.wrapping_add(1);
                        self.loading_shortcut = Some(self.shortcut_request);
                        self.send(Intent::LoadShortcut {
                            request: self.shortcut_request,
                            id,
                        });
                    }
                    (Some(ResultAction::DeleteShortcut), Target::Shortcut(id, _)) => {
                        self.overlay = Some(Overlay::ConfirmDeleteShortcut(*id));
                    }
                    _ => {}
                }
            }
            Overlay::ConfirmSystem => self.run_selected(true, cx),
            Overlay::ConfirmClear => {
                self.overlay = None;
                self.send(Intent::ClearClipboard);
            }
            Overlay::ConfirmDeleteShortcut(id) => {
                self.overlay = None;
                self.send(Intent::DeleteShortcut(id));
            }
        }
        cx.notify();
    }

    fn save_settings(&mut self, settings: Settings, cx: &mut Context<Self>) {
        if settings != self.settings && !self.settings_pending {
            self.settings_pending = true;
            self.send(Intent::Settings(settings));
            cx.notify();
        }
    }

    fn edit_setting(&mut self, index: usize, step: i32, cx: &mut Context<Self>) {
        if self.settings_pending {
            return;
        }
        self.settings_index = index;
        let mut settings = self.settings.clone();
        match SettingsWindow::ITEMS[index].0 {
            Setting::Theme => {
                let list = [Theme::System, Theme::Light, Theme::Dark];
                settings.theme = next_item(&list, settings.theme, step);
            }
            Setting::Accent => {
                let list = [0x168776, 0x4462ad, 0x885492, 0xaa4a38];
                settings.accent = next_item(&list, settings.accent, step);
            }
            Setting::Opacity => {
                let list = [65, 75, 85, 92, 96, 100];
                settings.opacity = next_item(&list, settings.opacity, step);
            }
            Setting::Animations => settings.animations = !settings.animations,
            Setting::Hotkey => self.capturing_hotkey = true,
            Setting::Fullscreen => settings.allow_fullscreen = !settings.allow_fullscreen,
            Setting::Startup => settings.startup = !settings.startup,
            Setting::Clipboard => settings.clipboard_enabled = !settings.clipboard_enabled,
            Setting::Ocr => settings.clipboard_ocr = !settings.clipboard_ocr,
            Setting::Retention => {
                let list = [1, 7, 30, 90];
                settings.clipboard_days = next_item(&list, settings.clipboard_days, step);
            }
            Setting::Clear => self.overlay = Some(Overlay::ConfirmClear),
        }
        self.save_settings(settings, cx);
        cx.notify();
    }
}

fn next_item<T: Copy + PartialEq>(list: &[T], current: T, step: i32) -> T {
    let index = list
        .iter()
        .position(|option| *option == current)
        .unwrap_or(0);
    list[(index as i32 + step).rem_euclid(list.len() as i32) as usize]
}

fn can_pin(target: &Target) -> bool {
    matches!(
        target,
        Target::App(_) | Target::Command(_) | Target::Clip(_) | Target::Shortcut(..)
    )
}

impl Render for CommandPalette {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let colors = Colors::new(&self.settings, window.appearance());
        let (placeholder, icon) = match self.page {
            Page::Launcher => ("搜索应用、窗口和命令…", "\u{e721}"),
            Page::Windows => ("搜索运行中的窗口…", "\u{e737}"),
            Page::Clipboard if self.settings.clipboard_ocr => {
                ("搜索剪贴板或图片内文字…", "\u{e77f}")
            }
            Page::Clipboard => ("搜索剪贴板文字或图片…", "\u{e77f}"),
            Page::Files => ("搜索 Windows 已索引的文件…", "\u{e8b7}"),
            Page::Shortcuts => ("搜索常用文本和快捷链接…", "\u{e734}"),
            Page::Settings => ("", "\u{e713}"),
        };
        self.input.update(cx, |input, _| {
            input.style(placeholder, colors.text, colors.muted, colors.accent)
        });
        let editor = self.shortcut_editor.as_ref();
        let heading = editor
            .map(|editor| {
                if editor.id.is_some() {
                    "编辑常用项"
                } else {
                    "新建常用项"
                }
            })
            .or((self.page == Page::Settings).then_some("Winycast 设置"));
        let search = div()
            .h(px(62.))
            .flex_shrink_0()
            .px(px(19.))
            .flex()
            .items_center()
            .gap(px(13.))
            .border_b_1()
            .border_color(colors.line)
            .child(glyph(icon, colors.accent, 20.))
            .child(if let Some(heading) = heading {
                div()
                    .flex_1()
                    .text_size(px(21.))
                    .child(heading)
                    .into_any_element()
            } else {
                self.input.clone().into_any_element()
            })
            .when(editor.is_none() && self.page == Page::Shortcuts, |header| {
                header.child(
                    button("new-shortcut", "新建", colors)
                        .bg(colors.selected)
                        .text_color(colors.accent)
                        .on_click(cx.listener(|this, _, window, cx| this.new_shortcut(window, cx))),
                )
            })
            .child(if editor.is_some() {
                button("cancel-shortcut", "取消", colors)
                    .on_click(
                        cx.listener(|this, _, window, cx| this.close_shortcut_editor(window, cx)),
                    )
                    .into_any_element()
            } else if self.page == Page::Settings {
                button("back-to-search", "返回", colors)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.change_page(Page::Launcher, window, cx);
                    }))
                    .into_any_element()
            } else {
                button(
                    "compact",
                    if self.compact { "展开" } else { "收起" },
                    colors,
                )
                .on_click(cx.listener(|this, _, _, cx| this.set_compact(!this.compact, cx)))
                .into_any_element()
            });
        let body = match (editor, self.page) {
            (Some(editor), _) => editor.render(colors, cx),
            (None, Page::Settings) => SettingsWindow::render(
                &self.settings,
                self.settings_index,
                &self.settings_scroll,
                self.settings_pending,
                self.capturing_hotkey,
                colors,
                cx,
            ),
            _ => self.render_results(colors, cx),
        };
        let body = div()
            .flex_1()
            .min_h_0()
            .size_full()
            .overflow_hidden()
            .child(body);
        let body = if self.settings.animations {
            body.with_animation(
                ("page", self.scene),
                Animation::new(Duration::from_millis(120)),
                |element, progress| element.opacity(0.48 + 0.52 * progress),
            )
            .into_any_element()
        } else {
            body.into_any_element()
        };
        let panel = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .rounded(px(19.))
            .bg(colors.surface)
            .border_1()
            .border_color(colors.line)
            .font_family("Segoe UI")
            .text_color(colors.text)
            .track_focus(&self.controls)
            .on_key_down(cx.listener(Self::key))
            .child(search)
            .when(!self.compact, |panel| panel.child(body))
            .child(self.footer(colors, cx))
            .when_some(self.overlay, |panel, overlay| {
                panel.child(ActionMenu::render(self, overlay, colors, cx))
            })
            .when_some(self.toast.as_ref(), |panel, (_, text)| {
                panel.child(NotificationToast::render(text.clone(), colors))
            });
        if self.settings.animations {
            let closing = self.closing;
            panel
                .with_animation(
                    ("summon", self.opening),
                    Animation::new(Duration::from_millis(if closing { 100 } else { 150 })),
                    move |element, progress| {
                        element.opacity(if closing {
                            1. - progress
                        } else {
                            0.66 + 0.34 * progress
                        })
                    },
                )
                .into_any_element()
        } else {
            panel.into_any_element()
        }
    }
}

impl CommandPalette {
    fn render_results(&self, colors: Colors, cx: &mut Context<Self>) -> AnyElement {
        if self.list.rows.is_empty() {
            let status = if self.page == Page::Files && self.query.trim().is_empty() {
                "输入文件名或扩展名"
            } else if self.list.pending {
                "正在搜索…"
            } else if self.page == Page::Files && self.list.failed {
                "无法读取 Windows Search 索引"
            } else if self.page == Page::Files {
                "Windows 索引中没有匹配的文件"
            } else if self.page == Page::Clipboard
                && !self.settings.clipboard_enabled
                && self.query.is_empty()
            {
                "剪贴板记录已暂停。可在设置中开启。"
            } else if self.page == Page::Clipboard && self.query.is_empty() {
                "尚无剪贴板记录"
            } else if self.page == Page::Shortcuts && self.query.is_empty() {
                "按 Ctrl + N 新建常用文本或快捷链接"
            } else {
                "没有找到匹配结果"
            };
            return div()
                .size_full()
                .flex()
                .justify_center()
                .items_center()
                .text_size(px(14.))
                .text_color(colors.muted)
                .child(status)
                .into_any_element();
        }
        let rows = self.list.rows.clone();
        let selection = self.list.index;
        let weak = cx.weak_entity();
        let list = uniform_list("results", rows.len(), move |range, _, _| {
            range
                .map(|index| {
                    ResultItem::render(
                        &rows[index],
                        index,
                        selection == index,
                        colors,
                        weak.clone(),
                    )
                })
                .collect()
        })
        .size_full()
        .track_scroll(self.list.scroll.clone());
        div()
            .size_full()
            .flex()
            .gap(px(12.))
            .p(px(10.))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .when(self.list.pending, |list| list.opacity(0.60))
                    .child(list),
            )
            .when_some(self.selected(), |body, row| {
                body.child(PreviewPanel::render(
                    row,
                    self.preview.as_ref().map(|(_, image)| image.clone()),
                    colors,
                ))
            })
            .into_any_element()
    }

    fn footer(&self, colors: Colors, cx: &mut Context<Self>) -> impl IntoElement {
        let mut footer = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(5.))
            .px(px(10.))
            .h(px(38.))
            .flex_shrink_0()
            .border_t_1()
            .border_color(colors.line)
            .text_size(px(12.));
        if let Some(editor) = &self.shortcut_editor {
            return footer
                .child(
                    div()
                        .text_color(colors.muted)
                        .child("Tab 切换输入框 · Ctrl + Tab 切换类型"),
                )
                .child(
                    button(
                        "save-shortcut",
                        if editor.saving() {
                            "正在保存…"
                        } else {
                            "保存  Ctrl ↵"
                        },
                        colors,
                    )
                    .bg(colors.selected)
                    .text_color(colors.accent)
                    .on_click(cx.listener(|this, _, _, cx| this.save_shortcut(cx))),
                );
        }
        if self.page == Page::Settings {
            return footer.child(
                div()
                    .px(px(8.))
                    .text_color(colors.muted)
                    .child("↑ ↓ 选择 · ← → 调整 · Esc 返回"),
            );
        }
        footer = footer.child(
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .children(SEARCH_TABS.map(|(id, title, page)| {
                    button(id, title, colors)
                        .when(page == self.page, |tab| {
                            tab.bg(colors.selected).text_color(colors.accent)
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.change_page(page, window, cx)
                        }))
                })),
        );
        footer.child(
            div()
                .flex()
                .items_center()
                .gap(px(3.))
                .child(button("settings", "设置", colors).on_click(
                    cx.listener(|this, _, window, cx| this.change_page(Page::Settings, window, cx)),
                ))
                .child(
                    button("actions", "操作  Ctrl K", colors)
                        .on_click(cx.listener(|this, _, _, cx| this.open_actions(cx))),
                )
                .child(
                    button(
                        "execute",
                        match self.selected().map(|row| &row.entry.target) {
                            Some(Target::Shortcut(_, ShortcutKind::Snippet)) => "复制  ↵",
                            Some(Target::Shortcut(_, ShortcutKind::Quicklink)) => "打开  ↵",
                            _ if self.page == Page::Files => "打开  ↵",
                            _ => "运行  ↵",
                        },
                        colors,
                    )
                    .bg(colors.selected)
                    .text_color(colors.accent)
                    .on_click(cx.listener(|this, _, _, cx| this.run_selected(false, cx))),
                ),
        )
    }
}

struct ResultItem;
impl ResultItem {
    fn render(
        row: &Row,
        index: usize,
        selected: bool,
        colors: Colors,
        palette: WeakEntity<CommandPalette>,
    ) -> Stateful<Div> {
        let icon = target_icon(&row.entry.target);
        let subtitle = if matches!(row.entry.target, Target::Clip(_)) {
            "剪贴板".into()
        } else if let Target::Shortcut(_, kind) = row.entry.target {
            kind.label().into()
        } else {
            row.entry.detail.clone()
        };
        div()
            .id(("result", index))
            .w_full()
            .h(px(42.))
            .px(px(10.))
            .flex()
            .items_center()
            .gap(px(10.))
            .rounded(px(8.))
            .cursor_pointer()
            .when(selected, |row| row.bg(colors.selected))
            .when(!selected, |row| {
                row.hover(move |style| style.bg(colors.hover))
            })
            .child(
                div()
                    .rounded(px(7.))
                    .bg(colors.card)
                    .size(px(27.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .flex_shrink_0()
                    .child(glyph(
                        icon,
                        if selected {
                            colors.accent
                        } else {
                            colors.muted
                        },
                        14.,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .flex_col()
                    .child(
                        div()
                            .text_size(px(14.))
                            .truncate()
                            .child(row.entry.title.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(colors.muted)
                            .truncate()
                            .child(subtitle),
                    ),
            )
            .when(row.pinned, |row| {
                row.child(glyph("\u{e734}", colors.accent, 12.))
            })
            .on_click(move |event, window, cx| {
                let _ = palette.update(cx, |palette, cx| {
                    palette.focus(window, cx);
                    palette.select(index, cx);
                    if event.click_count() >= 2 {
                        palette.run_selected(false, cx);
                    }
                });
            })
    }
}

struct PreviewPanel;
impl PreviewPanel {
    fn render(row: &Row, image: Option<Preview>, colors: Colors) -> impl IntoElement {
        let category = match row.entry.target {
            Target::App(_) => "应用程序",
            Target::File(_) => "文件",
            Target::Window(_) => "运行窗口",
            Target::Command(_) => "命令",
            Target::CopyText(_) => "计算结果",
            Target::Clip(_) => "剪贴板",
            Target::Shortcut(_, kind) => kind.label(),
        };
        div()
            .w(px(200.))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .gap(px(9.))
            .p(px(14.))
            .rounded(px(12.))
            .bg(colors.hover)
            .overflow_hidden()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(7.))
                    .text_color(colors.accent)
                    .child(glyph(target_icon(&row.entry.target), colors.accent, 14.))
                    .child(div().text_size(px(12.)).child(category)),
            )
            .child(
                div()
                    .text_size(px(17.))
                    .font_weight(FontWeight::MEDIUM)
                    .line_clamp(2)
                    .child(row.entry.title.clone()),
            )
            .when_some(image, |preview, image| {
                preview.child(
                    img(image)
                        .w_full()
                        .h(px(150.))
                        .object_fit(ObjectFit::Contain),
                )
            })
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(colors.muted)
                    .line_clamp(6)
                    .child(row.entry.detail.clone()),
            )
    }
}

struct ActionMenu;
impl ActionMenu {
    fn render(
        palette: &CommandPalette,
        overlay: Overlay,
        colors: Colors,
        cx: &mut Context<CommandPalette>,
    ) -> impl IntoElement {
        let dialog = match overlay {
            Overlay::Actions => {
                let mut menu = div().flex().flex_col().gap(px(4.));
                if let Some(row) = palette.selected() {
                    for (index, action) in result_actions(&row.entry.target).iter().enumerate() {
                        let label = match action {
                            ResultAction::Run => match row.entry.target {
                                Target::Clip(_)
                                | Target::CopyText(_)
                                | Target::Shortcut(_, ShortcutKind::Snippet) => "复制",
                                Target::File(_) => "打开文件",
                                Target::Shortcut(_, ShortcutKind::Quicklink) => "在浏览器打开",
                                _ => "运行",
                            },
                            ResultAction::Pin => {
                                match (matches!(row.entry.target, Target::Clip(_)), row.pinned) {
                                    (true, true) => "取消固定",
                                    (true, false) => "固定条目",
                                    (false, true) => "取消收藏",
                                    (false, false) => "收藏",
                                }
                            }
                            ResultAction::EditShortcut => "编辑",
                            ResultAction::DeleteShortcut => "删除",
                        };
                        menu = menu.child(
                            button(("entry-action", index), label, colors)
                                .when(palette.menu_index == index, |item| item.bg(colors.selected))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.apply_overlay(overlay, index, cx)
                                })),
                        );
                    }
                }
                menu
            }
            Overlay::ConfirmSystem | Overlay::ConfirmClear | Overlay::ConfirmDeleteShortcut(_) => {
                let title = if overlay == Overlay::ConfirmClear {
                    "清空全部剪贴板历史？"
                } else if matches!(overlay, Overlay::ConfirmDeleteShortcut(_)) {
                    "删除这个常用项？"
                } else if palette.selected().is_some_and(|row| {
                    row.entry.target == Target::Command(Command::System(SystemCommand::Sleep))
                }) {
                    "让 Windows 进入睡眠？"
                } else {
                    "锁定 Windows 屏幕？"
                };
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .child(div().px(px(6.)).child(title))
                    .child(
                        div()
                            .flex()
                            .justify_end()
                            .gap(px(5.))
                            .child(
                                button("cancel-confirm", "取消", colors).on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.overlay = None;
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                button("apply-confirm", "确认", colors)
                                    .bg(colors.selected)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.apply_overlay(overlay, 0, cx)
                                    })),
                            ),
                    )
            }
        };
        div()
            .absolute()
            .bottom(px(42.))
            .right(px(12.))
            .min_w(px(190.))
            .p(px(8.))
            .bg(colors.card)
            .rounded(px(12.))
            .border_1()
            .border_color(colors.line)
            .shadow_lg()
            .child(dialog)
    }
}

struct NotificationToast;
impl NotificationToast {
    fn render(message: SharedString, colors: Colors) -> impl IntoElement {
        div()
            .absolute()
            .left(px(15.))
            .bottom(px(45.))
            .max_w(px(470.))
            .rounded(px(9.))
            .py(px(7.))
            .px(px(12.))
            .bg(colors.card)
            .border_1()
            .border_color(colors.line)
            .text_color(colors.text)
            .text_size(px(12.))
            .shadow_md()
            .child(message)
    }
}

fn target_icon(target: &Target) -> &'static str {
    match target {
        Target::App(_) => "\u{e74c}",
        Target::File(_) => "\u{e8a5}",
        Target::Window(_) => "\u{e737}",
        Target::Command(_) => "\u{e713}",
        Target::CopyText(_) => "\u{e8ef}",
        Target::Clip(_) => "\u{e77f}",
        Target::Shortcut(_, ShortcutKind::Snippet) => "\u{e8a5}",
        Target::Shortcut(_, ShortcutKind::Quicklink) => "\u{e71b}",
    }
}

fn glyph(symbol: &'static str, color: Hsla, size: f32) -> impl IntoElement {
    div()
        .font_family("Segoe Fluent Icons")
        .text_size(px(size))
        .text_color(color)
        .child(symbol)
}

pub(crate) fn button(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    colors: Colors,
) -> Stateful<Div> {
    div()
        .id(id)
        .h(px(27.))
        .px(px(9.))
        .rounded(px(6.))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .text_size(px(12.))
        .text_color(colors.muted)
        .hover(move |style| style.bg(colors.hover).text_color(colors.text))
        .child(label.into())
}

pub(crate) fn edit_setting(
    palette: &mut CommandPalette,
    index: usize,
    cx: &mut Context<CommandPalette>,
) {
    palette.edit_setting(index, 1, cx);
}
