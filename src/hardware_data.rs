//! The fixed data of the hardware for the window's tab "Hardware data" (as in the Python version):
//! manufacturer, model, vendor and product IDs, serial numbers, drivers. Read once when the tab is first shown, not
//! measured – by `LhmBridge.exe --hardware-data` (WMI and DXCore, no admin rights), which hands over the rows as
//! they are; what they mean is worked out here.
//!
//! Categories and fields are ids (e.g. "graphics_card", "vendor_id"); their names and explanations are texts
//! (`hw.category.…`, `hw.field.…`, `hw.explain.…`), filled in when shown or copied.

use std::collections::{BTreeMap, HashMap};
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

use kit::t;
use regex::Regex;
use serde_json::{Map, Value};

use crate::energy::{fmt_date, num};

/// One device: its name and its fields (field id, value).
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub name: String,
    pub fields: Vec<(String, String)>,
}

/// All categories that have devices, in the order of `CATEGORIES`.
pub type Data = Vec<(String, Vec<Device>)>;

/// What the bridge read: the data and the computer's name.
#[derive(Debug, Clone, PartialEq)]
pub struct HardwareData {
    pub data: Data,
    pub computer: String,
}

/// Privacy mode: these fields (and the computer name when copying everything) are shown and copied as HIDDEN.
const SENSITIVE_FIELDS: [&str; 3] = ["serial_number", "mac_address", "processor_id"];
pub const HIDDEN: &str = "████████";

/// The value as it is shown and copied.
pub fn shown<'a>(field: &str, value: &'a str, private: bool) -> &'a str {
    if private && SENSITIVE_FIELDS.contains(&field) { HIDDEN } else { value }
}

// PCI vendor IDs (an excerpt: the most common ones in desktop PCs).
const PCI_VENDORS: [(&str, &str); 45] = [
    ("10DE", "Nvidia"), ("1002", "AMD"), ("1022", "AMD"), ("8086", "Intel"), ("10EC", "Realtek"), ("144D", "Samsung"),
    ("1043", "Asus"), ("1462", "MSI"), ("1458", "Gigabyte"), ("1849", "ASRock"), ("15B7", "SanDisk / WD"), ("1B4B", "Marvell"),
    ("14E4", "Broadcom"), ("168C", "Qualcomm Atheros"), ("1969", "Qualcomm Atheros"), ("1E0F", "Kioxia"), ("1179", "Toshiba"),
    ("1987", "Phison"), ("126F", "Silicon Motion"), ("1CC1", "ADATA"), ("2646", "Kingston"), ("C0A9", "Crucial"),
    ("1344", "Micron"), ("3842", "EVGA"), ("1DA2", "Sapphire"), ("174B", "Sapphire"), ("196E", "PNY"), ("19DA", "Zotac"),
    ("1682", "XFX"), ("148C", "PowerColor"), ("1569", "Palit"), ("10B0", "Gainward"), ("1028", "Dell"), ("103C", "HP"),
    ("17AA", "Lenovo"), ("1025", "Acer"), ("1B21", "ASMedia"), ("1D6A", "Aquantia"), ("14F1", "Conexant / Senary"),
    ("152D", "Quanta"), ("1D72", "Xiaomi"), ("19E5", "Huawei"), ("1414", "Microsoft"), ("1AF4", "Red Hat (virtio)"), ("15AD", "VMware"),
];
// USB vendor IDs (an excerpt).
const USB_VENDORS: [(&str, &str); 33] = [
    ("046D", "Logitech"), ("1532", "Razer"), ("1235", "Focusrite"), ("056A", "Wacom"), ("0DB0", "MSI"), ("1462", "MSI"),
    ("2E1A", "Insta360"), ("045E", "Microsoft"), ("1B1C", "Corsair"), ("1038", "SteelSeries"), ("0951", "Kingston"),
    ("03F0", "HP"), ("04F2", "Chicony"), ("05AC", "Apple"), ("054C", "Sony"), ("0781", "SanDisk"), ("0BDA", "Realtek"),
    ("8087", "Intel"), ("046A", "Cherry"), ("0B05", "Asus"), ("26CE", "ASRock"), ("048D", "ITE"), ("1B21", "ASMedia"),
    ("2109", "VIA Labs"), ("05E3", "Genesys Logic"), ("0A12", "Cambridge Silicon Radio"), ("1050", "Yubico"),
    ("0B95", "ASIX"), ("18D1", "Google"), ("04E8", "Samsung"), ("2717", "Xiaomi"), ("1A86", "QinHeng (WCH)"), ("0483", "STMicroelectronics"),
];
/// RAM manufacturer from the start of the part number – Windows often only reports "Unknown".
const RAM_PART_NUMBERS: [(&str, &str); 8] = [
    (r"^F[345]-", "G.Skill"), (r"^CM", "Corsair"), (r"^(KF|KHX|KVR|KCP)", "Kingston"), (r"^(BL|CT)", "Crucial"),
    (r"^M[34][0-9][0-9]", "Samsung"), (r"^HM", "SK Hynix"), (r"^MTA", "Micron"), (r"^A[XD][45]", "ADATA"),
];
const PLACEHOLDERS: [&str; 7] = ["", "unknown", "default string", "to be filled by o.e.m.", "none", "not available", "00000000"];

fn lookup(table: &[(&str, &'static str)], code: &str) -> Option<&'static str> {
    table.iter().find(|(c, _)| *c == code).map(|(_, n)| *n)
}

fn ram_type(code: Option<i64>) -> Option<&'static str> {
    Some(match code? {
        20 => "DDR",
        21 => "DDR2",
        24 => "DDR3",
        26 => "DDR4",
        29 => "LPDDR3",
        30 => "LPDDR4",
        34 => "DDR5",
        35 => "LPDDR5",
        _ => return None,
    })
}

/// A text id is translated, a name stays.
fn display(entry: &str) -> String {
    if kit::i18n::has(entry) { t!(entry) } else { entry.to_string() }
}

fn ram_form_factor(code: Option<i64>) -> Option<String> {
    match code? {
        8 => Some(display("hw.value.dimm")),
        12 => Some(display("hw.value.sodimm")),
        _ => None,
    }
}

fn net_kind(code: Option<i64>) -> Option<String> {
    Some(match code? {
        14 => display("hw.value.lan"),
        9 | 1 => "WLAN".into(),
        8 => display("hw.value.mobile"),
        10 => "Bluetooth".into(),
        _ => return None,
    })
}

fn bus_type(code: Option<i64>) -> Option<String> {
    Some(match code? {
        1 => "SCSI".into(),
        3 => "ATA".into(),
        7 => "USB".into(),
        8 => "RAID".into(),
        9 => "iSCSI".into(),
        10 => "SAS".into(),
        11 => "SATA".into(),
        12 => display("hw.value.sd_card"),
        17 => "NVMe".into(),
        _ => return None,
    })
}

fn media_type(code: Option<i64>) -> Option<String> {
    match code? {
        3 => Some(display("hw.value.hdd")),
        4 => Some("SSD".into()),
        _ => None,
    }
}

// ---------- Helpers ----------

type Row = Map<String, Value>;

fn get<'a>(row: &'a Row, key: &str) -> &'a str {
    row.get(key).and_then(Value::as_str).unwrap_or("")
}

/// Cleaned-up text – or None for empty values and placeholders like "To Be Filled By O.E.M.".
fn clean(text: &str) -> Option<String> {
    let text = text.trim();
    (!PLACEHOLDERS.contains(&text.to_lowercase().as_str())).then(|| text.to_string())
}

fn int(text: &str) -> Option<i64> {
    text.trim().parse().ok()
}

/// "10DE" → "10DE (Nvidia)"; unknown codes stay as they are.
fn with_name(code: &str, table: &[(&str, &'static str)]) -> String {
    match lookup(table, code) {
        Some(name) => format!("{code} ({name})"),
        None => code.to_string(),
    }
}

/// WMI date "20210303000000.000000+000" → "2021-03-03" (German: "03.03.2021").
fn wmi_date(text: &str) -> Option<String> {
    let digits = text.get(..8).filter(|d| d.bytes().all(|b| b.is_ascii_digit()))?;
    Some(fmt_date(&format!("{}-{}-{}", &digits[..4], &digits[4..6], &digits[6..8])))
}

/// "PCI\VEN_10DE&DEV_2805&…\4&1FC9…" → "PCI\VEN_10DE&DEV_2805&…" (without the part that only names the slot).
fn hardware_id(pnp: &str) -> String {
    if pnp.matches('\\').count() >= 2 { pnp.rsplit_once('\\').map_or(pnp, |(a, _)| a).to_string() } else { pnp.to_string() }
}

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("a valid pattern")
}

/// Vendor, product and subsystem ID from a PCI or HD audio ID.
fn pci_fields(pnp: &str) -> Vec<(String, String)> {
    let upper = pnp.to_uppercase();
    let re = regex(r"VEN_([0-9A-F]{4})&DEV_([0-9A-F]{4})(?:&SUBSYS_([0-9A-F]{8}))?(?:&REV_([0-9A-F]+))?");
    let Some(m) = re.captures(&upper) else { return vec![] };
    let (ven, dev) = (&m[1], &m[2]);
    let mut fields = vec![("vendor_id".to_string(), with_name(ven, &PCI_VENDORS)), ("product_id".into(), dev.to_string())];
    if let Some(subsys) = m.get(3).map(|s| s.as_str()).filter(|s| *s != "00000000") {
        // PCI: SUBSYS_<model><vendor>, audio codecs the other way round: SUBSYS_<vendor><model>.
        let codec = upper.starts_with("HDAUDIO\\") || upper.starts_with("INTELAUDIO\\FUNC_");
        let (sub_dev, sub_ven) = if codec { (&subsys[4..], &subsys[..4]) } else { (&subsys[..4], &subsys[4..]) };
        fields.push(("board_vendor".into(), with_name(sub_ven, &PCI_VENDORS)));
        fields.push(("subsystem_id".into(), sub_dev.to_string()));
    }
    if let Some(rev) = m.get(4) {
        fields.push(("revision".into(), rev.as_str().to_string()));
    }
    fields.push(("hardware_id".into(), hardware_id(pnp)));
    fields
}

fn usb_ids(pnp: &str) -> Option<(String, String)> {
    let m = regex(r"VID_([0-9A-F]{4})&PID_([0-9A-F]{4})").captures(&pnp.to_uppercase()).map(|m| (m[1].to_string(), m[2].to_string()))?;
    Some(m)
}

/// IDs from a PCI or USB ID (network adapters can be either).
fn id_fields(pnp: &str) -> Vec<(String, String)> {
    match usb_ids(pnp) {
        Some((vid, pid)) => vec![("vendor_id".into(), with_name(&vid, &USB_VENDORS)), ("product_id".into(), pid), ("hardware_id".into(), hardware_id(pnp))],
        None => pci_fields(pnp),
    }
}

/// The fields that have a value.
fn fields(list: Vec<(&str, Option<String>)>) -> Vec<(String, String)> {
    list.into_iter().filter_map(|(k, v)| v.filter(|v| !v.is_empty()).map(|v| (k.to_string(), v))).collect()
}

fn prepend(first: Vec<(&str, Option<String>)>, more: Vec<(String, String)>) -> Vec<(String, String)> {
    let mut f = fields(first);
    f.extend(more);
    f
}

fn gb(bytes: i64) -> Option<String> {
    (bytes > 0).then(|| format!("{} GB", bytes / 1024 / 1024 / 1024))
}

// ---------- The categories ----------

fn processors(rows: &[Row]) -> Vec<Device> {
    rows.iter()
        .map(|p| {
            let vendor = clean(get(p, "Manufacturer")).map(|v| match v.as_str() {
                "AuthenticAMD" => "AMD".into(),
                "GenuineIntel" => "Intel".into(),
                _ => v,
            });
            let l3 = int(get(p, "L3CacheSize")).filter(|v| *v > 0);
            let clock = int(get(p, "MaxClockSpeed")).filter(|v| *v > 0);
            Device {
                name: clean(get(p, "Name")).unwrap_or_else(|| t!("hw.name.processor")),
                fields: fields(vec![
                    ("manufacturer", vendor),
                    ("cores_threads", Some(format!("{} / {}", get(p, "NumberOfCores"), get(p, "NumberOfLogicalProcessors")))),
                    ("base_clock", clock.map(|c| format!("{c} MHz"))),
                    ("l3_cache", l3.map(|l| format!("{} MB", l / 1024))),
                    ("socket", clean(get(p, "SocketDesignation"))),
                    ("processor_id", clean(get(p, "ProcessorId"))),
                ]),
            }
        })
        .collect()
}

/// DXCore's word per GPU: (vendor, device, subsystem) → integrated / external / dedicated.
fn gpu_kinds(gpus: &[Value]) -> HashMap<(String, String, String), String> {
    gpus.iter()
        .filter_map(|g| {
            let id = |k: &str| g[k].as_str().map(str::to_string);
            let kind = if g["integrated"].as_bool() == Some(true) {
                t!("hw.value.integrated")
            } else if g["detachable"].as_bool() == Some(true) {
                t!("hw.value.external")
            } else {
                t!("hw.value.dedicated")
            };
            Some(((id("vendor")?, id("device")?, id("subsys")?), kind))
        })
        .collect()
}

fn graphics_cards(rows: &[Row], gpus: &[Value]) -> Vec<Device> {
    let kinds = gpu_kinds(gpus);
    let re = regex(r"VEN_(\w{4})&DEV_(\w{4})(?:&SUBSYS_(\w{8}))?");
    rows.iter()
        .filter_map(|g| {
            let pnp = get(g, "PNPDeviceID");
            let upper = pnp.to_uppercase();
            // Virtual displays (remote access, streaming) are left out.
            if !upper.starts_with("PCI\\") {
                return None;
            }
            let kind = re.captures(&upper).and_then(|m| {
                let (ven, dev, sub) = (m[1].to_string(), m[2].to_string(), m.get(3).map_or(String::new(), |s| s.as_str().to_string()));
                kinds.get(&(ven.clone(), dev.clone(), sub)).or_else(|| kinds.iter().find(|((v, d, _), _)| *v == ven && *d == dev).map(|(_, k)| k)).cloned()
            });
            let mut f = prepend(vec![("kind", kind)], pci_fields(pnp));
            f.extend(fields(vec![("driver_version", clean(get(g, "DriverVersion"))), ("driver_date", wmi_date(get(g, "DriverDate")))]));
            Some(Device { name: clean(get(g, "Name")).unwrap_or_else(|| t!("hw.name.graphics_card")), fields: f })
        })
        .collect()
}

fn motherboards(boards: &[Row], bios: &[Row]) -> Vec<Device> {
    boards
        .iter()
        .map(|b| {
            let (vendor, model) = (clean(get(b, "Manufacturer")), clean(get(b, "Product")));
            let mut list = vec![("manufacturer", vendor.clone()), ("model", model.clone()), ("version", clean(get(b, "Version"))), ("serial_number", clean(get(b, "SerialNumber")))];
            for x in bios {
                list.push(("bios_vendor", clean(get(x, "Manufacturer"))));
                list.push(("bios_version", clean(get(x, "SMBIOSBIOSVersion"))));
                list.push(("bios_date", wmi_date(get(x, "ReleaseDate"))));
            }
            let name = [vendor, model].into_iter().flatten().collect::<Vec<_>>().join(" ");
            Device { name: if name.is_empty() { t!("hw.name.motherboard") } else { name }, fields: fields(list) }
        })
        .collect()
}

fn memory_modules(rows: &[Row]) -> Vec<Device> {
    let mut devices = vec![];
    // LPDDR is always soldered and comes per memory channel → combined: name → (vendor, part, bytes, type, speed, channels).
    let mut soldered: Vec<(String, Option<String>, Option<String>, i64, String, Option<String>, u32)> = vec![];
    for r in rows {
        let part = clean(get(r, "PartNumber"));
        let mut vendor = clean(get(r, "Manufacturer"));
        let mut vendor_name = vendor.clone();
        if vendor.is_none()
            && let Some(p) = &part
        {
            vendor_name = RAM_PART_NUMBERS.iter().find(|(pattern, _)| regex(pattern).is_match(p)).map(|(_, n)| n.to_string());
            vendor = vendor_name.as_ref().map(|v| t!("hw.value.from_part_number", vendor = v));
        }
        let size = int(get(r, "Capacity")).unwrap_or(0);
        let kind = ram_type(int(get(r, "SMBIOSMemoryType")));
        let (configured, rated) = (int(get(r, "ConfiguredClockSpeed")).filter(|v| *v > 0), int(get(r, "Speed")).filter(|v| *v > 0));
        let name = [vendor_name.clone(), part.clone()].into_iter().flatten().collect::<Vec<_>>().join(" ");
        if let Some(k) = kind.filter(|k| k.starts_with("LP")) {
            let key = if name.is_empty() { t!("hw.name.memory") } else { name };
            match soldered.iter_mut().find(|g| g.0 == key) {
                Some(g) => {
                    g.3 += size;
                    g.6 += 1;
                }
                None => soldered.push((key, vendor, part, size, k.to_string(), configured.map(|c| format!("{c} MT/s")), 1)),
            }
            continue;
        }
        let slot = [clean(get(r, "BankLabel")), clean(get(r, "DeviceLocator"))].into_iter().flatten().collect::<Vec<_>>().join(" / ");
        devices.push(Device {
            name: if name.is_empty() { t!("hw.name.memory_module") } else { name },
            fields: fields(vec![
                ("manufacturer", vendor),
                ("part_number", part),
                ("size", gb(size)),
                ("type", kind.map(str::to_string)),
                ("form_factor", ram_form_factor(int(get(r, "FormFactor")))),
                ("speed", configured.map(|c| format!("{c} MT/s"))),
                ("rated_speed", rated.filter(|r| Some(*r) != configured).map(|r| format!("{r} MT/s"))),
                ("slot", Some(slot)),
                ("serial_number", clean(get(r, "SerialNumber"))),
            ]),
        });
    }
    for (name, vendor, part, size, kind, speed, channels) in soldered.into_iter().rev() {
        devices.insert(
            0,
            Device {
                name,
                fields: fields(vec![
                    ("manufacturer", vendor),
                    ("part_number", part),
                    ("size", gb(size)),
                    ("type", Some(kind)),
                    ("speed", speed),
                    ("form_factor", Some(t!("hw.value.soldered"))),
                    ("channels", Some(channels.to_string())),
                ]),
            },
        );
    }
    devices
}

fn drives(rows: &[Row]) -> Vec<Device> {
    let mut rows: Vec<&Row> = rows.iter().collect();
    rows.sort_by_key(|d| int(get(d, "DeviceId")).unwrap_or(0));
    rows.into_iter()
        .map(|d| {
            let size = get(d, "Size").parse::<f64>().ok().filter(|s| *s > 0.0);
            Device {
                name: clean(get(d, "FriendlyName")).unwrap_or_else(|| t!("hw.name.drive")),
                fields: fields(vec![
                    ("kind", media_type(int(get(d, "MediaType")))),
                    ("interface", bus_type(int(get(d, "BusType")))),
                    ("size", size.map(|s| if s >= 1e12 { format!("{} TB", num(s / 1e12, 1)) } else { format!("{:.0} GB", s / 1e9) })),
                    ("serial_number", clean(get(d, "SerialNumber")).map(|s| s.trim_end_matches('.').to_string())),
                    ("firmware", clean(get(d, "FirmwareVersion"))),
                    ("disk_number", Some(get(d, "DeviceId").to_string())),
                ]),
            }
        })
        .collect()
}

fn network_adapters(rows: &[Row], kinds: &[Row]) -> Vec<Device> {
    // The kind (LAN, WLAN, …) is only in the newer class MSFT_NetAdapter – Win32_NetworkAdapter also reports WLAN as
    // "Ethernet". They are matched by the device ID.
    let kinds: HashMap<String, Option<String>> = kinds.iter().map(|a| (get(a, "PnPDeviceID").to_uppercase(), net_kind(int(get(a, "NdisPhysicalMedium"))))).collect();
    rows.iter()
        .filter_map(|n| {
            let pnp = get(n, "PNPDeviceID");
            let upper = pnp.to_uppercase();
            // Virtual adapters (VPN, Hyper-V, …) are left out.
            if !(upper.starts_with("PCI\\") || upper.starts_with("USB\\")) {
                return None;
            }
            let mut f = prepend(vec![("kind", kinds.get(&upper).cloned().flatten())], id_fields(pnp));
            f.extend(fields(vec![("mac_address", clean(get(n, "MACAddress")))]));
            Some(Device { name: clean(get(n, "Name")).unwrap_or_else(|| t!("hw.name.network_adapter")), fields: f })
        })
        .collect()
}

fn audio_devices(rows: &[Row]) -> Vec<Device> {
    let realtek = regex(r"VEN_10EC&DEV_0([0-9A-F]{3})");
    rows.iter()
        .filter_map(|s| {
            let pnp = get(s, "PNPDeviceID");
            let upper = pnp.to_uppercase();
            // INTELAUDIO\FUNC_: the codec behind Intel Smart Sound (laptops) – the other INTELAUDIO entries are only
            // endpoints of it. USB audio is listed under USB devices, virtual ones are left out.
            if !(upper.starts_with("PCI") || upper.starts_with("HDAUDIO") || upper.starts_with("INTELAUDIO\\FUNC_")) {
                return None;
            }
            let mut name = clean(get(s, "Name")).unwrap_or_else(|| t!("hw.name.audio_device"));
            // Realtek codecs are named after their product ID, e.g. 0892 → ALC892.
            if let Some(m) = realtek.captures(&upper)
                && name.starts_with("High Definition Audio")
            {
                name = format!("Realtek ALC{}", &m[1]);
            }
            Some(Device { name, fields: pci_fields(pnp) })
        })
        .collect()
}

/// Windows names that say nothing about the device (also in German, as Windows reports them in its language).
fn generic(name: &str) -> bool {
    let l = name.to_lowercase();
    let word = |w: &str| l.strip_prefix(w).is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()));
    word("usb") || word("hid") || ["verbundgerät", "composite", "eingabegerät", "input device", "audiogerät"].iter().any(|g| l.contains(g))
}

/// Connected USB devices, one per vendor/product ID (a device often registers several times).
fn usb_devices(rows: &[Row]) -> Vec<Device> {
    let mut groups: BTreeMap<(String, String), Vec<&Row>> = BTreeMap::new();
    for e in rows {
        if let Some(ids) = usb_ids(get(e, "PNPDeviceID")) {
            groups.entry(ids).or_default().push(e);
        }
    }
    let mut devices: Vec<Device> = groups
        .into_iter()
        .map(|((vid, pid), entries)| {
            let names: Vec<String> = entries.iter().filter_map(|e| clean(get(e, "Name"))).filter(|n| !generic(n)).collect();
            let vendor = lookup(&USB_VENDORS, &vid)
                .map(str::to_string)
                .or_else(|| entries.iter().filter_map(|e| clean(get(e, "Manufacturer"))).find(|v| !v.starts_with('(') && !v.starts_with("Microsoft")));
            // Serial number: many devices have it as the last part of the ID (without "&" – that would be a Windows number).
            let serial = entries.iter().find_map(|e| {
                let pnp = get(e, "PNPDeviceID");
                let last = pnp.rsplit_once('\\')?.1;
                (!pnp.to_uppercase().contains("&MI_") && !last.contains('&')).then(|| last.to_string())
            });
            let name = match (names.first(), &vendor) {
                (Some(n), _) => n.clone(),
                (None, Some(v)) => format!("{} ({v})", t!("hw.name.usb_device")),
                (None, None) => t!("hw.name.usb_device"),
            };
            let vendor_id = if lookup(&USB_VENDORS, &vid).is_some() {
                with_name(&vid, &USB_VENDORS)
            } else if let Some(v) = &vendor {
                format!("{vid} ({v})")
            } else {
                vid.clone()
            };
            Device {
                name,
                fields: fields(vec![("vendor_id", Some(vendor_id)), ("product_id", Some(pid.clone())), ("serial_number", serial), ("hardware_id", Some(format!("USB\\VID_{vid}&PID_{pid}")))]),
            }
        })
        .collect();
    devices.sort_by_key(|d| d.name.to_lowercase());
    devices
}

/// The bridge's answer → the categories that have devices.
pub fn parse(v: &Value) -> Data {
    let rows = |name: &str| -> Vec<Row> { v["wmi"][name].as_array().map(|a| a.iter().filter_map(|r| r.as_object().cloned()).collect()).unwrap_or_default() };
    let gpus = v["gpus"].as_array().cloned().unwrap_or_default();
    let categories: Vec<(&str, Vec<Device>)> = vec![
        ("processor", processors(&rows("processor"))),
        ("graphics_card", graphics_cards(&rows("video"), &gpus)),
        ("motherboard", motherboards(&rows("board"), &rows("bios"))),
        ("memory", memory_modules(&rows("memory"))),
        ("drives", drives(&rows("disks"))),
        ("network", network_adapters(&rows("network"), &rows("net_kinds"))),
        ("audio", audio_devices(&rows("sound"))),
        ("usb_devices", usb_devices(&rows("usb"))),
    ];
    categories.into_iter().filter(|(_, d)| !d.is_empty()).map(|(c, d)| (c.to_string(), d)).collect()
}

/// Runs `LhmBridge.exe --hardware-data` next to this exe (no admin rights needed) – a second or two.
pub fn read() -> Result<HardwareData, String> {
    let exe = kit::app::program_dir().join("LhmBridge.exe");
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new(&exe)
        .arg("--hardware-data")
        .creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", exe.display()))?;
    let v: Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("{}: {e}", exe.display()))?;
    for e in v["errors"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        kit::app::log(&format!("hardware data: {e}"));
    }
    Ok(HardwareData { data: parse(&v), computer: v["computer"].as_str().unwrap_or_default().to_string() })
}

// ---------- As text ----------

pub fn category_name(category: &str) -> String {
    t!(&format!("hw.category.{category}"))
}

pub fn field_name(field: &str) -> String {
    t!(&format!("hw.field.{field}"))
}

/// What a field means – for the footer.
pub fn explanation(field: &str) -> Option<String> {
    let id = format!("hw.explain.{field}");
    kit::i18n::has(&id).then(|| t!(&id))
}

/// One category with all devices and fields – for a forum, a chat, notes.
pub fn as_text(category: &str, devices: &[Device], private: bool) -> String {
    let blocks: Vec<String> = devices
        .iter()
        .map(|d| {
            let mut lines = vec![d.name.clone()];
            lines.extend(d.fields.iter().map(|(f, v)| format!("  {}: {}", field_name(f), shown(f, v, private))));
            lines.join("\n")
        })
        .collect();
    format!("=== {} ===\n{}", category_name(category), blocks.join("\n\n"))
}

/// All categories, with the computer's name (hidden in privacy mode) and the date on top.
pub fn all_as_text(hw: &HardwareData, private: bool) -> String {
    let header = t!("hw.text.header", pc = if private { HIDDEN } else { hw.computer.as_str() }, date = fmt_date(&crate::energy::today()));
    let body: Vec<String> = hw.data.iter().map(|(c, d)| as_text(c, d, private)).collect();
    format!("{header}\n\n{}\n", body.join("\n\n\n"))
}

/// The device names of a category – identical devices combined ("2× G.Skill …").
pub fn model_names(devices: &[Device]) -> Vec<String> {
    let mut names: Vec<(String, usize)> = vec![];
    for d in devices {
        match names.iter_mut().find(|(n, _)| *n == d.name) {
            Some(e) => e.1 += 1,
            None => names.push((d.name.clone(), 1)),
        }
    }
    names.into_iter().map(|(n, c)| if c > 1 { format!("{c}× {n}") } else { n }).collect()
}

/// Only the model names: the category, below it one line per device with a tab in front.
pub fn model_names_as_text(data: &[(String, Vec<Device>)]) -> String {
    let mut lines = vec![];
    for (category, devices) in data {
        lines.push(category_name(category));
        lines.extend(model_names(devices).into_iter().map(|n| format!("\t{n}")));
    }
    lines.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ids_and_names() {
        kit::texts!();
        kit::i18n::set_language("en");
        let f = pci_fields(r"PCI\VEN_10DE&DEV_2805&SUBSYS_89211043&REV_A1\4&1FC9D7B&0&0019");
        let get = |k: &str| f.iter().find(|(f, _)| f == k).map(|(_, v)| v.as_str());
        assert_eq!(get("vendor_id"), Some("10DE (Nvidia)"));
        assert_eq!((get("product_id"), get("board_vendor"), get("subsystem_id"), get("revision")), (Some("2805"), Some("1043 (Asus)"), Some("8921"), Some("A1")));
        assert_eq!(get("hardware_id"), Some(r"PCI\VEN_10DE&DEV_2805&SUBSYS_89211043&REV_A1"));
        // Audio codecs: vendor first in SUBSYS.
        let codec = pci_fields(r"HDAUDIO\FUNC_01&VEN_10EC&DEV_0897&SUBSYS_14627D78&REV_1003\5&2");
        assert!(codec.contains(&("board_vendor".into(), "1462 (MSI)".into())) && codec.contains(&("subsystem_id".into(), "7D78".into())));
        assert_eq!(usb_ids(r"USB\VID_046d&PID_C539\5&1"), Some(("046D".into(), "C539".into())));
        assert_eq!((clean(" To Be Filled By O.E.M. "), clean(" X ")), (None, Some("X".into())));
        assert_eq!(wmi_date("20210303000000.000000+000").as_deref(), Some("2021-03-03"));
        assert!(generic("USB-Verbundgerät") && generic("HID-konforme Maus") && !generic("USBasp") && !generic("Logitech G Pro"));
        let d = vec![Device { name: "A".into(), fields: vec![] }, Device { name: "A".into(), fields: vec![] }, Device { name: "B".into(), fields: vec![] }];
        assert_eq!(model_names(&d), ["2× A", "B"]);
        assert_eq!(shown("serial_number", "123", true), HIDDEN);
        assert_eq!(shown("model", "X", true), "X");
    }

    #[test]
    fn a_bridge_answer() {
        kit::texts!();
        kit::i18n::set_language("en");
        let v = json!({
            "wmi": {
                "processor": [{"Name": "AMD Ryzen 5 5600X 6-Core Processor", "Manufacturer": "AuthenticAMD", "NumberOfCores": "6", "NumberOfLogicalProcessors": "12", "MaxClockSpeed": "3701", "L3CacheSize": "32768", "SocketDesignation": "AM4", "ProcessorId": "X"}],
                "video": [{"Name": "NVIDIA GeForce RTX 4060 Ti", "PNPDeviceID": r"PCI\VEN_10DE&DEV_2805&SUBSYS_89211043&REV_A1\4&1", "DriverVersion": "32.0.15.8180", "DriverDate": "20251020000000.000000-000"},
                          {"Name": "Remote Display", "PNPDeviceID": r"SWD\REMOTEDISPLAY\1"}],
                "memory": [{"Manufacturer": "Unknown", "PartNumber": "F4-3200C16-8GVKB ", "Capacity": "8589934592", "SMBIOSMemoryType": "26", "FormFactor": "8", "ConfiguredClockSpeed": "3200", "Speed": "3200", "BankLabel": "P0 CHANNEL A", "DeviceLocator": "DIMM 0"}],
                "usb": [{"Name": "USB-Verbundgerät", "PNPDeviceID": r"USB\VID_046D&PID_C539\5&1&0&1"}, {"Name": "Lightspeed Receiver", "PNPDeviceID": r"USB\VID_046D&PID_C539&MI_00\6&2"}],
            },
            "gpus": [{"vendor": "10DE", "device": "2805", "subsys": "89211043", "integrated": false, "detachable": false}],
        });
        let data = parse(&v);
        assert_eq!(data.iter().map(|(c, _)| c.as_str()).collect::<Vec<_>>(), ["processor", "graphics_card", "memory", "usb_devices"]);
        let cpu = &data[0].1[0];
        assert!(cpu.fields.contains(&("manufacturer".into(), "AMD".into())) && cpu.fields.contains(&("l3_cache".into(), "32 MB".into())));
        let gpu = &data[1].1;
        assert_eq!(gpu.len(), 1);
        assert_eq!(gpu[0].fields[0], ("kind".into(), t!("hw.value.dedicated")));
        let ram = &data[2].1[0];
        assert_eq!(ram.name, "G.Skill F4-3200C16-8GVKB");
        assert!(ram.fields.contains(&("size".into(), "8 GB".into())) && ram.fields.contains(&("type".into(), "DDR4".into())) && !ram.fields.iter().any(|(f, _)| f == "rated_speed"));
        let usb = &data[3].1[0];
        assert_eq!(usb.name, "Lightspeed Receiver");
        assert!(usb.fields.contains(&("vendor_id".into(), "046D (Logitech)".into())));
    }
}
