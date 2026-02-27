{
  pkgs ? import <nixpkgs> { },
}:

let
  manifest = (pkgs.lib.importTOML ./Cargo.toml).package;

  # Runtime libraries
  runtimeLibs = with pkgs; [
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxrandr
    libxi
    fontconfig
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
    install -Dm644 assets/com.evepreview.manager.desktop $out/share/applications/eve-preview-manager.desktop
    install -Dm644 assets/com.evepreview.manager.svg $out/share/icons/hicolor/scalable/apps/com.evepreview.manager.svg
    install -Dm644 assets/com.evepreview.manager.metainfo.xml $out/share/metainfo/com.evepreview.manager.metainfo.xml
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
