use crate::{
    bluetooth,
    device::{self, Device, Link, Scan},
    host::Host,
};
use ratatui::{
    Frame,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Cell, Paragraph, Row, Table, TableState, Wrap},
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

const RESCAN_EVERY: Duration = Duration::from_secs(6);
const INPUT_POLL: Duration = Duration::from_millis(150);
const LOG_LINES: u16 = 6;

/// Results sent back from background threads.
enum Message {
    Scanned(Scan),
    Log(String),
    MoveFinished,
}

struct App {
    hosts: Vec<Host>,
    devices: Vec<Device>,
    reachable: BTreeMap<String, bool>,
    table: TableState,
    /// Addresses of the devices marked for a group move.
    marked: BTreeSet<String>,
    log: Vec<String>,
    moving: bool,
    scanning: bool,
    scanned_once: bool,
    last_scan: Instant,
    sender: Sender<Message>,
}

impl App {
    fn new(hosts: Vec<Host>, sender: Sender<Message>) -> Self {
        Self {
            hosts,
            devices: Vec::new(),
            reachable: BTreeMap::new(),
            table: TableState::default(),
            marked: BTreeSet::new(),
            log: Vec::new(),
            moving: false,
            scanning: false,
            scanned_once: false,
            last_scan: Instant::now(),
            sender,
        }
    }

    fn start_scan(&mut self) {
        if self.scanning {
            return;
        }
        self.scanning = true;
        let hosts = self.hosts.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let _ = sender.send(Message::Scanned(device::scan(&hosts)));
        });
    }

    /// Moves the marked devices, or the highlighted one when nothing is marked.
    fn start_move(&mut self, to: String) {
        let chosen: Vec<Device> = if self.marked.is_empty() {
            self.selected().cloned().into_iter().collect()
        } else {
            self.devices
                .iter()
                .filter(|d| self.marked.contains(&d.address))
                .cloned()
                .collect()
        };
        if chosen.is_empty() {
            return;
        }
        self.moving = true;
        self.marked.clear();
        let hosts = self.hosts.clone();
        let sender = self.sender.clone();
        thread::spawn(move || {
            let log = |line: &str| {
                let _ = sender.send(Message::Log(line.to_string()));
            };
            for device in &chosen {
                bluetooth::move_device(&hosts, device, &to, log);
            }
            let _ = sender.send(Message::MoveFinished);
        });
    }

    fn selected(&self) -> Option<&Device> {
        self.table
            .selected()
            .and_then(|index| self.devices.get(index))
    }

    fn handle_message(&mut self, message: Message) {
        match message {
            Message::Scanned(scan) => {
                self.scanning = false;
                self.scanned_once = true;
                self.last_scan = Instant::now();
                self.devices = scan.devices;
                self.reachable = scan.reachable;
                let last = self.devices.len().checked_sub(1);
                let selection = last.map(|last| self.table.selected().unwrap_or(0).min(last));
                self.table.select(selection);
            }
            Message::Log(line) => self.log.push(line),
            Message::MoveFinished => {
                self.moving = false;
                self.start_scan();
            }
        }
    }

    /// Returns false when the key asks to quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let quit = matches!(key.code, KeyCode::Char('q') | KeyCode::Esc)
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL));
        if quit {
            return false;
        }
        if self.moving {
            return true;
        }
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_previous(),
            KeyCode::Char(' ') => self.toggle_mark(),
            KeyCode::Char('a') => self.toggle_mark_all(),
            KeyCode::Char('r') => self.start_scan(),
            KeyCode::Enter => {
                if let Some(here) = self.hosts.iter().find(|host| host.local) {
                    self.start_move(here.key.clone());
                }
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if let Some(host) = self.hosts.get(index) {
                    self.start_move(host.key.clone());
                }
            }
            _ => {}
        }
        true
    }

    fn select_next(&mut self) {
        let count = self.devices.len();
        if count > 0 {
            let current = self.table.selected().unwrap_or(0);
            self.table.select(Some((current + 1) % count));
        }
    }

    fn select_previous(&mut self) {
        let count = self.devices.len();
        if count > 0 {
            let current = self.table.selected().unwrap_or(0);
            self.table.select(Some((current + count - 1) % count));
        }
    }

    fn toggle_mark(&mut self) {
        let Some(address) = self.selected().map(|device| device.address.clone()) else {
            return;
        };
        if !self.marked.remove(&address) {
            self.marked.insert(address);
        }
    }

    fn toggle_mark_all(&mut self) {
        if self.marked.len() == self.devices.len() {
            self.marked.clear();
        } else {
            self.marked = self
                .devices
                .iter()
                .map(|device| device.address.clone())
                .collect();
        }
    }
}

/// Runs the interactive view until the user quits.
pub fn run(hosts: Vec<Host>) -> io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    let mut app = App::new(hosts, sender);
    app.start_scan();

    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut app, &receiver);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    app: &mut App,
    receiver: &Receiver<Message>,
) -> io::Result<()> {
    loop {
        for message in receiver.try_iter() {
            app.handle_message(message);
        }
        if !app.moving && app.last_scan.elapsed() > RESCAN_EVERY {
            app.start_scan();
        }
        terminal.draw(|frame| draw(frame, app))?;

        if !event::poll(INPUT_POLL)? {
            continue;
        }
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && !app.handle_key(key)
        {
            return Ok(());
        }
    }
}

fn draw(frame: &mut Frame, app: &mut App) {
    let [header, body, log, keys] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(LOG_LINES + 1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    frame.render_widget(Paragraph::new(header_line(app)), header);

    if app.devices.is_empty() {
        let text = if app.scanned_once {
            "No Magic keyboards, mice or trackpads are paired with any of these Macs."
        } else {
            ""
        };
        frame.render_widget(Paragraph::new(text).style(dim()), body);
    } else {
        let table = device_table(app);
        frame.render_stateful_widget(table, body, &mut app.table);
    }

    let first = app.log.len().saturating_sub(LOG_LINES as usize);
    let lines: Vec<Line> = app.log[first..]
        .iter()
        .map(|line| Line::from(line.as_str()))
        .collect();
    frame.render_widget(
        Paragraph::new(lines).style(dim()).wrap(Wrap { trim: true }),
        log,
    );

    let hint = format!(
        " ↑↓ pick   space mark   a mark all   1-{} send to that Mac   \
         enter bring to this Mac   r refresh   q quit",
        app.hosts.len()
    );
    frame.render_widget(Paragraph::new(hint).style(bold()), keys);
}

/// The title and the numbered Macs, each with a dot showing whether it answered.
fn header_line(app: &App) -> Line<'static> {
    let mut spans = vec![Span::styled(" magic   ", bold())];
    for (index, host) in app.hosts.iter().enumerate() {
        let answered = app.reachable.get(&host.key).copied();
        let (dot, color) = match answered {
            Some(true) => ("●", Color::Green),
            Some(false) => ("●", Color::Red),
            None => ("○", Color::DarkGray),
        };
        spans.push(Span::styled(
            format!("[{}] {}", index + 1, host.key),
            bold(),
        ));
        spans.push(Span::styled(
            if host.local { " this Mac " } else { " " },
            dim(),
        ));
        spans.push(Span::styled(dot, Style::default().fg(color)));
        let note = if answered == Some(false) {
            " unreachable    "
        } else {
            "    "
        };
        spans.push(Span::styled(note, dim()));
    }
    if app.moving {
        spans.push(Span::styled("moving…", Style::default().fg(Color::Yellow)));
    } else if !app.scanned_once {
        spans.push(Span::styled("looking for devices…", dim()));
    }
    Line::from(spans)
}

/// One row per device and one column per Mac.
fn device_table(app: &App) -> Table<'static> {
    let mut titles = vec![
        String::new(),
        "DEVICE".into(),
        "TYPE".into(),
        "BATTERY".into(),
    ];
    let mut widths = vec![
        Constraint::Length(3),
        Constraint::Min(22),
        Constraint::Length(9),
        Constraint::Length(8),
    ];
    for host in &app.hosts {
        titles.push(host.key.to_uppercase());
        widths.push(Constraint::Length(13));
    }

    let rows = app.devices.iter().map(|device| {
        let mark = if app.marked.contains(&device.address) {
            "[x]"
        } else {
            "[ ]"
        };
        let mut cells = vec![
            Cell::from(mark),
            Cell::from(device.name.clone()),
            Cell::from(device.kind.to_string()),
            Cell::from(device.battery.clone().unwrap_or_else(|| "-".into())),
        ];
        cells.extend(
            app.hosts
                .iter()
                .map(|host| link_cell(device.links.get(&host.key))),
        );
        Row::new(cells)
    });

    Table::new(rows, widths)
        .header(Row::new(titles).style(dim()))
        .row_highlight_style(Style::default().add_modifier(Modifier::REVERSED))
}

fn link_cell(link: Option<&Link>) -> Cell<'static> {
    match link {
        Some(Link::Connected) => Cell::from("connected").style(Style::default().fg(Color::Green)),
        Some(Link::Usb) => Cell::from("usb cable").style(Style::default().fg(Color::Yellow)),
        Some(Link::Paired) => Cell::from("paired, off").style(dim()),
        None => Cell::from("-").style(dim()),
    }
}

fn dim() -> Style {
    Style::default().fg(Color::DarkGray)
}

fn bold() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}
