//! Native Win32 system tray icon (`Shell_NotifyIconW`) with dynamic programmatic ARGB `HICON`
//! rendering (zero `.ico` bloat) and a clean native context menu.

use crate::protocol::{ChargeLimit, PerfMode};
use crate::ui::logo;
use std::ffi::c_void;
use windows::Win32::Foundation::{HWND, POINT};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject, GetDC, ReleaseDC,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
    Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconIndirect, CreatePopupMenu, DestroyIcon, DestroyMenu, GetCursorPos,
    GetSystemMetrics, HICON, ICONINFO, MF_CHECKED, MF_GRAYED, MF_SEPARATOR, MF_STRING,
    SM_CXSMICON, SetForegroundWindow, TPM_BOTTOMALIGN, TPM_RIGHTALIGN, TPM_RIGHTBUTTON,
    TrackPopupMenu, WM_APP,
};
use windows_core::PCWSTR;

pub const WM_TRAY_ICON: u32 = WM_APP + 1;
const TRAY_UID: u32 = 1;

// Context menu command IDs
pub const CMD_OPEN_FLYOUT: u32 = 1001;
pub const CMD_MODE_ECO: u32 = 1010;
pub const CMD_MODE_QUIET: u32 = 1011;
pub const CMD_MODE_AUTO: u32 = 1012;
pub const CMD_MODE_TURBO: u32 = 1013;
pub const CMD_MODE_FULL: u32 = 1014;
pub const CMD_TOGGLE_CHARGE_CARE: u32 = 1020;
pub const CMD_TOGGLE_TRAVEL: u32 = 1021;
pub const CMD_TOGGLE_AWAKE: u32 = 1030;
pub const CMD_AWAKE_30M: u32 = 1032;
pub const CMD_AWAKE_1H: u32 = 1033;
pub const CMD_AWAKE_2H: u32 = 1034;
pub const CMD_TOGGLE_SMART_PROFILES: u32 = 1035;
pub const CMD_CYCLE_HZ: u32 = 1036;
pub const CMD_TOGGLE_TOUCHPAD: u32 = 1031;
pub const CMD_TOGGLE_AUTOSTART: u32 = 1040;
pub const CMD_EXIT: u32 = 1099;

pub struct TrayIcon {
    hwnd: HWND,
    current_icon: Option<HICON>,
    added: bool,
}

pub struct TrayMenuState {
    pub current_mode: Option<PerfMode>,
    pub on_ac: bool,
    pub charge_care: bool,
    pub care_limit: ChargeLimit,
    pub travel_mode: bool,
    pub awake: bool,
    pub smart_profiles: bool,
    pub current_hz: Option<u32>,
    pub touchpad_enabled: Option<bool>,
    pub autostart: bool,
}

impl TrayIcon {
    pub fn new(hwnd: HWND) -> Self {
        Self {
            hwnd,
            current_icon: None,
            added: false,
        }
    }

    pub fn update(&mut self, mode: Option<PerfMode>, care_limit: Option<ChargeLimit>, tooltip: &str) {
        let (acc1, acc2) = logo::mode_accent_pair(mode);
        let care_dot = care_limit.is_some_and(|l| l.is_care_active());
        let new_icon = create_badge_icon(acc1, acc2, care_dot);

        let mut nid = NOTIFYICONDATAW {
            cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_UID,
            uFlags: NIF_MESSAGE | NIF_TIP | if new_icon.is_some() { NIF_ICON } else { Default::default() },
            uCallbackMessage: WM_TRAY_ICON,
            hIcon: new_icon.unwrap_or_default(),
            ..Default::default()
        };

        let tip_utf16: Vec<u16> = tooltip.encode_utf16().take(127).collect();
        for (dst, &src) in nid.szTip.iter_mut().zip(tip_utf16.iter()) {
            *dst = src;
        }

        // SAFETY: Standard Shell_NotifyIconW registration/update with valid HWND and HICON.
        unsafe {
            let action = if self.added { NIM_MODIFY } else { NIM_ADD };
            if Shell_NotifyIconW(action, &nid).as_bool() {
                self.added = true;
            }
            if let Some(old) = self.current_icon.take() {
                let _ = DestroyIcon(old);
            }
        }
        self.current_icon = new_icon;
    }

    pub fn show_context_menu(&self, s: &TrayMenuState) {
        show_context_menu_for(self.hwnd, s);
    }
}

/// Display the native tray context menu for `hwnd`.
/// Free function taking only `HWND` and `&TrayMenuState` so callers never hold a `RefCell`
/// borrow across `TrackPopupMenu`'s nested modal message loop.
pub fn show_context_menu_for(hwnd: HWND, s: &TrayMenuState) {
    // SAFETY: Standard Win32 popup menu creation, display at cursor, and cleanup.
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        append_item(menu, CMD_OPEN_FLYOUT, "Open Control Panel\tFn+F7", false, false);
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());

        let modes = [
            (PerfMode::Eco, CMD_MODE_ECO, "Mode: Eco"),
            (PerfMode::Quiet, CMD_MODE_QUIET, "Mode: Quiet"),
            (PerfMode::Auto, CMD_MODE_AUTO, "Mode: Auto"),
            (PerfMode::Turbo, CMD_MODE_TURBO, "Mode: Turbo"),
            (PerfMode::FullSpeed, CMD_MODE_FULL, "Mode: Full Speed (AC)"),
        ];
        for (m, id, label) in modes {
            let checked = s.current_mode == Some(m);
            let disabled = m.requires_ac() && !s.on_ac;
            append_item(menu, id, label, checked, disabled);
        }

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let care_label = format!("Battery Care ({})", s.care_limit);
        append_item(
            menu,
            CMD_TOGGLE_CHARGE_CARE,
            &care_label,
            s.charge_care && !s.travel_mode,
            false,
        );
        append_item(menu, CMD_TOGGLE_TRAVEL, "Travel Mode (100% Once)", s.travel_mode, false);

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_item(menu, CMD_TOGGLE_SMART_PROFILES, "Smart AC/Battery Profiles", s.smart_profiles, false);
        let hz_label = s
            .current_hz
            .map(|hz| format!("Cycle Display Hz ({hz} Hz)"))
            .unwrap_or_else(|| "Cycle Display Hz".into());
        append_item(menu, CMD_CYCLE_HZ, &hz_label, false, s.current_hz.is_none());

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_item(menu, CMD_TOGGLE_AWAKE, "Keep Awake (∞)", s.awake, false);
        append_item(menu, CMD_AWAKE_30M, "Keep Awake: 30m", false, false);
        append_item(menu, CMD_AWAKE_1H, "Keep Awake: 1h", false, false);
        append_item(menu, CMD_AWAKE_2H, "Keep Awake: 2h", false, false);

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        match s.touchpad_enabled {
            Some(on) => append_item(menu, CMD_TOGGLE_TOUCHPAD, "Touchpad Enabled", on, false),
            None => append_item(menu, CMD_TOGGLE_TOUCHPAD, "Touchpad (Unavailable)", false, true),
        }
        append_item(menu, CMD_TOGGLE_AUTOSTART, "Start at Login (Admin)", s.autostart, false);

        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        append_item(menu, CMD_EXIT, "Exit MiControl", false, false);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(
            menu,
            TPM_RIGHTALIGN | TPM_BOTTOMALIGN | TPM_RIGHTBUTTON,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = DestroyMenu(menu);
    }
}

impl Drop for TrayIcon {
    fn drop(&mut self) {
        // SAFETY: Removes tray icon and destroys owned HICON cleanly on shutdown.
        unsafe {
            if self.added {
                let nid = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.hwnd,
                    uID: TRAY_UID,
                    ..Default::default()
                };
                let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
            }
            if let Some(icon) = self.current_icon.take() {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

unsafe fn append_item(
    menu: windows::Win32::UI::WindowsAndMessaging::HMENU,
    id: u32,
    label: &str,
    checked: bool,
    disabled: bool,
) {
    let mut flags = MF_STRING;
    if checked {
        flags |= MF_CHECKED;
    }
    if disabled {
        flags |= MF_GRAYED;
    }
    let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: Valid HMENU handle and NUL-terminated UTF-16 menu string.
    unsafe {
        let _ = AppendMenuW(menu, flags, id as usize, PCWSTR(wide.as_ptr()));
    }
}

/// Render a crisp High-DPI ARGB squircle tray icon with mode accent glow and optional emerald care dot.
fn create_badge_icon(accent1: u32, accent2: u32, care_dot: bool) -> Option<HICON> {
    // SAFETY: Standard Win32 metric query; clamp 32..=64 so tray icons stay sharp on 150%-250% DPI.
    let sys_px = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let size = sys_px.clamp(32, 64);

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size, // top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };

    let rendered = logo::render_premul_argb(size as usize, accent1, accent2, care_dot, true);

    // SAFETY: Allocates top-down 32bpp DIB, copies rendered pixels, creates HICON, frees GDI bitmaps.
    unsafe {
        let hdc = GetDC(None);
        let mut bits_ptr: *mut c_void = std::ptr::null_mut();
        let Ok(hbm_color) =
            CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits_ptr, None, 0)
        else {
            ReleaseDC(None, hdc);
            return None;
        };
        ReleaseDC(None, hdc);

        if bits_ptr.is_null() {
            let _ = DeleteObject(hbm_color.into());
            return None;
        }

        let pixels = std::slice::from_raw_parts_mut(bits_ptr as *mut u32, (size * size) as usize);
        pixels.copy_from_slice(&rendered);

        let mask_stride = ((size + 15) / 16 * 2) as usize;
        let mask_bytes = vec![0u8; mask_stride * size as usize];
        let hbm_mask = CreateBitmap(size, size, 1, 1, Some(mask_bytes.as_ptr() as *const c_void));

        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: hbm_mask,
            hbmColor: hbm_color,
        };
        let icon = CreateIconIndirect(&info).ok();
        let _ = DeleteObject(hbm_color.into());
        let _ = DeleteObject(hbm_mask.into());
        icon
    }
}
