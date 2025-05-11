use std::{fs::File, path::Path, process::Command, sync::mpsc::channel, thread};

use anyhow::{Result, bail};
use fs2::FileExt;
use log::{LevelFilter, error, info};
use serde::{Deserialize, Serialize};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::Signals,
};
use simplelog::{Config as LogConfig, WriteLogger};

enum Event {
    CheckUpdates,
    Shutdown,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Config {
    inverval_in_seconds: u64,
    warning_threshold: u64,
    critical_threshold: u64,
}

impl Config {
    fn create_default_config(config_path: &Path) -> Self {
        let config = Self::default();
        let config_contents = toml::to_string(&config).unwrap();
        match std::fs::write(config_path, config_contents) {
            Ok(_) => {
                info!("Created default config file at {:?}", config_path);
            }
            Err(e) => {
                error!(
                    "Failed to create default config file at {:?}: {}",
                    config_path, e
                );
            }
        }
        config
    }
    fn load() -> Result<Self> {
        let config_path = match dirs::config_dir() {
            Some(dir) => dir.join("hypr").join("app-indicator-rs.toml"),
            None => {
                bail!("Failed to get config directory");
            }
        };

        if !config_path.exists() {
            let config = Self::create_default_config(&config_path);
            return Ok(config);
        }

        let config_contents = match std::fs::read_to_string(config_path) {
            Ok(contents) => contents,
            Err(_) => {
                bail!("Failed to read config file");
            }
        };
        let config: Self = toml::from_str(&config_contents)?;

        Ok(config)
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            inverval_in_seconds: 1200,
            warning_threshold: 25,
            critical_threshold: 100,
        }
    }
}

#[derive(Debug, Serialize)]
struct Output {
    class: String,
    text: String,
    alt: String,
    tooltip: String,
}

impl Output {
    fn new(class: &str, text: &str, alt: &str, tooltip: &str) -> Self {
        Self {
            class: class.to_string(),
            text: text.to_string(),
            alt: alt.to_string(),
            tooltip: tooltip.to_string(),
        }
    }
}

fn main() -> Result<()> {
    setup_logging();
    verify_checkupdates_is_installed()?;

    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            bail!("Failed to get XDG_RUNTIME_DIR");
        }
    };
    let lock_path = format!("{}/app-indicator-rs.lock", runtime_dir);
    let lock_file = match File::create(&lock_path) {
        Ok(file) => file,
        Err(_) => {
            bail!("Failed to create lock file");
        }
    };

    if lock_file.try_lock_exclusive().is_err() {
        error!("Failed to acquire lock. Another instance is running.");
        bail!("Exiting");
    }

    info!("Lock acquired");

    let config = Config::load()?;

    let (tx, rx) = channel::<Event>();

    let mut signals = Signals::new([SIGINT, SIGTERM])?;

    let signal_tx = tx.clone();
    thread::spawn(move || {
        for signal in signals.forever() {
            info!("Received signal {:?}", signal);
            signal_tx.send(Event::Shutdown).unwrap();
        }
    });

    let timer_config = config.clone();
    let timer_tx = tx.clone();
    thread::spawn(move || {
        loop {
            thread::sleep(std::time::Duration::from_secs(
                timer_config.inverval_in_seconds,
            ));
            timer_tx.send(Event::CheckUpdates).unwrap();
        }
    });

    tx.send(Event::CheckUpdates).unwrap();

    loop {
        let event = match rx.recv() {
            Ok(event) => event,
            Err(_) => {
                error!("Failed to receive event");
                break;
            }
        };

        match event {
            Event::CheckUpdates => {
                let updates: u64 = match check_updates()?.trim().parse() {
                    Ok(updates) => updates,
                    Err(e) => {
                        error!("Failed to parse output of checkupdates: {}", e);
                        break;
                    }
                };
                if updates == 0 {
                    let output = Output::new("", "0", "0", "No updates available");
                    send_output(&output)?;
                    continue;
                }

                if updates > config.warning_threshold && updates <= config.critical_threshold {
                    let output = Output::new(
                        "yellow",
                        &updates.to_string(),
                        &updates.to_string(),
                        &format!("You have {} updates available", &updates.to_string()),
                    );
                    send_output(&output)?;
                    continue;
                }

                let output = Output::new(
                    "red",
                    &updates.to_string(),
                    &updates.to_string(),
                    &format!("You have {} updates available", &updates.to_string()),
                );
                send_output(&output)?;
            }
            Event::Shutdown => {
                break;
            }
        }
    }

    Ok(())
}

fn send_output(output: &Output) -> Result<()> {
    let output_contents = serde_json::to_string(output)?;
    println!("{}", output_contents);
    Ok(())
}

fn check_updates() -> Result<String> {
    match Command::new("sh")
        .arg("-c")
        .arg("checkupdates | wc -l")
        .output()
    {
        Ok(output) => {
            if !output.status.success() {
                bail!("checkupdates failed");
            };

            let stdout = match String::from_utf8(output.stdout) {
                Ok(stdout) => stdout,
                Err(e) => {
                    bail!("Failed to parse stdout of checkupdates: {}", e);
                }
            };
            Ok(stdout)
        }
        Err(e) => bail!("Failed to check for updates: {}", e),
    }
}

fn verify_checkupdates_is_installed() -> Result<()> {
    match Command::new("which").arg("checkupdates").output() {
        Ok(output) => {
            if !output.status.success() {
                bail!("checkupdates is not installed");
            };
            info!("checkupdates is installed");
            Ok(())
        }
        Err(e) => bail!("Failed to check if checkupdates is installed: {}", e),
    }
}
fn setup_logging() {
    let runtime_dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(dir) => dir,
        Err(_) => {
            println!("Failed to get XDG_RUNTIME_DIR when setting up logging");
            return;
        }
    };

    let log_path = format!("{}/app-indicator-rs.log", runtime_dir);
    let log_file = match File::create(log_path) {
        Ok(file) => file,
        Err(_) => {
            println!("Failed to create log file when setting up logging");
            return;
        }
    };
    if let Err(e) = WriteLogger::init(LevelFilter::Info, LogConfig::default(), log_file) {
        println!("Failed to initialize logging: {}", e);
    };
}
