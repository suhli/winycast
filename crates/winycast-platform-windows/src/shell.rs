use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    mem::size_of,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow, bail};
use windows::{
    Win32::{
        Foundation::{
            CloseHandle, ERROR_NOT_ALL_ASSIGNED, GetLastError, HANDLE, HWND, LPARAM, TRUE,
        },
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        Security::{
            AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW,
            SE_PRIVILEGE_ENABLED, SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES,
            TOKEN_QUERY,
        },
        System::{
            Com::{
                COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize, IBindCtx,
            },
            Power::SetSuspendState,
            Threading::{
                GetCurrentProcess, GetCurrentProcessId, OpenProcess, OpenProcessToken,
                PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
            },
        },
        UI::{
            Shell::{
                BHID_EnumItems, FOLDERID_AppsFolder, IEnumShellItems, IShellItem, KF_FLAG_DEFAULT,
                SHGetKnownFolderItem, SIGDN, SIGDN_DESKTOPABSOLUTEPARSING, SIGDN_NORMALDISPLAY,
                ShellExecuteW,
            },
            WindowsAndMessaging::*,
        },
    },
    core::{BOOL, PCWSTR, PWSTR, w},
};
use winycast_core::{Entry, SystemCommand, Target, WindowTarget};

struct Apartment;
impl Apartment {
    fn enter() -> Result<Self> {
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        }
        Ok(Self)
    }
}
impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe {
            CoUninitialize();
        }
    }
}

pub fn applications(cancelled: impl Fn() -> bool) -> Vec<Entry> {
    let mut entries = Vec::new();
    let mut titles = HashSet::new();
    if let Ok(_com) = Apartment::enter() {
        unsafe {
            if let Ok(folder) =
                SHGetKnownFolderItem::<IShellItem>(&FOLDERID_AppsFolder, KF_FLAG_DEFAULT, None)
                && let Ok(items) =
                    folder.BindToHandler::<_, IEnumShellItems>(None::<&IBindCtx>, &BHID_EnumItems)
            {
                let mut item = [None];
                let mut fetched = 0;
                while !cancelled()
                    && items.Next(&mut item, Some(&mut fetched)).is_ok()
                    && fetched == 1
                {
                    let Some(item) = item[0].take() else {
                        continue;
                    };
                    let Ok(title) = display_name(&item, SIGDN_NORMALDISPLAY) else {
                        continue;
                    };
                    let Ok(target) = display_name(&item, SIGDN_DESKTOPABSOLUTEPARSING) else {
                        continue;
                    };
                    insert_app(
                        &mut entries,
                        &mut titles,
                        title.to_string_lossy().into_owned(),
                        target,
                    );
                }
            }
        }
    }
    // Start Menu links cover desktop programs that don't appear in AppsFolder.
    for base in ["APPDATA", "PROGRAMDATA"]
        .into_iter()
        .filter_map(std::env::var_os)
    {
        let mut stack = vec![PathBuf::from(base).join("Microsoft/Windows/Start Menu/Programs")];
        while let Some(path) = stack.pop() {
            if cancelled() {
                return Vec::new();
            }
            let Ok(files) = std::fs::read_dir(path) else {
                continue;
            };
            for file in files.flatten() {
                let Ok(kind) = file.file_type() else {
                    continue;
                };
                let location = file.path();
                if kind.is_dir() {
                    stack.push(location);
                } else if kind.is_file()
                    && location.extension().is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("lnk") || ext.eq_ignore_ascii_case("appref-ms")
                    })
                    && let Some(name) = location.file_stem()
                    && !titles.contains(&name.to_string_lossy().to_lowercase())
                {
                    insert_app(
                        &mut entries,
                        &mut titles,
                        name.to_string_lossy().into_owned(),
                        location.into_os_string(),
                    );
                }
            }
        }
    }
    entries
}

fn insert_app(
    entries: &mut Vec<Entry>,
    titles: &mut HashSet<String>,
    title: String,
    target: OsString,
) {
    if title.trim().is_empty() || target.is_empty() {
        return;
    }
    let key = format!("app:{}", target.to_string_lossy().to_lowercase());
    titles.insert(title.to_lowercase());
    entries.push(Entry::new(key, title, "应用程序", "", Target::App(target)));
}

unsafe fn display_name(item: &IShellItem, mode: SIGDN) -> windows::core::Result<OsString> {
    unsafe {
        let name = item.GetDisplayName(mode)?;
        let value = OsString::from_wide(name.as_wide());
        CoTaskMemFree(Some(name.0.cast()));
        Ok(value)
    }
}

pub fn launch(target: &OsStr) -> Result<()> {
    let _com = Apartment::enter()?;
    let wide: Vec<_> = target.encode_wide().chain([0]).collect();
    let result = unsafe {
        ShellExecuteW(
            None,
            w!("open"),
            PCWSTR(wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        bail!("Windows Shell 无法打开此应用（错误 {}）", result.0 as isize);
    }
    Ok(())
}

pub fn running_windows(cancelled: impl Fn() -> bool) -> Vec<Entry> {
    let mut state = WindowScan {
        windows: Vec::new(),
        cancelled: &cancelled,
    };
    // EnumWindows is synchronous and doesn't retain this callback pointer.
    unsafe {
        let _ = EnumWindows(
            Some(collect_window),
            LPARAM((&mut state as *mut WindowScan<'_>) as isize),
        );
    }
    state.windows
}

struct WindowScan<'a> {
    windows: Vec<Entry>,
    cancelled: &'a dyn Fn() -> bool,
}

unsafe extern "system" fn collect_window(hwnd: HWND, param: LPARAM) -> BOOL {
    unsafe {
        let scan = &mut *(param.0 as *mut WindowScan<'_>);
        if (scan.cancelled)() {
            return BOOL(0);
        }
        if !IsWindowVisible(hwnd).as_bool()
            || hwnd == GetShellWindow()
            || hwnd == GetDesktopWindow()
        {
            return TRUE;
        }
        let styles = GetWindowLongW(hwnd, GWL_EXSTYLE) as u32;
        let owned = GetWindow(hwnd, GW_OWNER).is_ok_and(|owner| !owner.is_invalid());
        if styles & WS_EX_TOOLWINDOW.0 != 0 || (owned && styles & WS_EX_APPWINDOW.0 == 0) {
            return TRUE;
        }
        let mut cloaked = 0u32;
        if DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&mut cloaked as *mut u32).cast(),
            size_of::<u32>() as u32,
        )
        .is_ok()
            && cloaked != 0
        {
            return TRUE;
        }
        let mut pid = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == GetCurrentProcessId() {
            return TRUE;
        }
        let mut title = [0u16; 1024];
        let size = GetWindowTextW(hwnd, &mut title);
        if size <= 0 {
            return TRUE;
        }
        let title = String::from_utf16_lossy(&title[..size as usize]);
        if title.trim().is_empty() || title == "Program Manager" {
            return TRUE;
        }
        let detail = process_name(pid).unwrap_or_else(|| "运行窗口".into());
        let id = hwnd.0 as isize;
        scan.windows.push(Entry::new(
            format!("window:{pid}:{id}"),
            title,
            detail.clone(),
            &detail,
            Target::Window(WindowTarget {
                handle: id,
                process: pid,
            }),
        ));
        TRUE
    }
}

pub(crate) fn process_name(pid: u32) -> Option<String> {
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0; 2048];
        let mut len = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buf.as_mut_ptr()),
            &mut len,
        );
        let _ = CloseHandle(process);
        result.ok()?;
        let path = OsString::from_wide(&buf[..len as usize]);
        Some(Path::new(&path).file_name()?.to_string_lossy().into_owned())
    }
}

pub fn activate(target: WindowTarget) -> Result<()> {
    unsafe {
        let hwnd = HWND(target.handle as _);
        let mut current = 0;
        GetWindowThreadProcessId(hwnd, Some(&mut current));
        if current == 0 || current != target.process || !IsWindowVisible(hwnd).as_bool() {
            bail!("窗口已关闭或不可用");
        }
        if IsIconic(hwnd).as_bool() {
            let _ = ShowWindowAsync(hwnd, SW_RESTORE);
        }
        if !SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() != hwnd {
            bail!("Windows 未允许切换到所选窗口");
        }
        Ok(())
    }
}

pub fn run_system(command: SystemCommand) -> Result<()> {
    match command {
        SystemCommand::WindowsSettings => launch(OsStr::new("ms-settings:")),
        SystemCommand::Lock => unsafe {
            windows::Win32::System::Shutdown::LockWorkStation()?;
            Ok(())
        },
        SystemCommand::Sleep => sleep(),
    }
}

fn sleep() -> Result<()> {
    struct Token(HANDLE);
    impl Drop for Token {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    static SUSPEND: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _operation = SUSPEND.lock().map_err(|_| anyhow!("睡眠命令不可用"))?;
    unsafe {
        let mut token = Token(HANDLE::default());
        OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token.0,
        )?;
        let mut luid = Default::default();
        LookupPrivilegeValueW(PCWSTR::null(), SE_SHUTDOWN_NAME, &mut luid)?;
        let enabled = TOKEN_PRIVILEGES {
            PrivilegeCount: 1,
            Privileges: [LUID_AND_ATTRIBUTES {
                Luid: luid,
                Attributes: SE_PRIVILEGE_ENABLED,
            }],
        };
        let mut previous = TOKEN_PRIVILEGES::default();
        let mut returned = 0;
        AdjustTokenPrivileges(
            token.0,
            false,
            Some(&enabled),
            size_of::<TOKEN_PRIVILEGES>() as u32,
            Some(&mut previous),
            Some(&mut returned),
        )?;
        if GetLastError() == ERROR_NOT_ALL_ASSIGNED {
            bail!("当前账户没有 Windows 睡眠权限");
        }
        let result = if SetSuspendState(false, false, false) {
            Ok(())
        } else {
            Err(windows::core::Error::from_win32().into())
        };
        AdjustTokenPrivileges(token.0, false, Some(&previous), 0, None, None)?;
        result
    }
}

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "Winycast";

pub fn set_startup(enabled: bool) -> Result<()> {
    let key = windows_registry::CURRENT_USER.create(RUN_KEY)?;
    if enabled {
        let exe = std::env::current_exe()?;
        let mut command = OsString::from("\"");
        command.push(exe);
        command.push("\"");
        let command = command
            .into_string()
            .map_err(|_| anyhow!("启动路径不是有效的 Unicode"))?;
        key.set_string(RUN_VALUE, command.as_str())?;
    } else if let Err(error) = key.remove_value(RUN_VALUE) {
        if error.code().0 as u32 != 0x80070002 {
            return Err(error.into());
        }
    }
    Ok(())
}
