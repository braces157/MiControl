//! Fluent Dark Windows 11 design tokens grounded in `DESIGN.md` (zero Direct2D/GPU deps).

use crate::protocol::PerfMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        Self {
            r: ((v >> 16) & 0xFF) as u8,
            g: ((v >> 8) & 0xFF) as u8,
            b: (v & 0xFF) as u8,
        }
    }

    /// Windows GDI `COLORREF` (`0x00BBGGRR`).
    pub const fn colorref(self) -> u32 {
        (self.r as u32) | ((self.g as u32) << 8) | ((self.b as u32) << 16)
    }

    /// Top-down 32bpp BGRA pixel (`0xFFRRGGBB`).
    pub const fn bgra(self) -> u32 {
        0xFF00_0000 | ((self.r as u32) << 16) | ((self.g as u32) << 8) | (self.b as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RectF {
    pub left: f32,
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
}

pub const CANVAS: Rgb = Rgb::hex(0x1C1C1E);
pub const SURFACE: Rgb = Rgb::hex(0x27272A);
pub const SURFACE_HOVER: Rgb = Rgb::hex(0x323236);
pub const SURFACE_ACTIVE: Rgb = Rgb::hex(0x3A3A40);
pub const SURFACE_DISABLED: Rgb = Rgb::hex(0x202023);
pub const BORDER: Rgb = Rgb::hex(0x3F3F46);
pub const BORDER_FOCUS: Rgb = Rgb::hex(0x60CDFF);

pub const INK_PRIMARY: Rgb = Rgb::hex(0xF4F4F5);
pub const INK_SECONDARY: Rgb = Rgb::hex(0xA1A1AA);
pub const INK_MUTED: Rgb = Rgb::hex(0x71717A);
pub const INK_ON_ACCENT: Rgb = Rgb::hex(0x09090B);

pub const ACCENT: Rgb = Rgb::hex(0x38BDF8);
pub const ACCENT_SOFT: Rgb = Rgb::hex(0x1E3A4A);

pub const STATUS_ECO: Rgb = Rgb::hex(0x34D399);
pub const STATUS_QUIET: Rgb = Rgb::hex(0x60A5FA);
pub const STATUS_AUTO: Rgb = Rgb::hex(0x38BDF8);
pub const STATUS_TURBO: Rgb = Rgb::hex(0xFB923C);
pub const STATUS_FULL: Rgb = Rgb::hex(0xF87171);
pub const STATUS_WARN_BG: Rgb = Rgb::hex(0x3B2512);

pub const fn mode_color(mode: PerfMode) -> Rgb {
    match mode {
        PerfMode::Eco => STATUS_ECO,
        PerfMode::Quiet | PerfMode::Balance => STATUS_QUIET,
        PerfMode::Auto => STATUS_AUTO,
        PerfMode::Turbo => STATUS_TURBO,
        PerfMode::FullSpeed => STATUS_FULL,
    }
}

pub const fn mode_rgb_u32(mode: PerfMode) -> u32 {
    match mode {
        PerfMode::Eco => 0x34D399,
        PerfMode::Quiet | PerfMode::Balance => 0x60A5FA,
        PerfMode::Auto => 0x38BDF8,
        PerfMode::Turbo => 0xFB923C,
        PerfMode::FullSpeed => 0xF87171,
    }
}

pub const FLYOUT_WIDTH_DIP: f32 = 348.0;
pub const OSD_WIDTH_DIP: f32 = 248.0;
pub const OSD_HEIGHT_DIP: f32 = 58.0;
