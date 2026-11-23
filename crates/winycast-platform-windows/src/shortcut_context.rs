use std::mem::size_of;

use windows::Win32::{
    Foundation::HWND,
    System::{
        Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
        Threading::GetCurrentProcessId,
        Variant::VARIANT,
    },
    UI::{
        Accessibility::*,
        WindowsAndMessaging::{
            GA_ROOT, GUITHREADINFO, GetAncestor, GetGUIThreadInfo, GetWindowThreadProcessId,
        },
    },
};
use winycast_core::{WindowTarget, web_address};

use crate::{
    clipboard::{private_application, private_text},
    shell::{Apartment, process_name},
};

const DRAFT_LIMIT: usize = 8192;

#[derive(Clone, Copy)]
pub struct ShortcutContext {
    window: WindowTarget,
    focus: isize,
}

impl ShortcutContext {
    pub(crate) fn capture(hwnd: HWND) -> Option<Self> {
        unsafe {
            let mut process = 0;
            let thread = GetWindowThreadProcessId(hwnd, Some(&mut process));
            if thread == 0 || process == 0 || process == GetCurrentProcessId() {
                return None;
            }
            let mut info = GUITHREADINFO {
                cbSize: size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            let focus = if GetGUIThreadInfo(thread, &mut info).is_ok()
                && GetAncestor(info.hwndFocus, GA_ROOT) == hwnd
            {
                info.hwndFocus.0 as isize
            } else {
                0
            };
            Some(Self {
                window: WindowTarget {
                    handle: hwnd.0 as isize,
                    process,
                },
                focus,
            })
        }
    }

    pub fn selection(self) -> Option<String> {
        if private_application(&self.source_name()?) {
            return None;
        }
        let _apartment = Apartment::multithreaded().ok()?;
        let automation = automation()?;
        unsafe {
            let hwnd = HWND(self.window.handle as _);
            let root = automation.ElementFromHandle(hwnd).ok()?;
            let focus = HWND(self.focus as _);
            if !focus.is_invalid()
                && GetAncestor(focus, GA_ROOT) == hwnd
                && let Ok(focused) = automation.ElementFromHandle(focus)
            {
                if focused.CurrentIsPassword().ok()?.as_bool() {
                    return None;
                }
                if let Ok(pattern) =
                    focused.GetCurrentPatternAs::<IUIAutomationTextPattern>(UIA_TextPatternId)
                {
                    return selected_text(&pattern);
                }
            }
            let document = automation
                .CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(UIA_DocumentControlTypeId.0),
                )
                .ok()?;
            let document = root.FindFirst(TreeScope_Subtree, &document).ok()?;
            if document.CurrentIsOffscreen().ok()?.as_bool() {
                return None;
            }
            let pattern = document.GetCurrentPatternAs(UIA_TextPatternId).ok()?;
            selected_text(&pattern)
        }
    }

    pub fn browser_url(self) -> Option<String> {
        let name = self.source_name()?;
        if !["chrome.exe", "msedge.exe", "firefox.exe"]
            .iter()
            .any(|browser| name.eq_ignore_ascii_case(browser))
        {
            return None;
        }
        let _apartment = Apartment::multithreaded().ok()?;
        let automation = automation()?;
        unsafe {
            let root = automation
                .ElementFromHandle(HWND(self.window.handle as _))
                .ok()?;
            let mut names = [
                (UIA_ClassNamePropertyId, "OmniboxViewViews"),
                (UIA_AutomationIdPropertyId, "addressEditBox"),
                (UIA_AutomationIdPropertyId, "urlbar-input"),
                (UIA_NamePropertyId, "Address and search bar"),
                (UIA_NamePropertyId, "地址栏和搜索栏"),
            ]
            .into_iter();
            let (property, name) = names.next()?;
            let mut address = automation
                .CreatePropertyCondition(property, &VARIANT::from(name))
                .ok()?;
            for (property, name) in names {
                let part = automation
                    .CreatePropertyCondition(property, &VARIANT::from(name))
                    .ok()?;
                address = automation.CreateOrCondition(&address, &part).ok()?;
            }
            let edit = automation
                .CreatePropertyCondition(
                    UIA_ControlTypePropertyId,
                    &VARIANT::from(UIA_EditControlTypeId.0),
                )
                .ok()?;
            let address = automation.CreateAndCondition(&edit, &address).ok()?;
            let address = root.FindFirst(TreeScope_Descendants, &address).ok()?;
            if address.CurrentIsPassword().ok()?.as_bool()
                || address.CurrentIsOffscreen().ok()?.as_bool()
                || !in_browser_chrome(&automation, &root, &address).unwrap_or(false)
            {
                return None;
            }
            let value: IUIAutomationValuePattern =
                address.GetCurrentPatternAs(UIA_ValuePatternId).ok()?;
            browser_address(&String::try_from(value.CurrentValue().ok()?).ok()?)
        }
    }

    fn source_name(self) -> Option<String> {
        let mut process = 0;
        unsafe {
            GetWindowThreadProcessId(HWND(self.window.handle as _), Some(&mut process));
        }
        (process == self.window.process)
            .then(|| process_name(process))
            .flatten()
    }
}

fn in_browser_chrome(
    automation: &IUIAutomation2,
    root: &IUIAutomationElement,
    address: &IUIAutomationElement,
) -> windows::core::Result<bool> {
    unsafe {
        let walker = automation.ControlViewWalker()?;
        let mut element = address.clone();
        // Web content can advertise the same accessible name as the address field.
        for _ in 0..16 {
            if element.CurrentControlType()? == UIA_DocumentControlTypeId {
                return Ok(false);
            }
            if automation.CompareElements(&element, root)?.as_bool() {
                return Ok(true);
            }
            element = walker.GetParentElement(&element)?;
        }
        Ok(false)
    }
}

fn automation() -> Option<IUIAutomation2> {
    unsafe {
        let client: IUIAutomation2 =
            CoCreateInstance(&CUIAutomation8, None, CLSCTX_INPROC_SERVER).ok()?;
        client.SetConnectionTimeout(400).ok()?;
        client.SetTransactionTimeout(800).ok()?;
        Some(client)
    }
}

fn selected_text(pattern: &IUIAutomationTextPattern) -> Option<String> {
    unsafe {
        let ranges = pattern.GetSelection().ok()?;
        let mut text = String::new();
        for index in 0..ranges.Length().ok()? {
            let range = ranges.GetElement(index).ok()?;
            let value = String::try_from(range.GetText((DRAFT_LIMIT + 1) as i32).ok()?).ok()?;
            if value.is_empty() {
                continue;
            }
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&value);
            if text.len() > DRAFT_LIMIT {
                return None;
            }
        }
        (!text.trim().is_empty() && !private_text(&text)).then_some(text)
    }
}

fn browser_address(value: &str) -> Option<String> {
    let value = value.trim();
    if let Some(address) = web_address(value) {
        return Some(address.into());
    }
    // Chromium can elide HTTPS in an unfocused address field.
    if value.contains([':', '@', '\\']) || value.contains(char::is_whitespace) {
        return None;
    }
    let address = web_address(&format!("https://{value}"))?;
    address.host_str()?.contains('.').then(|| address.into())
}

#[cfg(test)]
mod tests {
    use std::{sync::mpsc, thread, time::Duration};

    use anyhow::{Result, anyhow};
    use windows::{
        Win32::{
            Foundation::{LPARAM, WPARAM},
            System::Threading::GetCurrentThreadId,
            UI::WindowsAndMessaging::{
                CreateWindowExW, DestroyWindow, DispatchMessageW, ES_MULTILINE, GetMessageW, MSG,
                PostThreadMessageW, SendMessageW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_QUIT, WS_POPUP,
            },
        },
        core::w,
    };

    use super::*;

    #[test]
    fn browser_address_normalization_accepts_web_addresses() {
        assert_eq!(
            browser_address("example.org/docs").as_deref(),
            Some("https://example.org/docs")
        );
        assert_eq!(
            browser_address("http://localhost:8000/").as_deref(),
            Some("http://localhost:8000/")
        );
        assert!(browser_address("search words.example.org").is_none());
        assert!(browser_address("file:///C:/sample").is_none());
    }

    #[test]
    #[ignore = "requires the Windows UI Automation service"]
    fn reads_only_the_selected_range_from_a_native_edit_control() -> Result<()> {
        let (ready, window) = mpsc::sync_channel(1);
        let owner = thread::spawn(move || -> Result<()> {
            unsafe {
                let hwnd = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("EDIT"),
                    w!("before chosen after"),
                    WS_POPUP | WINDOW_STYLE(ES_MULTILINE as u32),
                    0,
                    0,
                    300,
                    100,
                    None,
                    None,
                    None,
                    None,
                )?;
                const EM_SETSEL: u32 = 0x00b1;
                SendMessageW(hwnd, EM_SETSEL, Some(WPARAM(7)), Some(LPARAM(13)));
                ready
                    .send((hwnd.0 as isize, GetCurrentThreadId()))
                    .map_err(|_| anyhow!("selection test cancelled"))?;
                let mut message = MSG::default();
                while GetMessageW(&mut message, None, 0, 0).0 > 0 {
                    DispatchMessageW(&message);
                }
                DestroyWindow(hwnd)?;
            }
            Ok(())
        });
        let (handle, thread_id) = window.recv_timeout(Duration::from_secs(5))?;
        let context = ShortcutContext {
            window: WindowTarget {
                handle,
                process: unsafe { GetCurrentProcessId() },
            },
            focus: handle,
        };
        let selection = context.selection();
        unsafe {
            PostThreadMessageW(thread_id, WM_QUIT, WPARAM(0), LPARAM(0))?;
        }
        owner
            .join()
            .map_err(|_| anyhow!("test window thread panicked"))??;
        assert_eq!(selection.as_deref(), Some("chosen"));
        Ok(())
    }
}
