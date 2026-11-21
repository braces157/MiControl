//! Pure deterministic layout, hit-testing, and keyboard focus navigation for the Fluent Flyout.
//!
//! Zero `unsafe` code, zero OS handles — 100% unit-testable.

use crate::hardware::smart_profile::AwakeDuration;
use crate::protocol::{ChargeLimit, PerfMode};
use crate::ui::theme::{self, RectF, Rgb};

/// Snapshot of live hardware + config state rendered by the flyout.
#[derive(Debug, Clone)]
pub struct FlyoutSnapshot {
    pub wmi_available: bool,
    pub on_ac: bool,
    pub battery_pct: Option<u8>,
    pub adapter_watts: Option<u8>,
    pub battery_soh: Option<u8>,
    pub current_hz: Option<u32>,
    pub current_mode: Option<PerfMode>,
    pub rejected_modes: Vec<PerfMode>,
    pub charge_care: bool,
    pub care_limit: ChargeLimit,
    pub travel_mode: bool,
    pub charge_supported: bool,
    pub awake: bool,
    pub awake_preset: Option<AwakeDuration>,
    pub awake_status: String,
    pub smart_profiles: bool,
    pub ac_mode: PerfMode,
    pub battery_mode: PerfMode,
    pub auto_hz: bool,
    pub ac_hz: u32,
    pub battery_hz: u32,
    pub touchpad_available: bool,
    pub touchpad_enabled: Option<bool>,
    pub touchpad_dead_zone: bool,
    pub touchpad_dead_zone_mm: u32,
    pub touchpad_edge_sliders: bool,
    pub autostart: bool,
}

/// Interactive actions emitted when the user clicks or activates a flyout control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlyoutAction {
    SetMode(PerfMode),
    ToggleChargeCare,
    ToggleTravelMode,
    SetChargePreset(ChargeLimit),
    ToggleAwake,
    SetAwakeDuration(AwakeDuration),
    ToggleSmartProfiles,
    CycleRefreshRate,
    ToggleTouchpad,
    ToggleDeadZone,
    ToggleEdgeSliders,
    ToggleAutostart,
    ExitApp,
}

#[derive(Debug, Clone)]
pub struct ControlTile {
    pub rect: RectF,
    pub action: FlyoutAction,
    pub title: String,
    pub subtitle: Option<String>,
    pub active: bool,
    pub disabled: bool,
    pub accent: Rgb,
    pub centered: bool,
}

/// Compute deterministic flyout height (DIP) and control tile rectangles.
pub fn build_layout(s: &FlyoutSnapshot) -> (f32, Vec<ControlTile>) {
    let w = theme::FLYOUT_WIDTH_DIP;
    let pad = 14.0;
    let inner_w = w - pad * 2.0;
    let mut y = 50.0;
    let mut tiles = Vec::with_capacity(24);

    if !s.wmi_available {
        y += 38.0;
    }

    // Section 1: Performance Modes (5 tiles across)
    y += 20.0;
    let modes = [
        PerfMode::Eco,
        PerfMode::Quiet,
        PerfMode::Auto,
        PerfMode::Turbo,
        PerfMode::FullSpeed,
    ];
    let gap = 6.0;
    let mode_w = (inner_w - gap * 4.0) / 5.0;
    let mode_h = 42.0;
    for (i, m) in modes.into_iter().enumerate() {
        let x = pad + (i as f32) * (mode_w + gap);
        let rejected = s.rejected_modes.contains(&m);
        let disabled = !s.wmi_available || rejected || (m.requires_ac() && !s.on_ac);
        let label = match m {
            PerfMode::FullSpeed => "Full",
            other => other.label(),
        };
        tiles.push(ControlTile {
            rect: RectF {
                left: x,
                top: y,
                right: x + mode_w,
                bottom: y + mode_h,
            },
            action: FlyoutAction::SetMode(m),
            title: label.to_string(),
            subtitle: None,
            active: s.current_mode == Some(m),
            disabled,
            accent: theme::mode_color(m),
            centered: true,
        });
    }
    y += mode_h + 12.0;

    // Section 2: Battery Charge Care + Travel + 40..80 presets
    y += 20.0;
    let half_w = (inner_w - gap) / 2.0;
    let row_h = 44.0;
    let care_sub = if !s.charge_supported {
        "Unsupported".to_string()
    } else if s.travel_mode {
        "Paused (Travel)".to_string()
    } else if s.charge_care {
        format!("Capped at {}", s.care_limit)
    } else {
        "Off (100%)".to_string()
    };
    tiles.push(ControlTile {
        rect: RectF {
            left: pad,
            top: y,
            right: pad + half_w,
            bottom: y + row_h,
        },
        action: FlyoutAction::ToggleChargeCare,
        title: "Battery Care".into(),
        subtitle: Some(care_sub),
        active: s.charge_care && !s.travel_mode && s.charge_supported,
        disabled: !s.wmi_available || !s.charge_supported,
        accent: theme::STATUS_ECO,
        centered: false,
    });
    tiles.push(ControlTile {
        rect: RectF {
            left: pad + half_w + gap,
            top: y,
            right: pad + inner_w,
            bottom: y + row_h,
        },
        action: FlyoutAction::ToggleTravelMode,
        title: "Travel 100%".into(),
        subtitle: Some(
            if s.travel_mode {
                "Active until unplug"
            } else {
                "One-off full charge"
            }
            .into(),
        ),
        active: s.travel_mode && s.charge_supported,
        disabled: !s.wmi_available || !s.charge_supported,
        accent: theme::STATUS_TURBO,
        centered: false,
    });
    y += row_h + gap;

    let pill_h = 28.0;
    for (i, preset) in ChargeLimit::CARE_PRESETS.into_iter().enumerate() {
        let x = pad + (i as f32) * (mode_w + gap);
        tiles.push(ControlTile {
            rect: RectF {
                left: x,
                top: y,
                right: x + mode_w,
                bottom: y + pill_h,
            },
            action: FlyoutAction::SetChargePreset(preset),
            title: format!("{}%", preset.percent()),
            subtitle: None,
            active: s.care_limit == preset && s.charge_care && !s.travel_mode,
            disabled: !s.wmi_available || !s.charge_supported,
            accent: theme::STATUS_ECO,
            centered: true,
        });
    }
    y += pill_h + 12.0;

    // Section 3: Smart Profiles + Display Hz + Timed Keep-Awake + Touchpad
    y += 20.0;
    let ctrl_h = 44.0;

    // Row 1: Smart AC/Battery Profile (wide) | Display Hz quick toggle
    let hz_w = 82.0;
    let prof_w = inner_w - hz_w - gap;
    let prof_sub = if s.smart_profiles {
        if s.auto_hz {
            format!("AC {} {}Hz · Bat {} {}Hz", s.ac_mode, s.ac_hz, s.battery_mode, s.battery_hz)
        } else {
            format!("AC {} · Bat {} · Bright", s.ac_mode, s.battery_mode)
        }
    } else {
        "Manual mode, Hz & brightness".into()
    };
    tiles.push(ControlTile {
        rect: RectF {
            left: pad,
            top: y,
            right: pad + prof_w,
            bottom: y + ctrl_h,
        },
        action: FlyoutAction::ToggleSmartProfiles,
        title: "Smart AC/Battery".into(),
        subtitle: Some(prof_sub),
        active: s.smart_profiles,
        disabled: false,
        accent: theme::ACCENT,
        centered: false,
    });
    tiles.push(ControlTile {
        rect: RectF {
            left: pad + prof_w + gap,
            top: y,
            right: pad + inner_w,
            bottom: y + ctrl_h,
        },
        action: FlyoutAction::CycleRefreshRate,
        title: s
            .current_hz
            .map(|hz| format!("{hz} Hz"))
            .unwrap_or_else(|| "Hz".into()),
        subtitle: None,
        active: s.current_hz.is_some_and(|hz| hz >= 90),
        disabled: s.current_hz.is_none(),
        accent: theme::STATUS_AUTO,
        centered: true,
    });
    y += ctrl_h + gap;

    // Row 2: Keep Awake (Amphetamine) | Touchpad Toggle
    tiles.push(ControlTile {
        rect: RectF {
            left: pad,
            top: y,
            right: pad + half_w,
            bottom: y + ctrl_h,
        },
        action: FlyoutAction::ToggleAwake,
        title: "Keep Awake".into(),
        subtitle: Some(if s.awake {
            s.awake_status.clone()
        } else {
            "Off · tap timer below".into()
        }),
        active: s.awake,
        disabled: false,
        accent: theme::ACCENT,
        centered: false,
    });
    let tp_on = s.touchpad_enabled.unwrap_or(false);
    let tp_sub = if !s.touchpad_available {
        "Not detected"
    } else if tp_on {
        "Enabled"
    } else {
        "Disabled"
    };
    tiles.push(ControlTile {
        rect: RectF {
            left: pad + half_w + gap,
            top: y,
            right: pad + inner_w,
            bottom: y + ctrl_h,
        },
        action: FlyoutAction::ToggleTouchpad,
        title: "Touchpad".into(),
        subtitle: Some(tp_sub.into()),
        active: s.touchpad_available && tp_on,
        disabled: !s.touchpad_available,
        accent: theme::ACCENT,
        centered: false,
    });
    y += ctrl_h + gap;

    // Row 2b: Timed Keep-Awake (Amphetamine) duration pills: 30m | 1h | 2h | ∞
    let dur_w = (inner_w - gap * 3.0) / 4.0;
    for (i, dur) in AwakeDuration::PRESETS.into_iter().enumerate() {
        let x = pad + (i as f32) * (dur_w + gap);
        tiles.push(ControlTile {
            rect: RectF {
                left: x,
                top: y,
                right: x + dur_w,
                bottom: y + pill_h,
            },
            action: FlyoutAction::SetAwakeDuration(dur),
            title: format!("Awake {}", dur.label()),
            subtitle: None,
            active: s.awake && s.awake_preset == Some(dur),
            disabled: false,
            accent: theme::ACCENT,
            centered: true,
        });
    }
    y += pill_h + gap;

    // Row 3: Bottom Dead-Zone | Edge Sliders
    tiles.push(ControlTile {
        rect: RectF {
            left: pad,
            top: y,
            right: pad + half_w,
            bottom: y + ctrl_h,
        },
        action: FlyoutAction::ToggleDeadZone,
        title: "Bottom Dead-Zone".into(),
        subtitle: Some(if s.touchpad_dead_zone {
            format!("{} mm palm guard", s.touchpad_dead_zone_mm)
        } else {
            "Off".into()
        }),
        active: s.touchpad_dead_zone && s.touchpad_available,
        disabled: !s.touchpad_available,
        accent: theme::ACCENT,
        centered: false,
    });
    tiles.push(ControlTile {
        rect: RectF {
            left: pad + half_w + gap,
            top: y,
            right: pad + inner_w,
            bottom: y + ctrl_h,
        },
        action: FlyoutAction::ToggleEdgeSliders,
        title: "Edge Sliders".into(),
        subtitle: Some(
            if s.touchpad_edge_sliders {
                "L: Bright · R: Vol"
            } else {
                "Off (0% CPU)"
            }
            .into(),
        ),
        active: s.touchpad_edge_sliders && s.touchpad_available,
        disabled: !s.touchpad_available,
        accent: theme::ACCENT,
        centered: false,
    });
    y += ctrl_h + 12.0;

    // Footer: Start at Login | Exit
    let foot_h = 30.0;
    let exit_w = 72.0;
    let auto_w = inner_w - exit_w - gap;
    tiles.push(ControlTile {
        rect: RectF {
            left: pad,
            top: y,
            right: pad + auto_w,
            bottom: y + foot_h,
        },
        action: FlyoutAction::ToggleAutostart,
        title: if s.autostart {
            "Start at Login: On".into()
        } else {
            "Start at Login: Off".into()
        },
        subtitle: None,
        active: s.autostart,
        disabled: false,
        accent: theme::ACCENT_SOFT,
        centered: true,
    });
    tiles.push(ControlTile {
        rect: RectF {
            left: pad + auto_w + gap,
            top: y,
            right: pad + inner_w,
            bottom: y + foot_h,
        },
        action: FlyoutAction::ExitApp,
        title: "Exit".into(),
        subtitle: None,
        active: false,
        disabled: false,
        accent: theme::STATUS_FULL,
        centered: true,
    });
    y += foot_h + 14.0;

    (y, tiles)
}

pub fn hit_test(tiles: &[ControlTile], x_dip: f32, y_dip: f32) -> Option<usize> {
    tiles.iter().position(|t| {
        !t.disabled
            && x_dip >= t.rect.left
            && x_dip <= t.rect.right
            && y_dip >= t.rect.top
            && y_dip <= t.rect.bottom
    })
}

pub fn move_focus(tiles: &[ControlTile], cur: Option<usize>, delta: isize) -> Option<usize> {
    let enabled: Vec<usize> = tiles
        .iter()
        .enumerate()
        .filter_map(|(i, t)| (!t.disabled).then_some(i))
        .collect();
    if enabled.is_empty() {
        return None;
    }
    let pos = cur.and_then(|c| enabled.iter().position(|&i| i == c));
    let next_pos = match pos {
        None => {
            if delta < 0 {
                enabled.len() - 1
            } else {
                0
            }
        }
        Some(p) => (p as isize + delta).rem_euclid(enabled.len() as isize) as usize,
    };
    Some(enabled[next_pos])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot() -> FlyoutSnapshot {
        FlyoutSnapshot {
            wmi_available: true,
            on_ac: true,
            battery_pct: Some(80),
            adapter_watts: Some(100),
            battery_soh: Some(100),
            current_hz: Some(120),
            current_mode: Some(PerfMode::Auto),
            rejected_modes: vec![PerfMode::Balance],
            charge_care: true,
            care_limit: ChargeLimit::P80,
            travel_mode: false,
            charge_supported: true,
            awake: true,
            awake_preset: Some(AwakeDuration::Hour1),
            awake_status: "58m left · ≤20% safe".into(),
            smart_profiles: true,
            ac_mode: PerfMode::Auto,
            battery_mode: PerfMode::Quiet,
            auto_hz: true,
            ac_hz: 120,
            battery_hz: 60,
            touchpad_available: true,
            touchpad_enabled: Some(true),
            touchpad_dead_zone: true,
            touchpad_dead_zone_mm: 12,
            touchpad_edge_sliders: true,
            autostart: true,
        }
    }

    #[test]
    fn flyout_layout_has_no_overlapping_tiles_and_fits_bounds() {
        let (h, tiles) = build_layout(&sample_snapshot());
        assert!(h > 420.0 && h < 580.0, "unexpected height {h}");
        assert_eq!(tiles.len(), 24);

        for (i, a) in tiles.iter().enumerate() {
            assert!(a.rect.left >= 0.0 && a.rect.right <= theme::FLYOUT_WIDTH_DIP);
            assert!(a.rect.top >= 0.0 && a.rect.bottom <= h);
            for b in tiles.iter().skip(i + 1) {
                let overlap_x = a.rect.left < b.rect.right && b.rect.left < a.rect.right;
                let overlap_y = a.rect.top < b.rect.bottom && b.rect.top < a.rect.bottom;
                assert!(
                    !(overlap_x && overlap_y),
                    "overlapping tiles: {:?} vs {:?}",
                    a.action,
                    b.action
                );
            }
        }
    }

    #[test]
    fn keyboard_focus_skips_disabled_tiles() {
        let mut s = sample_snapshot();
        s.on_ac = false;
        let (_, tiles) = build_layout(&s);
        let full_idx = tiles
            .iter()
            .position(|t| t.action == FlyoutAction::SetMode(PerfMode::FullSpeed))
            .unwrap();
        assert!(tiles[full_idx].disabled);

        let mut focus = None;
        for _ in 0..tiles.len() {
            focus = move_focus(&tiles, focus, 1);
            assert_ne!(focus, Some(full_idx));
        }
    }
}
