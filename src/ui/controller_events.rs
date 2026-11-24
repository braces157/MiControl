//! Hardware event routing (`HID_EVENT20`, Mi-button gestures, and AC/Battery power transitions)
//! for `AppController`.

use crate::hardware::{audio, display, smart_profile};
use crate::protocol::{HidEvent, MiGesture};
use crate::ui::controller::{
    AppController, TIMER_CHARGE_GUARD, TIMER_MI_DOUBLE, TIMER_MI_HOLD,
};
use crate::ui::theme;
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

impl AppController {
    pub fn handle_mi_gesture(&mut self, gesture: MiGesture) {
        match gesture {
            MiGesture::Click => self.cycle_mode(),
            MiGesture::DoubleClick => self.toggle_charge_care(),
            MiGesture::Hold => self.toggle_flyout(),
        }
    }

    pub fn handle_hid_event(&mut self, ev: HidEvent) {
        let now = self.now_ms();
        match ev {
            HidEvent::MiDown => {
                if let Some(ms) = self.mi_gesture.on_down(now) {
                    // SAFETY: One-shot timer on owned host HWND.
                    unsafe {
                        let _ = SetTimer(Some(self.hwnd), TIMER_MI_HOLD, ms as u32, None);
                    }
                }
            }
            HidEvent::MiUp => {
                // SAFETY: Cancels pending hold timer on owned host HWND.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_MI_HOLD);
                }
                let (gesture, dbl_ms) = self.mi_gesture.on_up(now);
                if let Some(g) = gesture {
                    unsafe {
                        let _ = KillTimer(Some(self.hwnd), TIMER_MI_DOUBLE);
                    }
                    self.handle_mi_gesture(g);
                } else if let Some(ms) = dbl_ms {
                    unsafe {
                        let _ = SetTimer(Some(self.hwnd), TIMER_MI_DOUBLE, ms as u32, None);
                    }
                }
            }
            HidEvent::PerfKey { .. } => {
                if now.saturating_sub(self.last_perf_event_ms) < 350 {
                    return;
                }
                self.last_perf_event_ms = now;
                if let Some(ref mifs) = self.mifs
                    && let Ok(Some(actual)) = mifs.get_perf_mode()
                {
                    self.current_mode = Some(actual);
                    let on_ac = crate::hardware::is_on_ac_power();
                    self.cfg.remember_perf_mode(actual, on_ac);
                    let _ = self.cfg.save();
                    self.show_osd("Performance Mode", actual.label(), theme::mode_color(actual));
                    self.refresh_tray();
                    self.refresh_flyout_if_open();
                }
            }
            HidEvent::MicMute { muted } => {
                let _ = audio::set_mic_mute(muted);
                self.show_osd(
                    "Microphone",
                    if muted { "Muted" } else { "Unmuted" },
                    if muted { theme::STATUS_FULL } else { theme::STATUS_ECO },
                );
            }
            HidEvent::KbdBacklight(lvl) => {
                self.show_osd("Keyboard Backlight", lvl.label(), theme::ACCENT);
            }
            HidEvent::FnLock { standard_fn } => {
                self.show_osd(
                    "Fn-Lock",
                    if standard_fn { "Standard F1–F12" } else { "Media Hotkeys" },
                    theme::ACCENT,
                );
            }
            HidEvent::CapsLock { on } => {
                self.show_osd("Caps Lock", if on { "On" } else { "Off" }, theme::ACCENT);
            }
            HidEvent::NumLock { on } => {
                self.show_osd("Num Lock", if on { "On" } else { "Off" }, theme::ACCENT);
            }
            HidEvent::WinLock { locked } => {
                self.show_osd(
                    "Windows Key",
                    if locked { "Locked" } else { "Unlocked" },
                    if locked { theme::STATUS_TURBO } else { theme::ACCENT },
                );
            }
            HidEvent::TouchpadState { enabled } => {
                self.show_osd(
                    "Touchpad",
                    if enabled { "Enabled" } else { "Disabled" },
                    if enabled { theme::ACCENT } else { theme::STATUS_TURBO },
                );
                self.refresh_flyout_if_open();
            }
            HidEvent::TouchpadToggle => self.toggle_touchpad(),
            HidEvent::RefreshRateKey => self.cycle_refresh_rate(),
            HidEvent::SettingsKey => self.toggle_charge_care(),
            HidEvent::ProjectionKey => crate::hardware::trigger_projection(),
            HidEvent::ScreenshotKey => crate::hardware::trigger_screenshot(),
            HidEvent::TaskViewKey => crate::hardware::trigger_task_view(),
            HidEvent::CameraPrivacy { blocked } => {
                self.show_osd(
                    "Camera Privacy",
                    if blocked { "Blocked" } else { "Active" },
                    if blocked { theme::STATUS_FULL } else { theme::STATUS_ECO },
                );
            }
            HidEvent::LowPowerWarning => {
                self.show_osd("Weak Charger", "Insufficient PD wattage", theme::STATUS_TURBO);
            }
            HidEvent::AiDown => {
                if should_trigger_ai_key(&mut self.ai_key_down, &mut self.last_ai_event_ms, now) {
                    self.toggle_flyout();
                }
            }
            HidEvent::AiUp => {
                self.ai_key_down = false;
            }
            HidEvent::Unknown { .. } => {}
        }
    }

    pub fn handle_power_change(&mut self) {
        let was_ac = self.on_ac;
        let now_ac = crate::hardware::is_on_ac_power();
        let unplugged = was_ac && !now_ac;
        let plugged_in = !was_ac && now_ac;
        self.on_ac = now_ac;

        if smart_profile::should_cutoff_awake(
            self.cfg.awake_enabled,
            now_ac,
            crate::hardware::battery_percent(),
            self.cfg.awake_min_battery_pct,
        ) {
            self.stop_awake(
                &format!("Stopped · Battery ≤ {}%", self.cfg.awake_min_battery_pct),
                theme::STATUS_TURBO,
            );
        }

        if unplugged && self.cfg.travel_mode {
            self.cfg.travel_mode = false;
            let _ = self.cfg.save();
        }

        if was_ac != now_ac {
            let cur_bright = self.mifs.as_ref().and_then(|m| m.get_display_brightness());
            if let Some(plan) =
                smart_profile::plan_power_transition(&mut self.cfg, was_ac, now_ac, cur_bright)
            {
                let _ = self.cfg.save();
                let mut summary = Vec::new();
                if let Some(mode) = plan.target_mode
                    && (!mode.requires_ac() || now_ac)
                    && let Some(ref mifs) = self.mifs
                    && mifs.set_perf_mode(mode).unwrap_or(false)
                {
                    self.current_mode = Some(mode);
                    summary.push(mode.label().to_string());
                }
                if let Some(hz) = plan.target_hz
                    && let Ok(applied) = display::set_internal_hz(hz)
                {
                    self.current_hz = Some(applied);
                    summary.push(format!("{applied} Hz"));
                }
                if let Some(b) = plan.target_brightness
                    && let Some(ref mifs) = self.mifs
                    && mifs.set_display_brightness(b)
                {
                    summary.push(format!("{b}% bright"));
                }
                if !summary.is_empty() {
                    let title = if now_ac { "AC Profile" } else { "Battery Profile" };
                    self.show_osd(title, &summary.join(" · "), theme::ACCENT);
                }
            }
        }

        if plugged_in
            && self.cfg.weak_charger_watts > 0
            && let Some(Ok(Some(w))) = self.mifs.as_ref().map(|m| m.get_adapter_watts())
        {
            self.adapter_watts = Some(w);
            if w > 0 && w < self.cfg.weak_charger_watts {
                self.show_osd(
                    "Weak PD Charger",
                    &format!("{w}W detected (< {}W)", self.cfg.weak_charger_watts),
                    theme::STATUS_TURBO,
                );
            }
        }

        // SAFETY: Debounce ChargeGuard re-arm 1.5s after power transition.
        unsafe {
            let _ = SetTimer(Some(self.hwnd), TIMER_CHARGE_GUARD, 1500, None);
        }
        self.refresh_tray();
        self.refresh_flyout_if_open();
    }
}

const AI_KEY_DEBOUNCE_MS: u64 = 180;
const AI_KEY_STUCK_RECOVERY_MS: u64 = 1500;

/// Pure edge-trigger + debounce + missed-release recovery guard for `Fn+F7` (`HID_EVENT20` `0x23`/`0x24`).
pub(crate) fn should_trigger_ai_key(
    is_down: &mut bool,
    last_trigger_ms: &mut Option<u64>,
    now_ms: u64,
) -> bool {
    if *is_down {
        let stuck = last_trigger_ms
            .is_some_and(|prev| now_ms.saturating_sub(prev) >= AI_KEY_STUCK_RECOVERY_MS);
        if !stuck {
            return false;
        }
    }
    if let Some(prev) = *last_trigger_ms
        && now_ms.saturating_sub(prev) < AI_KEY_DEBOUNCE_MS
    {
        *is_down = true;
        return false;
    }
    *is_down = true;
    *last_trigger_ms = Some(now_ms);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_key_toggles_cleanly_with_repeat_debounce_and_stuck_recovery() {
        let mut down = false;
        let mut last = None;

        // First press immediately after startup (even < debounce window) must trigger.
        assert!(should_trigger_ai_key(&mut down, &mut last, 20));
        assert!(down);

        // Held key repeat while still down must NOT re-trigger.
        assert!(!should_trigger_ai_key(&mut down, &mut last, 300));
        assert!(!should_trigger_ai_key(&mut down, &mut last, 600));

        // Release then rapid bounce (< 180 ms since trigger) must NOT double-toggle.
        down = false;
        assert!(!should_trigger_ai_key(&mut down, &mut last, 150));

        // Release then deliberate second press (>= 180 ms) triggers toggle off.
        down = false;
        assert!(should_trigger_ai_key(&mut down, &mut last, 350));

        // Missed AiUp event must auto-recover after 1.5s so the hotkey never wedges.
        assert!(should_trigger_ai_key(&mut down, &mut last, 1900));
    }
}
