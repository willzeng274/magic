use crate::{
    bluetooth,
    device::{self, Device, Link},
    host::{self, Host},
};
use std::{fs, process::Command};

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;

/// Runs a one-shot command and returns the process exit code.
pub fn run(args: &[String], hosts: &[Host]) -> i32 {
    match args[0].as_str() {
        "help" | "-h" | "--help" => {
            print_usage(hosts, &device::scan(hosts).devices);
            EXIT_OK
        }
        "self" => print_local_host(hosts),
        "status" => print_status(hosts),
        "check" => print_check(hosts),
        "setup" => setup(hosts),
        "here" => match hosts.iter().find(|host| host.local) {
            Some(host) => move_devices(hosts, &host.key, args.get(1)),
            None => {
                eprintln!("magic: this Mac is not in the list of Macs");
                EXIT_USAGE
            }
        },
        name if hosts.iter().any(|host| host.key == name) => move_devices(hosts, name, args.get(1)),
        unknown => {
            eprintln!("magic: unknown command '{unknown}'\n");
            print_usage(hosts, &[]);
            EXIT_USAGE
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
    println!("  check          Test SSH and Bluetooth permission on every Mac");
    println!("  setup          One-time: set up SSH between the Macs (run it on each Mac)");
    println!("  help           Show this help\n");
    println!("Devices: {}   Macs: {}", slugs.join(", "), macs.join(", "));
}

fn print_local_host(hosts: &[Host]) -> i32 {
    match hosts.iter().find(|host| host.local) {
        Some(host) => {
            println!("{}", host.key);
            EXIT_OK
        }
        None => EXIT_FAILED,
    }
}

fn print_status(hosts: &[Host]) -> i32 {
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
    EXIT_OK
}

fn print_check(hosts: &[Host]) -> i32 {
    for host in hosts {
        match bluetooth::check(host) {
            Ok(()) => println!("{}  ready", host.key),
            Err(problem) => println!("{}  NOT READY\n    {problem}", host.key),
        }
    }
    EXIT_OK
}

/// Moves the named device, or every live device when no name is given.
fn move_devices(hosts: &[Host], to: &str, only: Option<&String>) -> i32 {
    let scan = device::scan(hosts);
    let chosen: Vec<&Device> = match only {
        Some(slug) => scan
            .devices
            .iter()
            .filter(|device| device.slug() == *slug)
            .collect(),
        None => scan
            .devices
            .iter()
            .filter(|device| device.is_live())
            .collect(),
    };
    if chosen.is_empty() {
        return match only {
            Some(slug) => {
                eprintln!("magic: no device called '{slug}'. Run: magic status");
                EXIT_USAGE
            }
            None => {
                println!("no connected Magic devices found");
                EXIT_FAILED
            }
        };
    }

    let print = |line: String| println!("{line}");
    let mut all_moved = true;
    for device in chosen {
        all_moved &= bluetooth::move_device(hosts, device, to, &print);
    }
    if all_moved {
        EXIT_OK
    } else {
        EXIT_FAILED
    }
}

/// Lets this Mac log in over SSH to itself and to the other Macs with its own key.
fn setup(hosts: &[Host]) -> i32 {
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
        return EXIT_FAILED;
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
