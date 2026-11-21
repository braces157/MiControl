//! Pure state machines for:
//! 1. **Timed Keep-Awake (Amphetamine / `caffeinate`)** with `<20%` low-battery backpack safety cutoff.
//! 2. **AC / Battery Smart Profiles** (automatic Performance Mode, built-in panel Hz, and AC/Battery brightness memory).

use crate::config::AppConfig;
use crate::protocol::PerfMode;

/// Selectable Keep-Awake (Amphetamine) duration presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwakeDuration {
    Min30,
    Hour1,
    Hour2,
    Forever,
}

impl AwakeDuration {
    pub const PRESETS: [AwakeDuration; 4] = [
        AwakeDuration::Min30,
        AwakeDuration::Hour1,
        AwakeDuration::Hour2,
        AwakeDuration::Forever,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Min30 => "30m",
            Self::Hour1 => "1h",
            Self::Hour2 => "2h",
            Self::Forever => "∞",
        }
    }

    pub const fn duration_ms(self) -> Option<u64> {
        match self {
            Self::Min30 => Some(30 * 60 * 1000),
            Self::Hour1 => Some(60 * 60 * 1000),
            Self::Hour2 => Some(120 * 60 * 1000),
            Self::Forever => None,
        }
    }

    pub fn parse_cli(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "30m" | "30" => Some(Self::Min30),
            "1h" | "60m" | "60" => Some(Self::Hour1),
            "2h" | "120m" | "120" => Some(Self::Hour2),
            "on" | "inf" | "forever" | "always" | "1" | "true" => Some(Self::Forever),
            _ => None,
        }
    }
}

/// Deterministic monotonic countdown tracker for Timed Keep-Awake (zero polling).
#[derive(Debug, Clone, Default)]
pub struct AwakeTimer {
    active_preset: Option<AwakeDuration>,
    deadline_ms: Option<u64>,
}

impl AwakeTimer {
    /// Start or update the timer at monotonic `now_ms`.
    /// Returns optional `SetTimer` duration (ms) for one-shot OS expiration.
    pub fn arm(&mut self, preset: AwakeDuration, now_ms: u64) -> Option<u32> {
        self.active_preset = Some(preset);
        match preset.duration_ms() {
            Some(dur) => {
                self.deadline_ms = Some(now_ms.saturating_add(dur));
                Some(dur as u32)
            }
            None => {
                self.deadline_ms = None;
                None
            }
        }
    }

    pub fn clear(&mut self) {
        self.active_preset = None;
        self.deadline_ms = None;
    }

    pub const fn active_preset(&self) -> Option<AwakeDuration> {
        self.active_preset
    }

    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.deadline_ms.is_some_and(|d| now_ms >= d)
    }

    /// Remaining minutes (rounded up, minimum 1) if a timed countdown is active.
    pub fn remaining_minutes(&self, now_ms: u64) -> Option<u32> {
        let dl = self.deadline_ms?;
        let rem_ms = dl.saturating_sub(now_ms);
        if rem_ms == 0 {
            Some(0)
        } else {
            Some(rem_ms.div_ceil(60_000) as u32)
        }
    }

    /// Human-readable status for the Flyout subtitle.
    pub fn format_status(&self, now_ms: u64, min_battery_pct: u8) -> String {
        let guard = if min_battery_pct > 0 {
            format!(" · ≤{min_battery_pct}% safe")
        } else {
            String::new()
        };
        match self.remaining_minutes(now_ms) {
            Some(mins) if mins >= 60 => {
                let h = mins / 60;
                let m = mins % 60;
                if m == 0 {
                    format!("{h}h left{guard}")
                } else {
                    format!("{h}h {m}m left{guard}")
                }
            }
            Some(mins) => format!("{mins}m left{guard}"),
            None => format!("∞ active{guard}"),
        }
    }
}

/// Low-battery backpack safety cutoff check (triggers strictly while unplugged on battery).
pub const fn should_cutoff_awake(
    awake_enabled: bool,
    on_ac: bool,
    battery_pct: Option<u8>,
    min_battery_pct: u8,
) -> bool {
    if !awake_enabled || on_ac || min_battery_pct == 0 {
        return false;
    }
    match battery_pct {
        Some(pct) => pct <= min_battery_pct,
        None => false,
    }
}

/// Concrete hardware transitions computed when switching between AC and Battery power.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProfileActions {
    pub target_mode: Option<PerfMode>,
    pub target_hz: Option<u32>,
    pub target_brightness: Option<u8>,
}

/// Update remembered brightness for the *previous* power source and compute targets for `now_ac`.
pub fn plan_power_transition(
    cfg: &mut AppConfig,
    was_ac: bool,
    now_ac: bool,
    current_brightness: Option<u8>,
) -> Option<ProfileActions> {
    if !cfg.smart_profiles_enabled || was_ac == now_ac {
        return None;
    }

    // Remember current manual brightness for the power source we are leaving.
    if cfg.profile_remember_brightness
        && let Some(b) = current_brightness
    {
        let clamped = b.min(100);
        if was_ac {
            cfg.ac_brightness = Some(clamped);
        } else {
            cfg.battery_brightness = Some(clamped);
        }
    }

    let target_mode = if now_ac {
        Some(cfg.ac_perf_mode)
    } else {
        Some(cfg.battery_perf_mode)
    };
    let target_hz = if cfg.profile_auto_hz {
        Some(if now_ac { cfg.ac_refresh_hz } else { cfg.battery_refresh_hz })
    } else {
        None
    };
    let target_brightness = if cfg.profile_remember_brightness {
        if now_ac {
            cfg.ac_brightness
        } else {
            cfg.battery_brightness
        }
    } else {
        None
    };

    Some(ProfileActions {
        target_mode,
        target_hz,
        target_brightness,
    })
}

/// Compute the remembered performance mode to restore at startup for the current power source.
pub fn preferred_startup_mode(cfg: &AppConfig, on_ac: bool) -> Option<PerfMode> {
    let target = if cfg.smart_profiles_enabled {
        if on_ac {
            cfg.ac_perf_mode
        } else {
            cfg.battery_perf_mode
        }
    } else {
        cfg.last_perf_mode?
    };
    if target.requires_ac() && !on_ac {
        return None;
    }
    if cfg.rejected_for(on_ac).contains(&target) {
        return None;
    }
    Some(target)
}

/// Compute the remembered internal panel refresh rate to restore at startup for the current power source.
pub const fn preferred_startup_hz(cfg: &AppConfig, on_ac: bool) -> Option<u32> {
    if cfg.smart_profiles_enabled && cfg.profile_auto_hz {
        Some(if on_ac {
            cfg.ac_refresh_hz
        } else {
            cfg.battery_refresh_hz
        })
    } else {
        cfg.last_refresh_hz
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awake_timer_arms_counts_down_and_expires() {
        let mut t = AwakeTimer::default();
        assert_eq!(t.arm(AwakeDuration::Min30, 1000), Some(1_800_000));
        assert_eq!(t.remaining_minutes(1000), Some(30));
        assert_eq!(t.format_status(1000 + 5 * 60_000, 20), "25m left · ≤20% safe");
        assert!(!t.is_expired(1000 + 1_799_999));
        assert!(t.is_expired(1000 + 1_800_000));

        assert_eq!(t.arm(AwakeDuration::Forever, 5000), None);
        assert_eq!(t.format_status(999_999, 20), "∞ active · ≤20% safe");
        assert!(!t.is_expired(999_999));
    }

    #[test]
    fn battery_safety_cutoff_only_fires_on_battery_at_or_below_threshold() {
        assert!(!should_cutoff_awake(false, false, Some(15), 20));
        assert!(!should_cutoff_awake(true, true, Some(15), 20)); // plugged in -> safe
        assert!(!should_cutoff_awake(true, false, Some(21), 20));
        assert!(should_cutoff_awake(true, false, Some(20), 20));
        assert!(should_cutoff_awake(true, false, Some(12), 20));
        assert!(!should_cutoff_awake(true, false, Some(10), 0)); // disabled
    }

    #[test]
    fn smart_profile_remembers_brightness_and_plans_ac_battery_switch() {
        let mut cfg = AppConfig {
            smart_profiles_enabled: true,
            ac_perf_mode: PerfMode::Auto,
            battery_perf_mode: PerfMode::Quiet,
            profile_auto_hz: true,
            ac_refresh_hz: 120,
            battery_refresh_hz: 60,
            profile_remember_brightness: true,
            ac_brightness: Some(85),
            battery_brightness: Some(45),
            ..Default::default()
        };

        // Unplug while screen is at 90% -> saves 90% for AC, applies Battery profile (Quiet, 60Hz, 45%)
        let act = plan_power_transition(&mut cfg, true, false, Some(90)).expect("transition");
        assert_eq!(cfg.ac_brightness, Some(90));
        assert_eq!(act.target_mode, Some(PerfMode::Quiet));
        assert_eq!(act.target_hz, Some(60));
        assert_eq!(act.target_brightness, Some(45));

        // Plug back in while screen is at 40% -> saves 40% for Battery, restores AC (Auto, 120Hz, 90%)
        let act2 = plan_power_transition(&mut cfg, false, true, Some(40)).expect("transition");
        assert_eq!(cfg.battery_brightness, Some(40));
        assert_eq!(act2.target_mode, Some(PerfMode::Auto));
        assert_eq!(act2.target_hz, Some(120));
        assert_eq!(act2.target_brightness, Some(90));

        // No-op when disabled or unchanged power state
        assert_eq!(plan_power_transition(&mut cfg, true, true, Some(80)), None);
        cfg.smart_profiles_enabled = false;
        assert_eq!(plan_power_transition(&mut cfg, true, false, Some(80)), None);
    }

    #[test]
    fn startup_restore_picks_saved_mode_and_hz_respecting_power_and_rejections() {
        let mut cfg = AppConfig {
            smart_profiles_enabled: true,
            ac_perf_mode: PerfMode::FullSpeed,
            battery_perf_mode: PerfMode::Eco,
            profile_auto_hz: true,
            ac_refresh_hz: 120,
            battery_refresh_hz: 60,
            last_perf_mode: Some(PerfMode::Turbo),
            last_refresh_hz: Some(100),
            ..Default::default()
        };
        assert_eq!(preferred_startup_mode(&cfg, true), Some(PerfMode::FullSpeed));
        assert_eq!(preferred_startup_mode(&cfg, false), Some(PerfMode::Eco));
        assert_eq!(preferred_startup_hz(&cfg, true), Some(120));
        assert_eq!(preferred_startup_hz(&cfg, false), Some(60));

        // Smart profiles off -> restores last explicitly chosen mode/Hz, but blocks FullSpeed on battery
        cfg.smart_profiles_enabled = false;
        assert_eq!(preferred_startup_mode(&cfg, false), Some(PerfMode::Turbo));
        assert_eq!(preferred_startup_hz(&cfg, false), Some(100));
        cfg.last_perf_mode = Some(PerfMode::FullSpeed);
        assert_eq!(preferred_startup_mode(&cfg, false), None);

        // Rejected mode on AC is skipped
        cfg.record_rejected(PerfMode::FullSpeed, true);
        assert_eq!(preferred_startup_mode(&cfg, true), None);
    }
}
