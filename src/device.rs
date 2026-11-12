use crate::host::Host;
use serde_json::Value;
use std::{collections::BTreeMap, fmt, thread};

const APPLE_VENDOR_ID: &str = "0x004C";
const HID_MARKER: &str = "<<<HID>>>";

/// Prints the Bluetooth device list as JSON, then the HID registry entries that say
/// which devices are attached by cable.
const SCAN_COMMAND: &str = concat!(
    "system_profiler SPBluetoothDataType -json; echo '<<<HID>>>'; ",
    "ioreg -r -c AppleDeviceManagementHIDEventService -l | ",
    r#"grep -E '\+-o|"DeviceAddress"|"Product"|"Transport"'"#,
);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Keyboard,
    Mouse,
    Trackpad,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Keyboard => "keyboard",
            Kind::Mouse => "mouse",
            Kind::Trackpad => "trackpad",
        })
    }
}

/// How a device relates to one Mac.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Link {
    Connected,
    /// Paired with that Mac but not currently connected to it.
    Paired,
    /// Attached with a cable. It cannot be moved until it is unplugged.
    Usb,
}

#[derive(Clone)]
pub struct Device {
    /// Bluetooth address, upper case with colons.
    pub address: String,
    pub name: String,
    pub kind: Kind,
    pub battery: Option<String>,
    /// State on each Mac that knows the device, by host key.
    pub links: BTreeMap<String, Link>,
}

impl Device {
    fn new(address: String, name: String, kind: Kind) -> Self {
        Self {
            address,
            name,
            kind,
            battery: None,
            links: BTreeMap::new(),
        }
    }

    /// Command-line name: lower case, with runs of other characters turned into dashes.
    pub fn slug(&self) -> String {
        self.name
            .to_lowercase()
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("-")
    }

    pub fn is_live(&self) -> bool {
        self.links.values().any(|link| *link != Link::Paired)
    }

    pub fn cabled_to(&self) -> Option<&str> {
        self.links
            .iter()
            .find(|(_, link)| **link == Link::Usb)
            .map(|(host, _)| host.as_str())
    }
}

pub struct Scan {
    pub devices: Vec<Device>,
    /// Whether each Mac answered, by host key.
    pub reachable: BTreeMap<String, bool>,
}

/// Asks every Mac, in parallel, which Magic devices it knows.
pub fn scan(hosts: &[Host]) -> Scan {
    let workers: Vec<_> = hosts
        .iter()
        .cloned()
        .map(|host| thread::spawn(move || (host.key.clone(), host.run(SCAN_COMMAND))))
        .collect();

    let mut devices = BTreeMap::new();
    let mut reachable = BTreeMap::new();
    for worker in workers {
        let Ok((host, output)) = worker.join() else {
            continue;
        };
        let (bluetooth, hid) = output
            .text
            .split_once(HID_MARKER)
            .unwrap_or((&output.text, ""));
        let report: Option<Value> = serde_json::from_str(bluetooth).ok().filter(|_| output.ok);
        reachable.insert(host.clone(), report.is_some());
        if let Some(report) = report {
            read_bluetooth(&report, &host, &mut devices);
            read_cabled(hid, &host, &mut devices);
        }
    }

    let mut devices: Vec<Device> = devices.into_values().collect();
    devices.sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
    Scan { devices, reachable }
}

fn read_bluetooth(report: &Value, host: &str, devices: &mut BTreeMap<String, Device>) {
    let controller = &report["SPBluetoothDataType"][0];
    let sections = [
        ("device_connected", Link::Connected),
        ("device_not_connected", Link::Paired),
    ];
    for (section, link) in sections {
        let entries = controller[section].as_array().into_iter().flatten();
        for (name, info) in entries.filter_map(Value::as_object).flatten() {
            let field = |key: &str| info[key].as_str().unwrap_or_default();
            let Some(kind) = magic_kind(name, field("device_minorType"), field("device_vendorID"))
            else {
                continue;
            };
            let address = field("device_address").to_uppercase();
            if address.is_empty() {
                continue;
            }
            let device = devices
                .entry(address.clone())
                .or_insert_with(|| Device::new(address, name.clone(), kind));
            if link == Link::Connected {
                let level = field("device_batteryLevelMain");
                device.battery = (!level.is_empty()).then(|| level.to_string());
            }
            device.links.insert(host.to_string(), link);
        }
    }
}

/// Reads `ioreg` lines. Each registry entry starts with `+-o` and lists its
/// address and product before its transport.
fn read_cabled(hid: &str, host: &str, devices: &mut BTreeMap<String, Device>) {
    let mut address = String::new();
    let mut product = String::new();
    for line in hid.lines() {
        let value = line
            .split_once('=')
            .map_or("", |(_, value)| value.trim().trim_matches('"'));
        if line.contains("+-o") {
            address.clear();
            product.clear();
        } else if line.contains("\"DeviceAddress\"") {
            address = value.replace('-', ":").to_uppercase();
        } else if line.contains("\"Product\"") {
            product = value.to_string();
        } else if line.contains("\"Transport\"") && value == "USB" && !address.is_empty() {
            let device = devices.entry(address.clone()).or_insert_with(|| {
                Device::new(address.clone(), product.clone(), kind_from_name(&product))
            });
            device.links.insert(host.to_string(), Link::Usb);
        }
    }
}

/// Returns the kind of device if it is an Apple or Magic keyboard, mouse or trackpad.
fn magic_kind(name: &str, minor_type: &str, vendor_id: &str) -> Option<Kind> {
    let name = name.to_lowercase();
    let minor_type = minor_type.to_lowercase();
    let kind = if name.contains("trackpad") || minor_type.contains("trackpad") {
        Kind::Trackpad
    } else if minor_type.contains("keyboard") {
        Kind::Keyboard
    } else if minor_type.contains("mouse") {
        Kind::Mouse
    } else {
        return None;
    };
    (vendor_id.eq_ignore_ascii_case(APPLE_VENDOR_ID) || name.contains("magic")).then_some(kind)
}

fn kind_from_name(name: &str) -> Kind {
    let name = name.to_lowercase();
    if name.contains("trackpad") {
        Kind::Trackpad
    } else if name.contains("mouse") {
        Kind::Mouse
    } else {
        Kind::Keyboard
    }
}
