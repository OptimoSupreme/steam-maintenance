//! Keep the "System Update" non-Steam shortcut in every Steam user's shortcuts.vdf.
//!
//! Steam rewrites shortcuts.vdf when it exits, so this runs while Steam is down
//! (the NixOS module calls it at session start, before Steam, as the Steam user).
//! It upserts one entry per userdata directory, seeds the grid art Steam shows for
//! that entry when the entry is new, and remembers the app id of the most recently
//! used account so `check` can launch the shortcut by `steam://rungameid/`.

use crate::config::Config;
use crate::state::State;
use crate::util::{atomic_write, home_dir};
use crate::vdf::{self, Value};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

const SHORTCUTS_FILE: &str = "shortcuts.vdf";
const BACKUP_FILE: &str = "shortcuts.vdf.bak";
const GRID_DIR: &str = "grid";
/// Steam library roots relative to `$HOME`.
const STEAM_ROOTS: [&str; 2] = [".steam/steam", ".local/share/Steam"];
/// Which account used Steam last, relative to a Steam root.
const LOGIN_USERS_FILE: &str = "config/loginusers.vdf";
/// steamid64 of account id 0; userdata directories are named by the account id.
const STEAMID64_BASE: u64 = 76_561_197_960_265_728;
/// `(source name in grid_dir, suffix after the app id in config/grid)`.
const GRID_ART: [(&str, &str); 4] = [
    ("portrait.png", "p.png"),
    ("landscape.png", ".png"),
    ("hero.png", "_hero.png"),
    ("logo.png", LOGO_SUFFIX),
];
/// The copied logo doubles as the shortcut icon: unlike a store path it survives rebuilds.
const LOGO_SUFFIX: &str = "_logo.png";
const LOGO_SOURCE: &str = "logo.png";

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

/// The result of folding the shortcut into a parsed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upsert {
    pub root: Value,
    pub appid: u32,
    /// The tree differs from what was parsed.
    pub changed: bool,
    /// The entry is new or its icon was re-pointed, so the grid art should be seeded.
    pub seed_art: bool,
}

pub fn run(cfg: &Config) -> Result<()> {
    if fs::metadata("/proc/self")?.uid() == 0 {
        bail!(
            "register must run as the Steam user, not root: the gamescope pre-start hook \
             does that at session start, so do not run it under sudo"
        );
    }
    let Some(home) = home_dir() else {
        bail!("HOME is not set");
    };
    let dirs = userdata_config_dirs(&home);
    if dirs.is_empty() {
        println!("no Steam userdata found; start Steam once, then register again");
        return Ok(());
    }
    let preferred = most_recent_account(&home)
        .and_then(|account| dirs.iter().position(|d| account_id_of(d) == Some(account)));

    let mut appids: Vec<Option<u32>> = Vec::new();
    for dir in &dirs {
        let path = dir.join(SHORTCUTS_FILE);
        match register_dir(dir, cfg) {
            Ok((appid, outcome)) => {
                println!("{} {}", outcome.label(), path.display());
                appids.push(Some(appid));
            }
            Err(err) => {
                eprintln!("register: {}: {err:#}", path.display());
                appids.push(None);
            }
        }
    }
    let saved = preferred
        .and_then(|i| appids[i])
        .or_else(|| appids.iter().flatten().next().copied());
    let Some(appid) = saved else {
        bail!("no shortcuts.vdf could be updated");
    };
    let mut state = State::load();
    state.shortcut_appid = Some(appid);
    state.save().context("saving state")?;
    Ok(())
}

/// `userdata/<id>/config` under every Steam root that exists, deduplicated by
/// canonical path (`~/.steam/steam` is usually a symlink to `~/.local/share/Steam`).
fn userdata_config_dirs(home: &Path) -> Vec<PathBuf> {
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
    account_id_of_user_dir(path).is_some_and(|id| id != 0)
}

fn account_id_of_user_dir(path: &Path) -> Option<u64> {
    path.file_name()?.to_str()?.parse().ok()
}

/// The account id a `userdata/<id>/config` directory belongs to.
fn account_id_of(config_dir: &Path) -> Option<u64> {
    account_id_of_user_dir(config_dir.parent()?)
}

/// The account marked `MostRecent` in the first loginusers.vdf found, as an account id.
fn most_recent_account(home: &Path) -> Option<u64> {
    STEAM_ROOTS.iter().find_map(|root| {
        let text = fs::read_to_string(home.join(root).join(LOGIN_USERS_FILE)).ok()?;
        most_recent_steamid(&text)?.checked_sub(STEAMID64_BASE)
    })
}

/// Upsert the shortcut in one user's config directory, seeding the grid art for a new
/// entry and backing the file up before changing it.
fn register_dir(config: &Path, cfg: &Config) -> Result<(u32, Outcome)> {
    let path = config.join(SHORTCUTS_FILE);
    let grid = config.join(GRID_DIR);
    let old = match fs::read(&path) {
        Ok(bytes) if bytes.is_empty() => None,
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).context("reading"),
    };
    let root = match &old {
        Some(bytes) => vdf::parse(bytes).context("parsing")?,
        None => empty_root(),
    };
    let have_logo = cfg
        .grid_dir
        .as_ref()
        .is_some_and(|d| d.join(LOGO_SOURCE).is_file());
    let up = upsert(root, cfg, have_logo.then_some(grid.as_path()));
    if up.seed_art {
        if let Some(source) = &cfg.grid_dir {
            copy_grid_art(source, &grid, up.appid).context("copying grid art")?;
        }
    }
    let bytes = vdf::serialize(&up.root);
    if !up.changed && old.as_deref() == Some(bytes.as_slice()) {
        return Ok((up.appid, Outcome::Unchanged));
    }
    if old.is_some() {
        fs::copy(&path, path.with_file_name(BACKUP_FILE)).context("backing up")?;
    }
    atomic_write(&path, &bytes)?;
    let outcome = if old.is_some() {
        Outcome::Updated
    } else {
        Outcome::Created
    };
    Ok((up.appid, outcome))
}

/// The file Steam writes before any shortcut exists.
fn empty_root() -> Value {
    Value::Map(vec![(b"shortcuts".to_vec(), Value::map())])
}

/// Make the parsed file hold the shortcut: update the entry whose `Exe` matches,
/// otherwise append one. `icon_dir` is the user's config/grid directory when a logo
/// is available to point the icon at.
pub fn upsert(mut root: Value, cfg: &Config, icon_dir: Option<&Path>) -> Upsert {
    let before = root.clone();
    if root.get("shortcuts").and_then(Value::entries).is_none() {
        root.insert("shortcuts", Value::map());
    }
    let shortcuts = root.get_mut("shortcuts").expect("just ensured");
    let (appid, seed_art) = match find_entry(shortcuts, cfg) {
        Some(index) => {
            let entry = &mut shortcuts.entries_mut().expect("is a map")[index].1;
            let appid = entry
                .get("appid")
                .and_then(Value::as_i32)
                .map(|n| n as u32)
                .unwrap_or_else(|| shortcut_appid(&cfg.exe_quoted(), &cfg.app_name));
            let icon = icon_path(icon_dir, appid);
            let repointed = entry.get("icon").and_then(Value::as_bytes) != Some(icon.as_bytes());
            update_entry(entry, cfg, appid, &icon);
            (appid, repointed)
        }
        None => {
            let appid = shortcut_appid(&cfg.exe_quoted(), &cfg.app_name);
            let key = next_free_key(shortcuts);
            let entry = new_entry(cfg, appid, &icon_path(icon_dir, appid));
            shortcuts
                .entries_mut()
                .expect("is a map")
                .push((key.into_bytes(), entry));
            (appid, true)
        }
    };
    let changed = root != before;
    Upsert {
        root,
        appid,
        changed,
        seed_art,
    }
}

/// Index of our entry in the shortcuts map: the one whose `Exe` is the quoted exe.
/// Names are not matched, so a user's own shortcut called the same is left alone.
fn find_entry(shortcuts: &Value, cfg: &Config) -> Option<usize> {
    let exe = cfg.exe_quoted();
    shortcuts.entries()?.iter().position(|(_, entry)| {
        entry
            .get("Exe")
            .and_then(Value::as_bytes)
            .is_some_and(|v| v == exe.as_bytes())
    })
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

/// `<icon_dir>/<appid>_logo.png`, or empty when there is no logo to show.
fn icon_path(icon_dir: Option<&Path>, appid: u32) -> String {
    icon_dir
        .map(|d| {
            d.join(format!("{appid}{LOGO_SUFFIX}"))
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default()
}

/// Overwrite the owned fields of an existing entry; everything else stays.
fn update_entry(entry: &mut Value, cfg: &Config, appid: u32, icon: &str) {
    for (key, value) in owned_fields(cfg) {
        entry.insert(key, value);
    }
    entry.insert("icon", Value::string(icon));
    if entry.get("appid").is_none() {
        entry.insert("appid", Value::int32(appid as i32));
    }
}

/// A complete entry with today's Steam key spellings and order.
fn new_entry(cfg: &Config, appid: u32, icon: &str) -> Value {
    let mut entry = Value::Map(vec![(b"appid".to_vec(), Value::int32(appid as i32))]);
    // icon and ShortcutPath sit between StartDir and LaunchOptions in Steam's order.
    let mut fields = owned_fields(cfg);
    fields.insert(3, ("icon", Value::string(icon)));
    fields.insert(4, ("ShortcutPath", Value::string("")));
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

// ---- loginusers.vdf ------------------------------------------------------

/// A token of text KeyValues: a brace or the contents of a quoted string.
#[derive(Debug, PartialEq, Eq)]
enum Token<'a> {
    Open,
    Close,
    Str(&'a str),
}

/// Tolerant scan of text KeyValues: quoted strings and braces, `//` comments dropped,
/// anything else skipped.
fn kv_tokens(text: &str) -> Vec<Token<'_>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => out.push(Token::Open),
            b'}' => out.push(Token::Close),
            b'"' => {
                let start = i + 1;
                let mut end = start;
                while end < bytes.len() && bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                let end = end.min(bytes.len());
                out.push(Token::Str(&text[start..end]));
                i = end;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The steamid64 whose block carries `"MostRecent" "1"` in loginusers.vdf.
fn most_recent_steamid(text: &str) -> Option<u64> {
    let mut depth = 0usize;
    let mut key: Option<&str> = None;
    let mut current: Option<u64> = None;
    for token in kv_tokens(text) {
        match token {
            Token::Open => {
                if depth == 1 {
                    current = key.and_then(|k| k.parse().ok());
                }
                depth += 1;
                key = None;
            }
            Token::Close => {
                depth = depth.saturating_sub(1);
                key = None;
            }
            Token::Str(s) => match key.take() {
                None => key = Some(s),
                Some("MostRecent") if depth == 2 && s == "1" => return current,
                Some(_) => {}
            },
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRID: &str = "/home/u/.local/share/Steam/userdata/100/config/grid";

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

    /// A grid source directory holding all four artwork files.
    fn grid_source(dir: &Path) -> PathBuf {
        let src = dir.join("src");
        fs::create_dir_all(&src).unwrap();
        for (name, _) in GRID_ART {
            fs::write(src.join(name), name).unwrap();
        }
        src
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
        let up = upsert(empty_root(), &cfg, Some(Path::new(GRID)));
        assert!(up.changed);
        assert!(up.seed_art);
        let appid = up.appid;
        assert_eq!(appid, shortcut_appid(&cfg.exe_quoted(), "System Update"));
        let shortcuts = up.root.get("shortcuts").unwrap().entries().unwrap();
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
        assert_eq!(str_of(entry, "icon"), format!("{GRID}/{appid}_logo.png"));
        assert_eq!(i32_of(entry, "AllowOverlay"), 1);
        assert_eq!(i32_of(entry, "OpenVR"), 0);
        assert_eq!(i32_of(entry, "LastPlayTime"), 0);
        let tags = entry.get("tags").unwrap();
        assert_eq!(str_of(tags, "0"), "favorite");
        // A file Steam can read back.
        assert_eq!(vdf::parse(&vdf::serialize(&up.root)).unwrap(), up.root);
    }

    #[test]
    fn upsert_without_logo_leaves_icon_empty_and_untouched() {
        let cfg = cfg("/bin/sm", "System Update", None);
        let up = upsert(empty_root(), &cfg, None);
        assert_eq!(str_of(our_entry(&up.root, &cfg), "icon"), "");
        let again = upsert(up.root, &cfg, None);
        assert!(!again.changed);
        assert!(!again.seed_art);
        assert_eq!(str_of(our_entry(&again.root, &cfg), "icon"), "");
    }

    #[test]
    fn upsert_appends_after_existing_entries_and_preserves_them() {
        let cfg = default_cfg();
        let original = fixture("shortcuts-steam_rom_manager.vdf");
        let root = vdf::parse(&original).unwrap();
        let up = upsert(root.clone(), &cfg, Some(Path::new(GRID)));
        assert!(up.changed);
        let old_entries = root.get("shortcuts").unwrap().entries().unwrap();
        let new_entries = up.root.get("shortcuts").unwrap().entries().unwrap();
        assert_eq!(new_entries.len(), old_entries.len() + 1);
        assert_eq!(&new_entries[..old_entries.len()], &old_entries[..]);
        assert_eq!(new_entries.last().unwrap().0, b"4");
        assert_eq!(i32_of(&new_entries[4].1, "appid") as u32, up.appid);
        // The old entries are still byte-identical in the serialized file.
        let bytes = vdf::serialize(&up.root);
        let prefix_len = original.len() - 2;
        assert_eq!(&bytes[..prefix_len], &original[..prefix_len]);
        assert_eq!(&bytes[bytes.len() - 2..], b"\x08\x08");
    }

    #[test]
    fn upsert_twice_is_a_no_op() {
        let cfg = default_cfg();
        let grid = Some(Path::new(GRID));
        let root = vdf::parse(&fixture("shortcuts-shortcuts.vdf")).unwrap();
        let once = upsert(root, &cfg, grid);
        let twice = upsert(once.root.clone(), &cfg, grid);
        assert!(once.changed);
        assert!(!twice.changed);
        assert!(!twice.seed_art);
        assert_eq!(once.appid, twice.appid);
        assert_eq!(vdf::serialize(&once.root), vdf::serialize(&twice.root));
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
        let up = upsert(root, &cfg, None);
        assert!(up.changed);
        assert_eq!(up.appid, fixture_appid as u32);
        let shortcuts = up.root.get("shortcuts").unwrap().entries().unwrap();
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
    fn upsert_repoints_the_icon_once_and_reseeds_art_only_then() {
        let cfg = default_cfg();
        let root = vdf::parse(&fixture("shortcuts-linux_shortcut.vdf")).unwrap();
        let without = upsert(root, &cfg, None);
        assert!(without.seed_art, "new entry");
        let pointed = upsert(without.root, &cfg, Some(Path::new(GRID)));
        assert!(pointed.changed);
        assert!(pointed.seed_art, "icon re-pointed");
        assert_eq!(
            str_of(our_entry(&pointed.root, &cfg), "icon"),
            format!("{GRID}/{}_logo.png", pointed.appid)
        );
        let settled = upsert(pointed.root, &cfg, Some(Path::new(GRID)));
        assert!(!settled.changed);
        assert!(!settled.seed_art);
    }

    #[test]
    fn upsert_never_matches_by_name_alone() {
        let cfg = cfg(
            "/run/current-system/sw/bin/steam-maintenance",
            "Spore",
            None,
        );
        let root = vdf::parse(&fixture("shortcuts-shortcutsspore.vdf")).unwrap();
        let up = upsert(root, &cfg, None);
        let shortcuts = up.root.get("shortcuts").unwrap().entries().unwrap();
        assert_eq!(
            shortcuts.len(),
            2,
            "the user's Spore shortcut is left alone"
        );
        assert_eq!(i32_of(&shortcuts[0].1, "appid") as u32, 0xF768_BA3B);
        assert_ne!(str_of(&shortcuts[0].1, "Exe"), cfg.exe_quoted());
        assert_eq!(i32_of(&shortcuts[1].1, "appid") as u32, up.appid);
        assert_eq!(str_of(&shortcuts[1].1, "Exe"), cfg.exe_quoted());
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
    fn register_dir_creates_then_leaves_unchanged_and_backs_up_before_updating() {
        let cfg = default_cfg();
        let dir = temp_dir("file");
        let path = dir.join(SHORTCUTS_FILE);
        let bak = dir.join(BACKUP_FILE);

        let (appid, outcome) = register_dir(&dir, &cfg).unwrap();
        assert_eq!(outcome, Outcome::Created);
        assert!(!bak.exists());
        let created = fs::read(&path).unwrap();
        assert_eq!(
            vdf::parse(&created).unwrap(),
            upsert(empty_root(), &cfg, None).root,
            "no logo in the store dir, so no icon"
        );

        let (appid2, outcome) = register_dir(&dir, &cfg).unwrap();
        assert_eq!(outcome, Outcome::Unchanged);
        assert_eq!(appid, appid2);
        assert_eq!(fs::read(&path).unwrap(), created);
        assert!(!bak.exists(), "unchanged files are not backed up");

        let original = fixture("shortcuts-shortcuts2.vdf");
        fs::write(&path, &original).unwrap();
        let (_, outcome) = register_dir(&dir, &cfg).unwrap();
        assert_eq!(outcome, Outcome::Updated);
        assert_eq!(fs::read(&bak).unwrap(), original);
        let updated = fs::read(&path).unwrap();
        assert_ne!(updated, original);
        assert_eq!(register_dir(&dir, &cfg).unwrap().1, Outcome::Unchanged);
        assert_eq!(fs::read(&path).unwrap(), updated);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn register_dir_treats_an_empty_file_as_empty_shortcuts() {
        let cfg = default_cfg();
        let dir = temp_dir("empty");
        let path = dir.join(SHORTCUTS_FILE);
        fs::write(&path, b"").unwrap();
        let (_, outcome) = register_dir(&dir, &cfg).unwrap();
        assert_eq!(outcome, Outcome::Created);
        assert!(!dir.join(BACKUP_FILE).exists());
        assert_eq!(
            vdf::parse(&fs::read(&path).unwrap()).unwrap(),
            upsert(empty_root(), &cfg, None).root
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn register_dir_refuses_a_corrupt_file() {
        let dir = temp_dir("corrupt");
        let path = dir.join(SHORTCUTS_FILE);
        let mut bytes = fixture("shortcuts-linux_shortcut.vdf");
        bytes.truncate(bytes.len() - 3);
        fs::write(&path, &bytes).unwrap();
        assert!(register_dir(&dir, &default_cfg()).is_err());
        assert_eq!(fs::read(&path).unwrap(), bytes, "left untouched");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn register_dir_seeds_art_once_and_points_the_icon_at_the_copied_logo() {
        let dir = temp_dir("seed");
        let src = grid_source(&dir);
        let config = dir.join("userdata/100/config");
        fs::create_dir_all(&config).unwrap();
        let with_art = cfg("/bin/sm", "System Update", Some(src.to_str().unwrap()));

        let (appid, outcome) = register_dir(&config, &with_art).unwrap();
        assert_eq!(outcome, Outcome::Created);
        let logo = config.join(GRID_DIR).join(format!("{appid}_logo.png"));
        assert_eq!(fs::read(&logo).unwrap(), b"logo.png");
        assert!(config
            .join(GRID_DIR)
            .join(format!("{appid}p.png"))
            .is_file());
        let root = vdf::parse(&fs::read(config.join(SHORTCUTS_FILE)).unwrap()).unwrap();
        assert_eq!(
            str_of(our_entry(&root, &with_art), "icon"),
            logo.to_string_lossy()
        );

        // Art the user removed through Steam stays removed.
        fs::remove_file(config.join(GRID_DIR).join(format!("{appid}_hero.png"))).unwrap();
        assert_eq!(
            register_dir(&config, &with_art).unwrap().1,
            Outcome::Unchanged
        );
        assert!(!config
            .join(GRID_DIR)
            .join(format!("{appid}_hero.png"))
            .exists());

        // A rebuilt package (new store path) changes nothing in the file.
        let moved = dir.join("src2");
        fs::rename(&src, &moved).unwrap();
        let rebuilt = cfg("/bin/sm", "System Update", Some(moved.to_str().unwrap()));
        assert_eq!(
            register_dir(&config, &rebuilt).unwrap().1,
            Outcome::Unchanged
        );
        assert!(!config.join(BACKUP_FILE).exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn grid_art_is_copied_only_where_missing() {
        let dir = temp_dir("grid");
        let src = grid_source(&dir);
        let dest = dir.join("config/grid");
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
        assert_eq!(
            account_id_of(Path::new("/x/userdata/10342635/config")),
            Some(10342635)
        );
    }

    const LOGIN_USERS: &str = r#""users"
{
	"76561197970531363"
	{
		"AccountName"		"first"
		"PersonaName"		"First \"Quoted\" User"
		"RememberPassword"		"1"
		"MostRecent"		"0"
		"Timestamp"		"1700000000"
	}
	// a comment with "MostRecent" "1" in it
	"76561198012345678"
	{
		"AccountName"		"second"
		"PersonaName"		"Second"
		"MostRecent"		"1"
		"Timestamp"		"1700000001"
	}
}
"#;

    #[test]
    fn most_recent_account_comes_from_loginusers() {
        assert_eq!(most_recent_steamid(LOGIN_USERS), Some(76561198012345678));
        assert_eq!(
            76561198012345678 - STEAMID64_BASE,
            52_079_950,
            "account id is the steamid64 offset"
        );
        let none = LOGIN_USERS.replace("\"MostRecent\"\t\t\"1\"", "\"MostRecent\"\t\t\"0\"");
        assert_eq!(most_recent_steamid(&none), None);
        assert_eq!(most_recent_steamid(""), None);
        assert_eq!(
            most_recent_steamid("\"users\" { \"x\" { \"MostRecent\" \"1\""),
            None
        );
        assert_eq!(
            most_recent_steamid("\"MostRecent\" \"1\""),
            None,
            "top level"
        );

        let dir = temp_dir("login");
        let root = dir.join(".local/share/Steam");
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join(LOGIN_USERS_FILE), LOGIN_USERS).unwrap();
        assert_eq!(most_recent_account(&dir), Some(52_079_950));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn kv_tokens_are_tolerant() {
        assert_eq!(
            kv_tokens("\"a\" { \"b\" \"c\\\"d\" } junk \"unterminated"),
            vec![
                Token::Str("a"),
                Token::Open,
                Token::Str("b"),
                Token::Str("c\\\"d"),
                Token::Close,
                Token::Str("unterminated"),
            ]
        );
        assert_eq!(kv_tokens("\"é\\"), vec![Token::Str("é\\")]);
    }
}
