//! Finding what OBS's own capture sources capture: the window a Game Capture
//! hooked or a Window Capture shows, the display a Display Capture shows.
//!
//! Nothing here may wait on another thread: it runs on OBS's graphics
//! thread. So OBS's own windows are never looked at (reading the title of a
//! window of one's own process asks the thread that owns it, OBS's interface
//! thread, which may be waiting on the graphics thread: that froze OBS), and
//! titles are read with `InternalGetWindowText`, which never asks.

use std::collections::HashMap;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{DISPLAY_DEVICEW, EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MONITORINFOEXW, MonitorFromPoint};
use windows::Win32::System::Threading::{GetCurrentProcessId, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetClassNameW, GetWindowThreadProcessId, InternalGetWindowText, IsWindow, IsWindowVisible};
use windows::core::{BOOL, PCWSTR, PWSTR};

/// A top-level window, as OBS names windows: its title, class and program
/// (the executable's file name).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WindowName {
    pub title: String,
    pub class: String,
    pub exe: String,
}

impl WindowName {
    /// A Window Capture's `window` setting: `title:class:exe`, each with
    /// `:` written `#3A` and `#` written `#22` (OBS's `encode_dstr`).
    pub fn from_setting(setting: &str) -> Option<Self> {
        let mut parts = setting.split(':');
        let (title, class, exe) = (parts.next()?, parts.next()?, parts.next()?);
        let decode = |s: &str| s.replace("#3A", ":").replace("#22", "#");
        Some(Self { title: decode(title), class: decode(class), exe: decode(exe) })
    }
}

/// A window found, with its process, to tell later whether it's still the
/// same one ([`Found::still_there`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Found {
    pub window: isize,
    pub pid: u32,
}

impl Found {
    /// The window is still there, and still its program's.
    pub fn still_there(&self) -> bool {
        let hwnd = HWND(self.window as *mut _);
        unsafe { IsWindow(Some(hwnd)) }.as_bool() && pid_of(hwnd) == self.pid
    }
}

/// The visible top-level window best matching `name`, another program's:
/// the program must be the same, then the same class and title, else the
/// same class, else the same title, else any of the program's windows.
pub fn window(name: &WindowName) -> Option<Found> {
    let me = unsafe { GetCurrentProcessId() };
    let mut exes: HashMap<u32, Option<String>> = HashMap::new();
    let mut best: Option<(u32, Found)> = None;
    for hwnd in windows_shown() {
        let pid = pid_of(hwnd);
        if pid == 0 || pid == me {
            continue;
        }
        let exe = exes.entry(pid).or_insert_with(|| exe_of(pid));
        if !exe.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&name.exe)) {
            continue;
        }
        let score = match (class_of(hwnd) == name.class, title_of(hwnd) == name.title) {
            (true, true) => 3,
            (true, false) => 2,
            (false, true) => 1,
            (false, false) => 0,
        };
        if best.is_none_or(|(s, _)| score > s) {
            best = Some((score, Found { window: hwnd.0 as isize, pid }));
        }
    }
    best.map(|(_, found)| found)
}

/// Every visible top-level window.
fn windows_shown() -> Vec<HWND> {
    unsafe extern "system" fn each(hwnd: HWND, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<isize>) };
        if unsafe { IsWindowVisible(hwnd) }.as_bool() {
            list.push(hwnd.0 as isize);
        }
        BOOL(1)
    }
    let mut handles: Vec<isize> = Vec::new();
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut handles as *mut Vec<isize> as isize)) };
    handles.into_iter().map(|h| HWND(h as *mut _)).collect()
}

fn pid_of(hwnd: HWND) -> u32 {
    let mut pid = 0u32;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    pid
}

/// A window's title, as Windows keeps it: never asks the window's thread.
fn title_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe { InternalGetWindowText(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

fn class_of(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let n = unsafe { GetClassNameW(hwnd, &mut buf) }.max(0) as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// The file name of a process's program.
fn exe_of(pid: u32) -> Option<String> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    let got = unsafe { QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len) };
    let _ = unsafe { CloseHandle(process) };
    got.ok()?;
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Some(path.rsplit(['\\', '/']).next().unwrap_or(&path).to_owned())
}

/// The display a Display Capture shows, by its `monitor_id` setting (the
/// display's device interface name, as Windows' `EnumDisplayDevices` gives
/// it); the main display if that's empty or none matches.
pub fn display(monitor_id: &str) -> isize {
    unsafe extern "system" fn each(monitor: HMONITOR, _: HDC, _: *mut RECT, list: LPARAM) -> BOOL {
        let list = unsafe { &mut *(list.0 as *mut Vec<isize>) };
        list.push(monitor.0 as isize);
        BOOL(1)
    }
    let mut monitors: Vec<isize> = Vec::new();
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(each), LPARAM(&mut monitors as *mut Vec<isize> as isize)) };
    let wanted = monitor_id.trim();
    if !wanted.is_empty() {
        for m in &monitors {
            if interface_name(HMONITOR(*m as *mut _)).is_some_and(|id| id.eq_ignore_ascii_case(wanted)) {
                return *m;
            }
        }
    }
    unsafe { MonitorFromPoint(Default::default(), MONITOR_DEFAULTTOPRIMARY) }.0 as isize
}

/// A display's device interface name (`\\?\DISPLAY#…`).
fn interface_name(monitor: HMONITOR) -> Option<String> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if !unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) }.as_bool() {
        return None;
    }
    let mut device = DISPLAY_DEVICEW { cb: std::mem::size_of::<DISPLAY_DEVICEW>() as u32, ..Default::default() };
    // EDD_GET_DEVICE_INTERFACE_NAME: the interface name, not the PnP id.
    if !unsafe { EnumDisplayDevicesW(PCWSTR(info.szDevice.as_ptr()), 0, &mut device, 1) }.as_bool() {
        return None;
    }
    let end = device.DeviceID.iter().position(|&c| c == 0).unwrap_or(device.DeviceID.len());
    Some(String::from_utf16_lossy(&device.DeviceID[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_setting() {
        let name = WindowName::from_setting("osu!  (#3A ranked#3A):osu!stable:osu!.exe").unwrap();
        assert_eq!(name, WindowName { title: "osu!  (: ranked:)".into(), class: "osu!stable".into(), exe: "osu!.exe".into() });
        let name = WindowName::from_setting("Price #22 1:Cls:a.exe").unwrap();
        assert_eq!(name.title, "Price # 1");
        assert_eq!(WindowName::from_setting(""), None);
    }

    /// Looking for a window never waits on a window of its own process
    /// whose thread is busy, as OBS's interface thread can be while the
    /// graphics thread looks (that froze OBS). The window here is tiny, off
    /// every display, and never takes focus.
    #[test]
    fn never_waits_on_its_own_windows() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::core::w;

        let (made, window_made) = mpsc::channel();
        let (release, hold) = mpsc::channel::<()>();
        let owner = std::thread::spawn(move || unsafe {
            let instance = GetModuleHandleW(None).unwrap();
            unsafe extern "system" fn proc(hwnd: HWND, msg: u32, wp: windows::Win32::Foundation::WPARAM, lp: LPARAM) -> windows::Win32::Foundation::LRESULT {
                unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
            }
            let class = WNDCLASSW { lpfnWndProc: Some(proc), hInstance: instance.into(), lpszClassName: w!("hasteclip_obs_test"), ..Default::default() };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("hasteclip_obs_test"),
                w!("busy window"),
                WS_POPUP,
                -32000,
                -32000,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )
            .unwrap();
            // Shown without taking focus.
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
            made.send(hwnd.0 as isize).unwrap();
            // Busy: no messages handled till released.
            let _ = hold.recv();
            let _ = DestroyWindow(hwnd);
        });
        let window_made_hwnd = window_made.recv().unwrap();
        let exe = std::env::current_exe().unwrap().file_name().unwrap().to_string_lossy().into_owned();
        let started = Instant::now();
        let found = window(&WindowName { title: "busy window".into(), class: "hasteclip_obs_test".into(), exe });
        let took = started.elapsed();
        // What froze OBS: GetWindowTextW on a window of one's own process
        // waits for its thread to answer, and this one's busy.
        let hwnd = window_made_hwnd;
        let (asked, answered) = mpsc::channel();
        let asker = std::thread::spawn(move || {
            let mut buf = [0u16; 64];
            unsafe { GetWindowTextW(HWND(hwnd as *mut _), &mut buf) };
            let _ = asked.send(());
        });
        let waited = answered.recv_timeout(Duration::from_secs(1)).is_err();
        release.send(()).unwrap();
        owner.join().unwrap();
        asker.join().unwrap();
        assert!(waited, "GetWindowTextW answered at once: the busy window isn't what froze OBS");
        assert!(took < Duration::from_secs(2), "took {took:?}");
        assert_eq!(found, None, "its own process's windows are left out");
    }
}
