//! Event-driven Win32 host window & message loop bridging OS events to `AppController`.
//!
//! Architectural invariant: NEVER hold a `RefCell` borrow across Win32 calls that pump or
//! synchronously re-enter `host_wnd_proc` (`TrackPopupMenu`, flyout action callbacks, etc.).

pub mod canvas;
pub mod controller;
mod controller_events;
pub mod flyout;
pub mod flyout_layout;
pub mod logo;
pub mod osd;
pub mod theme;
pub mod tray;

use crate::config::AppConfig;
use crate::hardware::smart_profile::{AwakeDuration, AwakeTimer};
use crate::hardware::touchpad::{SliderFeedback, TouchpadManager};
use crate::hardware::wmi_mifs::{HidEventWatcher, MifsClient};
use crate::hardware::{awake, display};
use crate::protocol::{HidEvent, MiGestureState, PerfMode};
use crate::ui::controller::{
    AppController, TIMER_AWAKE_EXPIRE, TIMER_CHARGE_GUARD, TIMER_MI_DOUBLE, TIMER_MI_HOLD,
};
use crate::ui::flyout::FlyoutWindow;
use crate::ui::flyout_layout::FlyoutAction;
use crate::ui::osd::OsdWindow;
use crate::ui::tray::{TrayIcon, WM_TRAY_ICON};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Power::{
    RegisterSuspendResumeNotification, UnregisterPowerSettingNotification,
};
use windows::Win32::UI::WindowsAndMessaging::{
    ChangeWindowMessageFilterEx, CreateWindowExW, DEVICE_NOTIFY_WINDOW_HANDLE, DefWindowProcW,
    DestroyWindow, DispatchMessageW, FindWindowW, GetMessageW, GetWindowLongPtrW, GWLP_USERDATA,
    KillTimer, MSG, MSGFLT_ALLOW, PBT_APMPOWERSTATUSCHANGE, PBT_APMRESUMEAUTOMATIC,
    PBT_APMSUSPEND, PostMessageW, PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
    SetWindowLongPtrW, TranslateMessage, WM_APP, WM_COMMAND, WM_DESTROY, WM_ENDSESSION,
    WM_LBUTTONUP, WM_POWERBROADCAST, WM_QUERYENDSESSION, WM_RBUTTONUP, WM_TIMER, WNDCLASSW,
};
use windows_core::PCWSTR;

const WM_APP_HID_EVENT: u32 = WM_APP + 10;
const WM_APP_SLIDER_FEEDBACK: u32 = WM_APP + 11;
const WM_APP_FLYOUT_ACTION: u32 = WM_APP + 12;

/// Cached registered wake message ID; read lock-free / borrow-free inside `host_wnd_proc`.
static WAKE_MSG_ID: AtomicU32 = AtomicU32::new(0);

fn wake_message_id() -> u32 {
    let cached = WAKE_MSG_ID.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    // SAFETY: Standard RegisterWindowMessageW with static wide string.
    let id = unsafe { RegisterWindowMessageW(windows_core::w!("MiControl_WakeFlyout_v1")) };
    if id != 0 {
        WAKE_MSG_ID.store(id, Ordering::Relaxed);
    }
    id
}

/// Signal an already-running instance to pop open its flyout.
pub fn signal_existing_instance() {
    let msg = wake_message_id();
    if msg == 0 {
        return;
    }
    // SAFETY: Finds our own registered host window by class+title and posts the wake message directly.
    unsafe {
        if let Ok(hwnd) = FindWindowW(
            windows_core::w!("MiControlHostWindow"),
            windows_core::w!("MiControlHost"),
        ) {
            let _ = PostMessageW(Some(hwnd), msg, WPARAM(0), LPARAM(0));
        }
    }
}

struct HostContext {
    controller: RefCell<AppController>,
    flyout_actions: Arc<Mutex<VecDeque<FlyoutAction>>>,
}

/// Launch the event-driven tray + Fluent Flyout + OSD application loop.
pub fn run_tray_app() -> Result<(), String> {
    let mut cfg = AppConfig::load();
    let on_ac_boot = crate::hardware::is_on_ac_power();
    let bat_boot = crate::hardware::battery_percent();
    if crate::hardware::smart_profile::should_cutoff_awake(
        cfg.awake_enabled,
        on_ac_boot,
        bat_boot,
        cfg.awake_min_battery_pct,
    ) {
        awake::disable(&mut cfg);
        let _ = cfg.save();
    } else {
        awake::recover_if_needed(&mut cfg);
    }

    let wake_msg = wake_message_id();

    // SAFETY: Standard Win32 hidden host window creation.
    let hwnd = unsafe {
        let hinst = GetModuleHandleW(PCWSTR::null())
            .map_err(|e| format!("GetModuleHandleW failed: {e}"))?;
        let class_name = windows_core::w!("MiControlHostWindow");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(host_wnd_proc),
            hInstance: hinst.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);
        let hwnd = CreateWindowExW(
            Default::default(),
            class_name,
            windows_core::w!("MiControlHost"),
            Default::default(),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .map_err(|e| format!("CreateWindowExW(Host) failed: {e}"))?;
        if wake_msg != 0 {
            let _ = ChangeWindowMessageFilterEx(hwnd, wake_msg, MSGFLT_ALLOW, None);
        }
        hwnd
    };

    let osd = OsdWindow::create()?;
    let mifs = MifsClient::connect().ok();

    let hid_queue = Arc::new(Mutex::new(VecDeque::new()));
    let slider_queue = Arc::new(Mutex::new(VecDeque::new()));
    let flyout_actions = Arc::new(Mutex::new(VecDeque::new()));

    let hwnd_raw = hwnd.0 as isize;
    let q_slider = Arc::clone(&slider_queue);
    let mut touchpad = TouchpadManager::new(Some(Arc::new(move |fb| {
        {
            let mut q = q_slider.lock().unwrap_or_else(|e| e.into_inner());
            q.push_back(fb);
        }
        // SAFETY: Posting WM_APP notification to our own live host window.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd_raw as *mut c_void)),
                WM_APP_SLIDER_FEEDBACK,
                WPARAM(0),
                LPARAM(0),
            );
        }
    })));
    touchpad.sync_edge_reader(&cfg);

    // Subscribe to HID_EVENT20 unconditionally (works even without MIFS/admin on Xiaomi/Redmi laptops;
    // quietly exits its background thread on non-Xiaomi systems where the WMI class does not exist).
    let q_hid = Arc::clone(&hid_queue);
    let hid_watcher = Some(HidEventWatcher::start(move |ev| {
        {
            let mut q = q_hid.lock().unwrap_or_else(|e| e.into_inner());
            q.push_back(ev);
        }
        // SAFETY: Posting WM_APP notification to our own live host window.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(hwnd_raw as *mut c_void)),
                WM_APP_HID_EVENT,
                WPARAM(0),
                LPARAM(0),
            );
        }
    }));

    // SAFETY: Standard Modern Standby / Sleep-Resume notification bound to host HWND.
    let power_notify =
        unsafe { RegisterSuspendResumeNotification(HANDLE(hwnd.0), DEVICE_NOTIFY_WINDOW_HANDLE).ok() };

    let mi_hold = cfg.mi_hold_ms;
    let mi_dbl = cfg.mi_double_click_ms;

    let mut awake_timer = AwakeTimer::default();
    if cfg.awake_enabled {
        let _ = awake_timer.arm(AwakeDuration::Forever, 0);
    }

    let mut ctrl = AppController {
        hwnd,
        start_instant: Instant::now(),
        cfg,
        mifs,
        touchpad,
        tray: TrayIcon::new(hwnd),
        osd,
        flyout: None,
        current_mode: None,
        current_hz: display::current_internal_hz(),
        adapter_watts: None,
        battery_soh: None,
        on_ac: crate::hardware::is_on_ac_power(),
        awake_timer,
        mi_gesture: MiGestureState::new(mi_hold, mi_dbl),
        last_perf_event_ms: 0,
        ai_key_down: false,
        last_ai_event_ms: None,
        hid_queue,
        slider_queue,
        hid_watcher,
        power_notify,
    };

    ctrl.restore_saved_settings_on_startup();
    ctrl.refresh_tray();

    // Decouple Flyout clicks via async PostMessageW so flyout_wnd_proc never re-enters AppController.
    let q_actions = Arc::clone(&flyout_actions);
    let initial_snap = ctrl.snapshot();
    let flyout = FlyoutWindow::create(
        initial_snap,
        Rc::new(move |action| {
            {
                let mut q = q_actions.lock().unwrap_or_else(|e| e.into_inner());
                q.push_back(action);
            }
            // SAFETY: Posts async action message to host HWND after flyout releases its own borrows.
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd_raw as *mut c_void)),
                    WM_APP_FLYOUT_ACTION,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }),
    )?;
    ctrl.flyout = Some(flyout);

    let ctx = Box::new(HostContext {
        controller: RefCell::new(ctrl),
        flyout_actions,
    });
    let ctx_ptr = Box::into_raw(ctx);

    // SAFETY: Stores HostContext pointer in GWLP_USERDATA for the duration of the Win32 message loop.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, ctx_ptr as isize);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }

        SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
        let ctx = Box::from_raw(ctx_ptr);
        {
            let mut st = ctx.controller.borrow_mut();
            if st.cfg.awake_enabled {
                if st.awake_timer.active_preset() == Some(AwakeDuration::Forever) {
                    awake::suspend_for_exit(&mut st.cfg);
                } else {
                    awake::disable(&mut st.cfg);
                    let _ = st.cfg.save();
                }
            }
            st.touchpad.stop_edge_reader();
            st.hid_watcher = None;
            if let Some(pn) = st.power_notify.take() {
                let _ = UnregisterPowerSettingNotification(pn);
            }
        }
        drop(ctx);
        let _ = DestroyWindow(hwnd);
    }

    Ok(())
}

unsafe extern "system" fn host_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const HostContext;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) };
    }

    // Lock-free, borrow-free wake check (safe even during TrackPopupMenu modal loops).
    let wake_msg = WAKE_MSG_ID.load(Ordering::Relaxed);
    if wake_msg != 0 && msg == wake_msg {
        if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
            st.toggle_flyout();
        }
        return LRESULT(0);
    }

    match msg {
        WM_TRAY_ICON => {
            let ev = (lparam.0 & 0xFFFF) as u32;
            if ev == WM_LBUTTONUP {
                if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                    st.toggle_flyout();
                }
            } else if ev == WM_RBUTTONUP {
                // Build menu state under a brief borrow, DROP the borrow, then run TrackPopupMenu!
                let menu_state = unsafe {
                    (*ptr).controller.try_borrow_mut().ok().map(|mut st| {
                        st.sync_from_firmware();
                        st.refresh_tray();
                        st.tray_menu_state()
                    })
                };
                if let Some(state) = menu_state {
                    tray::show_context_menu_for(hwnd, &state);
                }
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u32;
            if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                match id {
                    tray::CMD_OPEN_FLYOUT => st.toggle_flyout(),
                    tray::CMD_MODE_ECO => st.set_mode(PerfMode::Eco, true),
                    tray::CMD_MODE_QUIET => st.set_mode(PerfMode::Quiet, true),
                    tray::CMD_MODE_AUTO => st.set_mode(PerfMode::Auto, true),
                    tray::CMD_MODE_TURBO => st.set_mode(PerfMode::Turbo, true),
                    tray::CMD_MODE_FULL => st.set_mode(PerfMode::FullSpeed, true),
                    tray::CMD_TOGGLE_CHARGE_CARE => st.toggle_charge_care(),
                    tray::CMD_TOGGLE_TRAVEL => st.toggle_travel_mode(),
                    tray::CMD_TOGGLE_AWAKE => st.toggle_awake(),
                    tray::CMD_AWAKE_30M => st.start_awake(AwakeDuration::Min30),
                    tray::CMD_AWAKE_1H => st.start_awake(AwakeDuration::Hour1),
                    tray::CMD_AWAKE_2H => st.start_awake(AwakeDuration::Hour2),
                    tray::CMD_TOGGLE_SMART_PROFILES => st.toggle_smart_profiles(),
                    tray::CMD_CYCLE_HZ => st.cycle_refresh_rate(),
                    tray::CMD_TOGGLE_TOUCHPAD => st.toggle_touchpad(),
                    tray::CMD_TOGGLE_AUTOSTART => st.toggle_autostart(),
                    tray::CMD_EXIT => unsafe { PostQuitMessage(0) },
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_APP_FLYOUT_ACTION => {
            let actions: Vec<FlyoutAction> = unsafe {
                let mut q = (*ptr).flyout_actions.lock().unwrap_or_else(|e| e.into_inner());
                q.drain(..).collect()
            };
            for action in actions {
                if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                    st.handle_flyout_action(action);
                }
            }
            LRESULT(0)
        }
        WM_APP_HID_EVENT => {
            let events: Vec<HidEvent> = unsafe {
                let Ok(st) = (*ptr).controller.try_borrow() else {
                    return LRESULT(0);
                };
                let mut q = st.hid_queue.lock().unwrap_or_else(|e| e.into_inner());
                q.drain(..).collect()
            };
            for ev in events {
                if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                    st.handle_hid_event(ev);
                }
            }
            LRESULT(0)
        }
        WM_APP_SLIDER_FEEDBACK => {
            let latest: Option<SliderFeedback> = unsafe {
                let Ok(st) = (*ptr).controller.try_borrow() else {
                    return LRESULT(0);
                };
                let mut q = st.slider_queue.lock().unwrap_or_else(|e| e.into_inner());
                q.drain(..).next_back()
            };
            if let Some(fb) = latest
                && let Ok(st) = unsafe { (*ptr).controller.try_borrow() }
            {
                match fb {
                    SliderFeedback::Brightness(pct) => {
                        st.show_osd("Brightness", &format!("{pct}%"), theme::ACCENT);
                    }
                    SliderFeedback::Volume(pct) => {
                        st.show_osd("Volume", &format!("{pct}%"), theme::STATUS_ECO);
                    }
                }
            }
            LRESULT(0)
        }
        WM_POWERBROADCAST => {
            match wparam.0 as u32 {
                PBT_APMSUSPEND => {
                    if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                        st.run_charge_guard(true);
                    }
                }
                PBT_APMRESUMEAUTOMATIC | PBT_APMPOWERSTATUSCHANGE => {
                    if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                        st.handle_power_change();
                    }
                }
                _ => {}
            }
            LRESULT(1)
        }
        WM_QUERYENDSESSION => {
            if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                st.run_charge_guard(true);
                let _ = st.cfg.save();
            }
            LRESULT(1)
        }
        WM_ENDSESSION => {
            if wparam.0 != 0
                && let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() }
                && st.cfg.awake_enabled
            {
                if st.awake_timer.active_preset() == Some(AwakeDuration::Forever) {
                    awake::suspend_for_exit(&mut st.cfg);
                } else {
                    awake::disable(&mut st.cfg);
                    let _ = st.cfg.save();
                }
            }
            LRESULT(0)
        }
        WM_TIMER => {
            let id = wparam.0;
            unsafe {
                let _ = KillTimer(Some(hwnd), id);
            }
            if let Ok(mut st) = unsafe { (*ptr).controller.try_borrow_mut() } {
                let now = st.now_ms();
                match id {
                    TIMER_CHARGE_GUARD => {
                        st.run_charge_guard(false);
                        st.sync_from_firmware();
                        st.refresh_tray();
                        st.refresh_flyout_if_open();
                    }
                    TIMER_MI_HOLD => {
                        if let Some(g) = st.mi_gesture.on_hold_timeout(now) {
                            st.handle_mi_gesture(g);
                        }
                    }
                    TIMER_MI_DOUBLE => {
                        if let Some(g) = st.mi_gesture.on_double_timeout(now) {
                            st.handle_mi_gesture(g);
                        }
                    }
                    TIMER_AWAKE_EXPIRE => st.on_awake_timer_expired(),
                    _ => {}
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
