use crate::{
    bluetooth,
    device::{self, Device, Link},
    host::{self, Host},
    known,
};
use std::{fs, process::Command, process::ExitCode};

const USAGE_ERROR: u8 = 2;

/// Runs a one-shot command. `command` is the first argument, `device` the optional second.
pub fn run(hosts: &[Host], command: &str, device: Option<&str>) -> ExitCode {
    match command {
        "help" | "-h" | "--help" => {
            print_usage(hosts, &device::scan(hosts).devices);
            ExitCode::SUCCESS
        }
        "self" => print_local_host(hosts),
        "status" => print_status(hosts),
        "check" => print_check(hosts),
        "setup" => setup(hosts),
        "rescue" => rescue(hosts, device),
        "forget" => forget(hosts, device),
        "here" => {
            if let Some(here) = hosts.iter().find(|host| host.local) {
                move_devices(hosts, &here.key, device)
            } else {
                eprintln!("magic: this Mac is not in the list of Macs");
                ExitCode::from(USAGE_ERROR)
            }
        }
        mac if hosts.iter().any(|host| host.key == mac) => move_devices(hosts, mac, device),
        unknown => {
            eprintln!("magic: unknown command '{unknown}'\n");
            print_usage(hosts, &[]);
            ExitCode::from(USAGE_ERROR)
        }
    }
}

fn print_usage(hosts: &[Host], devices: &[Device]) {
    let macs: Vec<&str> = hosts.iter().map(|host| host.key.as_str()).collect();
    let slugs: Vec<String> = devices.iter().map(Device::slug).collect();
    let example_mac = macs.last().copied().unwrap_or("m5");
    let example_device = slugs.first().map_or("black-mouse", String::as_str);
    let any_mac = macs.join(" | ");

    println!("magic - move your Magic keyboards, mice and trackpads between Macs\n");
    println!("Usage: magic            open the interactive view");
    println!("       magic <command>\n");
    println!("  {any_mac:<14} Move every Magic device to that Mac");
    println!("  <mac> <device> Move one device, for example: magic {example_mac} {example_device}");
    println!("  here           Move every Magic device to this Mac");
    println!("  status         Show which Mac each device is on");
    println!("  rescue         Get back a device that ended up on no Mac");
    println!("  forget <dev>   Drop a device you no longer own from the list");
    println!("  check          Test SSH and Bluetooth permission on every Mac");
    println!("  setup          One-time: set up SSH between the Macs (run it on each Mac)");
    println!("  help           Show this help\n");
    println!("Devices: {}   Macs: {}", slugs.join(", "), macs.join(", "));
}

fn print_local_host(hosts: &[Host]) -> ExitCode {
    let Some(here) = hosts.iter().find(|host| host.local) else {
        return ExitCode::FAILURE;
    };
    println!("{}", here.key);
    ExitCode::SUCCESS
}

fn print_status(hosts: &[Host]) -> ExitCode {
    let scan = device::scan(hosts);
    for host in hosts {
        if scan.reachable.get(&host.key) != Some(&true) {
            println!("{}: unreachable or no Bluetooth access", host.key);
            continue;
        }
        for device in &scan.devices {
            let state = match device.links.get(&host.key) {
                Some(Link::Connected) => "connected",
                Some(Link::Paired) => "paired, not connected",
                Some(Link::Usb) => "usb cable",
                None => "not paired",
            };
            println!("{}  {:<28} {state}", host.key, device.slug());
        }
    }
    ExitCode::SUCCESS
}

fn print_check(hosts: &[Host]) -> ExitCode {
    for host in hosts {
        match bluetooth::check(host) {
            Ok(()) => println!("{}  ready", host.key),
            Err(problem) => println!("{}  NOT READY\n    {problem}", host.key),
        }
    }
    ExitCode::SUCCESS
}

/// Moves the named device, or every live device when no name is given.
fn move_devices(hosts: &[Host], to: &str, only: Option<&str>) -> ExitCode {
    let scan = device::scan(hosts);
    let chosen: Vec<&Device> = match only {
        Some(slug) => scan
            .devices
            .iter()
            .filter(|device| device.slug() == slug)
            .collect(),
        None => scan
            .devices
            .iter()
            .filter(|device| device.is_live())
            .collect(),
    };
    if chosen.is_empty() {
        if let Some(slug) = only {
            eprintln!("magic: no device called '{slug}'. Run: magic status");
            return ExitCode::from(USAGE_ERROR);
        }
        println!("no connected Magic devices found");
        return ExitCode::FAILURE;
    }

    let mut all_moved = true;
    for device in chosen {
        all_moved &= bluetooth::move_device(hosts, device, to, |line| println!("{line}"));
    }
    if all_moved {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Recovers the named device, or every device that no Mac holds.
fn rescue(hosts: &[Host], only: Option<&str>) -> ExitCode {
    let scan = device::scan(hosts);
    let lost: Vec<&Device> = scan
        .devices
        .iter()
        .filter(|device| !device.is_live() && only.is_none_or(|slug| device.slug() == slug))
        .collect();
    if lost.is_empty() {
        println!("nothing to rescue: every device is on a Mac");
        return ExitCode::SUCCESS;
    }
    let mut all_back = true;
    for device in lost {
        all_back &= bluetooth::rescue(hosts, device, |line| println!("{line}")).is_some();
    }
    if all_back {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn forget(hosts: &[Host], slug: Option<&str>) -> ExitCode {
    let Some(slug) = slug else {
        eprintln!("magic: say which device, for example: magic forget black-mouse");
        return ExitCode::from(USAGE_ERROR);
    };
    let scan = device::scan(hosts);
    match scan.devices.iter().find(|device| device.slug() == slug) {
        Some(device) if device.links.is_empty() => {
            known::forget(&device.address);
            println!("forgot {}", device.name);
            ExitCode::SUCCESS
        }
        Some(device) => {
            eprintln!(
                "magic: {} is still paired with a Mac. Remove it in Bluetooth settings first.",
                device.name
            );
            ExitCode::FAILURE
        }
        None => {
            eprintln!("magic: no device called '{slug}'. Run: magic status");
            ExitCode::from(USAGE_ERROR)
        }
    }
}

/// Lets this Mac log in over SSH to itself and to the other Macs with its own key.
fn setup(hosts: &[Host]) -> ExitCode {
    let ssh_dir = host::home_dir().join(".ssh");
    let private_key = ssh_dir.join("id_ed25519");
    let public_key_path = ssh_dir.join("id_ed25519.pub");
    if !public_key_path.exists() {
        println!("creating an SSH key for this Mac");
        let _ = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&private_key)
            .status();
    }
    let Ok(public_key) = fs::read_to_string(&public_key_path) else {
        eprintln!("magic: no SSH public key at {}", public_key_path.display());
        return ExitCode::FAILURE;
    };
    let public_key = public_key.trim();

    for host in hosts {
        let mac = &host.key;
        if host.local {
            let path = ssh_dir.join("authorized_keys");
            let existing = fs::read_to_string(&path).unwrap_or_default();
            if existing.contains(public_key) {
                println!("{mac}: this Mac already accepts its own key");
                continue;
            }
            let separator = if existing.is_empty() || existing.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            match fs::write(&path, format!("{existing}{separator}{public_key}\n")) {
                Ok(()) => println!("{mac}: added this Mac's key to {}", path.display()),
                Err(err) => println!("{mac}: could not write {}: {err}", path.display()),
            }
            let _ = Command::new("chmod").arg("600").arg(&path).status();
        } else if host.run("true").ok {
            println!("{mac}: already reachable");
        } else {
            println!("{mac}: copying this Mac's key there (asks for that Mac's password once)");
            let _ = Command::new("ssh-copy-id")
                .args(["-o", "StrictHostKeyChecking=accept-new", "-i"])
                .arg(&public_key_path)
                .arg(&host.target)
                .status();
        }
    }

    println!();
    print_check(hosts)
}
