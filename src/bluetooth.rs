//! Moving a device: the Mac that holds it unpairs it, then the target Mac pairs it at once
//! and keeps trying until the device is really connected. If that fails it goes back.
//!
//! Pairing must not start before the release: while a Mac is calling a device its own
//! outgoing SSH slows to a crawl, which delays the release past the pairing window.
//!
//! Every `blueutil` call goes over SSH, including calls to the local Mac. macOS aborts
//! `blueutil` when a terminal app runs it, even with Bluetooth access granted, but allows
//! it for SSH once `/usr/libexec/sshd-keygen-wrapper` has that access.

use crate::{
    device::{Device, Link},
    host::Host,
};
use std::{fmt, time::Instant};

const BLUEUTIL: &str = "/opt/homebrew/bin/blueutil";
/// How long a Mac keeps trying to pair a device before giving up.
const PAIR_SECONDS: u32 = 25;
const SSH_FAILURES: &[&str] = &[
    "Permission denied",
    "Could not resolve",
    "timed out",
    "refused",
];

/// Why a Mac cannot control Bluetooth right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotReady {
    BlueutilMissing,
    SshLogin,
    BluetoothPermission,
}

impl fmt::Display for NotReady {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::BlueutilMissing => "blueutil is missing. Run: brew install blueutil",
            Self::SshLogin => "cannot log in over SSH. Run: magic setup",
            Self::BluetoothPermission => {
                "SSH is not allowed to use Bluetooth there. On that Mac: System Settings > \
                 Privacy & Security > Bluetooth > +, press Cmd+Shift+G, \
                 add /usr/libexec/sshd-keygen-wrapper."
            }
        })
    }
}

/// Checks that a Mac can control Bluetooth.
pub fn check(host: &Host) -> Result<(), NotReady> {
    let output = host.run_over_ssh(&format!("{BLUEUTIL} --power")).text;
    if output == "1" {
        Ok(())
    } else if output.contains("No such file") || output.contains("not found") {
        Err(NotReady::BlueutilMissing)
    } else if SSH_FAILURES.iter().any(|failure| output.contains(failure)) {
        Err(NotReady::SshLogin)
    } else {
        Err(NotReady::BluetoothPermission)
    }
}

/// Moves one device to the Mac named `to`, reporting progress through `log`.
/// Returns whether the device ended up connected there.
pub fn move_device(hosts: &[Host], device: &Device, to: &str, log: impl Fn(&str)) -> bool {
    let name = &device.name;
    let Some(target) = hosts.iter().find(|host| host.key == to) else {
        log(&format!("unknown Mac '{to}'"));
        return false;
    };
    if device.links.get(to) == Some(&Link::Connected) {
        log(&format!("{name}: already on {to}"));
        return true;
    }
    if let Some(mac) = device.cabled_to() {
        log(&format!(
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
            log(&format!("{}: {problem}", host.key));
            log(&format!("{name}: nothing changed."));
            return false;
        }
    }

    let address = &device.address;
    let origin = holders
        .iter()
        .copied()
        .find(|host| device.links.get(&host.key) == Some(&Link::Connected));
    if let Some(origin) = origin
        && !is_connected(origin, address)
    {
        log(&format!(
            "{name}: {} no longer has it connected. Nothing changed.",
            origin.key
        ));
        return false;
    }

    let started = Instant::now();
    for host in &holders {
        log(&format!("{name}: releasing from {}", host.key));
        host.run_over_ssh(&format!("{BLUEUTIL} --unpair {address}"));
    }
    log(&format!("{name}: pairing on {to}"));
    let connected = pair(target, address);
    if connected {
        log(&format!(
            "{name}: connected to {to} in {:.1}s",
            started.elapsed().as_secs_f32()
        ));
        return true;
    }

    // The target never got it, so try to put it back where it was.
    if let Some(origin) = origin {
        log(&format!(
            "{name}: {to} could not get it, returning it to {}",
            origin.key
        ));
        if pair(origin, address) {
            log(&format!("{name}: back on {}", origin.key));
            return false;
        }
    }
    log(&format!(
        "{name}: not connected to any Mac now. Plug it into {to} with its cable \
         for a few seconds, or switch it off and on and try again."
    ));
    false
}

fn is_connected(host: &Host, address: &str) -> bool {
    host.run_over_ssh(&format!("{BLUEUTIL} --is-connected {address}"))
        .text
        .ends_with('1')
}

/// Pairs and connects a device on one Mac. Returns whether it ended up connected.
fn pair(host: &Host, address: &str) -> bool {
    host.run_over_ssh(&pair_script(address)).text.ends_with('1')
}

/// Shell script that keeps pairing until the device is really connected, then prints 1.
/// A pairing the device did not accept is removed before the next try.
fn pair_script(address: &str) -> String {
    // `blueutil --pair` can hang for minutes, so every call gets a time limit.
    format!(
        "b() {{ perl -e 'alarm shift; exec @ARGV' \"$@\" >/dev/null 2>&1; }}; \
         on() {{ [ \"$({BLUEUTIL} --is-connected {address} 2>/dev/null)\" = 1 ]; }}; \
         end=$(( $(date +%s) + {PAIR_SECONDS} )); \
         while [ $(date +%s) -lt $end ]; do \
           on && echo 1 && exit 0; \
           b 8 {BLUEUTIL} --pair {address}; \
           b 6 {BLUEUTIL} --connect {address}; \
           i=0; while [ $i -lt 4 ]; do on && echo 1 && exit 0; sleep 0.5; i=$((i+1)); done; \
           b 4 {BLUEUTIL} --unpair {address}; \
         done; echo 0"
    )
}
