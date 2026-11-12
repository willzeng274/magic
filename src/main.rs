//! magic: see which Mac each Magic keyboard, mouse and trackpad is on, and move them.
//!
//! A Magic device remembers one Mac at a time. Moving it means unpairing it from the Mac that
//! has it and pairing it to the other. Reading where things are needs no permission
//! (system_profiler). Moving uses blueutil, which macOS only allows once Bluetooth access is
//! granted: to the terminal app for this Mac, and to SSH for the other Mac.

use ratatui::{
    crossterm::event::{self, Event, KeyCode, KeyEventKind},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Paragraph, Row, Table, TableState},
    Frame,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Command,
    sync::mpsc::{self, Sender},
    thread,
    time::{Duration, Instant},
};

const BLUEUTIL: &str = "/opt/homebrew/bin/blueutil";
const DEFAULT_HOSTS: &[(&str, &str)] = &[("m3", "user@usersM3-Pro.local"), ("m5", "whopuser@usersM5-Pro.local")];

#[derive(Clone)]
struct Host {
    key: String,
    target: String, // user@host for ssh
    local: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Link {
    Connected,
    Paired,
}

#[derive(Clone)]
struct Device {
    addr: String,
    name: String,
    kind: String,
    battery: String,
    links: BTreeMap<String, Link>, // host key -> state on that Mac
}

impl Device {
    fn slug(&self) -> String {
        let s: String = self.name.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        s.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-")
    }
}

struct Scan {
    devices: Vec<Device>,
    reachable: BTreeMap<String, bool>,
}

enum Msg {
    Scan(Scan),
    Log(String),
    Done,
}

// ---------------------------------------------------------------- hosts and commands

fn local_name() -> String {
    Command::new("scutil").args(["--get", "LocalHostName"]).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_lowercase()).unwrap_or_default()
}

/// The Macs to manage: ~/.config/magic/hosts ("name user@host" per line) or the built-in pair.
fn load_hosts() -> Vec<Host> {
    let me = local_name();
    let mut pairs: Vec<(String, String)> = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let path = std::path::Path::new(&home).join(".config/magic/hosts");
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                let mut it = line.split_whitespace();
                if let (Some(k), Some(t)) = (it.next(), it.next()) {
                    if !k.starts_with('#') {
                        pairs.push((k.to_string(), t.to_string()));
                    }
                }
            }
        }
    }
    if pairs.is_empty() {
        pairs = DEFAULT_HOSTS.iter().map(|(k, t)| (k.to_string(), t.to_string())).collect();
    }
    pairs
        .into_iter()
        .map(|(key, target)| {
            let host = target.rsplit('@').next().unwrap_or("").trim_end_matches(".local").to_lowercase();
            Host { local: host == me, key, target }
        })
        .collect()
}

/// Run a shell command on a Mac. Returns (succeeded, combined output).
fn run_on(host: &Host, cmd: &str) -> (bool, String) {
    let out = if host.local {
        Command::new("/bin/sh").args(["-c", cmd]).output()
    } else {
        Command::new("ssh")
            .args(["-4", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5", "-o", "StrictHostKeyChecking=accept-new", &host.target, cmd])
            .output()
    };
    match out {
        Ok(o) => {
            let mut text = String::from_utf8_lossy(&o.stdout).to_string();
            text.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), text.trim().to_string())
        }
        Err(e) => (false, e.to_string()),
    }
}

// ---------------------------------------------------------------- reading devices

fn is_magic(name: &str, minor: &str, vendor: &str) -> Option<String> {
    let n = name.to_lowercase();
    let m = minor.to_lowercase();
    let kind = if n.contains("trackpad") || m.contains("trackpad") {
        "trackpad"
    } else if m.contains("keyboard") {
        "keyboard"
    } else if m.contains("mouse") {
        "mouse"
    } else {
        return None;
    };
    // Apple's Bluetooth vendor id, or anything that calls itself Magic.
    if vendor.eq_ignore_ascii_case("0x004C") || n.contains("magic") {
        Some(kind.to_string())
    } else {
        None
    }
}

fn scan(hosts: &[Host]) -> Scan {
    let handles: Vec<_> = hosts
        .iter()
        .cloned()
        .map(|h| thread::spawn(move || (h.key.clone(), run_on(&h, "system_profiler SPBluetoothDataType -json"))))
        .collect();
    let mut devices: BTreeMap<String, Device> = BTreeMap::new();
    let mut reachable = BTreeMap::new();
    for handle in handles {
        let Ok((key, (ok, text))) = handle.join() else { continue };
        let json: Option<Value> = if ok { serde_json::from_str(&text).ok() } else { None };
        reachable.insert(key.clone(), json.is_some());
        let Some(json) = json else { continue };
        let root = &json["SPBluetoothDataType"][0];
        for (section, link) in [("device_connected", Link::Connected), ("device_not_connected", Link::Paired)] {
            for entry in root[section].as_array().into_iter().flatten() {
                for (name, info) in entry.as_object().into_iter().flatten() {
                    let get = |k: &str| info[k].as_str().unwrap_or("").to_string();
                    let Some(kind) = is_magic(name, &get("device_minorType"), &get("device_vendorID")) else { continue };
                    let addr = get("device_address").to_uppercase();
                    if addr.is_empty() {
                        continue;
                    }
                    let d = devices.entry(addr.clone()).or_insert_with(|| Device { addr, name: name.clone(), kind, battery: String::new(), links: BTreeMap::new() });
                    if link == Link::Connected {
                        d.battery = get("device_batteryLevelMain");
                    }
                    d.links.insert(key.clone(), link);
                }
            }
        }
    }
    let mut list: Vec<Device> = devices.into_values().collect();
    list.sort_by(|a, b| (a.kind.as_str(), a.name.as_str()).cmp(&(b.kind.as_str(), b.name.as_str())));
    Scan { devices: list, reachable }
}

// ---------------------------------------------------------------- moving devices

/// Can this Mac drive Bluetooth right now? Err carries what to do about it.
fn preflight(host: &Host) -> Result<(), String> {
    let (_, out) = run_on(host, &format!("{BLUEUTIL} --power"));
    if out.trim() == "1" {
        return Ok(());
    }
    let k = &host.key;
    Err(if out.contains("No such file") || out.contains("not found") {
        format!("{k}: blueutil is missing. Run: brew install blueutil")
    } else if host.local {
        format!("{k}: this terminal is not allowed to use Bluetooth. System Settings > Privacy & Security > Bluetooth, switch on your terminal app, then try again.")
    } else if out.contains("Permission denied") || out.contains("Could not resolve") || out.contains("timed out") || out.contains("refused") {
        format!("{k}: cannot log in over SSH ({}).", out.lines().last().unwrap_or(""))
    } else {
        format!("{k}: SSH is not allowed to use Bluetooth there. On {k}: System Settings > Privacy & Security > Bluetooth > +, press Cmd+Shift+G, add /usr/libexec/sshd-keygen-wrapper.")
    })
}

fn move_device(hosts: &[Host], dev: &Device, to: &str, log: &dyn Fn(String)) -> bool {
    let Some(target) = hosts.iter().find(|h| h.key == to) else {
        log(format!("unknown Mac '{to}'"));
        return false;
    };
    if dev.links.get(to) == Some(&Link::Connected) {
        log(format!("{}: already on {to}", dev.name));
        return true;
    }
    let holders: Vec<&Host> = hosts.iter().filter(|h| h.key != to && dev.links.contains_key(&h.key)).collect();
    // Check every Mac involved before touching anything, so a device is never left stranded.
    for h in holders.iter().copied().chain(std::iter::once(target)) {
        if let Err(why) = preflight(h) {
            log(why);
            log(format!("{}: nothing changed.", dev.name));
            return false;
        }
    }
    for h in &holders {
        log(format!("{}: releasing from {}", dev.name, h.key));
        run_on(h, &format!("{BLUEUTIL} --unpair {}", dev.addr));
    }
    thread::sleep(Duration::from_secs(2));
    log(format!("{}: pairing on {to}", dev.name));
    let a = &dev.addr;
    let script = format!(
        "for i in 1 2 3 4 5 6; do {BLUEUTIL} --pair {a} >/dev/null 2>&1 && break; {BLUEUTIL} --inquiry 2 >/dev/null 2>&1; done; {BLUEUTIL} --connect {a} >/dev/null 2>&1; sleep 1; {BLUEUTIL} --is-connected {a}"
    );
    let (_, out) = run_on(target, &script);
    if out.trim().ends_with('1') {
        log(format!("{}: connected to {to}", dev.name));
        true
    } else {
        log(format!("{}: did not connect to {to}. Click or press a key on it and try again, or plug it into {to} with its cable.", dev.name));
        false
    }
}

// ---------------------------------------------------------------- plain commands

fn usage(hosts: &[Host], devices: &[Device]) {
    let keys: Vec<&str> = hosts.iter().map(|h| h.key.as_str()).collect();
    println!("magic - move your Magic keyboards, mice and trackpads between Macs\n");
    println!("Usage: magic            open the interactive view");
    println!("       magic <command>\n");
    println!("  {:<14} Move every Magic device to that Mac", keys.join(" | "));
    println!("  {:<14} Move every Magic device to this Mac", "here");
    println!("  {:<14} Show which Mac each device is on", "status");
    println!("  {:<14} Test SSH and Bluetooth permission on every Mac", "check");
    println!("  {:<14} Show this help\n", "help");
    let slugs: Vec<String> = devices.iter().map(|d| d.slug()).collect();
    println!("Devices: {}   Macs: {}", slugs.join(", "), keys.join(", "));
}

fn cli(args: &[String], hosts: &[Host]) -> i32 {
    let cmd = args[0].as_str();
    match cmd {
        "self" => match hosts.iter().find(|h| h.local) {
            Some(h) => {
                println!("{}", h.key);
                0
            }
            None => 1,
        },
        "check" => {
            for h in hosts {
                match preflight(h) {
                    Ok(()) => println!("{}  ready", h.key),
                    Err(why) => println!("{}  NOT READY\n    {}", h.key, why.splitn(2, ": ").nth(1).unwrap_or(&why)),
                }
            }
            0
        }
        "status" | "help" | "-h" | "--help" => {
            let s = scan(hosts);
            if cmd != "status" {
                usage(hosts, &s.devices);
                return 0;
            }
            for h in hosts {
                if !s.reachable.get(&h.key).copied().unwrap_or(false) {
                    println!("{}: unreachable or no Bluetooth access", h.key);
                    continue;
                }
                for d in &s.devices {
                    let state = match d.links.get(&h.key) {
                        Some(Link::Connected) => "connected",
                        Some(Link::Paired) => "paired, not connected",
                        None => "not paired",
                    };
                    println!("{}  {:<28} {}", h.key, d.slug(), state);
                }
            }
            0
        }
        _ => {
            let to = if cmd == "here" { hosts.iter().find(|h| h.local).map(|h| h.key.clone()) } else { hosts.iter().find(|h| h.key == cmd).map(|h| h.key.clone()) };
            let Some(to) = to else {
                eprintln!("magic: unknown command '{cmd}'\n");
                usage(hosts, &[]);
                return 2;
            };
            let s = scan(hosts);
            // Only devices that are live somewhere: paired-but-asleep ones cannot be moved.
            let live: Vec<&Device> = s.devices.iter().filter(|d| d.links.values().any(|l| *l == Link::Connected)).collect();
            if live.is_empty() {
                println!("no connected Magic devices found");
                return 1;
            }
            let mut ok = true;
            for d in live {
                ok &= move_device(hosts, d, &to, &|line| println!("{line}"));
            }
            if ok { 0 } else { 1 }
        }
    }
}

// ---------------------------------------------------------------- interactive view

struct App {
    hosts: Vec<Host>,
    devices: Vec<Device>,
    reachable: BTreeMap<String, bool>,
    table: TableState,
    marked: BTreeSet<String>,
    log: Vec<String>,
    busy: bool,
    scanning: bool,
    scanned_once: bool,
    last_scan: Instant,
}

fn start_scan(app: &mut App, tx: &Sender<Msg>) {
    if app.scanning {
        return;
    }
    app.scanning = true;
    let (hosts, tx) = (app.hosts.clone(), tx.clone());
    thread::spawn(move || {
        let _ = tx.send(Msg::Scan(scan(&hosts)));
    });
}

fn start_move(app: &mut App, tx: &Sender<Msg>, to: String) {
    let picked: Vec<Device> = if app.marked.is_empty() {
        app.table.selected().and_then(|i| app.devices.get(i)).cloned().into_iter().collect()
    } else {
        app.devices.iter().filter(|d| app.marked.contains(&d.addr)).cloned().collect()
    };
    if picked.is_empty() {
        return;
    }
    app.busy = true;
    app.marked.clear();
    let (hosts, tx) = (app.hosts.clone(), tx.clone());
    thread::spawn(move || {
        for d in &picked {
            let t = tx.clone();
            move_device(&hosts, d, &to, &move |line| {
                let _ = t.send(Msg::Log(line));
            });
        }
        let _ = tx.send(Msg::Done);
    });
}

fn draw(f: &mut Frame, app: &mut App) {
    let dim = Style::default().fg(Color::DarkGray);
    let log_rows = 6u16;
    let [top, body, logs, help] = Layout::vertical([Constraint::Length(2), Constraint::Min(3), Constraint::Length(log_rows + 1), Constraint::Length(1)]).areas(f.area());

    // header: the Macs, numbered, with reachability
    let mut spans = vec![Span::styled(" magic ", Style::default().add_modifier(Modifier::BOLD)), Span::raw("  ")];
    for (i, h) in app.hosts.iter().enumerate() {
        let up = app.reachable.get(&h.key).copied();
        let (dot, color) = match up {
            Some(true) => ("●", Color::Green),
            Some(false) => ("●", Color::Red),
            None => ("○", Color::DarkGray),
        };
        spans.push(Span::styled(format!("[{}] {}", i + 1, h.key), Style::default().add_modifier(Modifier::BOLD)));
        spans.push(Span::styled(if h.local { " this Mac " } else { " " }, dim));
        spans.push(Span::styled(dot, Style::default().fg(color)));
        spans.push(Span::styled(if up == Some(false) { " unreachable    " } else { "    " }, dim));
    }
    if app.busy {
        spans.push(Span::styled("moving…", Style::default().fg(Color::Yellow)));
    } else if app.scanning && !app.scanned_once {
        spans.push(Span::styled("looking for devices…", dim));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), top);

    // device table: one column per Mac
    let mut header = vec![Cell::from(""), Cell::from("DEVICE"), Cell::from("TYPE"), Cell::from("BATTERY")];
    let mut widths = vec![Constraint::Length(3), Constraint::Min(22), Constraint::Length(9), Constraint::Length(8)];
    for h in &app.hosts {
        header.push(Cell::from(h.key.to_uppercase()));
        widths.push(Constraint::Length(13));
    }
    let rows: Vec<Row> = app
        .devices
        .iter()
        .map(|d| {
            let mut cells = vec![
                Cell::from(if app.marked.contains(&d.addr) { "[x]" } else { "[ ]" }),
                Cell::from(d.name.clone()),
                Cell::from(d.kind.clone()),
                Cell::from(if d.battery.is_empty() { "-".to_string() } else { d.battery.clone() }),
            ];
            for h in &app.hosts {
                cells.push(match d.links.get(&h.key) {
                    Some(Link::Connected) => Cell::from("connected").style(Style::default().fg(Color::Green)),
                    Some(Link::Paired) => Cell::from("paired, off").style(dim),
                    None => Cell::from("-").style(dim),
                });
            }
            Row::new(cells)
        })
        .collect();
    if rows.is_empty() {
        let text = if app.scanned_once { "No Magic keyboards, mice or trackpads are paired with any of these Macs." } else { "" };
        f.render_widget(Paragraph::new(text).style(dim), body);
    } else {
        let table = Table::new(rows, widths).header(Row::new(header).style(dim)).row_highlight_style(Style::default().add_modifier(Modifier::REVERSED));
        f.render_stateful_widget(table, body, &mut app.table);
    }

    // last few log lines
    let start = app.log.len().saturating_sub(log_rows as usize);
    let lines: Vec<Line> = app.log[start..].iter().map(|l| Line::from(l.as_str())).collect();
    f.render_widget(Paragraph::new(lines).style(dim).wrap(ratatui::widgets::Wrap { trim: true }), logs);

    let nums = if app.hosts.len() > 1 { format!("1-{}", app.hosts.len()) } else { "1".into() };
    let keys = format!(" ↑↓ pick   space mark   a mark all   {nums} send to that Mac   enter bring to this Mac   r refresh   q quit");
    f.render_widget(Paragraph::new(keys).style(Style::default().add_modifier(Modifier::BOLD)), help);
}

fn tui(hosts: Vec<Host>) -> std::io::Result<()> {
    let (tx, rx) = mpsc::channel::<Msg>();
    let mut app = App {
        hosts,
        devices: Vec::new(),
        reachable: BTreeMap::new(),
        table: TableState::default(),
        marked: BTreeSet::new(),
        log: Vec::new(),
        busy: false,
        scanning: false,
        scanned_once: false,
        last_scan: Instant::now(),
    };
    start_scan(&mut app, &tx);
    let mut terminal = ratatui::init();
    let result = loop {
        while let Ok(msg) = rx.try_recv() {
            match msg {
                Msg::Scan(s) => {
                    app.scanning = false;
                    app.scanned_once = true;
                    app.last_scan = Instant::now();
                    app.devices = s.devices;
                    app.reachable = s.reachable;
                    let n = app.devices.len();
                    match app.table.selected() {
                        _ if n == 0 => app.table.select(None),
                        None => app.table.select(Some(0)),
                        Some(i) if i >= n => app.table.select(Some(n - 1)),
                        _ => {}
                    }
                }
                Msg::Log(line) => app.log.push(line),
                Msg::Done => {
                    app.busy = false;
                    start_scan(&mut app, &tx);
                }
            }
        }
        if !app.busy && app.last_scan.elapsed() > Duration::from_secs(6) {
            start_scan(&mut app, &tx);
        }
        if let Err(e) = terminal.draw(|f| draw(f, &mut app)) {
            break Err(e);
        }
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        let Event::Key(key) = event::read()? else { continue };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let n = app.devices.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => break Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(event::KeyModifiers::CONTROL) => break Ok(()),
            _ if app.busy => {}
            KeyCode::Down | KeyCode::Char('j') if n > 0 => app.table.select(Some(app.table.selected().map_or(0, |i| (i + 1) % n))),
            KeyCode::Up | KeyCode::Char('k') if n > 0 => app.table.select(Some(app.table.selected().map_or(0, |i| (i + n - 1) % n))),
            KeyCode::Char(' ') => {
                if let Some(d) = app.table.selected().and_then(|i| app.devices.get(i)) {
                    if !app.marked.remove(&d.addr) {
                        app.marked.insert(d.addr.clone());
                    }
                }
            }
            KeyCode::Char('a') => {
                if app.marked.len() == n {
                    app.marked.clear();
                } else {
                    app.marked = app.devices.iter().map(|d| d.addr.clone()).collect();
                }
            }
            KeyCode::Char('r') => start_scan(&mut app, &tx),
            KeyCode::Enter => {
                if let Some(h) = app.hosts.iter().find(|h| h.local) {
                    let to = h.key.clone();
                    start_move(&mut app, &tx, to);
                }
            }
            KeyCode::Char(c) if c.is_ascii_digit() => {
                let i = c as usize - '1' as usize;
                if let Some(h) = app.hosts.get(i) {
                    let to = h.key.clone();
                    start_move(&mut app, &tx, to);
                }
            }
            _ => {}
        }
    };
    ratatui::restore();
    result
}

fn main() {
    let hosts = load_hosts();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        std::process::exit(cli(&args, &hosts));
    }
    if let Err(e) = tui(hosts) {
        eprintln!("magic: {e}");
        std::process::exit(1);
    }
}
