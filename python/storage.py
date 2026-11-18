r"""
Where saved data lives: in data\ of the installed program – the folder that holds HardwareMonitor.exe (dist\ in the
repo, the unpacked folder of a release). The same folder holds runtime\ (our own Python) and lhm\
(LibreHardwareMonitor), both created by "Set up" (setup.ps1).

    data\settings.json               the page's settings (written by HardwareMonitor.exe; the Toolbox's language too)
    data\hardware_monitor.json       the monitor's settings (page values + window position, fan curves)
    data\hardware_monitor_energy.json  the power log
    data\monitor.log                  errors and notes of this Python part (pythonw has no console)

Reading and writing is done everywhere with load() and save().
"""

import copy
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
# Released: python\ lies in the program folder. In the repo: python\ lies next to dist\.
HOME = HERE.parent if (HERE.parent / "program.json").exists() else HERE.parent / "dist"
DATA_DIR = HOME / "data"


def settings_file(name):
    """data\\<name>.json – e.g. settings_file("hardware_monitor")."""
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    return DATA_DIR / f"{name}.json"


def log_file():
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    return DATA_DIR / "monitor.log"


def load(file, defaults):
    """Defaults, overwritten with what is in file. Unknown keys are kept; whether the values are valid is up to the
    caller to check. If the file is missing or broken, you simply get the defaults."""
    data = copy.deepcopy(defaults)
    try:
        saved = json.loads(file.read_text(encoding="utf-8-sig"))
        if isinstance(saved, dict):
            data.update(saved)
    except (OSError, ValueError):
        pass
    return data


def save(file, data):
    """Saves data as readable JSON – written aside and renamed, so HardwareMonitor.exe never reads half a file.
    Raises OSError if it doesn't work."""
    tmp = file.with_suffix(".json.tmp")
    tmp.write_text(json.dumps(data, indent=2, ensure_ascii=False), encoding="utf-8")
    tmp.replace(file)
