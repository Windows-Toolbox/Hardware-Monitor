//! The tab "Fans" of the detail window (from `python\fan_view.py`): the fans that can be controlled, and for the
//! selected one its mode (Auto / Curve / Fixed), the source temperature and a curve editor like in MSI Afterburner.
//!
//! Curve editor: x = temperature 20–100 °C, y = fan speed 0–100 %. Drag a point to move it (it stays between its
//! neighbors), double-click on an empty spot to add one (at most `fans::MAX_POINTS`), right-click a point to remove
//! it (at least 2 stay). A vertical line marks the current temperature with the speed the fan gets right now; from
//! the red limit of the traffic light on the fan always runs at 100 % (shaded red).
//! A change goes to the monitor at once (`ToMonitor::Fan`): it saves it and its controller applies it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, FontId, RichText, Sense, Stroke};
use kit::t;
use kit::ui;
use serde_json::json;

use crate::energy::num;
use crate::fans::{self, Config};
use crate::link::{Fan, State, ToMonitor};
use crate::power_view::shade;
use crate::window::ViewOut;

const T_MIN: i64 = 20;
const T_MAX: i64 = 100;
const MODES: [&str; 3] = ["auto", "curve", "fixed"];
const SOURCES: [&str; 3] = ["gpu", "cpu", "max"];
/// How long an edit counts over what the monitor sends – until its answer has the change.
const EDIT_WAIT: Duration = Duration::from_secs(3);
const ROW: f32 = 24.0;

/// The selected fan's settings as being edited.
#[derive(Clone, PartialEq)]
struct Edit {
    id: String,
    config: Config,
}

#[derive(Default)]
pub struct FanView {
    selected: Option<String>,
    edit: Option<Edit>,
    edited: Option<Instant>,
    /// The point being dragged.
    drag: Option<usize>,
}

/// The plot inside the editor's rect.
struct Plot {
    left: f32,
    top: f32,
    width: f32,
    height: f32,
}

impl Plot {
    fn to_xy(&self, temp: f64, pct: f64) -> egui::Pos2 {
        egui::pos2(self.left + ((temp - T_MIN as f64) / (T_MAX - T_MIN) as f64) as f32 * self.width, self.top + self.height - (pct / 100.0) as f32 * self.height)
    }

    fn from_xy(&self, p: egui::Pos2) -> [i64; 2] {
        let temp = T_MIN as f64 + ((p.x - self.left) / self.width) as f64 * (T_MAX - T_MIN) as f64;
        let pct = ((self.top + self.height - p.y) / self.height) as f64 * 100.0;
        [(temp.round() as i64).clamp(T_MIN, T_MAX), (pct.round() as i64).clamp(0, 100)]
    }
}

fn temperature(state: &State, source: &str) -> Option<f64> {
    let of = |kind: &str| state.temps.iter().find(|(k, _)| k == kind).and_then(|(_, v)| *v);
    match source {
        "max" => [of("cpu"), of("gpu")].into_iter().flatten().reduce(f64::max),
        s => of(s),
    }
}

fn hot_limit(state: &State, source: &str) -> i64 {
    let s = &state.settings;
    (match source {
        "max" => s.cpu_hot_temp.min(s.gpu_hot_temp),
        "gpu" => s.gpu_hot_temp,
        _ => s.cpu_hot_temp,
    }) as i64
}

fn item_name(fan: &Fan) -> String {
    if fan.kind == "gpu" { t!("fans.gpu_fans", name = fan.name) } else { fan.name.clone() }
}

fn mode_text(fan: &Fan, cfg: &Config) -> String {
    if cfg.mode == "auto" {
        return t!("fans.mode.auto");
    }
    let note = match fan.note.as_deref() {
        Some("hot") => Some(t!("fans.state.hot")),
        Some("no_temperature") => Some(t!("fans.state.no_temperature")),
        Some("error") => Some(t!("fans.state.error")),
        _ => None,
    };
    let text = if cfg.mode == "curve" {
        format!("{} · {}", t!("fans.mode.curve"), t!(&format!("fans.source.{}", cfg.source)))
    } else {
        format!("{} {} %", t!("fans.mode.fixed"), cfg.fixed)
    };
    match note {
        Some(n) => format!("{text} – {n}"),
        None => text,
    }
}

impl FanView {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &State, ready: bool, out: &mut ViewOut) {
        let fans = &state.fans;
        out.footer = Some(t!("fans.list.help"));
        let note = if !ready || state.loading {
            t!("window.loading_sensors")
        } else if fans.is_empty() {
            t!("fans.note.none")
        } else {
            t!("fans.note.intro")
        };
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(note).size(12.0).color(ui::muted(ui))).wrap());
        ui.add_space(6.0);
        if fans.is_empty() {
            self.selected = None;
            return;
        }
        if !self.selected.as_ref().is_some_and(|id| fans.iter().any(|f| &f.id == id)) {
            self.selected = Some(fans[0].id.clone());
        }
        let fan = fans.iter().find(|f| Some(&f.id) == self.selected.as_ref()).unwrap_or(&fans[0]).clone();
        // What the monitor says, unless the user just changed it here.
        let from_monitor = Config::from_settings(&fan.kind, fan.config.as_ref());
        let mine = self.edit.as_ref().is_some_and(|e| e.id == fan.id) && (self.drag.is_some() || self.edited.is_some_and(|t| t.elapsed() < EDIT_WAIT));
        if !mine {
            self.edit = Some(Edit { id: fan.id.clone(), config: from_monitor });
            self.edited = None;
        }

        self.list(ui, state, out);
        ui.add_space(10.0);
        self.editor(ui, state, &fan, out);
    }

    /// The fans: name, speed, control, mode – a click selects one.
    fn list(&mut self, ui: &mut egui::Ui, state: &State, out: &mut ViewOut) {
        let values: HashMap<&str, f64> = state.hardware.iter().flatten().flat_map(|h| h.sensors.iter()).map(|s| (s.id.as_str(), s.value)).collect();
        let font = FontId::proportional(13.0);
        for fan in &state.fans {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), Sense::click());
            let painter = ui.painter_at(rect);
            let selected = self.selected.as_ref() == Some(&fan.id);
            if selected {
                painter.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill.gamma_multiply(0.6));
            } else if resp.hovered() {
                painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
            }
            let color = if fan.software { ui::accent(ui) } else { ui.visuals().text_color() };
            let y = rect.center().y;
            let speeds: Vec<String> = fan.fan_ids.iter().filter_map(|i| values.get(i.as_str())).map(|v| format!("{v:.0}")).collect();
            let speed = if speeds.is_empty() { "–".to_string() } else { format!("{} RPM", speeds.join(" / ")) };
            let control = fan.control_ids.iter().filter_map(|i| values.get(i.as_str()).copied()).reduce(f64::max).map_or("–".to_string(), |v| format!("{v:.0} %"));
            let cfg = match &self.edit {
                Some(e) if e.id == fan.id => e.config.clone(),
                _ => Config::from_settings(&fan.kind, fan.config.as_ref()),
            };
            let mode_left = rect.max.x - 200.0;
            painter.text(egui::pos2(rect.min.x + 6.0, y), Align2::LEFT_CENTER, item_name(fan), font.clone(), color);
            painter.text(egui::pos2(mode_left - 70.0, y), Align2::RIGHT_CENTER, speed, font.clone(), color);
            painter.text(egui::pos2(mode_left - 10.0, y), Align2::RIGHT_CENTER, control, font.clone(), color);
            painter.text(egui::pos2(mode_left, y), Align2::LEFT_CENTER, mode_text(fan, &cfg), FontId::proportional(12.0), if fan.note.is_some() && cfg.mode != "auto" { ui::warn(ui) } else { ui::muted(ui) });
            if resp.hovered() {
                out.footer = Some(t!("fans.list.help"));
            }
            if resp.clicked() && !selected {
                self.selected = Some(fan.id.clone());
                self.edit = None;
                self.drag = None;
            }
        }
    }

    fn changed(&mut self, out: &mut ViewOut) {
        let Some(e) = &mut self.edit else { return };
        e.config.curve.sort();
        let c = &e.config;
        out.send = Some(ToMonitor::Fan { id: e.id.clone(), config: json!({"mode": c.mode, "source": c.source, "curve": c.curve, "fixed": c.fixed}) });
        self.edited = Some(Instant::now());
    }

    fn editor(&mut self, ui: &mut egui::Ui, state: &State, fan: &Fan, out: &mut ViewOut) {
        let Some(mut cfg) = self.edit.as_ref().map(|e| e.config.clone()) else { return };
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(t!("fans.mode")).color(ui::muted(ui)));
            for mode in MODES {
                let b = ui.add(egui::Button::new(t!(&format!("fans.mode.{mode}"))).selected(cfg.mode == mode));
                if b.hovered() {
                    out.footer = Some(t!(&format!("fans.mode.{mode}.help")));
                }
                if b.clicked() && cfg.mode != mode {
                    cfg.mode = mode.into();
                    changed = true;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let enabled = cfg.mode != "auto";
                for source in SOURCES.iter().rev() {
                    let b = ui.add_enabled(enabled, egui::Button::new(t!(&format!("fans.source.{source}"))).selected(cfg.source == *source));
                    if b.hovered() {
                        out.footer = Some(t!("fans.source.help"));
                    }
                    if b.clicked() && cfg.source != *source {
                        cfg.source = source.to_string();
                        changed = true;
                    }
                }
                ui.add_enabled(enabled, egui::Label::new(RichText::new(t!("fans.source")).color(ui::muted(ui))));
            });
        });
        match cfg.mode.as_str() {
            "fixed" => {
                ui.add_space(12.0);
                ui.spacing_mut().slider_width = (ui.available_width() - 80.0).max(100.0);
                let r = ui.add(egui::Slider::new(&mut cfg.fixed, 0..=100).suffix(" %"));
                if r.hovered() {
                    out.footer = Some(t!("fans.mode.fixed.help"));
                }
                // While dragging only shown; sent when let go (or typed / a click on the track).
                if r.drag_stopped() || (r.changed() && !r.dragged()) {
                    changed = true;
                }
                if r.dragged() {
                    self.edited = Some(Instant::now());
                }
            }
            "curve" => {
                ui.add_space(6.0);
                if self.curve(ui, state, fan, &mut cfg, out) {
                    changed = true;
                }
            }
            _ => {}
        }
        if fan.minimum > 0.0 && cfg.mode != "auto" {
            ui.add_space(4.0);
            let id = if fan.kind == "gpu" { "fans.minimum.gpu" } else { "fans.minimum" };
            ui.add(egui::Label::new(RichText::new(t!(id, pct = format!("{:.0}", fan.minimum))).size(12.0).color(ui::muted(ui))).wrap());
        }
        if let Some(e) = &mut self.edit {
            e.config = cfg;
        }
        if changed {
            self.changed(out);
        }
    }

    /// The curve editor. True when the curve changed and is to be sent.
    fn curve(&mut self, ui: &mut egui::Ui, state: &State, fan: &Fan, cfg: &mut Config, out: &mut ViewOut) -> bool {
        let mut send = false;
        // Below the chart: the live line and "Default curve".
        let below = 30.0;
        let size = egui::vec2(ui.available_width(), (ui.available_height() - below - if fan.minimum > 0.0 { 40.0 } else { 0.0 }).max(180.0));
        let (rect, resp) = ui.allocate_exact_size(size, Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
        let plot = Plot { left: rect.min.x + 44.0, top: rect.min.y + 10.0, width: rect.width() - 44.0 - 20.0, height: rect.height() - 10.0 - 22.0 };
        let (muted, grid, accent, bad) = (ui::muted(ui), ui.visuals().widgets.noninteractive.bg_stroke.color, ui::accent(ui), ui::bad(ui));
        let font = FontId::proportional(11.0);
        let bottom = plot.top + plot.height;

        let hot = hot_limit(state, &cfg.source);
        if hot < T_MAX {
            // From the red limit on: always 100 %.
            let (x_hot, x_end) = (plot.to_xy(hot as f64, 0.0).x, plot.to_xy(T_MAX as f64, 0.0).x);
            painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x_hot, plot.top), egui::pos2(x_end, bottom)), 0.0, bad.gamma_multiply(0.12));
            let label = t!("fans.red_limit", temp = hot);
            let galley = painter.layout_no_wrap(label.clone(), font.clone(), bad);
            // Does not fit into the red area: left of its border instead.
            let (x, align) = if x_hot + 4.0 + galley.size().x > x_end { (x_hot - 4.0, Align2::RIGHT_TOP) } else { (x_hot + 4.0, Align2::LEFT_TOP) };
            painter.text(egui::pos2(x, plot.top + 2.0), align, label, font.clone(), bad);
        }
        for temp in (T_MIN..=T_MAX).step_by(10) {
            let x = plot.to_xy(temp as f64, 0.0).x;
            painter.line_segment([egui::pos2(x, plot.top), egui::pos2(x, bottom)], Stroke::new(1.0, grid));
            if temp % 20 == 0 {
                painter.text(egui::pos2(x, bottom + 4.0), Align2::CENTER_TOP, format!("{temp} °C"), font.clone(), muted);
            }
        }
        for pct in (0..=100).step_by(10) {
            let y = plot.to_xy(T_MIN as f64, pct as f64).y;
            painter.line_segment([egui::pos2(plot.left, y), egui::pos2(plot.left + plot.width, y)], Stroke::new(1.0, grid));
            if pct % 20 == 0 {
                painter.text(egui::pos2(plot.left - 6.0, y), Align2::RIGHT_CENTER, format!("{pct} %"), font.clone(), muted);
            }
        }
        if fan.minimum > 0.0 {
            // The fan does not go below this.
            let y = plot.to_xy(T_MIN as f64, fan.minimum).y;
            painter.line_segment([egui::pos2(plot.left, y), egui::pos2(plot.left + plot.width, y)], Stroke::new(1.0, muted));
        }

        // Dragging, adding, removing.
        let pointer = resp.hover_pos();
        let point_at = |p: egui::Pos2, curve: &[[i64; 2]]| curve.iter().position(|&[t, c]| {
            let q = plot.to_xy(t as f64, c as f64);
            (q.x - p.x).abs() <= 10.0 && (q.y - p.y).abs() <= 10.0
        });
        if resp.drag_started() {
            self.drag = ui.ctx().input(|i| i.pointer.press_origin()).and_then(|p| point_at(p, &cfg.curve));
        }
        if let (Some(i), true, Some(p)) = (self.drag, resp.dragged(), ui.ctx().input(|i| i.pointer.interact_pos())) {
            if i < cfg.curve.len() {
                let [temp, pct] = plot.from_xy(p);
                // It stays between its neighbors, at least 1 °C apart.
                let low = if i > 0 { cfg.curve[i - 1][0] + 1 } else { T_MIN };
                let high = if i + 1 < cfg.curve.len() { cfg.curve[i + 1][0] - 1 } else { T_MAX };
                cfg.curve[i] = [temp.clamp(low, high.max(low)), pct];
                self.edited = Some(Instant::now());
            }
        }
        if resp.drag_stopped() && self.drag.take().is_some() {
            send = true;
        }
        let hot_point = self.drag.or_else(|| pointer.and_then(|p| point_at(p, &cfg.curve)));
        if resp.double_clicked()
            && let Some(p) = pointer
            && point_at(p, &cfg.curve).is_none()
            && cfg.curve.len() < fans::MAX_POINTS
        {
            let new = plot.from_xy(p);
            if !cfg.curve.iter().any(|q| q[0] == new[0]) {
                cfg.curve.push(new);
                cfg.curve.sort();
                send = true;
            }
        }
        if resp.secondary_clicked()
            && let Some(i) = pointer.and_then(|p| point_at(p, &cfg.curve))
            && cfg.curve.len() > 2
        {
            cfg.curve.remove(i);
            send = true;
        }

        // The curve: flat before the first and after the last point.
        let mut line = vec![plot.to_xy(T_MIN as f64, cfg.curve[0][1] as f64)];
        line.extend(cfg.curve.iter().map(|&[t, c]| plot.to_xy(t as f64, c as f64)));
        line.push(plot.to_xy(T_MAX as f64, cfg.curve[cfg.curve.len() - 1][1] as f64));
        painter.add(egui::Shape::line(line, Stroke::new(2.0, accent)));

        // Live: the current temperature and what the fan gets now.
        let temp = temperature(state, &cfg.source);
        let live = match temp {
            Some(temp) => {
                let pct = if temp >= hot as f64 { 100.0 } else { fans::curve_value(&cfg.curve, temp) }.clamp(fan.minimum, fan.maximum.max(fan.minimum));
                let at = plot.to_xy(temp.clamp(T_MIN as f64, T_MAX as f64), pct);
                painter.line_segment([egui::pos2(at.x, plot.top), egui::pos2(at.x, bottom)], Stroke::new(1.0, muted));
                painter.circle(at, 4.0, ui.visuals().strong_text_color(), Stroke::new(2.0, ui.visuals().extreme_bg_color));
                let applied = match (fan.software, fan.pct) {
                    (true, Some(p)) => format!("{p:.0} %"),
                    _ => "–".to_string(),
                };
                t!("fans.live", temp = num(temp, 0), curve = num(pct, 0), applied = applied)
            }
            None => t!("fans.state.no_temperature"),
        };
        for (i, &[t, c]) in cfg.curve.iter().enumerate() {
            let color = if hot_point == Some(i) { ui.visuals().strong_text_color() } else { shade(accent, 0.85) };
            painter.circle(plot.to_xy(t as f64, c as f64), 5.0, color, Stroke::new(2.0, ui.visuals().extreme_bg_color));
        }
        if let Some(i) = hot_point {
            ui.ctx().set_cursor_icon(if self.drag.is_some() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
            let [t, c] = cfg.curve[i];
            out.footer = Some(t!("fans.point.help", temp = t, pct = c));
        } else if resp.hovered() {
            out.footer = Some(t!("fans.editor.help"));
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new(live).size(12.0).color(ui::muted(ui)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = ui.button(t!("fans.reset_curve"));
                if b.hovered() {
                    out.footer = Some(t!("fans.reset_curve.help"));
                }
                if b.clicked() {
                    cfg.curve = Config::default_for(&fan.kind).curve;
                    send = true;
                }
            });
        });
        send
    }
}
