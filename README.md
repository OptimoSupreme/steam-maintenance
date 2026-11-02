# steam-maintenance

User-driven NixOS upgrades for Steam machines running Jovian gaming mode.

## What it does

- Adds a **System Update** tile to the Steam library. Opening it shows whether
  the fleet repo has a newer commit than the running system and whether the
  binary cache already holds that build, then offers Update, Later, or Up to
  date.
- Once a week (`checkDue`, default Sunday noon) a user timer checks for an
  update while Steam sits idle on its home screen and opens the same tile. A
  scheduled prompt can be deferred `maxDeferrals` times (default 2); the
  deferral that reaches the limit warns that the next one cannot be deferred,
  after which Later is hidden. Starting an upgrade, or pressing Up to date on
  a manual launch, resets the count. Cancelling an upgrade does not count as a
  deferral; it snoozes the prompt for a day.
- Update runs `nixos-rebuild boot` as root with live progress: evaluating,
  downloading with a percentage, building, installing. The build can be
  cancelled until the bootloader step. On a machine with a battery, Update is
  disabled below `batteryGate` percent unless charging.
- When the new generation is staged the worker reboots, but only while the UI
  is on screen: the UI touches a heartbeat file every two seconds and the
  worker refuses to reboot if the newest heartbeat is older than ten seconds.
  If the UI is gone, the generation waits for the next normal boot.

## Why it is a Steam shortcut

gamescope in gaming mode only shows windows of processes Steam launched, that
is, processes with a `reaper SteamLaunch` ancestor. A program started from a
systemd unit or a terminal gets a window that never appears on screen. So the
UI is registered as a non-Steam shortcut (`steam-maintenance register`, run
by a Jovian session pre-start hook before Steam reads `shortcuts.vdf`), and
the weekly check opens it through the `steam://` protocol handler. This is
also why Steam Input, not evdev, delivers the controller: a launched shortcut
gets a virtual Xbox 360 pad that SDL sees. Keyboard and mouse work as well.

## Using the flake

```nix
{
  inputs.steam-maintenance.url = "github:OptimoSupreme/steam-maintenance";

  # in a host
  imports = [ inputs.steam-maintenance.nixosModules.default ];
  services.steam-maintenance.enable = true;
}
```

The module requires `jovian.steam.enable`. Options under
`services.steam-maintenance`:

| Option            | Default                                                         |
| ----------------- | --------------------------------------------------------------- |
| `package`         | `pkgs.callPackage ./package.nix { }`                            |
| `user`            | `config.jovian.steam.user`                                      |
| `flake`           | `config.system.autoUpgrade.flake`, else `github:OptimoSupreme/nixos-configs` |
| `repo`            | `"OptimoSupreme/nixos-configs"`                                 |
| `branch`          | `"main"`                                                        |
| `cacheWorkflow`   | `"cache.yml"`, the workflow whose success means the build is cached |
| `checkDue`        | `"Sun 12:00"`, local time                                       |
| `maxDeferrals`    | `2`                                                             |
| `batteryGate`     | `30`, percent; `0` disables the gate                            |
| `toast`           | `true`, notify shortly before the scheduled prompt              |
| `protocolHandler` | `pkgs.jovianPackages.jovian-steam-protocol-handler`, else `"steam"` |

The module installs a `steam-maintenance` wrapper, the root unit
`steam-maintenance-upgrade.service` (no timer; the UI starts and stops it, a
polkit rule allows the Steam user to do so), the user timer
`steam-maintenance-check.timer` bound to `gamescope-session.target`, and the
session pre-start hook. `steam-maintenance` with no arguments opens the UI;
an admin over SSH can run `steam-maintenance check --force` to open it now.

## What the host still has to do

- Serve the build from a binary cache the machine trusts, and make
  `cacheWorkflow` the workflow that fills it. The UI only offers an update
  whose build is cached; a Steam machine should not compile.
- Set `system.configurationRevision` (for example to `self.rev or
  self.dirtyRev`) so `nixos-version --json` reports the running commit. Without
  it, every check looks like an update.
- Disable the daily `system.autoUpgrade` timer on these hosts, or the fleet
  will update behind the user's back. `system.autoUpgrade.flake` is still read
  as the default for `flake`.
- `nix.gc` and `nix.settings` are the host's business; the module sets neither.

## Files for debugging

| Path                                                      | Written by | Contents                                          |
| --------------------------------------------------------- | ---------- | ------------------------------------------------- |
| `/run/steam-maintenance/status.json`                      | worker     | phase, percent, detail, counters, error, log tail |
| `$XDG_RUNTIME_DIR/steam-maintenance/heartbeat`            | ui         | touched every 2 s while the UI is on screen       |
| `~/.local/state/steam-maintenance/state.json`             | ui, check  | next due time, deferrals, snooze, shortcut app id |
| `~/.local/state/steam-maintenance/launch-reason`          | check      | why the UI is being opened; consumed by the UI    |
| `~/.local/share/Steam/userdata/<id>/config/shortcuts.vdf` | register   | the Steam shortcut                                |
| `journalctl -u steam-maintenance-upgrade`                 | worker     | nixos-rebuild output                              |
| `journalctl --user -u steam-maintenance-check`            | check      | why the check did or did not open the UI          |
