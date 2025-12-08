# EVE Preview Manager

EVE Preview Manager — Yet another EVE-O-Preview clone for Linux, written in Rust. A reimplementation of my older [EVE-L_Preview](https://github.com/h0lylag/EVE-L_Preview). Inspired by [EVE-O-Preview](https://github.com/Proopai/eve-o-preview), [EVE-X-Preview](https://github.com/g0nzo83/EVE-X-Preview), [Nicotine](https://github.com/isomerc/nicotine), and [eve-l-preview](https://github.com/ilveth/eve-l-preview).

## Status

This project is under active development and should be working. Keep in mind it's primarily designed around my own workflow and environment on NixOS. I built this for myself first and foremost. I will try to provide binaries for people to run but I don't have extensive testing infrastructure in place to test all edge cases. You can always try building it from source if you have issues.

## Features

- Real-time thumbnail previews of all EVE client windows
- Click thumbnails to activate windows or drag to reposition
- Hotkey-based character cycling with configurable key bindings
- Customizable thumbnail appearance including size, opacity, fonts, colors, and borders
- Profile-based configuration system for managing multiple setups
- One-click character import for cycle groups
- Optional features: cycle through logged-off clients, auto-minimize inactive windows, position inheritance for new characters

## Screenshots
<p align="center">
  <a href="https://i.imgur.com/rSvkvbG.png">
    <img src="https://i.imgur.com/rSvkvbG.png" alt="EVE Preview Manager in action" width="400">
  </a>
  <a href="https://i.imgur.com/tfztoAt.png">
    <img src="https://i.imgur.com/tfztoAt.png" alt="EVE Preview Manager Settings" width="400">
  </a>
</p>

## Requirements

- Linux x86_64 with X11 (Wayland users: works via XWayland)
- User must be in `input` group for hotkey detection (`sudo usermod -aG input $USER`) - Requires re-logging to take effect
- Runtime dependencies: OpenGL, fontconfig, dbus, libxkbcommon (should already be installed on most modern Linux distributions)

## Installation

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

### Other Distros (Ubuntu, Arch, Fedora, etc.)

Download the latest release from the [Releases](https://github.com/h0lylag/EVE-Preview-Manager/releases) page:

```bash
unzip eve-preview-manager-v*.zip
chmod +x ./eve-preview-manager
./eve-preview-manager
```

### Build from Source

**Build dependencies:** Rust/Cargo, pkg-config, fontconfig, dbus, X11, libxkbcommon

```bash
git clone https://github.com/h0lylag/EVE-Preview-Manager.git
cd EVE-Preview-Manager
cargo build --release
```

## Usage

1. Launch EVE Preview Manager — it starts in GUI mode with a system tray icon
2. Open your EVE Online clients
3. Thumbnail previews will appear for each EVE window
4. Click a thumbnail to switch to that window
5. Drag thumbnails to reposition them
6. Use the system tray menu or GUI to configure settings
7. Configure hotkeys and select your preferred input device in the GUI
8. Config file is stored at `~/.config/eve-preview-manager/config.json`
