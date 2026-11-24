//! Application controller state: coordinates WMI MIFS, ChargeGuard, hotkeys, Mi-button gestures,
//! Owl Keep-Awake, Touchpad controls, Tray icon, Flyout snapshot, and OSD notifications.

use crate::config::AppConfig;
use crate::hardware::smart_profile::{self, AwakeDuration, AwakeTimer};
use crate::hardware::touchpad::{SliderFeedback, TouchpadManager};
use crate::hardware::wmi_mifs::{HidEventWatcher, MifsClient};
use crate::hardware::{autostart, awake, charge_guard, display};
use crate::protocol::{self, ChargeLimit, HidEvent, MiGestureState, PerfMode};
use crate::ui::flyout::FlyoutWindow;
use crate::ui::flyout_layout::{FlyoutAction, FlyoutSnapshot};
use crate::ui::osd::OsdWindow;
use crate::ui::theme::{self, Rgb};
use crate::ui::tray::{TrayIcon, TrayMenuState};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::Win32::Foundation::HWND;
use windows::Win32::System::Power::HPOWERNOTIFY;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostQuitMessage, SetTimer};

pub const TIMER_CHARGE_GUARD: usize = 101;
pub const TIMER_MI_HOLD: usize = 102;
pub const TIMER_MI_DOUBLE: usize = 103;
pub const TIMER_AWAKE_EXPIRE: usize = 104;

pub struct AppController {
    pub hwnd: HWND,
    pub start_instant: Instant,
    pub cfg: AppConfig,
    pub mifs: Option<MifsClient>,
    pub touchpad: TouchpadManager,
    pub tray: TrayIcon,
    pub osd: OsdWindow,
    pub flyout: Option<FlyoutWindow>,
    pub current_mode: Option<PerfMode>,
    pub current_hz: Option<u32>,
    pub adapter_watts: Option<u8>,
    pub battery_soh: Option<u8>,
    pub on_ac: bool,
    pub awake_timer: AwakeTimer,
    pub mi_gesture: MiGestureState,
    pub last_perf_event_ms: u64,
    pub ai_key_down: bool,
    pub last_ai_event_ms: Option<u64>,
    pub hid_queue: Arc<Mutex<VecDeque<HidEvent>>>,
    pub slider_queue: Arc<Mutex<VecDeque<SliderFeedback>>>,
    pub hid_watcher: Option<HidEventWatcher>,
    pub power_notify: Option<HPOWERNOTIFY>,
}

impl AppController {
    pub fn now_ms(&self) -> u64 {
        self.start_instant.elapsed().as_millis() as u64
    }

    pub fn show_osd(&self, title: &str, subtitle: &str, accent: Rgb) {
        if self.cfg.osd_enabled {
            self.osd.show(title, subtitle, accent, self.cfg.osd_duration_ms);
        }
    }

    pub fn snapshot(&self) -> FlyoutSnapshot {
        let on_ac = crate::hardware::is_on_ac_power();
        let charge_supported = self
            .mifs
            .as_ref()
            .is_some_and(|m| m.dialect().supports_charge_group());
        FlyoutSnapshot {
            wmi_available: self.mifs.is_some(),
            on_ac,
            battery_pct: crate::hardware::battery_percent(),
            adapter_watts: self.adapter_watts,
            battery_soh: self.battery_soh,
            current_hz: self.current_hz,
            current_mode: self.current_mode,
            rejected_modes: self.cfg.rejected_for(on_ac).to_vec(),
            charge_care: self.cfg.charge_care,
            care_limit: self.cfg.charge_limit,
            travel_mode: self.cfg.travel_mode,
            charge_supported,
            awake: self.cfg.awake_enabled,
            awake_preset: self.awake_timer.active_preset(),
            awake_status: self
                .awake_timer
                .format_status(self.now_ms(), self.cfg.awake_min_battery_pct),
            smart_profiles: self.cfg.smart_profiles_enabled,
            ac_mode: self.cfg.ac_perf_mode,
            battery_mode: self.cfg.battery_perf_mode,
            auto_hz: self.cfg.profile_auto_hz,
            ac_hz: self.cfg.ac_refresh_hz,
            battery_hz: self.cfg.battery_refresh_hz,
            touchpad_available: self.touchpad.is_available(),
            touchpad_enabled: self.touchpad.is_enabled(),
            touchpad_dead_zone: self.cfg.touchpad_dead_zone,
            touchpad_dead_zone_mm: self.cfg.touchpad_dead_zone_mm,
            touchpad_edge_sliders: self.cfg.touchpad_edge_sliders,
            autostart: autostart::is_enabled(),
        }
    }

    pub fn tray_menu_state(&self) -> TrayMenuState {
        TrayMenuState {
            current_mode: self.current_mode,
            on_ac: self.on_ac,
            charge_care: self.cfg.charge_care,
            care_limit: self.cfg.charge_limit,
            travel_mode: self.cfg.travel_mode,
            awake: self.cfg.awake_enabled,
            smart_profiles: self.cfg.smart_profiles_enabled,
            current_hz: self.current_hz,
            touchpad_enabled: self.touchpad.is_enabled(),
            autostart: autostart::is_enabled(),
        }
    }

    pub fn refresh_tray(&mut self) {
        let mode_str = self
            .current_mode
            .map(|m| m.label())
            .unwrap_or("Unavailable");
        let care = self.cfg.desired_firmware_limit();
        let tip = if self.cfg.travel_mode {
            format!("MiControl · {mode_str} · Travel 100%")
        } else if care.is_care_active() {
            format!("MiControl · {mode_str} · Care {care}")
        } else {
            format!("MiControl · {mode_str} · 100%")
        };
        let care_opt = self
            .mifs
            .as_ref()
            .filter(|m| m.dialect().supports_charge_group())
            .map(|_| care);
        self.tray.update(self.current_mode, care_opt, &tip);
    }

    pub fn refresh_flyout_if_open(&self) {
        if let Some(ref f) = self.flyout
            && f.is_visible()
        {
            f.update_state(self.snapshot());
        }
    }

    /// Apply all persisted user settings from `config.json` on startup (config is authoritative
    /// at boot so stale EC/OS defaults never overwrite saved preferences).
    pub fn restore_saved_settings_on_startup(&mut self) {
        self.on_ac = crate::hardware::is_on_ac_power();
        self.current_hz = display::current_internal_hz();
        self.touchpad.sync_zones_if_drifted(&self.cfg);

        if let Some(want_hz) = smart_profile::preferred_startup_hz(&self.cfg, self.on_ac)
            && self.current_hz != Some(want_hz)
            && let Ok(applied) = display::set_internal_hz(want_hz)
        {
            self.current_hz = Some(applied);
        }

        let Some(ref mifs) = self.mifs else {
            return;
        };

        // 1. Enforce saved Battery Care / Travel limit BEFORE any readback reconciliation.
        if mifs.dialect().supports_charge_group() {
            let desired = self.cfg.desired_firmware_limit();
            let fw_cur = mifs.get_charge_limit().ok().flatten();
            if fw_cur != Some(desired) {
                let _ = mifs.set_charge_limit(desired, false);
            }
        }

        let actual_mode = mifs.get_perf_mode().ok().flatten();
        if let Some(want_mode) = smart_profile::preferred_startup_mode(&self.cfg, self.on_ac)
            && actual_mode != Some(want_mode)
        {
            match mifs.set_perf_mode(want_mode) {
                Ok(true) => self.current_mode = Some(want_mode),
                Ok(false) => {
                    if self.cfg.record_rejected(want_mode, self.on_ac) {
                        let _ = self.cfg.save();
                    }
                    self.current_mode = mifs.get_perf_mode().ok().flatten().or(actual_mode);
                }
                Err(_) => self.current_mode = actual_mode,
            }
        } else {
            self.current_mode = actual_mode;
        }

        if let Ok(watts) = mifs.get_adapter_watts() {
            self.adapter_watts = watts;
        }
        if let Ok(soh) = mifs.get_battery_health() {
            self.battery_soh = soh;
        }
    }

    pub fn sync_from_firmware(&mut self) {
        self.on_ac = crate::hardware::is_on_ac_power();
        self.current_hz = display::current_internal_hz();
        let Some(ref mifs) = self.mifs else {
            return;
        };
        if let Ok(mode) = mifs.get_perf_mode() {
            self.current_mode = mode;
        }
        if let Ok(fw_charge) = mifs.get_charge_limit()
            && charge_guard::reconcile_from_firmware(&mut self.cfg, fw_charge)
        {
            let _ = self.cfg.save();
        }
        if let Ok(watts) = mifs.get_adapter_watts() {
            self.adapter_watts = watts;
        }
        if let Ok(soh) = mifs.get_battery_health() {
            self.battery_soh = soh;
        }
    }

    pub fn run_charge_guard(&mut self, urgent: bool) {
        let Some(ref mifs) = self.mifs else {
            return;
        };
        let fw_cur = mifs.get_charge_limit().ok().flatten();
        if let charge_guard::GuardAction::Apply { target, urgent } =
            charge_guard::evaluate_rearm(&self.cfg, fw_cur, urgent)
        {
            let _ = mifs.set_charge_limit(target, urgent);
        }
    }

    pub fn set_mode(&mut self, mode: PerfMode, with_osd: bool) {
        let on_ac = crate::hardware::is_on_ac_power();
        if mode.requires_ac() && !on_ac {
            if with_osd {
                self.show_osd("Full Speed", "Requires AC power", theme::STATUS_FULL);
            }
            return;
        }
        let Some(ref mifs) = self.mifs else {
            if with_osd {
                self.show_osd("Firmware Unavailable", "Run as Administrator", theme::STATUS_FULL);
            }
            return;
        };
        match mifs.set_perf_mode(mode) {
            Ok(true) => {
                self.current_mode = Some(mode);
                self.cfg.remember_perf_mode(mode, on_ac);
                let _ = self.cfg.save();
                if with_osd {
                    self.show_osd("Performance Mode", mode.label(), theme::mode_color(mode));
                }
            }
            Ok(false) => {
                if self.cfg.record_rejected(mode, on_ac) {
                    let _ = self.cfg.save();
                }
                if let Ok(actual) = mifs.get_perf_mode() {
                    self.current_mode = actual;
                }
                if with_osd {
                    self.show_osd(
                        "Mode Rejected",
                        &format!("{} unsupported by BIOS", mode.label()),
                        theme::STATUS_FULL,
                    );
                }
            }
            Err(err) => {
                if with_osd {
                    self.show_osd("WMI Error", &err, theme::STATUS_FULL);
                }
            }
        }
        self.refresh_tray();
        self.refresh_flyout_if_open();
    }

    pub fn cycle_mode(&mut self) {
        let on_ac = crate::hardware::is_on_ac_power();
        if let Some(next) =
            protocol::next_perf_mode(self.current_mode, on_ac, self.cfg.rejected_for(on_ac))
        {
            self.set_mode(next, true);
        }
    }

    pub fn toggle_charge_care(&mut self) {
        self.cfg.travel_mode = false;
        self.cfg.charge_care = !self.cfg.charge_care;
        let target = self.cfg.desired_firmware_limit();
        let ok = self
            .mifs
            .as_ref()
            .and_then(|m| m.set_charge_limit(target, false).ok())
            .unwrap_or(false);
        if ok {
            let _ = self.cfg.save();
            let sub = if target.is_care_active() {
                format!("Capped at {target}")
            } else {
                "Full 100%".into()
            };
            self.show_osd(
                "Battery Protection",
                &sub,
                if target.is_care_active() {
                    theme::STATUS_ECO
                } else {
                    theme::ACCENT
                },
            );
        } else {
            self.cfg.charge_care = !self.cfg.charge_care;
            self.show_osd("Battery Protection", "Rejected by firmware", theme::STATUS_FULL);
        }
        self.refresh_tray();
        self.refresh_flyout_if_open();
    }

    pub fn set_charge_preset(&mut self, preset: ChargeLimit) {
        self.cfg.charge_limit = preset;
        self.cfg.charge_care = true;
        self.cfg.travel_mode = false;
        let ok = self
            .mifs
            .as_ref()
            .and_then(|m| m.set_charge_limit(preset, false).ok())
            .unwrap_or(false);
        if ok {
            let _ = self.cfg.save();
            self.show_osd("Battery Care Limit", &format!("Capped at {preset}"), theme::STATUS_ECO);
        } else {
            self.show_osd(
                "Battery Care Limit",
                &format!("{preset} rejected by firmware"),
                theme::STATUS_FULL,
            );
        }
        self.refresh_tray();
        self.refresh_flyout_if_open();
    }

    pub fn toggle_travel_mode(&mut self) {
        self.cfg.travel_mode = !self.cfg.travel_mode;
        let target = self.cfg.desired_firmware_limit();
        let ok = self
            .mifs
            .as_ref()
            .and_then(|m| m.set_charge_limit(target, false).ok())
            .unwrap_or(false);
        if ok {
            let _ = self.cfg.save();
            if self.cfg.travel_mode {
                self.show_osd("Travel Mode", "Charging to 100% once", theme::STATUS_TURBO);
            } else {
                self.show_osd("Travel Mode Off", &format!("Restored {target}"), theme::STATUS_ECO);
            }
        } else {
            self.cfg.travel_mode = !self.cfg.travel_mode;
            self.show_osd("Travel Mode", "Failed to apply", theme::STATUS_FULL);
        }
        self.refresh_tray();
        self.refresh_flyout_if_open();
    }

    pub fn toggle_awake(&mut self) {
        if self.cfg.awake_enabled {
            self.stop_awake("Off · Normal sleep", theme::INK_SECONDARY);
        } else {
            self.start_awake(AwakeDuration::Forever);
        }
    }

    pub fn start_awake(&mut self, dur: AwakeDuration) {
        let on_ac = crate::hardware::is_on_ac_power();
        let bat = crate::hardware::battery_percent();
        if smart_profile::should_cutoff_awake(true, on_ac, bat, self.cfg.awake_min_battery_pct) {
            self.show_osd(
                "Keep Awake Blocked",
                &format!("Battery ≤ {}% safety cutoff", self.cfg.awake_min_battery_pct),
                theme::STATUS_TURBO,
            );
            return;
        }
        if !awake::enable(&mut self.cfg) {
            self.show_osd("Keep Awake", "Failed to lock power plan", theme::STATUS_FULL);
            return;
        }
        let _ = self.cfg.save();
        let now = self.now_ms();
        // SAFETY: Manages one-shot expiration timer on owned host HWND.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_AWAKE_EXPIRE);
            if let Some(ms) = self.awake_timer.arm(dur, now) {
                let _ = SetTimer(Some(self.hwnd), TIMER_AWAKE_EXPIRE, ms, None);
            }
        }
        let guard = if self.cfg.awake_min_battery_pct > 0 {
            format!("Lid safe · ≤{}% guard", self.cfg.awake_min_battery_pct)
        } else {
            "Lid sleep blocked".into()
        };
        self.show_osd(&format!("Keep Awake ({})", dur.label()), &guard, theme::ACCENT);
        self.refresh_flyout_if_open();
    }

    pub fn stop_awake(&mut self, reason: &str, accent: Rgb) {
        // SAFETY: Cancels any pending expiration timer on owned host HWND.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_AWAKE_EXPIRE);
        }
        self.awake_timer.clear();
        awake::disable(&mut self.cfg);
        let _ = self.cfg.save();
        self.show_osd("Keep Awake", reason, accent);
        self.refresh_flyout_if_open();
    }

    pub fn on_awake_timer_expired(&mut self) {
        if self.cfg.awake_enabled && self.awake_timer.is_expired(self.now_ms()) {
            self.stop_awake("Timer finished · Sleep restored", theme::INK_SECONDARY);
        }
    }

    pub fn toggle_smart_profiles(&mut self) {
        self.cfg.smart_profiles_enabled = !self.cfg.smart_profiles_enabled;
        let _ = self.cfg.save();
        let sub = if self.cfg.smart_profiles_enabled {
            format!(
                "AC {} {}Hz · Bat {} {}Hz",
                self.cfg.ac_perf_mode,
                self.cfg.ac_refresh_hz,
                self.cfg.battery_perf_mode,
                self.cfg.battery_refresh_hz
            )
        } else {
            "Off · manual mode & Hz".into()
        };
        self.show_osd("Smart AC/Battery", &sub, theme::ACCENT);
        self.refresh_flyout_if_open();
    }

    pub fn cycle_refresh_rate(&mut self) {
        match display::cycle_internal_hz() {
            Ok(hz) => {
                self.current_hz = Some(hz);
                let on_ac = crate::hardware::is_on_ac_power();
                self.cfg.remember_refresh_hz(hz, on_ac);
                let _ = self.cfg.save();
                self.show_osd("Display Refresh Rate", &format!("{hz} Hz"), theme::STATUS_AUTO);
            }
            Err(err) => {
                self.show_osd("Display Refresh Rate", &err, theme::STATUS_FULL);
            }
        }
        self.refresh_flyout_if_open();
    }

    pub fn toggle_touchpad(&mut self) {
        match self.touchpad.toggle() {
            Some(true) => self.show_osd("Touchpad", "Enabled", theme::ACCENT),
            Some(false) => self.show_osd("Touchpad", "Disabled", theme::STATUS_TURBO),
            None => self.show_osd("Touchpad", "Not available", theme::STATUS_FULL),
        }
        self.refresh_flyout_if_open();
    }

    pub fn toggle_dead_zone(&mut self) {
        self.cfg.touchpad_dead_zone = !self.cfg.touchpad_dead_zone;
        let _ = self.cfg.save();
        let ok = self.touchpad.apply_zones(&self.cfg);
        let sub = if !ok {
            "Registry write failed (Admin required)".into()
        } else if self.cfg.touchpad_dead_zone {
            format!("Enabled ({} mm bottom guard)", self.cfg.touchpad_dead_zone_mm)
        } else {
            "Disabled".into()
        };
        self.show_osd(
            "Touchpad Dead-Zone",
            &sub,
            if ok { theme::ACCENT } else { theme::STATUS_FULL },
        );
        self.refresh_flyout_if_open();
    }

    pub fn toggle_edge_sliders(&mut self) {
        self.cfg.touchpad_edge_sliders = !self.cfg.touchpad_edge_sliders;
        let _ = self.cfg.save();
        let _ = self.touchpad.apply_zones(&self.cfg);
        self.touchpad.sync_edge_reader(&self.cfg);
        let sub = if self.cfg.touchpad_edge_sliders {
            "Enabled (Left: Bright · Right: Vol)"
        } else {
            "Disabled"
        };
        self.show_osd("Edge Sliders", sub, theme::ACCENT);
        self.refresh_flyout_if_open();
    }

    pub fn toggle_autostart(&mut self) {
        let next = !autostart::is_enabled();
        match autostart::set_enabled(next) {
            Ok(()) => self.show_osd(
                "Start at Login",
                if next { "Enabled (Task Scheduler)" } else { "Disabled" },
                theme::ACCENT,
            ),
            Err(_) => self.show_osd("Start at Login", "Failed (Admin required)", theme::STATUS_FULL),
        }
        self.refresh_flyout_if_open();
    }

    pub fn toggle_flyout(&mut self) {
        let visible = self.flyout.as_ref().is_some_and(|f| f.is_visible());
        if visible {
            if let Some(ref f) = self.flyout {
                f.hide();
            }
        } else {
            self.sync_from_firmware();
            self.refresh_tray();
            let snap = self.snapshot();
            if let Some(ref f) = self.flyout {
                f.show(snap);
            }
        }
    }

    pub fn handle_flyout_action(&mut self, action: FlyoutAction) {
        match action {
            FlyoutAction::SetMode(m) => self.set_mode(m, true),
            FlyoutAction::ToggleChargeCare => self.toggle_charge_care(),
            FlyoutAction::ToggleTravelMode => self.toggle_travel_mode(),
            FlyoutAction::SetChargePreset(p) => self.set_charge_preset(p),
            FlyoutAction::ToggleAwake => self.toggle_awake(),
            FlyoutAction::SetAwakeDuration(d) => self.start_awake(d),
            FlyoutAction::ToggleSmartProfiles => self.toggle_smart_profiles(),
            FlyoutAction::CycleRefreshRate => self.cycle_refresh_rate(),
            FlyoutAction::ToggleTouchpad => self.toggle_touchpad(),
            FlyoutAction::ToggleDeadZone => self.toggle_dead_zone(),
            FlyoutAction::ToggleEdgeSliders => self.toggle_edge_sliders(),
            FlyoutAction::ToggleAutostart => self.toggle_autostart(),
            // SAFETY: Ends the application's Win32 message loop cleanly.
            FlyoutAction::ExitApp => unsafe { PostQuitMessage(0) },
        }
    }
}
