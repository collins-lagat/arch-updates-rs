# arch-updates-rs

A simple Rust program that checks for Arch Linux updates using **checkupdates** and displays them in **Waybar**.

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
