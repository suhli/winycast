mod calculator;
mod search;
mod shortcut;

use serde::{Deserialize, Serialize};

pub use calculator::calculate;
pub use search::{Entry, Usage, rank};
pub use shortcut::{Shortcut, ShortcutDraft, ShortcutKind, shortcut_entry_id};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Page {
    #[default]
    Launcher,
    Windows,
    Clipboard,
    Files,
    Shortcuts,
    Settings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemCommand {
    Lock,
    Sleep,
    WindowsSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Command {
    Page(Page),
    System(SystemCommand),
    Rescan,
}

pub fn commands() -> Vec<Entry> {
    use Command::*;
    [
        (
            "clipboard",
            "剪贴板历史",
            "Clipboard History",
            Page(crate::Page::Clipboard),
        ),
        (
            "windows",
            "切换窗口",
            "Switch Windows",
            Page(crate::Page::Windows),
        ),
        (
            "settings",
            "Winycast 设置",
            "Winycast Settings preferences",
            Page(crate::Page::Settings),
        ),
        (
            "files",
            "搜索文件",
            "File Search Windows Search 文件搜索",
            Page(crate::Page::Files),
        ),
        (
            "shortcuts",
            "常用文本与链接",
            "Snippets Quicklinks Shortcuts 常用项",
            Page(crate::Page::Shortcuts),
        ),
        (
            "windows-settings",
            "打开 Windows 设置",
            "Windows Settings",
            System(SystemCommand::WindowsSettings),
        ),
        (
            "lock",
            "锁定屏幕",
            "Lock PC 屏幕锁屏",
            System(SystemCommand::Lock),
        ),
        (
            "sleep",
            "睡眠",
            "Sleep Suspend",
            System(SystemCommand::Sleep),
        ),
        ("rescan", "重新扫描应用", "Refresh Rescan Apps", Rescan),
    ]
    .into_iter()
    .map(|(id, title, aliases, command)| {
        Entry::new(
            format!("command:{id}"),
            title,
            "命令",
            aliases,
            Target::Command(command),
        )
    })
    .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Target {
    App(std::ffi::OsString),
    File(std::path::PathBuf),
    Window(WindowTarget),
    Command(Command),
    CopyText(String),
    Clip(i64),
    Shortcut(i64, ShortcutKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowTarget {
    pub handle: isize,
    pub process: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    pub entry: Entry,
    pub pinned: bool,
}

#[derive(Clone, Debug)]
pub enum ClipboardData {
    Text(String),
    Png(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum Theme {
    #[default]
    System,
    Dark,
    Light,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: Theme,
    pub opacity: u8,
    pub accent: u32,
    pub animations: bool,
    pub hotkey: Hotkey,
    pub allow_fullscreen: bool,
    pub startup: bool,
    pub clipboard_enabled: bool,
    pub clipboard_ocr: bool,
    pub clipboard_days: u16,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: Theme::System,
            opacity: 96,
            accent: 0x168776,
            animations: true,
            hotkey: Hotkey::default(),
            allow_fullscreen: false,
            startup: false,
            clipboard_enabled: false,
            clipboard_ocr: false,
            clipboard_days: 30,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Hotkey {
    pub modifiers: u32,
    pub key: u32,
}

impl Default for Hotkey {
    fn default() -> Self {
        Self {
            modifiers: 0x03,
            key: 0x20,
        }
    }
}

impl Hotkey {
    pub fn from_key(
        name: &str,
        control: bool,
        alt: bool,
        shift: bool,
        windows: bool,
    ) -> Option<Self> {
        if !(control || alt || windows) {
            return None;
        }
        let modifiers = u32::from(alt)
            | (u32::from(control) << 1)
            | (u32::from(shift) << 2)
            | (u32::from(windows) << 3);
        let key = match name {
            "space" => 0x20,
            key if key.len() == 1 && key.as_bytes()[0].is_ascii_alphanumeric() => {
                u32::from(key.as_bytes()[0].to_ascii_uppercase())
            }
            key if key.starts_with('f') => {
                let n: u32 = key[1..].parse().ok()?;
                if !(1..=24).contains(&n) || n == 12 {
                    return None;
                }
                0x70 + n - 1
            }
            _ => return None,
        };
        Some(Self { modifiers, key })
    }

    pub fn label(self) -> String {
        let mut pieces = Vec::new();
        for (mask, label) in [(2, "Ctrl"), (1, "Alt"), (4, "Shift"), (8, "Win")] {
            if self.modifiers & mask != 0 {
                pieces.push(label.to_owned());
            }
        }
        pieces.push(match self.key {
            0x20 => "Space".to_owned(),
            0x30..=0x39 | 0x41..=0x5a => char::from_u32(self.key).unwrap().to_string(),
            0x70..=0x87 => format!("F{}", self.key - 0x70 + 1),
            _ => format!("VK {:02X}", self.key),
        });
        pieces.join(" + ")
    }
}
