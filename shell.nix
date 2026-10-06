{ pkgs ? import <nixpkgs> { } }:

# Run `nix-shell` from the repository root. rustup reads rust-toolchain.toml
# and installs the pinned compiler, rustfmt, clippy, and musl target on entry.
pkgs.mkShell {
  packages = with pkgs; [
    rustup
    clang
    pkg-config
  ] ++ lib.optionals stdenv.isDarwin [ lld ];

  shellHook = ''
    ${pkgs.lib.optionalString pkgs.stdenv.isDarwin ''
      # The default target is Linux/musl; Apple's linker cannot link its ELF objects.
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=clang
      export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C link-arg=--target=x86_64-unknown-linux-musl -C link-arg=-fuse-ld=lld"
    ''}
    rustup show active-toolchain
  '';
}
