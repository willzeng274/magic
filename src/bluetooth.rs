//! Moving a device: the Mac that holds it unpairs it, then the target Mac pairs it.
//!
//! Every `blueutil` call goes over SSH, including calls to the local Mac. macOS aborts
//! `blueutil` when a terminal app runs it, even with Bluetooth access granted, but allows
//! it for SSH once `/usr/libexec/sshd-keygen-wrapper` has that access.

use crate::{
    device::{Device, Link},
    host::Host,
};
use std::{thread, time::Duration};

const BLUEUTIL: &str = "/opt/homebrew/bin/blueutil";
const PAIR_ATTEMPTS: u32 = 6;
const SSH_FAILURES: &[&str] = &[
    "Permission denied",
    "Could not resolve",
    "timed out",
    "refused",
];

/// Checks that a Mac can control Bluetooth. The error says what to do about it.
pub fn check(host: &Host) -> Result<(), String> {
    let output = host.run_over_ssh(&format!("{BLUEUTIL} --power")).text;
    if output == "1" {
        return Ok(());
    }
    let mac = &host.key;
    Err(
        if output.contains("No such file") || output.contains("not found") {
            "blueutil is missing. Run: brew install blueutil".to_string()
        } else if SSH_FAILURES.iter().any(|failure| output.contains(failure)) {
            "cannot log in over SSH. Run: magic setup".to_string()
        } else {
            format!(
                "SSH is not allowed to use Bluetooth there. On {mac}: System Settings > \
             Privacy & Security > Bluetooth > +, press Cmd+Shift+G, \
             add /usr/libexec/sshd-keygen-wrapper."
            )
        },
    )
}

/// Moves one device to the Mac named `to`, reporting progress through `log`.
/// Returns whether the device ended up connected there.
pub fn move_device(hosts: &[Host], device: &Device, to: &str, log: &dyn Fn(String)) -> bool {
    let name = &device.name;
    let Some(target) = hosts.iter().find(|host| host.key == to) else {
        log(format!("unknown Mac '{to}'"));
        return false;
    };
    if device.links.get(to) == Some(&Link::Connected) {
        log(format!("{name}: already on {to}"));
        return true;
    }
    if let Some(mac) = device.cabled_to() {
        log(format!(
            "{name}: plugged into {mac} with a cable. \
             Unplug it, wait for it to show connected, then move it."
        ));
        return false;
    }

    let holders: Vec<&Host> = hosts
        .iter()
        .filter(|host| host.key != to && device.links.contains_key(&host.key))
        .collect();

    // Verify every Mac first, so a device is never released and then left unclaimed.
    for host in holders.iter().copied().chain([target]) {
        if let Err(problem) = check(host) {
            log(format!("{}: {problem}", host.key));
            log(format!("{name}: nothing changed."));
            return false;
        }
    }

    let address = &device.address;
    for host in &holders {
        log(format!("{name}: releasing from {}", host.key));
        host.run_over_ssh(&format!("{BLUEUTIL} --unpair {address}"));
    }

    // A released device needs a moment before it becomes discoverable again.
    thread::sleep(Duration::from_secs(2));
    log(format!("{name}: pairing on {to}"));
    let connected = target
        .run_over_ssh(&pair_script(address))
        .text
        .ends_with('1');
    if connected {
        log(format!("{name}: connected to {to}"));
    } else {
        log(format!(
            "{name}: did not connect to {to}. Click or press a key on it and try again, \
             or plug it into {to} with its cable."
        ));
    }
    connected
}

/// Shell script that pairs and connects a device, then prints 1 if it is connected.
fn pair_script(address: &str) -> String {
    format!(
        "n=0; while [ $n -lt {PAIR_ATTEMPTS} ]; do \
           {BLUEUTIL} --pair {address} >/dev/null 2>&1 && break; \
           {BLUEUTIL} --inquiry 2 >/dev/null 2>&1; n=$((n+1)); \
         done; \
         {BLUEUTIL} --connect {address} >/dev/null 2>&1; sleep 1; \
         {BLUEUTIL} --is-connected {address}"
    )
}
