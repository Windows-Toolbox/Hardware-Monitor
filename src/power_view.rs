//! The tab "Power" of the detail window: the power right now (with a line of the last two minutes), today, this
//! month (and the month at the same pace) and how long the PC has been on, and a chart of the log – per minute for
//! the last 60 minutes (in W, from the monitor's memory), per hour for the last 24 hours, per day for the last 30
//! days, per month for the last 12 months.
//!
//! Each bar is stacked (`energy::bars`): CPU + GPU as their sensors measure them; above them everything else the
//! smart plug measured; where the plug did not measure (or there is none) the rest of the PC as estimated. The bar
//! still running carries, dashed, what is expected until its end: the hour at its pace so far, the day up to the
//! average of the finished days, the month up to the forecast. A dashed line marks the average of the finished bars.
//! The setting `power_chart` draws separate lines instead (the total, CPU + GPU, the rest); there the running
//! period's point stands where it is expected to end (dashed), or – too early to tell – not yet, so that a new hour
//! does not pull the lines down. The axis counts energy or – switched – what it costs. Hovering over a bar: its values in the footer; "Copy table" copies them all
//! (tab-separated, e.g. for Excel).

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke};
use kit::t;
use kit::ui;

use crate::energy::{self, Bar, num};
use crate::link::{Settings, State};
use crate::window::ViewOut;

/// How far back the line in the tile "Now" goes.
const SPARK: Duration = Duration::from_secs(120);
/// The colour of "everything else" – a violet that stands apart from the accent's blue.
const VIOLET: Color32 = Color32::from_rgb(0xb4, 0x9c, 0xf0);
const VIOLET_ON_LIGHT: Color32 = Color32::from_rgb(0x7c, 0x5c, 0xc4);

pub struct PowerView {
    view: &'static str,
    /// The axis in money instead of energy.
    money: bool,
    /// The power of the last two minutes, for the line in the tile "Now" – only while the window is open.
    spark: VecDeque<(Instant, f64)>,
}

impl Default for PowerView {
    fn default() -> Self {
        PowerView { view: "day", money: false, spark: VecDeque::new() }
    }
}

/// Darker, for the bars that are not under the mouse.
pub fn shade(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb((c.r() as f32 * f) as u8, (c.g() as f32 * f) as u8, (c.b() as f32 * f) as u8)
}

fn violet(ui: &egui::Ui) -> Color32 {
    if ui.visuals().dark_mode { VIOLET } else { VIOLET_ON_LIGHT }
}

/// The three parts of a bar, bottom up: CPU + GPU, the rest at the plug, the estimated rest of the PC.
fn part_colors(ui: &egui::Ui) -> [Color32; 3] {
    [ui::accent(ui), violet(ui), ui::muted(ui).gamma_multiply(0.7)]
}

/// What the axis counts in: Wh / kWh, or money.
struct Axis {
    /// Wh per unit.
    per: f64,
    unit: String,
    decimals: usize,
}

impl Axis {
    fn value(&self, wh: f64) -> f64 {
        wh / self.per
    }
}

impl PowerView {
    fn axis(&self, s: &Settings) -> Axis {
        if self.view == "hour" {
            // Wh per minute × 60 = the minute's average power.
            Axis { per: 1.0 / 60.0, unit: "W".into(), decimals: 0 }
        } else if self.money && s.energy_price > 0.0 {
            Axis { per: 1000.0 / s.energy_price, unit: currency(s), decimals: 2 }
        } else if self.view == "day" {
            Axis { per: 1.0, unit: "Wh".into(), decimals: 0 }
        } else {
            Axis { per: 1000.0, unit: "kWh".into(), decimals: 1 }
        }
    }

    /// What the running bar (the last one) is expected to add until its end.
    fn expected(&self, bars: &[Bar], forecast: Option<f64>, minute: u32) -> f64 {
        let Some((last, done)) = bars.split_last() else { return 0.0 };
        let total = last.total();
        if total <= 0.0 {
            return 0.0;
        }
        match self.view {
            // The running minute at its pace so far – from 10 measured seconds on.
            "hour" if last.seconds >= 10.0 => total * 60.0 / last.seconds.min(60.0) - total,
            // Too early in the hour, the pace says little.
            "day" if minute >= 5 => total * 60.0 / minute as f64 - total,
            "month" => {
                let finished: Vec<f64> = done.iter().map(Bar::total).filter(|t| *t > 0.0).collect();
                if finished.is_empty() { 0.0 } else { (finished.iter().sum::<f64>() / finished.len() as f64 - total).max(0.0) }
            }
            "year" => forecast.map_or(0.0, |f| (f - total).max(0.0)),
            _ => 0.0,
        }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, state: &State, out: &mut ViewOut) {
        let s = &state.settings;
        let plug = s.plug;
        let estimate = state.hours.as_ref();
        let plug_hours = if plug { state.plug_hours.as_ref() } else { None };
        let cost = |wh: f64| format!("{} {}", num(wh / 1000.0 * s.energy_price, 2), s.energy_currency).trim().to_string();
        let lines = s.power_chart == "line";
        out.footer = Some(t!(match (plug, lines) {
            (true, false) => "power.hint.plug",
            (true, true) => "power.hint.plug_lines",
            _ => "power.hint",
        }));
        let now = energy::now_minute();
        let bars = if self.view == "hour" {
            state.minutes.as_ref().map(|e| energy::bars(e, if plug { state.plug_minutes.as_ref() } else { None }, "hour", now))
        } else {
            estimate.map(|e| energy::bars(e, plug_hours, self.view, now))
        }
        .unwrap_or_default();
        let totals = estimate.map(|e| energy::totals_of(e, plug_hours, &energy::today())).unwrap_or_default();
        let forecast = totals.month_since.as_deref().and_then(|d| energy::forecast(totals.month, d, now));
        let expected = self.expected(&bars, forecast, now.4);
        // Too early in the running period to tell where it ends: the lines leave it out.
        let early = bars.last().is_some_and(|b| match self.view {
            "hour" => b.seconds < 10.0,
            "day" => now.4 < 5,
            _ => false,
        });

        // The line of the last two minutes: one value a second.
        let p = &state.power;
        let watts_now = if plug { p.plug } else { Some(p.battery.unwrap_or(p.estimate)) };
        if let Some(w) = watts_now
            && self.spark.back().is_none_or(|(t, _)| t.elapsed() >= Duration::from_millis(900))
        {
            self.spark.push_back((Instant::now(), w));
        }
        while self.spark.front().is_some_and(|(t, _)| t.elapsed() > SPARK) {
            self.spark.pop_front();
        }

        // The views, the unit, "Copy table".
        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            let views: Vec<(String, Option<String>)> = energy::VIEWS.iter().map(|v| (t!(&format!("power.view.{v}")), None)).collect();
            let chosen = energy::VIEWS.iter().position(|v| *v == self.view).unwrap_or(0);
            if let (Some(i), _) = ui::segmented(ui, "power_view", &views, chosen) {
                self.view = energy::VIEWS[i];
            }
            if self.view != "hour" {
                ui.add_space(10.0);
                let units = [("kWh".to_string(), None), (currency(s), None)];
                let (clicked, hovered) = ui::segmented(ui, "power_unit", &units, self.money as usize);
                if let Some(i) = clicked {
                    self.money = i == 1;
                }
                if hovered.is_some() {
                    out.footer = Some(t!("power.unit.help"));
                }
            }
            // On the right – or, in a narrow window, in the next row.
            let mut copy = |ui: &mut egui::Ui| {
                let b = ui.add_enabled(!bars.is_empty(), egui::Button::new(format!("⧉  {}", t!("power.copy_table"))));
                if b.hovered() {
                    out.footer = Some(t!("power.copy_table.help"));
                }
                if b.clicked() {
                    out.copy = Some((self.table(&bars, s), t!("power.table.copied", n = bars.len())));
                }
            };
            if ui.available_size_before_wrap().x >= 170.0 {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), copy);
            } else {
                ui.end_row();
                copy(ui);
            }
        });
        ui.add_space(8.0);

        // The tiles.
        let cpu_gpu = p.cpu.unwrap_or(0.0) + p.gpu.unwrap_or(0.0);
        let spark_help = format!(" {}", t!("power.help.spark"));
        let (now_value, now_sub, now_help) = if plug {
            match p.plug {
                Some(w) => {
                    let (cg, rest) = (cpu_gpu.min(w), (w - cpu_gpu).max(0.0));
                    (
                        format!("{} W", num(w, 0)),
                        t!("power.now.cpu_gpu_sub", cpu_gpu = num(cg, 0), rest = num(rest, 0)),
                        t!("power.help.plug", name = p.plug_name, watts = num(w, 0), cpu_gpu = num(cg, 0), rest = num(rest, 0)) + &spark_help,
                    )
                }
                None => ("–".into(), t!("power.measuring"), t!("power.help.plug_none")),
            }
        } else if let Some(b) = p.battery {
            (format!("{} W", num(b, 0)), t!("power.on_battery"), t!("power.help.battery") + &spark_help)
        } else {
            let parts: Vec<String> = [("CPU", p.cpu), ("GPU", p.gpu)].iter().filter_map(|(n, v)| v.map(|v| format!("{n} {}", num(v, 0)))).collect();
            let help = t!(
                "power.help.now",
                cpu = num(p.cpu.unwrap_or(0.0), 0),
                gpu = num(p.gpu.unwrap_or(0.0), 0),
                other = s.energy_other_watts,
                efficiency = s.energy_psu_efficiency,
                watts = num(p.estimate, 0)
            );
            (format!("≈ {} W", num(p.estimate, 0)), parts.join(" · "), help + &spark_help)
        };
        let uptime = energy::uptime_seconds();
        let started = SystemTime::now() - Duration::from_secs_f64(uptime);
        let energy_tile = |wh: f64| if wh > 0.0 { (energy::fmt_kwh(wh), cost(wh)) } else { ("–".to_string(), String::new()) };
        let (today_value, today_sub) = energy_tile(totals.today);
        let (month_value, month_sub) = energy_tile(totals.month);
        let forecast_line = forecast.map(|f| t!("power.forecast", energy = energy::fmt_kwh(f), cost = cost(f)));
        let tiles = [
            (t!("power.tile.now"), now_value, now_sub, None, now_help),
            (t!("power.tile.today"), today_value, today_sub, None, t!("power.help.today")),
            (t!("power.tile.month"), month_value, month_sub, forecast_line.clone(), t!("power.help.month")),
            (t!("power.tile.uptime"), energy::fmt_duration(uptime), t!("power.since", time = energy::clock(started)), None, t!("power.help.uptime")),
        ];
        let extra_line = forecast_line.is_some();
        let mut now_rect = None;
        ui.columns(4, |columns| {
            for (i, (ui, (label, value, sub, extra, help))) in columns.iter_mut().zip(tiles).enumerate() {
                let mut over_forecast = false;
                let frame = egui::Frame::new().fill(ui.visuals().faint_bg_color).corner_radius(6.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.label(ui::caption(ui, &label));
                    ui.add(egui::Label::new(RichText::new(value).size(18.0).strong()).truncate());
                    ui.add(egui::Label::new(RichText::new(if sub.is_empty() { " ".to_string() } else { sub }).size(12.0).color(ui::muted(ui))).truncate());
                    if extra_line {
                        let is_forecast = extra.is_some();
                        let r = ui.add(egui::Label::new(RichText::new(extra.unwrap_or_else(|| " ".into())).size(11.5).color(ui::accent(ui))).truncate());
                        over_forecast = is_forecast && r.hovered();
                    }
                });
                if i == 0 {
                    now_rect = Some(frame.response.rect);
                }
                if over_forecast {
                    out.footer = Some(format!("{} {}", forecast_line.clone().unwrap_or_default(), t!("power.help.forecast")));
                } else if ui.rect_contains_pointer(frame.response.rect) {
                    out.footer = Some(help);
                }
            }
        });
        if let Some(rect) = now_rect {
            self.sparkline(ui, rect);
        }

        // What the log is, the chart's title and legend.
        let note = match estimate {
            None => t!("window.loading"),
            Some(_) if !s.energy_logging => t!("power.note.off"),
            Some(_) if totals.all <= 0.0 => t!(if plug { "power.note.plug_empty" } else { "power.note.empty" }),
            Some(_) if self.view == "hour" => t!("power.note.minutes"),
            Some(_) => t!(if plug { "power.note.both" } else { "power.note.on" }, since = energy::fmt_date(totals.since.as_deref().unwrap_or_default())),
        };
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(note).size(12.0).color(ui::muted(ui))).wrap());
        ui.add_space(8.0);
        ui.label(ui::caption(ui, &t!(&format!("power.chart.{}", self.view))));
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| self.legend(ui, &bars, expected, lines));
        ui.add_space(4.0);
        let axis = self.axis(s);
        if let Some(text) = self.chart(ui, &bars, expected, early, lines, &axis, &cost) {
            out.footer = Some(text);
        }
    }

    /// The line of the last two minutes, in the right half of the tile "Now".
    fn sparkline(&self, ui: &egui::Ui, tile: egui::Rect) {
        if self.spark.len() < 2 {
            return;
        }
        let area = egui::Rect::from_min_max(egui::pos2(tile.center().x, tile.min.y + 10.0), egui::pos2(tile.max.x - 10.0, tile.min.y + 36.0));
        let (lo, hi) = self.spark.iter().fold((f64::MAX, f64::MIN), |(lo, hi), (_, w)| (lo.min(*w), hi.max(*w)));
        let (lo, hi) = (lo - 5.0, hi + 5.0);
        let now = Instant::now();
        let points: Vec<egui::Pos2> = self
            .spark
            .iter()
            .map(|(t, w)| {
                let age = now.duration_since(*t).as_secs_f32() / SPARK.as_secs_f32();
                egui::pos2(area.max.x - age * area.width(), area.max.y - ((w - lo) / (hi - lo)) as f32 * area.height())
            })
            .collect();
        let painter = ui.painter_at(tile);
        let accent = ui::accent(ui);
        painter.line_segment([egui::pos2(area.min.x, area.max.y), area.max], Stroke::new(1.0, accent.gamma_multiply(0.25)));
        let last = points.last().copied();
        painter.add(egui::Shape::line(points, Stroke::new(1.5, accent)));
        if let Some(last) = last {
            painter.circle_filled(last, 2.5, accent);
        }
    }

    /// The legend: what the parts (or lines) and the dashed box mean – only what is shown.
    fn legend(&self, ui: &mut egui::Ui, bars: &[Bar], expected: f64, lines: bool) {
        let [cg, rest, guessed] = part_colors(ui);
        let muted = ui::muted(ui);
        let sum = |f: fn(&Bar) -> f64| bars.iter().map(f).sum::<f64>();
        // (colour, text, how it is drawn: 0 a box, 1 a line, 2 a dashed box)
        let mut items: Vec<(Color32, String, u8)> = vec![];
        if lines {
            items.push((ui.visuals().strong_text_color(), t!("power.legend.total"), 1));
            items.push((cg, t!("power.legend.cpu_gpu"), 1));
            items.push((rest, t!("power.legend.rest_line"), 1));
        } else {
            for (value, color, id) in [(sum(|b| b.cpu_gpu), cg, "power.legend.cpu_gpu"), (sum(|b| b.rest), rest, "power.legend.rest"), (sum(|b| b.estimated), guessed, "power.legend.estimated")] {
                if value > 0.0 {
                    items.push((color, t!(id), 0));
                }
            }
        }
        if expected > 0.0 {
            items.push((muted, t!("power.legend.expected"), 2));
        }
        ui.spacing_mut().item_spacing.x = 5.0;
        for (color, text, kind) in items {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(10.0, 10.0), Sense::hover());
            let painter = ui.painter();
            match kind {
                1 => {
                    painter.line_segment([rect.left_center(), rect.right_center()], Stroke::new(2.0, color));
                }
                2 => painter.extend(egui::Shape::dashed_line(&[rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()], Stroke::new(1.0, color), 2.0, 2.0)),
                _ => {
                    painter.rect_filled(rect, 2.0, color);
                }
            }
            ui.label(RichText::new(text).size(11.5).color(muted));
            ui.add_space(10.0);
        }
    }

    /// Draws the chart into the rest of the tab; the footer text of the bar under the mouse comes back.
    #[allow(clippy::too_many_arguments)]
    fn chart(&self, ui: &mut egui::Ui, bars: &[Bar], expected: f64, early: bool, lines: bool, axis: &Axis, cost: &dyn Fn(f64) -> String) -> Option<String> {
        let size = egui::vec2(ui.available_width(), ui.available_height().max(140.0) - 4.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
        if bars.is_empty() || rect.width() < 100.0 {
            return None;
        }
        let count = bars.len();
        let totals: Vec<f64> = bars.iter().map(|b| axis.value(b.total())).collect();
        let max = totals.iter().cloned().fold(0.0, f64::max).max(axis.value(bars[count - 1].total() + expected));
        let step = energy::nice_step(max);
        let top = if max > 0.0 { step * (max / step).ceil().max(1.0) } else { step * 3.0 };
        let (left, right, top_margin, bottom) = (rect.min.x + 60.0, rect.max.x - 8.0, rect.min.y + 18.0, rect.max.y - 22.0);
        let plot_h = bottom - top_margin;
        let y_of = |v: f64| bottom - (v / top) as f32 * plot_h;
        let (muted, grid, text) = (ui::muted(ui), ui.visuals().widgets.noninteractive.bg_stroke.color, ui.visuals().text_color());
        let colors = part_colors(ui);
        let font = FontId::proportional(11.0);

        let decimals = if axis.decimals == 2 || step < 0.1 { 2 } else if step < 1.0 { 1 } else { 0 };
        let mut tick = 0.0;
        while tick <= top + step / 1000.0 {
            let y = y_of(tick);
            painter.line_segment([egui::pos2(left, y), egui::pos2(right, y)], Stroke::new(1.0, grid));
            painter.text(egui::pos2(left - 6.0, y), Align2::RIGHT_CENTER, format!("{} {}", num(tick, decimals), axis.unit), font.clone(), muted);
            tick += step;
        }

        let slot = (right - left) / count as f32;
        let center = |i: usize| left + (i as f32 + 0.5) * slot;
        let hover = resp.hover_pos().filter(|p| p.x >= left && p.x < right).map(|p| ((p.x - left) / slot) as usize).filter(|&i| i < count);
        let highest = (0..count).max_by(|&a, &b| totals[a].total_cmp(&totals[b])).unwrap_or(0);
        let label = |v: f64| format!("{} {}", num(v, axis.decimals), axis.unit);
        for (i, bar) in bars.iter().enumerate() {
            if hover == Some(i) {
                painter.rect_filled(egui::Rect::from_min_max(egui::pos2(left + i as f32 * slot, top_margin), egui::pos2(left + (i + 1) as f32 * slot, bottom)), 2.0, grid.gamma_multiply(0.35));
            }
            if let Some(l) = energy::x_label(bar.start, i, count, self.view) {
                painter.text(egui::pos2(center(i), bottom + 4.0), Align2::CENTER_TOP, l, font.clone(), muted);
            }
        }
        let expected_top = (expected > 0.0).then(|| y_of(totals[count - 1] + axis.value(expected)));

        if lines {
            // Separate lines: the total, CPU + GPU, everything else; nothing logged = a gap. The running period (the
            // last one) is drawn where it is expected to end – each line scaled alike –, dashed; too early: not yet.
            let last = count - 1;
            let running = match totals[last] {
                t if t <= 0.0 || early => None,
                t => Some((t + axis.value(expected)) / t),
            };
            let series: [(Vec<f64>, Color32, f32); 3] = [
                (totals.clone(), ui.visuals().strong_text_color(), 2.0),
                (bars.iter().map(|b| axis.value(b.cpu_gpu)).collect(), colors[0], 1.5),
                (bars.iter().map(|b| axis.value(b.rest + b.estimated)).collect(), colors[1], 1.5),
            ];
            for (values, color, width) in &series {
                let mut run: Vec<egui::Pos2> = vec![];
                let flush = |run: &mut Vec<egui::Pos2>| {
                    match run.len() {
                        0 => {}
                        1 => {
                            painter.circle_filled(run[0], 2.5, *color);
                        }
                        _ => {
                            painter.add(egui::Shape::line(std::mem::take(run), Stroke::new(*width, *color)));
                        }
                    }
                    run.clear();
                };
                for (i, v) in values.iter().enumerate().take(last) {
                    if totals[i] > 0.0 {
                        run.push(egui::pos2(center(i), y_of(*v)));
                    } else {
                        flush(&mut run);
                    }
                }
                flush(&mut run);
                let point = |i: usize| match (i == last, running) {
                    (false, _) => Some(egui::pos2(center(i), y_of(values[i]))),
                    (true, Some(f)) => Some(egui::pos2(center(i), y_of(values[i] * f))),
                    (true, None) => None,
                };
                if let Some(end) = point(last) {
                    if last > 0 && totals[last - 1] > 0.0 {
                        let from = egui::pos2(center(last - 1), y_of(values[last - 1]));
                        painter.extend(egui::Shape::dashed_line(&[from, end], Stroke::new(*width, *color), 4.0, 3.0));
                    }
                    painter.circle_stroke(end, 3.0, Stroke::new(1.5, *color));
                }
                if let Some(p) = hover.filter(|&i| totals[i] > 0.0).and_then(point) {
                    painter.circle(p, 3.5, *color, Stroke::new(1.5, ui.visuals().extreme_bg_color));
                }
            }
            let shown = (0..count).filter(|&i| i != last).max_by(|&a, &b| totals[a].total_cmp(&totals[b])).unwrap_or(0);
            if totals[shown] > 0.0 {
                painter.text(egui::pos2(center(shown), y_of(totals[shown]) - 6.0), Align2::CENTER_BOTTOM, label(totals[shown]), font.clone(), text);
            }
        } else {
            let bar_w = (slot - 2.0).clamp(2.0, 24.0);
            let radius = (bar_w / 2.0).min(4.0) as u8;
            for (i, bar) in bars.iter().enumerate() {
                let (x0, x1) = (center(i) - bar_w / 2.0, center(i) + bar_w / 2.0);
                let parts = [bar.cpu_gpu, bar.rest, bar.estimated];
                let topmost = parts.iter().rposition(|v| *v > 0.0);
                let mut below = 0.0;
                for (k, wh) in parts.iter().enumerate() {
                    if *wh <= 0.0 {
                        continue;
                    }
                    let y1 = y_of(axis.value(below));
                    let y0 = y_of(axis.value(below + wh)).min(y1 - 1.0);
                    below += wh;
                    let round = if Some(k) == topmost && !(i == count - 1 && expected > 0.0) { radius } else { 0 };
                    let color = if hover == Some(i) { colors[k] } else { shade(colors[k], 0.78) };
                    painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1)), CornerRadius { nw: round, ne: round, sw: 0, se: 0 }, color);
                }
                if i == highest && totals[i] > 0.0 {
                    painter.text(egui::pos2(center(i), y_of(totals[i]) - 3.0), Align2::CENTER_BOTTOM, label(totals[i]), font.clone(), text);
                }
            }
            if let Some(y0) = expected_top {
                let (x0, x1, y1) = (center(count - 1) - bar_w / 2.0, center(count - 1) + bar_w / 2.0, y_of(totals[count - 1]));
                let r = egui::Rect::from_min_max(egui::pos2(x0 + 0.5, y0), egui::pos2(x1 - 0.5, y1));
                painter.rect_filled(r, 2.0, ui::accent(ui).gamma_multiply(0.08));
                painter.extend(egui::Shape::dashed_line(&[r.left_bottom(), r.left_top(), r.right_top(), r.right_bottom()], Stroke::new(1.0, muted), 3.0, 2.0));
            }
        }

        // The average of the finished bars with a value.
        let finished: Vec<f64> = totals[..count - 1].iter().copied().filter(|v| *v > 0.0).collect();
        if finished.len() > 1 {
            let avg = finished.iter().sum::<f64>() / finished.len() as f64;
            let y = y_of(avg);
            painter.extend(egui::Shape::dashed_line(&[egui::pos2(left, y), egui::pos2(right, y)], Stroke::new(1.0, text.gamma_multiply(0.55)), 5.0, 4.0));
            // On a small ground of its own, so that it stays readable over the bars.
            let galley = painter.layout_no_wrap(t!("power.average", value = label(avg)), font.clone(), text.gamma_multiply(0.85));
            let at = egui::pos2(right - 4.0 - galley.size().x, y - 3.0 - galley.size().y);
            painter.rect_filled(egui::Rect::from_min_size(at, galley.size()).expand(2.0), 3.0, ui.visuals().extreme_bg_color.gamma_multiply(0.85));
            painter.galley(at, galley, text);
        }

        let i = hover?;
        let bar = &bars[i];
        let unit = if self.view == "day" { "Wh" } else { "kWh" };
        let period = energy::period(bar.start, self.view);
        let total = bar.total();
        if total <= 0.0 {
            return Some(t!("power.bar.empty", period = period));
        }
        if self.view == "hour" {
            // Per minute: its average power; the running one at its pace.
            let per = if i == count - 1 && bar.seconds > 0.0 { 3600.0 / bar.seconds.min(60.0) } else { 60.0 };
            let parts: Vec<String> = [(bar.cpu_gpu, "power.part.cpu_gpu_w"), (bar.rest, "power.part.rest_w"), (bar.estimated, "power.part.estimated_w")]
                .iter()
                .filter(|(wh, _)| *wh > 0.0)
                .map(|(wh, id)| t!(id, watts = num(wh * per, 0)))
                .collect();
            return Some(t!("power.bar.minute", period = period, watts = num(total * per, 0), parts = parts.join(" + ")));
        }
        let parts: Vec<String> = [(bar.cpu_gpu, "power.part.cpu_gpu"), (bar.rest, "power.part.rest"), (bar.estimated, "power.part.estimated")]
            .iter()
            .filter(|(wh, _)| *wh > 0.0)
            .map(|(wh, id)| t!(id, energy = energy::fmt_energy(*wh, unit)))
            .collect();
        let mut text = t!("power.bar", period = period, energy = energy::fmt_energy(total, unit), cost = cost(total), parts = parts.join(" + "), measured = energy::fmt_duration(bar.seconds));
        if i == count - 1 && expected > 0.0 {
            text += &format!(" · {}", t!("power.bar.expected", energy = energy::fmt_energy(total + expected, unit), cost = cost(total + expected)));
        }
        Some(text)
    }

    /// The bars as a tab-separated table (pastes into Excel).
    fn table(&self, bars: &[Bar], s: &Settings) -> String {
        let head = [
            t!("power.table.period"),
            t!("power.table.total"),
            t!("power.table.cpu_gpu"),
            t!("power.table.rest"),
            t!("power.table.estimated"),
            format!("{} ({})", t!("power.table.cost"), currency(s)),
            t!("power.table.measured"),
        ];
        let mut lines = vec![head.join("\t")];
        let kwh = |wh: f64| num(wh / 1000.0, 3);
        for b in bars {
            let measured = if b.seconds > 0.0 { energy::fmt_duration(b.seconds) } else { String::new() };
            let total = b.total();
            lines.push([energy::period(b.start, self.view), kwh(total), kwh(b.cpu_gpu), kwh(b.rest), kwh(b.estimated), num(total / 1000.0 * s.energy_price, 2), measured].join("\t"));
        }
        lines.join("\n") + "\n"
    }
}

/// The currency as set – or the word "cost" without one.
fn currency(s: &Settings) -> String {
    let c = s.energy_currency.trim();
    if c.is_empty() { t!("power.table.cost") } else { c.to_string() }
}
