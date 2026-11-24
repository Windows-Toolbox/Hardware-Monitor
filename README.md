# Hardware Monitor

CPU and GPU temperature as numbers in the taskbar; a click on one opens a window with almost all hardware values
in tabs – plus power consumption with costs (estimated, or measured by a smart plug), fan curves, drive health (like
CrystalDiskInfo), the fixed hardware data and a privacy mode. A program of the
[Windows Toolbox](https://github.com/Windows-Toolbox/Toolbox).

> **Downloaded from the releases?** The `.exe` is not signed (signing costs money). Windows SmartScreen may warn
> once at the first start: *More info → Run anyway*.

## Rules for Claude (read first)

These rules hold on every PC and in every session; they are here because `CLAUDE.md` and Claude's memory stay local.

- Everything is Rust, except the bridge `bridge\LhmBridge.cs` (C#, because LibreHardwareMonitor is a .NET library).
  Keep the bridge small: it only hands over raw values – what they mean is worked out in Rust.
- A change to `LhmBridge.cs` makes "Set up" necessary again (the installed copy differs) – until then the monitor
  reads nothing. Say so before installing such a build.
- New texts: `assets\i18n.json`, German and English.
- The fan curve editor controls the real fans – never switch a fan to Curve or Fixed on the developer's PC without
  asking.
- Keep the checklist below up to date.

## Using it

1. Add the folder in the Toolbox (**Add program…**), or get it under **Get programs**.
2. Press **Set up** on its page. It installs the driver PawnIO (winget), downloads LibreHardwareMonitor (checked by
   its SHA-256), puts it and the bridge into `C:\Program Files\WindowsToolbox\hardware_monitor\` and registers the
   task `\WindowsToolbox\hardware_monitor`. Windows asks for admin rights once.
3. **Start**. **Start with Windows** in the Toolbox starts it at sign-in.

A click on a temperature in the taskbar (or "Open" in the Toolbox) opens the detail window. The settings are on the
page; the privacy mode can also be switched in the tray menu and in the window. Which values the detail window
shows is set in `detail_window.json` – its `help` at the top explains the format.

What is yours lies in `dist\data\`: `settings.json` (settings, fan curves, the window's place),
`hardware_monitor_energy.json` and `hardware_monitor_plug.json` (the power logs) and `log.txt`. The Toolbox's
*Export…* takes all of it along except the log.

## Removing it

Press **Uninstall** in the section "Setup" before you delete the folder: it removes the task and the protected
folder (Windows asks for admin rights once). The driver PawnIO stays – other programs (FanControl,
LibreHardwareMonitor) may use it; it can be removed in Windows Settings under Apps. The Toolbox's *Uninstall all*
removes the task and the folder too.

## How it fits together

```
 Toolbox ◄─pipe─► HardwareMonitor.exe ◄─pipe─► LhmBridge.exe  (admin, through the task: LibreHardwareMonitor)
                  (the monitor: page,
                   taskbar, fans, power)
                        ▲
                        │ pipe (link.rs)
                        ▼
                  HardwareMonitor.exe --window  (the detail window, a process of its own while it is open)
```

- **The monitor** (`monitor.rs`) runs on the program's worker: it starts the bridge through its task, reads CPU and
  GPU first, the rest in the background; the Nvidia temperature comes from `nvml.dll` directly. Efficiency mode while
  idle. It controls the fans, logs the power and reads the smart plug.
- **The detail window** (`window.rs`) says over its pipe what it shows; the monitor then reads that each round and
  sends it. Closing the window gives all its memory back.
- **The bridge** (`bridge\LhmBridge.cs`) is the only part with admin rights. When its connection ends, every fan it
  set goes back to Auto. `LhmBridge.exe --hardware-data` (no admin rights) prints the WMI data for the tab
  "Hardware data".
- **Gear** in the detail window: opens the page in the Toolbox.

## Building

`build.ps1` builds the release, compiles the bridge with the `csc.exe` that is part of Windows (only when its source
changed) and installs both into `dist\`. `release.ps1` puts exe, bridge, manifest, README and `detail_window.json`
into a signed zip and publishes it as a GitHub release (the version from `Cargo.toml`).

`cargo test` runs the unit tests; `cargo test real -- --ignored --nocapture` asks the real bridge and the plug
(sets no fan).

## The files

| Path | What |
|---|---|
| `src/main.rs` | the page, Start/Stop, the status, "Set up", the detail window's pipe |
| `src/monitor.rs` | the monitor's loop: reading, taskbar, power, plug, fans, what the window wants |
| `src/bridge.rs`, `bridge/LhmBridge.cs`, `lhm.json` | the bridge to LibreHardwareMonitor, its setup steps, the pinned LHM version |
| `src/sensors.rs`, `src/nvml.rs` | what the bridge reads and what is chosen from it; the Nvidia values straight from the driver |
| `src/fans.rs`, `src/energy.rs`, `src/plug.rs` | fan curves, the power estimate and logs, the smart plug (Modbus TCP, mDNS) |
| `src/window.rs`, `src/link.rs` | the detail window and what goes between it and the monitor |
| `src/content.rs`, `detail_window.json` | which tab shows which values |
| `src/power_view.rs`, `src/fan_view.rs`, `src/drive_view.rs`, `src/hardware_data.rs` | the tabs Power, Fans, Drive health, Hardware data |
| `assets/` | `i18n.json` (all texts), `icon.ico` |

## Checklist

✓ checked, ◐ built, not checked by hand yet.

- ✓ Tests: page keys, texts, settings of the Python version's time, the bars of the power chart, fan curves, the plug's log, drive
  health rules, hardware data from the bridge's rows, the window's content file
- ✓ Set up / Test / Uninstall (2026-11-24): one admin prompt, the bridge reads with admin rights, Clean up leaves it
  alone, Uninstall removes task and folder
- ✓ The monitor (2026-11-24): status line, numbers in the taskbar with a stripe, privacy mode in the tray menu,
  efficiency mode, the smart plug
- ✓ The detail window (2026-11-24): list tabs (select, copy, fold), privacy mode, size and place kept; Power (views,
  tiles, hover, Copy table), Fans (curve editor on real fans), Drive health, Hardware data (Overview / Details, Copy)
- ✓ Fan curves and the window's place taken over from the Python version's `hardware_monitor.json` (2026-11-24,
  on the developer PC; the old file went at the next start)
- ◐ A fresh PC from the release zip alone

## Later

- **Fans tab**: choosing the fans and filtering them with the CPU / GPU buttons feels odd (hand test 2026-11-18) –
  rethink with the user before changing it.
