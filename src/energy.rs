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
    let (y, mo, d, h, _) = local_minute(t);
    (y, mo, d, h)
}

/// (year, month, day, hour, minute) in local time.
pub fn local_minute(t: SystemTime) -> (u16, u16, u16, u16, u16) {
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
        (here.wYear, here.wMonth, here.wDay, here.wHour, here.wMinute)
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

// ---------- The chart of the window's Power tab (python\energy.py: buckets, fmt_energy, fmt_duration) ----------

/// The three views of the chart.
pub const VIEWS: [&str; 3] = ["day", "month", "year"];

/// A moment of local time to the hour: (year, month, day, hour).
pub type LocalHour = (i32, u32, u32, u32);

/// Days since 1970-01-01 of a date (proleptic Gregorian calendar).
fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i32, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    ((yoe + era * 400 + if m <= 2 { 1 } else { 0 }) as i32, m, d)
}

/// 0 = Monday.
pub fn weekday(y: i32, m: u32, d: u32) -> usize {
    (days_from_civil(y, m, d) + 3).rem_euclid(7) as usize
}

/// One bar: where it starts, Wh, measured seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Bucket {
    pub start: LocalHour,
    pub wh: f64,
    pub seconds: f64,
}

/// The bars of a view, oldest first, empty ones included: "day" = the last 24 hours, "month" = the last 30 days,
/// "year" = the last 12 months – up to and including `now`.
pub fn buckets(hours: &Hours, view: &str, now: LocalHour) -> Vec<Bucket> {
    let (y, m, d, h) = now;
    let today = days_from_civil(y, m, d);
    let starts: Vec<LocalHour> = match view {
        "day" => (0..24)
            .rev()
            .map(|i| {
                let hour = today * 24 + h as i64 - i;
                let (y, m, d) = civil_from_days(hour.div_euclid(24));
                (y, m, d, hour.rem_euclid(24) as u32)
            })
            .collect(),
        "month" => (0..30)
            .rev()
            .map(|i| {
                let (y, m, d) = civil_from_days(today - i);
                (y, m, d, 0)
            })
            .collect(),
        _ => (0..12)
            .rev()
            .map(|i| {
                let months = y * 12 + m as i32 - 1 - i;
                (months.div_euclid(12), months.rem_euclid(12) as u32 + 1, 1, 0)
            })
            .collect(),
    };
    // Which bar a log key ("YYYY-MM-DDTHH") belongs to.
    let bar_of = |key: &str| -> Option<LocalHour> {
        let num = |a: usize, b: usize| key.get(a..b)?.parse::<u32>().ok();
        let (ky, km, kd, kh) = (num(0, 4)? as i32, num(5, 7)?, num(8, 10)?, num(11, 13)?);
        Some(match view {
            "day" => (ky, km, kd, kh),
            "month" => (ky, km, kd, 0),
            _ => (ky, km, 1, 0),
        })
    };
    let mut sums: BTreeMap<LocalHour, [f64; 2]> = BTreeMap::new();
    for (k, [wh, s]) in hours {
        if let Some(bar) = bar_of(k) {
            let e = sums.entry(bar).or_insert([0.0, 0.0]);
            e[0] += wh;
            e[1] += s;
        }
    }
    starts
        .into_iter()
        .map(|start| {
            let [wh, seconds] = sums.get(&start).copied().unwrap_or([0.0, 0.0]);
            Bucket { start, wh, seconds }
        })
        .collect()
}

/// Now, to the hour.
pub fn now_hour() -> LocalHour {
    let (y, m, d, h) = local(SystemTime::now());
    (y as i32, m as u32, d as u32, h as u32)
}

/// 1234 Wh → "1.23 kWh"; `unit` "Wh" → "1234 Wh".
pub fn fmt_energy(wh: f64, unit: &str) -> String {
    if unit == "Wh" { format!("{} Wh", num(wh, 0)) } else { fmt_kwh(wh) }
}

/// 3725 → "1 h 2 min", 200000 → "2 d 7 h".
pub fn fmt_duration(seconds: f64) -> String {
    let minutes = (seconds.max(0.0) / 60.0) as u64;
    let (days, hours, minutes) = (minutes / 1440, minutes / 60 % 24, minutes % 60);
    if days > 0 {
        format!("{days} d {hours} h")
    } else if hours > 0 {
        format!("{hours} h {minutes} min")
    } else {
        format!("{minutes} min")
    }
}

/// How long Windows has been running – "Up time" in Task Manager.
pub fn uptime_seconds() -> f64 {
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount64() as f64 / 1000.0 }
}

/// A round step for the axis (1, 2, 2.5 or 5 × 10^n), so that about 3 gridlines fit up to `value`.
pub fn nice_step(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let raw = value / 3.0;
    let power = 10f64.powf(raw.log10().floor());
    for factor in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if raw <= factor * power {
            return factor * power;
        }
    }
    10.0 * power
}

const DAYS_EN: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const DAYS_DE: [&str; 7] = ["Mo", "Di", "Mi", "Do", "Fr", "Sa", "So"];
const MONTHS_EN: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const MONTHS_DE: [&str; 12] = ["Jan", "Feb", "Mär", "Apr", "Mai", "Jun", "Jul", "Aug", "Sep", "Okt", "Nov", "Dez"];

pub fn month_name(m: u32) -> &'static str {
    let names = if kit::i18n::language() == "de" { &MONTHS_DE } else { &MONTHS_EN };
    names[(m as usize + 11) % 12]
}

/// "6 Nov" / "6. Nov".
pub fn day_month(d: u32, m: u32) -> String {
    if kit::i18n::language() == "de" { format!("{d}. {}", month_name(m)) } else { format!("{d} {}", month_name(m)) }
}

/// A bar's period: "Thu 6 Nov, 14:00–15:00" / "Thu 6 Nov" / "Nov 2026".
pub fn period(start: LocalHour, view: &str) -> String {
    let (y, m, d, h) = start;
    let days = if kit::i18n::language() == "de" { &DAYS_DE } else { &DAYS_EN };
    let day = format!("{} {}", days[weekday(y, m, d)], day_month(d, m));
    match view {
        "day" => format!("{day}, {h:02}:00–{:02}:00", (h + 1) % 24),
        "month" => day,
        _ => format!("{} {y}", month_name(m)),
    }
}

/// The label under a bar – not every bar has one.
pub fn x_label(start: LocalHour, i: usize, count: usize, view: &str) -> Option<String> {
    let (_, m, d, h) = start;
    match view {
        "day" => (h % 6 == 0).then(|| format!("{h:02}:00")),
        "month" => ((count - 1 - i) % 7 == 0).then(|| day_month(d, m)),
        _ => Some(month_name(m).to_string()),
    }
}

/// "HH:MM" of a moment in local time.
pub fn clock(t: SystemTime) -> String {
    let (_, _, _, h, mi) = local_minute(t);
    format!("{h:02}:{mi:02}")
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

    #[test]
    fn dates_back_and_forth() {
        for (y, m, d) in [(1970, 1, 1), (2000, 2, 29), (2026, 11, 24), (2027, 1, 1), (2024, 12, 31)] {
            assert_eq!(civil_from_days(days_from_civil(y, m, d)), (y, m, d));
        }
        assert_eq!(days_from_civil(1970, 1, 2), 1);
        // 2026-11-24 is a Tuesday, 2026-11-06 a Friday.
        assert_eq!((weekday(2026, 11, 24), weekday(2026, 11, 6)), (1, 4));
    }

    #[test]
    fn the_bars_of_the_three_views() {
        let mut hours = Hours::new();
        hours.insert("2026-11-24T00".into(), [10.0, 3600.0]);
        hours.insert("2026-11-23T23".into(), [20.0, 1800.0]);
        hours.insert("2026-11-01T12".into(), [5.0, 60.0]);
        hours.insert("2025-12-31T23".into(), [7.0, 60.0]);
        hours.insert("2025-11-30T10".into(), [100.0, 60.0]); // 13 months back: in no bar
        hours.insert("broken".into(), [1.0, 1.0]);
        let now = (2026, 11, 24, 0);
        let day = buckets(&hours, "day", now);
        assert_eq!(day.len(), 24);
        assert_eq!((day[0].start, day[23].start), ((2026, 11, 23, 1), (2026, 11, 24, 0)));
        assert_eq!((day[22].wh, day[23].wh, day[23].seconds), (20.0, 10.0, 3600.0));
        let month = buckets(&hours, "month", now);
        assert_eq!((month.len(), month[0].start, month[29].start), (30, (2026, 10, 26, 0), (2026, 11, 24, 0)));
        assert_eq!((month[28].wh, month[29].wh, month[6].wh), (20.0, 10.0, 5.0));
        let year = buckets(&hours, "year", now);
        assert_eq!((year.len(), year[0].start, year[11].start), (12, (2025, 12, 1, 0), (2026, 11, 1, 0)));
        assert_eq!((year[0].wh, year[11].wh), (7.0, 35.0));
        assert_eq!(year.iter().map(|b| b.wh).sum::<f64>(), 42.0);
        // Over the new year, to the hour.
        assert_eq!(buckets(&hours, "day", (2027, 1, 1, 0))[0].start, (2026, 12, 31, 1));
    }

    #[test]
    fn durations_steps_and_labels() {
        kit::texts!();
        kit::i18n::set_language("en");
        assert_eq!((fmt_duration(3725.0), fmt_duration(200_000.0), fmt_duration(59.0)), ("1 h 2 min".into(), "2 d 7 h".into(), "0 min".into()));
        assert_eq!((nice_step(9.0), nice_step(0.5), nice_step(70.0), nice_step(0.0)), (5.0, 0.2, 25.0, 1.0));
        assert_eq!(period((2026, 11, 6, 23), "day"), "Fri 6 Nov, 23:00–00:00");
        assert_eq!(period((2026, 11, 1, 0), "year"), "Nov 2026");
        assert_eq!(x_label((2026, 11, 6, 6), 0, 24, "day").as_deref(), Some("06:00"));
        assert_eq!(x_label((2026, 11, 6, 7), 0, 24, "day"), None);
        assert_eq!(fmt_energy(1234.0, "Wh"), "1234 Wh");
    }
}
