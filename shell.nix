{ pkgs ? import <nixpkgs> { } }:

# Run `nix-shell` from the repository root. rustup reads rust-toolchain.toml
# and installs the pinned compiler, rustfmt, clippy, and musl target on entry.
pkgs.mkShell {
  packages = with pkgs; [
    rustup
    clang
    pkg-config
  ];

  shellHook = ''
    rustup show active-toolchain
  '';
}
