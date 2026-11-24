//! Between the monitor and its detail window – two processes of the same exe (Toolbox\DESIGN.md 5, "the roles of
//! an exe"): the monitor runs all day and talks to the bridge; the window is started on a click, gets what it
//! shows from the monitor and ends when it is closed, giving all its memory back.
//!
//! One pipe, `windows-toolbox.hardware_monitor.window`, served by the monitor; one JSON object per line. The window
//! says what it shows (`Want`) – the monitor then reads that each round and sends a `State`. Without a window the
//! monitor reads only what the taskbar needs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sensors::Hardware;

/// The window's pipe, as `kit::ipc` names it.
pub const PIPE: &str = "hardware_monitor.window";
/// The internal argument the monitor starts the window with: `--window [tab]`.
pub const ARGUMENT: &str = "--window";

/// What the window shows right now – and so what the monitor reads for it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Want {
    /// "list" (values of `devices`), "power", "fans", "health", or "" (nothing: minimized, hardware data).
    pub view: String,
    /// For "list": the devices – `None` = all ("all" tab).
    #[serde(default)]
    pub devices: Option<Vec<String>>,
}

impl Want {
    /// The LHM hardware types the monitor asks the bridge for; `None` = everything.
    pub fn types(&self) -> Option<Vec<&'static str>> {
        let devices: Vec<&str> = match self.view.as_str() {
            "list" => self.devices.as_ref()?.iter().map(String::as_str).collect(),
            "fans" => vec!["gpu", "motherboard"],
            _ => vec![],
        };
        Some(devices.iter().flat_map(|d| crate::sensors::types(d).iter().copied()).collect())
    }

    pub fn anything(&self) -> bool {
        !self.view.is_empty()
    }
}

/// Window → monitor.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToMonitor {
    Want(Want),
    /// One fan's settings changed in the curve editor (`fan_control` in the monitor's file).
    Fan { id: String, config: Value },
    /// The privacy switch in the window's header: on or off (not "toggle" – two quick clicks stay right).
    Privacy { on: bool },
    /// Size and position when the window closes: "WxH+X+Y" in pixels, as the Python window wrote it.
    Geometry { value: String },
}

/// Monitor → window.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToWindow {
    State(Box<State>),
    /// Come to the front – on this tab if given (a second click, or "Show chart" on the page).
    Front { tab: Option<String> },
}

/// The settings the window needs.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Settings {
    pub language: String,
    pub theme: String,
    pub zoom: f32,
    pub cpu_warn_temp: u32,
    pub cpu_hot_temp: u32,
    pub gpu_warn_temp: u32,
    pub gpu_hot_temp: u32,
    pub privacy_mode: bool,
    /// "WxH+X+Y" or "".
    pub window: String,
    pub energy_logging: bool,
    pub energy_price: f64,
    pub energy_currency: String,
    pub plug: bool,
}

/// One controllable fan and what the control does with it.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Fan {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub control_ids: Vec<String>,
    pub fan_ids: Vec<String>,
    pub minimum: f64,
    pub maximum: f64,
    pub software: bool,
    pub pct: Option<f64>,
    pub note: Option<String>,
    /// Its entry of `fan_control` (missing = the defaults).
    pub config: Option<Value>,
}

/// The power right now.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct Power {
    pub estimate: f64,
    pub cpu: Option<f64>,
    pub gpu: Option<f64>,
    pub battery: Option<f64>,
    /// Measured by the smart plug.
    pub plug: Option<f64>,
    pub plug_name: String,
}

/// One round of the monitor, for the window.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
pub struct State {
    pub settings: Settings,
    /// The taskbar's temperatures, in the taskbar's order; a kind without a temperature is left out.
    pub temps: Vec<(String, Option<f64>)>,
    /// What was read for the window this round; `None` = nothing for it (yet).
    pub hardware: Option<Vec<Hardware>>,
    /// The `Want` this reading answers.
    pub answers: Want,
    /// The rest of the hardware is still being opened (the first seconds after the start).
    pub loading: bool,
    /// The devices this PC has sensors for – known once everything is open.
    pub present: Option<Vec<String>>,
    pub power: Power,
    /// The logs, for the chart – only while it is shown.
    pub hours: Option<BTreeMap<String, [f64; 2]>>,
    pub plug_hours: Option<BTreeMap<String, [f64; 2]>>,
    pub fans: Vec<Fan>,
    /// The last drive health reading (the bridge's `drives` answer) – only while that tab is shown.
    pub drives: Option<Value>,
}
