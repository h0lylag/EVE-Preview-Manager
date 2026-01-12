# EVE Preview Manager

[Website](https://epm.sh) | [Discord](https://discord.gg/MxdW5NCjwV) | [Flathub](https://flathub.org/apps/com.evepreview.manager) | [AUR](https://aur.archlinux.org/packages/eve-preview-manager) | [FlakeHub](https://flakehub.com/flake/h0lylag/EVE-Preview-Manager)

EVE Preview Manager - Yet another EVE-O-Preview clone for Linux, written in Rust. A reimplementation of my older [EVE-L_Preview](https://github.com/h0lylag/EVE-L_Preview). Inspired by [EVE-O-Preview](https://github.com/Proopai/eve-o-preview), [EVE-X-Preview](https://github.com/g0nzo83/EVE-X-Preview), [Nicotine](https://github.com/isomerc/nicotine), and [eve-l-preview](https://github.com/ilveth/eve-l-preview).

<br>

## Features

- Real-time thumbnail previews of all EVE client windows
- Per-character and cycle group hotkeys with configurable key bindings
- Customizable thumbnail appearance including size, opacity, fonts, colors, and borders
- Profile-based configuration system for managing multiple setups
- One-click character import for cycle groups
- Optional features: cycle through logged-off clients, auto-minimize inactive windows, position inheritance for new characters, disable thumbnails altogether

<br>

## Screenshots
<p align="center">
  <a href="https://i.imgur.com/ztw7B1Q.png">
    <img src="https://i.imgur.com/ztw7B1Q.png" alt="EVE Preview Manager in action" width="400">
  </a>
  <a href="https://i.imgur.com/tfztoAt.png">
    <img src="https://i.imgur.com/tfztoAt.png" alt="EVE Preview Manager Settings" width="400">
  </a>
</p>

<br>

## Usage

1. **Launch the Application**: Run `eve-preview-manager` (or `flatpak run com.evepreview.manager`). It starts in GUI mode and creates a system tray icon.
2. **Manage Profiles**: Use the GUI to create specific profiles for different activities (e.g., PvP, Mining). You can add, remove, or duplicate profiles to quickly switch between setups.
3. **Configure Display Settings**: Customize the look and feel of your thumbnails, including size, opacity, fonts, borders, and colors to match your preferences.
4. **Set Up Hotkeys**: Configure hotkeys to cycle between clients in your active group.
5. **Manage Characters**:
   - **Add Characters**: Click the "Add" button to include EVE characters in your cycle group. Active and previously detected clients will appear in the popup.
   - **Manual Entry**: Alternatively, switch to "Text Editor" mode to manually paste a list of character names (one per line).
   - **Individual Hotkeys**: Once added to the cycle group, you can bind specific hotkeys to individual characters for direct access.
6. **Save & Apply**: Click "Save & Apply" to save your current configuration and refresh the previews.
7. **Swap Profiles**: Swapping profiles can be done quickly by right-clicking the system tray icon and selecting the desired profile.

**Note**: Configuration is stored in `~/.config/eve-preview-manager/config.json`.

<br>

## System Requirements
- **Required:** OpenGL, fontconfig, dbus, libxkbcommon, libxcb (standard on most distros).
- **Recommended:** Wayland (via XWayland). Native X11 environments are supported but users may experience issues with preview overlays fighting for Z-order and incorrect image offsets.
- **Optional:** If using evdev instead of x11 hotkeys, you will need to add your user to the `input` group. Not recommended unless you know what you're doing.

<br>

## Installation

### Flatpak (Recommended)

Install from [Flathub](https://flathub.org/apps/com.evepreview.manager):

```bash
flatpak install flathub com.evepreview.manager
```

### Arch Linux (AUR)

Install from the [AUR](https://aur.archlinux.org/packages/eve-preview-manager) using your preferred AUR helper (e.g., `yay`, `paru`, `pikaur`, etc):

```bash
yay -S eve-preview-manager
```

### NixOS

#### 1. Add Flake Input

Add the input to your `flake.nix`. We use FlakeHub for versioned releases.

```nix
inputs = {
  eve-preview-manager.url = "https://flakehub.com/f/h0lylag/EVE-Preview-Manager/*";
};
```

#### 2. Add Package

Add the package to your system packages.

```nix
{
  environment.systemPackages = [
    eve-preview-manager.packages.${pkgs.stdenv.hostPlatform.system}.default
  ];
}
```

### Manual Installation

Download the latest release from the [Releases](https://github.com/h0lylag/EVE-Preview-Manager/releases) page. This archive contains a standalone binary that works on most major Linux distributions (Ubuntu, Fedora, etc.).

```bash
unzip eve-preview-manager-v*.zip
chmod +x ./eve-preview-manager
./eve-preview-manager
```

### Build from Source

**Build dependencies:** Rust/Cargo, pkg-config, fontconfig, dbus, X11, libxkbcommon

```bash
git clone https://github.com/h0lylag/EVE-Preview-Manager.git
cargo build --release
```

<br>

## Contributing

Contributions are welcome! If you find a bug or have a feature request, please open an issue. Pull requests are also appreciated.

<br>

## License

Distributed under the MIT License. See `LICENSE` for more information.

