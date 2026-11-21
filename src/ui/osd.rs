//! Ultra-lightweight frameless click-through Win11 Fluent OSD toast.
//!
//! Zero GPU driver DLLs, zero persistent buffers — allocates its DIB strictly inside `WM_PAINT`
//! and destroys its hide timer on dismiss (0% idle CPU, 0 idle graphics RAM).

use crate::ui::canvas::{self, PaintCanvas, TextAlign};
use crate::ui::theme::{self, RectF, Rgb};
use std::cell::RefCell;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, GWLP_USERDATA,
    HWND_TOPMOST, KillTimer, LWA_ALPHA, RegisterClassW, SPI_GETWORKAREA, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_SHOWWINDOW, SetLayeredWindowAttributes, SetTimer,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, SystemParametersInfoW, WM_PAINT, WM_TIMER,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
    WS_EX_TRANSPARENT, WS_POPUP,
};
use windows_core::PCWSTR;

const TIMER_HIDE: usize = 1;

struct OsdInner {
    title: String,
    subtitle: String,
    accent: Rgb,
}

pub struct OsdWindow {
    hwnd: HWND,
    state_ptr: *mut RefCell<OsdInner>,
}

impl OsdWindow {
    pub fn create() -> Result<Self, String> {
        let inner = Box::new(RefCell::new(OsdInner {
            title: String::new(),
            subtitle: String::new(),
            accent: theme::ACCENT,
        }));
        let state_ptr = Box::into_raw(inner);

        // SAFETY: Registers and creates a click-through, non-activating topmost popup window.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(PCWSTR::null())
                .map_err(|e| format!("GetModuleHandleW failed: {e}"))?;
            let class_name = windows_core::w!("MiControlOsdWindow");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(osd_wnd_proc),
                hInstance: hinst.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_LAYERED,
                class_name,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                248,
                58,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .map_err(|e| format!("CreateWindowExW(OSD) failed: {e}"))?;

            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 244, LWA_ALPHA);
            canvas::apply_win11_fluent_chrome(hwnd);
            hwnd
        };

        Ok(Self { hwnd, state_ptr })
    }

    pub fn show(&self, title: &str, subtitle: &str, accent: Rgb, duration_ms: u32) {
        // SAFETY: Single-threaded UI access to owned RefCell + Win32 window positioning.
        unsafe {
            if let Ok(mut inner) = (*self.state_ptr).try_borrow_mut() {
                inner.title = title.to_string();
                inner.subtitle = subtitle.to_string();
                inner.accent = accent;
            }

            let dpi = canvas::window_dpi(self.hwnd);
            let w = canvas::dip_to_px(theme::OSD_WIDTH_DIP, dpi);
            let h = canvas::dip_to_px(theme::OSD_HEIGHT_DIP, dpi);

            let mut work = RECT::default();
            let _ = SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut work as *mut RECT as *mut std::ffi::c_void),
                Default::default(),
            );
            let x = work.left + ((work.right - work.left) - w) / 2;
            let y = work.bottom - h - canvas::dip_to_px(36.0, dpi);

            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                x,
                y,
                w,
                h,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
            let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
            let _ = SetTimer(Some(self.hwnd), TIMER_HIDE, duration_ms.clamp(500, 6000), None);
        }
    }
}

impl Drop for OsdWindow {
    fn drop(&mut self) {
        // SAFETY: Kills timer and destroys window before freeing boxed state.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_HIDE);
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            drop(Box::from_raw(self.state_ptr));
        }
    }
}

unsafe extern "system" fn osd_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER if wparam.0 == TIMER_HIDE => {
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_HIDE);
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<OsdInner>;
                if !ptr.is_null()
                    && let Ok(inner) = (*ptr).try_borrow()
                {
                    paint_osd(hwnd, hdc, &inner);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn paint_osd(hwnd: HWND, hdc: windows::Win32::Graphics::Gdi::HDC, inner: &OsdInner) {
    let dpi = canvas::window_dpi(hwnd);
    let phys_w = canvas::dip_to_px(theme::OSD_WIDTH_DIP, dpi);
    let phys_h = canvas::dip_to_px(theme::OSD_HEIGHT_DIP, dpi);
    let Some(mut cv) = PaintCanvas::new(hdc, phys_w, phys_h, dpi) else {
        return;
    };

    let w = theme::OSD_WIDTH_DIP;
    let h = theme::OSD_HEIGHT_DIP;

    cv.clear(theme::CANVAS);
    cv.fill_round_rect(
        RectF {
            left: 12.0,
            top: 12.0,
            right: 16.0,
            bottom: h - 12.0,
        },
        2.0,
        inner.accent,
    );
    cv.draw_text(
        &inner.title,
        RectF {
            left: 26.0,
            top: 8.0,
            right: w - 12.0,
            bottom: 30.0,
        },
        13.0,
        600,
        theme::INK_PRIMARY,
        TextAlign::Left,
    );
    cv.draw_text(
        &inner.subtitle,
        RectF {
            left: 26.0,
            top: 30.0,
            right: w - 12.0,
            bottom: h - 8.0,
        },
        11.0,
        400,
        theme::INK_SECONDARY,
        TextAlign::Left,
    );
    cv.present(hdc);
}
