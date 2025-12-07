# EVE Preview Manager

EVE Preview Manager — Yet another EVE-O-Preview clone for Linux, written in Rust. A reimplementation of my older [EVE-L_Preview](https://github.com/h0lylag/EVE-L_Preview). Inspired by [EVE-O-Preview](https://github.com/Proopai/eve-o-preview), [EVE-X-Preview](https://github.com/g0nzo83/EVE-X-Preview), [Nicotine](https://github.com/isomerc/nicotine), and [eve-l-preview](https://github.com/ilveth/eve-l-preview).

## Status

This is still under active development and should be working. But keep in mind it's primarily designed around my own workflow and environment.

## Features

- Live thumbnail previews of all EVE client windows
- Click thumbnails to switch windows
- Customizable thumbnail size, opacity, and positioning
- Configurable hotkeys for cycling through characters
- Drag and snap thumbnails to screen edges
- System tray integration
- Profile-based configuration

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
  eve-preview-manager.packages.${pkgs.system}.default 
];
```

### Other Distros (Ubuntu, Arch, Fedora, etc.)

Download the latest release from the [Releases](https://github.com/h0lylag/EVE-Preview-Manager/releases) page:

```bash
tar xzf eve-preview-manager-*-x86_64.tar.gz
cd eve-preview-manager-*-x86_64
./eve-preview-manager
```

### Build from Source

**Build dependencies:** Rust/Cargo, pkg-config, fontconfig, dbus, X11, libxkbcommon

```bash
git clone https://github.com/h0lylag/EVE-Preview-Manager.git
cd EVE-Preview-Manager
cargo build --release
sudo install -Dm755 target/release/eve-preview-manager /usr/local/bin/eve-preview-manager
```

## Usage

1. Launch EVE Preview Manager — it starts in GUI mode with a system tray icon
2. Open your EVE Online clients
3. Thumbnail previews will appear for each EVE window
4. Click a thumbnail to switch to that window
5. Drag thumbnails to reposition them
6. Use the system tray menu or GUI to configure settings

### Hotkeys

Configure hotkeys in the GUI to cycle through EVE windows:
- **Cycle Next**: Move to next EVE character
- **Cycle Previous**: Move to previous EVE character
