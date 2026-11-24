//! The detail window (M4, step 3) – the "window role" of HardwareMonitor.exe (Toolbox\DESIGN.md 5): started by the
//! monitor with `--window [tab]` on a click, it shows what the monitor sends over `link::PIPE` and ends when it is
//! closed, giving all its memory back. What it shows comes from `detail_window.json` (`content.rs`).
//!
//! Like the Python window: the two temperatures large at the top, privacy mode, "Loading data …" / "● Live", the
//! gear (the page in the Toolbox); tabs that wrap; in a list tab the devices with their values – a click on a value
//! or a device name copies it, rows can be selected (click, Ctrl/Shift+click, dragging) and copied (Ctrl+C, right
//! click), the footer explains the value under the mouse. Size and position are kept.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, FontId, Key, RichText, Sense, ViewportCommand};
use kit::ipc::{self, Conn};
use kit::t;
use kit::ui;

use crate::content::{self, Content, Row, RowKind};
use crate::energy::num;
use crate::link::{self, State, ToMonitor, ToWindow, Want};
use crate::sensors::Sensor;

/// At 100 % scaling, the first time.
const SIZE: [f32; 2] = [560.0, 640.0];
const ROW: f32 = 22.0;
/// A value's bar – wider when a second value ("97 %") stands behind it.
const BAR_WIDTH: f32 = 90.0;
const BAR_WITH_PERCENT: f32 = 140.0;
const BAR_TYPES: [&str; 5] = ["Temperature", "Load", "Control", "Level", "Humidity"];
/// Limits, not measurements – no traffic light.
const NO_TRAFFIC_LIGHT: [&str; 3] = ["*Distance to TjMax", "Warning Temperature", "Critical Temperature"];
/// How long "… copied" stays in the footer.
const MESSAGE: Duration = Duration::from_secs(2);

#[derive(Default)]
struct Shared {
    state: Option<State>,
    /// `Front` came: come forward, maybe on a tab.
    front: Option<Option<String>>,
    /// The monitor has gone – the window closes.
    gone: bool,
}

fn lock(s: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

/// Runs the window until it is closed. `tab`: the tab to open on (a title id, e.g. "tab.power").
pub fn run(tab: Option<String>) {
    let Ok(conn) = ipc::connect_wait(link::PIPE, Duration::from_secs(5)) else {
        return kit::app::log("the window was started, but the monitor does not answer");
    };
    let (content, error) = match Content::load() {
        Ok(c) => (c, None),
        Err(e) => {
            kit::app::log(&format!("detail_window.json could not be read: {e}"));
            (Content::fallback(), Some(e))
        }
    };
    let shared = Arc::new(Mutex::new(Shared::default()));
    // The first state says the language and where the window was – read before the window exists.
    let (first_tx, first_rx) = std::sync::mpsc::channel::<()>();
    let reader_ctx: Arc<Mutex<Option<egui::Context>>> = Arc::new(Mutex::new(None));
    {
        let (conn, shared, ctx) = (conn.clone(), shared.clone(), reader_ctx.clone());
        std::thread::spawn(move || {
            while let Some(line) = conn.read_line() {
                match serde_json::from_str::<ToWindow>(&line) {
                    Ok(ToWindow::State(state)) => {
                        lock(&shared).state = Some(*state);
                        let _ = first_tx.send(());
                    }
                    Ok(ToWindow::Front { tab }) => lock(&shared).front = Some(tab),
                    Err(_) => continue,
                }
                if let Some(ctx) = ctx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    ctx.request_repaint();
                }
            }
            lock(&shared).gone = true;
            if let Some(ctx) = ctx.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                ctx.request_repaint();
            }
        });
    }
    let mut app = App::new(conn, shared.clone(), content, error);
    if let Some(tab) = tab {
        app.select_title(&tab);
    }
    app.send_want(false);
    let _ = first_rx.recv_timeout(Duration::from_secs(5));
    let settings = lock(&shared).state.as_ref().map(|s| s.settings.clone()).unwrap_or_default();
    kit::i18n::set_language(if settings.language.is_empty() { "en" } else { &settings.language });

    let title = window_title(settings.privacy_mode);
    let mut viewport = ui::viewport(&title).with_min_inner_size([320.0, 240.0]);
    let (size, position) = placement(&settings.window);
    viewport = viewport.with_inner_size(size).with_position(position);
    let options = eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, ..Default::default() };
    let result = eframe::run_native(
        &title,
        options,
        Box::new(move |cc| {
            ui::theme(&cc.egui_ctx, if settings.theme.is_empty() { "dark" } else { &settings.theme }, if settings.zoom > 0.0 { settings.zoom } else { 1.0 });
            *reader_ctx.lock().unwrap_or_else(|e| e.into_inner()) = Some(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    );
    if let Err(e) = result {
        ui::no_window(&t!("program.name"), &e);
    }
}

/// The window's title: the program's path, like Explorer shows its folder – in privacy mode only the name (the path
/// can hold the Windows user name).
fn window_title(private: bool) -> String {
    if private { t!("program.name") } else { std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| t!("program.name")) }
}

/// Size and position in points: the saved ones ("WxH+X+Y" in pixels) if that place is still on a screen, else at
/// the bottom right of the screen where the mouse is.
fn placement(saved: &str) -> ([f32; 2], [f32; 2]) {
    let scale = system_scale();
    let parse = || -> Option<[i32; 4]> {
        let (size, rest) = saved.split_once('+')?;
        let (w, h) = size.split_once('x')?;
        let i = rest.find(['+', '-'])?;
        let (x, y) = (&rest[..i], rest[i..].trim_start_matches('+'));
        Some([w.parse().ok()?, h.parse().ok()?, x.parse().ok()?, y.parse().ok()?])
    };
    if let Some([w, h, x, y]) = parse()
        && work_area(x + w / 2, y + h / 2).is_some()
    {
        return ([w as f32 / scale, h as f32 / scale], [x as f32 / scale, y as f32 / scale]);
    }
    let mut cursor = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 };
    unsafe { windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos(&mut cursor) };
    let [left, top, right, bottom] = work_area(cursor.x, cursor.y).unwrap_or([0, 0, 1920, 1040]);
    let (w, h) = (SIZE[0], SIZE[1].min((bottom - top) as f32 / scale - 56.0));
    let x = right as f32 / scale - w - 12.0;
    let y = bottom as f32 / scale - h - 44.0;
    let _ = left;
    ([w, h], [x, y])
}

fn system_scale() -> f32 {
    (unsafe { windows_sys::Win32::UI::HiDpi::GetDpiForSystem() } as f32 / 96.0).max(1.0)
}

/// The screen area without the taskbar at a point – `None` if no screen is there.
fn work_area(x: i32, y: i32) -> Option<[i32; 4]> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint};
    unsafe {
        let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONULL);
        if monitor.is_null() {
            return None;
        }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 {
            return None;
        }
        let r = info.rcWork;
        Some([r.left, r.top, r.right, r.bottom])
    }
}

/// How long a clicked privacy switch is shown before the monitor's word counts again.
const PRIVACY_WAIT: Duration = Duration::from_secs(3);

/// What a tab's view wants after a frame: the footer's text, something copied (text, message), a message to the
/// monitor.
#[derive(Default)]
pub struct ViewOut {
    pub footer: Option<String>,
    pub copy: Option<(String, String)>,
    pub send: Option<ToMonitor>,
}

/// A short message in the footer.
struct Message {
    text: String,
    error: bool,
    since: Instant,
}

struct App {
    conn: Arc<Conn>,
    shared: Arc<Mutex<Shared>>,
    content: Content,
    /// A mistake in `detail_window.json` – stays in the footer.
    error: Option<String>,
    tab: usize,
    want: Option<Want>,
    /// Shown "● Live" since the window opened.
    live: bool,
    collapsed: BTreeSet<String>,
    selected: BTreeSet<String>,
    /// Where Shift+click and dragging start.
    anchor: Option<String>,
    dragging: bool,
    message: Option<Message>,
    first_frame: bool,
    title: String,
    /// The last size and position in pixels, sent when the window closes.
    geometry: Option<String>,
    /// What the footer shows.
    footer: (String, Color32),
    opened: Instant,
    /// The privacy switch as just clicked – shown at once, until the monitor's answer says the same (or it is late).
    privacy_clicked: Option<(bool, Instant)>,
    power: crate::power_view::PowerView,
    fans: crate::fan_view::FanView,
}

/// The row under the mouse this frame, and what a click there would copy.
struct Hover {
    explain: String,
}

impl App {
    fn new(conn: Arc<Conn>, shared: Arc<Mutex<Shared>>, content: Content, error: Option<String>) -> App {
        App {
            conn,
            shared,
            content,
            error,
            tab: 0,
            want: None,
            live: false,
            collapsed: BTreeSet::new(),
            selected: BTreeSet::new(),
            anchor: None,
            dragging: false,
            message: None,
            first_frame: true,
            title: String::new(),
            geometry: None,
            footer: (String::new(), Color32::GRAY),
            opened: Instant::now(),
            privacy_clicked: None,
            power: Default::default(),
            fans: Default::default(),
        }
    }

    fn select_title(&mut self, title: &str) {
        if let Some(i) = self.content.tabs.iter().position(|t| t.title == title) {
            self.tab = i;
        }
    }

    fn send(&self, msg: &ToMonitor) {
        let _ = self.conn.send(msg);
    }

    /// Tells the monitor what to read – only when it changed. `minimized`: nothing.
    fn send_want(&mut self, minimized: bool) {
        let want = if minimized { Want::default() } else { self.content.tabs.get(self.tab).map(content::want).unwrap_or_default() };
        if self.want.as_ref() != Some(&want) {
            self.send(&ToMonitor::Want(want.clone()));
            self.want = Some(want);
        }
    }

    fn confirm(&mut self, text: String, error: bool) {
        self.message = Some(Message { text, error, since: Instant::now() });
    }

    fn copy(&mut self, ctx: &egui::Context, text: String, message: String) {
        ctx.copy_text(text);
        self.confirm(message, false);
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.first_frame {
            self.first_frame = false;
            ui::use_program_icon();
        }
        let (state, front, gone) = {
            let mut s = lock(&self.shared);
            (s.state.clone(), s.front.take(), s.gone)
        };
        if gone {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if let Some(tab) = front {
            if let Some(tab) = tab {
                self.select_title(&tab);
            }
            ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(ViewportCommand::Focus);
        }
        let Some(mut state) = state else {
            // Nothing from the monitor: it is starting – or it is not the Rust version that runs.
            let waited = self.opened.elapsed() > Duration::from_secs(5);
            egui::CentralPanel::default().show(ui, |ui| {
                if waited {
                    ui.label(RichText::new(t!("window.no_monitor")).color(ui::warn(ui)));
                } else {
                    ui.spinner();
                }
            });
            if !waited {
                ctx.request_repaint_after(Duration::from_millis(500));
            }
            return;
        };
        if let Some((on, at)) = self.privacy_clicked {
            if state.settings.privacy_mode == on || at.elapsed() > PRIVACY_WAIT {
                self.privacy_clicked = None;
            } else {
                state.settings.privacy_mode = on;
            }
        }
        if kit::i18n::language() != state.settings.language && !state.settings.language.is_empty() {
            kit::i18n::set_language(&state.settings.language);
        }
        let (minimized, close, geometry) = ctx.input(|i| {
            let v = i.viewport();
            let ppp = v.native_pixels_per_point.unwrap_or(1.0);
            let geometry = match (v.inner_rect, v.outer_rect) {
                (Some(inner), Some(outer)) if v.minimized != Some(true) && v.maximized != Some(true) => Some(format!(
                    "{}x{}+{}+{}",
                    (inner.width() * ppp).round() as i32,
                    (inner.height() * ppp).round() as i32,
                    (outer.min.x * ppp).round() as i32,
                    (outer.min.y * ppp).round() as i32
                )),
                _ => None,
            };
            (v.minimized == Some(true), v.close_requested(), geometry)
        });
        if geometry.is_some() {
            self.geometry = geometry;
        }
        if close && let Some(g) = &self.geometry {
            self.send(&ToMonitor::Geometry { value: g.clone() });
        }
        let title = window_title(state.settings.privacy_mode);
        if title != self.title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        // Which tabs this PC has.
        let present = state.present.clone();
        let visible: Vec<usize> = (0..self.content.tabs.len()).filter(|&i| present.as_ref().is_none_or(|p| content::shown(&self.content.tabs[i], p))).collect();
        if !visible.contains(&self.tab) {
            self.tab = visible.first().copied().unwrap_or(0);
        }
        self.send_want(minimized);
        let tab = self.content.tabs.get(self.tab).cloned();
        let want = self.want.clone().unwrap_or_default();
        let answered = state.answers == want && (state.hardware.is_some() || !matches!(want.view.as_str(), "list" | "fans"));
        let loaded = answered && !(state.loading && want.view == "list");
        if loaded {
            self.live = true;
        }

        let mut footer: Option<(String, Color32)> = None;
        egui::Panel::top("header").show(ui, |ui| {
            ui.add_space(6.0);
            egui::Sides::new().show(
                ui,
                |ui| {
                    for (kind, value) in &state.temps {
                        ui.label(RichText::new(kind.to_uppercase()).size(14.0).color(ui::muted(ui)));
                        let (warn, hot) = limits(&state.settings, kind);
                        let text = value.map_or("–".to_string(), |v| format!("{v:.0} °C"));
                        ui.label(RichText::new(text).size(24.0).strong().color(level_color(ui, *value, warn, hot)));
                        ui.add_space(18.0);
                    }
                },
                |ui| {
                    let gear = ui.add(egui::Button::new(RichText::new("⚙").size(18.0)).frame(false));
                    if gear.hovered() {
                        footer = Some((t!("window.gear"), ui::muted(ui)));
                    }
                    if gear.clicked() {
                        // The page in the Toolbox; no Toolbox → say so.
                        std::thread::spawn(|| {
                            if !kit::app::open_in_toolbox(crate::bridge::ID) {
                                kit::app::log("no Toolbox runs – its page cannot be shown");
                            }
                        });
                    }
                    let (status, color) = if self.live { (format!("●  {}", t!("window.live")), ui::accent(ui)) } else { (t!("window.loading"), ui::muted(ui)) };
                    ui.label(RichText::new(status).color(color));
                    ui.add_space(12.0);
                    let on = state.settings.privacy_mode;
                    let privacy = ui.add(egui::Button::new(RichText::new(format!("{}  {}", if on { "☑" } else { "☐" }, t!("window.privacy"))).color(if on { ui::accent(ui) } else { ui::muted(ui) })).frame(false));
                    if privacy.hovered() {
                        footer = Some((t!(if on { "window.privacy.on_help" } else { "window.privacy.off_help" }), ui::muted(ui)));
                    }
                    if privacy.clicked() {
                        self.privacy_clicked = Some((!on, Instant::now()));
                        self.send(&ToMonitor::Privacy { on: !on });
                    }
                },
            );
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for &i in &visible {
                    let chosen = i == self.tab;
                    if ui.add(egui::Button::new(content::label(&self.content.tabs[i].title)).selected(chosen)).clicked() && !chosen {
                        self.tab = i;
                        self.selected.clear();
                        self.anchor = None;
                    }
                }
            });
            ui.add_space(4.0);
        });

        // The footer: what the last frame found under the mouse (egui draws again whenever the mouse moves).
        let (text, color) = self.footer.clone();
        egui::Panel::bottom("footer").show(ui, |ui| {
            ui.add_space(4.0);
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(color)).wrap());
            ui.add_space(4.0);
        });
        let mut hover: Option<Hover> = None;
        let mut out = ViewOut::default();
        egui::CentralPanel::default().show(ui, |ui| match tab.as_ref().and_then(|t| t.view.clone()).as_deref() {
            None | Some("all") => {
                if let Some(tab) = &tab {
                    hover = self.list(ui, &ctx, tab, &state, answered);
                }
            }
            Some("power") => self.power.show(ui, &state, &mut out),
            Some("fans") => self.fans.show(ui, &state, answered, &mut out),
            Some(_) => {
                ui.add_space(20.0);
                ui.label(RichText::new(t!("window.coming")).color(ui::muted(ui)));
            }
        });
        if let Some(msg) = out.send {
            self.send(&msg);
        }
        if let Some((text, message)) = out.copy {
            self.copy(&ctx, text, message);
        }
        if footer.is_none() {
            footer = out.footer.map(|f| (f, ui::muted(ui)));
        }

        // Next frame's footer: a message, a mistake in the file, a button's help, the row under the mouse – or the hint.
        if let Some(m) = &self.message {
            if m.since.elapsed() > if m.error { MESSAGE * 2 } else { MESSAGE } {
                self.message = None;
            }
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        let muted = ui::muted(ui);
        let next = if let Some(m) = &self.message {
            (format!("{}  {}", if m.error { "⚠" } else { "✓" }, m.text), if m.error { ui::bad(ui) } else { ui::accent(ui) })
        } else if let Some(e) = &self.error {
            (format!("⚠  {}", t!("window.content_error", error = e)), ui::bad(ui))
        } else if let Some(f) = footer {
            f
        } else if let Some(h) = hover {
            (h.explain, muted)
        } else {
            (t!("window.hint"), muted)
        };
        if next != self.footer {
            self.footer = next;
            ctx.request_repaint();
        }
    }
}

fn limits(s: &link::Settings, kind: &str) -> (u32, u32) {
    if kind == "gpu" { (s.gpu_warn_temp, s.gpu_hot_temp) } else { (s.cpu_warn_temp, s.cpu_hot_temp) }
}

fn level_color(ui: &egui::Ui, value: Option<f64>, warn: u32, hot: u32) -> Color32 {
    match value {
        None => ui::muted(ui),
        Some(v) if v >= hot as f64 => ui::bad(ui),
        Some(v) if v >= warn as f64 => ui::warn(ui),
        Some(_) => ui::good(ui),
    }
}

/// A value as the window shows it.
pub fn fmt_value(kind: &str, value: f64) -> String {
    match kind {
        "Throughput" => {
            let mut v = value;
            for unit in ["B/s", "KB/s", "MB/s", "GB/s"] {
                if v < 1024.0 || unit == "GB/s" {
                    return format!("{} {unit}", num(v, if unit == "B/s" { 0 } else { 1 }));
                }
                v /= 1024.0;
            }
            unreachable!()
        }
        "TimeSpan" => {
            let minutes = (value / 60.0) as i64;
            format!("{}:{:02} h", minutes / 60, minutes % 60)
        }
        "Data" if value >= 1024.0 => format!("{} TB", num(value / 1024.0, 2)),
        // LHM gives mWh, e.g. a battery's capacity.
        "Energy" => format!("{} Wh", num(value / 1000.0, 1)),
        "SmallData" if value >= 1024.0 => format!("{} GB", num(value / 1024.0, 1)),
        "Factor" if value.fract() == 0.0 => num(value, 0),
        _ => {
            let (decimals, unit) = match kind {
                "Temperature" => (0, "°C"),
                "Load" | "Control" | "Level" | "Humidity" => (0, "%"),
                "Clock" => (0, "MHz"),
                "Frequency" => (0, "Hz"),
                "Power" => (1, "W"),
                "Voltage" => (3, "V"),
                "Current" => (2, "A"),
                "Fan" => (0, "RPM"),
                "Flow" => (0, "L/h"),
                "Data" => (1, "GB"),
                "SmallData" => (0, "MB"),
                "Timing" => (1, "ns"),
                "Noise" => (0, "dBA"),
                "Conductivity" => (0, "µS/cm"),
                "Factor" => (2, ""),
                _ => (1, ""),
            };
            format!("{} {unit}", num(value, decimals)).trim().to_string()
        }
    }
}

/// What a value row shows: its value text and colour, and the text behind its bar.
fn shown(ui: &egui::Ui, settings: &link::Settings, sensor: &Sensor, second: &Option<Box<Sensor>>, device: &str) -> (String, Color32) {
    if second.is_some() && sensor.kind == "Fan" && sensor.value == 0.0 {
        // Driven, but not spinning (or nothing attached).
        return (t!("value.stopped"), ui::muted(ui));
    }
    let text = fmt_value(&sensor.kind, sensor.value);
    let temp = sensor.kind == "Temperature" && !NO_TRAFFIC_LIGHT.iter().any(|p| content::glob(p, &sensor.name));
    if temp {
        // The graphics card by the GPU's limits, everything else (CPU, motherboard, drives …) by the CPU's.
        let (warn, hot) = limits(settings, if device == "gpu" { "gpu" } else { "cpu" });
        return (text, level_color(ui, Some(sensor.value), warn, hot));
    }
    (text, ui.visuals().text_color())
}

impl App {
    /// A list tab. `ready`: the reading fits the tab. The row under the mouse comes back for the footer.
    fn list(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, tab: &content::Tab, state: &State, ready: bool) -> Option<Hover> {
        let rows = if ready { content::rows(tab, state.hardware.as_deref().unwrap_or_default()) } else { vec![] };
        if rows.is_empty() {
            ui.add_space(12.0);
            let text = if ready { t!("window.no_sensors") } else { t!("window.loading_sensors") };
            ui.label(RichText::new(text).color(ui::muted(ui)));
            return None;
        }
        // What is folded away is not shown.
        let mut visible: Vec<&Row> = vec![];
        let mut hidden_below: Option<usize> = None;
        for row in &rows {
            if hidden_below.is_some_and(|d| row.depth > d) {
                continue;
            }
            hidden_below = None;
            visible.push(row);
            if !matches!(row.kind, RowKind::Value { .. }) && self.collapsed.contains(&row.key) {
                hidden_below = Some(row.depth);
            }
        }
        let with_percent = rows.iter().any(|r| matches!(&r.kind, RowKind::Value { second: Some(_), .. }));
        let bar_width = if with_percent { BAR_WITH_PERCENT } else { BAR_WIDTH };
        let keys: Vec<String> = visible.iter().map(|r| r.key.clone()).collect();
        self.keyboard(ctx, &visible, &keys);

        let mut hover = None;
        let mut rects: Vec<(String, egui::Rect)> = vec![];
        let mut clicked: Option<(String, egui::Modifiers)> = None;
        let mut toggled: Option<String> = None;
        // What to copy and the footer's message for it.
        let mut copied: Option<(String, String)> = None;
        let mut menu_action: Option<(&'static str, Option<String>)> = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for row in &visible {
                let width = ui.available_width();
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, ROW), Sense::click_and_drag());
                rects.push((row.key.clone(), rect));
                let painter = ui.painter_at(rect);
                let selected = self.selected.contains(&row.key);
                if selected {
                    painter.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill.gamma_multiply(0.6));
                } else if resp.hovered() {
                    painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
                }
                let indent = 6.0 + row.depth as f32 * 18.0;
                let y = rect.center().y;
                let text_color = ui.visuals().text_color();
                let pointer = resp.hover_pos();
                let mut over_copyable: Option<String> = None;
                match &row.kind {
                    RowKind::Device { .. } | RowKind::Section => {
                        let folded = self.collapsed.contains(&row.key);
                        let arrow = egui::Rect::from_center_size(egui::pos2(rect.min.x + indent + 6.0, y), egui::vec2(14.0, ROW));
                        painter.text(arrow.center(), Align2::CENTER_CENTER, if folded { "▸" } else { "▾" }, FontId::proportional(12.0), ui::muted(ui));
                        let device = matches!(row.kind, RowKind::Device { .. });
                        let font = if device { FontId::proportional(14.5) } else { FontId::proportional(12.0) };
                        let color = if device { ui::accent(ui) } else { ui::muted(ui) };
                        let name = painter.text(egui::pos2(arrow.max.x + 4.0, y), Align2::LEFT_CENTER, &row.text, font, color);
                        if resp.clicked() && pointer.is_some_and(|p| arrow.expand2(egui::vec2(4.0, 0.0)).contains(p)) {
                            toggled = Some(row.key.clone());
                        } else if device && pointer.is_some_and(|p| name.expand(3.0).contains(p)) {
                            over_copyable = Some(row.text.clone());
                        }
                        if resp.double_clicked() {
                            toggled = Some(row.key.clone());
                        }
                        if device && resp.hovered() {
                            hover = Some(Hover { explain: t!("window.click_name", name = row.text) });
                        }
                    }
                    RowKind::Value { sensor, second, device } => {
                        let (value, color) = shown(ui, &state.settings, sensor, second, device);
                        let bar_left = rect.max.x - bar_width;
                        let value_right = bar_left - 8.0;
                        painter.text(egui::pos2(rect.min.x + indent + 18.0, y), Align2::LEFT_CENTER, &row.text, FontId::proportional(13.0), text_color);
                        let value_rect = painter.text(egui::pos2(value_right, y), Align2::RIGHT_CENTER, &value, FontId::proportional(13.0), color);
                        let (bar_value, behind) = match second {
                            Some(s) => (Some(s.value), Some(fmt_value(&s.kind, s.value))),
                            None if BAR_TYPES.contains(&sensor.kind.as_str()) => (Some(sensor.value), None),
                            None => (None, None),
                        };
                        if let Some(v) = bar_value {
                            let bar = egui::Rect::from_min_size(egui::pos2(bar_left, y - 4.0), egui::vec2(70.0, 8.0));
                            painter.rect_filled(bar, 3.0, ui.visuals().extreme_bg_color);
                            let filled = egui::Rect::from_min_size(bar.min, egui::vec2(70.0 * (v.clamp(0.0, 100.0) / 100.0) as f32, 8.0));
                            painter.rect_filled(filled, 3.0, if second.is_some() { ui::accent(ui) } else { color.gamma_multiply(0.8) });
                            if let Some(b) = behind {
                                painter.text(egui::pos2(bar.max.x + 8.0, y), Align2::LEFT_CENTER, b, FontId::proportional(12.0), ui::muted(ui));
                            }
                        }
                        if pointer.is_some_and(|p| value_rect.expand(4.0).contains(p)) {
                            over_copyable = Some(value.clone());
                        }
                        if resp.hovered() {
                            let mut text = format!("{}: {}", sensor.name, self.content.explain(sensor));
                            if let Some(s) = second {
                                text += &format!(" {}", t!("window.the_bar", text = self.content.explain(s)));
                                if sensor.kind == "Fan" && sensor.value == 0.0 {
                                    text += &format!(" {}", t!("window.stopped_explained"));
                                }
                            }
                            hover = Some(Hover { explain: text });
                        }
                    }
                }
                if over_copyable.is_some() {
                    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() && toggled.as_ref() != Some(&row.key) {
                    let modifiers = ctx.input(|i| i.modifiers);
                    clicked = Some((row.key.clone(), modifiers));
                    if let (Some(text), false) = (&over_copyable, modifiers.ctrl || modifiers.shift) {
                        copied = Some((text.clone(), t!("window.copied", text = text)));
                    }
                }
                if resp.secondary_clicked() && !selected {
                    self.selected = BTreeSet::from([row.key.clone()]);
                    self.anchor = Some(row.key.clone());
                }
                if resp.drag_started() {
                    self.dragging = true;
                    self.anchor = Some(row.key.clone());
                }
                let copy_one = self.copy_one(row);
                resp.context_menu(|ui| {
                    let label = if matches!(row.kind, RowKind::Value { .. }) { t!("menu.copy_value") } else { t!("menu.copy_name") };
                    if ui.add_enabled(copy_one.is_some(), egui::Button::new(label)).clicked() {
                        menu_action = Some(("one", copy_one.clone()));
                        ui.close();
                    }
                    if ui.add(egui::Button::new(t!("menu.copy_selected")).shortcut_text(t!("menu.ctrl_c"))).clicked() {
                        menu_action = Some(("selected", None));
                        ui.close();
                    }
                    ui.separator();
                    if ui.add(egui::Button::new(t!("menu.select_all")).shortcut_text(t!("menu.ctrl_a"))).clicked() {
                        menu_action = Some(("all", None));
                        ui.close();
                    }
                });
            }
        });

        // Dragging with the button down selects everything between where it started and the row under the mouse.
        if self.dragging {
            if ctx.input(|i| i.pointer.primary_down()) {
                if let (Some(pos), Some(anchor)) = (ctx.input(|i| i.pointer.hover_pos()), self.anchor.clone()) {
                    let under = rects.iter().find(|(_, r)| pos.y >= r.min.y && pos.y < r.max.y).map(|(k, _)| k.clone()).or_else(|| {
                        if rects.first().is_some_and(|(_, r)| pos.y < r.min.y) { rects.first().map(|(k, _)| k.clone()) } else { rects.last().map(|(k, _)| k.clone()) }
                    });
                    if let Some(under) = under {
                        self.select_range(&keys, &anchor, &under);
                    }
                }
            } else {
                self.dragging = false;
            }
        }
        if let Some(key) = toggled {
            if !self.collapsed.remove(&key) {
                self.collapsed.insert(key);
            }
        } else if let Some((key, m)) = clicked {
            if m.ctrl {
                if !self.selected.remove(&key) {
                    self.selected.insert(key.clone());
                }
                self.anchor = Some(key);
            } else if m.shift {
                let anchor = self.anchor.clone().unwrap_or_else(|| key.clone());
                self.select_range(&keys, &anchor, &key);
            } else {
                self.selected = BTreeSet::from([key.clone()]);
                self.anchor = Some(key);
            }
        }
        if let Some((text, message)) = copied {
            self.copy(ctx, text, message);
        }
        match menu_action {
            Some(("one", Some(text))) => self.copy(ctx, text.clone(), t!("window.copied", text = text)),
            Some(("selected", _)) => self.copy_selected(ctx, &visible),
            Some(("all", _)) => self.selected = keys.iter().cloned().collect(),
            _ => {}
        }
        hover
    }

    fn select_range(&mut self, keys: &[String], from: &str, to: &str) {
        let (Some(a), Some(b)) = (keys.iter().position(|k| k == from), keys.iter().position(|k| k == to)) else { return };
        let (a, b) = (a.min(b), a.max(b));
        self.selected = keys[a..=b].iter().cloned().collect();
    }

    /// Ctrl+C, Ctrl+A, Escape.
    fn keyboard(&mut self, ctx: &egui::Context, visible: &[&Row], keys: &[String]) {
        let (copy, all, escape) = ctx.input(|i| {
            (
                i.events.iter().any(|e| matches!(e, egui::Event::Copy)),
                i.modifiers.command && i.key_pressed(Key::A),
                i.key_pressed(Key::Escape),
            )
        });
        if copy {
            self.copy_selected(ctx, visible);
        }
        if all {
            self.selected = keys.iter().cloned().collect();
        }
        if escape {
            self.selected.clear();
        }
    }

    /// What "Copy value" / "Copy name" copies for one row.
    fn copy_one(&self, row: &Row) -> Option<String> {
        match &row.kind {
            RowKind::Device { .. } => Some(row.text.clone()),
            RowKind::Section => None,
            RowKind::Value { sensor, second, .. } => {
                if second.is_some() && sensor.kind == "Fan" && sensor.value == 0.0 { Some(t!("value.stopped")) } else { Some(fmt_value(&sensor.kind, sensor.value)) }
            }
        }
    }

    /// The selected rows as text, indented like the list: "Name: value".
    fn copy_selected(&mut self, ctx: &egui::Context, visible: &[&Row]) {
        let rows: Vec<&&Row> = visible.iter().filter(|r| self.selected.contains(&r.key)).collect();
        if rows.is_empty() {
            self.confirm(t!("window.nothing_selected"), true);
            return;
        }
        let top = rows.iter().map(|r| r.depth).min().unwrap_or(0);
        let text = rows
            .iter()
            .map(|r| {
                let line = match &r.kind {
                    RowKind::Value { second, .. } => {
                        let mut v = self.copy_one(r).unwrap_or_default();
                        if let Some(s) = second {
                            v += &format!(", {}", fmt_value(&s.kind, s.value));
                        }
                        format!("{}: {v}", r.text)
                    }
                    _ => r.text.clone(),
                };
                format!("{}{line}", "  ".repeat(r.depth - top))
            })
            .collect::<Vec<_>>()
            .join("\n");
        let message = if rows.len() == 1 { t!("window.one_row_copied") } else { t!("window.rows_copied", n = rows.len()) };
        self.copy(ctx, text, message);
    }
}
