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
      packages.${system}.default = pkgs.callPackage ./package.nix { };

      nixosModules.default = ./module.nix;

      checks.${system} = {
        package = self.packages.${system}.default;
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
        STEAM_MAINTENANCE_FONT_REGULAR = "${pkgs.inter}/share/fonts/truetype/InterVariable.ttf";
        STEAM_MAINTENANCE_FONT_BOLD = "${pkgs.inter}/share/fonts/truetype/Inter.ttc";
        packages = with pkgs; [
          cargo
          rustc
          clippy
          rustfmt
          rust-analyzer
        ];
      };

      formatter.${system} = pkgs.nixfmt;
    };
}
