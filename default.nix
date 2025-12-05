{
  pkgs ? import <nixpkgs> { },
}:

let
  manifest = (pkgs.lib.importTOML ./Cargo.toml).package;

  # Runtime libraries for eframe (glow backend) + wayland + X11
  runtimeLibs = with pkgs; [
    libGL # OpenGL for eframe/glow
    libxkbcommon # Keyboard handling
    wayland # Wayland backend
    xorg.libX11 # X11 backend
    xorg.libXcursor
    xorg.libXrandr
    xorg.libXi
    fontconfig # Font discovery (fontconfig crate)
    dbus # D-Bus for ksni system tray
  ];
in

pkgs.rustPlatform.buildRustPackage rec {
  pname = manifest.name;
  version = manifest.version;

  cargoLock.lockFile = ./Cargo.lock;

  src = pkgs.lib.cleanSource ./.;

  # Skip tests in build
  doCheck = false;

  nativeBuildInputs = with pkgs; [
    makeWrapper
    pkg-config
  ];

  buildInputs = runtimeLibs;

  # Wrap binary with LD_LIBRARY_PATH for runtime-loaded libs (OpenGL, Wayland, X11)
  postInstall = ''
    wrapProgram $out/bin/eve-preview-manager --prefix LD_LIBRARY_PATH : "${pkgs.lib.makeLibraryPath runtimeLibs}"
  '';

  # Expose runtimeLibs for shell.nix to reuse
  passthru = {
    inherit runtimeLibs;
  };

  meta = with pkgs.lib; {
    description = "EVE Preview Manager — EVE Online Window Switcher and Preview Manager for Linux";
    homepage = "https://github.com/h0lylag/EVE-Preview-Manager";
    license = licenses.mit;
    platforms = [ "x86_64-linux" ];
    mainProgram = "eve-preview-manager";
  };

}
