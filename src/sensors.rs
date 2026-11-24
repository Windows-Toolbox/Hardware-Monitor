//! What the bridge reads, and the choices made from it: which temperature goes into the taskbar, which sensor is
//! the CPU's or the graphics card's power, which fans can be controlled. Ported from `python\hardware_monitor.py`.

use serde::{Deserialize, Serialize};

/// With "auto": the first of these that exists (the same lists as the Python part).
pub const CPU_SENSORS: [&str; 3] = ["Core (Tctl/Tdie)", "CPU Package", "Core Average"];
pub const GPU_SENSORS: [&str; 2] = ["GPU Core", "GPU Hot Spot"];
const CPU_POWER: [&str; 2] = ["Package", "CPU Package"];
/// The whole card first.
const GPU_POWER: [&str; 4] = ["GPU Board Power", "GPU Package", "GPU PPT", "GPU Power"];

/// LHM's hardware groups: the first two open in a fraction of a second, the rest takes longer (the RAM ~8 s).
pub const FAST_GROUPS: [&str; 2] = ["IsCpuEnabled", "IsGpuEnabled"];
pub const LATER_GROUPS: [&str; 8] = [
    "IsMemoryEnabled", "IsMotherboardEnabled", "IsControllerEnabled", "IsStorageEnabled",
    "IsNetworkEnabled", "IsPsuEnabled", "IsBatteryEnabled", "IsPowerMonitorEnabled",
];

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Control {
    pub min: f64,
    pub max: f64,
    #[serde(default)]
    pub mode: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Sensor {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub value: f64,
    #[serde(default)]
    pub control: Option<Control>,
}

/// One device or sub-device as the bridge sends it (a sub-device after its parent).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Hardware {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub sensors: Vec<Sensor>,
    /// Filled in by `Reading::parse`: "cpu", "gpu", "ram", "motherboard", "drive", "network", "battery", "other" –
    /// the device of its top-level hardware. (The bridge does not send it; the window gets it filled in.)
    #[serde(default)]
    pub device: String,
    /// The top-level hardware's name ("" for a top-level one).
    #[serde(default)]
    pub top_name: String,
}

/// LHM's hardware type → the device as `detail_window_content.py` names it.
pub fn device(kind: &str) -> &'static str {
    match kind {
        "Cpu" => "cpu",
        "GpuNvidia" | "GpuAmd" | "GpuIntel" => "gpu",
        "Memory" => "ram",
        "Motherboard" | "SuperIO" | "EmbeddedController" => "motherboard",
        "Storage" => "drive",
        "Network" => "network",
        "Battery" => "battery",
        _ => "other",
    }
}

/// The LHM hardware types of a device (what `read` asks the bridge for).
pub fn types(device: &str) -> &'static [&'static str] {
    match device {
        "cpu" => &["Cpu"],
        "gpu" => &["GpuNvidia", "GpuAmd", "GpuIntel"],
        "ram" => &["Memory"],
        "motherboard" => &["Motherboard"],
        "drive" => &["Storage"],
        "network" => &["Network"],
        "battery" => &["Battery"],
        _ => &[],
    }
}

/// One answer to `read`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reading {
    pub hardware: Vec<Hardware>,
    /// The bridge still opens hardware groups in the background.
    pub opening: bool,
}

impl Reading {
    pub fn parse(answer: &serde_json::Value) -> Reading {
        let mut hardware: Vec<Hardware> = serde_json::from_value(answer["hardware"].clone()).unwrap_or_default();
        for h in &mut hardware {
            h.device.clear();
            h.top_name.clear();
        }
        for i in 0..hardware.len() {
            let parent = hardware[i].parent.clone().and_then(|p| hardware[..i].iter().find(|h| h.id == p).cloned());
            let (device, top_name) = match parent {
                Some(p) => (p.device.clone(), if p.top_name.is_empty() { p.name.clone() } else { p.top_name.clone() }),
                None => (self::device(&hardware[i].kind).to_string(), String::new()),
            };
            hardware[i].device = device;
            hardware[i].top_name = top_name;
        }
        Reading { hardware, opening: answer["opening"].as_bool().unwrap_or(false) }
    }

    fn of<'a>(&'a self, device: &'a str) -> impl Iterator<Item = &'a Hardware> + 'a {
        self.hardware.iter().filter(move |h| h.device == device)
    }

    /// The temperature that comes first in `priority` – the chosen one, or the automatic order.
    pub fn temperature(&self, device: &str, priority: &[String]) -> Option<f64> {
        priority.iter().find_map(|name| self.of(device).flat_map(|h| &h.sensors).find(|s| s.kind == "Temperature" && s.name == *name).map(|s| s.value))
    }

    /// Whether there is any temperature of this device (Intel onboard graphics report none).
    pub fn has_temperature(&self, device: &str) -> bool {
        self.of(device).flat_map(|h| &h.sensors).any(|s| s.kind == "Temperature")
    }

    pub fn cpu_power(&self) -> Option<f64> {
        CPU_POWER.iter().find_map(|name| self.of("cpu").flat_map(|h| &h.sensors).find(|s| s.kind == "Power" && s.name == *name).map(|s| s.value))
    }

    /// The power of a graphics card of its own – an integrated one is part of the CPU's package power already.
    pub fn gpu_power(&self) -> Option<f64> {
        let cards: Vec<&Hardware> = self.of("gpu").filter(|h| h.parent.is_none() && !integrated_gpu(&h.name)).collect();
        GPU_POWER.iter().find_map(|name| cards.iter().flat_map(|h| &h.sensors).find(|s| s.kind == "Power" && s.name == *name).map(|s| s.value))
    }

    /// While a laptop runs on battery: what it draws in total. `None` on the charger or without a battery.
    pub fn battery_power(&self) -> Option<f64> {
        self.of("battery").flat_map(|h| &h.sensors).find(|s| s.kind == "Power" && s.name == "Discharge Rate").map(|s| s.value).filter(|w| *w > 0.0)
    }

    /// The devices this PC has sensors for.
    pub fn devices(&self) -> Vec<String> {
        let mut d: Vec<String> = self.hardware.iter().filter(|h| !h.sensors.is_empty()).map(|h| h.device.clone()).collect();
        d.sort();
        d.dedup();
        d
    }

    /// One item per graphics card (all its fans together, like in Afterburner) and one per motherboard fan header –
    /// what LHM can control (a Control sensor with a control).
    pub fn fans(&self) -> Vec<crate::fans::FanItem> {
        use crate::fans::FanItem;
        let mut items = vec![];
        for hw in self.hardware.iter().filter(|h| h.device == "gpu" || h.device == "motherboard") {
            let controls: Vec<&Sensor> = hw.sensors.iter().filter(|s| s.kind == "Control" && s.control.is_some()).collect();
            if controls.is_empty() {
                continue;
            }
            let speeds: Vec<&Sensor> = hw.sensors.iter().filter(|s| s.kind == "Fan").collect();
            if hw.device == "gpu" {
                let name = if hw.top_name.is_empty() { hw.name.clone() } else { hw.top_name.clone() };
                items.push(FanItem {
                    id: hw.id.clone(),
                    kind: "gpu".into(),
                    name,
                    control_ids: controls.iter().map(|s| s.id.clone()).collect(),
                    fan_ids: speeds.iter().map(|s| s.id.clone()).collect(),
                    minimum: controls.iter().filter_map(|s| s.control.as_ref()).map(|c| c.min).fold(0.0, f64::max),
                    maximum: controls.iter().filter_map(|s| s.control.as_ref()).map(|c| c.max).fold(100.0, f64::min),
                });
                continue;
            }
            // "Fan Control #2" belongs to the speed "Fan #2".
            for s in controls {
                let name = s.name.replace(" Control", "").replace("Control ", "").trim().to_string();
                let c = s.control.clone().unwrap_or_default();
                items.push(FanItem {
                    id: s.id.clone(),
                    kind: "case".into(),
                    name: name.clone(),
                    control_ids: vec![s.id.clone()],
                    fan_ids: speeds.iter().filter(|f| f.name == name).map(|f| f.id.clone()).collect(),
                    minimum: c.min,
                    maximum: c.max,
                });
            }
        }
        items
    }
}

/// Graphics in the processor, recognised by its name: Intel except Arc A/B cards, AMD "Radeon(TM) Graphics",
/// "Radeon 780M", "Vega 8 Graphics" (the rule of the Python part).
pub fn integrated_gpu(name: &str) -> bool {
    let words: Vec<&str> = name.split_whitespace().collect();
    let arc_card = words.windows(2).any(|w| w[0].starts_with("Arc") && w[1].len() >= 2 && matches!(w[1].as_bytes()[0], b'A' | b'B') && w[1].as_bytes()[1].is_ascii_digit());
    let radeon_m = words.windows(2).any(|w| w[0] == "Radeon" && w[1].len() == 4 && w[1].ends_with('M') && w[1][..3].bytes().all(|b| b.is_ascii_digit()));
    let vega = words.windows(3).any(|w| w[0] == "Vega" && w[1].bytes().all(|b| b.is_ascii_digit()) && w[2] == "Graphics");
    (name.contains("Intel") && !arc_card) || name.contains("Radeon(TM) Graphics") || radeon_m || vega
}

/// The chosen sensor first; if it does not exist, the automatic order.
pub fn priority(chosen: &str, defaults: &[&str]) -> Vec<String> {
    let mut list: Vec<String> = defaults.iter().map(|s| s.to_string()).collect();
    if chosen != "auto" && !chosen.is_empty() {
        list.insert(0, chosen.to_string());
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reading() -> Reading {
        Reading::parse(&json!({"type": "values", "opening": false, "hardware": [
            {"id": "/amdcpu/0", "type": "Cpu", "name": "AMD Ryzen 5 5600X", "parent": null, "sensors": [
                {"id": "/amdcpu/0/temperature/2", "type": "Temperature", "name": "Core (Tctl/Tdie)", "value": 52.5},
                {"id": "/amdcpu/0/power/0", "type": "Power", "name": "Package", "value": 61.0}]},
            {"id": "/gpu-nvidia/0", "type": "GpuNvidia", "name": "NVIDIA GeForce RTX 4060 Ti", "parent": null, "sensors": [
                {"id": "/gpu-nvidia/0/temperature/0", "type": "Temperature", "name": "GPU Core", "value": 47.0},
                {"id": "/gpu-nvidia/0/power/0", "type": "Power", "name": "GPU Package", "value": 30.0},
                {"id": "/gpu-nvidia/0/fan/1", "type": "Fan", "name": "GPU Fan 1", "value": 0.0},
                {"id": "/gpu-nvidia/0/control/1", "type": "Control", "name": "GPU Fan 1", "value": 0.0, "control": {"min": 30, "max": 100, "mode": "Undefined"}},
                {"id": "/gpu-nvidia/0/control/2", "type": "Control", "name": "GPU Fan 2", "value": 0.0, "control": {"min": 30, "max": 100, "mode": "Undefined"}}]},
            {"id": "/motherboard", "type": "Motherboard", "name": "MSI MAG X570S", "parent": null, "sensors": []},
            {"id": "/lpc/nct6687d/0", "type": "SuperIO", "name": "Nuvoton NCT6687D", "parent": "/motherboard", "sensors": [
                {"id": "/lpc/nct6687d/0/fan/1", "type": "Fan", "name": "Fan #2", "value": 900.0},
                {"id": "/lpc/nct6687d/0/control/1", "type": "Control", "name": "Fan Control #2", "value": 40.0, "control": {"min": 0, "max": 100, "mode": "Default"}}]}
        ]}))
    }

    #[test]
    fn temperatures_by_priority() {
        let r = reading();
        assert_eq!(r.temperature("cpu", &priority("auto", &CPU_SENSORS)), Some(52.5));
        assert_eq!(r.temperature("cpu", &priority("CPU Package", &CPU_SENSORS)), Some(52.5), "a missing choice falls back");
        assert_eq!(r.temperature("gpu", &priority("auto", &GPU_SENSORS)), Some(47.0));
        assert!(r.has_temperature("gpu") && !r.has_temperature("battery"));
        assert_eq!((r.cpu_power(), r.gpu_power(), r.battery_power()), (Some(61.0), Some(30.0), None));
        assert_eq!(r.devices(), vec!["cpu", "gpu", "motherboard"]);
        assert_eq!(r.hardware[3].device, "motherboard", "a sub-device belongs to its parent's device");
    }

    #[test]
    fn controllable_fans() {
        let fans = reading().fans();
        assert_eq!(fans.len(), 2);
        assert_eq!((fans[0].kind.as_str(), fans[0].name.as_str(), fans[0].control_ids.len(), fans[0].minimum), ("gpu", "NVIDIA GeForce RTX 4060 Ti", 2, 30.0));
        assert_eq!((fans[1].kind.as_str(), fans[1].name.as_str(), fans[1].fan_ids.clone()), ("case", "Fan #2", vec!["/lpc/nct6687d/0/fan/1".to_string()]));
    }

    #[test]
    fn integrated_graphics() {
        for name in ["Intel(R) UHD Graphics 770", "Intel(R) Iris(R) Xe Graphics", "AMD Radeon(TM) Graphics", "AMD Radeon 780M", "AMD Radeon Vega 8 Graphics"] {
            assert!(integrated_gpu(name), "{name}");
        }
        for name in ["Intel(R) Arc(TM) A770 Graphics", "Intel Arc B580", "NVIDIA GeForce RTX 4060 Ti", "AMD Radeon RX 7800 XT"] {
            assert!(!integrated_gpu(name), "{name}");
        }
    }
}
