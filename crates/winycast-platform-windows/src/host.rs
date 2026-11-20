use std::{cell::RefCell, mem::size_of};

use anyhow::{Result, anyhow};
use async_channel::Sender;
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM, LRESULT, POINT,
            WPARAM,
        },
        System::{
            DataExchange::{
                AddClipboardFormatListener, GetClipboardSequenceNumber,
                RemoveClipboardFormatListener,
            },
            LibraryLoader::GetModuleHandleW,
            Threading::CreateMutexW,
        },
        UI::{
            Input::KeyboardAndMouse::{
                HOT_KEY_MODIFIERS, MOD_NOREPEAT, RegisterHotKey, UnregisterHotKey,
            },
            Shell::{
                DefSubclassProc, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
                NOTIFYICONDATAW, RemoveWindowSubclass, Shell_NotifyIconW,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{PCWSTR, w},
};
use winycast_core::Hotkey;

use crate::clipboard::ClipboardWriter;
use crate::desktop::{PanelId, panel_event};

const HOST_CLASS: PCWSTR = w!("Winycast.DesktopHost.1");
const MUTEX: PCWSTR = w!("Local\\Winycast.Launcher.1");
const TRAY: u32 = WM_APP + 1;
const ACTIVATE: u32 = WM_APP + 2;
const HOTKEY_IDS: [i32; 2] = [101, 102];

#[derive(Clone, Copy, Debug)]
pub enum DesktopEvent {
    Toggle,
    Show,
    Settings,
    Quit,
    Deactivated(PanelId),
    PanelClose(PanelId),
    CompositionEnded(PanelId),
    HotkeyUnavailable,
    Clipboard(u32),
}

pub struct Instance(HANDLE);

impl Instance {
    pub fn acquire() -> Result<Option<Self>> {
        unsafe {
            let handle = CreateMutexW(None, false, MUTEX)?;
            let instance = Self(handle);
            if GetLastError() == ERROR_ALREADY_EXISTS {
                if let Ok(host) = FindWindowW(HOST_CLASS, None) {
                    let mut pid = 0;
                    GetWindowThreadProcessId(host, Some(&mut pid));
                    if pid != 0 {
                        let _ = AllowSetForegroundWindow(pid);
                    }
                    let _ = PostMessageW(Some(host), ACTIVATE, WPARAM(0), LPARAM(0));
                }
                return Ok(None);
            }
            Ok(Some(instance))
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct Callbacks {
    events: Sender<DesktopEvent>,
    taskbar_created: u32,
    active_hotkey: Option<usize>,
}

thread_local! {
    static CALLBACKS: RefCell<Option<Callbacks>> = const { RefCell::new(None) };
}

pub struct DesktopHost {
    hwnd: HWND,
    binding: Option<(usize, Hotkey)>,
    watching: bool,
}

impl DesktopHost {
    pub fn new(events: Sender<DesktopEvent>, hotkey: Hotkey) -> Result<Self> {
        unsafe {
            let module = GetModuleHandleW(None)?;
            let class = WNDCLASSW {
                hInstance: module.into(),
                lpszClassName: HOST_CLASS,
                lpfnWndProc: Some(host_proc),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return Err(windows::core::Error::from_win32().into());
            }
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW,
                HOST_CLASS,
                w!("Winycast background"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(module.into()),
                None,
            )?;
            CALLBACKS.with_borrow_mut(|state| {
                *state = Some(Callbacks {
                    events,
                    taskbar_created: RegisterWindowMessageW(w!("TaskbarCreated")),
                    active_hotkey: None,
                })
            });
            if !tray_icon(hwnd, NIM_ADD) {
                let _ = DestroyWindow(hwnd);
                CALLBACKS.with_borrow_mut(|state| *state = None);
                return Err(anyhow!("Could not register the Winycast tray icon"));
            }
            let mut host = Self {
                hwnd,
                binding: None,
                watching: false,
            };
            if host.set_hotkey(hotkey).is_err() {
                emit(DesktopEvent::HotkeyUnavailable);
                emit(DesktopEvent::Show);
            }
            Ok(host)
        }
    }

    pub fn clipboard_writer(&self) -> ClipboardWriter {
        ClipboardWriter(self.hwnd.0 as isize)
    }

    pub fn watch_clipboard(&mut self, enabled: bool) -> Result<()> {
        if enabled != self.watching {
            unsafe {
                if enabled {
                    AddClipboardFormatListener(self.hwnd)?;
                } else {
                    RemoveClipboardFormatListener(self.hwnd)?;
                }
            }
            self.watching = enabled;
        }
        Ok(())
    }

    pub fn set_hotkey(&mut self, hotkey: Hotkey) -> Result<()> {
        if self.binding.is_some_and(|(_, current)| current == hotkey) {
            return Ok(());
        }
        let next = self.binding.map_or(0, |(slot, _)| 1 - slot);
        unsafe {
            RegisterHotKey(
                Some(self.hwnd),
                HOTKEY_IDS[next],
                HOT_KEY_MODIFIERS(hotkey.modifiers) | MOD_NOREPEAT,
                hotkey.key,
            )?;
            if let Some((old, _)) = self.binding {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_IDS[old]);
            }
        }
        self.binding = Some((next, hotkey));
        CALLBACKS.with_borrow_mut(|state| {
            if let Some(state) = state {
                state.active_hotkey = Some(next);
            }
        });
        Ok(())
    }
}

impl Drop for DesktopHost {
    fn drop(&mut self) {
        let _ = self.watch_clipboard(false);
        unsafe {
            if let Some((slot, _)) = self.binding {
                let _ = UnregisterHotKey(Some(self.hwnd), HOTKEY_IDS[slot]);
            }
            tray_icon(self.hwnd, NIM_DELETE);
            let _ = DestroyWindow(self.hwnd);
        }
        CALLBACKS.with_borrow_mut(|state| *state = None);
    }
}

fn emit(event: DesktopEvent) {
    CALLBACKS.with_borrow(|state| {
        if let Some(state) = state {
            let _ = state.events.try_send(event);
        }
    });
}

unsafe fn tray_icon(hwnd: HWND, message: windows::Win32::UI::Shell::NOTIFY_ICON_MESSAGE) -> bool {
    let mut icon = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: TRAY,
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
        ..Default::default()
    };
    for (slot, unit) in icon.szTip.iter_mut().zip("Winycast".encode_utf16()) {
        *slot = unit;
    }
    unsafe { Shell_NotifyIconW(message, &icon).as_bool() }
}

unsafe fn tray_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, 1, w!("打开 Winycast"));
        let _ = AppendMenuW(menu, MF_STRING, 2, w!("设置"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, 3, w!("退出"));
        let mut point = POINT::default();
        if GetCursorPos(&mut point).is_ok() {
            let _ = SetForegroundWindow(hwnd);
            let choice = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                hwnd,
                None,
            );
            match choice.0 {
                1 => emit(DesktopEvent::Show),
                2 => emit(DesktopEvent::Settings),
                3 => emit(DesktopEvent::Quit),
                _ => {}
            }
            let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        }
        let _ = DestroyMenu(menu);
    }
}

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY => {
            if CALLBACKS.with_borrow(|state| {
                state
                    .as_ref()
                    .and_then(|s| s.active_hotkey)
                    .is_some_and(|slot| HOTKEY_IDS[slot] as usize == wparam.0)
            }) {
                emit(DesktopEvent::Toggle);
            }
        }
        ACTIVATE => emit(DesktopEvent::Show),
        WM_CLIPBOARDUPDATE => emit(DesktopEvent::Clipboard(unsafe {
            GetClipboardSequenceNumber()
        })),
        TRAY => match lparam.0 as u32 {
            WM_LBUTTONUP => emit(DesktopEvent::Toggle),
            WM_RBUTTONUP | WM_CONTEXTMENU => unsafe { tray_menu(hwnd) },
            _ => {}
        },
        _ => {
            if CALLBACKS.with_borrow(|s| s.as_ref().is_some_and(|s| message == s.taskbar_created)) {
                unsafe {
                    tray_icon(hwnd, NIM_ADD);
                }
            } else {
                return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
            }
        }
    }
    LRESULT(0)
}

pub(crate) unsafe extern "system" fn panel_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    _: usize,
) -> LRESULT {
    if message == WM_CLOSE {
        emit(DesktopEvent::PanelClose(PanelId(hwnd.0 as isize)));
        return LRESULT(0);
    }
    if let Some(event) = panel_event(hwnd, message, wparam, lparam) {
        emit(event);
    }
    if message == WM_NCDESTROY {
        unsafe {
            let _ = RemoveWindowSubclass(hwnd, Some(panel_proc), id);
        }
    }
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
