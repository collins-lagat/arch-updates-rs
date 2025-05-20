use std::{
    fs::File,
    io::{BufRead, BufReader},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc::{Sender, channel},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use log::{LevelFilter, error, info};
use notify::{
    self, Event as NotifyEvent, EventKind, Result as NotifyResult, Watcher,
    event::{AccessKind, AccessMode, CreateKind},
};
use serde::{Deserialize, Serialize};
use signal_hook::{
    consts::{SIGINT, SIGTERM},
    iterator::Signals,
};
use simplelog::{
    ColorChoice, CombinedLogger, Config as LogConfig, TermLogger, TerminalMode, WriteLogger,
};
use tray_icon::Icon;

const PACMAN_DIR: &str = "/var/lib/pacman/local";

const CHECKING_ICON_BYTES: &[u8] = include_bytes!("../assets/checking.png");
const NO_UPDATES_ICON_BYTES: &[u8] = include_bytes!("../assets/no-updates.png");
const UPDATES_ICON_BYTES: &[u8] = include_bytes!("../assets/updates.png");
const UPDATES_WARNING_LEVEL_ICON_BYTES: &[u8] = include_bytes!("../assets/updates-warn.png");
const UPDATES_CRITICAL_LEVEL_ICON_BYTES: &[u8] = include_bytes!("../assets/updates-critical.png");
const UPDATING_ICON_BYTES: &[u8] = include_bytes!("../assets/updating.png");

enum Event {
    Updates(Vec<String>),
    Checking,
    Updating,
    Shutdown,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
struct Config {
    inverval_in_seconds: u32,
    warning_threshold: u32,
    critical_threshold: u32,
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
            Some(dir) => dir.join("hypr").join("arch-updates-rs.toml"),
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

struct Debouncer {
    last_trigger_time: Instant,
    debounce_duration: Duration,
}
impl Debouncer {
    fn new(debounce_duration: Duration) -> Self {
        Debouncer {
            last_trigger_time: Instant::now(),
            debounce_duration,
        }
    }
    fn debounce(&mut self) -> bool {
        let current_time = Instant::now();
        if current_time.duration_since(self.last_trigger_time) >= self.debounce_duration {
            self.last_trigger_time = current_time;
            return true;
        }
        false
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
    let lock_path = format!("{}/arch-updates-rs.lock", runtime_dir);
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

    let tray_icon_config = config.clone();
    let _tx = tx.clone();
    let tray_icon_tx = setup_tray_icon(tray_icon_config, _tx);

    let timer_config = config.clone();
    let timer_tx = tx.clone();
    thread::spawn(move || {
        loop {
            info!("Next check in {} seconds", timer_config.inverval_in_seconds);
            thread::sleep(std::time::Duration::from_secs(
                timer_config.inverval_in_seconds as u64,
            ));
            timer_tx.send(Event::Checking).unwrap();
        }
    });

    let watcher_gtk_tx = tray_icon_tx.clone();
    thread::spawn(move || {
        let (tx, rx) = channel::<NotifyResult<NotifyEvent>>();
        let mut watcher = match notify::recommended_watcher(tx) {
            Ok(watcher) => watcher,
            Err(e) => {
                error!("Failed to create watcher: {}", e);
                return;
            }
        };

        if let Err(e) = watcher.watch(Path::new(PACMAN_DIR), notify::RecursiveMode::Recursive) {
            error!("Failed to watch directory: {}", e);
            return;
        }

        info!("Watching for updates in {:?}", PACMAN_DIR);

        let mut debouncer = Debouncer::new(Duration::from_millis(1000));

        for res in rx {
            match res {
                Ok(event) => match event.kind {
                    EventKind::Create(CreateKind::File)
                    | EventKind::Create(CreateKind::Folder)
                    | EventKind::Access(AccessKind::Close(AccessMode::Write)) => {
                        info!("event: {:?}", event);
                        if debouncer.debounce() {
                            watcher_gtk_tx.send(Event::Updating).unwrap();
                        }
                    }
                    _ => {}
                },
                Err(e) => {
                    error!("watch error: {}", e);
                }
            };
        }
    });

    tx.send(Event::Checking).unwrap();

    loop {
        let event = match rx.recv() {
            Ok(event) => event,
            Err(_) => {
                error!("Failed to receive event");
                break;
            }
        };

        match event {
            Event::Checking => {
                tray_icon_tx.send(Event::Checking).unwrap();

                let list_of_updates = match check_updates() {
                    Ok(list_of_updates) => list_of_updates,
                    Err(e) => {
                        error!("Failed to check for updates: {}", e);
                        break;
                    }
                };

                let num_of_updates = list_of_updates.len();

                info!("{} Updates available!", num_of_updates);

                tray_icon_tx.send(Event::Updates(list_of_updates)).unwrap();
            }
            Event::Updates(_) => {}
            Event::Updating => {
                thread::sleep(Duration::from_secs(5));
                tx.send(Event::Checking).unwrap();
            }
            Event::Shutdown => {
                break;
            }
        }
    }

    Ok(())
}

fn check_updates() -> Result<Vec<String>> {
    let mut child = match Command::new("checkupdates").stdout(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(e) => bail!("Failed to check for updates: {}", e),
    };

    let mut updates = Vec::<String>::new();

    if let Some(stdout) = child.stdout.take() {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            let line = match line {
                Ok(line) => line,
                Err(e) => {
                    bail!("Failed to read line from checkupdates: {}", e);
                }
            };

            let content = String::from_utf8(line.into_bytes())?;
            updates.push(content);
        }
    }

    child.wait()?;

    Ok(updates)
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

    let log_path = format!("{}/arch-updates-rs.log", runtime_dir);
    let log_file = match File::create(log_path) {
        Ok(file) => file,
        Err(_) => {
            println!("Failed to create log file when setting up logging");
            return;
        }
    };

    if let Err(e) = CombinedLogger::init(vec![
        TermLogger::new(
            LevelFilter::Info,
            LogConfig::default(),
            TerminalMode::Mixed,
            ColorChoice::Auto,
        ),
        WriteLogger::new(LevelFilter::Info, LogConfig::default(), log_file),
    ]) {
        println!("Failed to initialize logging: {}", e);
    };
}

fn convert_bytes_to_icon(bytes: &[u8]) -> Result<Icon> {
    let image_buff = match image::load_from_memory(bytes) {
        Ok(image_dyn) => image_dyn.into_rgba8(),
        Err(e) => return Err(e).context("Failed to load icon"),
    };

    let (width, height) = image_buff.dimensions();
    let icon_rgba = image_buff.into_raw();

    let icon = match Icon::from_rgba(icon_rgba, width, height) {
        Ok(icon) => icon,
        Err(e) => return Err(e).context("Failed to create icon"),
    };

    Ok(icon)
}

fn setup_tray_icon(config: Config, app_tx: Sender<Event>) -> Sender<Event> {
    let (tx, rx) = channel::<Event>();

    std::thread::spawn(move || {
        use tray_icon::{
            TrayIconBuilder,
            menu::{Menu, MenuItem, Submenu},
        };

        gtk::init().unwrap();

        let icon = match convert_bytes_to_icon(NO_UPDATES_ICON_BYTES) {
            Ok(icon) => icon,
            Err(e) => {
                error!("Failed to convert bytes to icon: {}", e);
                return;
            }
        };

        let menu = Menu::new();

        let list_of_updates_submenu = Submenu::new("0 pending updates", true);

        if let Err(e) = menu.append_items(&[&list_of_updates_submenu]) {
            error!("Failed to append menu item: {}", e);
            return;
        }

        let tray_icon = match TrayIconBuilder::new().with_menu(Box::new(menu)).build() {
            Ok(tray_icon) => tray_icon,
            Err(e) => {
                error!("Failed to build tray icon: {}", e);
                return;
            }
        };

        if let Err(e) = tray_icon.set_icon(Some(icon)) {
            error!("Failed to set icon: {}", e);
            return;
        };

        glib::timeout_add_local(Duration::from_millis(100), move || {
            while let Ok(event) = rx.try_recv() {
                match event {
                    Event::Checking => {
                        let checking_icon = match convert_bytes_to_icon(CHECKING_ICON_BYTES) {
                            Ok(icon) => icon,
                            Err(e) => {
                                error!("Failed to convert bytes to icon: {}", e);
                                return glib::ControlFlow::Break;
                            }
                        };
                        if let Err(e) = tray_icon.set_icon(Some(checking_icon)) {
                            error!("Failed to set icon: {}", e);
                            return glib::ControlFlow::Break;
                        };
                    }
                    Event::Updates(list_of_updates) => {
                        let updates_icon;
                        let num_of_updates = list_of_updates.len() as u32;
                        if num_of_updates == 0 {
                            updates_icon = match convert_bytes_to_icon(NO_UPDATES_ICON_BYTES) {
                                Ok(icon) => icon,
                                Err(e) => {
                                    error!("Failed to convert bytes to icon: {}", e);
                                    return glib::ControlFlow::Break;
                                }
                            };
                        } else if num_of_updates < config.warning_threshold {
                            updates_icon = match convert_bytes_to_icon(UPDATES_ICON_BYTES) {
                                Ok(icon) => icon,
                                Err(e) => {
                                    error!("Failed to convert bytes to icon: {}", e);
                                    return glib::ControlFlow::Break;
                                }
                            };
                        } else if num_of_updates < config.critical_threshold {
                            updates_icon =
                                match convert_bytes_to_icon(UPDATES_WARNING_LEVEL_ICON_BYTES) {
                                    Ok(icon) => icon,
                                    Err(e) => {
                                        error!("Failed to convert bytes to icon: {}", e);
                                        return glib::ControlFlow::Break;
                                    }
                                };
                        } else {
                            updates_icon =
                                match convert_bytes_to_icon(UPDATES_CRITICAL_LEVEL_ICON_BYTES) {
                                    Ok(icon) => icon,
                                    Err(e) => {
                                        error!("Failed to convert bytes to icon: {}", e);
                                        return glib::ControlFlow::Break;
                                    }
                                };
                        }

                        if let Err(e) = tray_icon.set_icon(Some(updates_icon)) {
                            error!("Failed to set icon: {}", e);
                            return glib::ControlFlow::Break;
                        };

                        for item in list_of_updates_submenu.items() {
                            if let Some(_item) = item.as_menuitem() {
                                if let Err(e) = list_of_updates_submenu.remove(_item) {
                                    error!("Failed to remove menu item: {}", e);
                                    return glib::ControlFlow::Break;
                                };
                            }
                        }

                        list_of_updates_submenu
                            .set_text(format!("{} pending updates", num_of_updates));

                        for update in list_of_updates.iter() {
                            let update_item = MenuItem::new(update, true, None);
                            if let Err(e) = list_of_updates_submenu.append_items(&[&update_item]) {
                                error!("Failed to append menu items: {}", e);
                                return glib::ControlFlow::Break;
                            }
                        }

                        info!("Updated tray icon");
                    }
                    Event::Updating => {
                        let updating_icon = match convert_bytes_to_icon(UPDATING_ICON_BYTES) {
                            Ok(icon) => icon,
                            Err(e) => {
                                error!("Failed to convert bytes to icon: {}", e);
                                return glib::ControlFlow::Break;
                            }
                        };
                        if let Err(e) = tray_icon.set_icon(Some(updating_icon)) {
                            error!("Failed to set icon: {}", e);
                            return glib::ControlFlow::Break;
                        };
                        app_tx.send(Event::Updating).unwrap();
                    }
                    Event::Shutdown => {
                        return glib::ControlFlow::Break;
                    }
                }
            }
            glib::ControlFlow::Continue
        });

        gtk::main();
    });

    tx
}
