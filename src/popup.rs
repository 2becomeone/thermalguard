use std::mem::size_of;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_PITCH, DT_CALCRECT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX,
    DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FF_DONTCARE, FW_NORMAL, FW_SEMIBOLD,
    FillRect, FrameRect, GetDC, HBRUSH, HDC, HFONT, HGDIOBJ, InvalidateRect, OUT_DEFAULT_PRECIS,
    PAINTSTRUCT, ReleaseDC, SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
};
use windows_sys::Win32::System::Diagnostics::Debug::MessageBeep;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetMessageW,
    GetWindowLongPtrW, HWND_TOPMOST, IDC_HAND, KillTimer, LWA_ALPHA, LoadCursorW, MB_ICONASTERISK, MB_ICONEXCLAMATION,
    MSG, PostQuitMessage, RegisterClassExW, SPI_GETWORKAREA, SW_HIDE, SWP_NOACTIVATE,
    SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, SystemParametersInfoW, TranslateMessage, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONUP,
    WM_PAINT, WM_RBUTTONUP, WM_TIMER, WNDCLASSEXW, WS_EX_LAYERED, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use crate::monitor::AlertSeverity;

const POPUP_WIDTH: i32 = 380;
const POPUP_MIN_HEIGHT: i32 = 108;
const POPUP_MAX_HEIGHT: i32 = 190;
const POPUP_MARGIN_SCREEN: i32 = 16;
const ACCENT_BAR_WIDTH: i32 = 6;
const TEXT_LEFT_PAD: i32 = ACCENT_BAR_WIDTH + 14;
const TEXT_RIGHT_PAD: i32 = 14;
const TITLE_TOP_PAD: i32 = 12;
const TITLE_HEIGHT: i32 = 24;
const BODY_TOP_PAD: i32 = TITLE_TOP_PAD + TITLE_HEIGHT + 4;
const FOOTER_HEIGHT: i32 = 18;
const FOOTER_BOTTOM_PAD: i32 = 10;
const TIMER_AUTOHIDE_ID: usize = 101;

const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}

const COLOR_BG: u32 = rgb(24, 24, 27);
const COLOR_BORDER: u32 = rgb(63, 63, 70);
const COLOR_TITLE: u32 = rgb(250, 250, 250);
const COLOR_BODY: u32 = rgb(212, 212, 216);
const COLOR_FOOTER: u32 = rgb(138, 138, 148);
const COLOR_CRITICAL: u32 = rgb(239, 68, 68);
const COLOR_WARNING: u32 = rgb(245, 158, 11);
const COLOR_INFO: u32 = rgb(34, 197, 94);

struct PopupState {
    title: String,
    body: String,
    severity: AlertSeverity,
    title_font: HFONT,
    body_font: HFONT,
    footer_font: HFONT,
    post_quit_on_hide: bool,
}

impl PopupState {
    fn new(post_quit_on_hide: bool) -> Self {
        let face = to_wide_nul("Segoe UI");
        let title_font = unsafe {
            CreateFontW(
                -17,
                0,
                0,
                0,
                FW_SEMIBOLD as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                face.as_ptr(),
            )
        };
        let body_font = unsafe {
            CreateFontW(
                -15,
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                face.as_ptr(),
            )
        };
        let footer_font = unsafe {
            CreateFontW(
                -12,
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                (DEFAULT_PITCH | FF_DONTCARE) as u32,
                face.as_ptr(),
            )
        };

        Self {
            title: String::new(),
            body: String::new(),
            severity: AlertSeverity::Info,
            title_font,
            body_font,
            footer_font,
            post_quit_on_hide,
        }
    }

    fn measure_height(&self, hwnd: HWND) -> i32 {
        let body_w: Vec<u16> = self.body.encode_utf16().collect();
        if body_w.is_empty() {
            return POPUP_MIN_HEIGHT;
        }

        let hdc = unsafe { GetDC(hwnd) };
        if hdc.is_null() {
            return POPUP_MIN_HEIGHT;
        }

        let old_font: HGDIOBJ = if !self.body_font.is_null() {
            unsafe { SelectObject(hdc, self.body_font as HGDIOBJ) }
        } else {
            null_mut()
        };

        let mut rc = RECT {
            left: 0,
            top: 0,
            right: POPUP_WIDTH - TEXT_LEFT_PAD - TEXT_RIGHT_PAD,
            bottom: 0,
        };
        unsafe {
            let _ = DrawTextW(
                hdc,
                body_w.as_ptr(),
                body_w.len() as i32,
                &mut rc,
                DT_CALCRECT | DT_LEFT | DT_WORDBREAK | DT_NOPREFIX,
            );
            if !old_font.is_null() {
                let _ = SelectObject(hdc, old_font);
            }
            let _ = ReleaseDC(hwnd, hdc);
        }

        let body_height = (rc.bottom - rc.top).max(20);
        let total = BODY_TOP_PAD + body_height + 10 + FOOTER_HEIGHT + FOOTER_BOTTOM_PAD;
        total.clamp(POPUP_MIN_HEIGHT, POPUP_MAX_HEIGHT)
    }

    fn paint(&self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let hdc: HDC = unsafe { BeginPaint(hwnd, &mut ps) };
        if hdc.is_null() {
            return;
        }

        let mut client = RECT::default();
        unsafe {
            let _ = windows_sys::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);
        }

        let accent_color = match self.severity {
            AlertSeverity::Critical => COLOR_CRITICAL,
            AlertSeverity::Warning => COLOR_WARNING,
            AlertSeverity::Info => COLOR_INFO,
        };

        unsafe {
            let bg_brush: HBRUSH = CreateSolidBrush(COLOR_BG);
            let border_brush: HBRUSH = CreateSolidBrush(COLOR_BORDER);
            let accent_brush: HBRUSH = CreateSolidBrush(accent_color);

            if !bg_brush.is_null() {
                let _ = FillRect(hdc, &client, bg_brush);
            }
            if !accent_brush.is_null() {
                let bar = RECT {
                    left: 0,
                    top: 0,
                    right: ACCENT_BAR_WIDTH,
                    bottom: client.bottom,
                };
                let _ = FillRect(hdc, &bar, accent_brush);
            }
            if !border_brush.is_null() {
                let _ = FrameRect(hdc, &client, border_brush);
            }

            let _ = SetBkMode(hdc, TRANSPARENT as i32);

            let mut orig_font: HGDIOBJ = null_mut();
            if !self.title_font.is_null() {
                orig_font = SelectObject(hdc, self.title_font as HGDIOBJ);
            }
            let _ = SetTextColor(hdc, COLOR_TITLE);
            let mut title_rc = RECT {
                left: TEXT_LEFT_PAD,
                top: TITLE_TOP_PAD,
                right: client.right - TEXT_RIGHT_PAD,
                bottom: TITLE_TOP_PAD + TITLE_HEIGHT,
            };
            let title_w: Vec<u16> = self.title.encode_utf16().collect();
            if !title_w.is_empty() {
                let _ = DrawTextW(
                    hdc,
                    title_w.as_ptr(),
                    title_w.len() as i32,
                    &mut title_rc,
                    DT_LEFT | DT_NOPREFIX | DT_END_ELLIPSIS,
                );
            }

            if !self.body_font.is_null() {
                let prev = SelectObject(hdc, self.body_font as HGDIOBJ);
                if orig_font.is_null() {
                    orig_font = prev;
                }
            }
            let _ = SetTextColor(hdc, COLOR_BODY);
            let mut body_rc = RECT {
                left: TEXT_LEFT_PAD,
                top: BODY_TOP_PAD,
                right: client.right - TEXT_RIGHT_PAD,
                bottom: client.bottom - FOOTER_HEIGHT - FOOTER_BOTTOM_PAD - 4,
            };
            let body_w: Vec<u16> = self.body.encode_utf16().collect();
            if !body_w.is_empty() {
                let _ = DrawTextW(
                    hdc,
                    body_w.as_ptr(),
                    body_w.len() as i32,
                    &mut body_rc,
                    DT_LEFT | DT_WORDBREAK | DT_NOPREFIX | DT_END_ELLIPSIS,
                );
            }

            if !self.footer_font.is_null() {
                let prev = SelectObject(hdc, self.footer_font as HGDIOBJ);
                if orig_font.is_null() {
                    orig_font = prev;
                }
            }
            let _ = SetTextColor(hdc, COLOR_FOOTER);
            let mut footer_rc = RECT {
                left: TEXT_LEFT_PAD,
                top: client.bottom - FOOTER_HEIGHT - FOOTER_BOTTOM_PAD,
                right: client.right - TEXT_RIGHT_PAD,
                bottom: client.bottom - FOOTER_BOTTOM_PAD,
            };
            let footer_w: Vec<u16> = "Thermal Guard  \u{2022}  Click to dismiss".encode_utf16().collect();
            let _ = DrawTextW(
                hdc,
                footer_w.as_ptr(),
                footer_w.len() as i32,
                &mut footer_rc,
                DT_LEFT | DT_NOPREFIX | DT_END_ELLIPSIS,
            );

            if !orig_font.is_null() {
                let _ = SelectObject(hdc, orig_font);
            }
            if !bg_brush.is_null() {
                let _ = DeleteObject(bg_brush as HGDIOBJ);
            }
            if !border_brush.is_null() {
                let _ = DeleteObject(border_brush as HGDIOBJ);
            }
            if !accent_brush.is_null() {
                let _ = DeleteObject(accent_brush as HGDIOBJ);
            }

            let _ = EndPaint(hwnd, &ps);
        }
    }
}

impl Drop for PopupState {
    fn drop(&mut self) {
        unsafe {
            if !self.title_font.is_null() {
                let _ = DeleteObject(self.title_font as HGDIOBJ);
                self.title_font = null_mut();
            }
            if !self.body_font.is_null() {
                let _ = DeleteObject(self.body_font as HGDIOBJ);
                self.body_font = null_mut();
            }
            if !self.footer_font.is_null() {
                let _ = DeleteObject(self.footer_font as HGDIOBJ);
                self.footer_font = null_mut();
            }
        }
    }
}

/// Reusable non-focus-stealing Win32 floating toast notification window.
pub struct PopupToast {
    hwnd: HWND,
    state: *mut PopupState,
}

impl PopupToast {
    pub fn new(post_quit_on_hide: bool) -> Result<Self, u32> {
        let hinstance = unsafe { GetModuleHandleW(null()) } as HINSTANCE;
        let class_name = to_wide_nul("ThermalGuardPopupToast");
        let wc = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: 0,
            lpfnWndProc: Some(popup_wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: null_mut(),
            hCursor: unsafe { LoadCursorW(null_mut(), IDC_HAND) },
            hbrBackground: null_mut(),
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
            hIconSm: null_mut(),
        };
        let atom = unsafe { RegisterClassExW(&wc) };
        if atom == 0 {
            let err = unsafe { GetLastError() };
            if err != 1410 {
                return Err(err);
            }
        }

        let title = to_wide_nul("Thermal Guard Alert");
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_POPUP,
                0,
                0,
                POPUP_WIDTH,
                POPUP_MIN_HEIGHT,
                null_mut(),
                null_mut(),
                hinstance,
                null(),
            )
        };
        if hwnd.is_null() {
            return Err(unsafe { GetLastError() });
        }

        unsafe {
            let _ = SetLayeredWindowAttributes(hwnd, 0, 245, LWA_ALPHA);
        }

        let state = Box::into_raw(Box::new(PopupState::new(post_quit_on_hide)));
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize);
        }

        Ok(Self { hwnd, state })
    }

    pub fn show(
        &mut self,
        title: &str,
        body: &str,
        severity: AlertSeverity,
        duration_ms: u32,
        sound: bool,
    ) {
        let effective_duration = if severity == AlertSeverity::Critical {
            duration_ms.max(9_000)
        } else {
            duration_ms.max(1_000)
        };

        let height = unsafe {
            let state = &mut *self.state;
            state.title.clear();
            state.title.push_str(title);
            state.body.clear();
            state.body.push_str(body);
            state.severity = severity;
            state.measure_height(self.hwnd)
        };

        let work = get_work_area();
        let x = (work.right - POPUP_WIDTH - POPUP_MARGIN_SCREEN).max(work.left);
        let y = (work.bottom - height - POPUP_MARGIN_SCREEN).max(work.top);

        unsafe {
            let _ = KillTimer(self.hwnd, TIMER_AUTOHIDE_ID);
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                x,
                y,
                POPUP_WIDTH,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = InvalidateRect(self.hwnd, null(), 1);
            let _ = SetTimer(self.hwnd, TIMER_AUTOHIDE_ID, effective_duration, None);
        }

        if sound {
            let beep_kind = match severity {
                AlertSeverity::Critical | AlertSeverity::Warning => MB_ICONEXCLAMATION,
                AlertSeverity::Info => MB_ICONASTERISK,
            };
            unsafe {
                let _ = MessageBeep(beep_kind);
            }
        }
    }
}

impl Drop for PopupToast {
    fn drop(&mut self) {
        unsafe {
            if !self.hwnd.is_null() {
                let _ = KillTimer(self.hwnd, TIMER_AUTOHIDE_ID);
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                let _ = DestroyWindow(self.hwnd);
                self.hwnd = null_mut();
            }
            if !self.state.is_null() {
                drop(Box::from_raw(self.state));
                self.state = null_mut();
            }
        }
    }
}

/// Displays a standalone test floating toast popup and waits until dismissed or `duration_ms` elapses.
pub fn run_test_popup(duration_ms: u32, sound: bool) -> Result<(), u32> {
    let mut popup = PopupToast::new(true)?;
    popup.show(
        "Thermal Guard \u{2014} Test Alert",
        "Floating popup notifications are active! You will be alerted if CPU or GPU overheats, heats up quickly, or throttles.",
        AlertSeverity::Warning,
        duration_ms,
        sound,
    );

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

unsafe extern "system" fn popup_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut PopupState;
    if !ptr.is_null() {
        let state = unsafe { &*ptr };
        match msg {
            WM_ERASEBKGND => return 1,
            WM_PAINT => {
                state.paint(hwnd);
                return 0;
            }
            WM_TIMER if wparam == TIMER_AUTOHIDE_ID => {
                hide_popup(hwnd, state.post_quit_on_hide);
                return 0;
            }
            WM_LBUTTONUP | WM_RBUTTONUP => {
                hide_popup(hwnd, state.post_quit_on_hide);
                return 0;
            }
            WM_DESTROY => {
                if state.post_quit_on_hide {
                    unsafe { PostQuitMessage(0) };
                }
                return 0;
            }
            _ => {}
        }
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

fn hide_popup(hwnd: HWND, quit_loop: bool) {
    unsafe {
        let _ = KillTimer(hwnd, TIMER_AUTOHIDE_ID);
        let _ = ShowWindow(hwnd, SW_HIDE);
        if quit_loop {
            PostQuitMessage(0);
        }
    }
}

fn get_work_area() -> RECT {
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    unsafe {
        let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut rc as *mut RECT as *mut _, 0);
    }
    rc
}

fn to_wide_nul(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
