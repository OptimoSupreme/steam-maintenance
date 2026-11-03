#### steam-maintenance ####

{
  description = "User-driven NixOS upgrades for Steam machines running Jovian gaming mode";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
    in
    {
      packages.${system} = {
        default = pkgs.callPackage ./package.nix { };
        plugin = pkgs.callPackage ./plugin/package.nix {
          steam-maintenance = self.packages.${system}.default;
        };
      };

      nixosModules.default = ./module.nix;

      checks.${system} = {
        package = self.packages.${system}.default;
        plugin = self.packages.${system}.plugin;
        module =
          (nixpkgs.lib.nixosSystem {
            inherit system;
            modules = [
              ./module.nix
              ./tests/eval-host.nix
            ];
          }).config.system.build.toplevel;
      };

      devShells.${system}.default = pkgs.mkShell {
        inputsFrom = [ self.packages.${system}.default ];
        packages = with pkgs; [
          cargo
          rustc
          clippy
          rustfmt
          rust-analyzer
          esbuild
          typescript
        ];
      };

      formatter.${system} = pkgs.nixfmt;
    };
}
