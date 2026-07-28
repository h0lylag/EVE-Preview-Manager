# EVE Preview Manager

[Website](https://epm.sh) | [Discord](https://discord.gg/MxdW5NCjwV) | [Flathub](https://flathub.org/apps/com.evepreview.manager) | [AUR](https://aur.archlinux.org/packages/eve-preview-manager) | [FlakeHub](https://flakehub.com/flake/h0lylag/EVE-Preview-Manager)

EVE Preview Manager is a Linux-native tool for managing multiple EVE Online clients, with live window previews, configurable hotkeys, and quick-swap profiles built for multiboxing.

<br>

## Features

- Real-time thumbnail previews of all EVE client windows
- Per-character and cycle group hotkeys with configurable key bindings
- Customizable thumbnail appearance including size, opacity, fonts, colors, and borders
- Profile-based configuration system for managing multiple setups
- One-click character import for cycle groups
- Optional cycling through logged-off clients
- Auto-minimize inactive windows and inherit positions for new characters
- Move one preview with RMB drag or all visible previews with an LMB+RMB drag
- Option to disable thumbnails altogether

<br>

## Screenshots

<p align="center">
  <a href="https://epm.sh/assets/images/gh-main.png">
    <img src="https://epm.sh/assets/images/gh-main.png" alt="EVE Preview Manager in action" width="400">
  </a>
  <a href="https://epm.sh/assets/images/gh-previews.png">
    <img src="https://epm.sh/assets/images/gh-previews.png" alt="EVE Preview Manager Settings" width="400">
  </a>
</p>

<br>

## Usage

1. **Launch the Application**: Run `eve-preview-manager` (or `flatpak run com.evepreview.manager`). It starts in GUI mode and creates a system tray icon.
2. **Manage Profiles**: Use the GUI to create specific profiles for different activities (e.g., PvP, Mining). You can add, remove, or duplicate profiles to quickly switch between setups.
3. **Configure Display Settings**: Customize the look and feel of your thumbnails, including size, opacity, fonts, borders, and colors to match your preferences.
4. **Set Up Hotkeys**: Configure hotkeys to cycle between clients in your active group.
5. **Arrange Previews**: Drag a preview with RMB to move it individually. Hold LMB and RMB in either order on any preview, then drag to move the previews visible when the chord begins as one group. Releasing either button ends the group drag; previews that appear later are not added mid-drag.
6. **Manage Characters**:
   - **Add Characters**: Click the "Add" button to include EVE characters in your cycle group. Active and previously detected clients will appear in the popup.
   - **Manual Entry**: Alternatively, switch to "Text Editor" mode to manually paste a list of character names (one per line).
   - **Individual Hotkeys**: Once added to the cycle group, you can bind specific hotkeys to individual characters for direct access.
7. **Save & Apply**: Click "Save & Apply" to save your current configuration and refresh the previews.
8. **Swap Profiles**: Swapping profiles can be done quickly by right-clicking the system tray icon and selecting the desired profile.

**Note**: Configuration is stored in `~/.config/eve-preview-manager/config.json`.

<br>

## System Requirements

- **Required:** OpenGL, fontconfig, dbus, libxkbcommon, libxcb (standard on most distros).
- **Recommended:** Wayland (via XWayland). Native X11 environments are supported but users may experience issues with preview overlays fighting for Z-order and incorrect image offsets.
- **Optional:** If using evdev instead of x11 hotkeys, you will need to add your user to the `input` group. Not recommended unless you know what you're doing.

<br>

## Installation

### Flatpak

Install from [Flathub](https://flathub.org/apps/com.evepreview.manager):

```bash
flatpak install flathub com.evepreview.manager
```

### Arch Linux (AUR)

Install from the [AUR](https://aur.archlinux.org/packages/eve-preview-manager) using your preferred AUR helper (e.g., `paru`, `yay`, `pamac`, `pikaur`, etc):

```bash
paru -S eve-preview-manager
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
