//! The bridge to LibreHardwareMonitor (Toolbox\DESIGN.md 10): `LhmBridge.exe` is the only part with admin rights.
//! "Set up" puts it with LHM into the protected folder and registers the task that starts it without a prompt
//! (`kit::setup`); this module starts it, connects and asks it things – one JSON object per line.

use std::sync::Arc;
use std::time::{Duration, Instant};

use kit::ipc::{self, Conn};
use kit::setup::{self, Step};
use serde_json::{Value, json};

/// The program's id – the protected folder and the task are named after it.
pub const ID: &str = "hardware_monitor";
/// The bridge's pipe (`Pipe` in `bridge\LhmBridge.cs`), as `kit::ipc` names it: `windows-toolbox.<this>`.
const PIPE: &str = "hardware_monitor.bridge";
pub const EXE: &str = "LhmBridge.exe";
/// From the task's start to an open pipe: .NET and the bridge itself (M0: 0.7–1 s).
const CONNECT_WAIT: Duration = Duration::from_secs(15);

/// The pinned LibreHardwareMonitor – `lhm.json`, which `build.ps1` reads too.
fn lhm() -> Value {
    serde_json::from_str(include_str!("../lhm.json")).unwrap_or_default()
}

/// What "Set up" does, in this order: the driver, LHM, the bridge, the task.
pub fn steps() -> Vec<Step> {
    let lhm = lhm();
    let s = |key: &str| lhm[key].as_str().unwrap_or_default().to_string();
    vec![
        Step::Winget { id: "namazso.PawnIO".into(), name: "PawnIO".into(), service: "PawnIO".into() },
        Step::Download { name: "LibreHardwareMonitor".into(), version: s("version"), url: s("url"), sha256: s("sha256"), into: "lhm".into() },
        Step::Copy { file: EXE.into(), into: String::new() },
        Step::Task { run: EXE.into() },
    ]
}

/// A connection to the running bridge. It ends when this is dropped.
pub struct Bridge {
    conn: Arc<Conn>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.conn.close();
    }
}

impl Bridge {
    /// Starts the bridge through the task and connects.
    pub fn start() -> Result<Bridge, String> {
        if !kit::process::run_task(&setup::task_name(ID)) {
            return Err(kit::t!("bridge.no_task"));
        }
        let conn = ipc::connect_wait(PIPE, CONNECT_WAIT).map_err(|_| kit::t!("bridge.no_answer"))?;
        Ok(Bridge { conn })
    }

    /// One command, its answer. An answer of type "error" is an `Err` with its message.
    pub fn ask(&self, command: Value) -> Result<Value, String> {
        self.conn.send_line(&command.to_string()).map_err(|e| e.to_string())?;
        let line = self.conn.read_line().ok_or_else(|| kit::t!("bridge.gone"))?;
        let answer: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
        match answer["type"].as_str() {
            Some("error") => Err(answer["message"].as_str().unwrap_or("?").to_string()),
            _ => Ok(answer),
        }
    }
}

/// The test on the page: start, open CPU and GPU, read once, close. A line about what came back.
pub fn test() -> Result<String, String> {
    let started = Instant::now();
    let bridge = Bridge::start()?;
    let opened = bridge.ask(json!({"cmd": "open", "groups": ["IsCpuEnabled", "IsGpuEnabled"]}))?;
    let values = bridge.ask(json!({"cmd": "read"}))?;
    let hardware = values["hardware"].as_array().cloned().unwrap_or_default();
    let count: usize = hardware.iter().map(|h| h["sensors"].as_array().map_or(0, Vec::len)).sum();
    let temp = |kind: &str, names: &[&str]| -> Option<f64> {
        names.iter().find_map(|name| {
            hardware.iter().filter(|h| h["type"].as_str().is_some_and(|t| t.starts_with(kind))).find_map(|h| {
                h["sensors"].as_array()?.iter().find(|s| s["type"] == "Temperature" && s["name"] == *name).and_then(|s| s["value"].as_f64())
            })
        })
    };
    let cpu = temp("Cpu", &["Core (Tctl/Tdie)", "CPU Package", "Core Average"]);
    let gpu = temp("Gpu", &["GPU Core"]);
    let shown = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{v:.0} °C"));
    Ok(kit::t!(
        "bridge.test.ok",
        seconds = format!("{:.1}", started.elapsed().as_secs_f64()),
        count = count,
        cpu = shown(cpu),
        gpu = shown(gpu),
        admin = if opened["elevated"].as_bool() == Some(true) { kit::t!("bridge.test.admin") } else { kit::t!("bridge.test.no_admin") },
        version = opened["version"].as_str().unwrap_or("?"),
    ))
}
