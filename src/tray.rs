use std::ffi::c_void;
use std::mem::size_of;
use std::path::PathBuf;
use std::ptr::{null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{CreateBitmap, DeleteObject, HBITMAP};
use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_SZ, RegCloseKey, RegDeleteValueW,
    RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows_sys::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_ERROR, NIIF_INFO, NIIF_NOSOUND, NIIF_WARNING,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION, NOTIFYICON_VERSION_4, NOTIFYICONDATAW,
    Shell_NotifyIconW, ShellExecuteW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyIcon, DestroyMenu, DispatchMessageW, GetCursorPos, GetMessageW, GetWindowLongPtrW,
    GWLP_USERDATA, HICON, HMENU, ICONINFO, KillTimer, MB_ICONEXCLAMATION, MB_ICONINFORMATION,
    MB_OK, MessageBoxW, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING, MSG, PostMessageW,
    PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetForegroundWindow, SetTimer,
    SetWindowLongPtrW, SW_SHOWNORMAL, TPM_BOTTOMALIGN, TPM_RIGHTBUTTON, TrackPopupMenu,
    TranslateMessage, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONDBLCLK, WM_NULL,
    WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_OVERLAPPED,
};

use crate::config::{Config, NotificationMode};
use crate::monitor::{Alert, AlertEngine, AlertKind, AlertSeverity, OverallStatus};
use crate::popup::{self, PopupToast};
use crate::sensors::{PollSnapshot, SensorSuite};

const TRAY_ICON_UID: u32 = 1;
const WM_TRAY: u32 = WM_APP + 1;
const TIMER_POLL_ID: usize = 1;
const TIMER_TEST_EXIT_ID: usize = 2;

const IDM_PAUSE: usize = 1001;
const IDM_TEST_NOTIFY: usize = 1002;
const IDM_OPEN_CONFIG: usize = 1003;
const IDM_RELOAD_CONFIG: usize = 1004;
const IDM_CPU_GUIDE: usize = 1005;
const IDM_AUTOSTART: usize = 1006;
const IDM_QUIT: usize = 1007;

const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE_NAME: &str = "ThermalGuard";

struct TrayState {
    hwnd: HWND,
    taskbar_created_msg: u32,
    config_path: PathBuf,
    sensors: SensorSuite,
    engine: AlertEngine,
    paused: bool,
    start_instant: Instant,
    last_snapshot: Option<PollSnapshot>,
    current_status: OverallStatus,
    current_icon: HICON,
    current_color: (u8, u8, u8),
    shown_startup_cpu_hint: bool,
    menu_open: bool,
    last_menu_closed: Option<Instant>,
    popup: Option<PopupToast>,
}

impl TrayState {
    fn new(hwnd: HWND, taskbar_created_msg: u32, config_path: PathBuf, config: Config) -> Self {
        let color = status_color(OverallStatus::Normal);
        let icon = create_status_icon(color);
        Self {
            hwnd,
            taskbar_created_msg,
            config_path,
            sensors: SensorSuite::new(),
            engine: AlertEngine::new(config),
            paused: false,
            start_instant: Instant::now(),
            last_snapshot: None,
            current_status: OverallStatus::Normal,
            current_icon: icon,
            current_color: color,
            shown_startup_cpu_hint: false,
            menu_open: false,
            last_menu_closed: None,
            popup: PopupToast::new(false).ok(),
        }
    }

    fn show_popup(&mut self, title: &str, body: &str, severity: AlertSeverity, play_sound: bool) {
        if self.popup.is_none() {
            self.popup = PopupToast::new(false).ok();
        }
        let (duration_ms, beep_on_critical) = {
            let cfg = self.engine.config();
            ((cfg.popup_duration_secs * 1000) as u32, cfg.beep_on_critical)
        };
        let want_sound = play_sound || (severity == AlertSeverity::Critical && beep_on_critical);
        if let Some(popup) = &mut self.popup {
            popup.show(title, body, severity, duration_ms, want_sound);
        }
    }

    fn deliver_notification(
        &mut self,
        title: &str,
        body: &str,
        severity: AlertSeverity,
        play_sound: bool,
    ) {
        let (mode, beep_on_critical) = {
            let cfg = self.engine.config();
            (cfg.notification_mode, cfg.beep_on_critical)
        };

        match mode {
            NotificationMode::Popup => {
                self.show_popup(title, body, severity, play_sound);
            }
            NotificationMode::Windows => {
                let _ = show_windows_balloon(
                    self.hwnd,
                    title,
                    body,
                    severity,
                    play_sound,
                    beep_on_critical,
                );
            }
            NotificationMode::Auto => {
                if show_windows_balloon(
                    self.hwnd,
                    title,
                    body,
                    severity,
                    play_sound,
                    beep_on_critical,
                )
                .is_err()
                {
                    self.show_popup(title, body, severity, play_sound);
                }
            }
        }
    }

    fn poll_and_update(&mut self) {
        let now_ms = self.start_instant.elapsed().as_millis() as u64;
        let snapshot = self.sensors.poll();

        let mut readings = Vec::with_capacity(2);
        if let Some(cpu) = &snapshot.cpu {
            readings.push(cpu.clone());
        }
        if let Some(gpu) = &snapshot.gpu {
            readings.push(gpu.clone());
        }

        if self.paused {
            self.current_status = OverallStatus::Paused;
            self.update_tray_appearance(&snapshot, OverallStatus::Paused);
            self.last_snapshot = Some(snapshot);
            return;
        }

        let (alerts, status) = self.engine.evaluate(now_ms, &readings);
        self.current_status = status;
        self.update_tray_appearance(&snapshot, status);

        if !alerts.is_empty() {
            self.emit_combined_alerts(&alerts);
        } else if !self.shown_startup_cpu_hint {
            self.shown_startup_cpu_hint = true;
            if snapshot.cpu.is_none() {
                if let Some(gpu) = &snapshot.gpu {
                    let body = format!(
                        "GPU monitoring active ({:.0}°C) — right-click tray icon for CPU temp setup.",
                        gpu.temp_c
                    );
                    self.deliver_notification("Thermal Guard", &body, AlertSeverity::Info, false);
                }
            }
        }

        self.last_snapshot = Some(snapshot);
    }

    fn emit_combined_alerts(&mut self, alerts: &[Alert]) {
        let all_cooldown = alerts.iter().all(|a| a.kind == AlertKind::CooledDown);
        let play_sound = self.engine.config().sound && !all_cooldown;

        if alerts.len() == 1 {
            let a = &alerts[0];
            self.deliver_notification(&a.title, &a.body, a.severity, play_sound);
            return;
        }

        let worst_severity = alerts
            .iter()
            .map(|a| a.severity)
            .max_by_key(|s| match s {
                AlertSeverity::Info => 0,
                AlertSeverity::Warning => 1,
                AlertSeverity::Critical => 2,
            })
            .unwrap_or(AlertSeverity::Warning);

        let title = match worst_severity {
            AlertSeverity::Critical => "CPU & GPU THERMAL ALERT!".to_string(),
            AlertSeverity::Warning => "CPU & GPU Thermal Warning".to_string(),
            AlertSeverity::Info => "CPU & GPU Cooled Down".to_string(),
        };

        let body = alerts
            .iter()
            .map(|a| format!("[{}] {}", a.sensor.label(), a.body))
            .collect::<Vec<_>>()
            .join("\n");

        self.deliver_notification(&title, &body, worst_severity, play_sound);
    }

    fn update_tray_appearance(&mut self, snapshot: &PollSnapshot, status: OverallStatus) {
        let target_color = status_color(status);
        let mut old_icon_to_destroy: Option<HICON> = None;

        if target_color != self.current_color {
            let new_icon = create_status_icon(target_color);
            if !new_icon.is_null() {
                old_icon_to_destroy = Some(self.current_icon);
                self.current_icon = new_icon;
                self.current_color = target_color;
            }
        }

        let tip = format_tooltip(snapshot, status);
        let mut nid = base_nid(self.hwnd);
        nid.uFlags = NIF_TIP | if old_icon_to_destroy.is_some() { NIF_ICON } else { 0 };
        nid.hIcon = self.current_icon;
        write_wide_buf(&mut nid.szTip, &tip);

        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            if let Some(old_icon) = old_icon_to_destroy {
                if !old_icon.is_null() {
                    let _ = DestroyIcon(old_icon);
                }
            }
        }
    }

    fn re_add_tray_icon(&self) {
        let tip = match &self.last_snapshot {
            Some(s) => format_tooltip(s, self.current_status),
            None => "Thermal Guard".to_string(),
        };
        let _ = add_tray_icon(self.hwnd, self.current_icon, &tip);
    }

    fn reload_config(&mut self) {
        let (cfg, warn) = Config::load_or_create(&self.config_path);
        let poll_ms = cfg.poll_interval_ms as u32;
        self.engine.update_config(cfg);
        unsafe {
            let _ = SetTimer(self.hwnd, TIMER_POLL_ID, poll_ms, None);
        }
        if let Some(msg) = warn {
            self.deliver_notification(
                "Thermal Guard — Config Warning",
                &msg,
                AlertSeverity::Warning,
                false,
            );
        } else {
            self.deliver_notification(
                "Thermal Guard",
                "Configuration reloaded.",
                AlertSeverity::Info,
                false,
            );
        }
        self.poll_and_update();
    }

    fn show_context_menu(&mut self) {
        if self.menu_open {
            return;
        }
        if let Some(closed_at) = self.last_menu_closed {
            if closed_at.elapsed() < Duration::from_millis(180) {
                return;
            }
        }
        self.menu_open = true;

        unsafe {
            let menu: HMENU = CreatePopupMenu();
            if menu.is_null() {
                self.menu_open = false;
                return;
            }

            let cpu_line = match self.last_snapshot.as_ref().and_then(|s| s.cpu.as_ref()) {
                Some(cpu) => format_reading_menu_line("CPU", cpu),
                None => "CPU: Setup needed (click guide below)".to_string(),
            };
            let gpu_line = match self.last_snapshot.as_ref().and_then(|s| s.gpu.as_ref()) {
                Some(gpu) => format_reading_menu_line("GPU", gpu),
                None => "GPU: Unavailable".to_string(),
            };

            append_menu_item(menu, MF_STRING | MF_GRAYED, 0, &cpu_line);
            append_menu_item(menu, MF_STRING | MF_GRAYED, 0, &gpu_line);
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, null());

            let pause_flags = MF_STRING | if self.paused { MF_CHECKED } else { 0 };
            append_menu_item(menu, pause_flags, IDM_PAUSE, "Pause Alerts");
            append_menu_item(menu, MF_STRING, IDM_TEST_NOTIFY, "Send Test Notification");
            append_menu_item(menu, MF_STRING, IDM_OPEN_CONFIG, "Open Config (config.toml)");
            append_menu_item(menu, MF_STRING, IDM_RELOAD_CONFIG, "Reload Config");
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, null());

            append_menu_item(menu, MF_STRING, IDM_CPU_GUIDE, "CPU Sensor Setup Guide");
            let autostart_flags = MF_STRING | if is_autostart_enabled() { MF_CHECKED } else { 0 };
            append_menu_item(menu, autostart_flags, IDM_AUTOSTART, "Start with Windows");
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, null());
            append_menu_item(menu, MF_STRING, IDM_QUIT, "Quit");

            let mut pt = POINT { x: 0, y: 0 };
            let _ = GetCursorPos(&mut pt);
            let _ = SetForegroundWindow(self.hwnd);
            let _ = TrackPopupMenu(
                menu,
                TPM_RIGHTBUTTON | TPM_BOTTOMALIGN,
                pt.x,
                pt.y,
                0,
                self.hwnd,
                null(),
            );
            let _ = PostMessageW(self.hwnd, WM_NULL, 0, 0);
            let _ = DestroyMenu(menu);
        }

        self.menu_open = false;
        self.last_menu_closed = Some(Instant::now());
    }

    fn handle_command(&mut self, cmd_id: usize) {
        match cmd_id {
            IDM_PAUSE => {
                self.paused = !self.paused;
                if !self.paused {
                    self.engine.reset();
                }
                self.poll_and_update();
            }
            IDM_TEST_NOTIFY => {
                let sound = self.engine.config().sound;
                self.deliver_notification(
                    "Thermal Guard — Test Alert",
                    "Notifications are working! You will be alerted on overheating, rapid temperature spikes, or thermal throttling.",
                    AlertSeverity::Warning,
                    sound,
                );
            }
            IDM_OPEN_CONFIG => {
                let _ = Config::load_or_create(&self.config_path);
                open_path_in_shell(self.hwnd, &self.config_path);
            }
            IDM_RELOAD_CONFIG => {
                self.reload_config();
            }
            IDM_CPU_GUIDE => {
                show_cpu_setup_guide(self.hwnd, self.last_snapshot.as_ref());
            }
            IDM_AUTOSTART => {
                if let Err(err) = toggle_autostart() {
                    self.deliver_notification(
                        "Thermal Guard — Autostart Error",
                        &err,
                        AlertSeverity::Warning,
                        false,
                    );
                }
            }
            IDM_QUIT => unsafe {
                PostQuitMessage(0);
            },
            _ => {}
        }
    }
}

impl Drop for TrayState {
    fn drop(&mut self) {
        remove_tray_icon(self.hwnd);
        if !self.current_icon.is_null() {
            unsafe {
                let _ = DestroyIcon(self.current_icon);
            }
            self.current_icon = null_mut();
        }
    }
}

/// Runs the persistent Windows system tray monitoring loop until Quit.
pub fn run_tray_loop(config_path: PathBuf, config: Config, config_warn: Option<String>) -> Result<(), u32> {
    let hinstance = unsafe { GetModuleHandleW(null()) } as HINSTANCE;
    let class_name = to_wide_nul("ThermalGuardTrayWindow");
    register_window_class(hinstance, &class_name, Some(tray_wnd_proc))?;

    let title = to_wide_nul("Thermal Guard");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        )
    };
    if hwnd.is_null() {
        return Err(unsafe { GetLastError() });
    }

    let taskbar_msg_name = to_wide_nul("TaskbarCreated");
    let taskbar_created_msg = unsafe { RegisterWindowMessageW(taskbar_msg_name.as_ptr()) };

    let poll_ms = config.poll_interval_ms as u32;
    let state = Box::new(TrayState::new(hwnd, taskbar_created_msg, config_path, config));

    if let Err(code) = add_tray_icon(hwnd, state.current_icon, "Thermal Guard — Initializing...") {
        return Err(code);
    }

    let state_ptr = Box::into_raw(state);
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
    }

    // Run initial poll immediately, then schedule periodic timer.
    unsafe {
        (*state_ptr).poll_and_update();
        if let Some(warn) = config_warn {
            (*state_ptr).deliver_notification(
                "Thermal Guard — Config Warning",
                &warn,
                AlertSeverity::Warning,
                false,
            );
        }
        let _ = SetTimer(hwnd, TIMER_POLL_ID, poll_ms, None);
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = KillTimer(hwnd, TIMER_POLL_ID);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        drop(Box::from_raw(state_ptr));
    }

    Ok(())
}

/// Sends a single test notification using `mode` (`auto` falls back to built-in popup).
pub fn run_test_notification(mode: NotificationMode, sound: bool) -> Result<&'static str, u32> {
    match mode {
        NotificationMode::Popup => {
            popup::run_test_popup(2000, sound)?;
            Ok("built-in floating popup")
        }
        NotificationMode::Windows => {
            run_windows_test_balloon(sound)?;
            Ok("Windows tray balloon")
        }
        NotificationMode::Auto => match run_windows_test_balloon(sound) {
            Ok(()) => Ok("Windows tray balloon"),
            Err(_) => {
                popup::run_test_popup(2000, sound)?;
                Ok("built-in floating popup (Windows notification service unavailable)")
            }
        },
    }
}

fn run_windows_test_balloon(sound: bool) -> Result<(), u32> {
    let hinstance = unsafe { GetModuleHandleW(null()) } as HINSTANCE;
    let class_name = to_wide_nul("ThermalGuardTestNotifyWindow");
    register_window_class(hinstance, &class_name, Some(test_wnd_proc))?;

    let title = to_wide_nul("Thermal Guard Test");
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class_name.as_ptr(),
            title.as_ptr(),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            hinstance,
            null(),
        )
    };
    if hwnd.is_null() {
        return Err(unsafe { GetLastError() });
    }

    let icon = create_status_icon(status_color(OverallStatus::Warning));
    if let Err(code) = add_tray_icon(hwnd, icon, "Thermal Guard — Test") {
        if !icon.is_null() {
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
        return Err(code);
    }

    let notify_res = show_windows_balloon(
        hwnd,
        "Thermal Guard — Test Alert",
        "Thermal Guard notifications are active! You will be notified if CPU or GPU overheats, heats up quickly, or throttles.",
        AlertSeverity::Warning,
        sound,
        false,
    );

    if notify_res.is_ok() {
        unsafe {
            let _ = SetTimer(hwnd, TIMER_TEST_EXIT_ID, 400, None);
            let mut msg = MSG::default();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            let _ = KillTimer(hwnd, TIMER_TEST_EXIT_ID);
        }
    }

    remove_tray_icon(hwnd);
    if !icon.is_null() {
        unsafe {
            let _ = DestroyIcon(icon);
        }
    }

    notify_res
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut TrayState;
    if !ptr.is_null() {
        let state = unsafe { &mut *ptr };
        if state.taskbar_created_msg != 0 && msg == state.taskbar_created_msg {
            state.re_add_tray_icon();
            return 0;
        }
        match msg {
            WM_TIMER if wparam == TIMER_POLL_ID => {
                state.poll_and_update();
                return 0;
            }
            WM_TRAY => {
                let event = (lparam as u32) & 0xFFFF;
                if event == WM_RBUTTONUP || event == WM_CONTEXTMENU {
                    state.show_context_menu();
                } else if event == WM_LBUTTONDBLCLK {
                    let summary = match &state.last_snapshot {
                        Some(s) => format_tooltip(s, state.current_status),
                        None => "Collecting sensor readings...".to_string(),
                    };
                    state.deliver_notification(
                        "Thermal Guard — Status",
                        &summary,
                        AlertSeverity::Info,
                        false,
                    );
                }
                return 0;
            }
            WM_COMMAND => {
                let cmd_id = (wparam & 0xFFFF) as usize;
                state.handle_command(cmd_id);
                return 0;
            }
            WM_DESTROY => {
                unsafe { PostQuitMessage(0) };
                return 0;
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

unsafe extern "system" fn test_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER if wparam == TIMER_TEST_EXIT_ID => {
            unsafe { PostQuitMessage(0) };
            0
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn register_window_class(
    hinstance: HINSTANCE,
    class_name: &[u16],
    wnd_proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
) -> Result<(), u32> {
    let wc = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: wnd_proc,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinstance,
        hIcon: null_mut(),
        hCursor: null_mut(),
        hbrBackground: null_mut(),
        lpszMenuName: null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: null_mut(),
    };
    let atom = unsafe { RegisterClassExW(&wc) };
    if atom == 0 {
        let err = unsafe { GetLastError() };
        // ERROR_CLASS_ALREADY_EXISTS (1410) is benign.
        if err != 1410 {
            return Err(err);
        }
    }
    Ok(())
}

fn base_nid(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW::default();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_ICON_UID;
    nid
}

fn add_tray_icon(hwnd: HWND, icon: HICON, tip: &str) -> Result<(), u32> {
    let mut nid = base_nid(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    write_wide_buf(&mut nid.szTip, tip);

    if unsafe { Shell_NotifyIconW(NIM_ADD, &nid) } == 0 {
        return Err(unsafe { GetLastError() });
    }

    nid.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    let _ = unsafe { Shell_NotifyIconW(NIM_SETVERSION, &nid) };
    Ok(())
}

fn remove_tray_icon(hwnd: HWND) {
    let nid = base_nid(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

fn show_windows_balloon(
    hwnd: HWND,
    title: &str,
    body: &str,
    severity: AlertSeverity,
    sound: bool,
    beep_on_critical: bool,
) -> Result<(), u32> {
    let mut nid = base_nid(hwnd);
    nid.uFlags = NIF_INFO;
    write_wide_buf(&mut nid.szInfoTitle, title);
    write_wide_buf(&mut nid.szInfo, body);

    let mut flags = match severity {
        AlertSeverity::Info => NIIF_INFO,
        AlertSeverity::Warning => NIIF_WARNING,
        AlertSeverity::Critical => NIIF_ERROR,
    };
    if !sound {
        flags |= NIIF_NOSOUND;
    }
    nid.dwInfoFlags = flags;

    if unsafe { Shell_NotifyIconW(NIM_MODIFY, &nid) } == 0 {
        return Err(unsafe { GetLastError() });
    }

    if severity == AlertSeverity::Critical && beep_on_critical {
        unsafe {
            let _ = MessageBeep(MB_ICONEXCLAMATION);
        }
    }

    Ok(())
}

fn status_color(status: OverallStatus) -> (u8, u8, u8) {
    match status {
        OverallStatus::Normal => (34, 197, 94),
        OverallStatus::Warning | OverallStatus::RapidRise => (245, 158, 11),
        OverallStatus::Critical => (239, 68, 68),
        OverallStatus::Paused => (148, 163, 184),
    }
}

/// Generates a crisp 16x16 antialiased status badge `HICON` without leaking GDI bitmaps.
fn create_status_icon(rgb: (u8, u8, u8)) -> HICON {
    let mut bgra = [0u8; 16 * 16 * 4];
    let (r, g, b) = rgb;

    for y in 0..16 {
        for x in 0..16 {
            let dx = x as f32 - 7.5;
            let dy = y as f32 - 7.5;
            let dist = (dx * dx + dy * dy).sqrt();
            let idx = (y * 16 + x) * 4;

            if dist <= 6.2 {
                // Inner filled badge with subtle top-left highlight
                let boost = if dx < -1.0 && dy < -1.0 { 22 } else { 0 };
                bgra[idx] = b.saturating_add(boost);
                bgra[idx + 1] = g.saturating_add(boost);
                bgra[idx + 2] = r.saturating_add(boost);
                bgra[idx + 3] = 255;
            } else if dist <= 7.4 {
                // Dark border ring with antialiased outer edge
                let alpha = ((7.4 - dist) / 1.2).clamp(0.0, 1.0);
                bgra[idx] = 24;
                bgra[idx + 1] = 24;
                bgra[idx + 2] = 28;
                bgra[idx + 3] = (alpha * 255.0).round() as u8;
            }
        }
    }

    let and_mask = [0u8; 32];
    unsafe {
        let color_bmp: HBITMAP = CreateBitmap(16, 16, 1, 32, bgra.as_ptr() as *const c_void);
        let mask_bmp: HBITMAP = CreateBitmap(16, 16, 1, 1, and_mask.as_ptr() as *const c_void);
        if color_bmp.is_null() || mask_bmp.is_null() {
            if !color_bmp.is_null() {
                let _ = DeleteObject(color_bmp);
            }
            if !mask_bmp.is_null() {
                let _ = DeleteObject(mask_bmp);
            }
            return null_mut();
        }

        let icon_info = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask_bmp,
            hbmColor: color_bmp,
        };
        let hicon = CreateIconIndirect(&icon_info);
        let _ = DeleteObject(color_bmp);
        let _ = DeleteObject(mask_bmp);
        hicon
    }
}

fn format_tooltip(snapshot: &PollSnapshot, status: OverallStatus) -> String {
    let cpu_str = match &snapshot.cpu {
        Some(c) if c.is_throttling => format!("{:.0}°C!", c.temp_c),
        Some(c) => format!("{:.0}°C", c.temp_c),
        None => "Setup needed".to_string(),
    };
    let gpu_str = match &snapshot.gpu {
        Some(g) if g.is_throttling => format!("{:.0}°C!", g.temp_c),
        Some(g) => format!("{:.0}°C", g.temp_c),
        None => "N/A".to_string(),
    };
    format!(
        "Thermal Guard\nCPU: {cpu_str} | GPU: {gpu_str}\nStatus: {}",
        status.label()
    )
}

fn format_reading_menu_line(prefix: &str, r: &crate::monitor::SensorReading) -> String {
    let limit = r
        .throttle_limit_c
        .map(|l| format!(" / {l:.0}°C limit"))
        .unwrap_or_default();
    let throttle = if r.is_throttling { " [THROTTLING]" } else { "" };
    format!("{prefix}: {:.0}°C{limit}{throttle} ({})", r.temp_c, r.source)
}

fn show_cpu_setup_guide(hwnd: HWND, snapshot: Option<&PollSnapshot>) {
    let current = snapshot
        .map(|s| format!("Current CPU backend: {}\nNote: {}\n\n", s.cpu_backend, s.cpu_status_hint))
        .unwrap_or_default();
    let text = format!(
        "{current}Windows desktop motherboards often omit ACPI thermal zones, so reading Intel CPU package/core temps requires one of:\n\n\
         Option 1 (Standalone, Recommended):\n\
         1. Open terminal and run:\n\
            winget install -e --id namazso.PawnIO\n\
         2. Run thermal-guard.exe as Administrator (or run scripts\\setup-autostart-elevated.ps1 once).\n\n\
         Option 2 (No Admin Needed for Thermal Guard):\n\
         Run HWiNFO64 (with Shared Memory Support enabled) or Core Temp in the background — Thermal Guard detects it automatically within 10 seconds."
    );
    let text_w = to_wide_nul(&text);
    let cap_w = to_wide_nul("Thermal Guard — CPU Sensor Setup Guide");
    unsafe {
        let _ = MessageBoxW(hwnd, text_w.as_ptr(), cap_w.as_ptr(), MB_OK | MB_ICONINFORMATION);
    }
}

fn append_menu_item(menu: HMENU, flags: u32, id: usize, label: &str) {
    let wide = to_wide_nul(label);
    unsafe {
        let _ = AppendMenuW(menu, flags, id, wide.as_ptr());
    }
}

fn open_path_in_shell(hwnd: HWND, path: &PathBuf) {
    let op = to_wide_nul("open");
    let file = to_wide_nul(&path.to_string_lossy());
    unsafe {
        let _ = ShellExecuteW(hwnd, op.as_ptr(), file.as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

fn current_exe_quoted() -> Option<String> {
    let mut buf = vec![0u16; 512];
    let len = unsafe { GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) };
    if len == 0 || len as usize >= buf.len() {
        return None;
    }
    let path = String::from_utf16_lossy(&buf[..len as usize]);
    Some(format!("\"{path}\""))
}

fn is_autostart_enabled() -> bool {
    let key_w = to_wide_nul(RUN_KEY_PATH);
    let val_w = to_wide_nul(RUN_VALUE_NAME);
    let mut hkey: HKEY = null_mut();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_READ, &mut hkey) };
    if status != 0 {
        return false;
    }
    let mut cb_data: u32 = 0;
    let q = unsafe {
        RegQueryValueExW(
            hkey,
            val_w.as_ptr(),
            null(),
            null_mut(),
            null_mut(),
            &mut cb_data,
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    q == 0 && cb_data > 0
}

fn toggle_autostart() -> Result<(), String> {
    let key_w = to_wide_nul(RUN_KEY_PATH);
    let val_w = to_wide_nul(RUN_VALUE_NAME);
    let mut hkey: HKEY = null_mut();
    let status = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key_w.as_ptr(), 0, KEY_READ | KEY_WRITE, &mut hkey) };
    if status != 0 {
        return Err(format!("Failed to open Run registry key (Win32 {status})"));
    }

    let enabled = is_autostart_enabled();
    let res = if enabled {
        let del = unsafe { RegDeleteValueW(hkey, val_w.as_ptr()) };
        if del == 0 {
            Ok(())
        } else {
            Err(format!("Failed to remove Run entry (Win32 {del})"))
        }
    } else if let Some(cmd) = current_exe_quoted() {
        let cmd_w = to_wide_nul(&cmd);
        let byte_len = (cmd_w.len() * size_of::<u16>()) as u32;
        let set = unsafe {
            RegSetValueExW(
                hkey,
                val_w.as_ptr(),
                0,
                REG_SZ,
                cmd_w.as_ptr() as *const u8,
                byte_len,
            )
        };
        if set == 0 {
            Ok(())
        } else {
            Err(format!("Failed to write Run entry (Win32 {set})"))
        }
    } else {
        Err("Could not determine executable path".to_string())
    };

    unsafe {
        let _ = RegCloseKey(hkey);
    }
    res
}

fn to_wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn write_wide_buf(dst: &mut [u16], s: &str) {
    dst.fill(0);
    if dst.is_empty() {
        return;
    }
    let max_units = dst.len() - 1;
    for (i, unit) in s.encode_utf16().take(max_units).enumerate() {
        dst[i] = unit;
    }
}
