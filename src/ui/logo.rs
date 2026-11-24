//! Pure-Rust 4x4 supersampled SDF renderer for the MiControl squircle brand mark.
//!
//! Shared by both the live System Tray `HICON` (dynamically tinted per performance mode +
//! optional Charge Care dot) and the Fluent Flyout header emblem. Zero external image crates.

use crate::protocol::PerfMode;

/// Primary & secondary luminous accent colors `(primary, highlight)` for a performance mode.
pub const fn mode_accent_pair(mode: Option<PerfMode>) -> (u32, u32) {
    match mode {
        Some(PerfMode::Eco) => (0x34D399, 0x6EE7B7),
        Some(PerfMode::Quiet | PerfMode::Balance) => (0x60A5FA, 0x93C5FD),
        Some(PerfMode::Auto) => (0x38BDF8, 0x7DD3FC),
        Some(PerfMode::Turbo) => (0xFB923C, 0xFDBA74),
        Some(PerfMode::FullSpeed) => (0xF87171, 0xFCA5A5),
        None => (0xFF6900, 0xFFA94D),
    }
}

#[inline]
fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

#[inline]
fn mix(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

#[inline]
fn mix3(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [mix(a[0], b[0], t), mix(a[1], b[1], t), mix(a[2], b[2], t)]
}

#[inline]
fn hex3(h: u32) -> [f32; 3] {
    [
        ((h >> 16) & 0xFF) as f32 / 255.0,
        ((h >> 8) & 0xFF) as f32 / 255.0,
        (h & 0xFF) as f32 / 255.0,
    ]
}

#[inline]
fn sd_squircle(x: f32, y: f32, r: f32) -> f32 {
    const N: f32 = 4.4;
    let ax = x.abs() / r;
    let ay = y.abs() / r;
    if ax < 1e-6 && ay < 1e-6 {
        return -r;
    }
    let axn = ax.powf(N);
    let ayn = ay.powf(N);
    let gx = if ax > 1e-6 { N * ax.powf(N - 1.0) / r } else { 0.0 };
    let gy = if ay > 1e-6 { N * ay.powf(N - 1.0) / r } else { 0.0 };
    let grad = gx.hypot(gy);
    if grad < 1e-6 {
        ((axn + ayn).powf(1.0 / N) - 1.0) * r
    } else {
        (axn + ayn - 1.0) / grad
    }
}

#[inline]
fn sd_capsule(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32, r: f32) -> f32 {
    let pax = px - ax;
    let pay = py - ay;
    let bax = bx - ax;
    let bay = by - ay;
    let h = ((pax * bax + pay * bay) / (bax * bax + bay * bay + 1e-9)).clamp(0.0, 1.0);
    (pax - bax * h).hypot(pay - bay * h) - r
}

#[inline]
fn sd_mi_glyph(x: f32, y: f32) -> (f32, f32) {
    const SW: f32 = 0.078;
    let d_l = sd_capsule(x, y, -0.49, 0.30, -0.49, -0.15, SW);
    let d_top = sd_capsule(x, y, -0.49, -0.15, 0.01, -0.15, SW);
    let d_arc = if x >= 0.01 && y <= 0.01 {
        ((x - 0.01).hypot(y - 0.01) - 0.16).abs() - SW
    } else {
        1e9
    };
    let d_r = sd_capsule(x, y, 0.17, 0.01, 0.17, 0.30, SW);
    let d_mid = sd_capsule(x, y, -0.16, 0.04, -0.16, 0.30, SW);
    let d_i = sd_capsule(x, y, 0.45, 0.04, 0.45, 0.30, SW);
    let d_white = d_l.min(d_top).min(d_arc).min(d_r).min(d_mid).min(d_i);
    let d_dot = (x - 0.45).hypot(y + 0.145) - 0.098;
    (d_white, d_dot)
}

/// Render a premultiplied ARGB (`0xAARRGGBB`, Windows 32bpp DIB layout) MiControl squircle logo.
pub fn render_premul_argb(
    size: usize,
    accent_hex: u32,
    accent2_hex: u32,
    care_dot: bool,
    tray_mode: bool,
) -> Vec<u32> {
    let size = size.max(1);
    let mut out = vec![0u32; size * size];
    let c_acc1 = hex3(accent_hex);
    let c_acc2 = hex3(accent2_hex);
    let bg_top = hex3(0x23262F);
    let bg_bot = hex3(0x111318);
    let rim_top = hex3(0x52586A);
    let white_top = [1.0, 1.0, 1.0];
    let white_bot = hex3(0xE2E8F0);
    let care_ring = hex3(0x111318);
    let care_green = hex3(0x34D399);

    let ss: usize = if size <= 48 { 4 } else { 2 };
    let inv_ss2 = 1.0 / (ss * ss) as f32;
    let inv_size = 1.0 / size as f32;
    let aa = (2.0 * inv_size) * 0.75;
    let r_outer = if tray_mode { 0.95 } else { 0.92 };
    let glow_boost = if tray_mode { 1.35 } else { 1.0 };
    let bar_r = if tray_mode { 0.056 } else { 0.046 };
    let rim_w = if tray_mode { 0.062 } else { 0.044 };
    let rim_alpha = if tray_mode { 0.80 } else { 0.68 };

    for py in 0..size {
        for px in 0..size {
            let mut sr = 0.0f32;
            let mut sg = 0.0f32;
            let mut sb = 0.0f32;
            let mut sa = 0.0f32;

            for sy in 0..ss {
                let v = ((py as f32 + (sy as f32 + 0.5) / ss as f32) * inv_size) * 2.0 - 1.0;
                for sx in 0..ss {
                    let u = ((px as f32 + (sx as f32 + 0.5) / ss as f32) * inv_size) * 2.0 - 1.0;

                    let d_sq = sd_squircle(u, v, r_outer);
                    let a_sq = 1.0 - smoothstep(-aa, aa, d_sq);

                    let (mut col, mut pa) = if a_sq > 0.0 {
                        let t_grad = ((v + 1.0) * 0.5).clamp(0.0, 1.0);
                        let base = mix3(bg_top, bg_bot, t_grad);
                        let glow_orb = (-((u - 0.45).powi(2) + (v + 0.16).powi(2)) * 3.2).exp()
                            * 0.28
                            * glow_boost;
                        let glow_bot = (-(u * u * 0.75 + (v - 0.78).powi(2) * 5.0)).exp()
                            * 0.32
                            * glow_boost;
                        let mut c = mix3(base, c_acc1, (glow_orb + glow_bot).clamp(0.0, 0.58));

                        let d_bar = sd_capsule(u, v, -0.35, 0.60, 0.35, 0.60, bar_r);
                        let a_bar = 1.0 - smoothstep(-aa, aa, d_bar);
                        let bar_t = ((u + 0.35) / 0.70).clamp(0.0, 1.0);
                        c = mix3(c, mix3(c_acc1, c_acc2, bar_t), a_bar);

                        let a_rim = smoothstep(-rim_w - aa, -rim_w + aa, d_sq) * a_sq;
                        let rim_c = mix3(rim_top, c_acc1, (0.35 + 0.50 * t_grad).clamp(0.0, 1.0));
                        c = mix3(c, rim_c, a_rim * rim_alpha);

                        let (d_white, d_dot) = sd_mi_glyph(u * 1.05, (v + 0.07) * 1.05);
                        let (d_sh, _) = sd_mi_glyph(u * 1.05, (v + 0.03) * 1.05);
                        let a_sh = (1.0 - smoothstep(-aa * 2.5, aa * 2.5, d_sh)) * 0.35;
                        c = mix3(c, [0.0, 0.0, 0.0], a_sh);

                        let a_white = 1.0 - smoothstep(-aa, aa, d_white);
                        let glyph_c = mix3(white_top, white_bot, ((v + 0.3) * 0.8).clamp(0.0, 1.0));
                        c = mix3(c, glyph_c, a_white);

                        let a_dot = 1.0 - smoothstep(-aa, aa, d_dot);
                        let dot_c = mix3(c_acc2, c_acc1, ((v + 0.25) * 3.0).clamp(0.0, 1.0));
                        c = mix3(c, dot_c, a_dot);

                        (c, a_sq)
                    } else {
                        ([0.0, 0.0, 0.0], 0.0)
                    };

                    if care_dot {
                        let d_cd = (u - 0.68).hypot(v - 0.68);
                        let a_ring = 1.0 - smoothstep(0.24 - aa, 0.24 + aa, d_cd);
                        if a_ring > 0.0 {
                            let a_in = 1.0 - smoothstep(0.165 - aa, 0.165 + aa, d_cd);
                            let badge = mix3(care_ring, care_green, a_in);
                            let out_a = a_ring + pa * (1.0 - a_ring);
                            if out_a > 1e-6 {
                                for k in 0..3 {
                                    col[k] = (badge[k] * a_ring + col[k] * pa * (1.0 - a_ring)) / out_a;
                                }
                                pa = out_a;
                            }
                        }
                    }

                    sr += col[0] * pa;
                    sg += col[1] * pa;
                    sb += col[2] * pa;
                    sa += pa;
                }
            }

            let a8 = ((sa * inv_ss2 * 255.0).round() as u32).min(255);
            let r8 = ((sr * inv_ss2 * 255.0).round() as u32).min(255);
            let g8 = ((sg * inv_ss2 * 255.0).round() as u32).min(255);
            let b8 = ((sb * inv_ss2 * 255.0).round() as u32).min(255);
            out[py * size + px] = (a8 << 24) | (r8 << 16) | (g8 << 8) | b8;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_nonempty_squircle_with_transparent_corners() {
        let px = render_premul_argb(32, 0xFF6900, 0xFFA94D, false, true);
        assert_eq!(px.len(), 32 * 32);
        assert_eq!(px[0] >> 24, 0);
        assert_eq!(px[16 * 32 + 16] >> 24, 255);
    }

    #[test]
    fn care_dot_renders_visible_emerald_badge() {
        let without = render_premul_argb(32, 0x38BDF8, 0x7DD3FC, false, true);
        let with = render_premul_argb(32, 0x38BDF8, 0x7DD3FC, true, true);
        assert_ne!(without, with);
        // Center of care dot ~(0.68, 0.68) -> pixel (27, 27) has high green channel
        let p = with[27 * 32 + 27];
        let g = (p >> 8) & 0xFF;
        assert!(g > 150, "expected emerald care dot green channel > 150, got {g}");
    }
}
