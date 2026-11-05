#### Steam Maintenance ####

{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.steam-maintenance;
  unit = "steam-maintenance-upgrade.service";

  ## Environment config.rs reads
  envVars = {
    STEAM_MAINTENANCE_FLAKE = cfg.flake;
    STEAM_MAINTENANCE_REPO = cfg.repo;
    STEAM_MAINTENANCE_BRANCH = cfg.branch;
    STEAM_MAINTENANCE_CACHE_WORKFLOW = cfg.cacheWorkflow;
    STEAM_MAINTENANCE_CHECK_DUE = cfg.checkDue;
    STEAM_MAINTENANCE_MAX_DEFERRALS = toString cfg.maxDeferrals;
    STEAM_MAINTENANCE_BATTERY_GATE = toString cfg.batteryGate;
    STEAM_MAINTENANCE_UNIT = unit;
  };

  ## nixpkgs exports nixos-version only through environment.systemPackages
  nixos-version =
    lib.findFirst (p: lib.getName p == "nixos-version")
      (throw "steam-maintenance: system.tools.nixos-version.enable is required")
      config.environment.systemPackages;

  tools = [
    config.systemd.package
    pkgs.git
    pkgs.curl
    config.system.build.nixos-rebuild
    config.nix.package
    pkgs.coreutils
    nixos-version
  ];

  wrapper = pkgs.writeShellApplication {
    name = "steam-maintenance";
    runtimeInputs = tools;
    text = ''
      ${lib.concatStringsSep "\n" (
        lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg value}") envVars
      )}
      exec ${lib.getExe cfg.package} "$@"
    '';
  };

  ## The Decky plugin, calling the wrapper so it sees the same configuration
  plugin = pkgs.callPackage ./plugin/package.nix {
    steam-maintenance = wrapper;
    inherit (cfg.package) version;
  };

  pluginDir = "${config.jovian.decky-loader.stateDir}/plugins/steam-maintenance";
in
{
  ## Options
  options.services.steam-maintenance = {
    enable = lib.mkEnableOption "user-driven NixOS upgrades from Steam gaming mode";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage ./package.nix { };
      defaultText = lib.literalExpression "pkgs.callPackage ./package.nix { }";
      description = "The steam-maintenance package.";
    };

    flake = lib.mkOption {
      type = lib.types.str;
      default =
        if config.system.autoUpgrade.flake != null then
          config.system.autoUpgrade.flake
        else
          "github:OptimoSupreme/nixos-configs";
      defaultText = lib.literalExpression ''config.system.autoUpgrade.flake or "github:OptimoSupreme/nixos-configs"'';
      description = "Flake reference nixos-rebuild builds from.";
    };

    repo = lib.mkOption {
      type = lib.types.str;
      default = "OptimoSupreme/nixos-configs";
      description = "GitHub owner/repo of that flake, for ls-remote and the Actions API.";
    };

    branch = lib.mkOption {
      type = lib.types.str;
      default = "main";
      description = "Branch the hosts follow.";
    };

    cacheWorkflow = lib.mkOption {
      type = lib.types.str;
      default = "cache.yml";
      description = "Workflow file in the repo whose success means the binary cache holds the build.";
    };

    checkDue = lib.mkOption {
      type = lib.types.str;
      default = "Sun 12:00";
      example = "Fri 18:00";
      description = "When the weekly check becomes due, `Day HH:MM` local time.";
    };

    maxDeferrals = lib.mkOption {
      type = lib.types.ints.unsigned;
      default = 2;
      description = "Deferrals allowed before a scheduled update installs on its own.";
    };

    batteryGate = lib.mkOption {
      type = lib.types.ints.between 0 100;
      default = 30;
      description = "Below this battery percentage, Update is disabled unless charging. 0 disables the gate.";
    };
  };

  ## Config
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.jovian.steam.enable;
        message = "services.steam-maintenance needs jovian.steam.enable: the check runs inside the Jovian gamescope session.";
      }
      {
        assertion = config.jovian.decky-loader.enable;
        message = "services.steam-maintenance needs jovian.decky-loader.enable: the update panel is a Decky plugin.";
      }
    ];

    environment.systemPackages = [ wrapper ];

    ## The plugin, linked from the store so Decky's own updater leaves it alone
    # Not tmpfiles: it refuses plugins/, root-owned under the Decky user's stateDir
    systemd.services.decky-loader.preStart = lib.mkAfter ''
      mkdir -p "${dirOf pluginDir}"
      # A real directory is a development copy
      if [ -L "${pluginDir}" ] || [ ! -e "${pluginDir}" ]; then
        ln -sfn "${plugin}" "${pluginDir}"
      fi
    '';

    ## The upgrade itself, as root, started and stopped by the plugin backend
    systemd.services.steam-maintenance-upgrade = {
      description = "System Update";
      restartIfChanged = false;
      unitConfig.X-StopOnRemoval = false;
      environment =
        config.nix.envVars
        // {
          HOME = "/root";
          NIX_PATH = config.environment.sessionVariables.NIX_PATH or "";
        }
        // config.networking.proxy.envVars
        // envVars;
      path = tools;
      serviceConfig = {
        Type = "exec";
        ExecStart = "${config.systemd.package}/bin/systemd-inhibit --what=sleep:idle --mode=block --who='System Update' --why='Installing a NixOS update' ${lib.getExe cfg.package} worker";
        RuntimeDirectory = "steam-maintenance";
        RuntimeDirectoryMode = "0755";
        RuntimeDirectoryPreserve = "yes";
        KillMode = "control-group";
        TimeoutStopSec = 90;
        OOMScoreAdjust = 500;
      };
    };

    ## Periodic check inside the gaming session
    systemd.user.timers.steam-maintenance-check = {
      wantedBy = [ "gamescope-session.target" ];
      partOf = [ "gamescope-session.target" ];
      timerConfig = {
        OnActiveSec = "3min";
        OnUnitActiveSec = "10min";
        AccuracySec = "1min";
      };
    };

    systemd.user.services.steam-maintenance-check = {
      description = "Steam Maintenance update check";
      after = [ "graphical-session.target" ];
      environment = envVars;
      serviceConfig = {
        Type = "oneshot";
        EnvironmentFile = "%t/gamescope-environment";
        ExecStart = "${wrapper}/bin/steam-maintenance check";
      };
    };
  };
}

# The check runs in the gaming session, where it can see what gamescope shows,
# and only ever hands a prompt to the plugin while Steam sits idle on its home
# screen. The plugin backend runs as root under Decky and starts, stops and
# follows the worker; the worker stages the new generation with `nixos-rebuild
# boot` and reboots only while the plugin's heartbeat is fresh, which the plugin
# sends while its update view is on screen and no game is running. Otherwise
# the generation simply waits for the next boot.
