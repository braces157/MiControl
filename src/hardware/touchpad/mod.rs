//! Precision Touchpad controller: hardware enable/disable, bottom dead-zone (`SuperCurtainBottom`),
//! and optional left/right edge sliders (brightness & volume) via Raw Input.

pub mod device;
pub mod gesture;
pub mod raw_input;

use crate::config::AppConfig;
use crate::hardware::audio;
use crate::hardware::touchpad::device::TouchpadDevice;
use crate::hardware::touchpad::gesture::{EdgeGestureTracker, EdgeSide};
use crate::hardware::touchpad::raw_input::RawTouchpadReader;
use crate::hardware::wmi_mifs::MifsClient;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

/// Approximate width of modern Xiaomi/Redmi 14"/15" glass touchpads (mm) for strip fraction math.
const NOMINAL_PAD_WIDTH_MM: f32 = 139.0;
const STEP_FRACTION: f32 = 0.032;

/// Callback notification emitted after an edge slider adjusts brightness or volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliderFeedback {
    Brightness(u8),
    Volume(u8),
}

pub struct TouchpadManager {
    device: Option<TouchpadDevice>,
    reader: Option<RawTouchpadReader>,
    worker: Option<SliderWorker>,
    on_feedback: Option<Arc<dyn Fn(SliderFeedback) + Send + Sync>>,
}

struct SliderWorker {
    stop: Arc<AtomicBool>,
    signal: Arc<(Mutex<bool>, Condvar)>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for SliderWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        {
            let (lock, cvar) = &*self.signal;
            let mut dirty = lock.lock().unwrap_or_else(|e| e.into_inner());
            *dirty = true;
            cvar.notify_one();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

impl TouchpadManager {
    pub fn new(on_feedback: Option<Arc<dyn Fn(SliderFeedback) + Send + Sync>>) -> Self {
        Self {
            device: TouchpadDevice::discover(),
            reader: None,
            worker: None,
            on_feedback,
        }
    }

    pub fn is_available(&self) -> bool {
        self.device.is_some()
    }

    pub fn instance_id(&self) -> Option<&str> {
        self.device.as_ref().map(|d| d.instance_id())
    }

    pub fn is_enabled(&self) -> Option<bool> {
        self.device.as_ref()?.is_enabled()
    }

    pub fn set_enabled(&self, enable: bool) -> bool {
        self.device.as_ref().is_some_and(|d| d.set_enabled(enable))
    }

    pub fn toggle(&self) -> Option<bool> {
        let cur = self.is_enabled()?;
        let next = !cur;
        if self.set_enabled(next) {
            Some(next)
        } else {
            None
        }
    }

    /// Persist curtain zones (`SuperCurtainBottom` / `Left` / `Right`) to HKLM and restart
    /// the touchpad node so Windows Precision Touchpad picks them up immediately.
    pub fn apply_zones(&self, cfg: &AppConfig) -> bool {
        let bottom = cfg.touchpad_dead_zone.then_some(cfg.touchpad_dead_zone_mm);
        let edges = cfg.touchpad_edge_sliders.then_some(cfg.touchpad_edge_width_mm);
        if !device::apply_curtain_zones(bottom, edges) {
            return false;
        }
        if let Some(dev) = &self.device {
            let _ = dev.restart();
        }
        true
    }

    /// Re-apply saved curtain zones on startup only if HKLM registry drifted from `cfg`,
    /// avoiding any touchpad restart stutter when settings already match.
    pub fn sync_zones_if_drifted(&self, cfg: &AppConfig) {
        if !self.is_available() {
            return;
        }
        let bottom = cfg.touchpad_dead_zone.then_some(cfg.touchpad_dead_zone_mm);
        let edges = cfg.touchpad_edge_sliders.then_some(cfg.touchpad_edge_width_mm);
        if !device::curtain_zones_match(bottom, edges) {
            let _ = self.apply_zones(cfg);
        }
    }

    /// Start or stop the Raw Input edge slider reader + coalescing worker to match `cfg`.
    /// When disabled, zero background threads exist (0% CPU, 0 RAM).
    pub fn sync_edge_reader(&mut self, cfg: &AppConfig) {
        self.stop_edge_reader();
        if !cfg.touchpad_edge_sliders || !self.is_available() {
            return;
        }

        let pending_b = Arc::new(AtomicI32::new(0));
        let pending_v = Arc::new(AtomicI32::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let signal = Arc::new((Mutex::new(false), Condvar::new()));

        let worker_handle = {
            let pb = Arc::clone(&pending_b);
            let pv = Arc::clone(&pending_v);
            let st = Arc::clone(&stop);
            let sig = Arc::clone(&signal);
            let fb = self.on_feedback.clone();
            thread::Builder::new()
                .name("micontrol-slider-worker".into())
                .spawn(move || run_slider_worker(pb, pv, st, sig, fb))
                .ok()
        };

        let width_frac = (cfg.touchpad_edge_width_mm as f32) / NOMINAL_PAD_WIDTH_MM;
        let swap = cfg.touchpad_edge_swap;
        let tracker = Mutex::new(EdgeGestureTracker::new(width_frac, STEP_FRACTION));
        let pb = Arc::clone(&pending_b);
        let pv = Arc::clone(&pending_v);
        let sig = Arc::clone(&signal);

        let reader = RawTouchpadReader::start(move |contacts| {
            let delta = {
                let mut tr = tracker.lock().unwrap_or_else(|e| e.into_inner());
                tr.update(contacts)
            };
            if let Some(d) = delta {
                let is_brightness = match d.side {
                    EdgeSide::Left => !swap,
                    EdgeSide::Right => swap,
                };
                // 3% brightness / 2% volume per discrete gesture step feels crisp and smooth.
                if is_brightness {
                    pb.fetch_add(d.steps * 3, Ordering::Relaxed);
                } else {
                    pv.fetch_add(d.steps * 2, Ordering::Relaxed);
                }
                let (lock, cvar) = &*sig;
                let mut dirty = lock.lock().unwrap_or_else(|e| e.into_inner());
                *dirty = true;
                cvar.notify_one();
            }
        });

        self.worker = Some(SliderWorker {
            stop,
            signal,
            handle: worker_handle,
        });
        self.reader = Some(reader);
    }

    pub fn stop_edge_reader(&mut self) {
        self.reader = None;
        self.worker = None;
    }
}

fn run_slider_worker(
    pending_b: Arc<AtomicI32>,
    pending_v: Arc<AtomicI32>,
    stop: Arc<AtomicBool>,
    signal: Arc<(Mutex<bool>, Condvar)>,
    feedback: Option<Arc<dyn Fn(SliderFeedback) + Send + Sync>>,
) {
    let mut wmi_client: Option<MifsClient> = None;

    loop {
        {
            let (lock, cvar) = &*signal;
            let mut dirty = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*dirty && !stop.load(Ordering::Acquire) {
                dirty = cvar.wait(dirty).unwrap_or_else(|e| e.into_inner());
            }
            *dirty = false;
        }
        if stop.load(Ordering::Acquire) {
            break;
        }

        let db = pending_b.swap(0, Ordering::AcqRel);
        if db != 0 {
            if wmi_client.is_none() {
                wmi_client = MifsClient::connect().ok();
            }
            if let Some(ref client) = wmi_client
                && let Some(cur) = client.get_display_brightness()
            {
                let next = (cur as i32 + db).clamp(0, 100) as u8;
                if client.set_display_brightness(next)
                    && let Some(ref cb) = feedback
                {
                    cb(SliderFeedback::Brightness(next));
                }
            }
        }

        let dv = pending_v.swap(0, Ordering::AcqRel);
        if dv != 0
            && let Some(next) = audio::step_speaker_volume(dv)
            && let Some(ref cb) = feedback
        {
            cb(SliderFeedback::Volume(next));
        }
    }
}
