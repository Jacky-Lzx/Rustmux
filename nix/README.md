# Nix package

The repository provides a flake package for `aarch64-linux` and `x86_64-linux`.
The Rust application also supports macOS; this Nix package currently covers Linux.

From the repository root on a supported Linux system:

```sh
nix build
./result/bin/rustmux --version
nix flake check --no-update-lock-file
```

`packages.<system>.default` and `packages.<system>.rustmux` refer to the same package.
`nix run . -- --help` runs the packaged binary without installing it globally.
The package reads its version from Cargo.toml and uses this checkout as its source.
No configuration repository or separate download of Rustmux itself is required.

To consume a published revision from another flake:

```nix
inputs.rustmux = {
  url = "github:Jacky-Lzx/Rustmux";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Install `rustmux.packages.${pkgs.stdenv.hostPlatform.system}.default` in the consuming
configuration, and commit its lock file to pin the Rustmux revision. Program settings
and personal keybindings remain in the consuming configuration; this package does
not deploy a user configuration or add NixOS/Home Manager options.

The build runs Rust unit, binary and integration tests, retaining the packaging
policy used before migration. `tests/terminal_loop.rs` and the Python nested-PTY
suite require host terminal behavior and are not run in the Nix build sandbox.
The flake checks additionally verify the installed binary's version and Nix formatting.
When Cargo dependencies change, update `cargoHash` in `nix/package.nix` and rebuild.
