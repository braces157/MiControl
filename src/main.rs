#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use clap::{Parser, Subcommand};
use micontrol::config::AppConfig;
use micontrol::hardware::touchpad::TouchpadManager;
use micontrol::hardware::wmi_mifs::MifsClient;
use micontrol::hardware::{autostart, awake, display};
use micontrol::protocol::{ChargeLimit, PerfMode};
use std::process::ExitCode;
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Threading::CreateMutexW;

#[derive(Parser)]
#[command(
    name = "micontrol",
    version,
    about = "Ultra-lightweight native Rust controller for Xiaomi/Redmi laptops"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Print live firmware mode, charge limit, PD wattage, battery SOH, Owl & Touchpad status
    Status,
    /// Switch performance mode (eco | quiet | balance | auto | turbo | full)
    Mode { target: String },
    /// Set battery charge limit (40 | 50 | 60 | 70 | 80 | 100)
    Charge { percent: u8 },
    /// Control Keep-Awake / Owl mode (on | off)
    Awake { state: String },
    /// Set or cycle built-in panel refresh rate (e.g. 60 | 120 | cycle | list)
    Hz { target: String },
    /// Toggle AC/Battery Smart Profiles (on | off | status)
    Profile { state: String },
    /// Control Precision Touchpad hardware (on | off | toggle)
    Touchpad { state: String },
    /// Configure elevated Task Scheduler autostart (on | off | status)
    Autostart { state: String },
}

struct SingleInstanceGuard(HANDLE);

impl SingleInstanceGuard {
    fn try_acquire() -> Option<Self> {
        // SAFETY: Creates a named per-session mutex; ERROR_ALREADY_EXISTS signals existing instance.
        unsafe {
            let handle = CreateMutexW(None, false, windows_core::w!("Local\\MiControl_SingleInstance_v1")).ok()?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(Self(handle))
        }
    }
}

impl Drop for SingleInstanceGuard {
    fn drop(&mut self) {
        // SAFETY: Closes owned mutex handle.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

fn attach_parent_console_if_cli() {
    if std::env::args_os().len() > 1 {
        // SAFETY: Attaches stdout/stderr to parent terminal when invoked with CLI args on GUI subsystem.
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
}

fn run_cli(cmd: Commands) -> Result<(), String> {
    match cmd {
        Commands::Status => {
            let cfg = AppConfig::load();
            let on_ac = micontrol::hardware::is_on_ac_power();
            let bat = micontrol::hardware::battery_percent();
            let tp = TouchpadManager::new(None);
            println!("Power       : {} (Battery: {})",
                if on_ac { "AC" } else { "Battery" },
                bat.map(|b| format!("{b}%")).unwrap_or_else(|| "N/A".into())
            );
            match MifsClient::connect() {
                Ok(mifs) => {
                    println!("WMI Dialect : {:?}", mifs.dialect());
                    println!(
                        "Perf Mode   : {}",
                        mifs.get_perf_mode()
                            .ok()
                            .flatten()
                            .map(|m| m.to_string())
                            .unwrap_or_else(|| "Unknown".into())
                    );
                    println!(
                        "Charge Cap  : {} (Config: {}, Travel: {})",
                        mifs.get_charge_limit()
                            .ok()
                            .flatten()
                            .map(|c| c.to_string())
                            .unwrap_or_else(|| "Unsupported".into()),
                        if cfg.charge_care { cfg.charge_limit.to_string() } else { "Off".into() },
                        if cfg.travel_mode { "On" } else { "Off" }
                    );
                    println!(
                        "PD Adapter  : {}",
                        mifs.get_adapter_watts()
                            .ok()
                            .flatten()
                            .filter(|&w| w > 0)
                            .map(|w| format!("{w}W"))
                            .unwrap_or_else(|| "None / Non-PD".into())
                    );
                    println!(
                        "Battery SOH : {}",
                        mifs.get_battery_health()
                            .ok()
                            .flatten()
                            .map(|v| format!("{v}%"))
                            .unwrap_or_else(|| "N/A".into())
                    );
                }
                Err(err) => {
                    println!("WMI MIFS    : Unavailable ({err})");
                }
            }
            println!(
                "Display Hz  : {} (Supported: {:?})",
                display::current_internal_hz()
                    .map(|hz| format!("{hz} Hz"))
                    .unwrap_or_else(|| "N/A".into()),
                display::list_internal_refresh_rates()
            );
            println!(
                "Smart Prof  : {} (AC: {} {}Hz, Bat: {} {}Hz, BrightMem: {})",
                if cfg.smart_profiles_enabled { "On" } else { "Off" },
                cfg.ac_perf_mode,
                cfg.ac_refresh_hz,
                cfg.battery_perf_mode,
                cfg.battery_refresh_hz,
                if cfg.profile_remember_brightness { "On" } else { "Off" }
            );
            println!(
                "Keep Awake  : {} (Safety cutoff: ≤{}%)",
                if cfg.awake_enabled { "On" } else { "Off" },
                cfg.awake_min_battery_pct
            );
            println!(
                "Touchpad    : {} (Dead-Zone: {}, Edge Sliders: {})",
                match tp.is_enabled() {
                    Some(true) => "Enabled",
                    Some(false) => "Disabled",
                    None => "Not detected",
                },
                if cfg.touchpad_dead_zone { "On" } else { "Off" },
                if cfg.touchpad_edge_sliders { "On" } else { "Off" }
            );
            println!("Autostart   : {}", if autostart::is_enabled() { "On" } else { "Off" });
            Ok(())
        }
        Commands::Mode { target } => {
            let mode = PerfMode::parse_cli(&target).ok_or_else(|| {
                format!("Unknown mode '{target}'. Use: eco, quiet, balance, auto, turbo, full")
            })?;
            let mifs = MifsClient::connect()?;
            if mifs.set_perf_mode(mode)? {
                let mut cfg = AppConfig::load();
                cfg.remember_perf_mode(mode, micontrol::hardware::is_on_ac_power());
                let _ = cfg.save();
                println!("Performance mode set to {mode}");
                Ok(())
            } else {
                Err(format!("Firmware rejected performance mode {mode}"))
            }
        }
        Commands::Charge { percent } => {
            let limit = ChargeLimit::from_percent(percent)
                .ok_or_else(|| format!("Invalid percent {percent}. Firmware supports: 40, 50, 60, 70, 80, 100"))?;
            let mifs = MifsClient::connect()?;
            if mifs.set_charge_limit(limit, false)? {
                let mut cfg = AppConfig::load();
                cfg.travel_mode = false;
                if limit.is_care_active() {
                    cfg.charge_care = true;
                    cfg.charge_limit = limit;
                } else {
                    cfg.charge_care = false;
                }
                let _ = cfg.save();
                println!("Battery charge limit set to {limit}");
                Ok(())
            } else {
                Err(format!("Firmware rejected charge limit {limit}"))
            }
        }
        Commands::Awake { state } => {
            let mut cfg = AppConfig::load();
            match state.to_ascii_lowercase().as_str() {
                "on" | "1" | "true" => {
                    if awake::enable(&mut cfg) {
                        let _ = cfg.save();
                        println!("Keep-Awake (Owl) enabled");
                        Ok(())
                    } else {
                        Err("Failed to enable Keep-Awake".into())
                    }
                }
                "off" | "0" | "false" => {
                    awake::disable(&mut cfg);
                    let _ = cfg.save();
                    println!("Keep-Awake (Owl) disabled and lid action restored");
                    Ok(())
                }
                _ => Err("Use 'on' or 'off'".into()),
            }
        }
        Commands::Hz { target } => match target.trim().to_ascii_lowercase().as_str() {
            "list" => {
                let rates = display::list_internal_refresh_rates();
                println!("Internal panel refresh rates: {rates:?}");
                Ok(())
            }
            "cycle" => {
                let hz = display::cycle_internal_hz()?;
                let mut cfg = AppConfig::load();
                cfg.remember_refresh_hz(hz, micontrol::hardware::is_on_ac_power());
                let _ = cfg.save();
                println!("Internal display set to {hz} Hz");
                Ok(())
            }
            other => {
                let req = other
                    .trim_end_matches("hz")
                    .parse::<u32>()
                    .map_err(|_| format!("Invalid Hz '{target}'. Use e.g. 60, 120, cycle, or list"))?;
                let applied = display::set_internal_hz(req)?;
                let mut cfg = AppConfig::load();
                cfg.remember_refresh_hz(applied, micontrol::hardware::is_on_ac_power());
                let _ = cfg.save();
                println!("Internal display set to {applied} Hz");
                Ok(())
            }
        },
        Commands::Profile { state } => {
            let mut cfg = AppConfig::load();
            match state.trim().to_ascii_lowercase().as_str() {
                "on" | "enable" => {
                    cfg.smart_profiles_enabled = true;
                    let _ = cfg.save();
                    println!("Smart AC/Battery Profiles enabled");
                    Ok(())
                }
                "off" | "disable" => {
                    cfg.smart_profiles_enabled = false;
                    let _ = cfg.save();
                    println!("Smart AC/Battery Profiles disabled");
                    Ok(())
                }
                "status" => {
                    println!(
                        "Smart Profiles: {} (AC: {} {}Hz / Bat: {} {}Hz)",
                        if cfg.smart_profiles_enabled { "On" } else { "Off" },
                        cfg.ac_perf_mode,
                        cfg.ac_refresh_hz,
                        cfg.battery_perf_mode,
                        cfg.battery_refresh_hz
                    );
                    Ok(())
                }
                _ => Err("Use 'on', 'off', or 'status'".into()),
            }
        }
        Commands::Touchpad { state } => {
            let tp = TouchpadManager::new(None);
            if !tp.is_available() {
                return Err("Precision Touchpad not found".into());
            }
            match state.to_ascii_lowercase().as_str() {
                "on" | "enable" => {
                    if tp.set_enabled(true) {
                        println!("Touchpad enabled");
                        Ok(())
                    } else {
                        Err("Failed to enable touchpad (Admin required)".into())
                    }
                }
                "off" | "disable" => {
                    if tp.set_enabled(false) {
                        println!("Touchpad disabled");
                        Ok(())
                    } else {
                        Err("Failed to disable touchpad (Admin required)".into())
                    }
                }
                "toggle" => match tp.toggle() {
                    Some(on) => {
                        println!("Touchpad {}", if on { "enabled" } else { "disabled" });
                        Ok(())
                    }
                    None => Err("Failed to toggle touchpad (Admin required)".into()),
                },
                _ => Err("Use 'on', 'off', or 'toggle'".into()),
            }
        }
        Commands::Autostart { state } => match state.to_ascii_lowercase().as_str() {
            "on" | "enable" => {
                autostart::set_enabled(true)?;
                println!("Autostart enabled via Task Scheduler");
                Ok(())
            }
            "off" | "disable" => {
                autostart::set_enabled(false)?;
                println!("Autostart disabled");
                Ok(())
            }
            "status" => {
                println!("Autostart: {}", if autostart::is_enabled() { "On" } else { "Off" });
                Ok(())
            }
            _ => Err("Use 'on', 'off', or 'status'".into()),
        },
    }
}

fn install_crash_log_hook() {
    std::panic::set_hook(Box::new(|info| {
        let path = AppConfig::default_path()
            .parent()
            .map(|p| p.join("crash.log"))
            .unwrap_or_else(|| std::path::PathBuf::from("micontrol_crash.log"));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let msg = format!("MiControl panic: {info}\n");
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, msg.as_bytes()));
    }));
}

fn main() -> ExitCode {
    install_crash_log_hook();
    attach_parent_console_if_cli();
    let cli = Cli::parse();

    if let Some(cmd) = cli.command {
        return match run_cli(cmd) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("Error: {err}");
                ExitCode::FAILURE
            }
        };
    }

    // Tray GUI mode: enforce single instance (wake existing flyout if already running).
    let Some(_guard) = SingleInstanceGuard::try_acquire() else {
        micontrol::ui::signal_existing_instance();
        return ExitCode::SUCCESS;
    };

    match micontrol::ui::run_tray_app() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("MiControl failed to start: {err}");
            ExitCode::FAILURE
        }
    }
}
