# EVE Preview Manager

EVE Preview Manager - Yet another EVE-O-Preview clone for Linux, written in Rust. A reimplementation of my older [EVE-L_Preview](https://github.com/h0lylag/EVE-L_Preview). Inspired by [EVE-O-Preview](https://github.com/Proopai/eve-o-preview), [EVE-X-Preview](https://github.com/g0nzo83/EVE-X-Preview), [Nicotine](https://github.com/isomerc/nicotine), and [eve-l-preview](https://github.com/ilveth/eve-l-preview).

## Status

This project is under active development and should be working. It's primarily designed around my own workflow and environment on NixOS. While pre-built binaries are provided, if you encounter issues, building from source is always an option.

## Features

- Real-time thumbnail previews of all EVE client windows
- Per-character and cycle group hotkeys with configurable key bindings
- Customizable thumbnail appearance including size, opacity, fonts, colors, and borders
- Profile-based configuration system for managing multiple setups
- One-click character import for cycle groups
- Optional features: cycle through logged-off clients, auto-minimize inactive windows, position inheritance for new characters, disable thumbnails altogether

## Screenshots
<p align="center">
  <a href="https://i.imgur.com/ztw7B1Q.png">
    <img src="https://i.imgur.com/ztw7B1Q.png" alt="EVE Preview Manager in action" width="400">
  </a>
  <a href="https://i.imgur.com/tfztoAt.png">
    <img src="https://i.imgur.com/tfztoAt.png" alt="EVE Preview Manager Settings" width="400">
  </a>
</p>

## Usage

1. **Launch the Application**: Run `eve-preview-manager`. It starts in GUI mode and creates a system tray icon.
2. **Manage Profiles**: Use the GUI to create specific profiles for different activities (e.g., PvP, Mining). You can add, remove, or duplicate profiles to quickly switch between setups.
3. **Configure Display Settings**: Customize the look and feel of your thumbnails, including size, opacity, fonts, borders, and colors to match your preferences.
4. **Set Up Hotkeys**:
   - **Input Device**: Select your input device (auto-detect is recommended).
   - **Cycle Hotkeys**: Configure hotkeys to cycle between clients in your active group.
5. **Manage Characters**:
   - **Add Characters**: Click the "Add" button to include EVE characters in your cycle group. Active and previously detected clients will appear in the popup.
   - **Manual Entry**: Alternatively, switch to "Text Editor" mode to manually paste a list of character names (one per line).
   - **Individual Hotkeys**: Once added to the cycle group, you can bind specific hotkeys to individual characters for direct access.

**Note**: Configuration is stored in `~/.config/eve-preview-manager/config.json`.

## Requirements

- Linux x86_64 with X11 (Wayland users: works via XWayland)
- User must be in `input` group for hotkey detection: `sudo usermod -aG input $USER` (requires re-logging to take effect)
- Runtime dependencies: OpenGL, fontconfig, dbus, libxkbcommon (should already be installed on most modern Linux distributions)

## Installation

### Pre-built Binaries (Ubuntu, Arch, Fedora, etc.)

Download the latest release from the [Releases](https://github.com/h0lylag/EVE-Preview-Manager/releases) page:

```bash
unzip eve-preview-manager-v*.zip
chmod +x ./eve-preview-manager
./eve-preview-manager
```

### NixOS

Add the repo to your flake inputs:
```nix
{
  inputs.eve-preview-manager.url = "github:h0lylag/EVE-Preview-Manager";
}
```

Then add it to your packages:
```nix
environment.systemPackages = [ 
  eve-preview-manager.packages.${pkgs.stdenv.hostPlatform.system}.default
];
```

### Build from Source

**Build dependencies:** Rust/Cargo, pkg-config, fontconfig, dbus, X11, libxkbcommon

```bash
git clone https://github.com/h0lylag/EVE-Preview-Manager.git
cd EVE-Preview-Manager
cargo build --release
```
