//! What the detail window shows: `detail_window.json`, read each time the window opens – edit it, close the
//! window, open it again, and the change is there (the "save, reopen, see it" loop of the Python part's
//! `detail_window_content.py`, which this file replaces). Its `help` at the top describes the format.
//!
//! From it and a reading come the rows of a tab: device headings, sections that fold shut, values.

use std::path::PathBuf;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

use crate::link::Want;
use crate::sensors::{Hardware, Sensor};

const FILE: &str = "detail_window.json";

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Tab {
    pub title: String,
    #[serde(default)]
    pub groups: Vec<Group>,
    /// A tab of its own instead of values: "power", "fans", "health", "hardware_data", "all".
    #[serde(default)]
    pub view: Option<String>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
pub struct Group {
    pub device: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    pub values: Vec<Entry>,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
#[serde(untagged)]
pub enum Entry {
    Section {
        section: String,
        values: Vec<Entry>,
        #[serde(default)]
        one_row: bool,
    },
    Value {
        sensor: String,
        #[serde(default, rename = "type")]
        kind: Option<String>,
        #[serde(default)]
        text: Option<String>,
        #[serde(default)]
        exclude: Option<String>,
    },
}

#[derive(Debug)]
pub struct Content {
    pub tabs: Vec<Tab>,
    explanations: Vec<(String, Regex, String)>,
    type_explanations: std::collections::HashMap<String, String>,
}

/// Where the file lies: next to the exe (a release) – or in the repo above `dist\`.
pub fn file() -> PathBuf {
    let home = kit::app::program_dir();
    let here = home.join(FILE);
    if here.exists() { here } else { home.parent().map(|p| p.join(FILE)).filter(|p| p.exists()).unwrap_or(here) }
}

impl Content {
    /// Reads the file. A mistake in it is an `Err` with the reason – the window then shows only "all values".
    pub fn load() -> Result<Content, String> {
        let text = std::fs::read_to_string(file()).map_err(|e| format!("{}: {e}", file().display()))?;
        Content::parse(&text)
    }

    pub fn parse(text: &str) -> Result<Content, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let tabs: Vec<Tab> = serde_json::from_value(v["tabs"].clone()).map_err(|e| format!("tabs: {e}"))?;
        let mut explanations = vec![];
        for e in v["explanations"].as_array().into_iter().flatten() {
            let part = |i: usize| e.get(i).and_then(Value::as_str).unwrap_or_default().to_string();
            let pattern = part(1);
            let regex = Regex::new(&format!("^(?:{pattern})$")).map_err(|err| format!("explanations: {pattern}: {err}"))?;
            explanations.push((part(0), regex, part(2)));
        }
        let type_explanations = serde_json::from_value(v["type_explanations"].clone()).unwrap_or_default();
        Ok(Content { tabs, explanations, type_explanations })
    }

    /// When the file cannot be read: everything LHM finds, so the window still works.
    pub fn fallback() -> Content {
        Content { tabs: vec![Tab { title: "tab.all".into(), groups: vec![], view: Some("all".into()) }], explanations: vec![], type_explanations: Default::default() }
    }

    /// The footer's explanation of a value: the first entry whose pattern matches the whole name.
    pub fn explain(&self, sensor: &Sensor) -> String {
        for (kind, regex, text) in &self.explanations {
            if *kind != sensor.kind {
                continue;
            }
            if let Some(caps) = regex.captures(&sensor.name) {
                let mut out = label(text);
                for i in 1..caps.len() {
                    out = out.replace(&format!("{{{}}}", i - 1), caps.get(i).map_or("", |m| m.as_str()));
                }
                // Python's "{}" – the first group.
                return out.replace("{}", caps.get(1).map_or("", |m| m.as_str()));
            }
        }
        match self.type_explanations.get(&sensor.kind) {
            Some(id) => label(id),
            None => kit::t!("explain.default"),
        }
    }
}

/// A title from the file: an id of the texts in the chosen language – or own text, which stays as it is.
pub fn label(text: &str) -> String {
    if kit::i18n::has(text) { kit::i18n::text(text) } else { text.to_string() }
}

/// `*` stands for any text; upper and lower case do not matter (Python's `fnmatchcase` on lower-case names).
pub fn glob(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match p.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some((c, rest)) => n.first().is_some_and(|m| m == c) && go(rest, &n[1..]),
        }
    }
    let (p, n): (Vec<char>, Vec<char>) = (pattern.to_lowercase().chars().collect(), name.to_lowercase().chars().collect());
    go(&p, &n)
}

fn matches(sensor: &Sensor, pattern: &str, kind: &Option<String>, exclude: &Option<String>) -> bool {
    kind.as_ref().is_none_or(|k| *k == sensor.kind) && glob(pattern, &sensor.name) && !exclude.as_ref().is_some_and(|x| glob(x, &sensor.name))
}

/// What a tab needs read.
pub fn want(tab: &Tab) -> Want {
    match tab.view.as_deref() {
        Some("all") => Want { view: "list".into(), devices: None },
        Some("hardware_data") => Want::default(),
        Some(view) => Want { view: view.into(), devices: None },
        None => {
            let mut d: Vec<String> = tab.groups.iter().map(|g| g.device.clone()).collect();
            d.sort();
            d.dedup();
            Want { view: "list".into(), devices: Some(d) }
        }
    }
}

/// Whether a tab stays visible on a PC with these devices (sensors of at least one of its devices; tabs of their
/// own always).
pub fn shown(tab: &Tab, present: &[String]) -> bool {
    tab.view.is_some() || tab.groups.iter().any(|g| present.contains(&g.device))
}

/// One row of a tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Stays the same while the structure does – folded rows stay folded.
    pub key: String,
    /// 0 = a device, 1 = a value or section of it, 2 = a value in a section.
    pub depth: usize,
    pub text: String,
    pub kind: RowKind,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RowKind {
    /// A device's name – a heading that folds; its device for the traffic light of its temperatures.
    Device { device: String },
    /// A heading in a device (a section, or a sensor type in the "all" tab).
    Section,
    /// A value; `second`: a second one in the same row (the fan's control beside its speed); `device` decides
    /// which limits colour a temperature (the GPU's, else the CPU's).
    Value { sensor: Box<Sensor>, second: Option<Box<Sensor>>, device: String },
    /// Hardware data: a category ("Graphics card") – a heading with "Copy"; `index` in the data.
    Category { index: usize },
    /// Hardware data: a device's name; `leaf`: nothing below it (the overview).
    Name { leaf: bool },
    /// Hardware data: a field of a device and its value (already hidden in privacy mode).
    Info { field: String, value: String },
}

impl RowKind {
    /// A heading that folds what is below it.
    pub fn folds(&self) -> bool {
        matches!(self, RowKind::Device { .. } | RowKind::Section | RowKind::Category { .. } | RowKind::Name { leaf: false })
    }
}

/// The rows of the tab "Hardware data": category → device → fields, or in the overview category → model names
/// (identical devices combined). `private`: privacy mode – sensitive values hidden.
pub fn hardware_rows(data: &crate::hardware_data::Data, overview: bool, private: bool) -> Vec<Row> {
    use crate::hardware_data as hd;
    let mut out = vec![];
    for (c, (category, devices)) in data.iter().enumerate() {
        out.push(Row { key: format!("hw{c}"), depth: 0, text: hd::category_name(category), kind: RowKind::Category { index: c } });
        if overview {
            for (d, name) in hd::model_names(devices).into_iter().enumerate() {
                out.push(Row { key: format!("hw{c}|o{d}"), depth: 1, text: name, kind: RowKind::Name { leaf: true } });
            }
            continue;
        }
        for (d, device) in devices.iter().enumerate() {
            let head = format!("hw{c}|d{d}");
            out.push(Row { key: head.clone(), depth: 1, text: device.name.clone(), kind: RowKind::Name { leaf: false } });
            for (i, (field, value)) in device.fields.iter().enumerate() {
                let value = hd::shown(field, value, private).to_string();
                out.push(Row { key: format!("{head}|{i}"), depth: 2, text: hd::field_name(field), kind: RowKind::Info { field: field.clone(), value } });
            }
        }
    }
    out
}

/// The sensor types in the order the "all" tab lists them (and `type.<type>` in the texts).
pub const SENSOR_TYPES: [&str; 21] = [
    "Temperature", "Load", "Clock", "Frequency", "Power", "Voltage", "Current", "Energy", "Fan", "Control", "Flow",
    "Data", "SmallData", "Throughput", "Level", "Factor", "TimeSpan", "Timing", "Noise", "Conductivity", "Humidity",
];

fn device_name(hw: &Hardware) -> String {
    if hw.top_name.is_empty() { hw.name.clone() } else { format!("{} – {}", hw.top_name, hw.name) }
}

/// The rows of a tab from a reading.
pub fn rows(tab: &Tab, hardware: &[Hardware]) -> Vec<Row> {
    let mut out = vec![];
    if tab.view.as_deref() == Some("all") {
        for hw in hardware.iter().filter(|h| !h.sensors.is_empty()) {
            let head = hw.id.clone();
            out.push(Row { key: head.clone(), depth: 0, text: device_name(hw), kind: RowKind::Device { device: hw.device.clone() } });
            for kind in SENSOR_TYPES {
                let sensors: Vec<&Sensor> = hw.sensors.iter().filter(|s| s.kind == kind).collect();
                if sensors.is_empty() {
                    continue;
                }
                let key = format!("{head}|{kind}");
                out.push(Row { key: key.clone(), depth: 1, text: kit::i18n::text(&format!("type.{kind}")), kind: RowKind::Section });
                for s in sensors {
                    out.push(Row { key: format!("{key}|{}", s.id), depth: 2, text: s.name.clone(), kind: RowKind::Value { sensor: Box::new(s.clone()), second: None, device: hw.device.clone() } });
                }
            }
        }
        return out;
    }
    for (nr, group) in tab.groups.iter().enumerate() {
        for hw in hardware {
            if hw.device != group.device || !glob(group.name.as_deref().unwrap_or("*"), &hw.name) {
                continue;
            }
            let head = format!("{nr}|{}", hw.id);
            let mut group_rows = vec![];
            for (i, entry) in group.values.iter().enumerate() {
                match entry {
                    Entry::Section { section, values, one_row } => {
                        let sub = section_rows(values, *one_row, &hw.sensors);
                        if sub.is_empty() {
                            continue;
                        }
                        // The section's id in the key: folded stays folded, also after a language switch.
                        let key = format!("{head}|{section}");
                        group_rows.push(Row { key: key.clone(), depth: 1, text: label(section), kind: RowKind::Section });
                        for (j, (text, sensor, second)) in sub.into_iter().enumerate() {
                            group_rows.push(Row { key: format!("{key}|{j}"), depth: 2, text, kind: RowKind::Value { sensor: Box::new(sensor), second: second.map(Box::new), device: hw.device.clone() } });
                        }
                    }
                    Entry::Value { sensor: pattern, kind, text, exclude } => {
                        for (j, s) in hw.sensors.iter().filter(|s| matches(s, pattern, kind, exclude)).enumerate() {
                            let shown = text.as_deref().map(label).unwrap_or_else(|| s.name.clone());
                            group_rows.push(Row { key: format!("{head}|{i}|{j}"), depth: 1, text: shown, kind: RowKind::Value { sensor: Box::new(s.clone()), second: None, device: hw.device.clone() } });
                        }
                    }
                }
            }
            if !group_rows.is_empty() {
                let title = group.title.as_deref().map(label).unwrap_or_else(|| device_name(hw));
                out.push(Row { key: head, depth: 0, text: title, kind: RowKind::Device { device: hw.device.clone() } });
                out.extend(group_rows);
            }
        }
    }
    out
}

/// The rows of a section: each sensor once (it may match two patterns); with `one_row` values of the same name in
/// one row – "Fan Control #2" belongs to "Fan #2", the control becomes the bar of the speed.
fn section_rows(values: &[Entry], one_row: bool, sensors: &[Sensor]) -> Vec<(String, Sensor, Option<Sensor>)> {
    let mut rows: Vec<(String, Sensor)> = vec![];
    for v in values {
        let Entry::Value { sensor: pattern, kind, text, exclude } = v else { continue };
        for s in sensors.iter().filter(|s| matches(s, pattern, kind, exclude)) {
            if !rows.iter().any(|(_, r)| r.id == s.id) {
                rows.push((text.as_deref().map(label).unwrap_or_else(|| s.name.clone()), s.clone()));
            }
        }
    }
    if !one_row {
        return rows.into_iter().map(|(t, s)| (t, s, None)).collect();
    }
    let mut combined: Vec<(String, String, Sensor, Option<Sensor>)> = vec![];
    for (text, s) in rows {
        let name = s.name.replace(" Control", "").replace("Control ", "");
        match combined.iter_mut().find(|c| c.0 == name) {
            Some(c) if c.3.is_none() => c.3 = Some(s),
            Some(_) => {}
            None => combined.push((name, text, s, None)),
        }
    }
    combined.into_iter().map(|(_, t, s, second)| (t, s, second)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sensors::Reading;
    use serde_json::json;

    #[test]
    fn the_file_in_the_repo_is_valid() {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/detail_window.json")).unwrap();
        let c = Content::parse(&text).unwrap();
        assert!(c.tabs.len() >= 10);
        assert!(c.tabs.iter().any(|t| t.view.as_deref() == Some("fans")));
    }

    #[test]
    fn wildcards() {
        assert!(glob("CCD* (Tdie)", "CCD1 (Tdie)"));
        assert!(glob("*fan*", "GPU Fan 1"));
        assert!(!glob("GPU Fan*", "Fan #1"));
        assert!(glob("*", ""));
    }

    #[test]
    fn rows_of_a_tab() {
        kit::texts!();
        kit::i18n::set_language("en");
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/detail_window.json")).unwrap();
        let c = Content::parse(&text).unwrap();
        let reading = Reading::parse(&json!({"hardware": [
            {"id": "/amdcpu/0", "type": "Cpu", "name": "AMD Ryzen 5 5600X", "parent": null, "sensors": [
                {"id": "t2", "type": "Temperature", "name": "Core (Tctl/Tdie)", "value": 52.5},
                {"id": "t3", "type": "Temperature", "name": "CCD1 (Tdie)", "value": 50.0}]},
            {"id": "/motherboard", "type": "Motherboard", "name": "MSI", "parent": null, "sensors": []},
            {"id": "/lpc/0", "type": "SuperIO", "name": "Nuvoton", "parent": "/motherboard", "sensors": [
                {"id": "f1", "type": "Fan", "name": "Fan #2", "value": 900.0},
                {"id": "c1", "type": "Control", "name": "Fan #2", "value": 40.0, "control": {"min": 0, "max": 100}}]}
        ]}));
        let temps = c.tabs.iter().find(|t| t.title == "tab.temperatures").unwrap();
        let rows = rows(temps, &reading.hardware);
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(texts, vec!["AMD Ryzen 5 5600X", "Core (Tctl/Tdie)", "CCD1 (Tdie)", "MSI – Nuvoton", "Fans", "Fan #2"]);
        let RowKind::Value { second, .. } = &rows[5].kind else { panic!() };
        assert_eq!(second.as_ref().map(|s| s.value), Some(40.0), "the control is the bar of the speed");
        assert_eq!(c.explain(&reading.hardware[0].sensors[1]), kit::i18n::text("explain.temp.ccd").replace("{0}", "1").replace("{}", "1"));
        assert_eq!(want(temps).devices, Some(vec!["cpu".to_string(), "gpu".to_string(), "motherboard".to_string()]));
        assert!(shown(temps, &["cpu".into()]) && !shown(c.tabs.iter().find(|t| t.title == "tab.battery").unwrap(), &["cpu".into()]));
    }
}
