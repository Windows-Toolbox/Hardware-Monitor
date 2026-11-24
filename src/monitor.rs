//! The monitor: it runs on the program's worker thread while the program is started. This exe itself has no admin
//! rights; the bridge (`bridge.rs`) has them and reads LibreHardwareMonitor. Economical in the background: CPU and
//! GPU open first (the taskbar has values within a second), the rest loads meanwhile; each round reads only the CPU
//! – the Nvidia temperature comes from `nvml.dll` directly – and the efficiency mode is on. While the detail window
//! is open, it also reads what the window shows and sends it there (`link.rs`); it controls the fans (`fans.rs`)
//! and logs the power (`energy.rs`, `plug.rs`).

use std::net::Ipv4Addr;
use std::time::{Duration, Instant, SystemTime};

use kit::app::{self, Runtime};
use kit::proto::{Health, Live};
use kit::setup;
use kit::t;
use serde_json::{Map, Value, json};

use crate::bridge::{self, Bridge};
use crate::energy::{self, EnergyLog, PlugLog};
use crate::fans::{self, Controller, Fans};
use crate::link::{self, ToWindow};
use crate::nvml::Nvml;
use crate::plug::{self, Plug};
use crate::sensors::{self, Reading};
use crate::{Monitor, Phase, Report, Settings};

/// Between two drive health readings while that tab is shown.
const HEALTH_EVERY: Duration = Duration::from_secs(10);
/// After the bridge could not be started or has gone: the next try.
const RETRY: Duration = Duration::from_secs(10);
/// While no plug was found: the next look.
const LOOK_AGAIN: Duration = Duration::from_secs(60);
/// CPU blue, GPU green – the stripes under the numbers in the taskbar, as the Python version's icons had them.
const STRIPE_CPU: u32 = 0x468cff;
const STRIPE_GPU: u32 = 0x76b900;

pub const ENERGY_FILE: &str = "hardware_monitor_energy.json";
pub const PLUG_FILE: &str = "hardware_monitor_plug.json";

/// The fans through the bridge.
struct Through<'a>(&'a Bridge);

impl Fans for Through<'_> {
    fn set(&mut self, ids: &[String], pct: f64) -> Result<(), String> {
        self.0.ask(json!({"cmd": "set_fan", "ids": ids, "pct": pct})).map(|_| ())
    }
    fn release(&mut self, ids: &[String]) -> Result<(), String> {
        self.0.ask(json!({"cmd": "release", "ids": ids})).map(|_| ())
    }
}

/// What the page and the tray see of the plug.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlugView {
    pub line: String,
    pub health: Health,
    pub watts: Option<f64>,
}

/// The plug while the monitor runs: found (or given), connected, read.
struct PlugState {
    plug: Option<Plug>,
    pub name: String,
    next_look: Option<Instant>,
    /// The address it was asked for ("" = found by itself).
    wanted: String,
}

impl PlugState {
    fn new() -> PlugState {
        PlugState { plug: None, name: String::new(), next_look: None, wanted: String::new() }
    }

    /// One reading – finding the plug first if needed.
    fn read(&mut self, s: &Settings) -> (Option<plug::Values>, PlugView) {
        let wanted = s.plug_address.trim().to_string();
        if wanted != self.wanted {
            *self = PlugState { wanted: wanted.clone(), ..PlugState::new() };
        }
        if self.plug.is_none() {
            if self.next_look.is_some_and(|t| Instant::now() < t) {
                return (None, PlugView { line: t!("plug.not_found"), health: Health::Warn, watts: None });
            }
            self.next_look = Some(Instant::now() + LOOK_AGAIN);
            let found = if wanted.is_empty() {
                plug::discover(Duration::from_secs(2)).map(|(ip, info)| (ip, Some(info)))
            } else {
                wanted.parse::<Ipv4Addr>().ok().map(|ip| (ip, None))
            };
            let Some((ip, info)) = found else {
                let line = if wanted.is_empty() { t!("plug.not_found") } else { t!("plug.bad_address", address = wanted) };
                return (None, PlugView { line, health: Health::Warn, watts: None });
            };
            let mut p = Plug::new(ip);
            let info = info.or_else(|| p.info().ok());
            self.name = info.map(|i| i.name()).unwrap_or_else(|| t!("plug.default_name"));
            app::log(&format!("smart plug: {} at {ip}", self.name));
            self.plug = Some(p);
        }
        let name = if s.plug_name.trim().is_empty() { self.name.clone() } else { s.plug_name.trim().to_string() };
        let p = self.plug.as_mut().expect("there");
        match p.values() {
            Ok(v) => {
                let line = t!("plug.reading", name = name, address = p.address, watts = energy::num(v.watts, 0), volts = energy::num(v.volts, 0), kwh = energy::num(v.kwh, 2));
                (Some(v.clone()), PlugView { line, health: if v.on { Health::Ok } else { Health::Warn }, watts: Some(v.watts) })
            }
            Err(e) => {
                let line = t!("plug.no_answer", name = name, address = p.address, why = e);
                // Found by itself: look again soon – its address may have changed.
                if wanted.is_empty() {
                    self.plug = None;
                    self.next_look = Some(Instant::now() + Duration::from_secs(10));
                }
                (None, PlugView { line, health: Health::Bad, watts: None })
            }
        }
    }
}

fn publish(rt: &Runtime<Monitor>, report: Report) {
    crate::publish(rt, report);
}

fn report(phase: Phase, message: String) -> Report {
    Report { phase, message, ..Report::starting() }
}

/// Whether the monitor should go on: the program is started.
fn wanted(rt: &Runtime<Monitor>) -> bool {
    rt.running()
}

/// Runs until the program is stopped.
pub fn run(rt: &Runtime<Monitor>) {
    publish(rt, Report::starting());
    while wanted(rt) {
        crate::do_orders(rt);
        if !setup::pending(bridge::ID, &bridge::steps()).is_empty() {
            publish(rt, report(Phase::NotSetUp, t!("message.bridge_not_set_up")));
            rt.sleep(Duration::from_secs(5));
            continue;
        }
        match Bridge::start() {
            Ok(b) => {
                if let Err(why) = session(rt, &b) {
                    app::log(&format!("the Rust monitor stopped reading: {why}"));
                    publish(rt, report(Phase::Failed, t!("message.bridge_lost", why = why)));
                    rt.sleep(RETRY);
                }
            }
            Err(why) => {
                publish(rt, report(Phase::Failed, why));
                rt.sleep(RETRY);
            }
        }
    }
    kit::process::efficiency_mode(false);
    rt.with(|p, _| p.fan_info.clear());
    // A detail window would show old values: it closes with the monitor.
    if let Some(window) = rt.with(|p, _| p.window.take()) {
        window.close();
    }
}

/// One connection to the bridge: open, then read round after round. `Err` when the bridge failed.
fn session(rt: &Runtime<Monitor>, b: &Bridge) -> Result<(), String> {
    let opened = b.ask(json!({"cmd": "open", "groups": sensors::FAST_GROUPS}))?;
    if opened["elevated"].as_bool() != Some(true) {
        app::log("the bridge runs without admin rights – CPU and motherboard values are missing");
    }
    let nvml = Nvml::open();
    let mut energy = EnergyLog::load(app::data_dir().join(ENERGY_FILE));
    let mut plug_log = PlugLog::load(app::data_dir().join(PLUG_FILE));
    let mut plug = PlugState::new();
    let mut controller = Controller::default();
    let (mut later, mut complete, mut battery) = (false, false, false);
    let mut present: Option<Vec<String>> = None;
    let mut drives: Option<Value> = None;
    let mut drives_read: Option<Instant> = None;
    let mut efficiency = None;
    let result = loop {
        if !wanted(rt) {
            break Ok(());
        }
        crate::do_orders(rt);
        let s = rt.with(|_, s| s.clone());
        let fan_config: &Map<String, Value> = &s.fan_control;
        let on = s.efficiency_mode != "off";
        if efficiency != Some(on) {
            kit::process::efficiency_mode(on);
            efficiency = Some(on);
        }

        // The GPU through LHM only if NVML cannot give the chosen sensor.
        let gpu_by_nvml = nvml.is_some() && (s.gpu_sensor == "auto" || s.gpu_sensor == "GPU Core");
        let mut types: Vec<&str> = sensors::types("cpu").to_vec();
        if !gpu_by_nvml {
            types.extend(sensors::types("gpu"));
        }
        if battery {
            types.extend(sensors::types("battery"));
        }
        // What the window shows is read too; "all" reads everything.
        let want = rt.with(|p, _| p.window_want.clone());
        let for_window = matches!(want.view.as_str(), "list" | "fans");
        let command = match (for_window, want.types()) {
            (true, None) => json!({"cmd": "read"}),
            (true, Some(more)) => {
                types.extend(more);
                types.sort();
                types.dedup();
                json!({"cmd": "read", "types": types})
            }
            _ => json!({"cmd": "read", "types": types}),
        };
        let reading = Reading::parse(&b.ask(command)?);
        if !later {
            // The taskbar has its values; now the rest – the bridge goes on answering meanwhile.
            b.ask(json!({"cmd": "open", "groups": sensors::LATER_GROUPS}))?;
            later = true;
        } else if !complete && !reading.opening {
            let all = Reading::parse(&b.ask(json!({"cmd": "read"}))?);
            controller.items = all.fans();
            battery = all.devices().iter().any(|d| d == "battery");
            present = Some(all.devices());
            app::log(&format!("controllable fans: {}", controller.items.iter().map(|i| format!("{} ({})", i.name, i.id)).collect::<Vec<_>>().join(", ")));
            complete = true;
        }

        let cpu = reading.temperature("cpu", &sensors::priority(&s.cpu_sensor, &sensors::CPU_SENSORS));
        let gpu = if gpu_by_nvml { nvml.as_ref().and_then(Nvml::temperature) } else { reading.temperature("gpu", &sensors::priority(&s.gpu_sensor, &sensors::GPU_SENSORS)) };
        let has_gpu = gpu_by_nvml || reading.has_temperature("gpu");

        // Power: measured CPU and GPU, the rest from the settings – or the battery's discharge rate.
        let now = Instant::now();
        let at = SystemTime::now();
        let gpu_power = if gpu_by_nvml { nvml.as_ref().and_then(Nvml::power) } else { reading.gpu_power() };
        let estimate = reading.battery_power().unwrap_or_else(|| energy::wall_watts(reading.cpu_power(), gpu_power, s.energy_other_watts as f64, s.energy_psu_efficiency as f64));
        if s.energy_logging {
            energy.add(estimate, now, at);
        } else {
            energy.pause();
        }
        let (values, plug_view) = if s.plug { plug.read(&s) } else { (None, PlugView::default()) };
        if let (Some(v), true) = (&values, s.energy_logging) {
            plug_log.add(v.kwh, at);
        }

        if controller.wanted(fan_config) {
            let hot = (s.cpu_hot_temp as f64, s.gpu_hot_temp as f64);
            controller.apply(&mut Through(b), fan_config, (cpu, gpu), hot, now);
        }

        let summary = summary(&s, estimate, plug_view.watts, &energy, &plug_log);
        let fan_info = controller.items.iter().map(|i| (i.name.clone(), controller.state.get(&i.id).cloned().unwrap_or_default())).collect();
        rt.with(|p, _| p.fan_info = fan_info);
        let plug_name = if s.plug_name.trim().is_empty() { plug.name.clone() } else { s.plug_name.trim().to_string() };
        let plug_watts = plug_view.watts;
        publish(rt, running_report(&s, cpu, has_gpu.then_some(gpu), summary, plug_view));

        // The window, if one is open: what it shows.
        if let Some(window) = rt.with(|p, _| p.window.clone()) {
            if want.view == "health" && drives_read.is_none_or(|t| t.elapsed() >= HEALTH_EVERY) {
                drives = b.ask(json!({"cmd": "drives"})).ok();
                drives_read = Some(Instant::now());
            }
            let look = rt.look();
            let price = crate::parse_price(&s.energy_price).unwrap_or(0.0);
            let settings = link::Settings {
                language: look.language,
                theme: look.theme,
                zoom: look.zoom,
                cpu_warn_temp: s.cpu_warn_temp,
                cpu_hot_temp: s.cpu_hot_temp,
                gpu_warn_temp: s.gpu_warn_temp,
                gpu_hot_temp: s.gpu_hot_temp,
                privacy_mode: s.privacy_mode,
                window: s.window.clone(),
                energy_logging: s.energy_logging,
                energy_other_watts: s.energy_other_watts,
                energy_psu_efficiency: s.energy_psu_efficiency,
                energy_price: price,
                energy_currency: s.energy_currency.trim().to_string(),
                plug: s.plug,
            };
            let temps = s.icon_order.iter().filter(|k| *k == "cpu" || has_gpu).map(|k| (k.clone(), if k == "cpu" { cpu } else { gpu })).collect();
            let fans = controller
                .items
                .iter()
                .map(|i| {
                    let state = controller.state.get(&i.id).cloned().unwrap_or_default();
                    link::Fan {
                        id: i.id.clone(),
                        kind: i.kind.clone(),
                        name: i.name.clone(),
                        control_ids: i.control_ids.clone(),
                        fan_ids: i.fan_ids.clone(),
                        minimum: i.minimum,
                        maximum: i.maximum,
                        software: state.software,
                        pct: state.pct,
                        note: state.note,
                        config: fan_config.get(&i.id).cloned(),
                    }
                })
                .collect();
            let power_view = want.view == "power";
            let state = link::State {
                settings,
                temps,
                hardware: for_window.then(|| reading.hardware.clone()),
                answers: want.clone(),
                loading: !complete,
                present: present.clone(),
                power: link::Power { estimate, cpu: reading.cpu_power(), gpu: gpu_power, battery: reading.battery_power(), plug: plug_watts, plug_name },
                hours: power_view.then(|| energy.hours.clone()),
                plug_hours: power_view.then(|| plug_log.hours.clone()),
                fans,
                drives: (want.view == "health").then(|| drives.clone()).flatten(),
            };
            let _ = window.send(&ToWindow::State(Box::new(state)));
        }

        // A "Delete all saved data" from the page while running: both logs.
        if rt.with(|p, _| std::mem::take(&mut p.energy_delete_asked)) {
            let ok = energy.delete_all() & plug_log.delete_all();
            app::log(if ok { "the power logs were deleted" } else { "the power logs could not be deleted" });
        }

        let open = rt.with(|p, _| p.window.is_some() && p.window_want.anything());
        let mut every = Duration::from_secs_f64(if open { s.popup_interval } else { s.icon_interval }.max(0.5));
        if controller.wanted(fan_config) {
            every = every.min(fans::INTERVAL);
        }
        rt.sleep(every);
    };
    // Every fan back before the connection closes (the bridge would do it too – this way the log says so).
    controller.release_all(&mut Through(b));
    energy.save();
    plug_log.save();
    result
}

/// The traffic light of a temperature.
fn level(value: Option<f64>, warn: u32, hot: u32) -> Health {
    match value {
        None => Health::Unknown,
        Some(v) if v >= hot as f64 => Health::Bad,
        Some(v) if v >= warn as f64 => Health::Warn,
        Some(_) => Health::Ok,
    }
}

/// `gpu`: `None` = this PC has no GPU temperature (no icon, nothing in the line).
fn running_report(s: &Settings, cpu: Option<f64>, gpu: Option<Option<f64>>, summary: String, plug: PlugView) -> Report {
    let shown = |v: Option<f64>| v.map_or("–".to_string(), |v| format!("{v:.0} °C"));
    let mut parts = vec![];
    let mut live = vec![];
    for kind in &s.icon_order {
        let (value, warn, hot, stripe) = match kind.as_str() {
            "cpu" => (Some(cpu), s.cpu_warn_temp, s.cpu_hot_temp, STRIPE_CPU),
            _ => (gpu, s.gpu_warn_temp, s.gpu_hot_temp, STRIPE_GPU),
        };
        let Some(value) = value else { continue };
        parts.push(format!("{} {}", kind.to_uppercase(), shown(value)));
        if s.taskbar_temps {
            let text = value.map_or("--".to_string(), |v| format!("{v:.0}"));
            live.push(Live { text, state: level(value, warn, hot), stripe: Some(stripe) });
        }
    }
    Report { phase: Phase::Running, message: t!("message.running"), temps: parts.join("  ·  "), order: Some(s.icon_order.clone()), power_summary: Some(summary), live, plug: (s.plug).then_some(plug) }
}

/// The power line on the page: now, today, this month, in total. With a plug its measured values (the whole desk),
/// beside the PC's estimate.
fn summary(s: &Settings, estimate: f64, plug: Option<f64>, energy: &EnergyLog, plug_log: &PlugLog) -> String {
    let mut parts = vec![];
    match plug {
        Some(desk) => {
            parts.push(t!("page.power.desk_now", watts = energy::num(desk, 0)));
            parts.push(t!("page.power.pc_now", watts = energy::num(estimate, 0)));
            parts.push(t!("page.power.others", watts = energy::num((desk - estimate).max(0.0), 0)));
        }
        None => parts.push(t!("page.power.now", watts = energy::num(estimate, 0))),
    }
    let hours = if s.plug && !plug_log.hours.is_empty() { &plug_log.hours } else { &energy.hours };
    let totals = energy::totals(hours, &energy::today());
    let price = crate::parse_price(&s.energy_price).unwrap_or(0.0);
    let cost = |wh: f64| format!("{} {}", energy::num(wh / 1000.0 * price, 2), s.energy_currency.trim()).trim().to_string();
    for (id, wh) in [("page.power.today", totals.today), ("page.power.month", totals.month), ("page.power.all", totals.all)] {
        if wh > 0.0 {
            parts.push(t!(id, energy = energy::fmt_kwh(wh), cost = cost(wh)));
        }
    }
    if let Some(since) = totals.since.filter(|_| totals.all > 0.0) {
        parts.push(t!("page.power.since", since = energy::fmt_date(&since)));
    }
    if totals.all == 0.0 {
        parts.push(if s.energy_logging { t!("page.power.nothing_yet") } else { t!("page.power.off") });
    }
    parts.join("  ·  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real thing on a PC that is set up: `cargo test real -- --ignored --nocapture`. Starts the bridge through
    /// its task, reads like the monitor does, looks for the plug – and sets no fan.
    #[test]
    #[ignore]
    fn real_bridge_and_plug() {
        kit::texts!();
        kit::i18n::set_language("en");
        let b = Bridge::start().expect("the bridge – is it set up?");
        let opened = b.ask(json!({"cmd": "open", "groups": sensors::FAST_GROUPS})).unwrap();
        assert_eq!(opened["elevated"].as_bool(), Some(true));
        let r = Reading::parse(&b.ask(json!({"cmd": "read", "types": sensors::types("cpu")})).unwrap());
        let cpu = r.temperature("cpu", &sensors::priority("auto", &sensors::CPU_SENSORS));
        println!("CPU {cpu:?} °C, package {:?} W", r.cpu_power());
        assert!(cpu.is_some_and(|t| t > 15.0), "a real CPU temperature needs admin rights");
        let nvml = Nvml::open();
        println!("GPU (NVML) {:?} °C, {:?} W", nvml.as_ref().and_then(Nvml::temperature), nvml.as_ref().and_then(Nvml::power));
        b.ask(json!({"cmd": "open", "groups": sensors::LATER_GROUPS})).unwrap();
        let started = Instant::now();
        let all = loop {
            let all = Reading::parse(&b.ask(json!({"cmd": "read"})).unwrap());
            if !all.opening || started.elapsed() > Duration::from_secs(30) {
                break all;
            }
            std::thread::sleep(Duration::from_millis(500));
        };
        println!("devices {:?} after {:.1} s", all.devices(), started.elapsed().as_secs_f64());
        for f in all.fans() {
            println!("fan {} {} – {:?} / {:?} – {}..{} %", f.kind, f.name, f.control_ids, f.fan_ids, f.minimum, f.maximum);
        }
        assert!(all.fans().iter().any(|f| f.kind == "gpu"));
        let drives = b.ask(json!({"cmd": "drives"})).unwrap();
        println!("{} drives", drives["drives"].as_array().map_or(0, Vec::len));
        drop(b);
        let found = plug::discover(Duration::from_secs(2));
        println!("plug: {found:?}");
        if let Some((ip, info)) = found {
            let mut p = Plug::new(ip);
            println!("{} – {:?} – {:?}", info.name(), p.values(), p.info());
        }
    }

    #[test]
    fn the_line_and_the_taskbar() {
        kit::texts!();
        kit::i18n::set_language("en");
        let s = Settings { taskbar_temps: true, icon_order: vec!["gpu".into(), "cpu".into()], ..Settings::default() };
        let r = running_report(&s, Some(52.4), Some(Some(84.0)), String::new(), PlugView::default());
        assert_eq!(r.temps, "GPU 84 °C  ·  CPU 52 °C");
        assert_eq!(r.live.iter().map(|l| (l.text.as_str(), l.state, l.stripe)).collect::<Vec<_>>(), vec![("84", Health::Bad, Some(STRIPE_GPU)), ("52", Health::Ok, Some(STRIPE_CPU))]);
        // Without a GPU temperature (Intel onboard graphics): only the CPU; off: no live displays.
        let r = running_report(&Settings::default(), None, None, String::new(), PlugView::default());
        assert_eq!((r.temps.as_str(), r.live.len()), ("CPU –", 0));
    }
}
