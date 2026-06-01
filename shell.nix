{
  pkgs ? import <nixpkgs> { },
  rustToolchain ? null,
}:
let
  mainPackage = pkgs.callPackage ./default.nix { inherit rustToolchain; };
  rustPackages =
    if rustToolchain == null then
      with pkgs; [
        clippy
        rustfmt
      ]
    else
      [ rustToolchain ];
in
pkgs.mkShell {
  # Inherit build dependencies from the main package (includes cargo, rustc)
  inputsFrom = [ mainPackage ];

  # Additional Rust tools come from rust-toolchain.toml when rustToolchain is provided.
  packages = rustPackages;

  # Runtime library path for running the binary in dev shell
  LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath mainPackage.passthru.runtimeLibs;
}
