# steam-maintenance

User-driven NixOS upgrades for Steam machines running Jovian gaming mode.

## What it does

- Adds a **System Update** panel to the Quick Access Menu through a Decky
  plugin. Opening it shows whether the fleet repo has a newer commit than the
  running system and whether the binary cache already holds that build, then
  offers Update, Restart or Refresh. It can be opened mid-game; an update
  started from it runs in the background.
- Once a week (`checkDue`, default Sunday noon) a user timer checks for an
  update while Steam sits idle on its home screen and, if there is one, shows
  a prompt through the plugin with Update and Later. A scheduled prompt can be
  deferred `maxDeferrals` times (default 2); the deferral that reaches the
  limit warns that the next round installs on its own, and that round starts
  the update without asking (a toast says so). Nothing is ever prompted or
  started while a game is running. A pending restart is prompted the same way
  but never forced.
- Update runs `nixos-rebuild boot` as root with live progress: evaluating,
  downloading with a percentage, building, installing. The build can be
  cancelled until the bootloader step; cancelling snoozes the prompt for a
  day. On a machine with a battery, Update is disabled below `batteryGate`
  percent unless charging.
- When the new generation is staged the worker reboots, but only while the
  plugin confirms the update view is on screen and no game is running: the
  plugin sends a heartbeat every two seconds under those conditions and the
  worker refuses to reboot if the newest heartbeat is older than ten seconds.
  Otherwise it leaves the generation staged, a toast says so, and the panel
  offers Restart now; failing that, the generation boots next time anyway.

## How the pieces fit

| Piece                                   | Runs as    | Does                                                                 |
| --------------------------------------- | ---------- | -------------------------------------------------------------------- |
| `steam-maintenance check` (user timer)  | Steam user | Gates on Steam being idle at home, probes, hands a prompt to the plugin |
| Decky plugin backend (`plugin/main.py`) | root       | Runs `steam-maintenance plugin <verb>`; user verbs drop to the Steam user |
| `steam-maintenance plugin watch`        | Steam user | Streams prompts and worker progress to the frontend as events        |
| Decky plugin frontend (`plugin/src`)    | Steam UI   | Panel, prompt modal, toasts, heartbeat                               |
| `steam-maintenance-upgrade.service`     | root       | `nixos-rebuild boot`, GC, guarded reboot                             |

The check has to live in the gaming session because only there can it read
gamescope's focused app. The backend has to be root because a process
outside the user's logind session gets no polkit grant to drive system units.
The frontend is bundled with esbuild against Steam's own React and Decky's
component library, which the loader exposes as globals; only `@decky/api` is
bundled in, from a pinned npm tarball, so the build needs no Node toolchain.

## Using the flake

```nix
{
  inputs.steam-maintenance.url = "github:OptimoSupreme/steam-maintenance";

  # in a host
  imports = [ inputs.steam-maintenance.nixosModules.default ];
  services.steam-maintenance.enable = true;
}
```

The module requires `jovian.steam.enable` and `jovian.decky-loader.enable`.
Options under `services.steam-maintenance`:

| Option          | Default                                                                      |
| --------------- | ---------------------------------------------------------------------------- |
| `package`       | `pkgs.callPackage ./package.nix { }`                                         |
| `flake`         | `config.system.autoUpgrade.flake`, else `github:OptimoSupreme/nixos-configs` |
| `repo`          | `"OptimoSupreme/nixos-configs"`                                              |
| `branch`        | `"main"`                                                                     |
| `cacheWorkflow` | `"cache.yml"`, the workflow whose success means the build is cached          |
| `checkDue`      | `"Sun 12:00"`, local time                                                    |
| `maxDeferrals`  | `2`                                                                          |
| `batteryGate`   | `30`, percent; `0` disables the gate                                         |

The module installs a `steam-maintenance` wrapper, the root unit
`steam-maintenance-upgrade.service` (no timer; the plugin starts and stops
it), the user timer `steam-maintenance-check.timer` bound to
`gamescope-session.target`, and links the plugin into
`<jovian.decky-loader.stateDir>/plugins/steam-maintenance` from the store, so
a deploy brings it along and Decky's own updater leaves it alone. An admin
over SSH can run `steam-maintenance check --force` to trigger the prompt now.

## What the host still has to do

- Serve the build from a binary cache the machine trusts, and make
  `cacheWorkflow` the workflow that fills it. The plugin only offers an update
  whose build is cached; a Steam machine should not compile.
- Set `system.configurationRevision` (for example to `self.rev or
  self.dirtyRev`) so `nixos-version --json` reports the running commit. Without
  it, every check looks like an update.
- Disable the daily `system.autoUpgrade` timer on these hosts, or the fleet
  will update behind the user's back. `system.autoUpgrade.flake` is still read
  as the default for `flake`.
- `nix.gc` and `nix.settings` are the host's business; the module sets neither.

## Files for debugging

| Path                                                  | Written by     | Contents                                          |
| ----------------------------------------------------- | -------------- | ------------------------------------------------- |
| `/run/steam-maintenance/status.json`                  | worker         | phase, percent, detail, counters, error, log tail |
| `/run/steam-maintenance/request.json`                 | plugin (root)  | the commit the user saw and asked for             |
| `$XDG_RUNTIME_DIR/steam-maintenance/heartbeat`        | plugin (user)  | touched every 2 s while the view is on screen     |
| `$XDG_RUNTIME_DIR/steam-maintenance/prompt.json`      | check          | the prompt waiting for the plugin to pick it up   |
| `~/.local/state/steam-maintenance/state.json`         | check, plugin  | next due time, deferrals, snooze                  |
| `journalctl -u steam-maintenance-upgrade`             | worker         | nixos-rebuild output                              |
| `journalctl --user -u steam-maintenance-check`        | check          | why the check did or did not prompt               |
| `/var/lib/decky-loader/logs/steam-maintenance/`       | plugin backend | what the backend ran and what failed              |

## Developing

`nix develop` gives cargo, clippy, rustfmt, esbuild and tsc. `nix flake
check` builds the binary, the plugin (type check included) and evaluates the
module against `tests/eval-host.nix`. `nix build .#plugin` produces the
directory Decky loads; to try it on a device without a deploy, copy it over
`/var/lib/decky-loader/plugins/steam-maintenance` and restart
`decky-loader.service`.
