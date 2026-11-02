//! Keep the "System Update" non-Steam shortcut in every Steam user's shortcuts.vdf.
//!
//! Steam rewrites shortcuts.vdf when it exits, so this runs while Steam is down
//! (the NixOS module calls it at session start, before Steam). It upserts one entry
//! per userdata directory, copies the grid art Steam shows for that entry, and
//! remembers the app id so `check` can launch the shortcut by `steam://rungameid/`.

use crate::config::Config;
use crate::state::State;
use crate::util::{atomic_write, home_dir};
use crate::vdf::{self, Value};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const SHORTCUTS_FILE: &str = "shortcuts.vdf";
const BACKUP_FILE: &str = "shortcuts.vdf.bak";
/// Steam library roots relative to `$HOME`.
const STEAM_ROOTS: [&str; 2] = [".steam/steam", ".local/share/Steam"];
/// `(source name in grid_dir, suffix after the app id in config/grid)`.
const GRID_ART: [(&str, &str); 4] = [
    ("portrait.png", "p.png"),
    ("landscape.png", ".png"),
    ("hero.png", "_hero.png"),
    ("logo.png", "_logo.png"),
];

/// Steam's legacy shortcut id: crc32 of the quoted exe followed by the name, high bit set.
pub fn shortcut_appid(exe_quoted: &str, app_name: &str) -> u32 {
    crc32fast::hash(format!("{exe_quoted}{app_name}").as_bytes()) | 0x8000_0000
}

/// The 64-bit id `steam://rungameid/<id>` expects for a shortcut.
pub fn rungameid(appid: u32) -> u64 {
    ((appid as u64) << 32) | 0x0200_0000
}

/// What happened to one shortcuts.vdf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Updated,
    Unchanged,
}

impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Outcome::Created => "created",
            Outcome::Updated => "updated",
            Outcome::Unchanged => "unchanged",
        }
    }
}

pub fn run(cfg: &Config) -> Result<()> {
    let dirs = userdata_config_dirs();
    if dirs.is_empty() {
        println!("no Steam userdata found; start Steam once, then register again");
        return Ok(());
    }
    let mut first_appid = None;
    for dir in dirs {
        let path = dir.join(SHORTCUTS_FILE);
        let (appid, outcome) =
            register_file(&path, cfg).with_context(|| format!("updating {}", path.display()))?;
        println!("{} {}", outcome.label(), path.display());
        if let Some(grid_dir) = &cfg.grid_dir {
            copy_grid_art(grid_dir, &dir.join("grid"), appid)
                .with_context(|| format!("copying grid art into {}", dir.display()))?;
        }
        first_appid.get_or_insert(appid);
    }
    if let Some(appid) = first_appid {
        let mut state = State::load();
        state.shortcut_appid = Some(appid);
        state.save().context("saving state")?;
    }
    Ok(())
}

/// `userdata/<id>/config` under every Steam root that exists, deduplicated by
/// canonical path (`~/.steam/steam` is usually a symlink to `~/.local/share/Steam`).
fn userdata_config_dirs() -> Vec<PathBuf> {
    let Some(home) = home_dir() else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut dirs = Vec::new();
    for root in STEAM_ROOTS {
        let userdata = home.join(root).join("userdata");
        let Ok(entries) = fs::read_dir(&userdata) else {
            continue;
        };
        let mut found: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_user_id(p) && p.join("config").is_dir())
            .collect();
        found.sort();
        for user in found {
            let config = user.join("config");
            let canonical = fs::canonicalize(&config).unwrap_or_else(|_| config.clone());
            if seen.insert(canonical) {
                dirs.push(config);
            }
        }
    }
    dirs
}

/// A userdata child named by a non-zero account id.
fn is_user_id(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.parse::<u64>().ok())
        .is_some_and(|id| id != 0)
}

/// Upsert the shortcut in one file, backing it up before the first change.
fn register_file(path: &Path, cfg: &Config) -> Result<(u32, Outcome)> {
    let old = match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).context("reading"),
    };
    let root = match &old {
        Some(bytes) => vdf::parse(bytes).context("parsing")?,
        None => empty_root(),
    };
    let (root, appid, changed) = upsert(root, cfg);
    let bytes = vdf::serialize(&root);
    if !changed && old.as_deref() == Some(bytes.as_slice()) {
        return Ok((appid, Outcome::Unchanged));
    }
    if old.is_some() {
        fs::copy(path, path.with_file_name(BACKUP_FILE)).context("backing up")?;
    }
    atomic_write(path, &bytes)?;
    let outcome = if old.is_some() {
        Outcome::Updated
    } else {
        Outcome::Created
    };
    Ok((appid, outcome))
}

/// The file Steam writes before any shortcut exists.
fn empty_root() -> Value {
    Value::Map(vec![(b"shortcuts".to_vec(), Value::map())])
}

/// Make the parsed file hold the shortcut: update the entry whose `Exe` (or, failing
/// that, `AppName`) matches, otherwise append one. Returns the new tree, the entry's
/// app id, and whether anything changed.
pub fn upsert(mut root: Value, cfg: &Config) -> (Value, u32, bool) {
    let before = root.clone();
    if root.get("shortcuts").and_then(Value::entries).is_none() {
        root.insert("shortcuts", Value::map());
    }
    let shortcuts = root.get_mut("shortcuts").expect("just ensured");
    let appid = match find_entry(shortcuts, cfg) {
        Some(index) => {
            let entry = &mut shortcuts.entries_mut().expect("is a map")[index].1;
            let appid = entry
                .get("appid")
                .and_then(Value::as_i32)
                .map(|n| n as u32)
                .unwrap_or_else(|| shortcut_appid(&cfg.exe_quoted(), &cfg.app_name));
            update_entry(entry, cfg, appid);
            appid
        }
        None => {
            let appid = shortcut_appid(&cfg.exe_quoted(), &cfg.app_name);
            let key = next_free_key(shortcuts);
            let entry = new_entry(cfg, appid);
            shortcuts
                .entries_mut()
                .expect("is a map")
                .push((key.into_bytes(), entry));
            appid
        }
    };
    let changed = root != before;
    (root, appid, changed)
}

/// Index of our entry in the shortcuts map: `Exe` equal to the quoted exe wins,
/// then `AppName` equal to the configured name.
fn find_entry(shortcuts: &Value, cfg: &Config) -> Option<usize> {
    let entries = shortcuts.entries()?;
    let exe = cfg.exe_quoted();
    let by = |key: &str, want: &str| {
        entries.iter().position(|(_, entry)| {
            entry
                .get(key)
                .and_then(Value::as_bytes)
                .is_some_and(|v| v == want.as_bytes())
        })
    };
    by("Exe", &exe).or_else(|| by("AppName", &cfg.app_name))
}

/// One past the largest numeric key, or "0" when there is none.
fn next_free_key(shortcuts: &Value) -> String {
    shortcuts
        .entries()
        .into_iter()
        .flatten()
        .filter_map(|(k, _)| std::str::from_utf8(k).ok()?.parse::<u64>().ok())
        .max()
        .map_or(0, |n| n + 1)
        .to_string()
}

/// The fields this program owns, in the order Steam writes them.
fn owned_fields(cfg: &Config) -> Vec<(&'static str, Value)> {
    vec![
        ("AppName", Value::string(&cfg.app_name)),
        ("Exe", Value::string(&cfg.exe_quoted())),
        ("StartDir", Value::string("/")),
        ("LaunchOptions", Value::string("")),
        ("IsHidden", Value::int32(0)),
        ("AllowDesktopConfig", Value::int32(1)),
        ("AllowOverlay", Value::int32(1)),
        ("OpenVR", Value::int32(0)),
        ("Devkit", Value::int32(0)),
        ("DevkitGameID", Value::string("")),
        ("DevkitOverrideAppID", Value::int32(0)),
    ]
}

fn icon_path(cfg: &Config) -> Option<String> {
    cfg.grid_dir
        .as_ref()
        .map(|d| d.join("logo.png").to_string_lossy().into_owned())
}

/// Overwrite the owned fields of an existing entry; everything else stays.
fn update_entry(entry: &mut Value, cfg: &Config, appid: u32) {
    for (key, value) in owned_fields(cfg) {
        entry.insert(key, value);
    }
    if let Some(icon) = icon_path(cfg) {
        entry.insert("icon", Value::string(&icon));
    }
    if entry.get("appid").is_none() {
        entry.insert("appid", Value::int32(appid as i32));
    }
}

/// A complete entry with today's Steam key spellings and order.
fn new_entry(cfg: &Config, appid: u32) -> Value {
    let mut entry = Value::Map(vec![(b"appid".to_vec(), Value::int32(appid as i32))]);
    // icon and ShortcutPath sit between StartDir and LaunchOptions in Steam's order.
    let mut fields = owned_fields(cfg);
    let icon = ("icon", Value::string(&icon_path(cfg).unwrap_or_default()));
    let shortcut_path = ("ShortcutPath", Value::string(""));
    fields.insert(3, icon);
    fields.insert(4, shortcut_path);
    for (key, value) in fields {
        entry.insert(key, value);
    }
    entry.insert("LastPlayTime", Value::int32(0));
    entry.insert("FlatpakAppID", Value::string(""));
    let tags = Value::Map(vec![(b"0".to_vec(), Value::string("favorite"))]);
    entry.insert("tags", tags);
    entry
}

/// Copy the four artwork files into config/grid under the app id, never over
/// art the user already put there.
fn copy_grid_art(grid_dir: &Path, dest_dir: &Path, appid: u32) -> Result<()> {
    for (source, suffix) in GRID_ART {
        let from = grid_dir.join(source);
        if !from.is_file() {
            continue;
        }
        let to = dest_dir.join(format!("{appid}{suffix}"));
        if to.exists() {
            continue;
        }
        fs::create_dir_all(dest_dir)?;
        fs::copy(&from, &to).with_context(|| format!("copying {}", from.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(exe: &str, app_name: &str, grid_dir: Option<&str>) -> Config {
        Config {
            flake: String::new(),
            repo: String::new(),
            branch: String::new(),
            cache_workflow: String::new(),
            check_due: String::new(),
            max_deferrals: 2,
            battery_gate: 30,
            protocol_handler: PathBuf::from("steam"),
            unit: String::new(),
            grid_dir: grid_dir.map(PathBuf::from),
            toast: true,
            exe: exe.into(),
            app_name: app_name.into(),
        }
    }

    fn default_cfg() -> Config {
        cfg(
            "/run/current-system/sw/bin/steam-maintenance",
            "System Update",
            Some("/nix/store/abc-grid"),
        )
    }

    fn fixture(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name);
        fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sm-register-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn str_of(entry: &Value, key: &str) -> String {
        entry.get(key).and_then(|v| v.as_str()).unwrap()
    }

    fn i32_of(entry: &Value, key: &str) -> i32 {
        entry.get(key).and_then(Value::as_i32).unwrap()
    }

    fn our_entry<'a>(root: &'a Value, cfg: &Config) -> &'a Value {
        let shortcuts = root.get("shortcuts").unwrap();
        let index = find_entry(shortcuts, cfg).expect("entry present");
        &shortcuts.entries().unwrap()[index].1
    }

    #[test]
    fn crc32_check_value() {
        assert_eq!(crc32fast::hash(b"123456789"), 0xCBF4_3926);
        assert_eq!(shortcut_appid("1234", "56789"), 0xCBF4_3926);
    }

    #[test]
    fn appid_matches_steam_written_fixtures() {
        // shortcuts-failing.vdf entry 1: icon is grid\3515717211_icon.ico.
        let powershell = r#""C:\WINDOWS\System32\WindowsPowerShell\v1.0\powershell.exe""#;
        assert_eq!(shortcut_appid(powershell, "Far Cry 3"), 3_515_717_211);
        // shortcuts-steam_rom_manager.vdf entry 0: DevkitGameID is boilr-4012838840.
        let odd_realm = r"D:\\Itch\\oddrealm\oddrealm_v0_10_0_21_win/OddRealm.exe";
        assert_eq!(shortcut_appid(odd_realm, "Odd Realm"), 4_012_838_840);
        assert_eq!(shortcut_appid(odd_realm, "Odd Realm"), 0xEF2F_0FB8);
    }

    #[test]
    fn rungameid_shifts_the_appid_and_sets_the_shortcut_flag() {
        assert_eq!(rungameid(0xD18D_965B), 0xD18D_965B_0200_0000);
        assert_eq!(rungameid(1), 0x0000_0001_0200_0000);
    }

    #[test]
    fn upsert_appends_a_complete_entry_to_an_empty_file() {
        let cfg = default_cfg();
        let (root, appid, changed) = upsert(empty_root(), &cfg);
        assert!(changed);
        assert_eq!(appid, shortcut_appid(&cfg.exe_quoted(), "System Update"));
        let shortcuts = root.get("shortcuts").unwrap().entries().unwrap();
        assert_eq!(shortcuts.len(), 1);
        assert_eq!(shortcuts[0].0, b"0");
        let entry = &shortcuts[0].1;
        let keys: Vec<&str> = entry
            .entries()
            .unwrap()
            .iter()
            .map(|(k, _)| std::str::from_utf8(k).unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "appid",
                "AppName",
                "Exe",
                "StartDir",
                "icon",
                "ShortcutPath",
                "LaunchOptions",
                "IsHidden",
                "AllowDesktopConfig",
                "AllowOverlay",
                "OpenVR",
                "Devkit",
                "DevkitGameID",
                "DevkitOverrideAppID",
                "LastPlayTime",
                "FlatpakAppID",
                "tags",
            ]
        );
        assert_eq!(i32_of(entry, "appid") as u32, appid);
        assert_eq!(str_of(entry, "AppName"), "System Update");
        assert_eq!(
            str_of(entry, "Exe"),
            "\"/run/current-system/sw/bin/steam-maintenance\""
        );
        assert_eq!(str_of(entry, "StartDir"), "/");
        assert_eq!(str_of(entry, "icon"), "/nix/store/abc-grid/logo.png");
        assert_eq!(i32_of(entry, "AllowOverlay"), 1);
        assert_eq!(i32_of(entry, "OpenVR"), 0);
        assert_eq!(i32_of(entry, "LastPlayTime"), 0);
        let tags = entry.get("tags").unwrap();
        assert_eq!(str_of(tags, "0"), "favorite");
        // A file Steam can read back.
        assert_eq!(vdf::parse(&vdf::serialize(&root)).unwrap(), root);
    }

    #[test]
    fn upsert_without_grid_dir_leaves_icon_empty_and_untouched() {
        let cfg = cfg("/bin/sm", "System Update", None);
        let (root, _, _) = upsert(empty_root(), &cfg);
        assert_eq!(str_of(our_entry(&root, &cfg), "icon"), "");
        let (root, _, changed) = upsert(root, &cfg);
        assert!(!changed);
        assert_eq!(str_of(our_entry(&root, &cfg), "icon"), "");
    }

    #[test]
    fn upsert_appends_after_existing_entries_and_preserves_them() {
        let cfg = default_cfg();
        let original = fixture("shortcuts-steam_rom_manager.vdf");
        let root = vdf::parse(&original).unwrap();
        let (new_root, appid, changed) = upsert(root.clone(), &cfg);
        assert!(changed);
        let old_entries = root.get("shortcuts").unwrap().entries().unwrap();
        let new_entries = new_root.get("shortcuts").unwrap().entries().unwrap();
        assert_eq!(new_entries.len(), old_entries.len() + 1);
        assert_eq!(&new_entries[..old_entries.len()], &old_entries[..]);
        assert_eq!(new_entries.last().unwrap().0, b"4");
        assert_eq!(i32_of(&new_entries[4].1, "appid") as u32, appid);
        // The old entries are still byte-identical in the serialized file.
        let bytes = vdf::serialize(&new_root);
        let prefix_len = original.len() - 2;
        assert_eq!(&bytes[..prefix_len], &original[..prefix_len]);
        assert_eq!(&bytes[bytes.len() - 2..], b"\x08\x08");
    }

    #[test]
    fn upsert_twice_is_a_no_op() {
        let cfg = default_cfg();
        let root = vdf::parse(&fixture("shortcuts-shortcuts.vdf")).unwrap();
        let (once, appid1, changed1) = upsert(root, &cfg);
        let (twice, appid2, changed2) = upsert(once.clone(), &cfg);
        assert!(changed1);
        assert!(!changed2);
        assert_eq!(appid1, appid2);
        assert_eq!(vdf::serialize(&once), vdf::serialize(&twice));
    }

    #[test]
    fn upsert_updates_a_matching_entry_in_place_keeping_its_appid_and_extras() {
        // Pretend the Cave Story+ entry is ours: same Exe, old name, foreign appid.
        let cfg = cfg("/home/philip/.local/bin/legendary", "System Update", None);
        let root = vdf::parse(&fixture("shortcuts-linux_shortcut.vdf")).unwrap();
        let fixture_appid = i32_of(our_entry(&root, &cfg), "appid");
        assert_ne!(
            fixture_appid as u32,
            shortcut_appid(&cfg.exe_quoted(), &cfg.app_name)
        );
        let (new_root, appid, changed) = upsert(root, &cfg);
        assert!(changed);
        assert_eq!(appid, fixture_appid as u32);
        let shortcuts = new_root.get("shortcuts").unwrap().entries().unwrap();
        assert_eq!(shortcuts.len(), 1, "updated in place, not appended");
        let entry = &shortcuts[0].1;
        assert_eq!(i32_of(entry, "appid"), fixture_appid);
        assert_eq!(str_of(entry, "AppName"), "System Update");
        assert_eq!(str_of(entry, "StartDir"), "/");
        assert_eq!(str_of(entry, "LaunchOptions"), "");
        // Fields we do not own survive, with their original spelling.
        assert_eq!(
            str_of(entry, "ShortcutPath"),
            "/usr/share/applications/blender.desktop"
        );
        assert_eq!(i32_of(entry, "LastPlayTime"), 0x611a_d186);
        let keys: Vec<&[u8]> = entry
            .entries()
            .unwrap()
            .iter()
            .map(|(k, _)| k.as_slice())
            .collect();
        assert!(keys.contains(&&b"openvr"[..]), "lowercase openvr kept");
        assert!(!keys.contains(&&b"OpenVR"[..]));
        assert_eq!(i32_of(entry, "OpenVR"), 0);
        assert!(
            keys.contains(&&b"DevkitOverrideAppID"[..]),
            "missing field added"
        );
        assert!(
            !keys.contains(&&b"FlatpakAppID"[..]),
            "absent field not invented"
        );
    }

    #[test]
    fn upsert_falls_back_to_matching_by_app_name() {
        let cfg = cfg(
            "/run/current-system/sw/bin/steam-maintenance",
            "Spore",
            None,
        );
        let root = vdf::parse(&fixture("shortcuts-shortcutsspore.vdf")).unwrap();
        let (new_root, appid, _) = upsert(root, &cfg);
        let shortcuts = new_root.get("shortcuts").unwrap().entries().unwrap();
        assert_eq!(shortcuts.len(), 1);
        let entry = &shortcuts[0].1;
        assert_eq!(i32_of(entry, "appid") as u32, appid);
        assert_eq!(appid, 0xF768_BA3B, "fixture appid kept");
        assert_eq!(str_of(entry, "Exe"), cfg.exe_quoted());
        assert_eq!(str_of(entry, "appname"), "Spore");
    }

    #[test]
    fn next_free_key_is_one_past_the_largest() {
        let gaps = Value::Map(vec![
            (b"0".to_vec(), Value::map()),
            (b"7".to_vec(), Value::map()),
            (b"x".to_vec(), Value::map()),
        ]);
        assert_eq!(next_free_key(&gaps), "8");
        assert_eq!(next_free_key(&Value::map()), "0");
    }

    #[test]
    fn register_file_creates_then_leaves_unchanged_and_backs_up_before_updating() {
        let cfg = default_cfg();
        let dir = temp_dir("file");
        let path = dir.join(SHORTCUTS_FILE);
        let bak = dir.join(BACKUP_FILE);

        let (appid, outcome) = register_file(&path, &cfg).unwrap();
        assert_eq!(outcome, Outcome::Created);
        assert!(!bak.exists());
        let created = fs::read(&path).unwrap();
        assert_eq!(vdf::parse(&created).unwrap(), upsert(empty_root(), &cfg).0);

        let (appid2, outcome) = register_file(&path, &cfg).unwrap();
        assert_eq!(outcome, Outcome::Unchanged);
        assert_eq!(appid, appid2);
        assert_eq!(fs::read(&path).unwrap(), created);
        assert!(!bak.exists(), "unchanged files are not backed up");

        let original = fixture("shortcuts-shortcuts2.vdf");
        fs::write(&path, &original).unwrap();
        let (_, outcome) = register_file(&path, &cfg).unwrap();
        assert_eq!(outcome, Outcome::Updated);
        assert_eq!(fs::read(&bak).unwrap(), original);
        let updated = fs::read(&path).unwrap();
        assert_ne!(updated, original);
        assert_eq!(register_file(&path, &cfg).unwrap().1, Outcome::Unchanged);
        assert_eq!(fs::read(&path).unwrap(), updated);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn register_file_refuses_a_corrupt_file() {
        let dir = temp_dir("corrupt");
        let path = dir.join(SHORTCUTS_FILE);
        let mut bytes = fixture("shortcuts-linux_shortcut.vdf");
        bytes.truncate(bytes.len() - 3);
        fs::write(&path, &bytes).unwrap();
        assert!(register_file(&path, &default_cfg()).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes, "left untouched");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn grid_art_is_copied_only_where_missing() {
        let dir = temp_dir("grid");
        let src = dir.join("src");
        let dest = dir.join("config/grid");
        fs::create_dir_all(&src).unwrap();
        for (name, _) in GRID_ART {
            fs::write(src.join(name), name).unwrap();
        }
        fs::create_dir_all(&dest).unwrap();
        fs::write(dest.join("42_hero.png"), "user art").unwrap();

        copy_grid_art(&src, &dest, 42).unwrap();
        assert_eq!(fs::read(dest.join("42p.png")).unwrap(), b"portrait.png");
        assert_eq!(fs::read(dest.join("42.png")).unwrap(), b"landscape.png");
        assert_eq!(fs::read(dest.join("42_logo.png")).unwrap(), b"logo.png");
        assert_eq!(fs::read(dest.join("42_hero.png")).unwrap(), b"user art");

        let fresh = dir.join("fresh/grid");
        fs::remove_file(src.join("hero.png")).unwrap();
        copy_grid_art(&src, &fresh, 7).unwrap();
        assert!(fresh.join("7p.png").is_file());
        assert!(
            !fresh.join("7_hero.png").exists(),
            "missing sources are skipped"
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn user_ids_are_numeric_and_non_zero() {
        assert!(is_user_id(Path::new("/x/userdata/10342635")));
        assert!(!is_user_id(Path::new("/x/userdata/0")));
        assert!(!is_user_id(Path::new("/x/userdata/anonymous")));
    }
}
