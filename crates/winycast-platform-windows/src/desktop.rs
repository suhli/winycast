use std::mem::size_of;

use anyhow::{Result, anyhow};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::{
    Foundation::{HWND, LPARAM, POINT, RECT, WPARAM},
    Graphics::{
        Dwm::{DWMWA_EXTENDED_FRAME_BOUNDS, DwmGetWindowAttribute},
        Gdi::{
            GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromPoint,
            MonitorFromWindow,
        },
    },
    UI::{
        HiDpi::{
            DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow,
            SetProcessDpiAwarenessContext,
        },
        Input::Ime::{
            CPS_CANCEL, ImmGetContext, ImmNotifyIME, ImmReleaseContext, NI_COMPOSITIONSTR,
        },
        Shell::{QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState},
        WindowsAndMessaging::*,
    },
};

use crate::host::{DesktopEvent, panel_proc};

pub fn enable_per_monitor_dpi() {
    // Do this before GPUI creates the dispatcher HWND.
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PanelId(pub isize);

pub struct Panel {
    hwnd: HWND,
    previous: HWND,
}

impl Panel {
    pub fn attach(window: &impl HasWindowHandle) -> Result<Self> {
        let handle = window.window_handle().map_err(|error| anyhow!("{error}"))?;
        let RawWindowHandle::Win32(handle) = handle.as_raw() else {
            return Err(anyhow!("GPUI did not expose a Win32 window handle"));
        };
        let hwnd = HWND(handle.hwnd.get() as _);
        unsafe {
            use windows::Win32::UI::Shell::SetWindowSubclass;
            if !SetWindowSubclass(hwnd, Some(panel_proc), 1, 0).as_bool() {
                return Err(anyhow!("Could not attach the launcher window subclass"));
            }
        }
        Ok(Self {
            hwnd,
            previous: HWND::default(),
        })
    }

    pub fn id(&self) -> PanelId {
        PanelId(self.hwnd.0 as isize)
    }

    pub fn visible(&self) -> bool {
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn active(&self) -> bool {
        unsafe { GetForegroundWindow() == self.hwnd }
    }

    pub fn show(&mut self, width: i32, height: i32, allow_fullscreen: bool) -> Result<bool> {
        unsafe {
            let foreground = GetForegroundWindow();
            if !allow_fullscreen && foreground != self.hwnd && foreground_is_fullscreen(foreground)
            {
                return Ok(false);
            }

            let monitor = if foreground.is_invalid() {
                let mut cursor = POINT::default();
                let _ = GetCursorPos(&mut cursor);
                MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST)
            } else {
                MonitorFromWindow(foreground, MONITOR_DEFAULTTONEAREST)
            };
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                return Err(anyhow!("Could not read the active monitor work area"));
            }
            let work = info.rcWork;
            let work_width = work.right - work.left;
            let work_height = work.bottom - work.top;
            if !self.visible() {
                self.previous = foreground;
                // First move into the monitor while hidden so WM_DPICHANGED updates GPUI.
                SetWindowPos(
                    self.hwnd,
                    None,
                    work.left + work_width / 2,
                    work.top + work_height / 3,
                    0,
                    0,
                    SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOSIZE,
                )?;
            }
            let dpi = GetDpiForWindow(self.hwnd).max(96) as i32;
            let width = (width * dpi / 96).min(work_width);
            let height = (height * dpi / 96).min(work_height);
            SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                work.left + (work_width - width) / 2,
                work.top + (work_height / 5).min(work_height - height),
                width,
                height,
                SWP_SHOWWINDOW | SWP_NOACTIVATE,
            )?;
            if !SetForegroundWindow(self.hwnd).as_bool() && !self.active() {
                self.hide(false);
                return Err(anyhow!("Windows did not allow the launcher to take focus"));
            }
            Ok(true)
        }
    }

    pub fn hide(&mut self, restore: bool) {
        unsafe {
            let context = ImmGetContext(self.hwnd);
            if !context.0.is_null() {
                let _ = ImmNotifyIME(context, NI_COMPOSITIONSTR, CPS_CANCEL, 0);
                let _ = ImmReleaseContext(self.hwnd, context);
            }
            let was_active = self.active();
            let _ = ShowWindow(self.hwnd, SW_HIDE);
            if restore && was_active && IsWindow(Some(self.previous)).as_bool() {
                let now = GetForegroundWindow();
                // A click on another window wins over restoring the previously active app.
                if now == self.previous
                    || now == self.hwnd
                    || now.is_invalid()
                    || now == GetShellWindow()
                {
                    let _ = SetForegroundWindow(self.previous);
                }
            }
        }
        self.previous = HWND::default();
    }

    pub fn height(&self) -> i32 {
        unsafe {
            let mut bounds = RECT::default();
            if GetWindowRect(self.hwnd, &mut bounds).is_err() {
                return 0;
            }
            (bounds.bottom - bounds.top) * 96 / GetDpiForWindow(self.hwnd).max(96) as i32
        }
    }

    pub fn resize(&self, height: i32) -> Result<()> {
        unsafe {
            let mut bounds = RECT::default();
            GetWindowRect(self.hwnd, &mut bounds)?;
            let dpi = GetDpiForWindow(self.hwnd).max(96) as i32;
            let monitor = MonitorFromWindow(self.hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let height = if GetMonitorInfoW(monitor, &mut info).as_bool() {
                (height * dpi / 96)
                    .min(info.rcWork.bottom - bounds.top)
                    .max(1)
            } else {
                height * dpi / 96
            };
            SetWindowPos(
                self.hwnd,
                None,
                0,
                0,
                bounds.right - bounds.left,
                height,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            )?;
        }
        Ok(())
    }
}

unsafe fn foreground_is_fullscreen(hwnd: HWND) -> bool {
    unsafe {
        if SHQueryUserNotificationState().is_ok_and(|state| state == QUNS_RUNNING_D3D_FULL_SCREEN) {
            return true;
        }
        if hwnd.is_invalid() || hwnd == GetShellWindow() || hwnd == GetDesktopWindow() {
            return false;
        }
        let mut bounds = RECT::default();
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_EXTENDED_FRAME_BOUNDS,
            (&mut bounds as *mut RECT).cast(),
            size_of::<RECT>() as u32,
        )
        .is_err()
            && GetWindowRect(hwnd, &mut bounds).is_err()
        {
            return false;
        }
        let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut info).as_bool() {
            return false;
        }
        let screen = info.rcMonitor;
        let style = GetWindowLongW(hwnd, GWL_STYLE) as u32;
        !IsZoomed(hwnd).as_bool()
            && style & WS_CAPTION.0 == 0
            && bounds.left <= screen.left
            && bounds.top <= screen.top
            && bounds.right >= screen.right
            && bounds.bottom >= screen.bottom
    }
}

pub(crate) fn panel_event(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    _: LPARAM,
) -> Option<DesktopEvent> {
    match message {
        WM_ACTIVATE if wparam.0 & 0xffff == WA_INACTIVE as usize => {
            Some(DesktopEvent::Deactivated(PanelId(hwnd.0 as isize)))
        }
        WM_IME_ENDCOMPOSITION => Some(DesktopEvent::CompositionEnded(PanelId(hwnd.0 as isize))),
        _ => None,
    }
}
