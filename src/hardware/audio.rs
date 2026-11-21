//! Windows Core Audio integration:
//! - Default capture microphone mute sync (for Xiaomi hardware mic-mute hotkey `0x21`)
//! - Default render output volume step adjustment (for Precision Touchpad right-edge slider)

use crate::hardware::wmi_mifs::ComGuard;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications, eConsole,
    eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};

fn with_endpoint_volume<F, R>(flow: EDataFlow, role: windows::Win32::Media::Audio::ERole, f: F) -> Option<R>
where
    F: FnOnce(&IAudioEndpointVolume) -> Option<R>,
{
    let _com = ComGuard::init_mta();
    // SAFETY: Standard Core Audio COM activation for default audio endpoint.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(flow, role).ok()?;
        let ep: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        f(&ep)
    }
}

/// Sync Windows default capture microphone mute state (both Console and Communications roles).
pub fn set_mic_mute(muted: bool) -> bool {
    let mut ok = false;
    for role in [eConsole, eCommunications] {
        if with_endpoint_volume(eCapture, role, |ep| {
            // SAFETY: Valid COM call on live IAudioEndpointVolume.
            unsafe { ep.SetMute(muted, std::ptr::null()).ok() }
        })
        .is_some()
        {
            ok = true;
        }
    }
    ok
}

/// Read current master speaker volume in `0..=100`.
pub fn get_speaker_volume_percent() -> Option<u8> {
    with_endpoint_volume(eRender, eConsole, |ep| {
        // SAFETY: Valid COM call on live IAudioEndpointVolume.
        let scalar = unsafe { ep.GetMasterVolumeLevelScalar().ok()? };
        Some((scalar.clamp(0.0, 1.0) * 100.0).round() as u8)
    })
}

/// Adjust master speaker volume by `delta_percent` (e.g. `+2` or `-2`) and return the new percent.
/// Unmutes automatically when raising volume above zero.
pub fn step_speaker_volume(delta_percent: i32) -> Option<u8> {
    with_endpoint_volume(eRender, eConsole, |ep| {
        // SAFETY: Valid COM calls on live IAudioEndpointVolume.
        unsafe {
            let cur = (ep.GetMasterVolumeLevelScalar().ok()?.clamp(0.0, 1.0) * 100.0).round() as i32;
            let next = (cur + delta_percent).clamp(0, 100) as u8;
            ep.SetMasterVolumeLevelScalar(next as f32 / 100.0, std::ptr::null())
                .ok()?;
            if delta_percent > 0 && next > 0 {
                let _ = ep.SetMute(false, std::ptr::null());
            }
            Some(next)
        }
    })
}
