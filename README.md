# arch-updates-rs

A simple Rust program that checks for Arch Linux updates using **checkupdates** and displays them in **Waybar**.

Additionally, it detects when **pacman** and changes the mode to checking. It also schedules a check for updates every 5 seconds. This is to prevent waybar from displaying that their are available updates when they are none.

## Requirements

- **pacman-contrib** - Arch Linux package that provides `checkupdates`. This is strictly required.
- **waybar** - A status bar for Wayland. Optional is you have another way to display the output of the program.

## Installation

1. Build the program:

```bash
cargo build --release
```

2. Move the executable to bin folder in your PATH:

```bash
mv target/release/arch-updates-rs ~/.local/bin
```

## Configuration

You can configure the program by editing the `~/.config/hypr/arch-updates-rs.toml` file. The default configuration is as follows:

```toml
inverval_in_seconds = 1200
warning_threshold = 25
critical_threshold = 100
```

The `inverval_in_seconds` option sets the interval in seconds between each check for updates.

## Acknowledgements

This project was inspired by [arch-update](https://github.com/RaphaelRochet/arch-update), which is a GNOME Shell extension that shows the available updates for Arch Linux. I wanted to have as similar experience as the GNOME Shell extension, but in whatever DE I wanted.
