//! The tab "Drive health" of the detail window (as in the Python version) – like CrystalDiskInfo: every drive
//! with its health status (Good / Caution / Bad), temperature, remaining life and power-on hours; for the selected
//! one the key data and the full SMART table with a coloured dot per attribute.
//!
//! The bridge reads the values (DiskInfoToolkit, the library LHM uses) every 10 s while this tab is shown and hands
//! over only raw facts; the rules of CrystalDiskInfo for "good", "caution" and "bad" are here. A status is never shown
//! by colour alone – the word is always there too. In privacy mode the serial number is hidden, also when copying.

use eframe::egui::{self, Align2, Color32, FontId, RichText, Sense};
use kit::t;
use kit::ui;
use serde_json::Value;

use crate::energy::num;
use crate::hardware_data;
use crate::link::State;
use crate::window::ViewOut;

/// Reallocated / pending / uncorrectable sectors: raw > 0 = caution on a hard disk (like CrystalDiskInfo).
const CAUTION_IDS: [u64; 3] = [0x05, 0xC5, 0xC6];
const ROW: f32 = 24.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub id: u64,
    pub name: String,
    pub current: u64,
    pub worst: u64,
    pub threshold: u64,
    pub raw: u64,
    /// "good", "caution", "bad".
    pub status: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Drive {
    pub number: i64,
    pub model: String,
    pub firmware: String,
    pub serial: String,
    pub size: f64,
    pub bus: String,
    pub nvme: bool,
    pub ssd: bool,
    /// "good", "caution", "bad", "unknown".
    pub status: String,
    pub temp: Option<f64>,
    pub temp_warning: Option<f64>,
    pub temp_critical: Option<f64>,
    pub life: Option<f64>,
    pub hours: f64,
    pub count: f64,
    pub reads: Option<f64>,
    pub writes: Option<f64>,
    pub attributes: Vec<Attribute>,
}

/// One SMART attribute by the rules of CrystalDiskInfo: below its threshold = bad; on hard disks reallocated /
/// pending / uncorrectable sectors = caution; NVMe: a critical warning = bad, data integrity errors = caution.
fn attribute_status(kind: &str, id: u64, current: u64, threshold: u64, raw: u64, nvme: bool, ssd: bool) -> &'static str {
    if nvme {
        return if kind == "CriticalWarning" && raw > 0 {
            "bad"
        } else if kind == "MediaAndDataIntegrityErrors" && raw > 0 {
            "caution"
        } else {
            "good"
        };
    }
    if threshold > 0 && current < threshold {
        "bad"
    } else if !ssd && CAUTION_IDS.contains(&id) && raw > 0 {
        "caution"
    } else {
        "good"
    }
}

/// The bridge's `drives` answer → the drives, by number.
pub fn parse(v: &Value) -> Vec<Drive> {
    let n = |x: &Value| x.as_f64();
    let s = |x: &Value| x.as_str().unwrap_or_default().trim().to_string();
    let u = |x: &Value| x.as_u64().or_else(|| x.as_f64().map(|f| f as u64)).unwrap_or(0);
    let mut drives: Vec<Drive> = v["drives"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|d| {
            let (nvme, ssd) = (d["nvme"].as_bool().unwrap_or(false), d["ssd"].as_bool().unwrap_or(false));
            let mut attributes: Vec<Attribute> = d["attributes"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(i, a)| {
                    let (current, threshold, raw) = (u(&a["current"]), u(&a["threshold"]), u(&a["raw"]));
                    // NVMe values have no ID of their own – numbered like CrystalDiskInfo does (01, 02, …).
                    let id = if nvme { i as u64 + 1 } else { u(&a["id"]) };
                    Attribute { id, name: s(&a["name"]), current, worst: u(&a["worst"]), threshold, raw, status: attribute_status(&s(&a["kind"]), id, current, threshold, raw, nvme, ssd) }
                })
                .collect();
            if nvme {
                let raw_of = |kind: &str| d["attributes"].as_array().into_iter().flatten().find(|a| a["kind"] == kind).map(|a| u(&a["raw"]));
                if raw_of("AvailableSpare").unwrap_or(100) < raw_of("AvailableSpareThreshold").unwrap_or(0) {
                    let kinds: Vec<String> = d["attributes"].as_array().into_iter().flatten().map(|a| s(&a["kind"])).collect();
                    for (a, kind) in attributes.iter_mut().zip(kinds) {
                        if kind == "AvailableSpare" {
                            a.status = "caution";
                        }
                    }
                }
            }
            Drive {
                number: d["number"].as_i64().unwrap_or(0),
                model: s(&d["model"]),
                firmware: s(&d["firmware"]),
                serial: s(&d["serial"]),
                size: n(&d["size"]).unwrap_or(0.0),
                bus: s(&d["bus"]),
                nvme,
                ssd,
                status: s(&d["status"]).to_lowercase(),
                temp: n(&d["temp"]),
                temp_warning: n(&d["temp_warning"]),
                temp_critical: n(&d["temp_critical"]),
                life: n(&d["life"]),
                hours: n(&d["hours"]).unwrap_or(0.0),
                count: n(&d["count"]).unwrap_or(0.0),
                reads: n(&d["reads"]),
                writes: n(&d["writes"]),
                attributes,
            }
        })
        .collect();
    drives.sort_by_key(|d| d.number);
    drives
}

/// 48779 GB → "47.6 TB" (1024-based like the rest of the window).
fn fmt_gb(gb: f64) -> String {
    if gb >= 1024.0 { format!("{} TB", num(gb / 1024.0, 1)) } else { format!("{} GB", num(gb, 0)) }
}

/// Bytes as manufacturers count them: 240057409536 → "240 GB", 2000398934016 → "2.0 TB".
fn fmt_size(size: f64) -> String {
    if size >= 1e12 { format!("{} TB", num(size / 1e12, 1)) } else { format!("{:.0} GB", size / 1e9) }
}

/// 12345 → "12,345" (German "12.345").
fn grouped(n: f64) -> String {
    let digits = format!("{:.0}", n.max(0.0));
    let sep = if kit::i18n::language() == "de" { '.' } else { ',' };
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

/// (caution, bad) temperature: what the drive reports itself (NVMe), otherwise CrystalDiskInfo's defaults.
fn temp_status(d: &Drive) -> &'static str {
    let Some(temp) = d.temp else { return "unknown" };
    let (caution, bad) = match (d.temp_warning, d.temp_critical) {
        (Some(w), Some(c)) if w > 0.0 && c > 0.0 => (w, c),
        _ if d.ssd => (55.0, 65.0),
        _ => (45.0, 55.0),
    };
    if temp < caution {
        "good"
    } else if temp < bad {
        "caution"
    } else {
        "bad"
    }
}

fn interface(d: &Drive) -> String {
    if d.nvme {
        return "NVMe SSD".into();
    }
    // "BusTypeSata" → "SATA"
    let bus = d.bus.replace("BusType", "").to_uppercase();
    format!("{bus} · {}", if d.ssd { "SSD".to_string() } else { t!("health.hdd") })
}

fn status_color(ui: &egui::Ui, status: &str) -> Color32 {
    match status {
        "good" => ui::good(ui),
        "caution" => ui::warn(ui),
        "bad" => ui::bad(ui),
        _ => ui::muted(ui),
    }
}

fn status_word(status: &str) -> String {
    let s = if ["good", "caution", "bad"].contains(&status) { status } else { "unknown" };
    t!(&format!("health.status.{s}"))
}

#[derive(Default)]
pub struct DriveView {
    selected: Option<i64>,
}

impl DriveView {
    pub fn show(&mut self, ui: &mut egui::Ui, state: &State, out: &mut ViewOut) {
        out.footer = Some(t!("health.hint"));
        let private = state.settings.privacy_mode;
        let drives = state.drives.as_ref().map(parse);
        ui.add_space(6.0);
        let Some(drives) = drives.filter(|d| !d.is_empty()) else {
            let text = if state.drives.is_none() { t!("health.reading") } else { t!("health.none") };
            ui.label(RichText::new(text).color(ui::muted(ui)));
            return;
        };
        if !self.selected.is_some_and(|n| drives.iter().any(|d| d.number == n)) {
            self.selected = Some(drives[0].number);
        }

        // The drives.
        let font = FontId::proportional(13.0);
        for d in &drives {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW), Sense::click());
            let painter = ui.painter_at(rect);
            let selected = self.selected == Some(d.number);
            if selected {
                painter.rect_filled(rect, 3.0, ui.visuals().selection.bg_fill.gamma_multiply(0.6));
            } else if resp.hovered() {
                painter.rect_filled(rect, 3.0, ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(0.5));
            }
            let y = rect.center().y;
            let right = rect.max.x - 6.0;
            let text = ui.visuals().text_color();
            painter.text(egui::pos2(rect.min.x + 6.0, y), Align2::LEFT_CENTER, format!("{}  {}", d.number, d.model), font.clone(), text);
            painter.text(egui::pos2(right, y), Align2::RIGHT_CENTER, format!("{} h", grouped(d.hours)), font.clone(), text);
            if let Some(life) = d.life {
                painter.text(egui::pos2(right - 85.0, y), Align2::RIGHT_CENTER, format!("{} %", num(life, 0)), font.clone(), text);
            }
            let temp = d.temp.map_or("–".to_string(), |t| format!("{} °C", num(t, 0)));
            painter.text(egui::pos2(right - 145.0, y), Align2::RIGHT_CENTER, temp, font.clone(), status_color(ui, temp_status(d)));
            painter.text(egui::pos2(right - 290.0, y), Align2::LEFT_CENTER, format!("●  {}", status_word(&d.status)), font.clone(), status_color(ui, &d.status));
            if resp.hovered() {
                out.footer = Some(t!("health.list.help"));
            }
            if resp.clicked() {
                self.selected = Some(d.number);
            }
        }
        let Some(d) = drives.iter().find(|d| Some(d.number) == self.selected) else { return };

        // Its key data, in two columns, and "Copy".
        ui.add_space(10.0);
        let mut status_text = status_word(&d.status);
        if let Some(life) = d.life {
            status_text = t!("health.with_life", status = status_text, life = num(life, 0));
        }
        let serial = if d.serial.is_empty() { "–".to_string() } else { hardware_data::shown("serial_number", &d.serial, private).to_string() };
        let fields: Vec<(&str, String, Option<Color32>)> = vec![
            ("status", format!("●  {status_text}"), Some(status_color(ui, &d.status))),
            ("temp", d.temp.map_or("–".to_string(), |t| format!("{} °C", num(t, 0))), Some(status_color(ui, temp_status(d)))),
            ("firmware", if d.firmware.is_empty() { "–".into() } else { d.firmware.clone() }, None),
            ("interface", interface(d), None),
            ("serial", serial, None),
            ("size", fmt_size(d.size), None),
            ("hours", format!("{} h", grouped(d.hours)), None),
            ("count", grouped(d.count), None),
            ("reads", d.reads.map_or("–".to_string(), fmt_gb), None),
            ("writes", d.writes.map_or("–".to_string(), fmt_gb), None),
        ];
        ui.horizontal_top(|ui| {
            egui::Grid::new("drive_fields").num_columns(4).spacing([10.0, 4.0]).show(ui, |ui| {
                for pair in fields.chunks(2) {
                    for (i, (name, value, color)) in pair.iter().enumerate() {
                        if i > 0 {
                            ui.add_space(8.0);
                        }
                        ui.label(RichText::new(t!(&format!("health.field.{name}"))).size(12.0).color(ui::muted(ui)));
                        let mut text = RichText::new(value);
                        if *name == "status" || *name == "temp" {
                            text = text.strong();
                        }
                        ui.label(match color {
                            Some(c) => text.color(*c),
                            None => text,
                        });
                    }
                    ui.end_row();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                let b = ui.button(format!("⧉  {}", t!("health.copy")));
                if b.hovered() {
                    out.footer = Some(t!("health.copy.help"));
                }
                if b.clicked() {
                    let mut lines = vec![format!("{}  {}", d.number, d.model)];
                    for (name, value, _) in &fields {
                        lines.push(format!("  {}: {}", t!(&format!("health.field.{name}")), value.trim_start_matches(['●', ' '])));
                    }
                    lines.push(String::new());
                    lines.push(["id", "attribute", "current", "worst", "threshold", "raw"].map(|c| t!(&format!("health.col.{c}"))).join("\t"));
                    for a in &d.attributes {
                        let values = if d.nvme { [String::new(), String::new(), String::new()] } else { [a.current.to_string(), a.worst.to_string(), a.threshold.to_string()] };
                        lines.push(format!("{:02X}\t{}\t{}\t{:012X}", a.id, a.name, values.join("\t"), a.raw));
                    }
                    out.copy = Some((lines.join("\n") + "\n", t!("health.copied", model = d.model)));
                }
            });
        });

        // The SMART table.
        ui.add_space(10.0);
        let width = ui.available_width();
        // (column, width, right-aligned); the attribute takes what is left.
        let columns: [(&str, f32, bool); 6] = [("id", 50.0, false), ("attribute", 0.0, false), ("current", 60.0, true), ("worst", 55.0, true), ("threshold", 75.0, true), ("raw", 115.0, true)];
        let fixed: f32 = columns.iter().map(|c| c.1).sum();
        let widths: Vec<f32> = columns.iter().map(|c| if c.1 == 0.0 { (width - fixed - 12.0).max(120.0) } else { c.1 }).collect();
        let row = |ui: &mut egui::Ui, texts: [String; 6], color: Color32, dot: Option<Color32>, header: bool| {
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, if header { 22.0 } else { 20.0 }), Sense::hover());
            let painter = ui.painter_at(rect);
            let mut x = rect.min.x + 6.0;
            let font = FontId::proportional(if header { 12.0 } else { 12.5 });
            for (i, text) in texts.iter().enumerate() {
                let (_, _, right) = columns[i];
                let y = rect.center().y;
                if i == 0 && let Some(c) = dot {
                    painter.text(egui::pos2(x, y), Align2::LEFT_CENTER, "●", font.clone(), c);
                    painter.text(egui::pos2(x + 16.0, y), Align2::LEFT_CENTER, text, font.clone(), color);
                } else if right {
                    painter.text(egui::pos2(x + widths[i] - 8.0, y), Align2::RIGHT_CENTER, text, font.clone(), color);
                } else {
                    painter.text(egui::pos2(x, y), Align2::LEFT_CENTER, text, font.clone(), color);
                }
                x += widths[i];
            }
        };
        let header = ["id", "attribute", "current", "worst", "threshold", "raw"].map(|c| t!(&format!("health.col.{c}")));
        row(ui, header, ui::muted(ui), None, true);
        ui.separator();
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            for a in &d.attributes {
                let values = if d.nvme { [String::new(), String::new(), String::new()] } else { [a.current.to_string(), a.worst.to_string(), a.threshold.to_string()] };
                let [c, w, th] = values;
                let texts = [format!("{:02X}", a.id), a.name.clone(), c, w, th, format!("{:012X}", a.raw)];
                row(ui, texts, ui.visuals().text_color(), Some(status_color(ui, a.status)), false);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_rules_of_crystal_disk_info() {
        // Hard disk: pending sectors → caution; below threshold → bad.
        assert_eq!(attribute_status("", 0xC5, 100, 0, 3, false, false), "caution");
        assert_eq!(attribute_status("", 0xC5, 100, 0, 3, false, true), "good");
        assert_eq!(attribute_status("", 0x01, 5, 10, 0, false, true), "bad");
        assert_eq!(attribute_status("CriticalWarning", 1, 0, 0, 1, true, true), "bad");
        let v = json!({"drives": [
            {"number": 1, "model": "B", "nvme": true, "ssd": true, "status": "Good", "hours": 10, "attributes": [
                {"id": 0, "kind": "AvailableSpare", "raw": 5}, {"id": 0, "kind": "AvailableSpareThreshold", "raw": 10}]},
            {"number": 0, "model": "A", "nvme": false, "ssd": true, "status": "Caution", "life": 46.0, "temp": 30.0}]});
        let d = parse(&v);
        assert_eq!((d[0].model.as_str(), d[0].status.as_str(), d[1].attributes[0].id, d[1].attributes[1].id), ("A", "caution", 1, 2));
        assert_eq!(d[1].attributes[0].status, "caution");
        assert_eq!(temp_status(&d[0]), "good");
    }

    #[test]
    fn numbers() {
        kit::texts!();
        kit::i18n::set_language("en");
        assert_eq!((grouped(12345.0), grouped(999.0), grouped(1234567.0)), ("12,345".into(), "999".into(), "1,234,567".into()));
        assert_eq!((fmt_size(240_057_409_536.0), fmt_size(2_000_398_934_016.0)), ("240 GB".into(), "2.0 TB".into()));
        assert_eq!(fmt_gb(48779.0), "47.6 TB");
    }
}
