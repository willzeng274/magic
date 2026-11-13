//! Remembers every device this Mac has seen, so one that ends up paired with no Mac
//! stays in the list and can be rescued.

use crate::{
    device::{Device, Kind},
    host,
};
use std::{fmt::Write, fs, path::PathBuf};

fn path() -> PathBuf {
    host::home_dir().join(".config/magic/known")
}

/// Loads the remembered devices, one `address<TAB>kind<TAB>name` per line.
pub fn load() -> Vec<Device> {
    fs::read_to_string(path())
        .unwrap_or_default()
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, '\t');
            let address = fields.next()?.to_string();
            let kind = Kind::parse(fields.next()?)?;
            let name = fields.next()?.to_string();
            Some(Device::new(address, name, kind))
        })
        .collect()
}

/// Saves the list, skipping the write when nothing changed.
pub fn save(devices: &[Device]) {
    let mut text = String::new();
    for d in devices {
        let _ = writeln!(text, "{}\t{}\t{}", d.address, d.kind, d.name);
    }
    let path = path();
    if fs::read_to_string(&path).is_ok_and(|current| current == text) {
        return;
    }
    if let Some(folder) = path.parent() {
        let _ = fs::create_dir_all(folder);
    }
    let _ = fs::write(path, text);
}

pub fn forget(address: &str) {
    let kept: Vec<Device> = load()
        .into_iter()
        .filter(|device| device.address != address)
        .collect();
    save(&kept);
}
