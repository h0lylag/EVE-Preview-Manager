{
  pkgs ? import <nixpkgs> { },
}:
let
  mainPackage = pkgs.callPackage ./default.nix { };
in
pkgs.mkShell {
  # Inherit build dependencies from the main package (includes cargo, rustc)
  inputsFrom = [ mainPackage ];

  # Additional dev tools not included in rustPlatform
  packages = with pkgs; [
    rust-analyzer
    clippy
    rustfmt
  ];

  # Runtime library path for running the binary in dev shell
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath mainPackage.passthru.runtimeLibs;
}
