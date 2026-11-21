//! Hardware integration layer (WMI MIFS, ChargeGuard, Core Audio, Keep-Awake, Touchpad, Autostart).

pub mod audio;
pub mod autostart;
pub mod awake;
pub mod charge_guard;
pub mod display;
pub mod smart_profile;
pub mod touchpad;
pub mod wmi_mifs;

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, SendInput, VIRTUAL_KEY,
    VK_LWIN, VK_P, VK_S, VK_SHIFT, VK_TAB,
};

/// Returns `true` when plugged into AC power, `false` on battery (defaults to `true` if unknown).
pub fn is_on_ac_power() -> bool {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: Standard Win32 GetSystemPowerStatus call with initialized struct.
    if unsafe { GetSystemPowerStatus(&mut s) }.is_ok() {
        s.ACLineStatus != 0
    } else {
        true
    }
}

/// Returns current battery charge percentage (`0..=100`), or `None` if unavailable.
pub fn battery_percent() -> Option<u8> {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: Standard Win32 GetSystemPowerStatus call with initialized struct.
    if unsafe { GetSystemPowerStatus(&mut s) }.is_ok() && s.BatteryLifePercent <= 100 {
        Some(s.BatteryLifePercent)
    } else {
        None
    }
}

/// Send a synthetic hotkey chord (used for Projection `Win+P`, Screenshot `Win+Shift+S`, Task View `Win+Tab`).
pub fn send_chord(keys: &[VIRTUAL_KEY]) {
    let mut inputs = Vec::with_capacity(keys.len() * 2);
    for &vk in keys {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    ..Default::default()
                },
            },
        });
    }
    for &vk in keys.iter().rev() {
        inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk,
                    dwFlags: KEYEVENTF_KEYUP,
                    ..Default::default()
                },
            },
        });
    }
    // SAFETY: Valid contiguous slice of INPUT structs.
    unsafe {
        let _ = SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

pub fn trigger_projection() {
    if std::process::Command::new("DisplaySwitch.exe").spawn().is_err() {
        send_chord(&[VK_LWIN, VK_P]);
    }
}

pub fn trigger_screenshot() {
    send_chord(&[VK_LWIN, VK_SHIFT, VK_S]);
}

pub fn trigger_task_view() {
    send_chord(&[VK_LWIN, VK_TAB]);
}
