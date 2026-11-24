//! Native Win32 + zero-GPU software-DIB Windows 11 Fluent Quick Control Flyout.
//!
//! Allocates its backbuffer strictly during `WM_PAINT` and frees it immediately after `BitBlt`
//! (0 idle GPU memory, 0 GPU driver DLLs loaded, 0% idle CPU).

use crate::ui::canvas::{self, PaintCanvas, TextAlign};
use crate::ui::flyout_layout::{
    self, ControlTile, FlyoutAction, FlyoutSnapshot, hit_test, move_focus,
};
use crate::ui::theme::{self, RectF};
use std::cell::RefCell;
use std::ffi::c_void;
use std::rc::Rc;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, SetActiveWindow, TRACKMOUSEEVENT, TME_LEAVE, TrackMouseEvent, VK_DOWN, VK_ESCAPE,
    VK_LEFT, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_SPACE, VK_TAB, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, CreateWindowExW, DefWindowProcW, DestroyWindow, GetForegroundWindow,
    GetWindowLongPtrW, GetWindowThreadProcessId, GWLP_USERDATA, HWND_TOPMOST, IDC_ARROW,
    IsWindowVisible, LoadCursorW, RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SW_SHOW,
    SWP_SHOWWINDOW, SetForegroundWindow, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    SystemParametersInfoW, WA_INACTIVE, WM_ACTIVATE, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDOWN,
    WM_LBUTTONUP, WM_MOUSEMOVE, WM_PAINT, WNDCLASSW, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows_core::PCWSTR;

const WM_MOUSELEAVE: u32 = 0x02A3;

struct FlyoutInner {
    snapshot: FlyoutSnapshot,
    tiles: Vec<ControlTile>,
    height_dip: f32,
    hover_idx: Option<usize>,
    pressed_idx: Option<usize>,
    focus_idx: Option<usize>,
    tracking_mouse: bool,
    on_action: Rc<dyn Fn(FlyoutAction)>,
}

pub struct FlyoutWindow {
    hwnd: HWND,
    state_ptr: *mut RefCell<FlyoutInner>,
}

impl FlyoutWindow {
    pub fn create(
        initial: FlyoutSnapshot,
        on_action: Rc<dyn Fn(FlyoutAction)>,
    ) -> Result<Self, String> {
        let (height_dip, tiles) = flyout_layout::build_layout(&initial);
        let inner = Box::new(RefCell::new(FlyoutInner {
            snapshot: initial,
            tiles,
            height_dip,
            hover_idx: None,
            pressed_idx: None,
            focus_idx: None,
            tracking_mouse: false,
            on_action,
        }));
        let state_ptr = Box::into_raw(inner);

        // SAFETY: Creates hidden frameless Win32 popup window with Fluent Win11 DWM rounded corners.
        let hwnd = unsafe {
            let hinst = GetModuleHandleW(PCWSTR::null())
                .map_err(|e| format!("GetModuleHandleW failed: {e}"))?;
            let class_name = windows_core::w!("MiControlFlyoutWindow");
            let wc = WNDCLASSW {
                lpfnWndProc: Some(flyout_wnd_proc),
                hInstance: hinst.into(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: class_name,
                ..Default::default()
            };
            let _ = RegisterClassW(&wc);

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                class_name,
                windows_core::w!("MiControl"),
                WS_POPUP,
                0,
                0,
                348,
                380,
                None,
                None,
                Some(hinst.into()),
                None,
            )
            .map_err(|e| format!("CreateWindowExW(Flyout) failed: {e}"))?;

            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_ptr as isize);
            canvas::apply_win11_fluent_chrome(hwnd);
            hwnd
        };

        Ok(Self { hwnd, state_ptr })
    }

    pub fn is_visible(&self) -> bool {
        // SAFETY: Valid HWND owned by self.
        unsafe { IsWindowVisible(self.hwnd).as_bool() }
    }

    pub fn update_state(&self, snapshot: FlyoutSnapshot) {
        // SAFETY: Single-threaded UI access; borrow dropped before any Win32 messaging calls.
        unsafe {
            let Ok(mut inner) = (*self.state_ptr).try_borrow_mut() else {
                return;
            };
            let (new_h, tiles) = flyout_layout::build_layout(&snapshot);
            let height_changed = (new_h - inner.height_dip).abs() > 0.5;
            inner.snapshot = snapshot;
            inner.tiles = tiles;
            inner.height_dip = new_h;
            drop(inner);

            if IsWindowVisible(self.hwnd).as_bool() {
                if height_changed {
                    self.position_window();
                }
                let _ = InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    pub fn show(&self, snapshot: FlyoutSnapshot) {
        self.update_state(snapshot);
        // SAFETY: Positions window above taskbar bottom-right and brings it to foreground.
        unsafe {
            if let Ok(mut inner) = (*self.state_ptr).try_borrow_mut() {
                inner.hover_idx = None;
                inner.pressed_idx = None;
                inner.focus_idx = None;
            }
            self.position_window();
            let _ = ShowWindow(self.hwnd, SW_SHOW);
            bring_to_foreground(self.hwnd);
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    pub fn hide(&self) {
        // SAFETY: Valid HWND owned by self.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    fn position_window(&self) {
        // SAFETY: Anchors flyout cleanly inside SPI_GETWORKAREA bottom-right with 12 DIP margin.
        unsafe {
            let dpi = canvas::window_dpi(self.hwnd);
            let Ok(h_dip) = (*self.state_ptr).try_borrow().map(|i| i.height_dip) else {
                return;
            };
            let w = canvas::dip_to_px(theme::FLYOUT_WIDTH_DIP, dpi);
            let h = canvas::dip_to_px(h_dip, dpi);
            let margin = canvas::dip_to_px(12.0, dpi);

            let mut work = RECT::default();
            let _ = SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some(&mut work as *mut RECT as *mut c_void),
                Default::default(),
            );
            let x = (work.right - w - margin).max(work.left + margin);
            let y = (work.bottom - h - margin).max(work.top + margin);
            let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_SHOWWINDOW);
        }
    }
}

impl Drop for FlyoutWindow {
    fn drop(&mut self) {
        // SAFETY: Destroys HWND before reclaiming boxed state.
        unsafe {
            SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
            let _ = DestroyWindow(self.hwnd);
            drop(Box::from_raw(self.state_ptr));
        }
    }
}

/// Reliably activate and focus the Flyout even when triggered from a background WMI hotkey
/// (`Fn+F7` / `HID_EVENT20`) or single-instance wake IPC while another app owns foreground focus.
fn bring_to_foreground(hwnd: HWND) {
    // SAFETY: Standard Win32 foreground activation with bounded AttachThreadInput fallback.
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() {
            return;
        }
        let fg = GetForegroundWindow();
        let fg_tid = if !fg.0.is_null() {
            GetWindowThreadProcessId(fg, None)
        } else {
            0
        };
        let cur_tid = GetCurrentThreadId();
        let attached = fg_tid != 0
            && fg_tid != cur_tid
            && AttachThreadInput(cur_tid, fg_tid, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetActiveWindow(hwnd);
        if attached {
            let _ = AttachThreadInput(cur_tid, fg_tid, false);
        }
    }
}

unsafe extern "system" fn flyout_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const RefCell<FlyoutInner>;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    match msg {
        WM_ERASEBKGND => LRESULT(1),
        WM_ACTIVATE => {
            if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let dpi = canvas::window_dpi(hwnd);
            let x = canvas::px_to_dip((lparam.0 & 0xFFFF) as i16 as i32, dpi);
            let y = canvas::px_to_dip(((lparam.0 >> 16) & 0xFFFF) as i16 as i32, dpi);
            let (need_track, changed) = unsafe {
                let Ok(mut inner) = (*ptr).try_borrow_mut() else {
                    return LRESULT(0);
                };
                let need_track = !inner.tracking_mouse;
                inner.tracking_mouse = true;
                let hit = hit_test(&inner.tiles, x, y);
                let changed = hit != inner.hover_idx;
                if changed {
                    inner.hover_idx = hit;
                }
                (need_track, changed)
            };
            if need_track {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                unsafe {
                    let _ = TrackMouseEvent(&mut tme);
                }
            }
            if changed {
                unsafe {
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            unsafe {
                if let Ok(mut inner) = (*ptr).try_borrow_mut() {
                    inner.tracking_mouse = false;
                    inner.hover_idx = None;
                    inner.pressed_idx = None;
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let dpi = canvas::window_dpi(hwnd);
            let x = canvas::px_to_dip((lparam.0 & 0xFFFF) as i16 as i32, dpi);
            let y = canvas::px_to_dip(((lparam.0 >> 16) & 0xFFFF) as i16 as i32, dpi);
            unsafe {
                if let Ok(mut inner) = (*ptr).try_borrow_mut() {
                    inner.pressed_idx = hit_test(&inner.tiles, x, y);
                    inner.focus_idx = inner.pressed_idx;
                }
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let dpi = canvas::window_dpi(hwnd);
            let x = canvas::px_to_dip((lparam.0 & 0xFFFF) as i16 as i32, dpi);
            let y = canvas::px_to_dip(((lparam.0 >> 16) & 0xFFFF) as i16 as i32, dpi);
            let to_fire = unsafe {
                let Ok(mut inner) = (*ptr).try_borrow_mut() else {
                    return LRESULT(0);
                };
                let up_hit = hit_test(&inner.tiles, x, y);
                let pressed = inner.pressed_idx.take();
                let cb = Rc::clone(&inner.on_action);
                let action = match (pressed, up_hit) {
                    (Some(a), Some(b)) if a == b => Some(inner.tiles[a].action),
                    _ => None,
                };
                (action, cb)
            };
            unsafe {
                let _ = InvalidateRect(Some(hwnd), None, false);
            }
            if let (Some(action), cb) = to_fire {
                cb(action);
            }
            LRESULT(0)
        }
        WM_KEYDOWN => {
            let vk = wparam.0 as u16;
            if vk == VK_ESCAPE.0 {
                unsafe {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                return LRESULT(0);
            }
            if vk == VK_TAB.0 || vk == VK_RIGHT.0 || vk == VK_DOWN.0 || vk == VK_LEFT.0 || vk == VK_UP.0 {
                let shift_down = unsafe { (GetKeyState(VK_SHIFT.0 as i32) as u16 & 0x8000) != 0 };
                let delta = if (vk == VK_TAB.0 && shift_down) || vk == VK_LEFT.0 || vk == VK_UP.0 {
                    -1
                } else {
                    1
                };
                unsafe {
                    if let Ok(mut inner) = (*ptr).try_borrow_mut() {
                        inner.focus_idx = move_focus(&inner.tiles, inner.focus_idx, delta);
                    }
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                return LRESULT(0);
            }
            if vk == VK_RETURN.0 || vk == VK_SPACE.0 {
                let to_fire = unsafe {
                    let Ok(inner) = (*ptr).try_borrow() else {
                        return LRESULT(0);
                    };
                    let cb = Rc::clone(&inner.on_action);
                    let action = inner
                        .focus_idx
                        .and_then(|i| inner.tiles.get(i))
                        .filter(|t| !t.disabled)
                        .map(|t| t.action);
                    (action, cb)
                };
                if let (Some(action), cb) = to_fire {
                    cb(action);
                }
                return LRESULT(0);
            }
            LRESULT(0)
        }
        WM_PAINT => {
            unsafe {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);
                if let Ok(inner) = (*ptr).try_borrow() {
                    paint_flyout(hwnd, hdc, &inner);
                }
                let _ = EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn paint_flyout(hwnd: HWND, hdc: HDC, inner: &FlyoutInner) {
    let dpi = canvas::window_dpi(hwnd);
    let phys_w = canvas::dip_to_px(theme::FLYOUT_WIDTH_DIP, dpi);
    let phys_h = canvas::dip_to_px(inner.height_dip, dpi);
    let Some(mut cv) = PaintCanvas::new(hdc, phys_w, phys_h, dpi) else {
        return;
    };

    let w = theme::FLYOUT_WIDTH_DIP;
    let pad = 14.0;
    let s = &inner.snapshot;

    let mut parts: Vec<String> = Vec::new();
    if let Some(pct) = s.battery_pct {
        parts.push(format!("{pct}%"));
    }
    if s.on_ac {
        if let Some(w_pd) = s.adapter_watts.filter(|&w| w > 0) {
            parts.push(format!("{w_pd}W PD"));
        } else {
            parts.push("AC".into());
        }
    } else {
        parts.push("Battery".into());
    }
    if let Some(soh) = s.battery_soh.filter(|&v| v > 0 && v <= 100) {
        parts.push(format!("SOH {soh}%"));
    }
    let badge = parts.join(" · ");

    cv.clear(theme::CANVAS);

    let logo_px = canvas::dip_to_px(22.0, dpi).max(16) as usize;
    let (acc1, acc2) = crate::ui::logo::mode_accent_pair(s.current_mode);
    let logo_buf = crate::ui::logo::render_premul_argb(logo_px, acc1, acc2, false, false);
    cv.blit_premul_argb(pad, 14.0, logo_px, &logo_buf);

    cv.draw_text(
        "MiControl",
        RectF {
            left: pad + 28.0,
            top: 12.0,
            right: 152.0,
            bottom: 38.0,
        },
        14.0,
        700,
        theme::INK_PRIMARY,
        TextAlign::Left,
    );
    cv.draw_text(
        &badge,
        RectF {
            left: 152.0,
            top: 12.0,
            right: w - pad,
            bottom: 38.0,
        },
        10.5,
        500,
        theme::INK_SECONDARY,
        TextAlign::Right,
    );

    let mut sec_y = 50.0;
    if !s.wmi_available {
        let warn_rect = RectF {
            left: pad,
            top: sec_y,
            right: w - pad,
            bottom: sec_y + 30.0,
        };
        cv.fill_round_rect(warn_rect, 6.0, theme::STATUS_WARN_BG);
        cv.stroke_round_rect(warn_rect, 6.0, 1.0, theme::STATUS_TURBO);
        cv.draw_text(
            "Firmware WMI unavailable — run as Administrator",
            RectF {
                left: pad + 10.0,
                top: sec_y,
                right: w - pad - 10.0,
                bottom: sec_y + 30.0,
            },
            9.5,
            500,
            theme::INK_PRIMARY,
            TextAlign::Left,
        );
        sec_y += 38.0;
    }

    let section_labels = [
        (sec_y, "PERFORMANCE MODE"),
        (sec_y + 74.0, "BATTERY PROTECTION"),
        (sec_y + 184.0, "SMART PROFILES & CONTROLS"),
    ];
    for (sy, title) in section_labels {
        cv.draw_text(
            title,
            RectF {
                left: pad,
                top: sy,
                right: w - pad,
                bottom: sy + 18.0,
            },
            10.0,
            600,
            theme::INK_SECONDARY,
            TextAlign::Left,
        );
    }

    for (idx, tile) in inner.tiles.iter().enumerate() {
        let hovered = inner.hover_idx == Some(idx);
        let pressed = inner.pressed_idx == Some(idx);
        let focused = inner.focus_idx == Some(idx);

        let (fill, title_ink, sub_ink, border) = if tile.disabled {
            (theme::SURFACE_DISABLED, theme::INK_MUTED, theme::INK_MUTED, theme::BORDER)
        } else if tile.active {
            if tile.action == FlyoutAction::ToggleAutostart {
                (theme::ACCENT_SOFT, theme::INK_PRIMARY, theme::INK_SECONDARY, theme::ACCENT)
            } else {
                (tile.accent, theme::INK_ON_ACCENT, theme::INK_ON_ACCENT, tile.accent)
            }
        } else if pressed {
            (theme::SURFACE_ACTIVE, theme::INK_PRIMARY, theme::INK_SECONDARY, theme::BORDER)
        } else if hovered {
            (theme::SURFACE_HOVER, theme::INK_PRIMARY, theme::INK_SECONDARY, theme::BORDER)
        } else {
            (theme::SURFACE, theme::INK_PRIMARY, theme::INK_SECONDARY, theme::BORDER)
        };

        cv.fill_round_rect(tile.rect, 8.0, fill);
        if focused {
            cv.stroke_round_rect(tile.rect, 8.0, 2.0, theme::BORDER_FOCUS);
        } else {
            cv.stroke_round_rect(tile.rect, 8.0, 1.0, border);
        }

        if tile.centered || tile.subtitle.is_none() {
            cv.draw_text(&tile.title, tile.rect, 11.5, 600, title_ink, TextAlign::Center);
        } else {
            let mid = (tile.rect.top + tile.rect.bottom) * 0.5;
            cv.draw_text(
                &tile.title,
                RectF {
                    left: tile.rect.left + 10.0,
                    top: tile.rect.top + 5.0,
                    right: tile.rect.right - 8.0,
                    bottom: mid + 2.0,
                },
                11.5,
                600,
                title_ink,
                TextAlign::Left,
            );
            if let Some(ref sub) = tile.subtitle {
                cv.draw_text(
                    sub,
                    RectF {
                        left: tile.rect.left + 10.0,
                        top: mid,
                        right: tile.rect.right - 8.0,
                        bottom: tile.rect.bottom - 5.0,
                    },
                    9.5,
                    400,
                    sub_ink,
                    TextAlign::Left,
                );
            }
        }
    }

    cv.present(hdc);
}
