//! The tab "Power" of the detail window (as in version 2): the power right now, today, this month and
//! how long the PC has been on, and a bar chart of the log – per hour for the last 24 hours, per day for the last 30
//! days, per month for the last 12 months.
//!
//! With the smart plug on, a second switch picks what the tiles and the chart show: the plug's log (measured, for
//! everything on the plug) or the PC's estimate. One series at a time, so no legend – the title names it. Bars at
//! most 24 px wide with a rounded top, hairline gridlines, only the highest bar labeled; the rest is in the footer
//! while hovering over a bar and in "Copy table" (tab-separated, e.g. for Excel).

use std::time::{Duration, SystemTime};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, RichText, Sense, Stroke};
use kit::t;
use kit::ui;

use crate::energy::{self, Bucket, Hours, num};
use crate::link::State;
use crate::window::ViewOut;

pub struct PowerView {
    view: &'static str,
    /// With the plug on: its log (true) or the PC's estimate.
    plug: bool,
}

impl Default for PowerView {
    fn default() -> Self {
        PowerView { view: "day", plug: true }
    }
}

/// Darker, for the bars that are not under the mouse.
pub fn shade(c: Color32, f: f32) -> Color32 {
    Color32::from_rgb((c.r() as f32 * f) as u8, (c.g() as f32 * f) as u8, (c.b() as f32 * f) as u8)
}

impl PowerView {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &State, out: &mut ViewOut) {
        let s = &state.settings;
        let plug = s.plug && self.plug;
        let hours: Option<&Hours> = if plug { state.plug_hours.as_ref() } else { state.hours.as_ref() };
        let cost = |wh: f64| format!("{} {}", num(wh / 1000.0 * s.energy_price, 2), s.energy_currency).trim().to_string();
        out.footer = Some(t!(if s.plug { "power.hint.plug" } else { "power.hint" }));
        let bars = hours.map(|h| energy::buckets(h, self.view, energy::now_hour())).unwrap_or_default();

        // The views, the source, "Copy table".
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            {
                for view in energy::VIEWS {
                    if ui.add(egui::Button::new(t!(&format!("power.view.{view}"))).selected(view == self.view)).clicked() {
                        self.view = view;
                    }
                }
                if s.plug {
                    ui.add_space(14.0);
                    for (on, id) in [(true, "power.source.plug"), (false, "power.source.pc")] {
                        let b = ui.add(egui::Button::new(t!(id)).selected(self.plug == on));
                        if b.clicked() {
                            self.plug = on;
                        }
                        if b.hovered() {
                            out.footer = Some(t!("power.source.help"));
                        }
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let b = ui.add_enabled(!bars.is_empty(), egui::Button::new(format!("⧉  {}", t!("power.copy_table"))));
                if b.hovered() {
                    out.footer = Some(t!("power.copy_table.help"));
                }
                if b.clicked() {
                    out.copy = Some((self.table(&bars, s.energy_price, &s.energy_currency), t!("power.table.copied", n = bars.len())));
                }
            });
        });
        ui.add_space(8.0);

        // The tiles.
        let p = &state.power;
        let today = energy::today();
        let totals = hours.map(|h| energy::totals(h, &today)).unwrap_or_default();
        let (now, now_sub, now_help) = if plug {
            match p.plug {
                Some(w) => {
                    let rest = (w - p.estimate).max(0.0);
                    (
                        format!("{} W", num(w, 0)),
                        t!("power.now.plug_sub", pc = num(p.estimate, 0), rest = num(rest, 0)),
                        t!("power.help.plug", name = p.plug_name, watts = num(w, 0), pc = num(p.estimate, 0), rest = num(rest, 0)),
                    )
                }
                None => ("–".into(), t!("power.measuring"), t!("power.help.plug_none")),
            }
        } else if let Some(b) = p.battery {
            (format!("{} W", num(b, 0)), t!("power.on_battery"), t!("power.help.battery"))
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
            (format!("≈ {} W", num(p.estimate, 0)), parts.join(" · "), help)
        };
        let uptime = energy::uptime_seconds();
        let started = SystemTime::now() - Duration::from_secs_f64(uptime);
        let energy_tile = |wh: f64| if wh > 0.0 { (energy::fmt_kwh(wh), cost(wh)) } else { ("–".to_string(), String::new()) };
        let (today_value, today_sub) = energy_tile(totals.today);
        let (month_value, month_sub) = energy_tile(totals.month);
        let tiles = [
            (t!("power.tile.now"), now, now_sub, now_help),
            (t!("power.tile.today"), today_value, today_sub, t!("power.help.today")),
            (t!("power.tile.month"), month_value, month_sub, t!("power.help.month")),
            (t!("power.tile.uptime"), energy::fmt_duration(uptime), t!("power.since", time = energy::clock(started)), t!("power.help.uptime")),
        ];
        ui.columns(4, |columns| {
            for (ui, (label, value, sub, help)) in columns.iter_mut().zip(tiles) {
                let frame = egui::Frame::new().fill(ui.visuals().faint_bg_color).corner_radius(6.0).inner_margin(egui::Margin::symmetric(10, 6)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(label).size(12.0).color(ui::muted(ui)));
                    ui.label(RichText::new(value).size(18.0).strong());
                    ui.label(RichText::new(if sub.is_empty() { " ".to_string() } else { sub }).size(12.0).color(ui::muted(ui)));
                });
                if ui.rect_contains_pointer(frame.response.rect) {
                    out.footer = Some(help);
                }
            }
        });

        // What the log is, and the chart's title.
        let note = match hours {
            None => t!("window.loading"),
            Some(_) if !s.energy_logging => t!("power.note.off"),
            Some(h) if h.is_empty() => t!(if plug { "power.note.plug_empty" } else { "power.note.empty" }),
            Some(_) => {
                let since = energy::fmt_date(totals.since.as_deref().unwrap_or_default());
                t!(if plug { "power.note.plug_on" } else { "power.note.on" }, since = since)
            }
        };
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(note).size(12.0).color(ui::muted(ui))).wrap());
        ui.add_space(8.0);
        let mut title = t!(&format!("power.chart.{}", self.view));
        if s.plug {
            title += &format!(" · {}", t!(if plug { "power.chart.plug" } else { "power.chart.pc" }));
        }
        ui.label(RichText::new(title).size(12.0).color(ui::muted(ui)));
        ui.add_space(2.0);
        if let Some(text) = self.chart(ui, &bars, &cost) {
            out.footer = Some(text);
        }
    }

    /// Draws the bars into the rest of the tab; the footer text of the bar under the mouse comes back.
    fn chart(&self, ui: &mut egui::Ui, bars: &[Bucket], cost: &dyn Fn(f64) -> String) -> Option<String> {
        let size = egui::vec2(ui.available_width(), ui.available_height().max(140.0) - 4.0);
        let (rect, resp) = ui.allocate_exact_size(size, Sense::hover());
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
        if bars.is_empty() || rect.width() < 100.0 {
            return None;
        }
        let (unit, scale) = if self.view == "day" { ("Wh", 1.0) } else { ("kWh", 1000.0) };
        let values: Vec<f64> = bars.iter().map(|b| b.wh / scale).collect();
        let max = values.iter().cloned().fold(0.0, f64::max);
        let step = energy::nice_step(max);
        let top = if max > 0.0 { step * (max / step).ceil().max(1.0) } else { step * 3.0 };
        let (left, right, top_margin, bottom) = (rect.min.x + 56.0, rect.max.x - 8.0, rect.min.y + 18.0, rect.max.y - 22.0);
        let plot_h = bottom - top_margin;
        let y_of = |v: f64| bottom - (v / top) as f32 * plot_h;
        let (muted, grid) = (ui::muted(ui), ui.visuals().widgets.noninteractive.bg_stroke.color);
        let font = FontId::proportional(11.0);

        let decimals = if step >= 1.0 { 0 } else if step >= 0.1 { 1 } else { 2 };
        let mut tick = 0.0;
        while tick <= top + step / 1000.0 {
            let y = y_of(tick);
            painter.line_segment([egui::pos2(left, y), egui::pos2(right, y)], Stroke::new(1.0, grid));
            painter.text(egui::pos2(left - 6.0, y), Align2::RIGHT_CENTER, format!("{} {unit}", num(tick, decimals)), font.clone(), muted);
            tick += step;
        }

        let count = bars.len();
        let slot = (right - left) / count as f32;
        let hover = resp.hover_pos().filter(|p| p.x >= left && p.x < right).map(|p| ((p.x - left) / slot) as usize).filter(|&i| i < count);
        let bar_w = (slot - 2.0).clamp(2.0, 24.0);
        let radius = (bar_w / 2.0).min(4.0) as u8;
        let highest = (0..count).max_by(|&a, &b| values[a].total_cmp(&values[b])).unwrap_or(0);
        let accent = ui::accent(ui);
        for (i, (value, bar)) in values.iter().zip(bars).enumerate() {
            let x0 = left + i as f32 * slot + (slot - bar_w) / 2.0;
            let x1 = x0 + bar_w;
            // Tiny values stay visible as a sliver.
            let y = y_of(*value).min(bottom - 1.0);
            if hover == Some(i) {
                painter.rect_filled(egui::Rect::from_min_max(egui::pos2(left + i as f32 * slot, top_margin), egui::pos2(left + (i + 1) as f32 * slot, bottom)), 2.0, grid.gamma_multiply(0.35));
            }
            if *value > 0.0 {
                let color = if hover == Some(i) { accent } else { shade(accent, 0.78) };
                painter.rect_filled(egui::Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, bottom)), CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 }, color);
                if i == highest {
                    painter.text(egui::pos2((x0 + x1) / 2.0, y - 3.0), Align2::CENTER_BOTTOM, energy::fmt_energy(value * scale, unit), font.clone(), ui.visuals().text_color());
                }
            }
            if let Some(label) = energy::x_label(bar.start, i, count, self.view) {
                painter.text(egui::pos2((x0 + x1) / 2.0, bottom + 4.0), Align2::CENTER_TOP, label, font.clone(), muted);
            }
        }
        let bar = &bars[hover?];
        let period = energy::period(bar.start, self.view);
        Some(if bar.seconds > 0.0 {
            t!("power.bar", period = period, energy = energy::fmt_energy(bar.wh, unit), cost = cost(bar.wh), measured = energy::fmt_duration(bar.seconds))
        } else {
            t!("power.bar.empty", period = period)
        })
    }

    /// The bars as a tab-separated table (pastes into Excel).
    fn table(&self, bars: &[Bucket], price: f64, currency: &str) -> String {
        let mut lines = vec![[t!("power.table.period"), "kWh".into(), format!("{} ({currency})", t!("power.table.cost")), t!("power.table.measured")].join("\t")];
        for b in bars {
            let measured = if b.seconds > 0.0 { energy::fmt_duration(b.seconds) } else { String::new() };
            lines.push([energy::period(b.start, self.view), num(b.wh / 1000.0, 3), num(b.wh / 1000.0 * price, 2), measured].join("\t"));
        }
        lines.join("\n") + "\n"
    }
}
