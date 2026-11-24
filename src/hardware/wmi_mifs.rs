//! Native COM/WMI driver-free transport for Xiaomi/Redmi MIFS (`MICommonInterface`),
//! `HID_EVENT20` hotkey push notifications, and `WmiMonitorBrightness`.
//!
//! Zero external WMI crates — talks directly to `IWbemLocator` / `IWbemServices` via `windows-rs`.

use crate::protocol::{
    self, CMD_CHARGE, CMD_MIC, CMD_PERF, ChargeLimit, HidEvent, MIFS_BUFFER_SIZE, MifsDialect,
    PerfMode, SUB_ADAPTER_WATTS, SUB_BATTERY_HEALTH, SUB_CHARGE_LIMIT,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoInitializeSecurity, CoSetProxyBlanket, CoUninitialize, EOAC_NONE,
    RPC_C_AUTHN_LEVEL_CALL, RPC_C_AUTHN_LEVEL_DEFAULT, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Ole::{
    SafeArrayAccessData, SafeArrayCreateVector, SafeArrayDestroy, SafeArrayGetLBound,
    SafeArrayGetUBound, SafeArrayUnaccessData,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VARIANT, VT_ARRAY, VT_BSTR, VT_I4, VT_UI1, VT_UI4};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemObjectSink, IWbemObjectSink_Impl, IWbemServices,
    WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE, WbemLocator,
};
use windows_core::{BSTR, Error, PCWSTR, Result as WinResult, implement};

/// RAII guard for COM initialization on the current thread.
pub struct ComGuard {
    initialized: bool,
}

impl ComGuard {
    pub fn init_mta() -> Self {
        // SAFETY: Standard per-thread COM initialization. S_OK / S_FALSE both require CoUninitialize.
        let hr = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        if hr.is_ok() {
            // Best-effort process security init; RPC_E_TOO_LATE is expected if already initialized.
            // SAFETY: Standard WMI COM security setup with default principal and impersonation.
            unsafe {
                let _ = CoInitializeSecurity(
                    None,
                    -1,
                    None,
                    None,
                    RPC_C_AUTHN_LEVEL_DEFAULT,
                    RPC_C_IMP_LEVEL_IMPERSONATE,
                    None,
                    EOAC_NONE,
                    None,
                );
            }
            Self { initialized: true }
        } else {
            Self { initialized: false }
        }
    }
}

impl Drop for ComGuard {
    fn drop(&mut self) {
        if self.initialized {
            // SAFETY: Paired 1:1 with a successful CoInitializeEx on this thread.
            unsafe { CoUninitialize() };
        }
    }
}

fn connect_root_wmi() -> WinResult<IWbemServices> {
    // SAFETY: Standard WbemLocator COM activation and ConnectServer to local ROOT\WMI.
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let services = locator.ConnectServer(
            &BSTR::from("ROOT\\WMI"),
            &BSTR::new(),
            &BSTR::new(),
            &BSTR::new(),
            0,
            &BSTR::new(),
            None,
        )?;
        CoSetProxyBlanket(
            &services,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            PCWSTR::null(),
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )?;
        Ok(services)
    }
}

fn bytes_to_variant(bytes: &[u8]) -> WinResult<VARIANT> {
    // SAFETY: SafeArrayCreateVector allocates a 1D byte SAFEARRAY; we lock, copy, unlock,
    // and transfer ownership into VARIANT (which calls VariantClear -> SafeArrayDestroy on drop).
    unsafe {
        let psa = SafeArrayCreateVector(VT_UI1, 0, bytes.len() as u32);
        if psa.is_null() {
            return Err(Error::from_hresult(windows::Win32::Foundation::E_OUTOFMEMORY));
        }
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        if let Err(err) = SafeArrayAccessData(psa, &mut ptr) {
            let _ = SafeArrayDestroy(psa);
            return Err(err);
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr as *mut u8, bytes.len());
        let _ = SafeArrayUnaccessData(psa);

        let mut var = VARIANT::default();
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_ARRAY | VT_UI1;
        inner.Anonymous.parray = psa;
        Ok(var)
    }
}

fn variant_to_bytes(var: &VARIANT) -> Option<Vec<u8>> {
    // SAFETY: We check the exact variant tag (`VT_ARRAY | VT_UI1`) and non-null SAFEARRAY pointer
    // before accessing contiguous element memory and unaccessing before returning.
    unsafe {
        let inner = &*var.Anonymous.Anonymous;
        if inner.vt != (VT_ARRAY | VT_UI1) {
            return None;
        }
        let psa = inner.Anonymous.parray;
        if psa.is_null() {
            return None;
        }
        let lbound = SafeArrayGetLBound(psa, 1).ok()?;
        let ubound = SafeArrayGetUBound(psa, 1).ok()?;
        if ubound < lbound {
            return Some(Vec::new());
        }
        let len = (ubound - lbound + 1) as usize;
        let mut ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        SafeArrayAccessData(psa, &mut ptr).ok()?;
        let slice = std::slice::from_raw_parts(ptr as *const u8, len);
        let vec = slice.to_vec();
        let _ = SafeArrayUnaccessData(psa);
        Some(vec)
    }
}

fn get_string_prop(obj: &IWbemClassObject, name: &str) -> Option<String> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut val = VARIANT::default();
    // SAFETY: Valid NUL-terminated UTF-16 property name and initialized VARIANT out-param;
    // VT_BSTR tag verified before reading bstrVal.
    unsafe {
        obj.Get(PCWSTR(wide.as_ptr()), 0, &mut val, None, None).ok()?;
        let inner = &*val.Anonymous.Anonymous;
        if inner.vt != VT_BSTR {
            return None;
        }
        let s = inner.Anonymous.bstrVal.to_string();
        if s.is_empty() { None } else { Some(s) }
    }
}

fn get_u8_prop(obj: &IWbemClassObject, name: &str) -> Option<u8> {
    let wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut val = VARIANT::default();
    // SAFETY: Valid NUL-terminated UTF-16 property name; exact variant tag verified before union access.
    unsafe {
        obj.Get(PCWSTR(wide.as_ptr()), 0, &mut val, None, None).ok()?;
        let inner = &*val.Anonymous.Anonymous;
        match inner.vt {
            VT_UI1 => Some(inner.Anonymous.bVal),
            VT_I4 => u8::try_from(inner.Anonymous.lVal).ok(),
            VT_UI4 => u8::try_from(inner.Anonymous.ulVal).ok(),
            VT_BSTR => inner.Anonymous.bstrVal.to_string().trim().parse::<u8>().ok(),
            _ => None,
        }
    }
}

/// Query the canonical WMI `__RELPATH` (e.g. `MICommonInterface.InstanceName="ACPI\\PNP0C14\\MIFS_0"`)
/// for the first instance of `class_name`. Using `__RELPATH` directly avoids single-quote/escaping bugs
/// that cause `WBEM_E_NOT_FOUND` (`0x80041002`) in `IWbemServices::ExecMethod`.
fn query_first_relpath(services: &IWbemServices, class_name: &str) -> WinResult<String> {
    let query = format!("SELECT * FROM {class_name}");
    // SAFETY: Standard WQL ExecQuery + Next(1) enumeration reading system property `__RELPATH`.
    unsafe {
        let enumerator = services.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(query),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?;
        let mut objs = [None];
        let mut returned = 0u32;
        enumerator.Next(WBEM_INFINITE, &mut objs, &mut returned).ok()?;
        if returned == 0 {
            return Err(Error::from_hresult(windows::Win32::Foundation::E_FAIL));
        }
        let obj = objs[0]
            .as_ref()
            .ok_or_else(|| Error::from_hresult(windows::Win32::Foundation::E_FAIL))?;
        get_string_prop(obj, "__RELPATH")
            .ok_or_else(|| Error::from_hresult(windows::Win32::Foundation::E_FAIL))
    }
}

/// Live WMI client for `MICommonInterface::MiInterface` and `WmiMonitorBrightness`.
pub struct MifsClient {
    _com: ComGuard,
    services: IWbemServices,
    instance_path: String,
    in_params_template: IWbemClassObject,
    dialect: MifsDialect,
}

impl MifsClient {
    pub fn connect() -> Result<Self, String> {
        let com = ComGuard::init_mta();
        let services = connect_root_wmi()
            .map_err(|e| format!("Failed to connect ROOT\\WMI (Admin required): {e}"))?;

        let instance_path = query_first_relpath(&services, "MICommonInterface")
            .map_err(|e| format!("MICommonInterface instance not found (non-Xiaomi or no admin): {e}"))?;

        // SAFETY: Standard IWbemServices::GetObject + GetMethod on verified class name.
        let in_params_template = unsafe {
            let mut class_obj: Option<IWbemClassObject> = None;
            services
                .GetObject(
                    &BSTR::from("MICommonInterface"),
                    Default::default(),
                    None,
                    Some(&mut class_obj),
                    None,
                )
                .map_err(|e| format!("GetObject(MICommonInterface) failed: {e}"))?;
            let class_obj = class_obj.ok_or_else(|| "MICommonInterface class null".to_string())?;
            let mut in_sig: Option<IWbemClassObject> = None;
            class_obj
                .GetMethod(windows_core::w!("MiInterface"), 0, &mut in_sig, std::ptr::null_mut())
                .map_err(|e| format!("GetMethod(MiInterface) failed: {e}"))?;
            in_sig.ok_or_else(|| "MiInterface input signature null".to_string())?
        };

        let mut client = Self {
            _com: com,
            services,
            instance_path,
            in_params_template,
            dialect: MifsDialect::Unsupported,
        };

        let probe = protocol::build_get(CMD_PERF, 0, 0);
        let out = client
            .invoke_raw(&probe)
            .map_err(|e| format!("MIFS dialect probe failed ({}): {e}", client.instance_path))?;
        client.dialect = protocol::detect_dialect(CMD_PERF, &out);

        Ok(client)
    }

    pub const fn dialect(&self) -> MifsDialect {
        self.dialect
    }

    /// Invoke `MICommonInterface::MiInterface` with a 32-byte payload and return `OutData`.
    pub fn invoke_raw(&self, in_data: &[u8; MIFS_BUFFER_SIZE]) -> Result<Vec<u8>, String> {
        // SAFETY: Spawns fresh method parameter instance, writes VT_ARRAY|VT_UI1 InData,
        // invokes ExecMethod, and extracts OutData SAFEARRAY.
        unsafe {
            let in_inst = self
                .in_params_template
                .SpawnInstance(0)
                .map_err(|e| format!("SpawnInstance failed: {e}"))?;
            let var_in = bytes_to_variant(in_data)
                .map_err(|e| format!("InData SAFEARRAY creation failed: {e}"))?;
            in_inst
                .Put(windows_core::w!("InData"), 0, &var_in, 0)
                .map_err(|e| format!("InData Put failed: {e}"))?;

            let mut out_params: Option<IWbemClassObject> = None;
            self.services
                .ExecMethod(
                    &BSTR::from(&self.instance_path),
                    &BSTR::from("MiInterface"),
                    Default::default(),
                    None,
                    &in_inst,
                    Some(&mut out_params),
                    None,
                )
                .map_err(|e| format!("ExecMethod(MiInterface) failed: {e}"))?;

            let out_obj = out_params.ok_or_else(|| "MiInterface returned no output".to_string())?;
            let mut var_out = VARIANT::default();
            out_obj
                .Get(windows_core::w!("OutData"), 0, &mut var_out, None, None)
                .map_err(|e| format!("OutData Get failed: {e}"))?;

            variant_to_bytes(&var_out)
                .ok_or_else(|| "OutData was not a byte array".to_string())
        }
    }

    pub fn get_perf_mode(&self) -> Result<Option<PerfMode>, String> {
        let out = self.invoke_raw(&protocol::build_get(CMD_PERF, 0, 0))?;
        Ok(protocol::parse_perf_reply(self.dialect, &out))
    }

    /// Write performance mode and verify firmware actually accepted it via readback.
    pub fn set_perf_mode(&self, mode: PerfMode) -> Result<bool, String> {
        let out = self.invoke_raw(&protocol::build_set(CMD_PERF, mode.as_byte(), 0))?;
        if !protocol::is_set_accepted(self.dialect, CMD_PERF, &out) {
            return Ok(false);
        }
        // Firmware may echo success while silently rejecting (e.g. Balance on TM24 or FullSpeed on DC).
        let readback = self.get_perf_mode()?;
        Ok(readback == Some(mode))
    }

    pub fn get_charge_limit(&self) -> Result<Option<ChargeLimit>, String> {
        if !self.dialect.supports_charge_group() {
            return Ok(None);
        }
        let out = self.invoke_raw(&protocol::build_get(CMD_CHARGE, SUB_CHARGE_LIMIT, 0))?;
        Ok(protocol::parse_charge_reply(self.dialect, &out))
    }

    /// Set charge limit. When enabling a care limit (`< 100%`), follows the EC state-machine reset
    /// sequence (`SET 0` -> 80ms -> `SET code`) unless `urgent` (pre-suspend) requires zero delay.
    pub fn set_charge_limit(&self, limit: ChargeLimit, urgent: bool) -> Result<bool, String> {
        if !self.dialect.supports_charge_group() {
            return Ok(false);
        }

        if limit.is_care_active() && !urgent {
            let _ = self.invoke_raw(&protocol::build_set(CMD_CHARGE, SUB_CHARGE_LIMIT, 0));
            thread::sleep(Duration::from_millis(80));
        }

        let out = self.invoke_raw(&protocol::build_set(
            CMD_CHARGE,
            SUB_CHARGE_LIMIT,
            limit.mifs_code(),
        ))?;
        Ok(protocol::is_set_accepted(self.dialect, CMD_CHARGE, &out))
    }

    /// Connected USB-C PD adapter wattage (W), or `None` / `Some(0)` if on battery / non-PD.
    pub fn get_adapter_watts(&self) -> Result<Option<u8>, String> {
        if !self.dialect.supports_charge_group() {
            return Ok(None);
        }
        let out = self.invoke_raw(&protocol::build_get(CMD_CHARGE, SUB_ADAPTER_WATTS, 0))?;
        Ok(protocol::parse_charge_group_byte(self.dialect, &out))
    }

    /// Battery health (SOH1 %) reported by EC (`cmd 0x10`, `sub 0x01`).
    pub fn get_battery_health(&self) -> Result<Option<u8>, String> {
        if !self.dialect.supports_charge_group() {
            return Ok(None);
        }
        let out = self.invoke_raw(&protocol::build_get(CMD_CHARGE, SUB_BATTERY_HEALTH, 0))?;
        Ok(protocol::parse_charge_group_byte(self.dialect, &out))
    }

    /// Sync hardware mic LED / mute state (`cmd 0x0A`).
    pub fn set_mic_mute_led(&self, muted: bool) -> Result<bool, String> {
        let val = if muted { 0 } else { 1 };
        let out = self.invoke_raw(&protocol::build_set(CMD_MIC, val, 0))?;
        Ok(protocol::is_set_accepted(self.dialect, CMD_MIC, &out))
    }

    /// Read current internal display brightness (0..=100) via `WmiMonitorBrightness`.
    pub fn get_display_brightness(&self) -> Option<u8> {
        // SAFETY: Standard WQL query on built-in WmiMonitorBrightness class.
        unsafe {
            let enumerator = self
                .services
                .ExecQuery(
                    &BSTR::from("WQL"),
                    &BSTR::from("SELECT CurrentBrightness FROM WmiMonitorBrightness"),
                    WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
                    None,
                )
                .ok()?;
            let mut objs = [None];
            let mut returned = 0u32;
            enumerator.Next(WBEM_INFINITE, &mut objs, &mut returned).ok().ok()?;
            if returned == 0 {
                return None;
            }
            get_u8_prop(objs[0].as_ref()?, "CurrentBrightness")
        }
    }

    /// Set internal display brightness (0..=100) via `WmiMonitorBrightnessMethods::WmiSetBrightness`.
    pub fn set_display_brightness(&self, level: u8) -> bool {
        let clamped = level.min(100);
        let Ok(path) = query_first_relpath(&self.services, "WmiMonitorBrightnessMethods") else {
            return false;
        };
        // SAFETY: Standard ExecMethod on WmiMonitorBrightnessMethods with Timeout=0 and Brightness=u8.
        unsafe {
            let mut class_obj: Option<IWbemClassObject> = None;
            if self
                .services
                .GetObject(
                    &BSTR::from("WmiMonitorBrightnessMethods"),
                    Default::default(),
                    None,
                    Some(&mut class_obj),
                    None,
                )
                .is_err()
            {
                return false;
            }
            let Some(class_obj) = class_obj else {
                return false;
            };
            let mut in_sig: Option<IWbemClassObject> = None;
            if class_obj
                .GetMethod(
                    windows_core::w!("WmiSetBrightness"),
                    0,
                    &mut in_sig,
                    std::ptr::null_mut(),
                )
                .is_err()
            {
                return false;
            }
            let Some(in_sig) = in_sig else {
                return false;
            };
            let Ok(in_inst) = in_sig.SpawnInstance(0) else {
                return false;
            };
            let v_timeout = VARIANT::from(0u32);
            let v_level = VARIANT::from(clamped);
            if in_inst.Put(windows_core::w!("Timeout"), 0, &v_timeout, 0).is_err() {
                return false;
            }
            if in_inst.Put(windows_core::w!("Brightness"), 0, &v_level, 0).is_err() {
                return false;
            }
            self.services
                .ExecMethod(
                    &BSTR::from(&path),
                    &BSTR::from("WmiSetBrightness"),
                    Default::default(),
                    None,
                    &in_inst,
                    None,
                    None,
                )
                .is_ok()
        }
    }
}

#[implement(IWbemObjectSink)]
struct HidEventSink {
    on_event: Arc<dyn Fn(HidEvent) + Send + Sync>,
}

impl IWbemObjectSink_Impl for HidEventSink_Impl {
    fn Indicate(&self, lobjectcount: i32, apobjarray: *const Option<IWbemClassObject>) -> WinResult<()> {
        if lobjectcount <= 0 || apobjarray.is_null() {
            return Ok(());
        }
        // SAFETY: WMI guarantees `apobjarray` contains `lobjectcount` valid pointers for the duration of Indicate.
        let slice = unsafe { std::slice::from_raw_parts(apobjarray, lobjectcount as usize) };
        for item in slice {
            let Some(obj) = item else { continue };
            let mut var = VARIANT::default();
            // SAFETY: Valid property lookup for EventDetail byte array on HID_EVENT20.
            let ok = unsafe {
                obj.Get(windows_core::w!("EventDetail"), 0, &mut var, None, None).is_ok()
            };
            if ok
                && let Some(bytes) = variant_to_bytes(&var)
                && let Some(ev) = protocol::decode_hid_event(&bytes)
            {
                (self.on_event)(ev);
            }
        }
        Ok(())
    }

    fn SetStatus(
        &self,
        _lflags: i32,
        _hresult: windows_core::HRESULT,
        _strparam: &BSTR,
        _pobjparam: windows_core::Ref<'_, IWbemClassObject>,
    ) -> WinResult<()> {
        Ok(())
    }
}

/// Background push listener for `SELECT * FROM HID_EVENT20`.
/// Consumes 0% CPU while idle; cleanly unsubscribes on drop.
pub struct HidEventWatcher {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl HidEventWatcher {
    pub fn start(on_event: impl Fn(HidEvent) + Send + Sync + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = Arc::clone(&stop);
        let cb: Arc<dyn Fn(HidEvent) + Send + Sync> = Arc::new(on_event);

        let handle = thread::Builder::new()
            .name("micontrol-wmi-events".into())
            .spawn(move || {
                let _com = ComGuard::init_mta();
                let Ok(services) = connect_root_wmi() else {
                    return;
                };
                let sink: IWbemObjectSink = HidEventSink { on_event: cb }.into();
                // SAFETY: Subscribes async COM sink to HID_EVENT20 and cancels before thread exit.
                let sub = unsafe {
                    services.ExecNotificationQueryAsync(
                        &BSTR::from("WQL"),
                        &BSTR::from("SELECT * FROM HID_EVENT20"),
                        Default::default(),
                        None,
                        &sink,
                    )
                };
                if sub.is_err() {
                    return;
                }
                while !stop_bg.load(Ordering::Relaxed) {
                    thread::sleep(Duration::from_millis(250));
                }
                // SAFETY: Cancels the active async notification subscription cleanly.
                unsafe {
                    let _ = services.CancelAsyncCall(&sink);
                }
            })
            .ok();

        Self { stop, handle }
    }
}

impl Drop for HidEventWatcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_in_data_put_on_local_micommoninterface_schema() {
        let _com = ComGuard::init_mta();
        let Ok(services) = connect_root_wmi() else {
            // Non-interactive / restricted WMI environment (e.g. headless CI).
            return;
        };
        unsafe {
            let mut class_obj: Option<IWbemClassObject> = None;
            if services
                .GetObject(
                    &BSTR::from("MICommonInterface"),
                    Default::default(),
                    None,
                    Some(&mut class_obj),
                    None,
                )
                .is_err()
            {
                // Skip Xiaomi-specific firmware schema checks on non-Xiaomi hosts / CI VMs.
                return;
            }
            let class_obj = class_obj.expect("class_obj");
            let mut in_sig: Option<IWbemClassObject> = None;
            class_obj
                .GetMethod(windows_core::w!("MiInterface"), 0, &mut in_sig, std::ptr::null_mut())
                .expect("GetMethod MiInterface");
            let in_sig = in_sig.expect("in_sig");
            let in_inst = in_sig.SpawnInstance(0).expect("SpawnInstance");

            let buf = protocol::build_get(CMD_PERF, 0, 0);
            let var_in = bytes_to_variant(&buf).expect("bytes_to_variant");
            let put_res = in_inst.Put(windows_core::w!("InData"), 0, &var_in, 0);
            println!("Put(VT_ARRAY|VT_UI1) result: {put_res:?}");

            let mut back = VARIANT::default();
            let get_res = in_inst.Get(windows_core::w!("InData"), 0, &mut back, None, None);
            println!("Get back result: {get_res:?}, vt=0x{:x}", (*back.Anonymous.Anonymous).vt.0);
            println!("variant_to_bytes: {:?}", variant_to_bytes(&back).map(|v| v.len()));

            let mut cimtype: i32 = 0;
            let _ = in_sig.Get(windows_core::w!("InData"), 0, std::ptr::null_mut(), Some(&mut cimtype), None);
            println!("InData cimtype = 0x{:x}", cimtype);

            let mut out_sig: Option<IWbemClassObject> = None;
            class_obj
                .GetMethod(windows_core::w!("MiInterface"), 0, std::ptr::null_mut(), &mut out_sig)
                .expect("GetMethod out_sig");
            if let Some(os) = out_sig {
                let mut out_ct: i32 = 0;
                let _ = os.Get(windows_core::w!("OutData"), 0, std::ptr::null_mut(), Some(&mut out_ct), None);
                println!("OutData cimtype = 0x{:x}", out_ct);
            }
            assert!(put_res.is_ok(), "Put failed: {put_res:?}");

            // Regression pin: canonical __RELPATH must resolve cleanly via GetObject,
            // whereas single-quoted paths fail with WBEM_E_NOT_FOUND (0x80041002).
            if let Ok(relpath) = query_first_relpath(&services, "WmiMonitorBrightness") {
                assert!(
                    relpath.contains("InstanceName=\""),
                    "expected canonical double-quoted __RELPATH, got: {relpath}"
                );
                let mut out_obj: Option<IWbemClassObject> = None;
                services
                    .GetObject(&BSTR::from(&relpath), Default::default(), None, Some(&mut out_obj), None)
                    .expect("GetObject(__RELPATH) must succeed");
                assert!(out_obj.is_some());

                let bright_methods = query_first_relpath(&services, "WmiMonitorBrightnessMethods")
                    .expect("query_first_relpath(WmiMonitorBrightnessMethods)");
                assert!(bright_methods.contains("InstanceName=\""));
            }
        }
    }
}
