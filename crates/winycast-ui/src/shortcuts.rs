use async_channel::Sender;
use gpui::prelude::FluentBuilder;
use gpui::*;
use winycast_core::{Shortcut, ShortcutDraft, ShortcutKind};

use crate::{
    Intent,
    input::SearchInput,
    palette::{Colors, CommandPalette, button},
};

pub(crate) struct ShortcutEditor {
    pub request: u64,
    pub id: Option<i64>,
    pub kind: ShortcutKind,
    pub title: Entity<SearchInput>,
    pub body: Entity<SearchInput>,
    pub pending: Option<ShortcutDraft>,
    pub error: Option<SharedString>,
}

impl ShortcutEditor {
    pub fn new(
        request: u64,
        shortcut: Option<Shortcut>,
        intents: &Sender<Intent>,
        cx: &mut Context<CommandPalette>,
    ) -> Self {
        let (id, kind, title, body) = match shortcut {
            Some(shortcut) => (
                Some(shortcut.id),
                shortcut.draft.kind,
                shortcut.draft.title,
                shortcut.draft.body,
            ),
            None => (None, ShortcutKind::Snippet, String::new(), String::new()),
        };
        let title = cx.new(|cx| {
            let mut input = SearchInput::shortcut_field(intents.clone(), false, cx);
            input.set_text(title, cx);
            input
        });
        let body = cx.new(|cx| {
            let mut input = SearchInput::shortcut_field(intents.clone(), true, cx);
            input.set_text(body, cx);
            input
        });
        Self {
            request,
            id,
            kind,
            title,
            body,
            pending: None,
            error: None,
        }
    }

    pub fn fields(&self) -> [&Entity<SearchInput>; 2] {
        [&self.title, &self.body]
    }

    pub fn saving(&self) -> bool {
        self.pending.is_some()
    }

    pub fn draft(&self, cx: &App) -> ShortcutDraft {
        ShortcutDraft {
            kind: self.kind,
            title: self.title.read(cx).text().to_owned(),
            body: self.body.read(cx).text().to_owned(),
        }
    }

    pub fn render(&self, colors: Colors, cx: &mut Context<CommandPalette>) -> AnyElement {
        self.title.update(cx, |input, _| {
            input.style("输入名称", colors.text, colors.muted, colors.accent);
        });
        let label = match self.kind {
            ShortcutKind::Snippet => "文本",
            ShortcutKind::Quicklink => "网址",
        };
        self.body.update(cx, |input, _| {
            let placeholder = match self.kind {
                ShortcutKind::Snippet => "输入或粘贴文本",
                ShortcutKind::Quicklink => "https://",
            };
            input.style(placeholder, colors.text, colors.muted, colors.accent);
        });
        let types = [ShortcutKind::Snippet, ShortcutKind::Quicklink];
        div()
            .flex()
            .flex_col()
            .gap(px(9.))
            .px(px(20.))
            .py(px(12.))
            .text_size(px(12.))
            .when(self.saving(), |form| form.opacity(0.65))
            .child(div().flex().gap(px(6.)).children(types.map(|kind| {
                button(("shortcut-kind", kind as usize), kind.label(), colors)
                    .border_1()
                    .border_color(colors.line)
                    .when(kind == self.kind, |item| {
                        item.bg(colors.selected).text_color(colors.accent)
                    })
                    .on_click(cx.listener(move |palette, _, _, cx| {
                        palette.set_shortcut_kind(kind, cx);
                    }))
            })))
            .child(field("名称", self.title.clone(), colors))
            .child(field(label, self.body.clone(), colors))
            .when_some(self.error.clone(), |form, error| {
                form.child(div().text_color(colors.text).child(error))
            })
            .into_any_element()
    }
}

fn field(label: &'static str, input: Entity<SearchInput>, colors: Colors) -> impl IntoElement {
    div()
        .flex()
        .flex_col()
        .gap(px(4.))
        .text_color(colors.muted)
        .child(label)
        .child(
            div()
                .flex()
                .flex_col()
                .rounded(px(7.))
                .border_1()
                .border_color(colors.line)
                .bg(colors.hover)
                .px(px(10.))
                .child(input),
        )
}
