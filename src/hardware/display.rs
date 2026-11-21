//! Built-in laptop panel refresh rate control via Windows CCD (`QueryDisplayConfig`)
//! and `ChangeDisplaySettingsExW`.
//!
//! Strictly targets the internal display (`DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL`),
//! never modifying external monitors. Zero background polling.

use std::collections::BTreeSet;
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS,
    DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo,
    GetDisplayConfigBufferSizes, QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::Graphics::Gdi::{
    CDS_UPDATEREGISTRY, ChangeDisplaySettingsExW, DEVMODEW, DISP_CHANGE_SUCCESSFUL,
    DM_DISPLAYFREQUENCY, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW,
};
use windows_core::PCWSTR;

/// Find the GDI device name (e.g. `\\.\DISPLAY1`) of the active built-in laptop panel.
pub fn find_internal_display_name() -> Option<String> {
    let mut path_count = 0u32;
    let mut mode_count = 0u32;
    // SAFETY: Standard CCD buffer-size query for active display paths.
    let rc = unsafe {
        GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
    };
    if rc != ERROR_SUCCESS || path_count == 0 {
        return None;
    }

    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
    // SAFETY: Buffers sized from GetDisplayConfigBufferSizes.
    let rc = unsafe {
        QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut path_count,
            paths.as_mut_ptr(),
            &mut mode_count,
            modes.as_mut_ptr(),
            None,
        )
    };
    if rc != ERROR_SUCCESS {
        return None;
    }

    for path in &paths[..path_count as usize] {
        let tech = path.targetInfo.outputTechnology;
        let is_internal = tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
            || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
            || tech == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS;
        if !is_internal {
            continue;
        }

        let mut src_name = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                size: std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                adapterId: path.sourceInfo.adapterId,
                id: path.sourceInfo.id,
            },
            ..Default::default()
        };
        // SAFETY: Initialized DISPLAYCONFIG_SOURCE_DEVICE_NAME header passed to DisplayConfigGetDeviceInfo.
        let rc = unsafe {
            DisplayConfigGetDeviceInfo(
                &mut src_name.header as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER,
            )
        };
        if rc == 0 {
            let end = src_name
                .viewGdiDeviceName
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(src_name.viewGdiDeviceName.len());
            let name = String::from_utf16_lossy(&src_name.viewGdiDeviceName[..end]);
            if !name.is_empty() {
                return Some(name);
            }
        }
    }
    None
}

fn current_mode_for(dev_name: &str) -> Option<DEVMODEW> {
    let wide: Vec<u16> = dev_name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut dm = DEVMODEW {
        dmSize: std::mem::size_of::<DEVMODEW>() as u16,
        ..Default::default()
    };
    // SAFETY: Standard EnumDisplaySettingsW for ENUM_CURRENT_SETTINGS.
    let ok = unsafe { EnumDisplaySettingsW(PCWSTR(wide.as_ptr()), ENUM_CURRENT_SETTINGS, &mut dm) };
    ok.as_bool().then_some(dm)
}

/// Return current refresh rate (Hz) of the built-in display, or `None` if unavailable.
pub fn current_internal_hz() -> Option<u32> {
    let dev = find_internal_display_name()?;
    let dm = current_mode_for(&dev)?;
    (dm.dmDisplayFrequency > 0).then_some(dm.dmDisplayFrequency)
}

/// List sorted unique refresh rates supported by the built-in display at its current resolution.
pub fn list_internal_refresh_rates() -> Vec<u32> {
    let Some(dev) = find_internal_display_name() else {
        return Vec::new();
    };
    let Some(cur) = current_mode_for(&dev) else {
        return Vec::new();
    };
    let wide: Vec<u16> = dev.encode_utf16().chain(std::iter::once(0)).collect();
    let mut rates = BTreeSet::new();
    let mut idx = 0u32;

    loop {
        let mut dm = DEVMODEW {
            dmSize: std::mem::size_of::<DEVMODEW>() as u16,
            ..Default::default()
        };
        // SAFETY: Standard EnumDisplaySettingsW enumeration loop.
        let ok = unsafe {
            EnumDisplaySettingsW(
                PCWSTR(wide.as_ptr()),
                windows::Win32::Graphics::Gdi::ENUM_DISPLAY_SETTINGS_MODE(idx),
                &mut dm,
            )
        };
        if !ok.as_bool() {
            break;
        }
        if dm.dmPelsWidth == cur.dmPelsWidth
            && dm.dmPelsHeight == cur.dmPelsHeight
            && dm.dmBitsPerPel == cur.dmBitsPerPel
            && dm.dmDisplayFrequency >= 24
        {
            rates.insert(dm.dmDisplayFrequency);
        }
        idx += 1;
        if idx > 2048 {
            break;
        }
    }

    rates.into_iter().collect()
}

/// Choose the closest supported refresh rate to `preferred_hz` (pure selection helper).
pub fn pick_closest_hz(supported: &[u32], preferred_hz: u32) -> Option<u32> {
    supported
        .iter()
        .copied()
        .min_by_key(|&hz| hz.abs_diff(preferred_hz))
}

/// Apply refresh rate (or closest supported rate) strictly to the built-in panel.
/// Returns the actual applied refresh rate in Hz.
pub fn set_internal_hz(preferred_hz: u32) -> Result<u32, String> {
    let dev = find_internal_display_name()
        .ok_or_else(|| "Built-in laptop display not detected".to_string())?;
    let supported = list_internal_refresh_rates();
    let target_hz = pick_closest_hz(&supported, preferred_hz)
        .ok_or_else(|| "No display refresh rates enumerated".to_string())?;

    let mut dm = current_mode_for(&dev)
        .ok_or_else(|| "Failed to query current display mode".to_string())?;
    if dm.dmDisplayFrequency == target_hz {
        return Ok(target_hz);
    }

    dm.dmDisplayFrequency = target_hz;
    dm.dmFields = DM_DISPLAYFREQUENCY;
    let wide: Vec<u16> = dev.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: Applies validated refresh rate strictly to the named internal GDI display device.
    let rc = unsafe {
        ChangeDisplaySettingsExW(
            PCWSTR(wide.as_ptr()),
            Some(&dm),
            None,
            CDS_UPDATEREGISTRY,
            None,
        )
    };
    if rc == DISP_CHANGE_SUCCESSFUL {
        Ok(target_hz)
    } else {
        Err(format!("ChangeDisplaySettingsExW returned {}", rc.0))
    }
}

/// Cycle the built-in panel between its supported high/low presets (or next rate in list).
pub fn cycle_internal_hz() -> Result<u32, String> {
    let rates = list_internal_refresh_rates();
    if rates.len() < 2 {
        return Err("Display only exposes a single refresh rate".into());
    }
    let cur = current_internal_hz().unwrap_or(rates[0]);
    // Prefer crisp 60 <-> max toggle if 60 and >=90 exist; otherwise cycle all >=48Hz rates.
    let high = *rates.last().unwrap_or(&cur);
    let target = if rates.contains(&60) && high >= 90 {
        if cur >= 90 { 60 } else { high }
    } else {
        let usable: Vec<u32> = rates.into_iter().filter(|&r| r >= 48).collect();
        let pos = usable.iter().position(|&r| r == cur).map(|i| (i + 1) % usable.len()).unwrap_or(0);
        usable[pos]
    };
    set_internal_hz(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_closest_supported_refresh_rate() {
        let rates = [24, 30, 48, 60, 75, 100, 120];
        assert_eq!(pick_closest_hz(&rates, 120), Some(120));
        assert_eq!(pick_closest_hz(&rates, 60), Some(60));
        assert_eq!(pick_closest_hz(&rates, 144), Some(120));
        assert_eq!(pick_closest_hz(&rates, 55), Some(60));
        assert_eq!(pick_closest_hz(&[], 60), None);
    }

    #[test]
    fn discovers_local_internal_laptop_display_and_rates() {
        // Read-only probe on this Redmi Book Pro 14 2025 (should find internal panel & >=1 rate).
        if let Some(name) = find_internal_display_name() {
            assert!(name.starts_with(r"\\.\DISPLAY"), "unexpected GDI name: {name}");
            let rates = list_internal_refresh_rates();
            assert!(!rates.is_empty(), "expected at least one refresh rate");
            assert!(current_internal_hz().is_some());
        }
    }
}
