{
  description = "Rustmux terminal multiplexer";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs, ... }:
    let
      forLinuxSystems = nixpkgs.lib.genAttrs [
        "aarch64-linux"
        "x86_64-linux"
      ];
    in
    {
      packages = forLinuxSystems (system: {
        default = nixpkgs.legacyPackages.${system}.callPackage ./nix/package.nix { };
        rustmux = self.packages.${system}.default;
      });

      checks = forLinuxSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
        in
        {
          package = self.packages.${system}.default;
          version = pkgs.testers.testVersion {
            package = self.packages.${system}.default;
          };
          formatting =
            pkgs.runCommand "rustmux-nix-formatting"
              {
                nativeBuildInputs = [ pkgs.nixfmt ];
              }
              ''
                nixfmt --check ${./flake.nix} ${./nix/package.nix}
                touch "$out"
              '';
        }
      );

      formatter = forLinuxSystems (system: nixpkgs.legacyPackages.${system}.nixfmt);
    };
}
