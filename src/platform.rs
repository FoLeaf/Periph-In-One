use crate::{
    AppWindow,
    app::{Action, post},
};
pub mod update;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, winit_030::WinitWindowAccessor};
use std::{
    path::PathBuf,
    ptr::{null, null_mut},
};
use windows::Win32::{
    System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree, IPersistFile},
    UI::Shell::{
        FOLDERID_Startup, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath,
        SetCurrentProcessExplicitAppUserModelID, ShellLink,
    },
};
use windows::core::{Interface, PCWSTR};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::{Dwm::*, Gdi::*},
    System::{LibraryLoader::GetModuleHandleW, Registry::*, Threading::*},
    UI::{
        HiDpi::*,
        Shell::{DefSubclassProc, SetWindowSubclass},
        WindowsAndMessaging::*,
    },
};

pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn error_dialog(message: &str) {
    unsafe {
        MessageBoxW(
            null_mut(),
            wide(message).as_ptr(),
            wide("Periph In One").as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

pub fn initialize_taskbar() -> Result<(), String> {
    unsafe { SetCurrentProcessExplicitAppUserModelID(PCWSTR(wide("PeriphInOne.Desktop").as_ptr())) }
        .map_err(|_| "无法初始化任务栏，请重新启动软件".into())
}

fn application_icons(dpi: u32) -> Result<(HICON, HICON), String> {
    unsafe {
        let module = GetModuleHandleW(null());
        let load = |width, height| {
            LoadImageW(
                module,
                std::ptr::without_provenance(1),
                IMAGE_ICON,
                GetSystemMetricsForDpi(width, dpi),
                GetSystemMetricsForDpi(height, dpi),
                LR_SHARED,
            ) as HICON
        };
        // Resource 1 is the multi-resolution ICO embedded by winresource.
        // Shared module resources outlive HWND destruction and need no DestroyIcon.
        let small = load(SM_CXSMICON, SM_CYSMICON);
        let large = load(SM_CXICON, SM_CYICON);
        if small.is_null() || large.is_null() {
            return Err("无法加载应用图标，请重新下载软件".into());
        }
        Ok((small, large))
    }
}

pub struct SingleInstance(HANDLE);
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            CloseHandle(self.0);
        }
    }
}

pub fn show_message() -> u32 {
    // Keep the existing IPC and mutex identifiers across the product rename:
    // an older executable must not start a competing HID reader.
    static MESSAGE: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *MESSAGE.get_or_init(|| unsafe {
        RegisterWindowMessageW(wide("BatteryIsland.ShowPanel.v1").as_ptr())
    })
}

/// Independent of the panel: startup can stay in the tray without ever
/// creating a visible Slint window, while duplicate launches still wake it.
pub struct IpcWindow(HWND);
impl Drop for IpcWindow {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}

pub fn ipc_window() -> Result<IpcWindow, String> {
    unsafe {
        let module = GetModuleHandleW(null());
        let class = wide("PeriphInOne.Ipc.v1");
        let definition = WNDCLASSW {
            lpfnWndProc: Some(ipc_proc),
            hInstance: module,
            lpszClassName: class.as_ptr(),
            ..std::mem::zeroed()
        };
        RegisterClassW(&definition);
        let handle = CreateWindowExW(
            WS_EX_TOOLWINDOW,
            class.as_ptr(),
            null(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            module,
            null(),
        );
        if handle.is_null() {
            Err("无法创建单实例消息窗口".into())
        } else {
            Ok(IpcWindow(handle))
        }
    }
}

unsafe extern "system" fn ipc_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == show_message() {
        post(Action::Show);
        return 0;
    }
    match msg {
        WM_DEVICECHANGE => post(Action::Refresh),
        WM_POWERBROADCAST if wp == 18 || wp == 7 => post(Action::Refresh),
        _ => {}
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

pub fn single_instance(autostart: bool) -> Result<Option<SingleInstance>, String> {
    unsafe {
        let handle = CreateMutexW(null(), 0, wide("Local\\BatteryIsland.v1").as_ptr());
        if handle.is_null() {
            return Err("无法创建单实例锁".into());
        }
        if GetLastError() == ERROR_ALREADY_EXISTS {
            CloseHandle(handle);
            if !autostart {
                AllowSetForegroundWindow(ASFW_ANY);
                SendNotifyMessageW(HWND_BROADCAST, show_message(), 0, 0);
            }
            return Ok(None);
        }
        Ok(Some(SingleInstance(handle)))
    }
}

pub fn hwnd(window: &slint::Window) -> Option<HWND> {
    let provider = window.window_handle();
    match provider.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get() as HWND),
        _ => None,
    }
}

fn attach(window: &slint::Window, mini: bool) -> Result<(), String> {
    let handle = hwnd(window).ok_or("窗口句柄尚未创建")?;
    unsafe {
        // Slint caches the image even across HWND recreation; its pending
        // attributes only carry ICON_SMALL. Restore both icons on every HWND.
        let (small, large) = application_icons(GetDpiForWindow(handle).max(96))?;
        SendMessageW(handle, WM_SETICON, ICON_SMALL as usize, small as isize);
        SendMessageW(handle, WM_SETICON, ICON_BIG as usize, large as isize);
        // SetWindowSubclass with the same id replaces the previous entry, so
        // re-attaching after every hide/show cycle stays idempotent.
        if SetWindowSubclass(handle, Some(window_proc), 0x4249, mini as usize) == 0 {
            return Err("无法安装窗口事件处理".into());
        }
        let style = GetWindowLongPtrW(handle, GWL_STYLE) as u32;
        SetWindowLongPtrW(
            handle,
            GWL_STYLE,
            (style | WS_THICKFRAME | WS_SYSMENU | WS_MINIMIZEBOX) as isize,
        );
        SetWindowPos(
            handle,
            null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED | SWP_NOACTIVATE,
        );
        let corner = DWMWCP_ROUND;
        DwmSetWindowAttribute(
            handle,
            DWMWA_WINDOW_CORNER_PREFERENCE as u32,
            (&corner as *const DWM_WINDOW_CORNER_PREFERENCE).cast(),
            std::mem::size_of_val(&corner) as u32,
        );
    }
    Ok(())
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wp: WPARAM,
    lp: LPARAM,
    _: usize,
    mode: usize,
) -> LRESULT {
    let mini = mode != 0;
    // The FFI boundary never borrows application state or panics. Work is
    // queued into Slint, avoiding reentrant RefCell borrows during window calls.
    unsafe {
        match msg {
            WM_NCCALCSIZE if wp != 0 => return 0,
            WM_NCHITTEST => {
                let mut rect: RECT = std::mem::zeroed();
                if GetWindowRect(hwnd, &mut rect) != 0 {
                    let x = (lp as u16 as i16) as i32;
                    let y = ((lp >> 16) as u16 as i16) as i32;
                    return panel_hit_test(
                        x - rect.left,
                        y - rect.top,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                        GetDpiForWindow(hwnd),
                        mini,
                    ) as LRESULT;
                }
            }
            WM_EXITSIZEMOVE if !mini => post(Action::SaveSize),
            WM_CLOSE => {
                post(Action::Hide);
                return 0;
            }
            _ => {}
        }
        DefSubclassProc(hwnd, msg, wp, lp)
    }
}

fn panel_hit_test(x: i32, y: i32, width: i32, height: i32, dpi: u32, mini: bool) -> u32 {
    let scale = dpi.max(96) as f32 / 96.0;
    let (x, y, width, height) = (
        x as f32 / scale,
        y as f32 / scale,
        width as f32 / scale,
        height as f32 / scale,
    );
    if !mini {
        let hit = match (x < 7.0, x >= width - 7.0, y < 7.0, y >= height - 7.0) {
            (true, _, true, _) => HTTOPLEFT,
            (_, true, true, _) => HTTOPRIGHT,
            (true, _, _, true) => HTBOTTOMLEFT,
            (_, true, _, true) => HTBOTTOMRIGHT,
            (true, _, _, _) => HTLEFT,
            (_, true, _, _) => HTRIGHT,
            (_, _, true, _) => HTTOP,
            (_, _, _, true) => HTBOTTOM,
            _ => HTCLIENT,
        };
        if hit != HTCLIENT {
            return hit;
        }
    }
    let (header, button_y, button_right_offsets) = if mini {
        (42.0, 5.0, [78.0, 42.0])
    } else {
        (84.0, 28.0, [92.0, 52.0])
    };
    let over_button = y >= button_y
        && y < button_y + 32.0
        && button_right_offsets
            .iter()
            .any(|offset| x >= width - offset && x < width - offset + 32.0);
    // Let Windows own capture and the move loop. This works over the logo,
    // text and empty header alike, without a Slint pointer callback.
    if y >= 0.0 && y < header && !over_button {
        HTCAPTION
    } else {
        HTCLIENT
    }
}

pub fn center(window: &slint::Window) {
    unsafe {
        let monitor = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..std::mem::zeroed()
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return;
        }
        let mut size = window.size();
        let work = info.rcWork;
        size.width = size.width.min((work.right - work.left - 16).max(1) as u32);
        size.height = size.height.min((work.bottom - work.top - 16).max(1) as u32);
        window.set_size(size);
        window.set_position(slint::PhysicalPosition::new(
            work.left + (work.right - work.left - size.width as i32) / 2,
            work.top + (work.bottom - work.top - size.height as i32) / 2,
        ));
    }
}

fn cursor_rect() -> RECT {
    let mut point = POINT::default();
    unsafe {
        GetCursorPos(&mut point);
    }
    RECT {
        left: point.x,
        top: point.y,
        right: point.x + 1,
        bottom: point.y + 1,
    }
}

/// Screen rectangle of the tray icon, falling back to the cursor position
/// before the icon has reported its rect.
pub fn tray_anchor(tray: &tray_icon::TrayIcon) -> RECT {
    tray.rect()
        .map(|r| RECT {
            left: r.position.x as i32,
            top: r.position.y as i32,
            right: r.position.x as i32 + r.size.width as i32,
            bottom: r.position.y as i32 + r.size.height as i32,
        })
        .unwrap_or_else(cursor_rect)
}

pub fn window_rect(window: &slint::Window) -> RECT {
    let Some(handle) = hwnd(window) else {
        return cursor_rect();
    };
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(handle, &mut rect) } == 0 {
        cursor_rect()
    } else {
        rect
    }
}

/// A single pending presentation, cancelled by a newer request or Hide.
/// Slint's window future yields to native creation instead of guessing a delay.
#[derive(Default)]
pub struct Presenter {
    pending: Option<slint::JoinHandle<()>>,
}

impl Presenter {
    pub fn hide(&mut self, ui: &AppWindow) -> Result<(), String> {
        if let Some(pending) = self.pending.take() {
            pending.abort();
        }
        ui.hide().map_err(|e| e.to_string())
    }

    pub fn show(
        &mut self,
        ui: &AppWindow,
        size: slint::LogicalSize,
        anchor: Option<RECT>,
    ) -> Result<(), String> {
        // Release the software surface on every presentation, including an
        // already visible panel being collapsed by a duplicate launch.
        self.hide(ui)?;
        ui.window().set_minimized(false);
        ui.window().set_size(size);
        ui.show().map_err(|e| e.to_string())?;
        let weak = ui.as_weak();
        self.pending = Some(
            slint::spawn_local(async move {
                let Some(ui) = weak.upgrade() else { return };
                let result = async {
                    // Do not retain the Arc: it would keep a destroyed HWND alive.
                    drop(
                        ui.window()
                            .winit_window()
                            .await
                            .map_err(|e| e.to_string())?,
                    );
                    attach(ui.window(), ui.get_mini_mode())?;
                    // Native creation may select a different DPI than the old window.
                    ui.window().set_size(size);
                    if let Some(anchor) = anchor {
                        near_anchor(ui.window(), anchor);
                    } else {
                        center(ui.window());
                    }
                    ui.window().request_redraw();
                    Ok::<(), String>(())
                }
                .await;
                if result.is_err() {
                    ui.set_notice("无法显示面板，请从托盘退出后重试".into());
                }
            })
            .map_err(|e| e.to_string())?,
        );
        Ok(())
    }
}

/// Position within the tray icon's monitor, handling taskbars on any edge.
fn near_anchor(window: &slint::Window, anchor: RECT) {
    unsafe {
        let monitor = MonitorFromRect(&anchor, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..std::mem::zeroed()
        };
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return;
        }
        let work = info.rcWork;
        let width = (window.size().width as i32)
            .min(work.right - work.left - 16)
            .max(1);
        let height = (window.size().height as i32)
            .min(work.bottom - work.top - 16)
            .max(1);
        window.set_size(slint::PhysicalSize::new(width as u32, height as u32));
        let mut x = anchor.right - width;
        let mut y = anchor.top - height - 8;
        if anchor.bottom <= work.top {
            y = work.top + 8;
        }
        if anchor.right <= work.left {
            x = work.left + 8;
        }
        if anchor.left >= work.right {
            x = work.right - width - 8;
        }
        if y < work.top + 8 {
            y = anchor.bottom + 8;
        }
        x = x.clamp(work.left + 8, (work.right - width - 8).max(work.left + 8));
        y = y.clamp(work.top + 8, (work.bottom - height - 8).max(work.top + 8));
        window.set_position(slint::PhysicalPosition::new(x, y));
        if let Some(handle) = hwnd(window) {
            SetForegroundWindow(handle);
        }
    }
}

fn startup_path() -> Result<PathBuf, String> {
    unsafe {
        let path = SHGetKnownFolderPath(&FOLDERID_Startup, KF_FLAG_DEFAULT, None)
            .map_err(|e| e.to_string())?;
        let value = path.to_string().map_err(|e| e.to_string());
        CoTaskMemFree(Some(path.0.cast()));
        Ok(PathBuf::from(value?).join("Periph In One.lnk"))
    }
}

const APPROVED_PATH: &str =
    "Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\StartupApproved\\StartupFolder";

pub fn autostart_enabled() -> bool {
    let Ok(path) = startup_path() else {
        return false;
    };
    if !path.exists() {
        return false;
    }
    unsafe {
        // Validate the actual shortcut as well as Task Manager's approval state.
        let valid = (|| -> windows::core::Result<bool> {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            let file: IPersistFile = link.cast()?;
            file.Load(
                PCWSTR(wide(&path.to_string_lossy()).as_ptr()),
                windows::Win32::System::Com::STGM_READ,
            )?;
            let mut target = [0u16; 32768];
            link.GetPath(&mut target, null_mut(), 0)?;
            let n = target.iter().position(|&c| c == 0).unwrap_or(target.len());
            let actual = PathBuf::from(String::from_utf16_lossy(&target[..n]));
            Ok(std::env::current_exe().is_ok_and(|exe| {
                actual
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&exe.to_string_lossy())
            }))
        })()
        .unwrap_or(false);
        if !valid {
            return false;
        }
        let mut key = null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(APPROVED_PATH).as_ptr(),
            0,
            KEY_READ,
            &mut key,
        ) != ERROR_SUCCESS
        {
            return true;
        }
        let mut bytes = [0u8; 24];
        let mut len = bytes.len() as u32;
        let status = RegQueryValueExW(
            key,
            wide("Periph In One.lnk").as_ptr(),
            null(),
            null_mut(),
            bytes.as_mut_ptr(),
            &mut len,
        );
        RegCloseKey(key);
        status != ERROR_SUCCESS || len == 0 || !matches!(bytes[0], 3 | 7)
    }
}

fn create_startup_shortcut(
    path: &std::path::Path,
    exe: &std::path::Path,
) -> windows::core::Result<()> {
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(PCWSTR(wide(&exe.to_string_lossy()).as_ptr()))?;
        link.SetArguments(PCWSTR(wide("--autostart").as_ptr()))?;
        link.SetWorkingDirectory(PCWSTR(
            wide(&exe.parent().unwrap().to_string_lossy()).as_ptr(),
        ))?;
        link.SetDescription(PCWSTR(wide("Periph In One — 外设电量查看器").as_ptr()))?;
        let file: IPersistFile = link.cast()?;
        file.Save(PCWSTR(wide(&path.to_string_lossy()).as_ptr()), true)
    }
}

pub fn set_autostart(enabled: bool) -> Result<(), String> {
    let path = startup_path()?;
    if !enabled {
        if path.exists() {
            std::fs::remove_file(path).map_err(|e| e.to_string())?;
        }
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    create_startup_shortcut(&path, &exe).map_err(|e| e.to_string())?;
    unsafe {
        // Change only our own approval value after an explicit user toggle.
        let mut key = null_mut();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            wide(APPROVED_PATH).as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut key,
        ) == ERROR_SUCCESS
        {
            let status = RegDeleteValueW(key, wide("Periph In One.lnk").as_ptr());
            RegCloseKey(key);
            if status != ERROR_SUCCESS && status != ERROR_FILE_NOT_FOUND {
                return Err("自启快捷方式已创建，但任务管理器的禁用状态未能清除".into());
            }
        }
    }
    Ok(())
}

pub fn copy_text(text: &str) -> Result<(), String> {
    use windows_sys::Win32::System::{DataExchange::*, Memory::*};
    let bytes = wide(text);
    unsafe {
        if OpenClipboard(null_mut()) == 0 {
            return Err("剪贴板正忙，请重试".into());
        }
        let result = (|| {
            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes.len() * 2);
            if handle.is_null() {
                return Err("无法分配剪贴板内存".into());
            }
            let pointer = GlobalLock(handle);
            if pointer.is_null() {
                GlobalFree(handle);
                return Err("无法访问剪贴板内存".into());
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u16>(), bytes.len());
            GlobalUnlock(handle);
            EmptyClipboard();
            if SetClipboardData(13, handle).is_null() {
                GlobalFree(handle);
                return Err("无法复制到剪贴板".into());
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn application_resource_supplies_small_and_taskbar_icons_at_each_dpi() {
        for dpi in [96, 144, 192] {
            let (small, large) = application_icons(dpi).unwrap();
            for icon in [small, large] {
                unsafe {
                    let mut info = ICONINFO::default();
                    assert_ne!(GetIconInfo(icon, &mut info), 0);
                    assert!(!info.hbmColor.is_null());
                    DeleteObject(info.hbmColor);
                    if !info.hbmMask.is_null() {
                        DeleteObject(info.hbmMask);
                    }
                }
            }
        }
    }

    #[test]
    fn native_header_drag_preserves_buttons_content_and_resize_edges_at_each_dpi() {
        for dpi in [96, 144, 192] {
            let scale = dpi as f32 / 96.0;
            let hit = |x: i32, y: i32, width: i32, height: i32, mini| {
                panel_hit_test(
                    (x as f32 * scale) as i32,
                    (y as f32 * scale) as i32,
                    (width as f32 * scale) as i32,
                    (height as f32 * scale) as i32,
                    dpi,
                    mini,
                )
            };
            for width in [360, 590, 1040] {
                for (x, y) in [(40, 40), (150, 32), (width / 2, 72), (width - 20, 76)] {
                    assert_eq!(hit(x, y, width, 640, false), HTCAPTION);
                }
                for x in [width - 76, width - 36] {
                    assert_eq!(hit(x, 44, width, 640, false), HTCLIENT);
                }
                assert_eq!(hit(100, 120, width, 640, false), HTCLIENT);
                assert_eq!(hit(1, 100, width, 640, false), HTLEFT);
                assert_eq!(hit(100, 1, width, 640, false), HTTOP);
                assert_eq!(hit(width - 1, 639, width, 640, false), HTBOTTOMRIGHT);
            }
            assert_eq!(hit(40, 21, 280, 176, true), HTCAPTION);
            assert_eq!(hit(150, 21, 280, 176, true), HTCAPTION);
            assert_eq!(hit(218, 21, 280, 176, true), HTCLIENT);
            assert_eq!(hit(254, 21, 280, 176, true), HTCLIENT);
            assert_eq!(hit(100, 60, 280, 176, true), HTCLIENT);
        }
    }

    #[test]
    fn creates_shortcut_with_silent_start_argument_in_a_temporary_folder() {
        unsafe {
            windows::Win32::System::Com::CoInitializeEx(
                None,
                windows::Win32::System::Com::COINIT_APARTMENTTHREADED,
            )
            .ok()
            .unwrap();
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("Periph In One测试.lnk");
            let exe = std::env::current_exe().unwrap();
            create_startup_shortcut(&path, &exe).unwrap();
            assert!(path.is_file());
            let link: IShellLinkW =
                CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).unwrap();
            let file: IPersistFile = link.cast().unwrap();
            file.Load(
                PCWSTR(wide(&path.to_string_lossy()).as_ptr()),
                windows::Win32::System::Com::STGM_READ,
            )
            .unwrap();
            let mut target = [0u16; 32768];
            link.GetPath(&mut target, null_mut(), 0).unwrap();
            let length = target.iter().position(|&c| c == 0).unwrap();
            assert!(
                String::from_utf16_lossy(&target[..length])
                    .eq_ignore_ascii_case(&exe.to_string_lossy())
            );
            let mut arguments = [0u16; 64];
            link.GetArguments(&mut arguments).unwrap();
            let length = arguments.iter().position(|&c| c == 0).unwrap();
            assert_eq!(
                String::from_utf16_lossy(&arguments[..length]),
                "--autostart"
            );
            drop(file);
            drop(link);
            windows::Win32::System::Com::CoUninitialize();
        }
    }
}
