{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "rustmux";
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).package.version;

  src = lib.cleanSource ../.;

  cargoHash = "sha256-M/0h4b+9NDhu3Sw+sjcTssGujSdw/agHnK4H5ihka1c=";

  # Run all Rust unit/integration tests. The Python nested-PTY suite assumes
  # host shell behavior and absolute paths; validate the installed binary separately.
  preCheck = ''
    cargoTestFlags="--lib --bins"
    for test in tests/*.rs; do
      case "$test" in
        tests/terminal_loop.rs) continue ;;
      esac
      cargoTestFlags+=" --test $(basename "$test" .rs)"
    done
  '';

  checkFlags = [ "--test-threads=4" ];

  meta = {
    description = "Terminal multiplexer with persistent sessions and modal keybindings";
    homepage = "https://github.com/Jacky-Lzx/Rustmux";
    license = lib.licenses.mit;
    mainProgram = "rustmux";
    platforms = lib.platforms.linux;
  };
}
