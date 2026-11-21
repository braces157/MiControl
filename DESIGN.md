# MiControl UI Design Tokens (`DESIGN.md`)

Native Windows 11 Fluent Dark design system implemented directly with Win32 + zero-GPU software 32bpp SDF DIB + ClearType GDI + DWM (zero WebView, zero Chromium, zero Direct2D/D3D11/OpenGL/wgpu GPU driver DLLs loaded).

## 1. Palette Roles (Fluent Dark)

| Role | Hex | Usage |
| :--- | :--- | :--- |
| `canvas` | `#1C1C1E` | Flyout & OSD base surface (with Win11 `DWMSBT_TRANSIENTWINDOW` Acrylic fallback to solid dark) |
| `surface` | `#27272A` | Interactive tiles, mode pills, section containers |
| `surface_hover` | `#323236` | Pointer hover fill |
| `surface_active` | `#3A3A40` | Pressed fill |
| `border` | `#3F3F46` | 1px subtle hairline stroke on containers & tiles |
| `border_focus` | `#60CDFF` | 2px keyboard focus outline (`:focus-visible` equivalent) |
| `ink_primary` | `#F4F4F5` | Primary labels, active headings (`>= 14:1` on `canvas`) |
| `ink_secondary` | `#A1A1AA` | Subtitles, hardware metadata, inactive hints (`>= 5.8:1` on `canvas`) |
| `accent` | `#38BDF8` | Active performance mode / enabled toggle pill (sky-400; dark ink `#09090B` on fill `>= 9.5:1`) |
| `accent_soft` | `#1E3A4A` | Tinted badge background for secondary active pills |
| `status_eco` | `#34D399` | Eco mode & Charge Care healthy state |
| `status_quiet` | `#60A5FA` | Quiet mode |
| `status_auto` | `#38BDF8` | Auto / Smart mode |
| `status_turbo` | `#FB923C` | Turbo mode & Travel 100% override |
| `status_full` | `#F87171` | Full-Speed mode & error / warning badge |

## 2. Typography

- **Primary UI Font:** `Segoe UI Variable Text` (fallback `Segoe UI`) via DirectWrite ClearType
- **Numeric / Telemetry Font:** `Segoe UI Variable Display` with tabular figures (`13pt` SemiBold for title, `11pt` Medium for tiles, `9.5pt` Regular for captions)

## 3. Geometry, Spacing & Motion Budget

- **Flyout width:** `348 DIP` (logical pixels, scaled via `GetDpiForWindow` PerMonitorV2), corner radius `12 DIP` (`DWMWCP_ROUND`), outer padding `14 DIP`, section gap `10 DIP`, tile gap `6 DIP`.
- **OSD toast:** `220 x 56 DIP`, bottom-center or top-center above taskbar, `WS_EX_NOACTIVATE | WS_EX_TRANSPARENT | WS_EX_TOPMOST`, corner radius `10 DIP`.
- **Motion budget:** Instant state feedback on click; OSD shows for `1800 ms` with lightweight alpha fade-out (only while visible; timer destroyed on hide so idle CPU = `0.0%`).

## 4. Six Edge States

1. **Empty / Unavailable:** If MIFS WMI is unavailable (non-Xiaomi PC or non-admin), render clear inline status banner ("Firmware WMI unavailable — run as Administrator") instead of crashing or showing broken blanks.
2. **Loading / Probing:** Initial hardware probe executes asynchronously before paint; tiles show neutral unchecked state until firmware responds.
3. **Overflow & High-DPI Density:** All text uses DirectWrite trimming (`DWRITE_TRIMMING_GRANULARITY_CHARACTER`) and scales cleanly at 100%–250% DPI.
4. **Error & Rejection:** Firmware-rejected modes (e.g. `Balance` on TM24 or `FullSpeed` on battery) are disabled with a muted label and honest OSD feedback on rejection.
5. **Keyboard & Focus:** Full `Tab` / `Shift+Tab` / `Arrow` / `Enter` / `Space` / `Escape` navigation inside the flyout with visible `border_focus` rings.
6. **Contrast:** Every label meets WCAG AA (`>= 4.5:1` body, `>= 3:1` controls) across default, hover, active, and disabled states.
