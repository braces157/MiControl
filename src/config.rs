//! Minimal, zero-bloat JSON configuration persisted atomically to `%APPDATA%\MiControl\config.json`.

use crate::protocol::{ChargeLimit, PerfMode};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Whether battery charge protection is enabled.
    pub charge_care: bool,
    /// Target charge cap when `charge_care` is enabled (40/50/60/70/80%).
    pub charge_limit: ChargeLimit,
    /// One-off temporary override to charge to 100% until unplugged or toggled off.
    pub travel_mode: bool,
    /// Performance modes known to be rejected by this laptop's BIOS on AC.
    pub rejected_modes_ac: Vec<PerfMode>,
    /// Performance modes known to be rejected by this laptop's BIOS on battery.
    pub rejected_modes_battery: Vec<PerfMode>,
    /// Last user-selected performance mode (restored on startup when Smart Profiles is off).
    pub last_perf_mode: Option<PerfMode>,
    /// Last user-selected internal panel refresh rate in Hz (restored on startup when auto-Hz is off).
    pub last_refresh_hz: Option<u32>,
    /// Keep-Awake (Owl mode): prevents sleep and lid-close suspend on AC.
    pub awake_enabled: bool,
    /// When Owl mode is on, still allow the screen to turn off while keeping system awake.
    pub awake_allow_display_off: bool,
    /// Automatically disable Keep-Awake when on battery at or below this % (0 = disabled).
    pub awake_min_battery_pct: u8,
    /// Crash-safe restoration state for Windows AC Lid Close Action before Owl mode overwrote it.
    pub saved_lid_ac: Option<u32>,
    /// Power scheme GUID string whose AC lid action was saved.
    pub saved_lid_scheme: Option<String>,
    /// Automatically switch performance mode, built-in refresh rate, and brightness on AC/Battery change.
    pub smart_profiles_enabled: bool,
    /// Performance mode applied when plugged into AC (when Smart Profiles is enabled).
    pub ac_perf_mode: PerfMode,
    /// Performance mode applied when switching to Battery (when Smart Profiles is enabled).
    pub battery_perf_mode: PerfMode,
    /// Switch internal panel refresh rate automatically on AC/Battery change.
    pub profile_auto_hz: bool,
    /// Preferred internal panel refresh rate on AC (Hz).
    pub ac_refresh_hz: u32,
    /// Preferred internal panel refresh rate on Battery (Hz).
    pub battery_refresh_hz: u32,
    /// Remember and restore display brightness separately for AC vs Battery.
    pub profile_remember_brightness: bool,
    /// Last recorded display brightness on AC (0..=100).
    pub ac_brightness: Option<u8>,
    /// Last recorded display brightness on Battery (0..=100).
    pub battery_brightness: Option<u8>,
    /// Precision Touchpad bottom dead-zone enabled (`SuperCurtainBottom`).
    pub touchpad_dead_zone: bool,
    /// Bottom dead-zone height in mm (clamped 1..=40).
    pub touchpad_dead_zone_mm: u32,
    /// Precision Touchpad left/right edge sliders (left = brightness, right = volume).
    pub touchpad_edge_sliders: bool,
    /// Edge slider strip width in mm (clamped 4..=30).
    pub touchpad_edge_width_mm: u32,
    /// Swap left/right edge slider roles (default: left = brightness, right = volume).
    pub touchpad_edge_swap: bool,
    /// Show lightweight OSD toasts on hotkey / mode / charge changes.
    pub osd_enabled: bool,
    /// OSD visibility duration in milliseconds.
    pub osd_duration_ms: u32,
    /// Mi-button long-press threshold (ms).
    pub mi_hold_ms: u64,
    /// Mi-button double-click window (ms).
    pub mi_double_click_ms: u64,
    /// Warn on weak PD charger below this wattage (0 = disabled).
    pub weak_charger_watts: u8,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            charge_care: true,
            charge_limit: ChargeLimit::P80,
            travel_mode: false,
            rejected_modes_ac: Vec::new(),
            rejected_modes_battery: Vec::new(),
            last_perf_mode: None,
            last_refresh_hz: None,
            awake_enabled: false,
            awake_allow_display_off: false,
            awake_min_battery_pct: 20,
            saved_lid_ac: None,
            saved_lid_scheme: None,
            smart_profiles_enabled: true,
            ac_perf_mode: PerfMode::Auto,
            battery_perf_mode: PerfMode::Quiet,
            profile_auto_hz: true,
            ac_refresh_hz: 120,
            battery_refresh_hz: 60,
            profile_remember_brightness: true,
            ac_brightness: None,
            battery_brightness: None,
            touchpad_dead_zone: false,
            touchpad_dead_zone_mm: 12,
            touchpad_edge_sliders: false,
            touchpad_edge_width_mm: 12,
            touchpad_edge_swap: false,
            osd_enabled: true,
            osd_duration_ms: 1800,
            mi_hold_ms: 400,
            mi_double_click_ms: 300,
            weak_charger_watts: 65,
        }
    }
}

impl AppConfig {
    pub fn default_path() -> PathBuf {
        let base = std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        base.join("MiControl").join("config.json")
    }

    pub fn load_from(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(raw) => match serde_json::from_str::<Self>(&raw) {
                Ok(mut cfg) => {
                    cfg.sanitize();
                    cfg
                }
                Err(_) => Self::default(),
            },
            Err(_) => Self::default(),
        }
    }

    pub fn load() -> Self {
        Self::load_from(&Self::default_path())
    }

    pub fn save_to(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, json)?;
        // Atomic replace on Windows (best effort fallback to remove+rename)
        if fs::rename(&tmp, path).is_err() {
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)?;
        }
        Ok(())
    }

    pub fn save(&self) -> io::Result<()> {
        self.save_to(&Self::default_path())
    }

    pub fn sanitize(&mut self) {
        if !self.charge_limit.is_care_active() {
            self.charge_limit = ChargeLimit::P80;
        }
        self.touchpad_dead_zone_mm = self.touchpad_dead_zone_mm.clamp(1, 40);
        self.touchpad_edge_width_mm = self.touchpad_edge_width_mm.clamp(4, 30);
        self.osd_duration_ms = self.osd_duration_ms.clamp(500, 6000);
        self.mi_hold_ms = self.mi_hold_ms.clamp(150, 2000);
        self.mi_double_click_ms = self.mi_double_click_ms.clamp(0, 800);
        self.awake_min_battery_pct = self.awake_min_battery_pct.min(80);
        self.ac_refresh_hz = self.ac_refresh_hz.clamp(24, 240);
        self.battery_refresh_hz = self.battery_refresh_hz.clamp(24, 240);
        self.last_refresh_hz = self.last_refresh_hz.map(|hz| hz.clamp(24, 240));
        self.ac_brightness = self.ac_brightness.map(|b| b.min(100));
        self.battery_brightness = self.battery_brightness.map(|b| b.min(100));
    }

    /// Remember a user-chosen performance mode for both global restore and the active power profile.
    pub fn remember_perf_mode(&mut self, mode: PerfMode, on_ac: bool) {
        self.last_perf_mode = Some(mode);
        if on_ac {
            self.ac_perf_mode = mode;
        } else if !mode.requires_ac() {
            self.battery_perf_mode = mode;
        }
    }

    /// Remember a user-chosen refresh rate for both global restore and the active power profile.
    pub fn remember_refresh_hz(&mut self, hz: u32, on_ac: bool) {
        let clamped = hz.clamp(24, 240);
        self.last_refresh_hz = Some(clamped);
        if on_ac {
            self.ac_refresh_hz = clamped;
        } else {
            self.battery_refresh_hz = clamped;
        }
    }

    pub fn rejected_for(&self, on_ac: bool) -> &[PerfMode] {
        if on_ac {
            &self.rejected_modes_ac
        } else {
            &self.rejected_modes_battery
        }
    }

    pub fn record_rejected(&mut self, mode: PerfMode, on_ac: bool) -> bool {
        let list = if on_ac {
            &mut self.rejected_modes_ac
        } else {
            &mut self.rejected_modes_battery
        };
        if !list.contains(&mode) {
            list.push(mode);
            true
        } else {
            false
        }
    }

    /// Desired firmware charge limit taking `charge_care` and `travel_mode` into account.
    pub const fn desired_firmware_limit(&self) -> ChargeLimit {
        if self.charge_care && !self.travel_mode {
            self.charge_limit
        } else {
            ChargeLimit::P100
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_clamps_invalid_hand_edited_values() {
        let mut cfg = AppConfig {
            charge_limit: ChargeLimit::P100,
            touchpad_dead_zone_mm: 200,
            touchpad_edge_width_mm: 150,
            osd_duration_ms: 50,
            ..Default::default()
        };
        cfg.sanitize();
        assert_eq!(cfg.charge_limit, ChargeLimit::P80);
        assert_eq!(cfg.touchpad_dead_zone_mm, 40);
        assert_eq!(cfg.touchpad_edge_width_mm, 30);
        assert_eq!(cfg.osd_duration_ms, 500);
    }

    #[test]
    fn atomic_save_and_reload_roundtrip() {
        let dir = std::env::temp_dir().join(format!("micontrol_cfg_test_{}", std::process::id()));
        let path = dir.join("config.json");
        let mut cfg = AppConfig {
            charge_limit: ChargeLimit::P60,
            ..Default::default()
        };
        cfg.record_rejected(PerfMode::Balance, true);
        cfg.save_to(&path).expect("save config");

        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.charge_limit, ChargeLimit::P60);
        assert_eq!(loaded.rejected_modes_ac, vec![PerfMode::Balance]);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn desired_firmware_limit_respects_travel_mode() {
        let mut cfg = AppConfig {
            charge_care: true,
            charge_limit: ChargeLimit::P70,
            travel_mode: false,
            ..Default::default()
        };
        assert_eq!(cfg.desired_firmware_limit(), ChargeLimit::P70);
        cfg.travel_mode = true;
        assert_eq!(cfg.desired_firmware_limit(), ChargeLimit::P100);
        cfg.travel_mode = false;
        cfg.charge_care = false;
        assert_eq!(cfg.desired_firmware_limit(), ChargeLimit::P100);
    }

    #[test]
    fn remembers_mode_and_hz_per_power_source_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("micontrol_mem_test_{}", std::process::id()));
        let path = dir.join("config.json");
        let mut cfg = AppConfig::default();

        cfg.remember_perf_mode(PerfMode::Turbo, true);
        cfg.remember_perf_mode(PerfMode::Eco, false);
        cfg.remember_refresh_hz(100, true);
        cfg.remember_refresh_hz(48, false);
        cfg.save_to(&path).expect("save config");

        let loaded = AppConfig::load_from(&path);
        assert_eq!(loaded.ac_perf_mode, PerfMode::Turbo);
        assert_eq!(loaded.battery_perf_mode, PerfMode::Eco);
        assert_eq!(loaded.last_perf_mode, Some(PerfMode::Eco));
        assert_eq!(loaded.ac_refresh_hz, 100);
        assert_eq!(loaded.battery_refresh_hz, 48);
        assert_eq!(loaded.last_refresh_hz, Some(48));

        let _ = fs::remove_dir_all(&dir);
    }
}
