use super::*;
use crate::shortcuts::field;
use winycast_core::{MarkdownBlock, MarkdownBlockKind, Note, NoteDraft, NoteVersion};

pub(super) struct NoteEditor {
    session: u64,
    version: Option<NoteVersion>,
    baseline: NoteDraft,
    title: Entity<SearchInput>,
    body: Entity<SearchInput>,
    saving: Option<NoteDraft>,
    error: Option<SharedString>,
    mode: NoteMode,
    preview: Arc<[MarkdownBlock]>,
    scroll: ListState,
}

#[derive(Clone, Copy, PartialEq)]
enum NoteMode {
    Edit,
    Rendering(u64),
    Preview,
}

impl NoteEditor {
    fn new(
        session: u64,
        note: Option<(Note, Vec<MarkdownBlock>)>,
        intents: &Sender<Intent>,
        cx: &mut Context<CommandPalette>,
    ) -> Self {
        let (version, baseline, mode, blocks) = match note {
            Some((note, blocks)) => (Some(note.version), note.draft, NoteMode::Preview, blocks),
            None => (None, NoteDraft::default(), NoteMode::Edit, Vec::new()),
        };
        let title = cx.new(|cx| {
            let mut input = SearchInput::shortcut_field(intents.clone(), false, cx);
            input.set_text(baseline.title.clone(), cx);
            input
        });
        let body = cx.new(|cx| {
            let mut input = SearchInput::note_body(intents.clone(), cx);
            input.set_text(baseline.markdown.clone(), cx);
            input
        });
        Self {
            session,
            version,
            baseline,
            title,
            body,
            saving: None,
            error: None,
            mode,
            scroll: ListState::new(blocks.len(), ListAlignment::Top, px(180.)),
            preview: blocks.into(),
        }
    }

    pub(super) fn fields(&self) -> [&Entity<SearchInput>; 2] {
        [&self.title, &self.body]
    }

    pub(super) fn focus_field(&self, cx: &App) -> Option<FocusHandle> {
        (self.mode == NoteMode::Edit).then(|| {
            let field = if self.version.is_some() {
                &self.body
            } else {
                &self.title
            };
            field.read(cx).focus.clone()
        })
    }

    fn composing(&self, cx: &App) -> bool {
        self.fields().iter().any(|field| field.read(cx).composing())
    }

    fn dirty(&self, cx: &App) -> bool {
        self.title.read(cx).text() != self.baseline.title
            || self.body.read(cx).text() != self.baseline.markdown
    }

    pub(super) fn heading(&self, cx: &App) -> SharedString {
        if self.mode == NoteMode::Edit {
            return if self.version.is_some() {
                "编辑笔记"
            } else {
                "新建笔记"
            }
            .into();
        }
        let title = self.title.read(cx).text().trim();
        if title.is_empty() {
            "笔记预览".into()
        } else {
            title.to_owned().into()
        }
    }

    pub(super) fn mode_button(
        &self,
        colors: Colors,
        cx: &mut Context<CommandPalette>,
    ) -> AnyElement {
        button(
            "note-preview-toggle",
            if self.mode == NoteMode::Edit {
                "预览"
            } else {
                "编辑"
            },
            colors,
        )
        .on_click(cx.listener(|palette, _, window, cx| palette.toggle_note_preview(window, cx)))
        .into_any_element()
    }

    pub(super) fn footer(
        &self,
        footer: Div,
        colors: Colors,
        cx: &mut Context<CommandPalette>,
    ) -> Div {
        let status = self.error.clone().unwrap_or_else(|| {
            if self.mode == NoteMode::Edit {
                "Tab 切换输入框 · Ctrl + P 预览"
            } else {
                "↑ ↓ 滚动 · Ctrl + P 编辑"
            }
            .into()
        });
        footer
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(colors.muted)
                    .child(status),
            )
            .child(
                button(
                    "note-save",
                    if self.saving.is_some() {
                        "正在保存…"
                    } else {
                        "保存  Ctrl S"
                    },
                    colors,
                )
                .flex_shrink_0()
                .bg(colors.selected)
                .text_color(colors.accent)
                .on_click(cx.listener(|palette, _, _, cx| palette.save_note(cx))),
            )
    }

    pub(super) fn render(&self, colors: Colors, cx: &mut Context<CommandPalette>) -> AnyElement {
        match self.mode {
            NoteMode::Edit => {
                self.title.update(cx, |input, _| {
                    input.style("输入标题", colors.text, colors.muted, colors.accent);
                });
                self.body.update(cx, |input, _| {
                    input.style(
                        "输入 Markdown 正文",
                        colors.text,
                        colors.muted,
                        colors.accent,
                    );
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .px(px(20.))
                    .py(px(12.))
                    .text_size(px(12.))
                    .child(field("标题", self.title.clone(), colors))
                    .child(field("正文", self.body.clone(), colors))
                    .into_any_element()
            }
            NoteMode::Rendering(_) => preview_status("正在生成预览…", colors),
            NoteMode::Preview if self.preview.is_empty() => preview_status("笔记正文为空", colors),
            NoteMode::Preview => {
                let preview = self.preview.clone();
                list(self.scroll.clone(), move |index, _, _| {
                    render_block(&preview[index], colors)
                })
                .size_full()
                .into_any_element()
            }
        }
    }
}

impl CommandPalette {
    pub(super) fn active_note(&self) -> Option<&NoteEditor> {
        self.note_editor
            .as_ref()
            .filter(|_| self.page == Page::Notes)
    }

    fn next_note_request(&mut self) -> u64 {
        self.note_request = self.note_request.wrapping_add(1);
        self.note_request
    }

    pub fn new_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.change_page(Page::Notes, window, cx);
        if self.note_editor.is_some() {
            return;
        }
        let session = self.next_note_request();
        self.open_note_editor(session, None, window, cx);
    }

    pub fn load_note(&mut self, id: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.note_editor.is_some() {
            self.change_page(Page::Notes, window, cx);
            self.toast("请先返回当前笔记列表", window, cx);
            return;
        }
        let request = self.next_note_request();
        self.loading_note = Some(request);
        self.send(Intent::LoadNote { request, id });
    }

    fn open_note_editor(
        &mut self,
        session: u64,
        note: Option<(Note, Vec<MarkdownBlock>)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.change_page(Page::Notes, window, cx);
        self.loading_note = None;
        self.overlay = None;
        self.note_editor = Some(NoteEditor::new(session, note, &self.intents, cx));
        self.set_compact(false, cx);
        self.scene = self.scene.wrapping_add(1);
        self.focus(window, cx);
        cx.notify();
    }

    pub fn note_loaded(
        &mut self,
        request: u64,
        result: Result<(Note, Vec<MarkdownBlock>), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.loading_note != Some(request) {
            return;
        }
        self.loading_note = None;
        match result {
            Ok(note) => self.open_note_editor(request, Some(note), window, cx),
            Err(error) => self.toast(format!("无法打开笔记：{error}"), window, cx),
        }
    }

    fn toggle_note_preview(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_note() else {
            return;
        };
        if self.overlay.is_some() || editor.composing(cx) {
            return;
        }
        if editor.mode == NoteMode::Edit {
            let markdown = editor.body.read(cx).text().to_owned();
            let request = self.next_note_request();
            self.note_editor.as_mut().unwrap().mode = NoteMode::Rendering(request);
            self.send(Intent::RenderNote { request, markdown });
        } else {
            self.note_editor.as_mut().unwrap().mode = NoteMode::Edit;
        }
        self.scene = self.scene.wrapping_add(1);
        self.focus(window, cx);
        cx.notify();
    }

    pub fn note_preview(
        &mut self,
        request: u64,
        blocks: Vec<MarkdownBlock>,
        cx: &mut Context<Self>,
    ) {
        if let Some(editor) = &mut self.note_editor
            && editor.mode == NoteMode::Rendering(request)
        {
            editor.scroll.reset(blocks.len());
            editor.preview = blocks.into();
            editor.mode = NoteMode::Preview;
            cx.notify();
        }
    }

    fn save_note(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.note_editor else {
            return;
        };
        if self.overlay.is_some()
            || editor.saving.is_some()
            || editor.composing(cx)
            || (editor.version.is_some() && !editor.dirty(cx))
        {
            return;
        }
        let draft = NoteDraft {
            title: editor.title.read(cx).text().to_owned(),
            markdown: editor.body.read(cx).text().to_owned(),
        };
        match draft.clone().validated() {
            Ok(valid) => {
                let intent = Intent::SaveNote {
                    session: editor.session,
                    version: editor.version,
                    draft: valid,
                };
                editor.error = None;
                editor.saving = Some(draft);
                self.send(intent);
            }
            Err(error) => editor.error = Some(error.into()),
        }
        cx.notify();
    }

    pub fn note_saved(
        &mut self,
        session: u64,
        result: Result<Note, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self
            .note_editor
            .as_mut()
            .filter(|editor| editor.session == session)
        else {
            return;
        };
        let Some(sent) = editor.saving.take() else {
            return;
        };
        match result {
            Ok(note) => {
                editor.version = Some(note.version);
                editor.baseline = note.draft;
                if !editor.title.read(cx).composing()
                    && editor.title.read(cx).text() == sent.title
                    && sent.title != editor.baseline.title
                {
                    editor.title.update(cx, |title, cx| {
                        title.set_text(editor.baseline.title.clone(), cx)
                    });
                }
                self.request_search();
                self.toast("笔记已保存", window, cx);
            }
            Err(error) => editor.error = Some(format!("保存失败：{error}").into()),
        }
        cx.notify();
    }

    pub(super) fn close_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.active_note() else {
            return;
        };
        if self.overlay.is_some() || editor.saving.is_some() || editor.composing(cx) {
            return;
        }
        if editor.dirty(cx) {
            self.overlay = Some(Overlay::ConfirmDiscardNote);
            self.controls.focus(window);
            cx.notify();
        } else {
            self.discard_note(window, cx);
        }
    }

    pub(super) fn discard_note(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.note_editor = None;
        self.overlay = None;
        self.scene = self.scene.wrapping_add(1);
        self.focus(window, cx);
        self.request_search();
        cx.notify();
    }

    pub(super) fn note_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.active_note() else {
            return;
        };
        if self.closing || editor.composing(cx) {
            return;
        }
        let mods = event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        match key {
            "escape" => self.close_note(window, cx),
            "s" | "enter" if mods.control && !event.is_held => self.save_note(cx),
            "p" if mods.control && !event.is_held => self.toggle_note_preview(window, cx),
            "," if mods.control => self.change_page(Page::Settings, window, cx),
            "tab" if editor.mode == NoteMode::Edit => {
                let title = &editor.title.read(cx).focus;
                let body = &editor.body.read(cx).focus;
                if title.is_focused(window) {
                    body
                } else {
                    title
                }
                .focus(window);
            }
            "up" | "down" | "pageup" | "pagedown" if editor.mode == NoteMode::Preview => {
                editor.scroll.scroll_by(px(match key {
                    "up" => -32.,
                    "down" => 32.,
                    "pageup" => -240.,
                    _ => 240.,
                }));
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

fn preview_status(message: &'static str, colors: Colors) -> AnyElement {
    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .text_color(colors.muted)
        .text_size(px(14.))
        .child(message)
        .into_any_element()
}

fn render_block(block: &MarkdownBlock, colors: Colors) -> AnyElement {
    let content = div().min_w_0().ml(px(block.list_depth.min(8) as f32 * 15.));
    let content = match block.kind {
        MarkdownBlockKind::Rule => content.h(px(1.)).my(px(5.)).bg(colors.line),
        kind => {
            let code = kind == MarkdownBlockKind::Code;
            let size = match kind {
                MarkdownBlockKind::Heading(1) => 25.,
                MarkdownBlockKind::Heading(2) => 21.,
                MarkdownBlockKind::Heading(_) => 17.,
                _ => 14.,
            };
            let runs = block
                .spans
                .iter()
                .map(|span| {
                    let style = span.style;
                    let mut face = font(if style.code { "Consolas" } else { "Segoe UI" });
                    if style.strong || matches!(kind, MarkdownBlockKind::Heading(_)) {
                        face = face.bold();
                    }
                    if style.emphasis {
                        face = face.italic();
                    }
                    TextRun {
                        len: span.len,
                        font: face,
                        color: if style.link {
                            colors.accent
                        } else {
                            colors.text
                        },
                        background_color: (style.code && !code).then_some(colors.hover),
                        underline: style.link.then_some(UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        }),
                        strikethrough: style.strike.then_some(StrikethroughStyle {
                            thickness: px(1.),
                            color: None,
                        }),
                    }
                })
                .collect();
            content
                .text_size(px(size))
                .line_height(px(size + 7.))
                .when(code, |block| {
                    block.bg(colors.hover).rounded(px(6.)).p(px(8.))
                })
                .child(StyledText::new(block.text.clone()).with_runs(runs))
        }
    };
    div()
        .px(px(20.))
        .py(px(5.))
        .child(content.when(block.quoted, |block| {
            block.border_l_2().border_color(colors.accent).pl(px(9.))
        }))
        .into_any_element()
}
