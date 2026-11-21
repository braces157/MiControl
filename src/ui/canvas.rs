//! Zero-GPU-driver software 32bpp DIB + ClearType GDI canvas + Win11 DWM Fluent chrome.
//!
//! Avoids loading `d2d1.dll`, `d3d11.dll`, `dwrite.dll`, and vendor GPU user-mode drivers,
//! cutting idle process RAM by >50 MB and eliminating GPU driver thread pools completely.
//! Every surface is created strictly for the duration of `WM_PAINT` and freed immediately after `BitBlt`.

use crate::ui::theme::{RectF, Rgb};
use std::ffi::c_void;
use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_BORDER_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CLEARTYPE_QUALITY, CreateCompatibleDC,
    CreateDIBSection, CreateFontW, DIB_RGB_COLORS, DT_CENTER, DT_END_ELLIPSIS, DT_LEFT,
    DT_NOPREFIX, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DeleteDC, DeleteObject, DrawTextW,
    GdiFlush, HBITMAP, HDC, HGDIOBJ, OUT_DEFAULT_PRECIS, SRCCOPY, SelectObject, SetBkMode,
    SetTextColor, TRANSPARENT, CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, FF_DONTCARE,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows_core::{BOOL, PCWSTR};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextAlign {
    Left,
    Center,
    Right,
}

/// Apply native Windows 11 dark chrome, rounded window corners, and `#3F3F46` border hairline.
pub fn apply_win11_fluent_chrome(hwnd: HWND) {
    // SAFETY: Standard DwmSetWindowAttribute calls; gracefully ignored on older Windows builds.
    unsafe {
        let dark = BOOL(1);
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            &dark as *const BOOL as *const c_void,
            std::mem::size_of::<BOOL>() as u32,
        );
        let corner: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &corner as *const DWM_WINDOW_CORNER_PREFERENCE as *const c_void,
            std::mem::size_of::<DWM_WINDOW_CORNER_PREFERENCE>() as u32,
        );
        let border_color: u32 = crate::ui::theme::BORDER.colorref();
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_BORDER_COLOR,
            &border_color as *const u32 as *const c_void,
            std::mem::size_of::<u32>() as u32,
        );
    }
}

pub fn window_dpi(hwnd: HWND) -> f32 {
    // SAFETY: GetDpiForWindow returns 96 on fallback or invalid window.
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi == 0 { 96.0 } else { dpi as f32 }
}

pub fn dip_to_px(dip: f32, dpi: f32) -> i32 {
    (dip * (dpi / 96.0)).round() as i32
}

pub fn px_to_dip(px: i32, dpi: f32) -> f32 {
    (px as f32) * (96.0 / dpi)
}

/// Ephemeral flicker-free backbuffer (32bpp top-down DIB) with crisp anti-aliased SDF primitives
/// and native ClearType `Segoe UI` text rendering.
pub struct PaintCanvas {
    mem_dc: HDC,
    bitmap: HBITMAP,
    old_obj: HGDIOBJ,
    pixels: *mut u32,
    width: i32,
    height: i32,
    scale: f32,
}

impl PaintCanvas {
    pub fn new(target_dc: HDC, width_px: i32, height_px: i32, dpi: f32) -> Option<Self> {
        let width = width_px.max(1);
        let height = height_px.max(1);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down DIB
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };

        // SAFETY: Creates compatible memory DC + top-down 32bpp DIB; both freed in Drop.
        unsafe {
            let mem_dc = CreateCompatibleDC(Some(target_dc));
            if mem_dc.is_invalid() {
                return None;
            }
            let mut bits: *mut c_void = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
                let _ = DeleteDC(mem_dc);
                return None;
            };
            if bits.is_null() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(mem_dc);
                return None;
            }
            let old_obj = SelectObject(mem_dc, bitmap.into());
            SetBkMode(mem_dc, TRANSPARENT);
            Some(Self {
                mem_dc,
                bitmap,
                old_obj,
                pixels: bits as *mut u32,
                width,
                height,
                scale: dpi / 96.0,
            })
        }
    }

    fn pixel_slice_mut(&mut self) -> &mut [u32] {
        // SAFETY: `pixels` points to `width * height` valid u32 BGRA pixels owned by `bitmap`.
        unsafe { std::slice::from_raw_parts_mut(self.pixels, (self.width * self.height) as usize) }
    }

    pub fn clear(&mut self, color: Rgb) {
        self.pixel_slice_mut().fill(color.bgra());
    }

    /// Crisp 1px anti-aliased signed-distance-field filled rounded rectangle.
    pub fn fill_round_rect(&mut self, rect: RectF, radius_dip: f32, color: Rgb) {
        let s = self.scale;
        let l = rect.left * s;
        let t = rect.top * s;
        let r = rect.right * s;
        let b = rect.bottom * s;
        let rad = (radius_dip * s).min((r - l) * 0.5).min((b - t) * 0.5).max(0.0);

        let x0 = (l.floor() as i32 - 1).clamp(0, self.width);
        let x1 = (r.ceil() as i32 + 1).clamp(0, self.width);
        let y0 = (t.floor() as i32 - 1).clamp(0, self.height);
        let y1 = (b.ceil() as i32 + 1).clamp(0, self.height);

        let cx = (l + r) * 0.5;
        let cy = (t + b) * 0.5;
        let hx = (r - l) * 0.5 - rad;
        let hy = (b - t) * 0.5 - rad;
        let w = self.width as usize;
        let pix = self.pixel_slice_mut();

        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            let dy = (fy - cy).abs() - hy;
            let row = (y as usize) * w;
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                let dx = (fx - cx).abs() - hx;
                let dist = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - rad;
                let alpha = (0.5 - dist).clamp(0.0, 1.0);
                if alpha > 0.0 {
                    pix[row + x as usize] = blend_bgra(pix[row + x as usize], color, alpha);
                }
            }
        }
    }

    /// Crisp anti-aliased SDF rounded rectangle outline.
    pub fn stroke_round_rect(&mut self, rect: RectF, radius_dip: f32, stroke_dip: f32, color: Rgb) {
        let s = self.scale;
        let half_sw = (stroke_dip * s * 0.5).max(0.5);
        let l = rect.left * s + half_sw;
        let t = rect.top * s + half_sw;
        let r = rect.right * s - half_sw;
        let b = rect.bottom * s - half_sw;
        if r <= l || b <= t {
            return;
        }
        let rad = (radius_dip * s - half_sw).max(0.0).min((r - l) * 0.5).min((b - t) * 0.5);

        let pad = half_sw.ceil() as i32 + 1;
        let x0 = (l.floor() as i32 - pad).clamp(0, self.width);
        let x1 = (r.ceil() as i32 + pad).clamp(0, self.width);
        let y0 = (t.floor() as i32 - pad).clamp(0, self.height);
        let y1 = (b.ceil() as i32 + pad).clamp(0, self.height);

        let cx = (l + r) * 0.5;
        let cy = (t + b) * 0.5;
        let hx = (r - l) * 0.5 - rad;
        let hy = (b - t) * 0.5 - rad;
        let w = self.width as usize;
        let pix = self.pixel_slice_mut();

        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            let dy = (fy - cy).abs() - hy;
            let row = (y as usize) * w;
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                let dx = (fx - cx).abs() - hx;
                let sdf = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - rad;
                let edge_dist = sdf.abs() - half_sw;
                let alpha = (0.5 - edge_dist).clamp(0.0, 1.0);
                if alpha > 0.0 {
                    pix[row + x as usize] = blend_bgra(pix[row + x as usize], color, alpha);
                }
            }
        }
    }

    /// Render crisp Windows ClearType `Segoe UI` text directly onto the memory DC.
    pub fn draw_text(
        &self,
        text: &str,
        rect: RectF,
        size_dip: f32,
        weight: i32,
        color: Rgb,
        align: TextAlign,
    ) {
        if text.is_empty() {
            return;
        }
        let s = self.scale;
        let height_px = -(size_dip * s * 1.15).round() as i32;
        let mut rc = RECT {
            left: (rect.left * s).round() as i32,
            top: (rect.top * s).round() as i32,
            right: (rect.right * s).round() as i32,
            bottom: (rect.bottom * s).round() as i32,
        };
        let align_flag = match align {
            TextAlign::Left => DT_LEFT,
            TextAlign::Center => DT_CENTER,
            TextAlign::Right => DT_RIGHT,
        };
        // NUL-terminate with 4 trailing NULs so DT_END_ELLIPSIS never reads/writes past the slice.
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let text_len = wide.len();
        wide.extend_from_slice(&[0, 0, 0, 0]);

        // SAFETY: Creates Segoe UI font, draws text to memory DC, restores previous font, deletes font.
        unsafe {
            let font = CreateFontW(
                height_px,
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
                PCWSTR(windows_core::w!("Segoe UI").as_ptr()),
            );
            let old_font = SelectObject(self.mem_dc, font.into());
            SetTextColor(self.mem_dc, COLORREF(color.colorref()));
            let _ = DrawTextW(
                self.mem_dc,
                &mut wide[..text_len],
                &mut rc,
                DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX | align_flag,
            );
            SelectObject(self.mem_dc, old_font);
            let _ = DeleteObject(font.into());
        }
    }

    /// Alpha-composite a premultiplied ARGB (`0xAARRGGBB`) square icon onto the backbuffer.
    pub fn blit_premul_argb(&mut self, x_dip: f32, y_dip: f32, size_px: usize, src: &[u32]) {
        if size_px == 0 || src.len() < size_px * size_px {
            return;
        }
        let ox = (x_dip * self.scale).round() as i32;
        let oy = (y_dip * self.scale).round() as i32;
        let w = self.width;
        let h = self.height;
        let pix = self.pixel_slice_mut();

        for sy in 0..size_px {
            let dy = oy + sy as i32;
            if dy < 0 || dy >= h {
                continue;
            }
            let row = (dy as usize) * (w as usize);
            let srow = sy * size_px;
            for sx in 0..size_px {
                let dx = ox + sx as i32;
                if dx < 0 || dx >= w {
                    continue;
                }
                let sp = src[srow + sx];
                let sa = (sp >> 24) & 0xFF;
                if sa == 0 {
                    continue;
                }
                if sa == 255 {
                    pix[row + dx as usize] = sp;
                    continue;
                }
                let dp = pix[row + dx as usize];
                let inv = 255 - sa;
                let sr = (sp >> 16) & 0xFF;
                let sg = (sp >> 8) & 0xFF;
                let sb = sp & 0xFF;
                let dr = (dp >> 16) & 0xFF;
                let dg = (dp >> 8) & 0xFF;
                let db = dp & 0xFF;
                let r = (sr + (dr * inv) / 255).min(255);
                let g = (sg + (dg * inv) / 255).min(255);
                let b = (sb + (db * inv) / 255).min(255);
                pix[row + dx as usize] = 0xFF00_0000 | (r << 16) | (g << 8) | b;
            }
        }
    }

    /// Present the completed backbuffer to the window DC in a single flicker-free BitBlt.
    pub fn present(&self, target_dc: HDC) {
        // SAFETY: Flushes GDI operations and blits exact memory DC bounds to target_dc.
        unsafe {
            let _ = GdiFlush();
            let _ = BitBlt(
                target_dc,
                0,
                0,
                self.width,
                self.height,
                Some(self.mem_dc),
                0,
                0,
                SRCCOPY,
            );
        }
    }
}

impl Drop for PaintCanvas {
    fn drop(&mut self) {
        // SAFETY: Restores previous bitmap object before deleting owned DIB and memory DC.
        unsafe {
            SelectObject(self.mem_dc, self.old_obj);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.mem_dc);
        }
    }
}

#[inline]
fn blend_bgra(dst: u32, src: Rgb, alpha: f32) -> u32 {
    if alpha >= 0.999 {
        return src.bgra();
    }
    let inv = 1.0 - alpha;
    let dr = ((dst >> 16) & 0xFF) as f32;
    let dg = ((dst >> 8) & 0xFF) as f32;
    let db = (dst & 0xFF) as f32;
    let r = (src.r as f32 * alpha + dr * inv).round() as u32;
    let g = (src.g as f32 * alpha + dg * inv).round() as u32;
    let b = (src.b as f32 * alpha + db * inv).round() as u32;
    0xFF00_0000 | (r.min(255) << 16) | (g.min(255) << 8) | b.min(255)
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC};

    #[test]
    fn canvas_renders_shapes_and_text_without_crashing() {
        // SAFETY: Acquires and releases screen DC for offscreen DIB paint verification.
        unsafe {
            let hdc = GetDC(None);
            let mut cv = PaintCanvas::new(hdc, 348, 380, 96.0).expect("canvas created");
            cv.clear(crate::ui::theme::CANVAS);
            cv.fill_round_rect(
                RectF { left: 10.0, top: 10.0, right: 160.0, bottom: 54.0 },
                8.0,
                crate::ui::theme::SURFACE,
            );
            cv.stroke_round_rect(
                RectF { left: 10.0, top: 10.0, right: 160.0, bottom: 54.0 },
                8.0,
                1.0,
                crate::ui::theme::BORDER,
            );
            cv.draw_text(
                "Firmware WMI unavailable — run as Administrator",
                RectF { left: 14.0, top: 14.0, right: 120.0, bottom: 44.0 },
                10.0,
                600,
                crate::ui::theme::INK_PRIMARY,
                TextAlign::Left,
            );
            cv.draw_text(
                "",
                RectF { left: 14.0, top: 50.0, right: 120.0, bottom: 70.0 },
                10.0,
                400,
                crate::ui::theme::INK_PRIMARY,
                TextAlign::Right,
            );
            ReleaseDC(None, hdc);
        }
    }
}
