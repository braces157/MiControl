//! Elevated Task Scheduler autostart (`/SC ONLOGON /RL HIGHEST`), avoiding UAC prompts at login.

use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK_NAME: &str = "MiControlAutoStart";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Check whether the Task Scheduler logon entry currently exists.
pub fn is_enabled() -> bool {
    Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK_NAME])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Enable or disable elevated autostart pointing to the current executable.
pub fn set_enabled(enable: bool) -> Result<(), String> {
    if enable {
        let exe = std::env::current_exe()
            .map_err(|e| format!("Failed to locate executable path: {e}"))?;
        let tr = format!("\"{}\"", exe.display());
        let out = Command::new("schtasks.exe")
            .args([
                "/Create",
                "/TN",
                TASK_NAME,
                "/SC",
                "ONLOGON",
                "/RL",
                "HIGHEST",
                "/F",
                "/TR",
                &tr,
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Failed to run schtasks.exe: {e}"))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    } else {
        let out = Command::new("schtasks.exe")
            .args(["/Delete", "/TN", TASK_NAME, "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| format!("Failed to run schtasks.exe: {e}"))?;
        if out.status.success() || !is_enabled() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}
