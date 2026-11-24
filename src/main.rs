//! Hardware Monitor – CPU and GPU temperature in the taskbar, and a detail window with almost every value of the
//! hardware, the power consumption, fan curves and the drives' health.
//!
//! One exe, two roles (Toolbox\DESIGN.md 5): the monitor – a kit program with its page in the Toolbox, its worker
//! reads the sensors (`monitor.rs`) – and, started with `--window`, its detail window (`window.rs`), a process of its
//! own that asks the monitor over a pipe (`link.rs`) and ends when it is closed.
//!
//! The sensors come from LibreHardwareMonitor, which needs admin rights. Only the bridge `LhmBridge.exe`
//! (`bridge\LhmBridge.cs`, `bridge.rs`) has them: "Set up" puts it into a folder only admins can write to and
//! registers a task that starts it without a prompt. Everything else runs as the user.
//!
//! The settings, fan curves and the window's place are in `data\settings.json` (the kit's file); the power logs in
//! `data\hardware_monitor_energy.json` and `data\hardware_monitor_plug.json`.
#![windows_subsystem = "windows"]

mod bridge;
mod content;
mod drive_view;
mod energy;
mod fan_view;
mod fans;
mod hardware_data;
mod link;
mod monitor;
mod nvml;
mod plug;
mod power_view;
mod sensors;
mod window;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kit::app::{self, Program, Runtime};
use kit::page::Builder;
use kit::proto::{Health, Live, MenuEntry, Status, Tray};
use kit::setup::{self, Done};
use kit::{i18n, process, t};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The Python version kept fan curves and the window's place in this file; taken over once.
const OLD_FILE: &str = "hardware_monitor.json";
/// A second click on "Delete" within this time deletes.
const CONFIRM: Duration = Duration::from_secs(8);

const ICON_RATES: [f64; 4] = [1.0, 2.0, 5.0, 10.0];
const POPUP_RATES: [f64; 4] = [0.5, 1.0, 2.0, 5.0];
/// Sensor names of LibreHardwareMonitor – they stay English.
const CPU_SENSORS: [&str; 6] = ["Core (Tctl/Tdie)", "CPU Package", "Core Max", "Core Average", "CCD1 (Tdie)", "CCD2 (Tdie)"];
const GPU_SENSORS: [&str; 3] = ["GPU Core", "GPU Hot Spot", "GPU Memory Junction"];

/// The settings. A new one = one field here + one line in `page`.
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
    /// As typed ("0,35" works too).
    pub energy_price: String,
    pub energy_currency: String,
    pub energy_other_watts: u32,
    pub energy_psu_efficiency: u32,
    /// The Power tab's chart: "bars" or "line" (separate lines for the total, CPU + GPU and the rest).
    pub power_chart: String,
    /// The two temperatures as numbers next to the Toolbox's icon (DESIGN.md 6) – off by default.
    pub taskbar_temps: bool,
    /// Read a smart plug's measured power.
    pub plug: bool,
    /// "" = found by itself on the home network.
    pub plug_address: String,
    /// "" = the name from its serial number.
    pub plug_name: String,
    /// Each fan's settings (`fans.rs`): set in the detail window's tab "Fans".
    pub fan_control: Map<String, Value>,
    /// Names given to the motherboard's fan headers there (control id → name) – LHM only knows "Fan #2".
    pub fan_names: BTreeMap<String, String>,
    /// The headers marked as CPU fans there – listed under "CPU".
    pub cpu_fans: Vec<String>,
    /// The detail window's size and place: "WxH+X+Y" in pixels, or "".
    pub window: String,
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
            power_chart: "bars".into(),
            taskbar_temps: false,
            plug: false,
            plug_address: String::new(),
            plug_name: String::new(),
            fan_control: Map::new(),
            fan_names: BTreeMap::new(),
            cpu_fans: vec![],
            window: String::new(),
        }
    }
}

/// "0.35", "0,35", " 0.35 " → 0.35; a price from 0 to below 100, else `None`.
fn parse_price(text: &str) -> Option<f64> {
    text.trim().replace(',', ".").parse::<f64>().ok().filter(|p| (0.0..100.0).contains(p))
}

fn read_json(file: &Path) -> Map<String, Value> {
    std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str(s.trim_start_matches('\u{feff}')).ok()).unwrap_or_default()
}

/// The Python version's `data\hardware_monitor.json`: its fan curves and the window's place come into the settings. The file
/// goes once the settings with them are saved – at the next start.
fn take_over_old_file(rt: &Runtime<Monitor>) {
    let old = app::data_dir().join(OLD_FILE);
    if !old.exists() {
        return;
    }
    if read_json(&app::data_dir().join("settings.json")).contains_key("fan_control") {
        match std::fs::remove_file(&old) {
            Ok(()) => app::log(&format!("{OLD_FILE} was taken over earlier and is deleted now")),
            Err(e) => app::log(&format!("{OLD_FILE} could not be deleted: {e}")),
        }
        return;
    }
    let map = read_json(&old);
    let fans = map.get("fan_control").and_then(Value::as_object).cloned().unwrap_or_default();
    let window = map.get("window").and_then(Value::as_str).unwrap_or_default().to_string();
    rt.update(|s| {
        if s.fan_control.is_empty() {
            s.fan_control = fans;
        }
        if s.window.is_empty() {
            s.window = window;
        }
    });
    app::log(&format!("fan curves and the window's place taken over from {OLD_FILE}"));
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
    /// The numbers for the taskbar (empty when switched off).
    live: Vec<Live>,
    /// With a smart plug: what it says.
    plug: Option<monitor::PlugView>,
}

impl Report {
    fn starting() -> Report {
        Report { phase: Phase::Starting, message: t!("message.starting"), temps: String::new(), order: None, power_summary: None, live: vec![], plug: None }
    }

    fn health(&self) -> Health {
        match self.phase {
            Phase::Running => Health::Ok,
            Phase::Failed => Health::Bad,
            _ => Health::Warn,
        }
    }

    fn line(&self) -> String {
        if self.phase == Phase::Running && !self.temps.is_empty() { self.temps.clone() } else { self.message.clone() }
    }
}

/// What the page asks the worker to do with the bridge: set up, uninstall, test, look again.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Order {
    SetUp,
    Remove,
    Test,
    Check,
}

/// What the page shows about the setup.
#[derive(Debug, Clone, PartialEq, Default)]
struct BridgeView {
    /// Each step's name and how far it is; `None` = not checked yet.
    steps: Option<Vec<(String, Done)>>,
    /// Something of it is in Windows (task or protected folder).
    present: bool,
    /// An order runs right now.
    busy: Option<Order>,
    /// The last result: a line and its colour.
    result: Option<(String, Health)>,
}

impl BridgeView {
    fn ready(&self) -> bool {
        self.steps.as_ref().is_some_and(|s| s.iter().all(|(_, d)| *d == Done::Yes))
    }
}

struct Monitor {
    settings: Settings,
    report: Report,
    orders: Vec<Order>,
    /// The workers are at work.
    running: bool,
    /// The first click on "Delete" – the second one within `CONFIRM` deletes.
    delete_asked: Option<Instant>,
    bridge: BridgeView,
    /// Each controllable fan and what the control does with it.
    fan_info: Vec<(String, fans::State)>,
    /// Fans the window asked to boost ("Boost") – the monitor takes them at its next round.
    fan_boosts: Vec<String>,
    /// "Delete all saved data" while the monitor runs – it holds the logs and deletes them.
    energy_delete_asked: bool,
    /// The detail window's connection, while one is open (`link.rs`).
    window: Option<Arc<kit::ipc::Conn>>,
    /// What it shows – and so what the monitor reads for it.
    window_want: link::Want,
    /// A window was just started – a second click does not start another.
    window_started: Option<Instant>,
}

impl Monitor {
    fn new() -> Monitor {
        Monitor {
            settings: Settings::default(),
            report: Report::starting(),
            orders: vec![],
            running: false,
            delete_asked: None,
            bridge: BridgeView::default(),
            fan_info: vec![],
            fan_boosts: vec![],
            energy_delete_asked: false,
            window: None,
            window_want: link::Want::default(),
            window_started: None,
        }
    }

    /// Brings the detail window forward – or starts it: the same exe in its window role (`link::ARGUMENT`).
    fn open_window(&mut self, tab: Option<&str>) {
        if let Some(c) = &self.window {
            let _ = c.send(&link::ToWindow::Front { tab: tab.map(str::to_string) });
            return;
        }
        if self.window_started.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        let mut args = vec![link::ARGUMENT];
        args.extend(tab);
        match process::spawn(&exe, &args) {
            Ok(_) => self.window_started = Some(Instant::now()),
            Err(e) => app::log(&format!("the detail window could not be started: {e}")),
        }
    }

    /// "Set up": PawnIO, LibreHardwareMonitor and the bridge in the protected folder, the task.
    fn setup_section(&self, p: &mut Builder) {
        let b = &self.bridge;
        p.text(t!("page.setup.text", folder = setup::protected_dir(bridge::ID).display(), task = setup::task_name(bridge::ID)));
        match &b.steps {
            None => p.status(t!("page.setup.checking"), Health::Warn),
            Some(steps) => {
                for (name, done) in steps {
                    match done {
                        Done::Yes => p.status(t!("page.setup.there", what = name), Health::Ok),
                        Done::No => p.status(t!("page.setup.missing", what = name), Health::Warn),
                        Done::Outdated => p.status(t!("page.bridge.outdated", what = name), Health::Warn),
                    }
                }
            }
        }
        if let Some(order) = b.busy {
            p.status(
                match order {
                    Order::SetUp => t!("page.bridge.setting_up"),
                    Order::Remove => t!("page.bridge.removing"),
                    Order::Test => t!("page.bridge.testing"),
                    Order::Check => t!("page.setup.checking"),
                },
                Health::Warn,
            );
        } else if let Some((line, health)) = &b.result {
            p.status(line.clone(), *health);
        }
        if b.busy.is_none() {
            let ready = b.ready();
            p.buttons(|x| {
                x.button(t!("page.setup.button"), "bridge_set_up");
                if ready {
                    x.button(t!("page.bridge.test"), "bridge_test");
                }
                x.button(t!("page.setup.again"), "bridge_check");
                if b.present {
                    x.button(t!("page.bridge.remove"), "bridge_remove");
                }
            });
            p.text(t!("page.bridge.pawnio"));
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
                for (name, state) in &self.fan_info {
                    let text = match (state.software, state.pct, state.note.as_deref()) {
                        (true, _, Some("boost")) => {
                            p.status(t!("page.fan.boost", name = name), Health::Ok);
                            continue;
                        }
                        (true, Some(pct), Some("hot")) => t!("page.fan.hot", name = name, pct = pct),
                        (true, Some(pct), _) => t!("page.fan.controlled", name = name, pct = pct),
                        (_, _, Some("no_temperature")) => t!("page.fan.no_temperature", name = name),
                        (_, _, Some("error")) => t!("page.fan.error", name = name),
                        _ => continue,
                    };
                    p.status(text, if state.note.is_some() { Health::Warn } else { Health::Ok });
                }
            } else {
                p.text(t!("page.stopped"));
            }
        });
        p.section(t!("page.taskbar"), |p| {
            p.text(t!("page.taskbar.text"));
            p.switch(t!("page.taskbar.switch"), "taskbar_temps");
            p.text(t!("page.taskbar.hidden"));
            p.buttons(|b| {
                b.button(t!("page.taskbar.open"), "open_taskbar_settings");
            });
        });
        // Open until everything is there; then it only takes room.
        if self.bridge.ready() && self.bridge.busy.is_none() {
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
                // A name typed into the file stays choosable.
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
            p.choice(t!("page.power.chart"), "power_chart", &[("bars", t!("page.power.chart.bars")), ("line", t!("page.power.chart.line"))]);
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
        p.section(t!("page.plug"), |p| {
            p.text(t!("page.plug.text"));
            p.text(t!("page.plug.steps"));
            p.switch(t!("page.plug.switch"), "plug");
            if s.plug {
                p.entry(t!("page.plug.address"), "plug_address");
                p.entry(t!("page.plug.name"), "plug_name");
                match (&r.plug, self.running) {
                    (Some(view), true) => p.status(view.line.clone(), view.health),
                    _ => p.text(t!("page.plug.stopped")),
                }
            }
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
        if !self.running {
            return Tray::default();
        }
        let label = if self.settings.privacy_mode { t!("tray.privacy_on") } else { t!("tray.privacy_off") };
        Tray { line: self.report.line(), menu: vec![MenuEntry { label, action: "toggle_privacy".into() }], live: self.report.live.clone() }
    }

    fn applied(&mut self, s: &Settings) {
        self.settings = s.clone();
    }

    fn action(&mut self, name: &str, s: &mut Settings) -> Option<String> {
        match name {
            "bridge_set_up" | "bridge_remove" | "bridge_test" | "bridge_check" if self.bridge.busy.is_none() => {
                let order = match name {
                    "bridge_set_up" => Order::SetUp,
                    "bridge_remove" => Order::Remove,
                    "bridge_test" => Order::Test,
                    _ => Order::Check,
                };
                self.bridge.busy = Some(order);
                self.orders.push(order);
            }
            "toggle_privacy" => s.privacy_mode = !s.privacy_mode,
            "open_taskbar_settings" => {
                process::open("ms-settings:taskbar");
            }
            "show_power" | "show_fans" => {
                if !self.running {
                    return Some(t!("toast.not_running"));
                }
                self.open_window(Some(if name == "show_power" { "tab.power" } else { "tab.fans" }));
            }
            "delete_power" => {
                if !self.delete_asked.is_some_and(|t| t.elapsed() < CONFIRM) {
                    self.delete_asked = Some(Instant::now());
                    return None;
                }
                self.delete_asked = None;
                // The running monitor holds the logs in memory – it deletes them itself.
                if self.running {
                    self.energy_delete_asked = true;
                    return Some(t!("toast.deleted"));
                }
                let ok = [monitor::ENERGY_FILE, monitor::PLUG_FILE, monitor::MINUTES_FILE].iter().all(|f| {
                    let file = app::data_dir().join(f);
                    !file.exists() || std::fs::remove_file(&file).is_ok()
                });
                return Some(if ok { t!("toast.deleted") } else { t!("toast.delete_failed") });
            }
            "open_folder" => {
                let file = content::file();
                process::open(&file.parent().unwrap_or(&file).to_string_lossy());
            }
            "open_log" => {
                process::open(&app::data_dir().join("log.txt").to_string_lossy());
            }
            "open_settings_file" => {
                process::open(&app::data_dir().join("settings.json").to_string_lossy());
            }
            "reset" => {
                // Everything goes back – the fan curves too (fans back to Auto) and the window's place.
                *s = Settings::default();
                return Some(t!("toast.reset"));
            }
            _ => {}
        }
        None
    }

    /// A second start of HardwareMonitor.exe, or "show" from the Toolbox (a click on a temperature): the detail
    /// window while the monitor runs, else the page.
    fn show(&mut self) {
        if self.running {
            self.open_window(None);
        } else {
            app::open_in_toolbox(Self::META.id);
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

/// Runs the page's orders: "Set up", "Uninstall", "Test", "Check again".
fn do_orders(rt: &Runtime<Monitor>) {
    let orders = rt.with(|p, _| std::mem::take(&mut p.orders));
    for order in orders {
        rt.changed();
        let result = match order {
            Order::SetUp => Some(match setup::run(bridge::ID, &bridge::steps(), &t!("bridge.task_description")) {
                Ok(()) => (t!("page.bridge.set_up"), Health::Ok),
                Err(why) => (t!("page.bridge.failed", why = why), Health::Bad),
            }),
            // PawnIO stays: other programs may use it.
            Order::Remove => Some(match setup::remove(bridge::ID, &bridge::steps(), false) {
                Ok(()) => (t!("page.bridge.removed"), Health::Ok),
                Err(why) => (t!("page.bridge.failed", why = why), Health::Bad),
            }),
            Order::Test => Some(match bridge::test() {
                Ok(line) => (line, Health::Ok),
                Err(why) => (t!("page.bridge.failed", why = why), Health::Bad),
            }),
            Order::Check => None,
        };
        app::log(&format!("bridge {order:?}: {result:?}"));
        rt.with(|p, _| {
            p.bridge.busy = None;
            if result.is_some() {
                p.bridge.result = result;
            }
        });
        check_bridge(rt);
    }
}

/// The detail window's pipe: one window at a time; what it says is done here, what it wants is read by the monitor.
fn window_server(rt: Runtime<Monitor>) {
    let server = match kit::ipc::Server::bind(link::PIPE) {
        Ok(s) => s,
        Err(e) => return app::log(&format!("the window's pipe could not be created: {e}")),
    };
    loop {
        let Ok(conn) = server.accept() else {
            std::thread::sleep(Duration::from_millis(500));
            continue;
        };
        if let Some(old) = rt.with(|p, _| p.window.replace(conn.clone())) {
            old.close();
        }
        rt.with(|p, _| p.window_started = None);
        let rt = rt.clone();
        std::thread::spawn(move || {
            while let Some(line) = conn.read_line() {
                let Ok(msg) = serde_json::from_str::<link::ToMonitor>(&line) else { continue };
                window_said(&rt, msg);
            }
            rt.with(|p, _| {
                if p.window.as_ref().is_some_and(|w| Arc::ptr_eq(w, &conn)) {
                    p.window = None;
                    p.window_want = link::Want::default();
                }
            });
            rt.nudge();
        });
    }
}

fn window_said(rt: &Runtime<Monitor>, msg: link::ToMonitor) {
    match msg {
        link::ToMonitor::Want(want) => {
            rt.with(|p, _| p.window_want = want);
            rt.nudge();
        }
        link::ToMonitor::Privacy { on } => rt.update(|s| s.privacy_mode = on),
        link::ToMonitor::Fan { id, config } => rt.update(|s| {
            s.fan_control.insert(id, config);
        }),
        link::ToMonitor::FanName { id, name } => rt.update(|s| {
            let name: String = name.trim().chars().take(40).collect();
            if name.is_empty() {
                s.fan_names.remove(&id);
            } else {
                s.fan_names.insert(id, name);
            }
        }),
        link::ToMonitor::CpuFan { id, on } => rt.update(|s| {
            s.cpu_fans.retain(|f| *f != id);
            if on {
                s.cpu_fans.push(id);
            }
        }),
        link::ToMonitor::Boost { id } => {
            rt.with(|p, _| p.fan_boosts.push(id));
            rt.nudge();
        }
        link::ToMonitor::Geometry { value } => rt.update(|s| s.window = value),
    }
}

/// Looks how far the setup is.
fn check_bridge(rt: &Runtime<Monitor>) {
    let steps = bridge::steps().iter().map(|s| (s.name(), setup::done(bridge::ID, s))).collect();
    let present = setup::present(bridge::ID);
    rt.with(|p, _| {
        p.bridge.steps = Some(steps);
        p.bridge.present = present;
    });
    rt.changed();
}

fn work(rt: Runtime<Monitor>) {
    take_over_old_file(&rt);
    check_bridge(&rt);
    {
        let rt = rt.clone();
        std::thread::spawn(move || window_server(rt));
    }
    loop {
        // While the program only serves its page, the worker looks after the page's orders only.
        while !rt.running() {
            do_orders(&rt);
            rt.sleep(Duration::from_secs(3600));
        }
        rt.with(|p, _| p.running = true);
        monitor::run(&rt);
        rt.with(|p, _| p.running = false);
        publish(&rt, Report::starting());
    }
}

fn main() {
    kit::texts!();
    // The window role: a second process of this exe, for the detail window (`window.rs`).
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|a| a == link::ARGUMENT) {
        app::init(&Monitor::META);
        window::run(args.get(2).cloned());
        return;
    }
    app::init(&Monitor::META);
    app::run(Monitor::new(), work);
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

    /// A settings file of the Python version's time (without fan curves and window) still reads.
    #[test]
    fn python_version_settings_read() {
        let old = r#"{"icon_interval": 5.0, "privacy_mode": true, "rust_version": true, "_toolbox": {"language": "de"}}"#;
        let s: Settings = serde_json::from_str(old).unwrap();
        assert_eq!((s.icon_interval, s.privacy_mode, s.fan_control.len(), s.window.as_str()), (5.0, true, 0, ""));
    }

    #[test]
    fn the_page_tells_the_state() {
        let mut m = monitor(false);
        let s = Settings::default();
        assert!(page(&m, &s).contains("Stopped"));
        assert!(page(&m, &s).contains("Checking the setup"));
        m.bridge.steps = Some(vec![("PawnIO".into(), Done::Yes), ("LibreHardwareMonitor".into(), Done::No)]);
        let text = page(&m, &s);
        assert!(text.contains("PawnIO: there") && text.contains("LibreHardwareMonitor: missing"), "{text}");
        m.running = true;
        m.report = Report { phase: Phase::Running, temps: "CPU 45 °C  ·  GPU –".into(), order: Some(vec!["gpu".into(), "cpu".into()]), power_summary: Some("now ≈ 120 W".into()), ..Report::starting() };
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
}
