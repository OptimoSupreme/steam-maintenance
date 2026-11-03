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
    STEAM_MAINTENANCE_PROTOCOL_HANDLER = toString cfg.protocolHandler;
    STEAM_MAINTENANCE_UNIT = unit;
    STEAM_MAINTENANCE_GRID_DIR = "${cfg.package}/share/steam-maintenance/grid";
    STEAM_MAINTENANCE_TOAST = if cfg.toast then "1" else "0";
    STEAM_MAINTENANCE_EXE = "/run/current-system/sw/bin/steam-maintenance";
    STEAM_MAINTENANCE_APP_NAME = "System Update";
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
    pkgs.libnotify
    config.system.build.nixos-rebuild
    config.nix.package
    pkgs.coreutils
    nixos-version
  ];

  ## No arguments means ui, so the Steam shortcut needs no launch options
  wrapper = pkgs.writeShellApplication {
    name = "steam-maintenance";
    runtimeInputs = tools;
    text = ''
      ${lib.concatStringsSep "\n" (
        lib.mapAttrsToList (name: value: "export ${name}=${lib.escapeShellArg value}") envVars
      )}
      # Steam keeps no stderr for the shortcut it launches, so the UI logs to a file
      if [ "''${1:-ui}" = ui ]; then
        log="''${XDG_STATE_HOME:-$HOME/.local/state}/steam-maintenance/ui.log"
        mkdir -p "$(dirname "$log")"
        exec env -u LD_LIBRARY_PATH SDL_VIDEODRIVER=x11 ${lib.getExe cfg.package} "''${@:-ui}" 2>"$log"
      fi
      exec env -u LD_LIBRARY_PATH SDL_VIDEODRIVER=x11 ${lib.getExe cfg.package} "$@"
    '';
  };
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

    user = lib.mkOption {
      type = lib.types.str;
      default = config.jovian.steam.user;
      defaultText = lib.literalExpression "config.jovian.steam.user";
      description = "The Steam user, allowed to start and stop the upgrade unit.";
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
      description = "Deferrals allowed before a scheduled update becomes mandatory.";
    };

    batteryGate = lib.mkOption {
      type = lib.types.ints.between 0 100;
      default = 30;
      description = "Below this battery percentage, Update is disabled unless charging. 0 disables the gate.";
    };

    toast = lib.mkOption {
      type = lib.types.bool;
      default = true;
      description = "Send a desktop notification shortly before opening the UI.";
    };

    protocolHandler = lib.mkOption {
      type = lib.types.either lib.types.path lib.types.str;
      default = (pkgs.jovianPackages or { }).jovian-steam-protocol-handler or "steam";
      defaultText = lib.literalExpression ''pkgs.jovianPackages.jovian-steam-protocol-handler or "steam"'';
      description = "Program that hands a steam:// URL to the running Steam client.";
    };
  };

  ## Config
  config = lib.mkIf cfg.enable {
    assertions = [
      {
        assertion = config.jovian.steam.enable;
        message = "services.steam-maintenance needs jovian.steam.enable: the UI runs inside the Jovian gamescope session.";
      }
    ];

    environment.systemPackages = [ wrapper ];

    ## Register the shortcut before gamescope and Steam start
    environment.etc."jovian/gamescope-session/pre-start".text = ''
      ${wrapper}/bin/steam-maintenance register || true
    '';

    ## The upgrade itself, as root, started and stopped by the UI
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

    ## Let the Steam user start and stop the upgrade
    security.polkit.enable = true;
    security.polkit.extraConfig = ''
      polkit.addRule(function(action, subject) {
        if (action.id == "org.freedesktop.systemd1.manage-units" &&
            action.lookup("unit") == "${unit}" &&
            (action.lookup("verb") == "start" || action.lookup("verb") == "stop") &&
            subject.local && subject.active &&
            subject.user == "${cfg.user}") {
          return polkit.Result.YES;
        }
      });
    '';
  };
}

# Steam must launch the UI: gamescope only shows windows of processes Steam
# started, so the UI is a non-Steam shortcut (kept current by `register`) that
# the check opens through the steam:// protocol handler. The worker stages the
# new generation with `nixos-rebuild boot` and reboots only while the UI's
# heartbeat is fresh, so a machine never restarts behind a game or a blank
# screen; if the UI is gone, the generation simply waits for the next boot.
