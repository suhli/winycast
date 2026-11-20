mod clipboard;
mod desktop;
mod file_search;
mod host;
mod ocr;
mod shell;

pub use clipboard::{ClipboardWriter, capture, read_text, thumbnail};
pub use desktop::{Panel, PanelId, enable_per_monitor_dpi};
pub use file_search::search_files;
pub use host::{DesktopEvent, DesktopHost, Instance};
pub use ocr::ImageRecognizer;
pub use shell::{activate, applications, launch, run_system, running_windows, set_startup};

pub fn report_startup_error(message: &str) {
    use windows::{
        Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW},
        core::{HSTRING, w},
    };
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(message),
            w!("Winycast"),
            MB_ICONERROR | MB_OK,
        );
    }
}
