//! Hardware Monitor – the Toolbox's side of it.
//!
//! The monitor itself is still Python (`python\`): it reads the sensors through LibreHardwareMonitor, draws the
//! temperature icons in the taskbar and opens the detail window. It needs admin rights, so it is started through
//! the scheduled task "HardwareMonitor" that "Set up" (`python\setup.ps1`) registers once.
//!
//! This exe is the kit program around it (Toolbox\DESIGN.md 5): its page in the Toolbox, Start and Stop, "Start
//! with Windows", the status line. While its worker runs it makes sure the sensors run, and asks them every two
//! seconds how things are (TCP on 127.0.0.1, the port the Python part has always had). Stop asks them to quit.
//!
//! The settings live twice: the page's values in `data\settings.json` (the kit's file) and everything of the monitor
//! in `data\hardware_monitor.json` (also window position and fan curves). A change on the page is written into the
//! monitor's file and followed by "reload"; a change the monitor makes itself (rates and privacy mode in its
//! right-click menu) is noticed by the file's time and taken over. The Toolbox's language goes the same way.
//!
//! A full port to Rust is planned (Toolbox\DESIGN.md 10, M4); until then this keeps the classic Toolbox unneeded.
#![windows_subsystem = "windows"]

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use kit::app::{self, Program, Runtime};
use kit::page::Builder;
use kit::proto::{Health, Status, Tray};
use kit::{i18n, process, t};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The Python part's remote control (`IPC_PORT` in `python\hardware_monitor_settings.py`).
const PORT: u16 = 47815;
/// The scheduled task that starts the sensors with admin rights (`python\setup.ps1`).
const TASK: &str = "HardwareMonitor";
/// The monitor's own file in `data\` (`SETTINGS_FILE` in `python\hardware_monitor_settings.py`).
const MONITOR_FILE: &str = "hardware_monitor.json";
const ENERGY_FILE: &str = "hardware_monitor_energy.json";
/// Between two questions to the running sensors.
const POLL: Duration = Duration::from_secs(2);
/// How long the sensors may take to answer after the task was started (LHM needs a few seconds).
const START_WAIT: Duration = Duration::from_secs(40);
/// A second click on "Delete" within this time deletes.
const CONFIRM: Duration = Duration::from_secs(8);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;

const ICON_RATES: [f64; 4] = [1.0, 2.0, 5.0, 10.0];
const POPUP_RATES: [f64; 4] = [0.5, 1.0, 2.0, 5.0];
/// Sensor names of LibreHardwareMonitor – they stay English.
const CPU_SENSORS: [&str; 6] = ["Core (Tctl/Tdie)", "CPU Package", "Core Max", "Core Average", "CCD1 (Tdie)", "CCD2 (Tdie)"];
const GPU_SENSORS: [&str; 3] = ["GPU Core", "GPU Hot Spot", "GPU Memory Junction"];

/// The page's settings – the same names and defaults as `DEFAULT_SETTINGS` in `python\hardware_monitor_settings.py`.
/// A new setting = one field here + one line in `page` (the Python part reads it from its file).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(default)]
pub struct Settings {
    pub icon_interval: f64,
    pub popup_interval: f64,
    /// "auto", "on", "off".
    pub efficiency_mode: String,
    /// Takes effect when the sensors start again.
    pub icon_order: Vec<String>,
    /// "auto" or a sensor name.
    pub cpu_sensor: String,
    pub gpu_sensor: String,
    pub cpu_warn_temp: u32,
    pub cpu_hot_temp: u32,
    pub gpu_warn_temp: u32,
    pub gpu_hot_temp: u32,
    pub privacy_mode: bool,
    pub energy_logging: bool,
    /// As typed ("0,35" works too); a number in the monitor's file.
    pub energy_price: String,
    pub energy_currency: String,
    pub energy_other_watts: u32,
    pub energy_psu_efficiency: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            icon_interval: 2.0,
            popup_interval: 1.0,
            efficiency_mode: "auto".into(),
            icon_order: vec!["cpu".into(), "gpu".into()],
            cpu_sensor: "auto".into(),
            gpu_sensor: "auto".into(),
            cpu_warn_temp: 75,
            cpu_hot_temp: 90,
            gpu_warn_temp: 70,
            gpu_hot_temp: 83,
            privacy_mode: false,
            energy_logging: false,
            energy_price: "0.35".into(),
            energy_currency: "€".into(),
            energy_other_watts: 40,
            energy_psu_efficiency: 87,
        }
    }
}

/// "0.35", "0,35", " 0.35 " → 0.35; a price from 0 to below 100, else `None`.
fn parse_price(text: &str) -> Option<f64> {
    text.trim().replace(',', ".").parse::<f64>().ok().filter(|p| (0.0..100.0).contains(p))
}

// ---------- the monitor's file ----------

fn monitor_file() -> PathBuf {
    app::data_dir().join(MONITOR_FILE)
}

fn read_json(file: &Path) -> Map<String, Value> {
    std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok()).unwrap_or_default()
}

/// Written aside and renamed, so the Python part never reads half a file.
fn write_json(file: &Path, map: &Map<String, Value>) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = file.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(map)?)?;
    std::fs::rename(&tmp, file)
}

fn modified(file: &Path) -> Option<SystemTime> {
    std::fs::metadata(file).and_then(|m| m.modified()).ok()
}

/// The page's values as the Python part keeps them (the price as a number).
fn to_monitor(s: &Settings) -> Map<String, Value> {
    let Ok(Value::Object(mut m)) = serde_json::to_value(s) else { return Map::new() };
    match parse_price(&s.energy_price) {
        Some(price) => m["energy_price"] = json!(price),
        // Not a price: the monitor keeps the one it has.
        None => {
            m.remove("energy_price");
        }
    }
    m.insert("energy_currency".into(), json!(s.energy_currency.trim().chars().take(5).collect::<String>()));
    m
}

/// Writes the page's values and the language into the monitor's file – only what differs. `true` if it wrote.
fn export(s: &Settings, language: &str, file: &Path) -> std::io::Result<bool> {
    let mut map = read_json(file);
    let mut changed = false;
    for (k, v) in to_monitor(s).into_iter().chain([("language".to_string(), json!(language))]) {
        // 2 and 2.0 are the same rate.
        let same = match (map.get(&k), &v) {
            (Some(Value::Number(a)), Value::Number(b)) => a.as_f64() == b.as_f64(),
            (old, new) => old == Some(new),
        };
        if !same {
            map.insert(k, v);
            changed = true;
        }
    }
    if changed {
        write_json(file, &map)?;
    }
    Ok(changed)
}

/// The page's values from the monitor's file, on top of `s` – what the monitor changed itself.
fn import(s: &Settings, file: &Path) -> Settings {
    let mut current = match serde_json::to_value(s) {
        Ok(Value::Object(m)) => m,
        _ => return s.clone(),
    };
    for (k, v) in read_json(file) {
        if !current.contains_key(&k) {
            continue;
        }
        let v = match (k.as_str(), &v) {
            // The typed text stays when it means the same price.
            ("energy_price", Value::Number(n)) if parse_price(&s.energy_price) == n.as_f64() => json!(s.energy_price),
            ("energy_price", Value::Number(n)) => json!(n.to_string()),
            _ => v,
        };
        let old = current.insert(k.clone(), v);
        // A value that does not fit (an older file, a hand edit) keeps the page's.
        if serde_json::from_value::<Settings>(Value::Object(current.clone())).is_err() {
            if let Some(old) = old {
                current.insert(k, old);
            }
        }
    }
    serde_json::from_value(Value::Object(current)).unwrap_or_else(|_| s.clone())
}

// ---------- talking to the Python part ----------

/// One command to the running sensors; their answer, or `None` if they do not run.
fn ask(command: &str) -> Option<Value> {
    let addr = SocketAddr::from(([127, 0, 0, 1], PORT));
    let mut conn = TcpStream::connect_timeout(&addr, Duration::from_millis(600)).ok()?;
    conn.set_read_timeout(Some(Duration::from_secs(2))).ok()?;
    conn.write_all(format!("{command}\n").as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(conn).read_line(&mut line).ok()?;
    serde_json::from_str(&line).ok()
}

/// "reload" in the background – `applied` must not wait for the network.
fn reload_soon() {
    std::thread::spawn(|| ask("reload"));
}

fn hidden(program: &str) -> Command {
    let mut cmd = Command::new(program);
    cmd.creation_flags(CREATE_NO_WINDOW).stdin(Stdio::null());
    cmd
}

fn start_task() -> bool {
    hidden("schtasks.exe").args(["/run", "/tn", TASK]).stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

// ---------- setup ----------

/// Where the Python part lies: `python\` next to the exe (a release), or next to `dist\` (the repo).
fn python_dir() -> PathBuf {
    let home = app::program_dir();
    let inside = home.join("python");
    if inside.exists() { inside } else { home.parent().map(|p| p.join("python")).unwrap_or(inside) }
}

fn sensors_exe() -> PathBuf {
    app::program_dir().join("runtime").join("HardwareMonitorSensors.exe")
}

#[derive(Debug, Clone, PartialEq, Default)]
enum TaskState {
    #[default]
    Missing,
    /// Points to another exe or script – the classic Toolbox's task, or a moved folder.
    Elsewhere(String),
    /// Points here, but still starts the sensors at logon by itself (the classic Toolbox's trigger, kept by an
    /// older "Set up") – then "Start with Windows" off would not keep them off.
    AtLogon,
    Here,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct Setup {
    /// Not checked yet.
    unknown: bool,
    runtime: bool,
    lhm: bool,
    pawnio: bool,
    task: TaskState,
    /// The task starts at logon (the classic Toolbox's autostart).
    task_at_logon: bool,
    task_enabled: bool,
}

impl Setup {
    fn ready(&self) -> bool {
        self.runtime && self.lhm && self.pawnio && matches!(self.task, TaskState::Here | TaskState::AtLogon)
    }
}

/// Looks what "Set up" has done already. PawnIO and the task through one hidden PowerShell (about a second).
fn check_setup() -> Setup {
    let home = app::program_dir();
    let script = format!(
        "$t = Get-ScheduledTask -TaskName '{TASK}' -ErrorAction SilentlyContinue; \
         $r = @{{ pawnio = [bool](Get-Service PawnIO -ErrorAction SilentlyContinue) }}; \
         if ($t) {{ $r.execute = $t.Actions[0].Execute; $r.arguments = $t.Actions[0].Arguments; \
         $r.state = \"$($t.State)\"; $r.triggers = @($t.Triggers).Count }}; $r | ConvertTo-Json -Compress"
    );
    let out = hidden("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", &script]).stderr(Stdio::null()).output();
    let info: Value = out.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default();
    let exe = info["execute"].as_str().unwrap_or_default();
    let arguments = info["arguments"].as_str().unwrap_or_default();
    let script = python_dir().join("hardware_monitor.py");
    let task = if exe.is_empty() {
        TaskState::Missing
    } else if same_path(Path::new(exe), &sensors_exe()) && arguments.trim().trim_matches('"') == script.to_string_lossy() {
        if info["triggers"].as_u64().unwrap_or(0) > 0 { TaskState::AtLogon } else { TaskState::Here }
    } else {
        TaskState::Elsewhere(format!("{exe} {arguments}").trim().to_string())
    };
    Setup {
        unknown: false,
        runtime: sensors_exe().exists(),
        lhm: home.join("lhm").join("LibreHardwareMonitorLib.dll").exists(),
        pawnio: info["pawnio"].as_bool().unwrap_or(false),
        task,
        task_at_logon: info["triggers"].as_u64().unwrap_or(0) > 0,
        task_enabled: info["state"].as_str().is_some_and(|s| s != "Disabled"),
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()),
    }
}

/// "Set up": `python\setup.ps1` in a console window of its own, so you see what happens. Waits for its end.
fn run_setup() -> bool {
    let script = python_dir().join("setup.ps1");
    Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .creation_flags(CREATE_NEW_CONSOLE)
        .status()
        .is_ok_and(|s| s.success())
}

// ---------- the program ----------

#[derive(Debug, Clone, PartialEq)]
enum Phase {
    Starting,
    NotSetUp,
    Running,
    Failed,
}

/// What the worker last found out – for the status line, the tray and the page.
#[derive(Debug, Clone, PartialEq)]
struct Report {
    phase: Phase,
    message: String,
    /// "CPU 45 °C · GPU 38 °C".
    temps: String,
    /// The order the running sensors were started with.
    order: Option<Vec<String>>,
    power_summary: Option<String>,
}

impl Report {
    fn starting() -> Report {
        Report { phase: Phase::Starting, message: t!("message.starting"), temps: String::new(), order: None, power_summary: None }
    }

    fn health(&self) -> Health {
        match self.phase {
            Phase::Running => Health::Ok,
            Phase::Failed => Health::Bad,
            _ => Health::Warn,
        }
    }

    /// From the answer to "status".
    fn running(status: &Value) -> Report {
        let temp = |kind: &str| status[kind].as_f64().map_or("–".to_string(), |v| format!("{v:.0} °C"));
        let icons: Vec<String> = status["icons"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect()).unwrap_or_else(|| vec!["cpu".into(), "gpu".into()]);
        let temps = icons.iter().map(|k| format!("{} {}", k.to_uppercase(), temp(k))).collect::<Vec<_>>().join("  ·  ");
        let order = status["order"].as_array().map(|a| a.iter().filter_map(|k| k.as_str().map(str::to_string)).collect());
        Report { phase: Phase::Running, message: t!("message.running"), temps, order, power_summary: status["power_summary"].as_str().map(str::to_string) }
    }

    fn line(&self) -> String {
        if self.phase == Phase::Running && !self.temps.is_empty() { self.temps.clone() } else { self.message.clone() }
    }
}

/// What the page asks the worker to do.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Order {
    SetUp,
    CheckSetup,
}

struct Monitor {
    settings: Settings,
    report: Report,
    setup: Setup,
    /// "Set up" runs right now.
    setting_up: bool,
    orders: Vec<Order>,
    /// The workers are at work.
    running: bool,
    /// The first click on "Delete" – the second one within `CONFIRM` deletes.
    delete_asked: Option<Instant>,
    /// The monitor's file as it was after our last look or write; a newer one was changed by the monitor itself.
    seen: Option<SystemTime>,
}

impl Monitor {
    fn new() -> Monitor {
        Monitor {
            settings: Settings::default(),
            report: Report::starting(),
            setup: Setup { unknown: true, ..Default::default() },
            setting_up: false,
            orders: vec![],
            running: false,
            delete_asked: None,
            seen: None,
        }
    }

    fn setup_section(&self, p: &mut Builder) {
        p.text(t!("page.setup.text"));
        let s = &self.setup;
        if self.setting_up {
            p.status(t!("page.setup.running"), Health::Warn);
        } else if s.unknown {
            p.status(t!("page.setup.checking"), Health::Warn);
        } else {
            let line = |p: &mut Builder, what: String, ok: bool| {
                if ok {
                    p.status(t!("page.setup.there", what = what), Health::Ok);
                } else {
                    p.status(t!("page.setup.missing", what = what), Health::Warn);
                }
            };
            line(p, t!("page.setup.runtime"), s.runtime);
            line(p, t!("page.setup.lhm"), s.lhm);
            line(p, t!("page.setup.pawnio"), s.pawnio);
            let task = t!("page.setup.task", task = TASK);
            match &s.task {
                TaskState::Elsewhere(target) => p.status(t!("page.setup.elsewhere", what = task, target = target), Health::Warn),
                TaskState::AtLogon => p.status(t!("page.setup.at_logon", what = task), Health::Warn),
                other => line(p, task, *other == TaskState::Here),
            }
        }
        if !self.setting_up {
            p.buttons(|b| {
                b.button(t!("page.setup.button"), "set_up").button(t!("page.setup.again"), "check_setup");
            });
        }
    }
}

impl Program for Monitor {
    type Settings = Settings;
    const META: kit::Meta = kit::meta!();

    fn page(&self, p: &mut Builder, s: &Settings) {
        let r = &self.report;
        p.section(t!("page.status"), |p| {
            if self.running {
                p.status(r.line(), r.health());
                if r.order.as_ref().is_some_and(|o| *o != s.icon_order) {
                    p.status(t!("page.order.hint"), Health::Warn);
                }
            } else {
                p.text(t!("page.stopped"));
            }
        });
        // Open until everything is there; then it only takes room.
        if self.setup.ready() && self.setup.task == TaskState::Here {
            p.section_closed(t!("page.setup"), |p| self.setup_section(p));
        } else {
            p.section(t!("page.setup"), |p| self.setup_section(p));
        }
        p.section(t!("page.refresh"), |p| {
            p.text(t!("page.refresh.text"));
            let rates = |r: &[f64]| r.iter().map(|v| (*v, format!("{} s", v.to_string().replace('.', if i18n::language() == "de" { "," } else { "." })))).collect::<Vec<_>>();
            p.choice(t!("page.refresh.icons"), "icon_interval", &rates(&ICON_RATES));
            p.choice(t!("page.refresh.window"), "popup_interval", &rates(&POPUP_RATES));
            p.choice(t!("page.efficiency"), "efficiency_mode", &[("auto", t!("page.automatic")), ("on", t!("page.efficiency.on")), ("off", t!("page.efficiency.off"))]);
            p.text(t!("page.efficiency.info"));
        });
        p.section(t!("page.display"), |p| {
            p.choice(t!("page.order"), "icon_order", &[(vec!["cpu", "gpu"], t!("page.order.cpu")), (vec!["gpu", "cpu"], t!("page.order.gpu"))]);
            let sensors = |known: &[&str], current: &str| {
                let mut v = vec![("auto".to_string(), t!("page.automatic"))];
                v.extend(known.iter().map(|n| (n.to_string(), n.to_string())));
                // A name typed in the classic Toolbox or the file stays choosable.
                if current != "auto" && !known.contains(&current) {
                    v.push((current.to_string(), current.to_string()));
                }
                v
            };
            p.choice(t!("page.sensor_cpu"), "cpu_sensor", &sensors(&CPU_SENSORS, &s.cpu_sensor));
            p.choice(t!("page.sensor_gpu"), "gpu_sensor", &sensors(&GPU_SENSORS, &s.gpu_sensor));
            p.text(t!("page.sensor.text"));
            p.text(t!("page.colors.text"));
            // Yellow always below red: each slider ends where the other one starts.
            p.slider(t!("page.cpu_yellow"), "cpu_warn_temp", 40.0, (s.cpu_hot_temp.min(101) - 1) as f64, 1.0, "°C");
            p.slider(t!("page.cpu_red"), "cpu_hot_temp", (s.cpu_warn_temp.max(40) + 1) as f64, 110.0, 1.0, "°C");
            p.slider(t!("page.gpu_yellow"), "gpu_warn_temp", 40.0, (s.gpu_hot_temp.min(101) - 1) as f64, 1.0, "°C");
            p.slider(t!("page.gpu_red"), "gpu_hot_temp", (s.gpu_warn_temp.max(40) + 1) as f64, 110.0, 1.0, "°C");
        });
        p.section(t!("page.privacy"), |p| {
            p.text(t!("page.privacy.text"));
            p.switch(t!("page.privacy.switch"), "privacy_mode");
        });
        p.section(t!("page.power"), |p| {
            p.text(t!("page.power.text"));
            p.switch(t!("page.power.switch"), "energy_logging");
            match (&r.power_summary, self.running) {
                (Some(summary), true) => p.status(summary.clone(), if s.energy_logging { Health::Ok } else { Health::Warn }),
                _ => p.text(t!("page.power.summary_stopped")),
            }
            p.entry(t!("page.power.price"), "energy_price");
            p.entry(t!("page.power.currency"), "energy_currency");
            p.slider(t!("page.power.other"), "energy_other_watts", 0.0, 300.0, 5.0, "W");
            p.slider(t!("page.power.efficiency"), "energy_psu_efficiency", 50.0, 100.0, 1.0, "%");
            p.text(t!("page.power.calibrate"));
            let asked = self.delete_asked.is_some_and(|t| t.elapsed() < CONFIRM);
            if asked {
                p.status(t!("page.power.delete_question"), Health::Bad);
            }
            p.buttons(|b| {
                b.button(t!("page.power.show"), "show_power");
                if asked {
                    b.button(t!("page.power.delete_really"), "delete_power");
                } else {
                    b.button(t!("page.power.delete"), "delete_power");
                }
            });
        });
        p.section(t!("page.fans"), |p| {
            p.text(t!("page.fans.text"));
            p.buttons(|b| {
                b.button(t!("page.fans.show"), "show_fans");
            });
        });
        p.section(t!("page.detail_window"), |p| {
            p.text(t!("page.detail_window.text"));
            p.buttons(|b| {
                b.button(t!("page.open_folder"), "open_folder");
            });
        });
        p.section(t!("page.advanced"), |p| {
            p.buttons(|b| {
                b.button(t!("page.open_log"), "open_log").button(t!("page.open_settings_file"), "open_settings_file").button(t!("page.reset"), "reset");
            });
        });
    }

    fn status(&self, running: bool) -> Status {
        if running { Status { running, state: self.report.health(), text: self.report.line() } } else { Status::default() }
    }

    fn tray(&self) -> Tray {
        Tray { line: if self.running { self.report.line() } else { String::new() }, menu: vec![], live: vec![] }
    }

    fn applied(&mut self, s: &Settings) {
        self.settings = s.clone();
        let file = monitor_file();
        match export(s, &i18n::language(), &file) {
            Ok(true) => {
                self.seen = modified(&file);
                reload_soon();
            }
            Ok(false) => {}
            Err(e) => app::log(&format!("{} could not be written: {e}", file.display())),
        }
    }

    fn action(&mut self, name: &str, s: &mut Settings) -> Option<String> {
        match name {
            "set_up" if !self.setting_up => self.orders.push(Order::SetUp),
            "check_setup" => self.orders.push(Order::CheckSetup),
            "show_power" | "show_fans" => {
                if ask(name).is_none() {
                    return Some(t!("toast.not_running"));
                }
            }
            "delete_power" => {
                if !self.delete_asked.is_some_and(|t| t.elapsed() < CONFIRM) {
                    self.delete_asked = Some(Instant::now());
                    return None;
                }
                self.delete_asked = None;
                // The running monitor has the values in memory too – it deletes both.
                let ok = match ask("status") {
                    Some(_) => ask("energy_delete").is_some_and(|r| r["ok"].as_bool() == Some(true)),
                    None => {
                        let file = app::data_dir().join(ENERGY_FILE);
                        !file.exists() || std::fs::remove_file(&file).is_ok()
                    }
                };
                return Some(if ok { t!("toast.deleted") } else { t!("toast.delete_failed") });
            }
            "open_folder" => {
                process::open(&python_dir().to_string_lossy());
            }
            "open_log" => {
                process::open(&app::data_dir().join("monitor.log").to_string_lossy());
            }
            "open_settings_file" => {
                process::open(&monitor_file().to_string_lossy());
            }
            "reset" => {
                *s = Settings::default();
                // Everything of the monitor goes back too: window position and fan curves (fans back to Auto).
                let file = monitor_file();
                let mut map = read_json(&file);
                map.insert("fan_control".into(), json!({}));
                map.insert("window".into(), json!(""));
                if write_json(&file, &map).is_ok() {
                    self.seen = modified(&file);
                    reload_soon();
                }
                return Some(t!("toast.reset"));
            }
            _ => {}
        }
        None
    }

    /// A second start of HardwareMonitor.exe, or "show" from the Toolbox: the page.
    fn show(&mut self) {
        app::open_in_toolbox(Self::META.id);
    }

    fn shutdown(&mut self) {
        if self.running {
            ask("quit");
        }
    }
}

// ---------- the worker ----------

fn publish(rt: &Runtime<Monitor>, report: Report) {
    let changed = rt.with(|p, _| std::mem::replace(&mut p.report, report.clone()) != report);
    if changed {
        rt.changed();
    }
}

/// Runs the orders of the page: "Set up", "Check again".
fn do_orders(rt: &Runtime<Monitor>) {
    let orders = rt.with(|p, _| std::mem::take(&mut p.orders));
    for order in orders {
        if order == Order::SetUp {
            rt.with(|p, _| p.setting_up = true);
            rt.changed();
            let ok = run_setup();
            app::log(if ok { "set up" } else { "setup did not finish" });
        }
        let setup = check_setup();
        rt.with(|p, _| {
            p.setting_up = false;
            p.setup = setup;
        });
        rt.changed();
    }
}

/// What the monitor changed in its file itself (its right-click menu) comes onto the page.
fn take_over_changes(rt: &Runtime<Monitor>) {
    let file = monitor_file();
    let now = modified(&file);
    if now.is_none() || rt.with(|p, _| p.seen) == now {
        return;
    }
    rt.with(|p, _| p.seen = now);
    let (current, new) = rt.with(|_, s| (s.clone(), import(s, &file)));
    if new != current {
        rt.update(|s| *s = new);
    }
}

fn work(rt: Runtime<Monitor>) {
    let setup = check_setup();
    rt.with(|p, _| p.setup = setup);
    rt.changed();
    let mut language = i18n::language();
    loop {
        // While the program only serves its page, the worker looks after the page's orders only.
        while !rt.running() {
            do_orders(&rt);
            rt.sleep(Duration::from_secs(3600));
        }
        rt.with(|p, _| p.running = true);
        publish(&rt, Report::starting());
        let since = Instant::now();
        let (mut seen_running, mut asked_task, mut quit_itself) = (false, false, false);
        while rt.running() {
            do_orders(&rt);
            // The Toolbox switched its language: the monitor follows.
            if i18n::language() != language {
                language = i18n::language();
                let s = rt.with(|_, s| s.clone());
                if export(&s, &language, &monitor_file()).unwrap_or(false) {
                    rt.with(|p, _| p.seen = modified(&monitor_file()));
                    ask("reload");
                }
            }
            take_over_changes(&rt);
            match ask("status") {
                Some(status) => {
                    seen_running = true;
                    publish(&rt, Report::running(&status));
                }
                // It ran and has gone: "Quit" in its own right-click menu. The program stops with it.
                None if seen_running => {
                    app::log("the sensors were quit from their own menu");
                    quit_itself = true;
                    rt.stop();
                    break;
                }
                None if !asked_task => {
                    asked_task = true;
                    let ready = rt.with(|p, _| p.setup.ready());
                    if !ready {
                        let setup = check_setup();
                        rt.with(|p, _| p.setup = setup);
                    }
                    if !rt.with(|p, _| p.setup.ready()) {
                        publish(&rt, Report { phase: Phase::NotSetUp, message: t!("message.not_set_up"), ..Report::starting() });
                    } else if !start_task() {
                        publish(&rt, Report { phase: Phase::Failed, message: t!("message.start_failed", task = TASK), ..Report::starting() });
                    }
                }
                None => {
                    let ready = rt.with(|p, _| p.setup.ready());
                    if ready && since.elapsed() > START_WAIT {
                        publish(&rt, Report { phase: Phase::Failed, message: t!("message.no_answer"), ..Report::starting() });
                    } else if !ready {
                        // Set up in the meantime? Then try the task again.
                        asked_task = false;
                    }
                }
            }
            rt.sleep(POLL);
        }
        if !quit_itself {
            ask("quit");
        }
        rt.with(|p, _| p.running = false);
        publish(&rt, Report::starting());
    }
}

// ---------- from the classic Toolbox ----------

/// On the very first start (no `data\hardware_monitor.json` yet): the classic Toolbox's files are carried over
/// once – settings, power log – if one lies in a folder above. Its task started the monitor at logon; that
/// becomes this program's "Start with Windows".
fn carry_over(program_dir: &Path, data: &Path) -> Option<Settings> {
    if data.join(MONITOR_FILE).exists() {
        return None;
    }
    for above in program_dir.ancestors().skip(1).take(4) {
        let old = above.join("Toolbox").join("data");
        let old = if old.join(MONITOR_FILE).exists() { old } else { above.join("data") };
        if !old.join(MONITOR_FILE).exists() {
            continue;
        }
        let _ = std::fs::create_dir_all(data);
        for name in [MONITOR_FILE, ENERGY_FILE] {
            if old.join(name).exists() {
                let _ = std::fs::copy(old.join(name), data.join(name));
            }
        }
        app::log(&format!("settings carried over from {}", old.display()));
        return Some(import(&Settings::default(), &data.join(MONITOR_FILE)));
    }
    None
}

fn main() {
    kit::texts!();
    app::init(&Monitor::META);
    let data = app::data_dir();
    if let Some(old) = carry_over(&app::program_dir(), &data) {
        // The page's file gets the carried-over values before the kit reads it.
        if !data.join("settings.json").exists() {
            if let Ok(Value::Object(m)) = serde_json::to_value(&old) {
                let _ = write_json(&data.join("settings.json"), &m);
            }
        }
        // The classic task started the monitor at logon: "Start with Windows" takes that over.
        let setup = check_setup();
        if setup.task_at_logon && setup.task_enabled {
            if let Ok(exe) = std::env::current_exe() {
                let _ = kit::autostart::set(Monitor::META.id, Some(&exe));
                app::log("the classic task started the monitor at logon – \"Start with Windows\" is on");
            }
        }
    }
    let mut monitor = Monitor::new();
    monitor.seen = modified(&monitor_file());
    app::run(monitor, work);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(running: bool) -> Monitor {
        kit::texts!();
        kit::i18n::set_language("en");
        Monitor { running, ..Monitor::new() }
    }

    fn page(m: &Monitor, s: &Settings) -> String {
        let v = serde_json::to_value(s).unwrap();
        let mut b = Builder::new(&v);
        m.page(&mut b, s);
        serde_json::to_string(&b.finish()).unwrap()
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hardware-monitor-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Every key the page names is a field of the settings (Toolbox\DESIGN.md 13).
    #[test]
    fn page_keys_are_settings() {
        let m = monitor(true);
        kit::page::check_keys::<Settings>(|p| m.page(p, &Settings::default())).unwrap();
    }

    #[test]
    fn texts_are_complete() {
        kit::i18n::check_ids(env!("CARGO_MANIFEST_DIR")).unwrap();
    }

    /// The defaults are the Python part's (`DEFAULT_SETTINGS`): a page that was never opened changes nothing.
    #[test]
    fn defaults_match_the_python_part() {
        let python = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/python/hardware_monitor_settings.py")).unwrap();
        for (k, v) in to_monitor(&Settings::default()) {
            let v = match v {
                Value::Bool(b) => if b { "True".to_string() } else { "False".to_string() },
                Value::Number(n) if n.as_f64().is_some_and(|f| f.fract() == 0.0) => format!("{}", n.as_f64().unwrap() as i64),
                Value::Array(a) => format!("[{}]", a.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(", ")),
                other => other.to_string(),
            };
            assert!(python.contains(&format!("\"{k}\": {v},")), "{k}: {v}");
        }
    }

    #[test]
    fn the_monitor_s_file_both_ways() {
        let dir = temp("file");
        let file = dir.join(MONITOR_FILE);
        // The monitor's own values stay; only what differs is written.
        std::fs::write(&file, r#"{"window": "480x640+1+2", "fan_control": {"x": 1}, "icon_interval": 2, "energy_price": 0.35}"#).unwrap();
        let s = Settings::default();
        assert!(export(&s, "de", &file).unwrap());
        let m = read_json(&file);
        assert_eq!((m["window"].as_str(), m["fan_control"]["x"].as_i64(), m["language"].as_str()), (Some("480x640+1+2"), Some(1), Some("de")));
        assert!(!export(&s, "de", &file).unwrap(), "nothing differs any more");
        // A price as typed: a number in the file, the text stays on the page while it means the same.
        let typed = Settings { energy_price: "0,4".into(), ..s.clone() };
        export(&typed, "de", &file).unwrap();
        assert_eq!(read_json(&file)["energy_price"].as_f64(), Some(0.4));
        assert_eq!(import(&typed, &file).energy_price, "0,4");
        // Not a price: the file keeps its own.
        export(&Settings { energy_price: "abc".into(), ..s.clone() }, "de", &file).unwrap();
        assert_eq!(read_json(&file)["energy_price"].as_f64(), Some(0.4));

        // What the monitor changed itself comes onto the page; values that do not fit do not.
        let mut m = read_json(&file);
        m.insert("privacy_mode".into(), json!(true));
        m.insert("icon_interval".into(), json!(5));
        m.insert("cpu_warn_temp".into(), json!("hot"));
        m.insert("energy_price".into(), json!(0.5));
        write_json(&file, &m).unwrap();
        let back = import(&s, &file);
        assert_eq!((back.privacy_mode, back.icon_interval, back.cpu_warn_temp, back.energy_price.as_str()), (true, 5.0, 75, "0.5"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_page_tells_the_state() {
        let mut m = monitor(false);
        let s = Settings::default();
        assert!(page(&m, &s).contains("Stopped"));
        assert!(page(&m, &s).contains("Checking the setup"));
        m.setup = Setup { runtime: true, lhm: false, pawnio: true, task: TaskState::Elsewhere("C:\\old\\HardwareMonitor.exe".into()), ..Default::default() };
        let text = page(&m, &s);
        assert!(text.contains("LibreHardwareMonitor in lhm\\\\: missing") && text.contains("points elsewhere (C:\\\\old\\\\HardwareMonitor.exe)"), "{text}");
        m.running = true;
        m.report = Report::running(&json!({"ok": true, "cpu": 45.4, "gpu": null, "icons": ["cpu", "gpu"], "order": ["gpu", "cpu"], "power_summary": "now ≈ 120 W"}));
        let text = page(&m, &s);
        assert!(text.contains("CPU 45 °C  ·  GPU –"), "{text}");
        assert!(text.contains("after Stop and Start"), "the order differs from the running one");
        assert!(text.contains("now ≈ 120 W"));
        assert_eq!(m.status(true).text, "CPU 45 °C  ·  GPU –");
        assert_eq!(m.status(false), Status::default());
    }

    #[test]
    fn delete_asks_first() {
        let mut m = monitor(false);
        let mut s = Settings::default();
        assert_eq!(m.action("delete_power", &mut s), None);
        assert!(page(&m, &s).contains("Really delete"));
    }

    #[test]
    fn the_classic_toolbox_s_files() {
        let root = temp("carry");
        let (program, data) = (root.join("Hardware Monitor").join("dist"), root.join("Hardware Monitor").join("dist").join("data"));
        std::fs::create_dir_all(root.join("Toolbox").join("data")).unwrap();
        std::fs::create_dir_all(&program).unwrap();
        std::fs::write(root.join("Toolbox/data").join(MONITOR_FILE), r#"{"privacy_mode": true, "energy_price": 0.31, "window": "1x1+0+0"}"#).unwrap();
        std::fs::write(root.join("Toolbox/data").join(ENERGY_FILE), "{}").unwrap();
        let s = carry_over(&program, &data).unwrap();
        assert!(s.privacy_mode && s.energy_price == "0.31");
        assert!(data.join(ENERGY_FILE).exists() && read_json(&data.join(MONITOR_FILE))["window"] == "1x1+0+0");
        assert!(carry_over(&program, &data).is_none(), "never a second time");
        std::fs::remove_dir_all(&root).ok();
    }
}
