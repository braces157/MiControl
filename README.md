<div align="center">

# MiControl

**Ultra-lightweight native Rust hardware controller for Xiaomi & Redmi laptops on Windows.**

[![CI](https://github.com/braces157/MiControl/actions/workflows/ci.yml/badge.svg)](https://github.com/braces157/MiControl/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/braces157/MiControl?display_name=tag&color=38BDF8)](https://github.com/braces157/MiControl/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-34D399.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%2010%20%2F%2011-0078D4.svg)](#installation)
[![Rust](https://img.shields.io/badge/Rust-2024%20Edition-FB923C.svg)](#building-from-source)

*A single ~624 KB executable replacing Xiaomi PC Manager — zero Electron, zero WebView2, zero kernel drivers, zero background services, and 0% idle CPU.*

</div>

---

## Why MiControl?

Official OEM laptop suites ship hundreds of megabytes of background services, telemetry, and browser-based UI layers just to toggle thermal profiles and battery charge caps. **MiControl** talks directly to your laptop's built-in ACPI/WMI firmware (`ROOT\WMI\MICommonInterface` and `HID_EVENT20`) through native Win32/COM APIs in Rust.

| | MiControl | Xiaomi PC Manager |
| :--- | :--- | :--- |
| **Package size** | Single ~624 KB standalone `.exe` | ~700 MB installer suite |
| **UI stack** | Pure Win32 + software 32bpp SDF DIB + ClearType GDI + DWM | Heavy multi-process runtime / web UI |
| **GPU driver DLLs loaded** | None (no Direct2D / D3D11 / OpenGL / wgpu) | Loads GPU stack & compositor threads |
| **Kernel drivers / services** | None — uses built-in Windows WMI, CCD, Core Audio, CfgMgr32 | Installs custom background services |
| **Idle CPU** | `0.0%` (100% event-driven Win32 loop, zero background polling) | Periodic background polling |
| **Scriptable CLI** | Built-in (`status`, `mode`, `charge`, `hz`, `awake`, `profile`, `touchpad`, `autostart`) | GUI-only |

---

## Features

### Performance & Thermal Modes (`MIFS 0x08`)
- Supports all six firmware profiles: **Eco**, **Quiet**, **Balance**, **Auto**, **Turbo**, and **Full Speed** (AC-only).
- Automatically detects the laptop's MIFS firmware dialect (**Classic** for modern Intel / `TM24*` laptops, **Echo** for older Ryzen models like `TM2113`).
- Learns firmware-rejected modes separately for AC and Battery (e.g., `Balance` on models where EC rejects it) and disables unsupported tiles automatically.

### Battery Charge Care, Travel Mode & `ChargeGuard` (`MIFS 0x10`)
- Supports every firmware-validated charge threshold: **`40%`**, **`50%`**, **`60%`**, **`70%`**, **`80%`**, and **`100%`** *(note: Xiaomi EC firmware does not implement `90%`)*.
- **One-off Travel Mode (100%)**: temporarily lift your cap to charge to 100% before a trip — automatically disarms back to your saved Charge Care limit the moment you unplug.
- **`ChargeGuard` persistence**: Xiaomi EC firmware forgets the WMI charge threshold across sleep, hibernation, and power transitions. `ChargeGuard` automatically re-arms your cap on startup, pre-suspend/shutdown, resume, and AC/battery changes without ever letting transient `100%` readbacks override your preference.
- **Live power telemetry**: reads USB-PD adapter wattage (`sub 0x06`, with weak-charger warnings) and Battery State of Health (`SOH`, `sub 0x01`).

### Smart AC / Battery Profiles & Internal Panel Refresh Rate
- Automatically switches **Performance Mode**, **Built-in Panel Refresh Rate (Hz)**, and **per-source Display Brightness** (`WmiMonitorBrightness`) when plugging into AC or switching to battery.
- Refresh rate control uses Windows CCD (`QueryDisplayConfig` + `ChangeDisplaySettingsExW`) and strictly targets the internal eDP/LVDS laptop panel — external monitors are never touched.
- Remembers your last chosen mode and refresh rate per power source and restores them on startup.

### Keep-Awake ("Owl Mode")
- Amphetamine-style sleep prevention with timed presets (**`30m`**, **`1h`**, **`2h`**, **`∞`**) via `SetThreadExecutionState`.
- Overrides AC **Lid-Close Action** to *"Do Nothing"* while active, persisting your original Windows power scheme GUID and lid setting to `config.json` *before* mutating Windows power settings so it restores cleanly even after a crash or power cut.
- **Backpack safety cutoff**: automatically disarms Keep-Awake on battery when charge drops to or below your threshold (default `≤ 20%`).

### Precision Touchpad Controls (`CfgMgr32` + Raw Input)
- **Hardware toggle**: enables or disables the Precision Touchpad device node directly via `CfgMgr32`.
- **Bottom palm dead-zone**: manages Windows `SuperCurtainBottom` (`1–40 mm`).
- **Optional Left/Right Edge Sliders**: slide vertically along the left touchpad edge to adjust **Display Brightness** and the right edge to adjust **Speaker Volume** (swappable, configurable strip width `4–30 mm`). Runs on a dedicated `HWND_MESSAGE` Raw Input thread that is only spawned when enabled — **0 threads and 0% CPU when off**.

### Native Windows 11 Fluent UI & Dynamic System Tray
- **Fluent Dark Quick Control Flyout**: PerMonitorV2 High-DPI (`100%–250%`), full keyboard navigation (`Tab`, `Shift+Tab`, `Arrows`, `Enter`, `Space`, `Esc`), and transient backbuffers freed immediately after `BitBlt`. Design tokens live in [`DESIGN.md`](DESIGN.md).
- **Dynamic System Tray Icon**: pure-Rust 4×4 supersampled SDF squircle tinted to your active performance mode, with an emerald indicator dot when Charge Care is active.
- **Frameless Click-Through OSD**: lightweight topmost toast on hotkey and state changes whose timer is destroyed immediately on dismiss.
- **Single-Instance & UAC-Free Autostart**: re-launching `micontrol.exe` wakes the existing flyout; enabling Autostart creates an elevated Task Scheduler logon entry (`MiControlAutoStart`, `/SC ONLOGON /RL HIGHEST`) so MiControl starts automatically at login without UAC prompts.

---

## Hardware Hotkeys & Mi-Button Gestures

MiControl listens asynchronously to Xiaomi's `ROOT\WMI\HID_EVENT20` push stream (`IWbemObjectSink`):

| Hardware Key / Gesture | Action |
| :--- | :--- |
| **Mi Button — Single Click** | Cycle Performance Mode (skips rejected & AC-only modes on battery) |
| **Mi Button — Double Click** | Toggle Battery Charge Care |
| **Mi Button — Hold (`400 ms`)** | Toggle Quick Control Flyout |
| **AI Key** | Toggle Quick Control Flyout (with repeat debounce & stuck-key recovery) |
| **Performance Key (`Fn+K`)** | Sync firmware performance mode & show OSD |
| **Mic Mute Key** | Sync Windows Core Audio capture mic mute (Console + Communications) & show OSD |
| **Refresh Rate Key** | Cycle internal panel refresh rate (smart `60 Hz ↔ max Hz` toggle) |
| **Settings Key** | Toggle Battery Charge Care |
| **Touchpad Toggle Key** | Toggle Precision Touchpad hardware |
| **Kbd Backlight / Fn-Lock / Caps / Num / Win Lock / Camera Privacy** | Show native Fluent OSD toast |
| **Projection / Screenshot / Task View Keys** | Trigger `DisplaySwitch.exe` (`Win+P`) / `Win+Shift+S` / `Win+Tab` |

---

## Installation

### Download Prebuilt Binary (Recommended)

1. Download `micontrol.exe` from the [latest GitHub Release](https://github.com/braces157/MiControl/releases/latest).
2. Place it in a permanent folder (for example, `C:\Tools\MiControl\micontrol.exe`).
3. Run `micontrol.exe` (accept the UAC prompt — Administrator privileges are required for Xiaomi WMI MIFS access, `CfgMgr32` touchpad toggling, and Task Scheduler autostart).
4. Optional: click **Autostart** in the flyout (or run `micontrol autostart on`) to start elevated on Windows logon without future UAC prompts.

> [!TIP]
> If you previously used Xiaomi PC Manager, stop or uninstall it so both tools do not fight over EC performance modes or charge limits.

### Building from Source

**Prerequisites:** Windows 10/11 (x64) and the [Rust toolchain](https://rustup.rs/) (MSVC target, Rust 1.85+ for Edition 2024).

```powershell
git clone https://github.com/braces157/MiControl.git
cd MiControl

# Run unit tests & lint
cargo test --all-targets
cargo clippy --all-targets -- -D warnings

# Build optimized standalone executable (~624 KB)
cargo build --release
```

The compiled executable is written to `target\release\micontrol.exe`.

> [!NOTE]
> Debug builds embed `app.debug.manifest` (`asInvoker`) so you can run tests and iterate without UAC prompts; release builds embed `app.manifest` (`requireAdministrator`).

---

## CLI Reference

Running `micontrol.exe` with no arguments starts the system tray app (or pops open the flyout if already running). Passing a subcommand attaches to the parent console automatically:

```powershell
# Print live firmware mode, charge limit, PD wattage, battery SOH, display Hz, Owl & touchpad status
micontrol status

# Switch performance mode (eco | quiet | balance | auto | turbo | full)
micontrol mode auto
micontrol mode turbo

# Set battery charge limit (40 | 50 | 60 | 70 | 80 | 100)
micontrol charge 80
micontrol charge 100

# Control Keep-Awake (Owl mode)
micontrol awake on
micontrol awake off

# Control internal laptop panel refresh rate
micontrol hz list
micontrol hz 120
micontrol hz cycle

# Toggle automatic AC / Battery Smart Profiles
micontrol profile status
micontrol profile on
micontrol profile off

# Hardware-toggle Precision Touchpad
micontrol touchpad toggle
micontrol touchpad on
micontrol touchpad off

# Manage elevated Task Scheduler autostart
micontrol autostart status
micontrol autostart on
micontrol autostart off
```

---

## Configuration

Settings are saved atomically to `%APPDATA%\MiControl\config.json` (with automatic range clamping on load) and unexpected panics are logged to `%APPDATA%\MiControl\crash.log`.

<details>
<summary><strong>Default <code>config.json</code> schema</strong></summary>

```json
{
  "charge_care": true,
  "charge_limit": "P80",
  "travel_mode": false,
  "rejected_modes_ac": [],
  "rejected_modes_battery": [],
  "last_perf_mode": null,
  "last_refresh_hz": null,
  "awake_enabled": false,
  "awake_allow_display_off": false,
  "awake_min_battery_pct": 20,
  "saved_lid_ac": null,
  "saved_lid_scheme": null,
  "smart_profiles_enabled": true,
  "ac_perf_mode": "Auto",
  "battery_perf_mode": "Quiet",
  "profile_auto_hz": true,
  "ac_refresh_hz": 120,
  "battery_refresh_hz": 60,
  "profile_remember_brightness": true,
  "ac_brightness": null,
  "battery_brightness": null,
  "touchpad_dead_zone": false,
  "touchpad_dead_zone_mm": 12,
  "touchpad_edge_sliders": false,
  "touchpad_edge_width_mm": 12,
  "touchpad_edge_swap": false,
  "osd_enabled": true,
  "osd_duration_ms": 1800,
  "mi_hold_ms": 400,
  "mi_double_click_ms": 300,
  "weak_charger_watts": 65
}
```

</details>

---

## Architecture

MiControl separates pure deterministic state machines (100% OS-free and unit-tested) from thin Win32/COM adapters:

```text
src/
├── main.rs                    # CLI (clap), single-instance mutex, crash hook, GUI bootstrap
├── config.rs                  # Atomic JSON persistence & input sanitization
├── protocol.rs                # Pure MIFS 32-byte codec, HID_EVENT20 decoder, Mi-button FSM
├── hardware/
│   ├── wmi_mifs.rs            # Native COM IWbemLocator/IWbemServices (MIFS, HID_EVENT20, brightness)
│   ├── charge_guard.rs        # Pure ChargeGuard re-arm & firmware reconciliation state machine
│   ├── smart_profile.rs       # Pure AC/Battery profile planner & timed Keep-Awake countdown FSM
│   ├── awake.rs               # SetThreadExecutionState & crash-safe Windows power scheme lid override
│   ├── display.rs             # Internal panel refresh rate via Windows CCD & ChangeDisplaySettingsExW
│   ├── audio.rs               # Windows Core Audio mic mute sync & speaker volume stepping
│   ├── autostart.rs           # Elevated Task Scheduler logon task (schtasks.exe)
│   └── touchpad/
│       ├── device.rs          # CfgMgr32 hardware toggle & PrecisionTouchPad SuperCurtain registry
│       ├── gesture.rs         # Pure left/right edge-slider gesture state machine
│       └── raw_input.rs       # On-demand HWND_MESSAGE Raw Input HID reader (0 threads when off)
└── ui/
    ├── mod.rs                 # Event-driven Win32 host window, power/suspend notifications
    ├── controller.rs          # Application controller & hardware orchestration
    ├── controller_events.rs   # HID_EVENT20, Mi-button, AI key & AC/Battery event routing
    ├── flyout_layout.rs       # Pure layout, hit-testing & keyboard focus engine (zero unsafe)
    ├── flyout.rs              # Native Win11 Fluent Quick Control Flyout window
    ├── osd.rs                 # Frameless click-through Fluent OSD toast
    ├── canvas.rs              # Zero-GPU software 32bpp SDF DIB + ClearType GDI + DWM chrome
    ├── logo.rs                # 4x4 supersampled SDF squircle brand mark renderer
    ├── tray.rs                # Shell_NotifyIconW dynamic ARGB HICON & native context menu
    └── theme.rs               # Fluent Dark palette & geometry tokens (see DESIGN.md)
```

---

## Hardware Compatibility

- **Verified hardware:** Redmi Book Pro 14 2025 (`TM2411`, `Classic` MIFS dialect).
- **Expected compatibility:** Xiaomi Book / Redmi Book laptops exposing `ROOT\WMI\MICommonInterface` (`Classic` modern Intel / `TM24*` series and `Echo` legacy Ryzen series such as `TM2113` for performance modes).
- **Non-Xiaomi / non-elevated fallback:** If `MICommonInterface` is unavailable, MiControl does not crash — the flyout displays an inline status banner while non-MIFS controls (display Hz, Keep-Awake, touchpad, tray/CLI) remain available.

---

## Disclaimer & License

MiControl is an independent, community-developed open-source project and is **not affiliated with, endorsed by, or associated with Xiaomi Corporation**. Firmware interaction is provided as-is.

Licensed under the [MIT License](LICENSE).
