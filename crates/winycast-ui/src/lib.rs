mod input;
mod palette;
mod settings;

pub use palette::CommandPalette;

use std::sync::Arc;

use gpui::Image;
use winycast_core::{Page, Settings, Target};

pub const PANEL_WIDTH: i32 = 666;
pub const PANEL_HEIGHT: i32 = 414;
pub const COMPACT_HEIGHT: i32 = 112;

#[derive(Debug)]
pub enum Intent {
    Hide,
    Search(Page, String),
    Run { id: String, target: Target },
    Pin { id: String, target: Target },
    Preview(i64),
    Settings(Settings),
    ClearClipboard,
    Resize(bool),
    CopyInput(String),
    PasteInput,
}

pub type Preview = Arc<Image>;
