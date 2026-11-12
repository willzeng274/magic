use std::{env, fs, path::PathBuf, process::Command};

const DEFAULT_HOSTS: &[(&str, &str)] = &[
    ("m3", "user@usersM3-Pro.local"),
    ("m5", "whopuser@usersM5-Pro.local"),
];

const SSH_OPTIONS: &[&str] = &[
    "-4",
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=5",
    "-o",
    "StrictHostKeyChecking=accept-new",
];

/// One of the Macs that Magic devices move between.
#[derive(Clone)]
pub struct Host {
    /// Short name used on the command line, such as `m5`.
    pub key: String,
    /// SSH destination, `user@host`.
    pub target: String,
    /// Whether this is the Mac the program is running on.
    pub local: bool,
}

pub struct Output {
    pub ok: bool,
    pub text: String,
}

impl Host {
    /// Runs a shell command on this Mac, directly when it is the local one.
    pub fn run(&self, command: &str) -> Output {
        if self.local {
            capture(Command::new("/bin/sh").args(["-c", command]))
        } else {
            self.run_over_ssh(command)
        }
    }

    /// Runs a shell command on this Mac through SSH, even when it is the local one.
    pub fn run_over_ssh(&self, command: &str) -> Output {
        capture(
            Command::new("ssh")
                .args(SSH_OPTIONS)
                .args([&self.target, command]),
        )
    }
}

fn capture(command: &mut Command) -> Output {
    match command.output() {
        Ok(out) => {
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            Output {
                ok: out.status.success(),
                text: text.trim().to_string(),
            }
        }
        Err(err) => Output {
            ok: false,
            text: err.to_string(),
        },
    }
}

pub fn home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_default()
}

/// Loads the Macs from `~/.config/magic/hosts` (one `name user@host` per line),
/// falling back to the built-in pair.
pub fn load() -> Vec<Host> {
    let configured = fs::read_to_string(home_dir().join(".config/magic/hosts")).unwrap_or_default();
    let mut pairs: Vec<(String, String)> = configured
        .lines()
        .filter(|line| !line.trim_start().starts_with('#'))
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            Some((words.next()?.to_string(), words.next()?.to_string()))
        })
        .collect();
    if pairs.is_empty() {
        pairs = DEFAULT_HOSTS
            .iter()
            .map(|&(key, target)| (key.to_string(), target.to_string()))
            .collect();
    }

    let this_mac = local_host_name();
    pairs
        .into_iter()
        .map(|(key, target)| {
            let name = target.rsplit('@').next().unwrap_or_default();
            let local = name
                .trim_end_matches(".local")
                .eq_ignore_ascii_case(&this_mac);
            Host { key, target, local }
        })
        .collect()
}

fn local_host_name() -> String {
    Command::new("scutil")
        .args(["--get", "LocalHostName"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_default()
}
