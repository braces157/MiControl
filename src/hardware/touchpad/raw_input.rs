//! Ultra-lightweight Raw Input Precision Touchpad reader running on a dedicated `HWND_MESSAGE`
//! thread (only spawned when Edge Sliders are enabled — 0 threads and 0% CPU when off).

use crate::hardware::touchpad::gesture::TouchContact;
use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::thread::{self, JoinHandle};
use windows::Win32::Devices::HumanInterfaceDevice::{
    HIDP_CAPS, HIDP_STATUS_SUCCESS, HIDP_VALUE_CAPS, HidP_GetCaps, HidP_GetUsages,
    HidP_GetUsageValue, HidP_GetValueCaps, HidP_Input, PHIDP_PREPARSED_DATA,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER,
    RID_INPUT, RIDEV_INPUTSINK, RIDEV_REMOVE, RIDI_PREPARSEDDATA, RIM_TYPEHID,
    RegisterRawInputDevices,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, HWND_MESSAGE, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetWindowLongPtrW, WM_CLOSE, WM_DESTROY, WM_INPUT, WNDCLASSW,
};
use windows_core::PCWSTR;

const USAGE_PAGE_GENERIC: u16 = 0x01;
const USAGE_X: u16 = 0x30;
const USAGE_Y: u16 = 0x31;
const USAGE_PAGE_DIGITIZER: u16 = 0x0D;
const USAGE_TOUCHPAD: u16 = 0x05;
const USAGE_TIP_SWITCH: u16 = 0x42;
const USAGE_CONTACT_ID: u16 = 0x51;

struct ContactSlot {
    link_collection: u16,
    x_min: i32,
    x_max: i32,
    y_min: i32,
    y_max: i32,
}

struct ParsedDevice {
    preparsed: Vec<u8>,
    slots: Vec<ContactSlot>,
}

type FrameCallback = Arc<dyn Fn(&[TouchContact]) + Send + Sync>;

struct ReaderState {
    on_frame: FrameCallback,
    devices: HashMap<isize, ParsedDevice>,
    raw_buf: Vec<u8>,
    frame: Vec<TouchContact>,
}

pub struct RawTouchpadReader {
    hwnd_raw: Arc<AtomicIsize>,
    handle: Option<JoinHandle<()>>,
}

impl RawTouchpadReader {
    pub fn start(on_frame: impl Fn(&[TouchContact]) + Send + Sync + 'static) -> Self {
        let hwnd_raw = Arc::new(AtomicIsize::new(0));
        let hwnd_bg = Arc::clone(&hwnd_raw);
        let cb: FrameCallback = Arc::new(on_frame);

        let handle = thread::Builder::new()
            .name("micontrol-raw-touchpad".into())
            .spawn(move || run_reader_loop(hwnd_bg, cb))
            .ok();

        Self { hwnd_raw, handle }
    }
}

impl Drop for RawTouchpadReader {
    fn drop(&mut self) {
        let raw = self.hwnd_raw.load(Ordering::Acquire);
        if raw != 0 {
            // SAFETY: Posting WM_CLOSE to our own message-only window to exit its GetMessageW loop.
            unsafe {
                let _ = PostMessageW(Some(HWND(raw as *mut c_void)), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run_reader_loop(hwnd_slot: Arc<AtomicIsize>, on_frame: FrameCallback) {
    // SAFETY: Dedicated Win32 message-only window registering HID Precision Touchpad Raw Input.
    unsafe {
        let hinstance = match GetModuleHandleW(PCWSTR::null()) {
            Ok(h) => h,
            Err(_) => return,
        };
        let class_name = windows_core::w!("MiControlRawTouchpadMsg");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(raw_wnd_proc),
            hInstance: hinstance.into(),
            lpszClassName: class_name,
            ..Default::default()
        };
        let _ = RegisterClassW(&wc);

        let Ok(hwnd) = CreateWindowExW(
            Default::default(),
            class_name,
            PCWSTR::null(),
            Default::default(),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance.into()),
            None,
        ) else {
            return;
        };

        let state = Box::new(ReaderState {
            on_frame,
            devices: HashMap::new(),
            raw_buf: vec![0u8; 4096],
            frame: Vec::with_capacity(8),
        });
        let state_ptr = Box::into_raw(state);
        SetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA, state_ptr as isize);

        let rid = [RAWINPUTDEVICE {
            usUsagePage: USAGE_PAGE_DIGITIZER,
            usUsage: USAGE_TOUCHPAD,
            dwFlags: RIDEV_INPUTSINK,
            hwndTarget: hwnd,
        }];
        if RegisterRawInputDevices(&rid, std::mem::size_of::<RAWINPUTDEVICE>() as u32).is_err() {
            SetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA, 0);
            drop(Box::from_raw(state_ptr));
            let _ = DestroyWindow(hwnd);
            return;
        }

        hwnd_slot.store(hwnd.0 as isize, Ordering::Release);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            DispatchMessageW(&msg);
        }

        let remove = [RAWINPUTDEVICE {
            usUsagePage: USAGE_PAGE_DIGITIZER,
            usUsage: USAGE_TOUCHPAD,
            dwFlags: RIDEV_REMOVE,
            hwndTarget: HWND::default(),
        }];
        let _ = RegisterRawInputDevices(&remove, std::mem::size_of::<RAWINPUTDEVICE>() as u32);

        hwnd_slot.store(0, Ordering::Release);
        SetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA, 0);
        drop(Box::from_raw(state_ptr));
    }
}

unsafe extern "system" fn raw_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            // SAFETY: GWLP_USERDATA holds a valid ReaderState pointer for the lifetime of this window.
            let ptr = unsafe {
                GetWindowLongPtrW(hwnd, windows::Win32::UI::WindowsAndMessaging::GWLP_USERDATA)
            } as *mut ReaderState;
            if !ptr.is_null() {
                // SAFETY: Exclusive access on the single message-loop thread.
                unsafe { handle_raw_input(&mut *ptr, HRAWINPUT(lparam.0 as *mut c_void)) };
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            // SAFETY: Destroys our own message-only window.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: Ends the thread's GetMessageW loop.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe fn handle_raw_input(state: &mut ReaderState, hraw: HRAWINPUT) {
    let hdr_size = std::mem::size_of::<RAWINPUTHEADER>() as u32;
    let mut size = 0u32;
    // SAFETY: Query required raw input byte size.
    if unsafe { GetRawInputData(hraw, RID_INPUT, None, &mut size, hdr_size) } != 0 || size == 0 {
        return;
    }
    if state.raw_buf.len() < size as usize {
        state.raw_buf.resize(size as usize, 0);
    }
    // SAFETY: Buffer is at least `size` bytes.
    let read = unsafe {
        GetRawInputData(
            hraw,
            RID_INPUT,
            Some(state.raw_buf.as_mut_ptr() as *mut c_void),
            &mut size,
            hdr_size,
        )
    };
    if read == u32::MAX || (read as usize) < std::mem::size_of::<RAWINPUTHEADER>() {
        return;
    }

    // SAFETY: RAWINPUTHEADER sits at byte offset 0 of the returned buffer.
    let raw = unsafe { &*(state.raw_buf.as_ptr() as *const RAWINPUT) };
    if raw.header.dwType != RIM_TYPEHID.0 {
        return;
    }

    let hdevice = raw.header.hDevice;
    let dev_key = hdevice.0 as isize;
    if let std::collections::hash_map::Entry::Vacant(e) = state.devices.entry(dev_key) {
        if let Some(parsed) = unsafe { inspect_hid_device(hdevice) } {
            e.insert(parsed);
        } else {
            return;
        }
    }

    let Some(dev) = state.devices.get(&dev_key) else {
        return;
    };

    // SAFETY: RAWHID begins immediately after RAWINPUTHEADER.
    let hid = unsafe { &raw.data.hid };
    let count = hid.dwCount as usize;
    let elem_size = hid.dwSizeHid as usize;
    if count == 0 || elem_size == 0 {
        return;
    }
    let data_offset = std::mem::offset_of!(RAWINPUT, data) + std::mem::offset_of!(windows::Win32::UI::Input::RAWHID, bRawData);
    let total_needed = data_offset.saturating_add(count.saturating_mul(elem_size));
    if total_needed > read as usize {
        return;
    }

    let preparsed = PHIDP_PREPARSED_DATA(dev.preparsed.as_ptr() as isize);

    for r in 0..count {
        let start = data_offset + r * elem_size;
        let mut report = state.raw_buf[start..start + elem_size].to_vec();
        state.frame.clear();

        for slot in &dev.slots {
            let mut usages = [0u16; 8];
            let mut usage_len = usages.len() as u32;
            // SAFETY: Valid preparsed HID descriptor and report slice.
            let st = unsafe {
                HidP_GetUsages(
                    HidP_Input,
                    USAGE_PAGE_DIGITIZER,
                    Some(slot.link_collection),
                    usages.as_mut_ptr(),
                    &mut usage_len,
                    preparsed,
                    &mut report,
                )
            };
            if st != HIDP_STATUS_SUCCESS {
                continue;
            }
            let tip_down = usages[..usage_len as usize].contains(&USAGE_TIP_SWITCH);
            if !tip_down {
                continue;
            }

            let mut cid = 0u32;
            let mut x_raw = 0u32;
            let mut y_raw = 0u32;
            // SAFETY: Valid preparsed HID descriptor and report slice.
            let ok_id = unsafe {
                HidP_GetUsageValue(
                    HidP_Input,
                    USAGE_PAGE_DIGITIZER,
                    Some(slot.link_collection),
                    USAGE_CONTACT_ID,
                    &mut cid,
                    preparsed,
                    &report,
                ) == HIDP_STATUS_SUCCESS
            };
            let ok_x = unsafe {
                HidP_GetUsageValue(
                    HidP_Input,
                    USAGE_PAGE_GENERIC,
                    Some(slot.link_collection),
                    USAGE_X,
                    &mut x_raw,
                    preparsed,
                    &report,
                ) == HIDP_STATUS_SUCCESS
            };
            let ok_y = unsafe {
                HidP_GetUsageValue(
                    HidP_Input,
                    USAGE_PAGE_GENERIC,
                    Some(slot.link_collection),
                    USAGE_Y,
                    &mut y_raw,
                    preparsed,
                    &report,
                ) == HIDP_STATUS_SUCCESS
            };

            if ok_id && ok_x && ok_y && slot.x_max > slot.x_min && slot.y_max > slot.y_min {
                let x = (x_raw as i32 - slot.x_min) as f32 / (slot.x_max - slot.x_min) as f32;
                let y = (y_raw as i32 - slot.y_min) as f32 / (slot.y_max - slot.y_min) as f32;
                state.frame.push(TouchContact { id: cid, x, y });
            }
        }

        (state.on_frame)(&state.frame);
    }
}

unsafe fn inspect_hid_device(hdevice: HANDLE) -> Option<ParsedDevice> {
    let mut size = 0u32;
    // SAFETY: Query required preparsed HID descriptor size.
    if unsafe { GetRawInputDeviceInfoW(Some(hdevice), RIDI_PREPARSEDDATA, None, &mut size) } != 0
        || size == 0
    {
        return None;
    }
    let mut preparsed = vec![0u8; size as usize];
    // SAFETY: Fill preparsed buffer of exact queried size.
    let rc = unsafe {
        GetRawInputDeviceInfoW(
            Some(hdevice),
            RIDI_PREPARSEDDATA,
            Some(preparsed.as_mut_ptr() as *mut c_void),
            &mut size,
        )
    };
    if rc == u32::MAX {
        return None;
    }

    let pp = PHIDP_PREPARSED_DATA(preparsed.as_ptr() as isize);
    let mut caps = HIDP_CAPS::default();
    // SAFETY: Valid preparsed descriptor pointer.
    if unsafe { HidP_GetCaps(pp, &mut caps) } != HIDP_STATUS_SUCCESS {
        return None;
    }
    if caps.NumberInputValueCaps == 0 {
        return None;
    }

    let mut vc_len = caps.NumberInputValueCaps;
    let mut vcaps = vec![HIDP_VALUE_CAPS::default(); vc_len as usize];
    // SAFETY: Valid vcaps slice sized to NumberInputValueCaps.
    if unsafe { HidP_GetValueCaps(HidP_Input, vcaps.as_mut_ptr(), &mut vc_len, pp) }
        != HIDP_STATUS_SUCCESS
    {
        return None;
    }

    // Group X and Y logical ranges by LinkCollection (each finger slot is a link collection).
    let mut x_ranges: HashMap<u16, (i32, i32)> = HashMap::new();
    let mut y_ranges: HashMap<u16, (i32, i32)> = HashMap::new();

    for cap in &vcaps[..vc_len as usize] {
        if cap.UsagePage != USAGE_PAGE_GENERIC || cap.IsRange {
            continue;
        }
        // SAFETY: `IsRange == false` selects `Anonymous.NotRange`.
        let usage = unsafe { cap.Anonymous.NotRange.Usage };
        if usage == USAGE_X {
            x_ranges.insert(cap.LinkCollection, (cap.LogicalMin, cap.LogicalMax));
        } else if usage == USAGE_Y {
            y_ranges.insert(cap.LinkCollection, (cap.LogicalMin, cap.LogicalMax));
        }
    }

    let mut slots: Vec<ContactSlot> = x_ranges
        .into_iter()
        .filter_map(|(lc, (xmin, xmax))| {
            let &(ymin, ymax) = y_ranges.get(&lc)?;
            Some(ContactSlot {
                link_collection: lc,
                x_min: xmin,
                x_max: xmax,
                y_min: ymin,
                y_max: ymax,
            })
        })
        .collect();
    slots.sort_by_key(|s| s.link_collection);

    if slots.is_empty() {
        return None;
    }

    Some(ParsedDevice { preparsed, slots })
}
