# Hardware Monitor

CPU and GPU temperature as numbers in the taskbar; a click on an icon opens a window with almost all hardware
values in tabs – plus power consumption with costs, fan curves, drive health (like CrystalDiskInfo) and a privacy
mode. A program of the [Windows Toolbox](https://github.com/Windows-Toolbox/Toolbox).

> **Downloaded from the releases?** The `.exe` is not signed (signing costs money). Windows SmartScreen may warn
> once at the first start: *More info → Run anyway*.

## Rules for Claude (read first)

These rules hold on every PC and in every session; they are here because `CLAUDE.md` and Claude's memory stay local.

- The monitor itself is still **Python** (`python\`). The plan is a full port to Rust (Toolbox `DESIGN.md` 10,
  M4: a small C# bridge for LibreHardwareMonitor, everything else Rust). Until then this repo only connects it to
  the new Toolbox – new features go into the Python part as before.
- New texts on the page: `assets\i18n.json`, German and English. The monitor's own texts (`python\i18n.json`) are
  English only for now.
- Keep the checklist below up to date.

## Using it

1. Add the folder in the Toolbox (**Add program…**).
2. Press **Set up** on its page. A window opens and does what is missing: our own Python in `runtime\` (Python
   3.14 is installed with winget if it is not on the PC), the packages, LibreHardwareMonitor in `lhm\`, the driver
   PawnIO and the task "HardwareMonitor" in Task Scheduler. Windows asks for admin rights once.
3. **Start**. **Start with Windows** in the Toolbox starts it at sign-in.

The settings are on the page; the refresh rates and the privacy mode can also be switched in the right-click menu of
the icons. Which values the detail window shows is in `python\detail_window_content.py`.

What is yours lies in `dist\data\`: `hardware_monitor.json` (the monitor's settings, window position, fan curves),
`hardware_monitor_energy.json` (the power log), `monitor.log`, and `settings.json` / `log.txt` of the page. The
classic Toolbox's files (`Toolbox\data\hardware_monitor*.json`) are carried over at the first start, and if its
task started the monitor at sign-in, "Start with Windows" is switched on.

## How it fits together

```
 Toolbox ◄─pipe─► HardwareMonitor.exe ◄─TCP 127.0.0.1:47815─► runtime\HardwareMonitorSensors.exe python\hardware_monitor.py
 (normal)         (normal: page, Start/Stop,                  (admin, through the task: sensors via LibreHardwareMonitor,
                   status, settings file)                       tray icons, detail window)
```

- **Start**: `HardwareMonitor.exe` runs the task (`schtasks /run`), which starts the sensors with admin rights
  without asking. The task has no trigger of its own.
- **Status**: every 2 s "status" over TCP – temperatures for the Toolbox, the power line for the page.
- **Stop**: "quit"; the sensors hand every fan back to Auto when they end. "Quit" in their own right-click menu
  stops the program as well.
- **Settings**: the page writes its values into `data\hardware_monitor.json` and sends "reload"; what the monitor
  changes there itself comes back onto the page. The Toolbox's language goes the same way ("language").
- **Gear** in the detail window: opens the page in the Toolbox (`\\.\pipe\windows-toolbox.toolbox`).

## Building

`build.ps1` builds the release and installs it into `dist\` (needs Rust). In the repo the Python part stays in
`python\` – the task points there, so a changed `.py` counts at the next start without a build. `release.ps1`
puts exe, manifest, README and `python\` into the zip (not `runtime\`, `lhm\`, `data\`: "Set up" makes those).

## The files

| Path | What |
|---|---|
| `src/main.rs` | the page, Start/Stop, the status, the settings file both ways, "Set up", carrying over the classic Toolbox's files |
| `assets/` | `i18n.json` (the page's texts), `icon.ico` |
| `python/hardware_monitor.py` | the monitor: sensors, tray icons, remote control (port 47815) |
| `python/detail_window.py`, `detail_window_content.py` | the detail window and what it shows |
| `python/energy.py`, `fans.py`, `*_view.py`, `hardware_data.py` | power, fan control, their tabs, hardware data |
| `python/hardware_monitor_settings.py` | defaults and checks of the settings, port, texts |
| `python/storage.py`, `i18n.py`, `languages.json` | where the data lies, texts in the chosen language (once part of the classic Toolbox) |
| `python/setup.ps1`, `make_exe.py`, `requirements.txt` | "Set up": runtime, packages, the sensors' exe, LibreHardwareMonitor, PawnIO, task |
| `python/README.md` | the technical description of the Python part |

## Checklist

✓ checked, ◐ built, not checked by hand yet.

- ✓ Tests: page keys, texts, defaults equal to the Python part's, the settings file both ways, carrying over
- ✓ Set up steps 1–4 (runtime, packages, sensors' exe, LibreHardwareMonitor) on the developer PC; `--list` reads the sensors
- ✓ The page in a sandbox Toolbox
- ◐ Set up step 5 (PawnIO, task – admin prompt), replacing the classic task
- ◐ Start / Stop from the Toolbox, status line, "Start with Windows"
- ◐ Page changes reach the running monitor; right-click menu changes reach the page; language switch
- ◐ Show chart / fan control / delete power data from the page; gear in the detail window
- ◐ Carrying over the classic Toolbox's settings, power log and autostart
