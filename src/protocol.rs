//! Pure Xiaomi/Redmi MIFS WMI protocol codec, HID_EVENT20 decoder, and Mi-button gesture state machine.
//!
//! Zero OS dependencies — 100% deterministic and unit-testable.

use serde::{Deserialize, Serialize};
use std::fmt;

pub const MIFS_BUFFER_SIZE: usize = 32;
pub const OP_GET: u8 = 0xFA;
pub const OP_SET: u8 = 0xFB;
pub const STATUS_OK: u8 = 0x80;
pub const STATUS_UNSUPPORTED: u8 = 0xE0;

pub const CMD_PERF: u8 = 0x08;
pub const CMD_MIC: u8 = 0x0A;
pub const CMD_CHARGE: u8 = 0x10;

pub const SUB_BATTERY_HEALTH: u8 = 0x01;
pub const SUB_CHARGE_LIMIT: u8 = 0x02;
pub const SUB_CHARGE_ZONE: u8 = 0x03;
pub const SUB_ADAPTER_WATTS: u8 = 0x06;

/// Performance modes supported by Xiaomi/Redmi Bitland MIFS firmware (`cmd 0x08`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PerfMode {
    Eco,
    Quiet,
    Balance,
    Auto,
    Turbo,
    FullSpeed,
}

impl PerfMode {
    /// Ordered list for cycling and UI presentation (from quietest/lowest power to maximum).
    pub const ALL: [PerfMode; 6] = [
        PerfMode::Eco,
        PerfMode::Quiet,
        PerfMode::Balance,
        PerfMode::Auto,
        PerfMode::Turbo,
        PerfMode::FullSpeed,
    ];

    pub const fn as_byte(self) -> u8 {
        match self {
            Self::Balance => 0x01,
            Self::Quiet => 0x02,
            Self::Turbo => 0x03,
            Self::FullSpeed => 0x04,
            Self::Auto => 0x09,
            Self::Eco => 0x0A,
        }
    }

    pub const fn from_byte(b: u8) -> Option<Self> {
        match b {
            0x01 => Some(Self::Balance),
            0x02 => Some(Self::Quiet),
            0x03 => Some(Self::Turbo),
            0x04 => Some(Self::FullSpeed),
            0x09 => Some(Self::Auto),
            0x0A => Some(Self::Eco),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Eco => "Eco",
            Self::Quiet => "Quiet",
            Self::Balance => "Balance",
            Self::Auto => "Auto",
            Self::Turbo => "Turbo",
            Self::FullSpeed => "Full Speed",
        }
    }

    pub const fn short_code(self) -> &'static str {
        match self {
            Self::Eco => "EC",
            Self::Quiet => "QT",
            Self::Balance => "BL",
            Self::Auto => "AU",
            Self::Turbo => "TB",
            Self::FullSpeed => "FS",
        }
    }

    pub const fn requires_ac(self) -> bool {
        matches!(self, Self::FullSpeed)
    }

    pub fn parse_cli(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "eco" => Some(Self::Eco),
            "quiet" | "silent" | "silence" => Some(Self::Quiet),
            "balance" | "balanced" => Some(Self::Balance),
            "auto" | "smart" => Some(Self::Auto),
            "turbo" | "perf" | "performance" => Some(Self::Turbo),
            "full" | "fullspeed" | "full-speed" | "max" => Some(Self::FullSpeed),
            _ => None,
        }
    }
}

impl fmt::Display for PerfMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Firmware-validated battery charge thresholds (`cmd 0x10`, `sub 0x02`).
/// Note: 90% does not exist in Xiaomi MIFS firmware (code 3 is rejected by EC).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ChargeLimit {
    P40,
    P50,
    P60,
    P70,
    P80,
    P100,
}

impl ChargeLimit {
    pub const CARE_PRESETS: [ChargeLimit; 5] = [
        ChargeLimit::P40,
        ChargeLimit::P50,
        ChargeLimit::P60,
        ChargeLimit::P70,
        ChargeLimit::P80,
    ];

    pub const fn percent(self) -> u8 {
        match self {
            Self::P40 => 40,
            Self::P50 => 50,
            Self::P60 => 60,
            Self::P70 => 70,
            Self::P80 => 80,
            Self::P100 => 100,
        }
    }

    pub const fn is_care_active(self) -> bool {
        !matches!(self, Self::P100)
    }

    /// Encode to MIFS `arg2` code. Uses legacy `1` for 80% (same as official OEM software).
    pub const fn mifs_code(self) -> u8 {
        match self {
            Self::P100 => 0,
            Self::P80 => 1,
            Self::P70 => 5,
            Self::P60 => 6,
            Self::P50 => 7,
            Self::P40 => 8,
        }
    }

    /// Decode from MIFS `OUT[6]` code. Accepts both legacy `1` and granular `4` as 80%.
    pub const fn from_mifs_code(code: u8) -> Option<Self> {
        match code {
            0 => Some(Self::P100),
            1 | 4 => Some(Self::P80),
            5 => Some(Self::P70),
            6 => Some(Self::P60),
            7 => Some(Self::P50),
            8 => Some(Self::P40),
            _ => None,
        }
    }

    pub const fn from_percent(pct: u8) -> Option<Self> {
        match pct {
            40 => Some(Self::P40),
            50 => Some(Self::P50),
            60 => Some(Self::P60),
            70 => Some(Self::P70),
            80 => Some(Self::P80),
            100 => Some(Self::P100),
            _ => None,
        }
    }
}

impl fmt::Display for ChargeLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}%", self.percent())
    }
}

/// Detected MIFS response layout dialect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MifsDialect {
    /// Modern TM24* / Intel laptops: `OUT[1]` = status (`0x80` OK / `0xE0` unsupported),
    /// `OUT[3]` = cmd echo, `OUT[4]` = perf value, `OUT[6]` = `0x10` group value.
    Classic,
    /// Older Ryzen models (e.g. TM2113): `OUT[1]` = cmd echo, `OUT[2]` = perf value.
    /// Group `0x10` returns echo garbage and is NOT supported.
    Echo,
    Unsupported,
}

impl MifsDialect {
    pub const fn supports_charge_group(self) -> bool {
        matches!(self, Self::Classic)
    }
}

pub const fn build_get(cmd: u8, arg: u8, arg2: u8) -> [u8; MIFS_BUFFER_SIZE] {
    let mut buf = [0u8; MIFS_BUFFER_SIZE];
    buf[1] = OP_GET;
    buf[3] = cmd;
    buf[4] = arg;
    buf[6] = arg2;
    buf
}

pub const fn build_set(cmd: u8, arg: u8, arg2: u8) -> [u8; MIFS_BUFFER_SIZE] {
    let mut buf = [0u8; MIFS_BUFFER_SIZE];
    buf[1] = OP_SET;
    buf[3] = cmd;
    buf[4] = arg;
    buf[6] = arg2;
    buf
}

pub fn detect_dialect(probe_cmd: u8, out: &[u8]) -> MifsDialect {
    if out.len() < 7 {
        return MifsDialect::Unsupported;
    }
    match out[1] {
        STATUS_OK | STATUS_UNSUPPORTED => MifsDialect::Classic,
        b if b == probe_cmd => MifsDialect::Echo,
        _ => MifsDialect::Unsupported,
    }
}

pub fn parse_perf_reply(dialect: MifsDialect, out: &[u8]) -> Option<PerfMode> {
    match dialect {
        MifsDialect::Classic => {
            if out.len() >= 5 && out[1] == STATUS_OK {
                PerfMode::from_byte(out[4])
            } else {
                None
            }
        }
        MifsDialect::Echo => {
            if out.len() >= 3 && out[1] == CMD_PERF {
                PerfMode::from_byte(out[2])
            } else {
                None
            }
        }
        MifsDialect::Unsupported => None,
    }
}

pub fn parse_charge_reply(dialect: MifsDialect, out: &[u8]) -> Option<ChargeLimit> {
    if !dialect.supports_charge_group() {
        return None;
    }
    if out.len() >= 7 && out[1] == STATUS_OK {
        ChargeLimit::from_mifs_code(out[6])
    } else {
        None
    }
}

/// Parse single-byte sensor value from `cmd 0x10` (e.g., PD adapter watts `sub 0x06`, SOH1 `sub 0x01`).
pub fn parse_charge_group_byte(dialect: MifsDialect, out: &[u8]) -> Option<u8> {
    if !dialect.supports_charge_group() {
        return None;
    }
    if out.len() >= 7 && out[1] == STATUS_OK {
        Some(out[6])
    } else {
        None
    }
}

pub fn is_set_accepted(dialect: MifsDialect, cmd: u8, out: &[u8]) -> bool {
    match dialect {
        MifsDialect::Classic => out.len() >= 2 && out[1] == STATUS_OK,
        MifsDialect::Echo => out.len() >= 2 && out[1] == cmd,
        MifsDialect::Unsupported => false,
    }
}

/// Keyboard backlight level reported by `HID_EVENT20` (`0x05`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KbdBacklight {
    Off,
    Medium,
    High,
    Auto,
    Raw(u8),
}

impl KbdBacklight {
    pub const fn from_byte(v: u8) -> Self {
        match v {
            0x00 => Self::Off,
            0x05 => Self::Medium,
            0x0A => Self::High,
            0x80 => Self::Auto,
            other => Self::Raw(other),
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Off => "Off",
            Self::Medium => "50%",
            Self::High => "100%",
            Self::Auto => "Auto",
            Self::Raw(_) => "Changed",
        }
    }
}

/// Decoded firmware event from `root\wmi\HID_EVENT20` (`EventDetail`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HidEvent {
    MiDown,
    MiUp,
    PerfKey { raw_mode: u8 },
    MicMute { muted: bool },
    KbdBacklight(KbdBacklight),
    FnLock { standard_fn: bool },
    CapsLock { on: bool },
    NumLock { on: bool },
    WinLock { locked: bool },
    TouchpadState { enabled: bool },
    TouchpadToggle,
    RefreshRateKey,
    SettingsKey,
    ProjectionKey,
    ScreenshotKey,
    TaskViewKey,
    AiDown,
    AiUp,
    CameraPrivacy { blocked: bool },
    LowPowerWarning,
    Unknown { code: u8, value: u8 },
}

/// Decode `EventDetail` byte slice (`[1]` = code, `[2]` = value).
pub fn decode_hid_event(detail: &[u8]) -> Option<HidEvent> {
    if detail.len() < 3 {
        return None;
    }
    let code = detail[1];
    let val = detail[2];
    Some(match code {
        0x25 | 0x18 => HidEvent::MiDown,
        0x26 | 0x19 => HidEvent::MiUp,
        0x16 => HidEvent::PerfKey { raw_mode: val },
        // Hardware mic LED inverted: 0 = muted (LED on), 1 = unmuted (LED off)
        0x21 => HidEvent::MicMute { muted: val == 0 },
        0x05 => HidEvent::KbdBacklight(KbdBacklight::from_byte(val)),
        0x07 => HidEvent::FnLock { standard_fn: val != 0 },
        0x09 => HidEvent::CapsLock { on: val != 0 },
        0x0D => HidEvent::NumLock { on: val != 0 },
        0x17 => HidEvent::WinLock { locked: val != 0 },
        0x06 => HidEvent::TouchpadState { enabled: val != 0 },
        0x10 => HidEvent::TouchpadToggle,
        0x13 | 0x1A => HidEvent::RefreshRateKey,
        0x1B => HidEvent::SettingsKey,
        0x01 if val == 2 => HidEvent::LowPowerWarning,
        0x01 => HidEvent::ProjectionKey,
        0x02 => HidEvent::ScreenshotKey,
        0x03 => HidEvent::TaskViewKey,
        0x0C => HidEvent::LowPowerWarning,
        0x23 => HidEvent::AiDown,
        0x24 => HidEvent::AiUp,
        0xA0 => HidEvent::CameraPrivacy { blocked: val != 0 },
        _ => HidEvent::Unknown { code, value: val },
    })
}

/// Pure Mi-button gesture state machine (single click, double click, long hold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiGesture {
    Click,
    DoubleClick,
    Hold,
}

#[derive(Debug, Clone)]
pub struct MiGestureState {
    hold_ms: u64,
    double_ms: u64,
    pressed_at_ms: Option<u64>,
    hold_fired: bool,
    pending_click_at_ms: Option<u64>,
}

impl MiGestureState {
    pub const fn new(hold_ms: u64, double_ms: u64) -> Self {
        Self {
            hold_ms,
            double_ms,
            pressed_at_ms: None,
            hold_fired: false,
            pending_click_at_ms: None,
        }
    }

    /// Record Mi button press at monotonic timestamp `now_ms`.
    /// Returns optional timeout (ms) after which `on_hold_timeout` should be called.
    pub fn on_down(&mut self, now_ms: u64) -> Option<u64> {
        self.pressed_at_ms = Some(now_ms);
        self.hold_fired = false;
        if self.hold_ms > 0 {
            Some(self.hold_ms)
        } else {
            None
        }
    }

    /// Called when the hold timer fires. Emits `Hold` if still pressed.
    pub fn on_hold_timeout(&mut self, now_ms: u64) -> Option<MiGesture> {
        let down = self.pressed_at_ms?;
        if !self.hold_fired && now_ms.saturating_sub(down) >= self.hold_ms {
            self.hold_fired = true;
            self.pending_click_at_ms = None;
            return Some(MiGesture::Hold);
        }
        None
    }

    /// Record Mi button release. May immediately emit `DoubleClick` or `Click`,
    /// or return `None` while waiting `double_ms` for a potential second tap.
    pub fn on_up(&mut self, now_ms: u64) -> (Option<MiGesture>, Option<u64>) {
        let was_pressed = self.pressed_at_ms.take().is_some();
        if !was_pressed || self.hold_fired {
            self.hold_fired = false;
            return (None, None);
        }

        if let Some(first_ms) = self.pending_click_at_ms
            && now_ms.saturating_sub(first_ms) <= self.double_ms
        {
            self.pending_click_at_ms = None;
            return (Some(MiGesture::DoubleClick), None);
        }

        if self.double_ms == 0 {
            return (Some(MiGesture::Click), None);
        }

        self.pending_click_at_ms = Some(now_ms);
        (None, Some(self.double_ms))
    }

    /// Called when the double-click window expires. Emits `Click` if pending.
    pub fn on_double_timeout(&mut self, now_ms: u64) -> Option<MiGesture> {
        let first = self.pending_click_at_ms?;
        if self.pressed_at_ms.is_none() && now_ms.saturating_sub(first) >= self.double_ms {
            self.pending_click_at_ms = None;
            return Some(MiGesture::Click);
        }
        None
    }
}

/// Pick the next allowed performance mode when cycling.
pub fn next_perf_mode(
    current: Option<PerfMode>,
    on_ac: bool,
    rejected: &[PerfMode],
) -> Option<PerfMode> {
    let available: Vec<PerfMode> = PerfMode::ALL
        .into_iter()
        .filter(|m| !rejected.contains(m))
        .filter(|m| on_ac || !m.requires_ac())
        .collect();
    if available.is_empty() {
        return None;
    }
    let idx = current
        .and_then(|c| available.iter().position(|&m| m == c))
        .map(|i| (i + 1) % available.len())
        .unwrap_or(0);
    Some(available[idx])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_exact_mifs_32_byte_buffers() {
        let get_perf = build_get(CMD_PERF, 0, 0);
        assert_eq!(get_perf.len(), 32);
        assert_eq!(&get_perf[..8], &[0x00, 0xFA, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00]);
        assert!(get_perf[8..].iter().all(|&b| b == 0));

        let set_charge = build_set(CMD_CHARGE, SUB_CHARGE_LIMIT, ChargeLimit::P80.mifs_code());
        assert_eq!(
            &set_charge[..8],
            &[0x00, 0xFB, 0x00, 0x10, 0x02, 0x00, 0x01, 0x00]
        );
    }

    #[test]
    fn detects_classic_and_echo_dialects() {
        // Real reply captured on TM2411 (Redmi Book Pro 14 2025): GET 0x08 -> Auto (0x09)
        let classic = [0x00, 0x80, 0x00, 0x08, 0x09, 0x00, 0x00, 0x00];
        assert_eq!(detect_dialect(CMD_PERF, &classic), MifsDialect::Classic);
        assert_eq!(parse_perf_reply(MifsDialect::Classic, &classic), Some(PerfMode::Auto));

        // Unsupported status in Classic dialect is STILL Classic (not Unsupported dialect!)
        let classic_e0 = [0x00, 0xE0, 0x00, 0x08, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(detect_dialect(CMD_PERF, &classic_e0), MifsDialect::Classic);
        assert_eq!(parse_perf_reply(MifsDialect::Classic, &classic_e0), None);

        // Echo dialect (TM2113): OUT[1] = cmd, OUT[2] = perf mode, charge group rejected
        let echo = [0x00, 0x08, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(detect_dialect(CMD_PERF, &echo), MifsDialect::Echo);
        assert_eq!(parse_perf_reply(MifsDialect::Echo, &echo), Some(PerfMode::Quiet));
        assert_eq!(parse_charge_reply(MifsDialect::Echo, &[0x00, 0x10, 0x02, 0, 0, 0, 0x01]), None);
    }

    #[test]
    fn charge_limit_roundtrip_and_legacy_code4() {
        for pct in [40, 50, 60, 70, 80, 100] {
            let lim = ChargeLimit::from_percent(pct).expect("valid preset");
            assert_eq!(ChargeLimit::from_mifs_code(lim.mifs_code()), Some(lim));
        }
        assert_eq!(ChargeLimit::from_percent(90), None);
        assert_eq!(ChargeLimit::from_mifs_code(4), Some(ChargeLimit::P80));
        assert_eq!(ChargeLimit::P80.mifs_code(), 1);
    }

    #[test]
    fn decodes_hid_events_including_inverted_mic() {
        assert_eq!(
            decode_hid_event(&[0x01, 0x21, 0x00]),
            Some(HidEvent::MicMute { muted: true })
        );
        assert_eq!(
            decode_hid_event(&[0x01, 0x21, 0x01]),
            Some(HidEvent::MicMute { muted: false })
        );
        assert_eq!(
            decode_hid_event(&[0x01, 0x05, 0x80]),
            Some(HidEvent::KbdBacklight(KbdBacklight::Auto))
        );
        assert_eq!(decode_hid_event(&[0x01, 0x25, 0x01]), Some(HidEvent::MiDown));
        assert_eq!(decode_hid_event(&[0x01, 0x18, 0x01]), Some(HidEvent::MiDown));
        assert_eq!(decode_hid_event(&[0x01, 0x01, 0x02]), Some(HidEvent::LowPowerWarning));
        assert_eq!(decode_hid_event(&[0x01]), None);
    }

    #[test]
    fn mi_gesture_click_double_and_hold() {
        let mut g = MiGestureState::new(400, 300);

        assert_eq!(g.on_down(1000), Some(400));
        assert_eq!(g.on_up(1080), (None, Some(300)));
        assert_eq!(g.on_double_timeout(1390), Some(MiGesture::Click));

        g.on_down(2000);
        assert_eq!(g.on_up(2050), (None, Some(300)));
        g.on_down(2150);
        assert_eq!(g.on_up(2200), (Some(MiGesture::DoubleClick), None));
        assert_eq!(g.on_double_timeout(2600), None);

        g.on_down(3000);
        assert_eq!(g.on_hold_timeout(3400), Some(MiGesture::Hold));
        assert_eq!(g.on_up(3600), (None, None));
    }

    #[test]
    fn next_perf_mode_skips_rejected_and_fullspeed_on_battery() {
        let rejected = [PerfMode::Balance];
        assert_eq!(
            next_perf_mode(Some(PerfMode::Turbo), true, &rejected),
            Some(PerfMode::FullSpeed)
        );
        assert_eq!(
            next_perf_mode(Some(PerfMode::Turbo), false, &rejected),
            Some(PerfMode::Eco)
        );
        assert_eq!(
            next_perf_mode(Some(PerfMode::Quiet), true, &rejected),
            Some(PerfMode::Auto)
        );
    }
}
