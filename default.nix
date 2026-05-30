{
  pkgs ? import <nixpkgs> { },
  rustToolchain ? null,
}:

let
  manifest = (pkgs.lib.importTOML ./Cargo.toml).package;
  rustPlatform =
    if rustToolchain == null then
      pkgs.rustPlatform
    else
      pkgs.makeRustPlatform {
        cargo = rustToolchain;
        rustc = rustToolchain;
      };

  runtimeLibs = with pkgs; [
    stdenv.cc.cc.lib
    libGL
    libxkbcommon
    wayland
    libx11
    libxcursor
    libxrandr
    libxi
  ];
in

rustPlatform.buildRustPackage rec {
  pname = manifest.name;
  version = manifest.version;

  cargoHash = "sha256-gvUA2OfIpydh2omyhGQ9NncLmcTmxsEV5kB/Rr3KpAg=";

  src = pkgs.lib.cleanSource ./.;

  # Skip tests in build
  doCheck = false;

  nativeBuildInputs = with pkgs; [
    pkg-config
    autoPatchelfHook
  ];

  buildInputs = runtimeLibs ++ [ pkgs.fontconfig ];

  runtimeDependencies = runtimeLibs;

  postInstall = ''
    install -Dm644 assets/com.evepreview.manager.desktop $out/share/applications/eve-preview-manager.desktop
    install -Dm644 assets/com.evepreview.manager.svg $out/share/icons/hicolor/scalable/apps/com.evepreview.manager.svg
    install -Dm644 assets/com.evepreview.manager.metainfo.xml $out/share/metainfo/com.evepreview.manager.metainfo.xml
  '';

  # Expose runtimeLibs for shell.nix to reuse
  passthru = {
    inherit runtimeLibs;
  };

  meta = with pkgs.lib; {
    description = "Utility for EVE Online multiboxing with real-time previews and hotkeys";
    homepage = "https://github.com/h0lylag/EVE-Preview-Manager";
    changelog = "https://github.com/h0lylag/EVE-Preview-Manager/releases/tag/v${manifest.version}";
    license = licenses.mit;
    maintainers = with maintainers; [ h0lylag ];
    platforms = platforms.linux;
    mainProgram = "eve-preview-manager";
  };

}
