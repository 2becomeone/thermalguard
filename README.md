# Thermal Guard

An ultra-lightweight native Rust Windows system tray monitor that watches both **CPU** and **GPU** temperatures and pops up Windows notifications before or when you hit thermal throttling.

## Alerts

- **Thermal Throttling (Critical)** — Fires immediately when hardware thermal throttling is active (NVIDIA NVML thermal slowdown flags or Intel `IA32_PACKAGE_THERM_STATUS` / `IA32_THERM_STATUS` live flags).
- **Overheating (Critical)** — Fires immediately when temperature meets or exceeds `critical_c` (or dynamically `throttle_limit_c - 3°C`, whichever is lower).
- **Heating Up Quickly (Warning)** — Smoothed rate-of-rise detection (`rapid_rise_delta_c` within `rapid_rise_window_secs`), gated by `rapid_rise_min_temp_c` so normal idle spikes (e.g. 35°C → 52°C opening a browser) don't spam you, and includes estimated seconds until critical temperature.
- **High Temperature (Warning)** — Fires after sustained high temperature (`warn_sustain_secs`) above `warn_c`, with hysteresis (`hysteresis_c`) and per-alert cooldowns to prevent notification flapping.
- **Cooled Down (Info)** — Optional silent notice once a hot component stays below `warn_c - hysteresis_c` for 5 seconds.

## Sensor Backends

- **GPU**:
  1. **NVIDIA NVML (`nvml.dll`)** — Works out of the box without Administrator privileges; reads live GPU temp, thermal margin/limit (`nvmlDeviceGetMarginTemperature` / thresholds), fan speed, and HW/SW thermal slowdown flags.
  2. **HWiNFO64 Shared Memory** fallback.
- **CPU** (auto-detected, re-probed every 10 seconds if unavailable):
  1. **PawnIO (`IntelMSR`)** — Direct Intel package & core MSR temperatures + live `PROCHOT#` / TCC throttle status (requires PawnIO installed and Administrator privileges, unless PawnIO DACL allows non-admin).
  2. **HWiNFO64 Shared Memory** (`Global\HWiNFO_SENS_SM2`).
  3. **Core Temp Shared Memory** (`CoreTempMappingObjectEx` / `CoreTempMappingObject`).
  4. **Windows PDH ACPI Thermal Zone** (`\Thermal Zone Information(*)\High Precision Temperature`), where supported by motherboard BIOS.

### CPU Temperature Setup on Desktop Motherboards

Many desktop motherboards do not expose Windows ACPI thermal zones (`MSAcpi_ThermalZoneTemperature`). To enable direct CPU temperature monitoring, choose either option:

1. **Standalone via PawnIO (Recommended)**:
   ```powershell
   winget install -e --id namazso.PawnIO
   ```
   Then run `thermal-guard.exe` as Administrator (or run [`scripts/setup-autostart-elevated.ps1`](scripts/setup-autostart-elevated.ps1) once from an elevated PowerShell prompt to auto-launch at login without UAC prompts).
2. **Via Existing Monitor (No Admin Needed for Thermal Guard)**:
   Run **HWiNFO64** (with *Shared Memory Support* enabled) or **Core Temp** in the background — Thermal Guard detects it automatically within 10 seconds.

## Build & Run

```powershell
cargo test
cargo build --release
```

### CLI Options

```powershell
.\target\release\thermal-guard.exe --status | Out-Host       # Inspect live sensors, backends, and thresholds
.\target\release\thermal-guard.exe --test-notify | Out-Host  # Trigger a test notification
.\target\release\thermal-guard.exe --config-path | Out-Host  # Print active config.toml location
.\target\release\thermal-guard.exe                           # Run quietly in the system tray
```

> **Note:** Because `thermal-guard.exe` uses the Windows GUI subsystem so tray startup never flashes a console window, pipe CLI inspection flags through `Out-Host` (or `Out-String`) in PowerShell to display their output.

## Configuration

On first launch, Thermal Guard creates `%APPDATA%\thermal-guard\config.toml` with defaults you can customize anytime and reload from the tray icon context menu (**Reload Config**):

- `notification_mode` — `"auto"` (default: tries Windows native tray balloon and automatically falls back to Thermal Guard's built-in non-focus-stealing floating toast popup if Windows Push Notifications / `WpnUserService` or Focus Assist is disabled), `"popup"` (always use the built-in floating toast popup), or `"windows"` (Windows tray balloons only).
- `popup_duration_secs` — Auto-dismiss timeout for built-in floating toast popups (default `7` seconds; critical alerts always stay at least `9` seconds or until clicked).

