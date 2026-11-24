//! Fan control: curves (temperature → %) for the graphics card's fans and the motherboard's fan headers – like the
//! fan curve in MSI Afterburner. Ported from the Python monitor of version 2, with the same rules and the same
//! settings (`fan_control`, now in `data\settings.json`), so its curves carried over.
//!
//! One item per graphics card (all its fans follow one curve) and one per motherboard fan header. Per item: mode
//! "auto" (not controlled – the default), "curve" or "fixed", the source temperature ("gpu", "cpu" or "max") and
//! the curve points [[°C, %], …] or the fixed %.
//!
//! Safety:
//!   - Only items switched to "curve" or "fixed" are touched; everything else stays as without this program.
//!   - At or above the red limit of the traffic light (gpu_hot_temp / cpu_hot_temp) a controlled fan runs at 100 %.
//!   - No temperature (sensor gone) or an error → the fan is handed back to the card or the BIOS.
//!   - When the monitor ends – or crashes – the bridge loses its connection and hands every fan back itself.
//!   - Hysteresis: a fan only slows down again once the temperature has dropped HYSTERESIS °C below the value at
//!     which it was last sped up – otherwise it would hum up and down with every degree.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

/// Between two adjustments while a fan is controlled (the icons are measured that often too).
pub const INTERVAL: Duration = Duration::from_secs(2);
const HYSTERESIS: f64 = 3.0;
/// Set the speed again anyway – after sleep the driver may have taken over again.
const REAPPLY: Duration = Duration::from_secs(30);
pub const MAX_POINTS: usize = 8;

/// A fan (or all fans of a graphics card) that can be controlled.
#[derive(Debug, Clone, PartialEq)]
pub struct FanItem {
    /// LHM identifier: of the graphics card or of the motherboard's control sensor.
    pub id: String,
    /// "gpu" or "case".
    pub kind: String,
    pub name: String,
    /// The Control sensors the bridge sets.
    pub control_ids: Vec<String>,
    /// The speed sensors (RPM, for the window).
    pub fan_ids: Vec<String>,
    pub minimum: f64,
    pub maximum: f64,
}

/// One item's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub mode: String,
    pub source: String,
    pub curve: Vec<[i64; 2]>,
    pub fixed: i64,
}

impl Config {
    /// Quieter than most factory curves, 100 % before it gets critical.
    pub fn default_for(kind: &str) -> Config {
        let curve = if kind == "gpu" { vec![[40, 30], [60, 40], [70, 55], [78, 75], [83, 100]] } else { vec![[40, 25], [60, 35], [70, 50], [80, 75], [88, 100]] };
        Config { mode: "auto".into(), source: if kind == "gpu" { "gpu" } else { "cpu" }.into(), curve, fixed: 50 }
    }

    /// The item's entry of `fan_control` over its defaults; what is invalid keeps the default (`clean_config`).
    pub fn from_settings(kind: &str, entry: Option<&Value>) -> Config {
        let mut c = Config::default_for(kind);
        let Some(e) = entry.and_then(Value::as_object) else { return c };
        if let Some(m) = e.get("mode").and_then(Value::as_str).filter(|m| ["auto", "curve", "fixed"].contains(m)) {
            c.mode = m.into();
        }
        if let Some(s) = e.get("source").and_then(Value::as_str).filter(|s| ["gpu", "cpu", "max"].contains(s)) {
            c.source = s.into();
        }
        if let Some(points) = e.get("curve").and_then(Value::as_array) {
            let parsed: Option<Vec<[i64; 2]>> = points
                .iter()
                .map(|p| {
                    let p = p.as_array()?;
                    let (t, pct) = (p.first()?.as_i64()?, p.get(1)?.as_i64()?);
                    (p.len() == 2 && (0..=110).contains(&t) && (0..=100).contains(&pct)).then_some([t, pct])
                })
                .collect();
            if let Some(mut points) = parsed.filter(|p| (2..=MAX_POINTS).contains(&p.len())) {
                points.sort();
                c.curve = points;
            }
        }
        if let Some(f) = e.get("fixed").and_then(Value::as_i64).filter(|f| (0..=100).contains(f)) {
            c.fixed = f;
        }
        c
    }
}

/// % of the curve at `temp`: linear between the points, flat before the first and after the last one.
pub fn curve_value(points: &[[i64; 2]], temp: f64) -> f64 {
    let mut p = points.to_vec();
    p.sort();
    let Some(first) = p.first() else { return 100.0 };
    if temp <= first[0] as f64 {
        return first[1] as f64;
    }
    for w in p.windows(2) {
        let ([t0, p0], [t1, p1]) = (w[0], w[1]);
        if temp <= t1 as f64 {
            return if t1 == t0 { p1 as f64 } else { p0 as f64 + (p1 - p0) as f64 * (temp - t0 as f64) / (t1 - t0) as f64 };
        }
    }
    p[p.len() - 1][1] as f64
}

/// What one item is doing – for the window and the page.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct State {
    /// We drive it right now.
    pub software: bool,
    pub pct: Option<f64>,
    /// The temperature at which `pct` was chosen – the reference of the hysteresis.
    temp: Option<f64>,
    time: Option<Instant>,
    /// "hot", "no_temperature", "error".
    pub note: Option<String>,
}

/// What the controller asks the bridge to do.
pub trait Fans {
    fn set(&mut self, ids: &[String], pct: f64) -> Result<(), String>;
    fn release(&mut self, ids: &[String]) -> Result<(), String>;
}

/// Thresholds of the traffic light: (cpu_hot_temp, gpu_hot_temp).
pub type Hot = (f64, f64);

#[derive(Default)]
pub struct Controller {
    pub items: Vec<FanItem>,
    pub state: HashMap<String, State>,
}

impl Controller {
    /// Whether any fan should be controlled (or still is) – then the monitor adjusts every INTERVAL.
    pub fn wanted(&self, config: &Map<String, Value>) -> bool {
        self.state.values().any(|s| s.software) || self.items.iter().any(|i| Config::from_settings(&i.kind, config.get(&i.id)).mode != "auto")
    }

    /// Adjusts all fans. `temps`: (cpu, gpu) – `None` = unknown.
    pub fn apply(&mut self, fans: &mut dyn Fans, config: &Map<String, Value>, temps: (Option<f64>, Option<f64>), hot: Hot, now: Instant) {
        for item in self.items.clone() {
            let c = Config::from_settings(&item.kind, config.get(&item.id));
            let state = self.state.entry(item.id.clone()).or_default();
            if c.mode == "auto" {
                if state.software {
                    release(fans, &item, state);
                }
                state.note = None;
                continue;
            }
            let (temp, is_hot) = source_temp(&c.source, temps, hot);
            let Some(temp) = temp else {
                if state.software {
                    release(fans, &item, state);
                }
                state.note = Some("no_temperature".into());
                continue;
            };
            let target = if is_hot {
                state.note = Some("hot".into());
                100.0
            } else {
                state.note = None;
                if c.mode == "curve" { curve_value(&c.curve, temp) } else { c.fixed as f64 }
            };
            let mut target = target.clamp(item.minimum, item.maximum.max(item.minimum)).round();
            let last = state.pct;
            if let (Some(last), Some(reference)) = (last, state.temp)
                && target < last
                && temp > reference - HYSTERESIS
            {
                target = last; // not slower yet
            }
            let changed = Some(target) != last;
            let due = state.time.is_none_or(|t| now.duration_since(t) >= REAPPLY);
            if changed || !state.software || due {
                let reference = if changed || state.temp.is_none() { temp } else { state.temp.unwrap_or(temp) };
                match fans.set(&item.control_ids, target) {
                    Ok(()) => {
                        if !state.software {
                            kit::app::log(&format!("fan control: {} taken over ({target} % at {temp:.0} °C)", item.name));
                        }
                        *state = State { software: true, pct: Some(target), temp: Some(reference), time: Some(now), note: state.note.clone() };
                    }
                    Err(e) => {
                        kit::app::log(&format!("fan {} could not be set to {target} %: {e}", item.name));
                        release(fans, &item, state);
                        state.note = Some("error".into());
                    }
                }
            }
        }
    }

    /// Every controlled fan back – when the control is switched off or the monitor stops.
    pub fn release_all(&mut self, fans: &mut dyn Fans) {
        for item in self.items.clone() {
            if let Some(state) = self.state.get_mut(&item.id).filter(|s| s.software) {
                release(fans, &item, state);
            }
        }
    }
}

fn release(fans: &mut dyn Fans, item: &FanItem, state: &mut State) {
    match fans.release(&item.control_ids) {
        Ok(()) => kit::app::log(&format!("fan control: {} handed back", item.name)),
        Err(e) => kit::app::log(&format!("fan {} could not be handed back: {e}", item.name)),
    }
    *state = State { note: state.note.clone(), ..State::default() };
}

/// (temperature, at or above the red limit?) for "gpu", "cpu" or "max".
fn source_temp(source: &str, (cpu, gpu): (Option<f64>, Option<f64>), (cpu_hot, gpu_hot): Hot) -> (Option<f64>, bool) {
    let hot_cpu = cpu.is_some_and(|t| t >= cpu_hot);
    let hot_gpu = gpu.is_some_and(|t| t >= gpu_hot);
    match source {
        "max" => (cpu.into_iter().chain(gpu).reduce(f64::max), hot_cpu || hot_gpu),
        "cpu" => (cpu, hot_cpu),
        _ => (gpu, hot_gpu),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Default)]
    struct Fake {
        set: Vec<(Vec<String>, f64)>,
        released: Vec<Vec<String>>,
        fail: bool,
    }

    impl Fans for Fake {
        fn set(&mut self, ids: &[String], pct: f64) -> Result<(), String> {
            if self.fail {
                return Err("no".into());
            }
            self.set.push((ids.to_vec(), pct));
            Ok(())
        }
        fn release(&mut self, ids: &[String]) -> Result<(), String> {
            self.released.push(ids.to_vec());
            Ok(())
        }
    }

    fn gpu() -> FanItem {
        FanItem { id: "/gpu-nvidia/0".into(), kind: "gpu".into(), name: "RTX".into(), control_ids: vec!["c1".into(), "c2".into()], fan_ids: vec![], minimum: 30.0, maximum: 100.0 }
    }

    fn config(entry: Value) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert("/gpu-nvidia/0".into(), entry);
        m
    }

    #[test]
    fn the_curve() {
        let c = Config::default_for("gpu").curve;
        assert_eq!(curve_value(&c, 20.0), 30.0);
        assert_eq!(curve_value(&c, 65.0), 47.5);
        assert_eq!(curve_value(&c, 95.0), 100.0);
        assert_eq!(curve_value(&[[50, 20], [50, 80]], 50.0), 20.0);
    }

    #[test]
    fn invalid_settings_keep_the_defaults() {
        let c = Config::from_settings("gpu", Some(&json!({"mode": "loud", "source": "cpu", "curve": [[30, 200], [60, 50]], "fixed": 101})));
        assert_eq!((c.mode.as_str(), c.source.as_str(), c.curve.clone(), c.fixed), ("auto", "cpu", Config::default_for("gpu").curve, 50));
        let c = Config::from_settings("case", Some(&json!({"mode": "curve", "curve": [[70, 60], [30, 20]]})));
        assert_eq!((c.source.as_str(), c.curve.clone()), ("cpu", vec![[30, 20], [70, 60]]), "sorted, the kind's default source");
    }

    #[test]
    fn follows_the_curve_with_hysteresis() {
        let mut c = Controller { items: vec![gpu()], ..Default::default() };
        let mut fake = Fake::default();
        let cfg = config(json!({"mode": "curve", "source": "gpu", "curve": [[40, 30], [80, 70]]}));
        let t0 = Instant::now();
        let hot = (90.0, 83.0);
        assert!(c.wanted(&cfg));
        c.apply(&mut fake, &cfg, (None, Some(60.0)), hot, t0);
        assert_eq!(fake.set.last().unwrap().1, 50.0);
        // 2 °C cooler: not slower yet.
        c.apply(&mut fake, &cfg, (None, Some(58.0)), hot, t0 + INTERVAL);
        assert_eq!(fake.set.len(), 1, "nothing sent – the same speed");
        // 4 °C cooler: slower.
        c.apply(&mut fake, &cfg, (None, Some(56.0)), hot, t0 + INTERVAL * 2);
        assert_eq!(fake.set.last().unwrap().1, 46.0);
        // At the red limit: 100 %.
        c.apply(&mut fake, &cfg, (None, Some(83.0)), hot, t0 + INTERVAL * 3);
        assert_eq!((fake.set.last().unwrap().1, c.state["/gpu-nvidia/0"].note.as_deref()), (100.0, Some("hot")));
        // The sensor is gone: handed back.
        c.apply(&mut fake, &cfg, (None, None), hot, t0 + INTERVAL * 4);
        assert_eq!(fake.released.len(), 1);
        assert!(!c.state["/gpu-nvidia/0"].software);
        // Back to auto: nothing more to do.
        c.apply(&mut fake, &config(json!({"mode": "auto"})), (None, Some(60.0)), hot, t0 + INTERVAL * 5);
        assert!(!c.wanted(&config(json!({"mode": "auto"}))));
    }

    #[test]
    fn minimum_reapply_and_errors() {
        let mut c = Controller { items: vec![gpu()], ..Default::default() };
        let mut fake = Fake::default();
        let cfg = config(json!({"mode": "fixed", "fixed": 10}));
        let t0 = Instant::now();
        c.apply(&mut fake, &cfg, (None, Some(50.0)), (90.0, 83.0), t0);
        assert_eq!(fake.set[0].1, 30.0, "never below what the card allows");
        c.apply(&mut fake, &cfg, (None, Some(50.0)), (90.0, 83.0), t0 + REAPPLY);
        assert_eq!(fake.set.len(), 2, "set again after a while");
        c.release_all(&mut fake);
        assert_eq!(fake.released.len(), 1);
        let mut broken = Fake { fail: true, ..Default::default() };
        c.apply(&mut broken, &cfg, (None, Some(50.0)), (90.0, 83.0), t0);
        assert_eq!(c.state["/gpu-nvidia/0"].note.as_deref(), Some("error"));
        assert_eq!(broken.released.len(), 1, "an error hands the fan back");
    }
}
