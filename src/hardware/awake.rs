//! Keep-Awake (Owl Mode): prevents sleep & overrides AC Lid-Close action to "Do Nothing",
//! saving the original lid setting to `AppConfig` *before* mutating Windows power settings
//! so it can always be restored cleanly even after a crash or power cut.

use crate::config::AppConfig;
use windows::Win32::Foundation::ERROR_SUCCESS;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Power::{
    ES_CONTINUOUS, ES_DISPLAY_REQUIRED, ES_SYSTEM_REQUIRED, PowerGetActiveScheme,
    PowerReadACValueIndex, PowerSetActiveScheme, PowerWriteACValueIndex, SetThreadExecutionState,
};
use windows_core::GUID;

/// Windows Power Subgroup: Power Buttons and Lid (`4f971e89-eebd-4455-a8de-9e59040e7347`).
const GUID_SYSTEM_BUTTON_SUBGROUP: GUID = GUID::from_u128(0x4f971e89_eebd_4455_a8de_9e59040e7347);
/// Windows Power Setting: Lid Close Action (`5ca83367-6e45-459f-a27b-476b1d01c936`).
const GUID_LIDCLOSE_ACTION: GUID = GUID::from_u128(0x5ca83367_6e45_459f_a27b_476b1d01c936);

const LID_DO_NOTHING: u32 = 0;

/// Abstraction over Windows power scheme & execution state for deterministic unit tests.
pub trait PowerBackend {
    fn active_scheme(&self) -> Option<String>;
    fn read_lid_ac(&self, scheme: &str) -> Option<u32>;
    fn write_lid_ac(&self, scheme: &str, action: u32) -> bool;
    fn activate_scheme(&self, scheme: &str) -> bool;
    fn hold_awake(&self, keep_display: bool) -> bool;
    fn release_awake(&self);
    fn persist_config(&self, cfg: &AppConfig);
}

pub struct WinPowerBackend;

fn guid_to_string(g: &GUID) -> String {
    format!(
        "{:08x}-{:04x}-{:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        g.data1,
        g.data2,
        g.data3,
        g.data4[0],
        g.data4[1],
        g.data4[2],
        g.data4[3],
        g.data4[4],
        g.data4[5],
        g.data4[6],
        g.data4[7],
    )
}

fn parse_guid(s: &str) -> Option<GUID> {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok().map(GUID::from_u128)
}

impl PowerBackend for WinPowerBackend {
    fn active_scheme(&self) -> Option<String> {
        let mut ptr: *mut GUID = std::ptr::null_mut();
        // SAFETY: PowerGetActiveScheme allocates GUID via LocalAlloc/CoTaskMem; copied then freed.
        let rc = unsafe { PowerGetActiveScheme(None, &mut ptr) };
        if rc != ERROR_SUCCESS || ptr.is_null() {
            return None;
        }
        // SAFETY: Pointer is non-null and initialized by PowerGetActiveScheme.
        let guid = unsafe { *ptr };
        // SAFETY: Freeing OS-allocated pointer from PowerGetActiveScheme.
        unsafe { CoTaskMemFree(Some(ptr as *const std::ffi::c_void)) };
        Some(guid_to_string(&guid))
    }

    fn read_lid_ac(&self, scheme: &str) -> Option<u32> {
        let g = parse_guid(scheme)?;
        let mut val = 0u32;
        // SAFETY: Valid GUID pointers and u32 out-parameter.
        let rc = unsafe {
            PowerReadACValueIndex(
                None,
                Some(&g),
                Some(&GUID_SYSTEM_BUTTON_SUBGROUP),
                Some(&GUID_LIDCLOSE_ACTION),
                &mut val,
            )
        };
        (rc == ERROR_SUCCESS).then_some(val)
    }

    fn write_lid_ac(&self, scheme: &str, action: u32) -> bool {
        let Some(g) = parse_guid(scheme) else {
            return false;
        };
        // SAFETY: Valid GUID pointers for power scheme setting write.
        let rc = unsafe {
            PowerWriteACValueIndex(
                None,
                &g,
                Some(&GUID_SYSTEM_BUTTON_SUBGROUP),
                Some(&GUID_LIDCLOSE_ACTION),
                action,
            )
        };
        rc == ERROR_SUCCESS
    }

    fn activate_scheme(&self, scheme: &str) -> bool {
        let Some(g) = parse_guid(scheme) else {
            return false;
        };
        // SAFETY: Valid GUID pointer for PowerSetActiveScheme.
        unsafe { PowerSetActiveScheme(None, Some(&g)) == ERROR_SUCCESS }
    }

    fn hold_awake(&self, keep_display: bool) -> bool {
        let mut flags = ES_CONTINUOUS | ES_SYSTEM_REQUIRED;
        if keep_display {
            flags |= ES_DISPLAY_REQUIRED;
        }
        // SAFETY: Thread execution state call; returns 0 on failure.
        unsafe { SetThreadExecutionState(flags).0 != 0 }
    }

    fn release_awake(&self) {
        // SAFETY: Clears system/display awake requirement for this thread.
        unsafe {
            let _ = SetThreadExecutionState(ES_CONTINUOUS);
        }
    }

    fn persist_config(&self, cfg: &AppConfig) {
        let _ = cfg.save();
    }
}

/// Enable Owl / Keep-Awake mode using crash-safe save-before-write lid restoration.
pub fn enable_with(cfg: &mut AppConfig, backend: &dyn PowerBackend) -> bool {
    let Some(scheme) = backend.active_scheme() else {
        return false;
    };

    // If a prior scheme's lid setting is still pending restore, restore it first before switching.
    if let Some(prior) = cfg.saved_lid_scheme.clone()
        && prior != scheme
        && !restore_lid_with(cfg, backend)
    {
        return false;
    }

    if cfg.saved_lid_ac.is_none() {
        let Some(cur) = backend.read_lid_ac(&scheme) else {
            return false;
        };
        cfg.saved_lid_ac = Some(cur);
    }
    cfg.saved_lid_scheme = Some(scheme.clone());
    // Persist restoration marker BEFORE mutating Windows power plan.
    backend.persist_config(cfg);

    if !backend.write_lid_ac(&scheme, LID_DO_NOTHING) || !backend.activate_scheme(&scheme) {
        restore_lid_with(cfg, backend);
        return false;
    }

    if !backend.hold_awake(!cfg.awake_allow_display_off) {
        restore_lid_with(cfg, backend);
        return false;
    }

    cfg.awake_enabled = true;
    true
}

/// Disable Owl / Keep-Awake mode and restore original AC lid close action.
pub fn disable_with(cfg: &mut AppConfig, backend: &dyn PowerBackend) -> bool {
    backend.release_awake();
    cfg.awake_enabled = false;
    restore_lid_with(cfg, backend)
}

/// Release active Keep-Awake holds and restore Windows AC lid-close action during app exit,
/// while preserving `cfg.awake_enabled` in `config.json` so persistent Keep-Awake re-arms next launch.
pub fn suspend_for_exit_with(cfg: &mut AppConfig, backend: &dyn PowerBackend) -> bool {
    backend.release_awake();
    restore_lid_with(cfg, backend)
}

/// Restore any previously saved AC lid close action (also safe to call on startup after a crash).
pub fn restore_lid_with(cfg: &mut AppConfig, backend: &dyn PowerBackend) -> bool {
    let Some(saved) = cfg.saved_lid_ac else {
        return true;
    };
    let Some(active) = backend.active_scheme() else {
        return false;
    };
    let target = cfg.saved_lid_scheme.clone().unwrap_or_else(|| active.clone());
    if !backend.write_lid_ac(&target, saved) {
        return false;
    }
    if target == active && !backend.activate_scheme(&target) {
        return false;
    }
    cfg.saved_lid_ac = None;
    cfg.saved_lid_scheme = None;
    backend.persist_config(cfg);
    true
}

pub fn enable(cfg: &mut AppConfig) -> bool {
    enable_with(cfg, &WinPowerBackend)
}

pub fn disable(cfg: &mut AppConfig) -> bool {
    disable_with(cfg, &WinPowerBackend)
}

pub fn suspend_for_exit(cfg: &mut AppConfig) -> bool {
    suspend_for_exit_with(cfg, &WinPowerBackend)
}

pub fn recover_if_needed(cfg: &mut AppConfig) {
    if !cfg.awake_enabled && cfg.saved_lid_ac.is_some() {
        let _ = restore_lid_with(cfg, &WinPowerBackend);
    } else if cfg.awake_enabled {
        let _ = enable_with(cfg, &WinPowerBackend);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct FakePower {
        scheme: String,
        lid_ac: RefCell<u32>,
        awake: RefCell<bool>,
        fail_hold: bool,
    }

    impl PowerBackend for FakePower {
        fn active_scheme(&self) -> Option<String> {
            Some(self.scheme.clone())
        }
        fn read_lid_ac(&self, _scheme: &str) -> Option<u32> {
            Some(*self.lid_ac.borrow())
        }
        fn write_lid_ac(&self, _scheme: &str, action: u32) -> bool {
            *self.lid_ac.borrow_mut() = action;
            true
        }
        fn activate_scheme(&self, _scheme: &str) -> bool {
            true
        }
        fn hold_awake(&self, _keep_display: bool) -> bool {
            if self.fail_hold {
                return false;
            }
            *self.awake.borrow_mut() = true;
            true
        }
        fn release_awake(&self) {
            *self.awake.borrow_mut() = false;
        }
        fn persist_config(&self, _cfg: &AppConfig) {}
    }

    #[test]
    fn suspend_for_exit_restores_lid_without_clearing_awake_preference() {
        let fake = FakePower {
            scheme: "381b4222-f694-41f0-9685-ff5bb260df2e".into(),
            lid_ac: RefCell::new(1),
            awake: RefCell::new(false),
            fail_hold: false,
        };
        let mut cfg = AppConfig::default();
        assert!(enable_with(&mut cfg, &fake));
        assert!(cfg.awake_enabled);
        assert_eq!(*fake.lid_ac.borrow(), 0);

        assert!(suspend_for_exit_with(&mut cfg, &fake));
        assert_eq!(*fake.lid_ac.borrow(), 1);
        assert_eq!(cfg.saved_lid_ac, None);
        assert!(!*fake.awake.borrow());
        assert!(cfg.awake_enabled);
    }

    #[test]
    fn enable_saves_and_disable_restores_lid_action() {
        let fake = FakePower {
            scheme: "381b4222-f694-41f0-9685-ff5bb260df2e".into(),
            lid_ac: RefCell::new(1), // Sleep
            awake: RefCell::new(false),
            fail_hold: false,
        };
        let mut cfg = AppConfig::default();
        assert!(enable_with(&mut cfg, &fake));
        assert_eq!(*fake.lid_ac.borrow(), 0);
        assert_eq!(cfg.saved_lid_ac, Some(1));
        assert!(*fake.awake.borrow());

        assert!(disable_with(&mut cfg, &fake));
        assert_eq!(*fake.lid_ac.borrow(), 1);
        assert_eq!(cfg.saved_lid_ac, None);
        assert!(!*fake.awake.borrow());
    }

    #[test]
    fn rolls_back_lid_setting_if_execution_state_hold_fails() {
        let fake = FakePower {
            scheme: "381b4222-f694-41f0-9685-ff5bb260df2e".into(),
            lid_ac: RefCell::new(1),
            awake: RefCell::new(false),
            fail_hold: true,
        };
        let mut cfg = AppConfig::default();
        assert!(!enable_with(&mut cfg, &fake));
        assert_eq!(*fake.lid_ac.borrow(), 1);
        assert_eq!(cfg.saved_lid_ac, None);
    }
}
