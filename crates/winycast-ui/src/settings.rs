use gpui::prelude::FluentBuilder;
use gpui::*;
use winycast_core::{Settings, Theme};

use crate::palette::{Colors, CommandPalette, Setting, edit_setting};

pub struct SettingsWindow;

impl SettingsWindow {
    pub(crate) const ITEMS: [(Setting, &'static str); 10] = [
        (Setting::Theme, "主题"),
        (Setting::Accent, "主题色"),
        (Setting::Opacity, "背景透明度"),
        (Setting::Animations, "动画"),
        (Setting::Hotkey, "全局热键"),
        (Setting::Fullscreen, "允许在全屏应用上唤出"),
        (Setting::Startup, "登录 Windows 时启动"),
        (Setting::Clipboard, "剪贴板记录"),
        (Setting::Retention, "未固定条目保留天数"),
        (Setting::Clear, "清空剪贴板历史"),
    ];

    pub(crate) fn render(
        settings: &Settings,
        selected: usize,
        pending: bool,
        capture: bool,
        colors: Colors,
        cx: &mut Context<CommandPalette>,
    ) -> AnyElement {
        let on_off = |enabled| if enabled { "开启" } else { "关闭" }.to_owned();
        let values = [
            match settings.theme {
                Theme::System => "跟随系统",
                Theme::Light => "浅色",
                Theme::Dark => "深色",
            }
            .to_owned(),
            match settings.accent {
                0x4462ad => "靛蓝",
                0x885492 => "浅紫",
                0xaa4a38 => "珊瑚",
                _ => "青绿",
            }
            .to_owned(),
            format!("{}%", settings.opacity),
            on_off(settings.animations),
            if capture {
                "请按下组合键 · Esc 取消".into()
            } else {
                settings.hotkey.label()
            },
            on_off(settings.allow_fullscreen),
            on_off(settings.startup),
            if settings.clipboard_enabled {
                "记录中"
            } else {
                "已暂停"
            }
            .to_owned(),
            format!("{} 天", settings.clipboard_days),
            "清空…".into(),
        ];
        div()
            .id("settings-scroll")
            .size_full()
            .overflow_y_scroll()
            .p(px(10.))
            .when(pending, |panel| panel.opacity(0.65))
            .children(Self::ITEMS.into_iter().zip(values).enumerate().map(
                |(index, ((_, label), value))| {
                    div()
                        .id(("setting", index))
                        .px(px(12.))
                        .h(px(31.))
                        .rounded(px(7.))
                        .flex()
                        .items_center()
                        .justify_between()
                        .cursor_pointer()
                        .text_size(px(13.))
                        .when(index == selected, |row| row.bg(colors.selected))
                        .when(index != selected, |row| {
                            row.hover(move |style| style.bg(colors.hover))
                        })
                        .child(label)
                        .child(div().text_color(colors.accent).child(value))
                        .on_click(
                            cx.listener(move |palette, _, _, cx| edit_setting(palette, index, cx)),
                        )
                },
            ))
            .into_any_element()
    }
}
