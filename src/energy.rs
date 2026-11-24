//! Power consumption: estimate, log, add up – ported from `python\energy.py`, in the same file format, so the log
//! of the Python monitor goes on.
//!
//! Most power supplies cannot report what they draw, so the PC's power at the wall socket is estimated:
//!
//!     wall power ≈ (CPU package power + graphics card power + other components) ÷ power supply efficiency
//!
//! A laptop on battery measures it itself (discharge rate). With a smart plug (`plug.rs`) the power is *measured*
//! – for everything on that plug (the whole desk), which is logged on its own.
//!
//! The estimate's log: every measurement adds watts × seconds to the hour it falls into –
//! `data\hardware_monitor_energy.json`: {"version": 1, "hours": {"2026-11-06T14": [Wh, measured seconds], …}}.
//! The plug's log, `data\hardware_monitor_plug.json`, is made from the plug's own counter (kWh since it was set
//! up): the difference between two readings is exact, whatever happened in between – while the monitor did not run,
//! the plug counted on, and that time's energy is spread evenly over its hours.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const SAVE_EVERY: Duration = Duration::from_secs(300);
/// A longer gap between two measurements (sleep, a hang) is not counted.
const MAX_GAP: f64 = 30.0;

pub type Hours = BTreeMap<String, [f64; 2]>;

/// "YYYY-MM-DDTHH" of a moment, in local time.
pub fn hour_key(t: SystemTime) -> String {
    let (y, mo, d, h) = local(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}")
}

/// (year, month, day, hour) in local time.
pub fn local(t: SystemTime) -> (u16, u16, u16, u16) {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    let since = t.duration_since(UNIX_EPOCH).unwrap_or_default();
    // FILETIME: 100 ns steps since 1601.
    let ticks = since.as_nanos() as u64 / 100 + 116_444_736_000_000_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    unsafe {
        let (mut utc, mut here): (SYSTEMTIME, SYSTEMTIME) = (std::mem::zeroed(), std::mem::zeroed());
        FileTimeToSystemTime(&ft, &mut utc);
        SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut here);
        (here.wYear, here.wMonth, here.wDay, here.wHour)
    }
}

fn read_hours(v: &Value) -> Hours {
    let mut hours = Hours::new();
    if let Some(map) = v["hours"].as_object() {
        for (k, e) in map {
            if let (Some(wh), Some(s)) = (e.get(0).and_then(Value::as_f64), e.get(1).and_then(Value::as_f64)) {
                hours.insert(k.clone(), [wh, s]);
            }
        }
    }
    hours
}

fn write(file: &PathBuf, v: &Value) {
    let tmp = file.with_extension("tmp");
    let ok = std::fs::write(&tmp, v.to_string()).and_then(|()| std::fs::rename(&tmp, file));
    if let Err(e) = ok {
        kit::app::log(&format!("{} could not be saved: {e}", file.display()));
    }
}

/// The estimate's log.
pub struct EnergyLog {
    file: PathBuf,
    pub hours: Hours,
    last: Option<Instant>,
    saved: Instant,
    dirty: bool,
}

impl EnergyLog {
    pub fn load(file: PathBuf) -> EnergyLog {
        let hours = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).map(|v| read_hours(&v)).unwrap_or_default();
        EnergyLog { file, hours, last: None, saved: Instant::now(), dirty: false }
    }

    /// One measurement: watts since the last one (at most MAX_GAP back).
    pub fn add(&mut self, watts: f64, now: Instant, at: SystemTime) {
        let last = self.last.replace(now);
        let Some(last) = last else { return };
        let seconds = now.duration_since(last).as_secs_f64();
        if seconds <= 0.0 || seconds > MAX_GAP {
            return;
        }
        let e = self.hours.entry(hour_key(at)).or_insert([0.0, 0.0]);
        e[0] += watts * seconds / 3600.0;
        e[1] += seconds;
        self.dirty = true;
        if now.duration_since(self.saved) >= SAVE_EVERY {
            self.save();
        }
    }

    /// Logging switched off: the next measurement starts a new interval.
    pub fn pause(&mut self) {
        self.last = None;
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.saved = Instant::now();
        write(&self.file, &json!({"version": 1, "hours": self.hours}));
    }

    /// Deletes everything logged – in memory and the file.
    pub fn delete_all(&mut self) -> bool {
        self.hours.clear();
        self.dirty = false;
        self.last = None;
        !self.file.exists() || std::fs::remove_file(&self.file).is_ok()
    }
}

/// The plug's log, from its counter.
pub struct PlugLog {
    file: PathBuf,
    pub hours: Hours,
    /// The last reading: (unix seconds, counter in kWh).
    last: Option<(f64, f64)>,
    saved: Instant,
    dirty: bool,
}

impl PlugLog {
    pub fn load(file: PathBuf) -> PlugLog {
        let v = std::fs::read_to_string(&file).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or_default();
        let last = v["last"].as_array().and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?)));
        PlugLog { file, hours: read_hours(&v), last, saved: Instant::now(), dirty: false }
    }

    /// A reading of the counter at `at`. The energy since the last one goes into the hours it was used in; only
    /// time with readings at most MAX_GAP apart counts as "measured".
    pub fn add(&mut self, kwh: f64, at: SystemTime) {
        let now = at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64();
        let last = self.last.replace((now, kwh));
        let Some((then, before)) = last else {
            self.dirty = true;
            return;
        };
        let wh = (kwh - before) * 1000.0;
        // A counter that went back (the plug was reset) or a clock that did: start again from here.
        if wh < 0.0 || now <= then {
            self.dirty = true;
            return;
        }
        let measured = now - then <= MAX_GAP;
        // Spread over the hours from `then` to `now`, cut at every full hour.
        let mut t = then;
        while t < now {
            let next = ((t / 3600.0).floor() + 1.0) * 3600.0;
            let end = next.min(now);
            let share = (end - t) / (now - then);
            let e = self.hours.entry(hour_key(UNIX_EPOCH + Duration::from_secs_f64(t))).or_insert([0.0, 0.0]);
            e[0] += wh * share;
            if measured {
                e[1] += end - t;
            }
            t = end;
        }
        self.dirty = true;
        if self.saved.elapsed() >= SAVE_EVERY {
            self.save();
        }
    }

    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        self.saved = Instant::now();
        let last = self.last.map(|(t, k)| json!([t, k])).unwrap_or(Value::Null);
        write(&self.file, &json!({"version": 1, "hours": self.hours, "last": last}));
    }

    pub fn delete_all(&mut self) -> bool {
        self.hours.clear();
        self.dirty = self.last.is_some();
        !self.file.exists() || std::fs::remove_file(&self.file).is_ok()
    }
}

/// Estimated power at the wall from the measured CPU and GPU power (unknown counts as 0).
pub fn wall_watts(cpu: Option<f64>, gpu: Option<f64>, other: f64, efficiency_pct: f64) -> f64 {
    (cpu.unwrap_or(0.0) + gpu.unwrap_or(0.0) + other) / (efficiency_pct / 100.0)
}

/// Today, this month, in total (Wh) and the first day – of the hours given. `today` = "YYYY-MM-DD".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Totals {
    pub today: f64,
    pub month: f64,
    pub all: f64,
    pub since: Option<String>,
}

pub fn totals(hours: &Hours, today: &str) -> Totals {
    let month = &today[..7.min(today.len())];
    let mut t = Totals { since: hours.keys().next().map(|k| k[..10.min(k.len())].to_string()), ..Default::default() };
    for (k, [wh, _]) in hours {
        if k.starts_with(today) {
            t.today += wh;
        }
        if k.starts_with(month) {
            t.month += wh;
        }
        t.all += wh;
    }
    t
}

pub fn today() -> String {
    let (y, m, d, _) = local(SystemTime::now());
    format!("{y:04}-{m:02}-{d:02}")
}

/// A number with the decimal separator of the language.
pub fn num(value: f64, decimals: usize) -> String {
    let text = format!("{value:.decimals$}");
    if kit::i18n::language() == "de" { text.replace('.', ",") } else { text }
}

/// 1234 Wh → "1.23 kWh".
pub fn fmt_kwh(wh: f64) -> String {
    let kwh = wh / 1000.0;
    format!("{} kWh", num(kwh, if kwh < 10.0 { 2 } else if kwh < 100.0 { 1 } else { 0 }))
}

/// "2026-11-06" → "06.11.2026" in German, as it is otherwise.
pub fn fmt_date(day: &str) -> String {
    if kit::i18n::language() == "de" && day.len() == 10 { format!("{}.{}.{}", &day[8..10], &day[5..7], &day[..4]) } else { day.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hm-energy-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn the_estimate() {
        assert!((wall_watts(Some(60.0), Some(30.0), 40.0, 87.0) - 149.425).abs() < 0.01);
        assert_eq!(wall_watts(None, None, 0.0, 100.0), 0.0);
    }

    #[test]
    fn the_python_file_is_read_and_written_alike() {
        let dir = temp("log");
        let file = dir.join("hardware_monitor_energy.json");
        std::fs::write(&file, r#"{"version":1,"hours":{"2026-11-06T14":[12.5,3600.0],"2026-11-07T01":[1,60]}}"#).unwrap();
        let mut log = EnergyLog::load(file.clone());
        assert_eq!(log.hours.len(), 2);
        let (t0, at) = (Instant::now(), SystemTime::now());
        log.add(100.0, t0, at);
        log.add(100.0, t0 + Duration::from_secs(36), at);
        assert_eq!(log.hours.len(), 2, "a gap of more than 30 s is not counted");
        log.add(100.0, t0 + Duration::from_secs(54), at);
        let key = hour_key(at);
        assert!((log.hours[&key][0] - 0.5).abs() < 1e-9 && log.hours[&key][1] == 18.0);
        log.dirty = true;
        log.save();
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!(back["hours"]["2026-11-06T14"][0].as_f64(), Some(12.5));
        let t = totals(&log.hours, "2026-11-06");
        assert_eq!((t.today, t.since.as_deref()), (12.5, Some("2026-11-06")));
        assert!(log.delete_all() && !file.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_plug_counter_is_spread_over_its_hours() {
        let dir = temp("plug");
        let file = dir.join("hardware_monitor_plug.json");
        let mut log = PlugLog::load(file.clone());
        // A full hour boundary, then 3 h of absence: 0.3 kWh counted meanwhile.
        let base = UNIX_EPOCH + Duration::from_secs(1_790_000_000 / 3600 * 3600 + 1800);
        log.add(63.000, base);
        log.add(63.001, base + Duration::from_secs(10));
        assert!((log.hours.values().map(|h| h[0]).sum::<f64>() - 1.0).abs() < 1e-6);
        assert_eq!(log.hours.values().map(|h| h[1]).sum::<f64>(), 10.0, "10 s measured");
        log.add(63.301, base + Duration::from_secs(10 + 3 * 3600));
        let total: f64 = log.hours.values().map(|h| h[0]).sum();
        assert!((total - 301.0).abs() < 1e-6, "every Wh of the counter, {total}");
        assert_eq!(log.hours.len(), 4, "the half hour, two full hours, the rest");
        assert_eq!(log.hours.values().map(|h| h[1]).sum::<f64>(), 10.0, "the absence does not count as measured");
        // A reset plug: no negative energy.
        log.add(0.5, base + Duration::from_secs(20 + 3 * 3600));
        assert!((log.hours.values().map(|h| h[0]).sum::<f64>() - 301.0).abs() < 1e-6);
        log.save();
        let again = PlugLog::load(file);
        assert_eq!((again.hours.len(), again.last.map(|l| l.1)), (4, Some(0.5)), "the last reading is kept for the next start");
        std::fs::remove_dir_all(&dir).ok();
    }
}
