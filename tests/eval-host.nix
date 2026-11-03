#### Evaluation Fixture (a minimal Jovian-less host for `nix flake check`) ####

{ lib, ... }:

{
  ## Stand-ins for the Jovian options the module reads
  options.jovian = {
    steam = {
      enable = lib.mkOption { default = true; };
      user = lib.mkOption { default = "player"; };
    };
    decky-loader = {
      enable = lib.mkOption { default = true; };
      stateDir = lib.mkOption { default = "/var/lib/decky-loader"; };
    };
  };

  config = {
    nixpkgs.hostPlatform = "x86_64-linux";
    fileSystems."/" = {
      device = "/dev/null";
      fsType = "ext4";
    };
    boot.loader.grub.enable = false;
    system.stateVersion = "26.05";
    users.users.player.isNormalUser = true;
    services.steam-maintenance.enable = true;
  };
}
