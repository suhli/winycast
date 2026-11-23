mod input;
mod palette;
mod settings;
mod shortcuts;

pub use palette::CommandPalette;

use std::sync::Arc;

use gpui::{FocusHandle, Image};
use winycast_core::{NoteDraft, NoteVersion, Page, Settings, ShortcutDraft, ShortcutKind, Target};

pub const PANEL_WIDTH: i32 = 666;
pub const PANEL_HEIGHT: i32 = 414;
pub const COMPACT_HEIGHT: i32 = 112;

#[derive(Debug)]
pub enum ShortcutSource {
    Existing(i64),
    Context(ShortcutKind),
    Clipboard(i64, ShortcutKind),
}

#[derive(Debug)]
pub enum Intent {
    Hide,
    Search(Page, String),
    Run {
        id: String,
        target: Target,
    },
    Pin {
        id: String,
        target: Target,
    },
    Preview(i64),
    Settings(Settings),
    ClearClipboard,
    Resize(bool),
    CopyInput(String),
    PasteInput(FocusHandle),
    LoadShortcut {
        request: u64,
        source: ShortcutSource,
    },
    SaveShortcut {
        request: u64,
        id: Option<i64>,
        draft: ShortcutDraft,
    },
    DeleteShortcut(i64),
    LoadNote {
        request: u64,
        id: i64,
    },
    SaveNote {
        session: u64,
        version: Option<NoteVersion>,
        draft: NoteDraft,
    },
    DeleteNote(i64),
    RenderNote {
        request: u64,
        markdown: String,
    },
}

pub type Preview = Arc<Image>;
