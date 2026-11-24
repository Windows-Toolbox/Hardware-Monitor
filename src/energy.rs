//! Power consumption: estimate, log, add up – first ported from the Python version; since 2026-11-24 in a file
//! format of its own (version 2), older logs are discarded.
//!
//! Most power supplies cannot report what they draw, so the PC's power at the wall socket is estimated:
//!
//!     wall power ≈ (CPU package power + graphics card power + other components) ÷ power supply efficiency
//!
//! A laptop on battery measures it itself (discharge rate). With a smart plug (`plug.rs`) the power is *measured*
//! – for everything on that plug (the whole desk), which is logged on its own.
//!
//! The estimate's log: every measurement adds watts × seconds to the hour it falls into, and the same for CPU +
//! GPU alone (what their sensors measure) – `data\hardware_monitor_energy.json`:
//! {"version": 2, "hours": {"2026-11-06T14": [Wh, measured seconds, of the Wh: CPU + GPU], …}}.
//!
//! The Power tab stacks per hour (`bars`): CPU + GPU (measured), the rest the plug measured, and where the plug did
//! not measure (or there is none) the rest of the PC as estimated.
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

/// Per hour "YYYY-MM-DDTHH": [Wh, measured seconds, of the Wh: CPU + GPU] – the plug's log has no CPU + GPU (0).
pub type Hours = BTreeMap<String, [f64; 3]>;
/// The files' version; a file of another one is discarded (version 1 had no CPU + GPU).
const VERSION: u64 = 2;

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

/// The hours of a log file – none if it is of another version.
fn read_hours(v: &Value) -> Hours {
    let mut hours = Hours::new();
    if v["version"].as_u64() != Some(VERSION) {
        return hours;
    }
    if let Some(map) = v["hours"].as_object() {
        for (k, e) in map {
            if let (Some(wh), Some(s)) = (e.get(0).and_then(Value::as_f64), e.get(1).and_then(Value::as_f64)) {
                hours.insert(k.clone(), [wh, s, e.get(2).and_then(Value::as_f64).unwrap_or(0.0)]);
            }
        }
    }
    hours
}

/// A file's content – and whether it is of an older version (then it goes at the next save).
fn read_file(file: &PathBuf) -> (Value, bool) {
    let v = std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str::<Value>(&s).ok()).unwrap_or_default();
    let old = !v.is_null() && v["version"].as_u64() != Some(VERSION);
    if old {
        kit::app::log(&format!("{} is of an older version – it is discarded", file.display()));
    }
    (v, old)
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
        let (v, old) = read_file(&file);
        EnergyLog { file, hours: read_hours(&v), last: None, saved: Instant::now(), dirty: old }
    }

    /// One measurement: watts since the last one (at most MAX_GAP back); `cpu_gpu`: of them, CPU + GPU.
    pub fn add(&mut self, watts: f64, cpu_gpu: f64, now: Instant, at: SystemTime) {
        let last = self.last.replace(now);
        let Some(last) = last else { return };
        let seconds = now.duration_since(last).as_secs_f64();
        if seconds <= 0.0 || seconds > MAX_GAP {
            return;
        }
        let e = self.hours.entry(hour_key(at)).or_insert([0.0; 3]);
        e[0] += watts * seconds / 3600.0;
        e[1] += seconds;
        e[2] += cpu_gpu.min(watts) * seconds / 3600.0;
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
        write(&self.file, &json!({"version": VERSION, "hours": self.hours}));
    }

    /// Deletes everything logged – in memory and the file.
    pub fn delete_all(&mut self) -> bool {
        self.hours.clear();
        self.dirty = false;
        self.last = None;
        !self.file.exists() || std::fs::remove_file(&self.file).is_ok()
    }
}

/// "YYYY-MM-DDTHH:MM" of a moment, in local time.
pub fn minute_key(t: SystemTime) -> String {
    let (y, mo, d, h, mi) = local_minute(t);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}")
}

/// The last hour per minute – for the Power tab's view "60 minutes". Like the two logs: the estimate with CPU + GPU,
/// and the plug – here from its power right now (its counter counts whole Wh, too coarse for a minute). Saved once a
/// minute to `data\hardware_monitor_minutes.json` ({"version": 2, "estimate": {…}, "plug": {…}}, keys
/// "YYYY-MM-DDTHH:MM"), so that a restart of the monitor keeps the hour; minutes it did not run stay empty.
#[derive(Default)]
pub struct MinuteLog {
    /// None: kept in memory only (tests).
    file: Option<PathBuf>,
    pub estimate: Hours,
    pub plug: Hours,
    last: Option<Instant>,
    saved: Option<Instant>,
}

impl MinuteLog {
    /// How many minutes are kept – the hour shown and the minute running.
    const KEEP: u64 = 61;
    const SAVE_EVERY: Duration = Duration::from_secs(60);

    /// The minutes saved last time – those of the last hour.
    pub fn load(file: PathBuf) -> MinuteLog {
        let (v, _) = read_file(&file);
        let read = |part: &str| read_hours(&json!({"version": v["version"], "hours": v[part]}));
        let mut log = MinuteLog { estimate: read("estimate"), plug: read("plug"), file: Some(file), ..Default::default() };
        log.prune(SystemTime::now());
        log
    }

    fn prune(&mut self, at: SystemTime) {
        // Keys of one format sort by time.
        let oldest = minute_key(at - Duration::from_secs(Self::KEEP * 60));
        self.estimate.retain(|k, _| *k > oldest);
        self.plug.retain(|k, _| *k > oldest);
    }

    pub fn save(&mut self) {
        let Some(file) = &self.file else { return };
        write(file, &json!({"version": VERSION, "estimate": self.estimate, "plug": self.plug}));
        self.saved = Some(Instant::now());
    }

    pub fn delete_all(&mut self) -> bool {
        self.estimate.clear();
        self.plug.clear();
        self.file.as_ref().is_none_or(|f| !f.exists() || std::fs::remove_file(f).is_ok())
    }

    pub fn add(&mut self, watts: f64, cpu_gpu: f64, plug: Option<f64>, now: Instant, at: SystemTime) {
        let last = self.last.replace(now);
        let Some(last) = last else { return };
        let seconds = now.duration_since(last).as_secs_f64();
        if seconds <= 0.0 || seconds > MAX_GAP {
            return;
        }
        let key = minute_key(at);
        let e = self.estimate.entry(key.clone()).or_insert([0.0; 3]);
        e[0] += watts * seconds / 3600.0;
        e[1] += seconds;
        e[2] += cpu_gpu.min(watts) * seconds / 3600.0;
        if let Some(p) = plug {
            let e = self.plug.entry(key).or_insert([0.0; 3]);
            e[0] += p * seconds / 3600.0;
            e[1] += seconds;
        }
        self.prune(at);
        if self.saved.is_none_or(|t| t.elapsed() >= Self::SAVE_EVERY) {
            self.save();
        }
    }

    pub fn pause(&mut self) {
        self.last = None;
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
        let (v, old) = read_file(&file);
        let last = if old { None } else { v["last"].as_array().and_then(|a| Some((a.first()?.as_f64()?, a.get(1)?.as_f64()?))) };
        PlugLog { file, hours: read_hours(&v), last, saved: Instant::now(), dirty: old }
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
            let e = self.hours.entry(hour_key(UNIX_EPOCH + Duration::from_secs_f64(t))).or_insert([0.0; 3]);
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
        write(&self.file, &json!({"version": VERSION, "hours": self.hours, "last": last}));
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
    /// The first day of this month with a value – where the forecast starts.
    pub month_since: Option<String>,
}

pub fn totals(hours: &Hours, today: &str) -> Totals {
    let month = &today[..7.min(today.len())];
    let day = |k: &String| k[..10.min(k.len())].to_string();
    let mut t = Totals { since: hours.keys().next().map(day), ..Default::default() };
    for (k, [wh, _, _]) in hours {
        if k.starts_with(today) {
            t.today += wh;
        }
        if k.starts_with(month) {
            t.month += wh;
            if t.month_since.is_none() && *wh > 0.0 {
                t.month_since = Some(day(k));
            }
        }
        t.all += wh;
    }
    t
}

/// The two logs taken together, per hour: where the plug measured, its value ([plug Wh, seconds, of them CPU +
/// GPU]); elsewhere the estimate (`guessed`). Without a plug everything is estimated.
pub fn split(estimate: &Hours, plug: Option<&Hours>) -> (Hours, Hours) {
    let (mut measured, mut guessed) = (Hours::new(), Hours::new());
    let at_plug = |k: &String| plug.and_then(|p| p.get(k)).filter(|q| q[0] > 0.0);
    for (k, e) in estimate {
        match at_plug(k) {
            Some(q) => measured.insert(k.clone(), [q[0], q[1], e[2].min(q[0])]),
            None => guessed.insert(k.clone(), *e),
        };
    }
    // Hours only the plug has (the PC was off, or the monitor did not run): what it measured, CPU + GPU unknown.
    for (k, q) in plug.into_iter().flatten() {
        if q[0] > 0.0 && !estimate.contains_key(k) {
            measured.insert(k.clone(), [q[0], q[1], 0.0]);
        }
    }
    (measured, guessed)
}

/// Today and this month of both logs together (`split`).
pub fn totals_of(estimate: &Hours, plug: Option<&Hours>, today: &str) -> Totals {
    let (measured, guessed) = split(estimate, plug);
    let (a, b) = (totals(&measured, today), totals(&guessed, today));
    let first = |x: Option<String>, y: Option<String>| match (x, y) {
        (Some(x), Some(y)) => Some(x.min(y)),
        (x, y) => x.or(y),
    };
    Totals { today: a.today + b.today, month: a.month + b.month, all: a.all + b.all, since: first(a.since, b.since), month_since: first(a.month_since, b.month_since) }
}

pub fn days_in_month(y: i32, m: u32) -> u32 {
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    (days_from_civil(ny, nm, 1) - days_from_civil(y, m, 1)) as u32
}

/// This month at the same pace: what was used since the first day with a value, per day, × the month's days.
/// `since` = "YYYY-MM-DD"; `now` with the minute.
pub fn forecast(month_wh: f64, since: &str, now: (i32, u32, u32, u32, u32)) -> Option<f64> {
    let (y, m, d, h, mi) = now;
    let num = |a: usize, b: usize| since.get(a..b)?.parse::<u32>().ok();
    let (sy, sm, sd) = (num(0, 4)? as i32, num(5, 7)?, num(8, 10)?);
    if (sy, sm) != (y, m) || month_wh <= 0.0 {
        return None;
    }
    let elapsed = (days_from_civil(y, m, d) - days_from_civil(sy, sm, sd)) as f64 + (h as f64 * 60.0 + mi as f64) / 1440.0;
    Some(month_wh / elapsed.max(1.0) * days_in_month(y, m) as f64)
}

/// One bar of the Power tab: CPU + GPU (measured by their sensors), the rest the plug measured, the rest of the PC
/// as estimated where the plug did not measure (or there is none) – in Wh; measured seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub start: LocalTime,
    pub cpu_gpu: f64,
    pub rest: f64,
    pub estimated: f64,
    pub seconds: f64,
}

impl Bar {
    pub fn total(&self) -> f64 {
        self.cpu_gpu + self.rest + self.estimated
    }
}

/// The bars of a view (`buckets`), stacked from both logs.
pub fn bars(estimate: &Hours, plug: Option<&Hours>, view: &str, now: LocalTime) -> Vec<Bar> {
    let (measured, guessed) = split(estimate, plug);
    buckets(&measured, view, now)
        .into_iter()
        .zip(buckets(&guessed, view, now))
        .map(|(m, g)| Bar { start: m.start, cpu_gpu: m.cpu_gpu + g.cpu_gpu, rest: (m.wh - m.cpu_gpu).max(0.0), estimated: (g.wh - g.cpu_gpu).max(0.0), seconds: m.seconds + g.seconds })
        .collect()
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

// ---------- The chart of the window's Power tab ----------

/// The views of the chart: the last 60 minutes, 24 hours, 30 days, 12 months.
pub const VIEWS: [&str; 4] = ["hour", "day", "month", "year"];

/// A moment of local time to the minute: (year, month, day, hour, minute).
pub type LocalTime = (i32, u32, u32, u32, u32);

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
    pub start: LocalTime,
    pub wh: f64,
    pub seconds: f64,
    /// Of `wh`: CPU + GPU.
    pub cpu_gpu: f64,
}

/// The bars of a view, oldest first, empty ones included: "hour" = the last 60 minutes (of a log per minute,
/// `MinuteLog`), "day" = the last 24 hours, "month" = the last 30 days, "year" = the last 12 months – up to and
/// including `now`.
pub fn buckets(hours: &Hours, view: &str, now: LocalTime) -> Vec<Bucket> {
    let (y, m, d, h, mi) = now;
    let today = days_from_civil(y, m, d);
    let starts: Vec<LocalTime> = match view {
        "hour" => (0..60)
            .rev()
            .map(|i| {
                let minute = (today * 24 + h as i64) * 60 + mi as i64 - i;
                let (y, m, d) = civil_from_days(minute.div_euclid(1440));
                let of_day = minute.rem_euclid(1440);
                (y, m, d, (of_day / 60) as u32, (of_day % 60) as u32)
            })
            .collect(),
        "day" => (0..24)
            .rev()
            .map(|i| {
                let hour = today * 24 + h as i64 - i;
                let (y, m, d) = civil_from_days(hour.div_euclid(24));
                (y, m, d, hour.rem_euclid(24) as u32, 0)
            })
            .collect(),
        "month" => (0..30)
            .rev()
            .map(|i| {
                let (y, m, d) = civil_from_days(today - i);
                (y, m, d, 0, 0)
            })
            .collect(),
        _ => (0..12)
            .rev()
            .map(|i| {
                let months = y * 12 + m as i32 - 1 - i;
                (months.div_euclid(12), months.rem_euclid(12) as u32 + 1, 1, 0, 0)
            })
            .collect(),
    };
    // Which bar a log key ("YYYY-MM-DDTHH", per minute "YYYY-MM-DDTHH:MM") belongs to.
    let bar_of = |key: &str| -> Option<LocalTime> {
        let num = |a: usize, b: usize| key.get(a..b)?.parse::<u32>().ok();
        let (ky, km, kd, kh) = (num(0, 4)? as i32, num(5, 7)?, num(8, 10)?, num(11, 13)?);
        Some(match view {
            "hour" => (ky, km, kd, kh, num(14, 16)?),
            "day" => (ky, km, kd, kh, 0),
            "month" => (ky, km, kd, 0, 0),
            _ => (ky, km, 1, 0, 0),
        })
    };
    let mut sums: BTreeMap<LocalTime, [f64; 3]> = BTreeMap::new();
    for (k, [wh, s, cg]) in hours {
        if let Some(bar) = bar_of(k) {
            let e = sums.entry(bar).or_insert([0.0; 3]);
            e[0] += wh;
            e[1] += s;
            e[2] += cg;
        }
    }
    starts
        .into_iter()
        .map(|start| {
            let [wh, seconds, cpu_gpu] = sums.get(&start).copied().unwrap_or([0.0; 3]);
            Bucket { start, wh, seconds, cpu_gpu }
        })
        .collect()
}

/// Now, to the minute.
pub fn now_minute() -> LocalTime {
    let (y, m, d, h, mi) = local_minute(SystemTime::now());
    (y as i32, m as u32, d as u32, h as u32, mi as u32)
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
pub fn period(start: LocalTime, view: &str) -> String {
    let (y, m, d, h, mi) = start;
    let days = if kit::i18n::language() == "de" { &DAYS_DE } else { &DAYS_EN };
    let day = format!("{} {}", days[weekday(y, m, d)], day_month(d, m));
    match view {
        "hour" => format!("{day}, {h:02}:{mi:02}"),
        "day" => format!("{day}, {h:02}:00–{:02}:00", (h + 1) % 24),
        "month" => day,
        _ => format!("{} {y}", month_name(m)),
    }
}

/// The label under a bar – not every bar has one.
pub fn x_label(start: LocalTime, i: usize, count: usize, view: &str) -> Option<String> {
    let (_, m, d, h, mi) = start;
    match view {
        "hour" => (mi % 10 == 0).then(|| format!("{h:02}:{mi:02}")),
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
    fn the_log_is_read_and_written_alike() {
        let dir = temp("log");
        let file = dir.join("hardware_monitor_energy.json");
        // Version 1 (no CPU + GPU) is discarded.
        std::fs::write(&file, r#"{"version":1,"hours":{"2026-11-06T14":[12.5,3600.0]}}"#).unwrap();
        assert!(EnergyLog::load(file.clone()).hours.is_empty());
        std::fs::write(&file, r#"{"version":2,"hours":{"2026-11-06T14":[12.5,3600.0,5.0],"2026-11-07T01":[1,60]}}"#).unwrap();
        let mut log = EnergyLog::load(file.clone());
        assert_eq!((log.hours.len(), log.hours["2026-11-07T01"]), (2, [1.0, 60.0, 0.0]));
        let (t0, at) = (Instant::now(), SystemTime::now());
        log.add(100.0, 40.0, t0, at);
        log.add(100.0, 40.0, t0 + Duration::from_secs(36), at);
        assert_eq!(log.hours.len(), 2, "a gap of more than 30 s is not counted");
        log.add(100.0, 40.0, t0 + Duration::from_secs(54), at);
        let key = hour_key(at);
        assert!((log.hours[&key][0] - 0.5).abs() < 1e-9 && log.hours[&key][1] == 18.0 && (log.hours[&key][2] - 0.2).abs() < 1e-9);
        log.dirty = true;
        log.save();
        let back: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        assert_eq!((back["version"].as_u64(), back["hours"]["2026-11-06T14"][2].as_f64()), (Some(2), Some(5.0)));
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
        // A log of version 1: hours and last reading go.
        std::fs::write(dir.join("old.json"), r#"{"version":1,"hours":{"2026-11-06T14":[12.5,3600.0]},"last":[1.0,2.0]}"#).unwrap();
        let old = PlugLog::load(dir.join("old.json"));
        assert!(old.hours.is_empty() && old.last.is_none());
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
        hours.insert("2026-11-24T00".into(), [10.0, 3600.0, 4.0]);
        hours.insert("2026-11-23T23".into(), [20.0, 1800.0, 0.0]);
        hours.insert("2026-11-01T12".into(), [5.0, 60.0, 0.0]);
        hours.insert("2025-12-31T23".into(), [7.0, 60.0, 0.0]);
        hours.insert("2025-11-30T10".into(), [100.0, 60.0, 0.0]); // 13 months back: in no bar
        hours.insert("broken".into(), [1.0, 1.0, 0.0]);
        let now = (2026, 11, 24, 0, 30);
        let day = buckets(&hours, "day", now);
        assert_eq!(day.len(), 24);
        assert_eq!((day[0].start, day[23].start), ((2026, 11, 23, 1, 0), (2026, 11, 24, 0, 0)));
        assert_eq!((day[22].wh, day[23].wh, day[23].seconds, day[23].cpu_gpu), (20.0, 10.0, 3600.0, 4.0));
        let month = buckets(&hours, "month", now);
        assert_eq!((month.len(), month[0].start, month[29].start), (30, (2026, 10, 26, 0, 0), (2026, 11, 24, 0, 0)));
        assert_eq!((month[28].wh, month[29].wh, month[6].wh), (20.0, 10.0, 5.0));
        let year = buckets(&hours, "year", now);
        assert_eq!((year.len(), year[0].start, year[11].start), (12, (2025, 12, 1, 0, 0), (2026, 11, 1, 0, 0)));
        assert_eq!((year[0].wh, year[11].wh), (7.0, 35.0));
        assert_eq!(year.iter().map(|b| b.wh).sum::<f64>(), 42.0);
        // Over the new year, to the hour.
        assert_eq!(buckets(&hours, "day", (2027, 1, 1, 0, 0))[0].start, (2026, 12, 31, 1, 0));
        // The last 60 minutes, of a log per minute – over midnight; hour keys belong to no minute.
        let mut minutes = Hours::new();
        minutes.insert("2026-11-23T23:59".into(), [2.0, 60.0, 1.0]);
        minutes.insert("2026-11-24T00:30".into(), [3.0, 40.0, 1.0]);
        minutes.insert("2026-11-23T23:20".into(), [9.0, 60.0, 0.0]); // 70 minutes back
        let hour = buckets(&minutes, "hour", (2026, 11, 24, 0, 30));
        assert_eq!((hour.len(), hour[0].start, hour[59].start), (60, (2026, 11, 23, 23, 31), (2026, 11, 24, 0, 30)));
        assert_eq!((hour[28].wh, hour[59].wh, hour.iter().map(|b| b.wh).sum::<f64>()), (2.0, 3.0, 5.0));
        assert!(buckets(&hours, "hour", now).iter().all(|b| b.wh == 0.0));
    }

    #[test]
    fn the_stack_of_both_logs() {
        let mut estimate = Hours::new();
        estimate.insert("2026-11-24T15".into(), [222.0, 3600.0, 88.0]); // the plug measured this hour
        estimate.insert("2026-11-24T10".into(), [150.0, 3600.0, 60.0]); // this one not
        let mut plug = Hours::new();
        plug.insert("2026-11-24T15".into(), [231.0, 3600.0, 0.0]);
        plug.insert("2026-11-24T03".into(), [12.0, 0.0, 0.0]); // the PC was off: only the plug
        let now = (2026, 11, 24, 16, 0);
        let day = bars(&estimate, Some(&plug), "day", now);
        let at = |h: u32| day.iter().find(|b| b.start.3 == h).unwrap();
        assert_eq!((at(15).cpu_gpu, at(15).rest, at(15).estimated), (88.0, 143.0, 0.0));
        assert_eq!((at(10).cpu_gpu, at(10).rest, at(10).estimated), (60.0, 0.0, 90.0));
        assert_eq!((at(3).cpu_gpu, at(3).rest), (0.0, 12.0));
        // Without the plug: CPU + GPU and the estimated rest.
        let alone = bars(&estimate, None, "day", now);
        assert_eq!(alone.iter().map(Bar::total).sum::<f64>(), 372.0);
        assert_eq!(alone.iter().find(|b| b.start.3 == 15).map(|b| (b.cpu_gpu, b.estimated)), Some((88.0, 134.0)));
        let t = totals_of(&estimate, Some(&plug), "2026-11-24");
        assert_eq!((t.today, t.month_since.as_deref()), (231.0 + 150.0 + 12.0, Some("2026-11-24")));
    }

    #[test]
    fn the_minutes_of_the_last_hour() {
        let mut log = MinuteLog::default();
        let (t0, at) = (Instant::now(), SystemTime::now());
        log.add(240.0, 100.0, Some(300.0), t0, at);
        log.add(240.0, 100.0, Some(300.0), t0 + Duration::from_secs(15), at);
        let key = minute_key(at);
        assert_eq!((log.estimate[&key], log.plug[&key]), ([1.0, 15.0, 100.0 * 15.0 / 3600.0], [1.25, 15.0, 0.0]));
        // 2 hours later: the old minutes go.
        log.add(240.0, 100.0, None, t0 + Duration::from_secs(25), at + Duration::from_secs(7200));
        assert_eq!((log.estimate.len(), log.plug.len()), (1, 0));
    }

    #[test]
    fn the_minutes_outlast_a_restart() {
        let dir = temp("minutes");
        let file = dir.join("hardware_monitor_minutes.json");
        let mut log = MinuteLog::load(file.clone());
        let (t0, at) = (Instant::now(), SystemTime::now());
        log.add(240.0, 100.0, Some(300.0), t0, at);
        log.add(240.0, 100.0, Some(300.0), t0 + Duration::from_secs(15), at);
        assert!(file.exists(), "saved at the first minute");
        // Older than an hour in the file: it goes at the next start.
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
        v["estimate"]["2020-01-01T00:00"] = json!([1.0, 60.0, 0.0]);
        std::fs::write(&file, v.to_string()).unwrap();
        let again = MinuteLog::load(file.clone());
        assert_eq!((again.estimate.len(), again.plug.len()), (1, 1));
        assert_eq!(again.estimate[&minute_key(at)][0], 1.0);
        let mut again = again;
        assert!(again.delete_all() && !file.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_month_forecast() {
        assert_eq!((days_in_month(2026, 11), days_in_month(2026, 12), days_in_month(2028, 2)), (30, 31, 29));
        // Since the 18th, at noon of the 24th: 6.5 days for 13 kWh → 2 kWh a day → 60 kWh.
        let f = forecast(13_000.0, "2026-11-18", (2026, 11, 24, 12, 0)).unwrap();
        assert!((f - 60_000.0).abs() < 1e-6, "{f}");
        assert_eq!(forecast(13_000.0, "2026-10-18", (2026, 11, 24, 12, 0)), None, "another month");
        assert!((forecast(500.0, "2026-11-24", (2026, 11, 24, 1, 0)).unwrap() - 15_000.0).abs() < 1e-6, "at least one day");
    }

    #[test]
    fn durations_steps_and_labels() {
        kit::texts!();
        kit::i18n::set_language("en");
        assert_eq!((fmt_duration(3725.0), fmt_duration(200_000.0), fmt_duration(59.0)), ("1 h 2 min".into(), "2 d 7 h".into(), "0 min".into()));
        assert_eq!((nice_step(9.0), nice_step(0.5), nice_step(70.0), nice_step(0.0)), (5.0, 0.2, 25.0, 1.0));
        assert_eq!(period((2026, 11, 6, 23, 0), "day"), "Fri 6 Nov, 23:00–00:00");
        assert_eq!(period((2026, 11, 6, 18, 7), "hour"), "Fri 6 Nov, 18:07");
        assert_eq!(period((2026, 11, 1, 0, 0), "year"), "Nov 2026");
        assert_eq!(x_label((2026, 11, 6, 6, 0), 0, 24, "day").as_deref(), Some("06:00"));
        assert_eq!(x_label((2026, 11, 6, 7, 0), 0, 24, "day"), None);
        assert_eq!(x_label((2026, 11, 6, 18, 20), 0, 60, "hour").as_deref(), Some("18:20"));
        assert_eq!(fmt_energy(1234.0, "Wh"), "1234 Wh");
    }
}
