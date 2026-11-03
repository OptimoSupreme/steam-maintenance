//! Runtime configuration, passed by the NixOS module through environment variables.

#[derive(Debug, Clone)]
pub struct Config {
    /// Flake reference nixos-rebuild builds from, e.g. `github:OptimoSupreme/nixos-configs`.
    pub flake: String,
    /// GitHub `owner/repo` of that flake, for ls-remote and the Actions API.
    pub repo: String,
    /// Branch the hosts follow.
    pub branch: String,
    /// Workflow file in that repo whose success means the cache holds the build.
    pub cache_workflow: String,
    /// When the weekly check becomes due, `Day HH:MM` local time, e.g. `Sun 12:00`.
    pub check_due: String,
    /// Deferrals allowed before a round installs on its own.
    pub max_deferrals: u32,
    /// Below this battery percentage, Update is disabled unless charging. 0 disables the gate.
    pub battery_gate: u8,
    /// The root system unit that performs the upgrade.
    pub unit: String,
}

fn env(name: &str) -> Option<String> {
    std::env::var(format!("STEAM_MAINTENANCE_{name}"))
        .ok()
        .filter(|v| !v.is_empty())
}

impl Config {
    pub fn from_env() -> Config {
        Config {
            flake: env("FLAKE").unwrap_or_else(|| "github:OptimoSupreme/nixos-configs".into()),
            repo: env("REPO").unwrap_or_else(|| "OptimoSupreme/nixos-configs".into()),
            branch: env("BRANCH").unwrap_or_else(|| "main".into()),
            cache_workflow: env("CACHE_WORKFLOW").unwrap_or_else(|| "cache.yml".into()),
            check_due: env("CHECK_DUE").unwrap_or_else(|| "Sun 12:00".into()),
            max_deferrals: env("MAX_DEFERRALS")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            battery_gate: env("BATTERY_GATE")
                .and_then(|v| v.parse().ok())
                .unwrap_or(30),
            unit: env("UNIT").unwrap_or_else(|| "steam-maintenance-upgrade.service".into()),
        }
    }
}
