//! The tab "Fans" of the detail window: a switch Graphics card | Motherboard | CPU at the top, the fans of that
//! group, and below them the editor of the selected ones: mode (Auto / Curve / Fixed), the source temperature and a
//! curve editor like in MSI Afterburner.
//!
//! The groups: the graphics card is one item – its fan channels follow one setting together and are listed as
//! channels (the driver reports channels, not fans: three fans can hang on two channels). The motherboard's headers
//! can be given a name (LHM only knows "Fan #2") and be marked as CPU fans; those are listed under "CPU" instead. A
//! header that has not turned since the window opened (and is not controlled) folds away at the bottom.
//!
//! Selecting: a click selects one fan, Ctrl+click adds or removes one, Shift+click a range. With several selected
//! the editor shows the first one's settings, and every change goes to all of them – each keeps its own curve.
//! "Boost" runs a header at 100 % for `fans::BOOST` (the monitor ends it by itself) – to hear which fan it is.
//!
//! Curve editor: x = temperature 20–100 °C, y = fan speed 0–100 %. Drag a point to move it (it stays between its
//! neighbors), double-click on an empty spot to add one (at most `fans::MAX_POINTS`), right-click a point to remove
//! it (at least 2 stay). A vertical line marks the current temperature with the speed the fan gets right now; from
//! the red limit of the traffic light on the fan always runs at 100 % (shaded red).
//! A change goes to the monitor at once (`ToMonitor::Fan`): it saves it and its controller applies it.

use std::collections::{HashMap, HashSet};
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
const SUB_ROW: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Group {
    Gpu,
    Board,
    Cpu,
}

const GROUPS: [Group; 3] = [Group::Gpu, Group::Board, Group::Cpu];

impl Group {
    fn key(self) -> &'static str {
        match self {
            Group::Gpu => "gpu",
            Group::Board => "board",
            Group::Cpu => "cpu",
        }
    }
}

/// The fans sorted into the groups.
#[derive(Default)]
struct Sorted {
    gpu: Vec<Fan>,
    board: Vec<Fan>,
    cpu: Vec<Fan>,
    /// Motherboard headers that do not turn – folded away under "Motherboard".
    unused: Vec<Fan>,
}

impl Sorted {
    fn of(&self, group: Group) -> &[Fan] {
        match group {
            Group::Gpu => &self.gpu,
            Group::Board => &self.board,
            Group::Cpu => &self.cpu,
        }
    }
}

/// The shown fan's settings as being edited.
#[derive(Clone, PartialEq)]
struct Edit {
    id: String,
    config: Config,
}

#[derive(Default)]
pub struct FanView {
    group: Option<Group>,
    /// Per group the selected fans; the first one is shown in the editor.
    selected: HashMap<Group, Vec<String>>,
    /// Where a Shift+click range starts.
    anchor: Option<String>,
    edit: Option<Edit>,
    edited: Option<Instant>,
    /// The point being dragged.
    drag: Option<usize>,
    /// Headers seen turning since the window opened – they stay in the list when they stop.
    turned: HashSet<String>,
    unused_open: bool,
    /// The name being typed: (fan, text).
    typing: Option<(String, String)>,
    /// Names and CPU marks just changed here, until the monitor's answer has them.
    names: HashMap<String, (String, Instant)>,
    marks: HashMap<String, (bool, Instant)>,
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

fn mode_text(fan: &Fan, cfg: &Config) -> String {
    if fan.note.as_deref() == Some("boost") {
        return t!("fans.state.boost");
    }
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
    /// The name given to a header (just typed here, or the monitor's) – "" = none.
    fn given(&self, fan: &Fan) -> String {
        match self.names.get(&fan.id) {
            Some((n, t)) if t.elapsed() < EDIT_WAIT => n.clone(),
            _ => fan.label.trim().to_string(),
        }
    }

    /// What a fan is called: its given name, else LHM's.
    fn label(&self, fan: &Fan) -> String {
        let given = self.given(fan);
        if given.is_empty() { fan.name.clone() } else { given }
    }

    fn is_cpu(&self, fan: &Fan) -> bool {
        fan.kind == "case"
            && match self.marks.get(&fan.id) {
                Some((on, t)) if t.elapsed() < EDIT_WAIT => *on,
                _ => fan.cpu,
            }
    }

    /// The edit counts over the monitor's word: being dragged, or changed just now.
    fn editing(&self) -> bool {
        self.drag.is_some() || self.edited.is_some_and(|t| t.elapsed() < EDIT_WAIT)
    }

    /// A fan's settings as the list shows them: the edit for the selected ones while it counts.
    fn shown_config(&self, fan: &Fan, group: Group) -> Config {
        match &self.edit {
            Some(e) if self.editing() && self.selected.get(&group).is_some_and(|s| s.contains(&fan.id)) => e.config.clone(),
            _ => Config::from_settings(&fan.kind, fan.config.as_ref()),
        }
    }

    fn sort(&mut self, fans: &[Fan], values: &HashMap<String, f64>) -> Sorted {
        let mut s = Sorted::default();
        for fan in fans {
            if fan.kind == "gpu" {
                s.gpu.push(fan.clone());
                continue;
            }
            let speeds: Vec<f64> = fan.fan_ids.iter().filter_map(|i| values.get(i).copied()).collect();
            if speeds.iter().any(|v| *v > 0.0) {
                self.turned.insert(fan.id.clone());
            }
            let controlled = fan.software || Config::from_settings(&fan.kind, fan.config.as_ref()).mode != "auto";
            if self.is_cpu(fan) {
                s.cpu.push(fan.clone());
            } else if !fan.fan_ids.is_empty() && speeds.len() == fan.fan_ids.len() && !self.turned.contains(&fan.id) && !controlled {
                s.unused.push(fan.clone());
            } else {
                s.board.push(fan.clone());
            }
        }
        s
    }

    /// What a group's list shows (the folded headers too, while open).
    fn members(&self, group: Group, sorted: &Sorted) -> Vec<Fan> {
        let mut m = sorted.of(group).to_vec();
        if group == Group::Board && self.unused_open {
            m.extend(sorted.unused.iter().cloned());
        }
        m
    }

    /// The group's selection: only fans it has, at least one.
    fn selection(&mut self, group: Group, sorted: &Sorted) -> Vec<Fan> {
        let mut all = sorted.of(group).to_vec();
        if group == Group::Board {
            all.extend(sorted.unused.iter().cloned());
        }
        let sel = self.selected.entry(group).or_default();
        sel.retain(|id| all.iter().any(|f| &f.id == id));
        if sel.is_empty()
            && let Some(first) = sorted.of(group).first()
        {
            sel.push(first.id.clone());
        }
        sel.iter().filter_map(|id| all.iter().find(|f| &f.id == id).cloned()).collect()
    }

    fn select_group(&mut self, group: Group) {
        if self.group != Some(group) {
            self.group = Some(group);
            self.after_selecting();
        }
    }

    fn after_selecting(&mut self) {
        self.edit = None;
        self.edited = None;
        self.drag = None;
        self.typing = None;
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &State, ready: bool, out: &mut ViewOut) {
        let fans = &state.fans;
        out.footer = Some(t!("fans.list.help"));
        if fans.is_empty() {
            let note = if !ready || state.loading { t!("window.loading_sensors") } else { t!("fans.note.none") };
            ui.add_space(6.0);
            ui.add(egui::Label::new(RichText::new(note).size(12.0).color(ui::muted(ui))).wrap());
            return;
        }
        let values: HashMap<String, f64> = state.hardware.iter().flatten().flat_map(|h| h.sensors.iter()).map(|s| (s.id.clone(), s.value)).collect();
        let sorted = self.sort(fans, &values);
        let group = *self.group.get_or_insert_with(|| GROUPS.into_iter().find(|g| !sorted.of(*g).is_empty()).unwrap_or(Group::Board));

        ui.add_space(6.0);
        self.switcher(ui, &sorted, out);
        let group = self.group.unwrap_or(group);
        ui.add_space(4.0);
        ui.add(egui::Label::new(RichText::new(t!(&format!("fans.group.{}.note", group.key()))).size(12.0).color(ui::muted(ui))).wrap());
        ui.add_space(6.0);

        if sorted.of(group).is_empty() && !(group == Group::Board && !sorted.unused.is_empty()) {
            self.empty(ui, group, &sorted, out);
            return;
        }
        self.selection(group, &sorted);
        self.list(ui, group, &sorted, &values, out);
        if self.group != Some(group) {
            ui.ctx().request_repaint();
            return;
        }
        let chosen = self.selection(group, &sorted);
        let Some(lead) = chosen.first().cloned() else { return };
        // What the monitor says, unless the user just changed it here.
        let mine = self.edit.as_ref().is_some_and(|e| e.id == lead.id) && self.editing();
        if !mine {
            self.edit = Some(Edit { id: lead.id.clone(), config: Config::from_settings(&lead.kind, lead.config.as_ref()) });
            self.edited = None;
        }
        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);
        self.editor(ui, state, &chosen, out);
    }

    /// Graphics card | Motherboard | CPU, with how many fans each has.
    fn switcher(&mut self, ui: &mut egui::Ui, sorted: &Sorted, out: &mut ViewOut) {
        let choices: Vec<(String, Option<String>)> = GROUPS
            .iter()
            .map(|g| (t!(&format!("fans.group.{}", g.key())), (*g != Group::Gpu).then(|| sorted.of(*g).len().to_string())))
            .collect();
        let chosen = GROUPS.iter().position(|g| self.group == Some(*g)).unwrap_or(0);
        let (clicked, hovered) = ui::segmented(ui, "fans_group", &choices, chosen);
        if let Some(i) = hovered {
            out.footer = Some(t!(&format!("fans.group.{}.note", GROUPS[i].key())));
        }
        if let Some(i) = clicked {
            self.select_group(GROUPS[i]);
        }
    }

    /// A group without fans: what to do.
    fn empty(&mut self, ui: &mut egui::Ui, group: Group, sorted: &Sorted, out: &mut ViewOut) {
        match group {
            Group::Cpu => {
                ui.label(RichText::new(t!("fans.cpu.none")).strong());
                ui.add_space(2.0);
                ui.add(egui::Label::new(RichText::new(t!("fans.cpu.steps")).color(ui::muted(ui))).wrap());
                ui.add_space(6.0);
                if !sorted.board.is_empty() || !sorted.unused.is_empty() {
                    let b = ui.button(t!("fans.cpu.to_board"));
                    if b.hovered() {
                        out.footer = Some(t!("fans.group.board.note"));
                    }
                    if b.clicked() {
                        self.select_group(Group::Board);
                    }
                }
            }
            Group::Gpu => {
                ui.add(egui::Label::new(RichText::new(t!("fans.gpu.none")).color(ui::muted(ui))).wrap());
            }
            Group::Board => {
                let text = if sorted.cpu.is_empty() { t!("fans.board.none") } else { t!("fans.board.all_cpu") };
                ui.add(egui::Label::new(RichText::new(text).color(ui::muted(ui))).wrap());
            }
        }
    }

    /// The fans: name, speed, control, mode – a click selects one, Ctrl / Shift more.
    fn list(&mut self, ui: &mut egui::Ui, group: Group, sorted: &Sorted, values: &HashMap<String, f64>, out: &mut ViewOut) {
        let members = self.members(group, sorted);
        let muted = ui::muted(ui);
        ui.spacing_mut().item_spacing.y = 0.0;

        // The header: the columns, and "select all".
        let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 18.0), Sense::hover());
        let cols = Columns::new(rect);
        let painter = ui.painter_at(rect);
        let small = FontId::proportional(11.0);
        let y = rect.center().y;
        // The column headings: small capitals – left, right, right, left aligned.
        let caption = |text: String, x: f32, right: bool| {
            let g = ui::caption_galley(ui, &text);
            let at = egui::pos2(if right { x - g.size().x } else { x }, y - g.size().y / 2.0);
            let r = egui::Rect::from_min_size(at, g.size());
            painter.galley(at, g, muted);
            r
        };
        let head = caption(t!("fans.column.fan"), rect.min.x + 6.0, false);
        caption(t!("fans.column.speed"), cols.speed, true);
        caption(t!("fans.column.control"), cols.control, true);
        caption(t!("fans.column.mode"), cols.mode, false);
        let plain = sorted.of(group);
        if plain.len() > 1 {
            let selected = self.selected.get(&group).cloned().unwrap_or_default();
            let all = plain.iter().all(|f| selected.contains(&f.id));
            let text = if all { t!("fans.select_one") } else { t!("fans.select_all") };
            let accent = ui::accent(ui);
            let galley = painter.layout_no_wrap(text, small.clone(), accent);
            let at = egui::pos2(head.max.x + 12.0, y - galley.size().y / 2.0);
            let link = egui::Rect::from_min_size(at, galley.size());
            let resp = ui.interact(link.expand(2.0), ui.make_persistent_id("fans_select_all"), Sense::click());
            painter.line_segment([egui::pos2(link.min.x, link.max.y), link.max], Stroke::new(1.0, accent.gamma_multiply(if resp.hovered() { 1.0 } else { 0.5 })));
            painter.galley(at, galley, accent);
            if resp.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                out.footer = Some(t!("fans.select_all.help"));
            }
            if resp.clicked() {
                let ids: Vec<String> = if all { vec![plain[0].id.clone()] } else { plain.iter().map(|f| f.id.clone()).collect() };
                self.anchor = ids.first().cloned();
                self.selected.insert(group, ids);
                self.after_selecting();
            }
        }

        let unused_from = sorted.of(group).len();
        for (index, fan) in members.iter().enumerate() {
            if group == Group::Board && index == unused_from {
                self.fold(ui, sorted, out);
            }
            self.row(ui, group, fan, &members, values, out);
        }
        if group == Group::Board {
            if members.len() == unused_from && !sorted.unused.is_empty() {
                self.fold(ui, sorted, out);
            }
            if !sorted.cpu.is_empty() {
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.label(RichText::new(t!("fans.under_cpu")).size(12.0).color(muted));
                    for fan in &sorted.cpu {
                        let link = ui.link(RichText::new(self.label(fan)).size(12.0));
                        if link.hovered() {
                            out.footer = Some(t!("fans.under_cpu.help"));
                        }
                        if link.clicked() {
                            self.select_group(Group::Cpu);
                            self.selected.insert(Group::Cpu, vec![fan.id.clone()]);
                            self.anchor = Some(fan.id.clone());
                        }
                    }
                });
            }
        }
    }

    /// "Not turning: Fan #1, Fan #5" – a click folds them out or away.
    fn fold(&mut self, ui: &mut egui::Ui, sorted: &Sorted, out: &mut ViewOut) {
        ui.add_space(4.0);
        let names: Vec<String> = sorted.unused.iter().map(|f| self.label(f)).collect();
        let arrow = if self.unused_open { "▾" } else { "▸" };
        let text = format!("{arrow} {}", t!("fans.unused", names = names.join(", ")));
        let r = ui.add(egui::Label::new(RichText::new(text).size(12.0).color(ui::muted(ui))).sense(Sense::click()));
        if r.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            out.footer = Some(t!("fans.unused.help"));
        }
        if r.clicked() {
            self.unused_open = !self.unused_open;
        }
        ui.add_space(2.0);
    }

    fn row(&mut self, ui: &mut egui::Ui, group: Group, fan: &Fan, members: &[Fan], values: &HashMap<String, f64>, out: &mut ViewOut) {
        let font = FontId::proportional(13.0);
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), Sense::click());
        let cols = Columns::new(rect);
        let painter = ui.painter_at(rect);
        let selected = self.selected.get(&group).is_some_and(|s| s.contains(&fan.id));
        if selected {
            painter.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill.gamma_multiply(0.6));
        } else if resp.hovered() {
            painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
        }
        let color = if fan.software { ui::accent(ui) } else { ui.visuals().text_color() };
        let y = rect.center().y;
        let cfg = self.shown_config(fan, group);
        let control = fan.control_ids.iter().filter_map(|i| values.get(i).copied()).reduce(f64::max).map_or("–".to_string(), |v| format!("{v:.0} %"));
        let given = self.given(fan);
        let name = if given.is_empty() { fan.name.clone() } else { given.clone() };
        let at = painter.text(egui::pos2(rect.min.x + 6.0, y), Align2::LEFT_CENTER, name, font.clone(), color);
        if !given.is_empty() {
            painter.text(egui::pos2(at.max.x + 8.0, y), Align2::LEFT_CENTER, &fan.name, FontId::proportional(11.5), ui::muted(ui));
        }
        if fan.kind != "gpu" {
            let speed = fan.fan_ids.iter().filter_map(|i| values.get(i)).map(|v| format!("{v:.0}")).collect::<Vec<_>>();
            let speed = if speed.is_empty() { "–".to_string() } else { format!("{} RPM", speed.join(" / ")) };
            painter.text(egui::pos2(cols.speed, y), Align2::RIGHT_CENTER, speed, font.clone(), color);
        }
        painter.text(egui::pos2(cols.control, y), Align2::RIGHT_CENTER, control, font.clone(), color);
        let warn = fan.note.as_deref().is_some_and(|n| n != "boost") && cfg.mode != "auto";
        let mode_color = if warn {
            ui::warn(ui)
        } else if fan.note.as_deref() == Some("boost") {
            ui::accent(ui)
        } else {
            ui::muted(ui)
        };
        painter.text(egui::pos2(cols.mode, y), Align2::LEFT_CENTER, mode_text(fan, &cfg), FontId::proportional(12.0), mode_color);
        if resp.hovered() {
            out.footer = Some(t!("fans.list.help"));
        }
        if resp.clicked() {
            let m = ui.input(|i| i.modifiers);
            self.click(group, fan, members, m);
        }

        // The graphics card: its channels below it.
        if fan.kind == "gpu" {
            let per_channel = fan.control_ids.len() == fan.fan_ids.len();
            for (i, id) in fan.fan_ids.iter().enumerate() {
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), SUB_ROW), Sense::hover());
                let painter = ui.painter_at(rect);
                let y = rect.center().y;
                let muted = ui::muted(ui);
                let text = painter.text(egui::pos2(rect.min.x + 26.0, y), Align2::LEFT_CENTER, t!("fans.channel", n = i + 1), FontId::proportional(12.5), muted);
                let info = egui::pos2(text.max.x + 10.0, y);
                painter.circle_stroke(info, 6.5, Stroke::new(1.0, muted));
                painter.text(info, Align2::CENTER_CENTER, "i", FontId::proportional(10.0), muted);
                let rpm = values.get(id).map_or("–".to_string(), |v| format!("{v:.0} RPM"));
                painter.text(egui::pos2(cols.speed, y), Align2::RIGHT_CENTER, rpm, FontId::proportional(12.5), color);
                if per_channel && let Some(v) = values.get(&fan.control_ids[i]) {
                    painter.text(egui::pos2(cols.control, y), Align2::RIGHT_CENTER, format!("{v:.0} %"), FontId::proportional(12.5), muted);
                }
                if resp.hovered() {
                    out.footer = Some(t!("fans.channel.help", n = fan.fan_ids.len()));
                }
            }
        }
    }

    /// A click on a row: one fan, Ctrl = one more or less, Shift = from the last clicked one to here.
    fn click(&mut self, group: Group, fan: &Fan, members: &[Fan], m: egui::Modifiers) {
        let sel = self.selected.entry(group).or_default();
        let before = sel.clone();
        let anchor = self.anchor.as_ref().and_then(|a| members.iter().position(|f| &f.id == a));
        let here = members.iter().position(|f| f.id == fan.id).unwrap_or(0);
        if m.shift && let Some(a) = anchor {
            let (from, to) = (a.min(here), a.max(here));
            // The anchor stays first – its settings are the ones shown.
            let mut ids = vec![members[a].id.clone()];
            ids.extend(members[from..=to].iter().filter(|f| f.id != members[a].id).map(|f| f.id.clone()));
            *sel = ids;
        } else if m.command {
            if let Some(i) = sel.iter().position(|id| *id == fan.id) {
                if sel.len() > 1 {
                    sel.remove(i);
                }
            } else {
                sel.push(fan.id.clone());
            }
            self.anchor = Some(fan.id.clone());
        } else {
            *sel = vec![fan.id.clone()];
            self.anchor = Some(fan.id.clone());
        }
        if self.selected.get(&group).and_then(|s| s.first()) != before.first() {
            self.after_selecting();
        }
    }

    fn changed(&mut self, ids: &[String], out: &mut ViewOut) {
        let Some(e) = &mut self.edit else { return };
        e.config.curve.sort();
        let c = &e.config;
        let config = json!({"mode": c.mode, "source": c.source, "curve": c.curve, "fixed": c.fixed});
        out.send.extend(ids.iter().map(|id| ToMonitor::Fan { id: id.clone(), config: config.clone() }));
        self.edited = Some(Instant::now());
    }

    /// The header of the editor: one header's name, CPU mark and "Boost" – or which fans are edited together.
    fn head(&mut self, ui: &mut egui::Ui, chosen: &[Fan], cfg: &Config, out: &mut ViewOut) {
        let fan = &chosen[0];
        let muted = ui::muted(ui);
        if chosen.len() > 1 {
            let names: Vec<String> = chosen.iter().map(|f| self.label(f)).collect();
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(t!("fans.together", n = chosen.len())).strong());
                ui.label(RichText::new(names.join(", ")).size(12.0).color(muted));
            });
            let differ: Vec<String> = chosen[1..].iter().filter(|f| Config::from_settings(&f.kind, f.config.as_ref()) != *cfg).map(|f| self.label(f)).collect();
            let text = if differ.is_empty() || self.editing() {
                t!("fans.together.same", n = chosen.len())
            } else {
                t!("fans.together.differ", n = chosen.len(), name = self.label(fan), others = differ.join(", "))
            };
            ui.add(egui::Label::new(RichText::new(text).size(12.0).color(ui::accent(ui))).wrap());
            ui.add_space(4.0);
            return;
        }
        let given = self.given(fan);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            ui.label(RichText::new(if given.is_empty() { fan.name.clone() } else { given.clone() }).strong());
            if fan.kind != "case" {
                return;
            }
            if !given.is_empty() {
                ui.label(RichText::new(&fan.name).size(12.0).color(muted));
            }
            ui.add_space(12.0);
            ui.label(RichText::new(t!("fans.name")).color(muted));
            let mut text = match &self.typing {
                Some((id, t)) if *id == fan.id => t.clone(),
                _ => given.clone(),
            };
            let r = ui.add(egui::TextEdit::singleline(&mut text).hint_text(&fan.name).desired_width(150.0).char_limit(40));
            if r.hovered() || r.has_focus() {
                out.footer = Some(t!("fans.name.help", name = fan.name));
            }
            if r.changed() || r.has_focus() {
                self.typing = Some((fan.id.clone(), text.clone()));
            }
            if r.lost_focus() {
                let name = text.trim().to_string();
                if name != given {
                    out.send.push(ToMonitor::FanName { id: fan.id.clone(), name: name.clone() });
                    self.names.insert(fan.id.clone(), (name, Instant::now()));
                }
                self.typing = None;
            }
            let mut cpu = self.is_cpu(fan);
            let c = ui.checkbox(&mut cpu, t!("fans.cpu_fan"));
            if c.hovered() {
                out.footer = Some(t!("fans.cpu_fan.help"));
            }
            if c.changed() {
                out.send.push(ToMonitor::CpuFan { id: fan.id.clone(), on: cpu });
                self.marks.insert(fan.id.clone(), (cpu, Instant::now()));
                let to = if cpu { Group::Cpu } else { Group::Board };
                self.select_group(to);
                self.selected.insert(to, vec![fan.id.clone()]);
                self.anchor = Some(fan.id.clone());
            }
            let boosting = fan.note.as_deref() == Some("boost");
            let b = ui.add_enabled(!boosting, egui::Button::new(t!("fans.boost")));
            if b.hovered() {
                out.footer = Some(t!("fans.boost.help", seconds = fans::BOOST.as_secs()));
            }
            if b.clicked() {
                out.send.push(ToMonitor::Boost { id: fan.id.clone() });
            }
        });
        ui.add_space(4.0);
    }

    fn editor(&mut self, ui: &mut egui::Ui, state: &State, chosen: &[Fan], out: &mut ViewOut) {
        let Some(mut cfg) = self.edit.as_ref().map(|e| e.config.clone()) else { return };
        let fan = &chosen[0];
        let ids: Vec<String> = chosen.iter().map(|f| f.id.clone()).collect();
        ui.spacing_mut().item_spacing.y = 4.0;
        self.head(ui, chosen, &cfg, out);
        if self.group.is_some_and(|g| !self.selected.get(&g).is_some_and(|s| s.first() == Some(&fan.id))) {
            return; // marked as CPU fan just now: shown in its new group next frame
        }
        let mut changed = false;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(t!("fans.mode")).color(ui::muted(ui)));
            for mode in MODES {
                let b = ui.add(egui::Button::new(t!(&format!("fans.mode.{mode}"))).selected(cfg.mode == mode));
                if b.hovered() {
                    out.footer = Some(t!(&format!("fans.mode.{mode}.help")));
                }
                if b.clicked() && (cfg.mode != mode || chosen.len() > 1) {
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
                    if b.clicked() && (cfg.source != *source || chosen.len() > 1) {
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
            self.changed(&ids, out);
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

/// Where the list's columns stand (right edges of speed and control, left edge of the mode).
struct Columns {
    speed: f32,
    control: f32,
    mode: f32,
}

impl Columns {
    fn new(rect: egui::Rect) -> Columns {
        let mode = rect.max.x - 200.0;
        Columns { speed: mode - 92.0, control: mode - 14.0, mode }
    }
}
