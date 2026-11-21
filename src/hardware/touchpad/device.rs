//! Precision Touchpad device discovery, hardware toggle (`CfgMgr32`), and Windows
//! `SuperCurtainBottom/Left/Right` registry management.

use std::thread;
use std::time::Duration;
use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_DEVNODE_STATUS_FLAGS, CM_Disable_DevNode, CM_Enable_DevNode, CM_GETIDLIST_FILTER_PRESENT,
    CM_Get_DevNode_PropertyW, CM_Get_DevNode_Status, CM_Get_Device_ID_List_SizeW,
    CM_Get_Device_ID_ListW, CM_Get_Device_IDW, CM_Get_Parent, CM_LOCATE_DEVNODE_FLAGS,
    CM_LOCATE_DEVNODE_PHANTOM, CM_Locate_DevNodeW, CM_PROB, CM_PROB_DISABLED, CR_SUCCESS,
    DN_HAS_PROBLEM,
};
use windows::Win32::Devices::Properties::{DEVPROP_TYPE_STRING_LIST, DEVPROPTYPE};
use windows::Win32::Foundation::DEVPROPKEY;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};
use windows_core::{GUID, PCWSTR};

/// `DEVPKEY_Device_HardwareIds` = `{a45c254e-df1c-4efd-8020-67d146a850e0}`, pid 3.
const DEVPKEY_DEVICE_HARDWARE_IDS: DEVPROPKEY = DEVPROPKEY {
    fmtid: GUID::from_u128(0xa45c254e_df1c_4efd_8020_67d146a850e0),
    pid: 3,
};

const PTP_REG_PATH: &str = "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\PrecisionTouchPad";
const PTP_HID_NEEDLE: &str = "HID_DEVICE_UP:000D_U:0005";

#[derive(Debug, Clone)]
pub struct TouchpadDevice {
    parent_instance_id: String,
}

impl TouchpadDevice {
    /// Discover the Precision Touchpad's parent HID/I2C device node (e.g. `ACPI\GXTP7863\...`).
    pub fn discover() -> Option<Self> {
        // SAFETY: Standard CfgMgr32 device ID list enumeration on present devices.
        unsafe {
            let mut len = 0u32;
            if CM_Get_Device_ID_List_SizeW(&mut len, PCWSTR::null(), CM_GETIDLIST_FILTER_PRESENT)
                != CR_SUCCESS
                || len == 0
            {
                return None;
            }
            let mut buf = vec![0u16; len as usize];
            if CM_Get_Device_ID_ListW(PCWSTR::null(), &mut buf, CM_GETIDLIST_FILTER_PRESENT)
                != CR_SUCCESS
            {
                return None;
            }

            // Iterate NUL-separated UTF-16 device IDs ending with double NUL.
            for chunk in buf.split(|&c| c == 0) {
                if chunk.is_empty() {
                    break;
                }
                let id_str = String::from_utf16_lossy(chunk);
                if !id_str.to_ascii_uppercase().starts_with("HID\\") {
                    continue;
                }

                let mut wide: Vec<u16> = chunk.to_vec();
                wide.push(0);
                let mut devinst = 0u32;
                if CM_Locate_DevNodeW(
                    &mut devinst,
                    PCWSTR(wide.as_ptr()),
                    CM_LOCATE_DEVNODE_FLAGS(0),
                ) != CR_SUCCESS
                {
                    continue;
                }

                if !has_hardware_id(devinst, PTP_HID_NEEDLE) {
                    continue;
                }

                let mut parent = 0u32;
                if CM_Get_Parent(&mut parent, devinst, 0) != CR_SUCCESS {
                    continue;
                }
                let mut parent_buf = [0u16; 260];
                if CM_Get_Device_IDW(parent, &mut parent_buf, 0) == CR_SUCCESS {
                    let end = parent_buf.iter().position(|&c| c == 0).unwrap_or(parent_buf.len());
                    let parent_id = String::from_utf16_lossy(&parent_buf[..end]);
                    return Some(Self {
                        parent_instance_id: parent_id,
                    });
                }
            }
        }
        None
    }

    pub fn instance_id(&self) -> &str {
        &self.parent_instance_id
    }

    fn locate_parent(&self) -> Option<u32> {
        let wide: Vec<u16> = self
            .parent_instance_id
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut devinst = 0u32;
        // SAFETY: Valid NUL-terminated UTF-16 device instance ID.
        let cr = unsafe {
            CM_Locate_DevNodeW(&mut devinst, PCWSTR(wide.as_ptr()), CM_LOCATE_DEVNODE_PHANTOM)
        };
        (cr == CR_SUCCESS).then_some(devinst)
    }

    /// Returns `Some(true)` if enabled, `Some(false)` if disabled, `None` if unavailable.
    pub fn is_enabled(&self) -> Option<bool> {
        let devinst = self.locate_parent()?;
        let mut status = CM_DEVNODE_STATUS_FLAGS(0);
        let mut problem = CM_PROB(0);
        // SAFETY: Valid devinst handle from CM_Locate_DevNodeW.
        let cr = unsafe { CM_Get_DevNode_Status(&mut status, &mut problem, devinst, 0) };
        if cr != CR_SUCCESS {
            return None;
        }
        let disabled = (status.0 & DN_HAS_PROBLEM.0) != 0 && problem == CM_PROB_DISABLED;
        Some(!disabled)
    }

    /// Enable or disable the Precision Touchpad parent hardware node (without touching PTP registry).
    pub fn set_enabled(&self, enable: bool) -> bool {
        let Some(devinst) = self.locate_parent() else {
            return false;
        };
        // SAFETY: Valid devinst handle; caller runs elevated as Administrator.
        let cr = unsafe {
            if enable {
                CM_Enable_DevNode(devinst, 0)
            } else {
                CM_Disable_DevNode(devinst, 0)
            }
        };
        cr == CR_SUCCESS
    }

    /// Restart the touchpad node so `SuperCurtain*` registry changes apply immediately.
    pub fn restart(&self) -> bool {
        if !self.set_enabled(false) {
            return false;
        }
        thread::sleep(Duration::from_millis(80));
        self.set_enabled(true)
    }
}

unsafe fn has_hardware_id(devinst: u32, needle_upper: &str) -> bool {
    let mut prop_type = DEVPROPTYPE(0);
    let mut size = 0u32;
    // SAFETY: Caller passes valid devinst; first call queries required buffer byte length.
    let _ = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_DEVICE_HARDWARE_IDS,
            &mut prop_type,
            None,
            &mut size,
            0,
        )
    };
    if size == 0 {
        return false;
    }
    let mut raw = vec![0u8; size as usize];
    // SAFETY: Valid buffer matching queried byte size.
    let cr = unsafe {
        CM_Get_DevNode_PropertyW(
            devinst,
            &DEVPKEY_DEVICE_HARDWARE_IDS,
            &mut prop_type,
            Some(raw.as_mut_ptr()),
            &mut size,
            0,
        )
    };
    if cr != CR_SUCCESS || prop_type != DEVPROP_TYPE_STRING_LIST {
        return false;
    }
    let utf16: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&c| u16::from_le_bytes(c))
        .collect();
    for entry in utf16.split(|&c| c == 0) {
        if entry.is_empty() {
            break;
        }
        if String::from_utf16_lossy(entry)
            .to_ascii_uppercase()
            .contains(needle_upper)
        {
            return true;
        }
    }
    false
}

/// Check whether the current HKLM `SuperCurtainBottom/Left/Right` DWORD values already match
/// the requested settings so startup avoids restarting the touchpad unnecessarily.
pub fn curtain_zones_match(bottom_mm: Option<u32>, edge_mm: Option<u32>) -> bool {
    let want_bottom = bottom_mm.map(|m| m.clamp(1, 40) * 100);
    let want_edge = edge_mm.map(|m| m.clamp(4, 30) * 100);
    let path_w: Vec<u16> = PTP_REG_PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey = HKEY::default();
    // SAFETY: Read-only HKLM subkey open with KEY_QUERY_VALUE.
    unsafe {
        if RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path_w.as_ptr()),
            None,
            KEY_QUERY_VALUE,
            &mut hkey,
        )
        .is_err()
        {
            return want_bottom.is_none() && want_edge.is_none();
        }
        let ok = read_dword(hkey, "SuperCurtainBottom") == want_bottom
            && read_dword(hkey, "SuperCurtainLeft") == want_edge
            && read_dword(hkey, "SuperCurtainRight") == want_edge;
        let _ = RegCloseKey(hkey);
        ok
    }
}

unsafe fn read_dword(hkey: HKEY, name: &str) -> Option<u32> {
    let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut reg_type = REG_VALUE_TYPE(0);
    let mut buf = [0u8; 4];
    let mut len = 4u32;
    // SAFETY: Valid open HKEY, NUL-terminated name, and 4-byte stack buffer for REG_DWORD.
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut reg_type),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        )
    };
    if rc.is_ok() && reg_type == REG_DWORD && len == 4 {
        Some(u32::from_le_bytes(buf))
    } else {
        None
    }
}

/// Write or remove `SuperCurtainBottom` / `SuperCurtainLeft` / `SuperCurtainRight`
/// under `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\PrecisionTouchPad`.
pub fn apply_curtain_zones(bottom_mm: Option<u32>, edge_mm: Option<u32>) -> bool {
    let path_w: Vec<u16> = PTP_REG_PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey = HKEY::default();
    // SAFETY: Standard HKLM subkey open/create with KEY_SET_VALUE.
    unsafe {
        let rc = RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(path_w.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        );
        if rc.is_err() {
            return false;
        }
        let r1 = write_or_delete_dword(hkey, "SuperCurtainBottom", bottom_mm.map(|m| m.clamp(1, 40) * 100));
        let edge_himetric = edge_mm.map(|m| m.clamp(4, 30) * 100);
        let r2 = write_or_delete_dword(hkey, "SuperCurtainLeft", edge_himetric);
        let r3 = write_or_delete_dword(hkey, "SuperCurtainRight", edge_himetric);
        let _ = RegCloseKey(hkey);
        r1 && r2 && r3
    }
}

unsafe fn write_or_delete_dword(hkey: HKEY, name: &str, value: Option<u32>) -> bool {
    let name_w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: Valid open HKEY and NUL-terminated UTF-16 value name.
    unsafe {
        match value {
            Some(v) => {
                let bytes = v.to_le_bytes();
                RegSetValueExW(hkey, PCWSTR(name_w.as_ptr()), None, REG_DWORD, Some(&bytes)).is_ok()
            }
            None => {
                let _ = RegDeleteValueW(hkey, PCWSTR(name_w.as_ptr()));
                true
            }
        }
    }
}
