//! Battery state from /sys/class/power_supply.

use serde::Serialize;
use std::fs;
use std::path::Path;

const POWER_SUPPLY_DIR: &str = "/sys/class/power_supply";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Battery {
    pub percent: u8,
    pub charging: bool,
}

/// The first `type=Battery` supply, or None on a machine without one.
pub fn read() -> Option<Battery> {
    read_from(Path::new(POWER_SUPPLY_DIR))
}

/// Like `read`, scanning `dir` instead of the real sysfs directory.
pub fn read_from(dir: &Path) -> Option<Battery> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    entries.sort();
    entries.iter().find_map(|path| read_supply(path))
}

fn read_supply(path: &Path) -> Option<Battery> {
    if attr(path, "type")? != "Battery" {
        return None;
    }
    let percent = attr(path, "capacity")?.parse::<i64>().ok()?.clamp(0, 100) as u8;
    let charging = attr(path, "status")
        .map(|s| is_charging(&s))
        .unwrap_or(false);
    Some(Battery { percent, charging })
}

fn attr(path: &Path, name: &str) -> Option<String> {
    fs::read_to_string(path.join(name))
        .ok()
        .map(|s| s.trim().to_string())
}

/// True when the battery is too low to start an upgrade without a charger.
pub fn blocks(battery: Option<Battery>, gate: u8) -> bool {
    match battery {
        Some(b) if gate > 0 => !b.charging && b.percent < gate,
        _ => false,
    }
}

/// "Charging" and "Full" both mean the machine is on external power.
fn is_charging(status: &str) -> bool {
    matches!(
        status.trim().to_ascii_lowercase().as_str(),
        "charging" | "full"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sm-battery-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn supply(dir: &Path, name: &str, attrs: &[(&str, &str)]) {
        let path = dir.join(name);
        fs::create_dir_all(&path).unwrap();
        for (key, value) in attrs {
            fs::write(path.join(key), format!("{value}\n")).unwrap();
        }
    }

    #[test]
    fn skips_the_adapter_and_reads_the_battery() {
        let dir = temp_dir("bat");
        supply(&dir, "AC", &[("type", "Mains"), ("online", "1")]);
        supply(
            &dir,
            "BAT1",
            &[
                ("type", "Battery"),
                ("capacity", "57"),
                ("status", "Discharging"),
            ],
        );
        assert_eq!(
            read_from(&dir),
            Some(Battery {
                percent: 57,
                charging: false
            })
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn full_counts_as_charging_case_insensitively() {
        let dir = temp_dir("full");
        supply(
            &dir,
            "BAT0",
            &[("type", "Battery"), ("capacity", "100"), ("status", "FULL")],
        );
        assert_eq!(
            read_from(&dir),
            Some(Battery {
                percent: 100,
                charging: true
            })
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn capacity_is_clamped() {
        let dir = temp_dir("clamp");
        supply(
            &dir,
            "BAT0",
            &[
                ("type", "Battery"),
                ("capacity", "120"),
                ("status", "Charging"),
            ],
        );
        assert_eq!(read_from(&dir).unwrap().percent, 100);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn no_battery_is_none() {
        let dir = temp_dir("none");
        supply(&dir, "AC", &[("type", "Mains")]);
        assert_eq!(read_from(&dir), None);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read_from(&dir), None);
    }

    #[test]
    fn gate_blocks_a_low_battery_unless_charging_or_disabled() {
        let low = Some(Battery {
            percent: 20,
            charging: false,
        });
        assert!(blocks(low, 30));
        assert!(!blocks(low, 0));
        assert!(!blocks(low, 20));
        assert!(!blocks(
            Some(Battery {
                percent: 20,
                charging: true
            }),
            30
        ));
        assert!(!blocks(None, 30));
    }

    #[test]
    fn status_words() {
        assert!(is_charging("Charging"));
        assert!(is_charging("full"));
        assert!(!is_charging("Discharging"));
        assert!(!is_charging("Not charging"));
        assert!(!is_charging("Unknown"));
    }
}
