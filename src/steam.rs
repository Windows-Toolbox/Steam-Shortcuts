//! Steam itself: where it is, which account, whether it runs, quitting and starting it, and the list of non-Steam
//! games (`userdata\<account>\config\shortcuts.vdf`).
//!
//! Steam keeps that list in memory and writes it back when it quits – a change made while Steam runs is lost. So
//! the list is only written while Steam is closed, and a copy of the file as it was goes into `data\steam-backup\`
//! first (the last ten are kept).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows_sys::Win32::System::Registry::{HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW};

use crate::vdf::{self, Value};

/// SteamID64 = this + account id (the number of the folder in `userdata`).
const STEAM_ID_BASE: u64 = 76_561_197_960_265_728;

#[derive(Debug, Clone, PartialEq)]
pub struct Steam {
    pub dir: PathBuf,
    pub exe: PathBuf,
    /// The account whose list is used: the one signed in now, else the one signed in last.
    pub account: Option<u32>,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A text from the registry.
pub fn reg_text(root: HKEY, path: &str, name: &str) -> Option<String> {
    let (path, name) = (wide(path), wide(name));
    let mut buf = vec![0u16; 2048];
    let mut size = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(root, path.as_ptr(), name.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut size) };
    if r != 0 {
        return None;
    }
    let len = (size as usize / 2).saturating_sub(1);
    Some(String::from_utf16_lossy(&buf[..len.min(buf.len())]))
}

fn reg_number(root: HKEY, path: &str, name: &str) -> Option<u32> {
    let (path, name) = (wide(path), wide(name));
    let mut value = 0u32;
    let mut size = 4u32;
    let r = unsafe { RegGetValueW(root, path.as_ptr(), name.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut value as *mut u32).cast(), &mut size) };
    (r == 0).then_some(value)
}

/// Steam writes its paths with `/` and in lower case ("e:/software/steam").
fn windows_path(p: &str) -> PathBuf {
    PathBuf::from(p.replace('/', "\\"))
}

impl Steam {
    /// Steam as the registry knows it – `None` if it is not installed.
    pub fn find() -> Option<Steam> {
        let key = r"Software\Valve\Steam";
        let dir = windows_path(&reg_text(HKEY_CURRENT_USER, key, "SteamPath")?);
        let exe = reg_text(HKEY_CURRENT_USER, key, "SteamExe").map(|e| windows_path(&e)).unwrap_or_else(|| dir.join("steam.exe"));
        if !exe.exists() {
            return None;
        }
        let active = reg_number(HKEY_CURRENT_USER, r"Software\Valve\Steam\ActiveProcess", "ActiveUser").filter(|&a| a != 0);
        let account = active.or_else(|| {
            let text = std::fs::read_to_string(dir.join("config").join("loginusers.vdf")).ok()?;
            vdf::most_recent_user(&text).and_then(|id| u32::try_from(id.checked_sub(STEAM_ID_BASE)?).ok())
        });
        Some(Steam { dir, exe, account })
    }

    /// The list of non-Steam games of the account (it may not exist yet – Steam creates it with the first one).
    pub fn shortcuts_file(&self) -> Option<PathBuf> {
        let config = self.dir.join("userdata").join(self.account?.to_string()).join("config");
        config.is_dir().then(|| config.join("shortcuts.vdf"))
    }

    pub fn running(&self) -> bool {
        !kit::process::running(&self.exe).is_empty()
    }

    /// Asks Steam to quit and waits until it has (at most `timeout`). `true` = it is closed.
    pub fn quit(&self, timeout: Duration) -> bool {
        if !self.running() {
            return true;
        }
        let _ = kit::process::spawn(&self.exe, &["-shutdown"]);
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            std::thread::sleep(Duration::from_millis(500));
            if !self.running() {
                // Steam writes its files while it ends; a moment more so they are done.
                std::thread::sleep(Duration::from_secs(1));
                return true;
            }
        }
        false
    }

    pub fn start(&self) -> bool {
        kit::process::open(&self.exe.to_string_lossy())
    }
}

/// The program a web-like address opens (`uplay://…` → Ubisoft Connect): its exe (quoted) and the options to give
/// it, from `HKEY_CLASSES_ROOT\<scheme>\shell\open\command`.
pub fn handler(url: &str) -> Option<(String, String)> {
    let scheme = url.split_once(':')?.0;
    if scheme.len() < 2 || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
        return None;
    }
    let command = reg_text(HKEY_CLASSES_ROOT, &format!(r"{scheme}\shell\open\command"), "")?;
    Some(split_command(&command, url))
}

/// `"C:\x\Launcher.exe" --uri="%1"` → (`"C:\x\Launcher.exe"`, `--uri="<url>"`); without `%1` the address is added.
pub fn split_command(command: &str, url: &str) -> (String, String) {
    let command = command.trim();
    let (exe, rest) = match command.strip_prefix('"').and_then(|c| c.split_once('"')) {
        Some((exe, rest)) => (exe.to_string(), rest.trim()),
        None => match command.split_once(' ') {
            Some((exe, rest)) => (exe.to_string(), rest.trim()),
            None => (command.to_string(), ""),
        },
    };
    let options = if rest.contains("%1") { rest.replace("%1", url) } else { format!("{rest} {url}").trim().to_string() };
    (format!("\"{exe}\""), options)
}

/// One entry of the list, as this program writes it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Shortcut {
    pub appid: u32,
    pub name: String,
    /// Quoted, as Steam writes it.
    pub exe: String,
    pub start_dir: String,
    pub icon: String,
    pub options: String,
}

impl Shortcut {
    fn of(map: &[(String, Value)]) -> Option<Shortcut> {
        let text = |n: &str| match vdf::get(map, n) {
            Some(Value::Text(s)) => s.clone(),
            _ => String::new(),
        };
        let Some(Value::Int(appid)) = vdf::get(map, "appid") else { return None };
        Some(Shortcut { appid: *appid, name: text("AppName"), exe: text("Exe"), start_dir: text("StartDir"), icon: text("icon"), options: text("LaunchOptions") })
    }

    /// A new entry with every field Steam itself puts in.
    fn new_map(&self) -> Vec<(String, Value)> {
        let t = |s: &str| Value::Text(s.to_string());
        vec![
            ("appid".into(), Value::Int(self.appid)),
            ("AppName".into(), t(&self.name)),
            ("Exe".into(), t(&self.exe)),
            ("StartDir".into(), t(&self.start_dir)),
            ("icon".into(), t(&self.icon)),
            ("ShortcutPath".into(), t("")),
            ("LaunchOptions".into(), t(&self.options)),
            ("IsHidden".into(), Value::Int(0)),
            ("AllowDesktopConfig".into(), Value::Int(1)),
            ("AllowOverlay".into(), Value::Int(1)),
            ("OpenVR".into(), Value::Int(0)),
            ("Devkit".into(), Value::Int(0)),
            ("DevkitGameID".into(), t("")),
            ("DevkitOverrideAppID".into(), Value::Int(0)),
            ("LastPlayTime".into(), Value::Int(0)),
            ("FlatpakAppID".into(), t("")),
            ("sortas".into(), t("")),
            ("tags".into(), Value::Map(vec![])),
        ]
    }

    /// Changes only the fields this program sets – Steam's own (play time, tags, …) stay.
    fn update(&self, map: &mut Vec<(String, Value)>) {
        vdf::set(map, "AppName", Value::Text(self.name.clone()));
        vdf::set(map, "Exe", Value::Text(self.exe.clone()));
        vdf::set(map, "StartDir", Value::Text(self.start_dir.clone()));
        vdf::set(map, "icon", Value::Text(self.icon.clone()));
        vdf::set(map, "LaunchOptions", Value::Text(self.options.clone()));
    }
}

/// The number of an entry: as Steam made it for years – a checksum of exe and name with the top bit set. Stays the
/// entry's number afterwards, whatever is changed.
pub fn appid(exe: &str, name: &str) -> u32 {
    crc32(format!("{exe}{name}").as_bytes()) | 0x8000_0000
}

/// What starts the entry: `steam://rungameid/<this>`.
pub fn rungameid(appid: u32) -> u64 {
    ((appid as u64) << 32) | 0x0200_0000
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xedb8_8320 } else { crc >> 1 };
        }
    }
    !crc
}

/// The entries of the list (only those with a number).
pub fn read(file: &Path) -> Result<Vec<Shortcut>, String> {
    let data = match std::fs::read(file) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(e) => return Err(e.to_string()),
    };
    let (_, values) = vdf::read(&data)?;
    Ok(values.iter().filter_map(|(_, v)| if let Value::Map(m) = v { Shortcut::of(m) } else { None }).collect())
}

/// Brings the list in line: every entry of `want` is there with these fields (added or updated, by its number),
/// every number in `remove` is gone. Entries of others stay untouched. Steam must be closed. The file as it was is
/// copied into `backup` first.
pub fn apply(file: &Path, want: &[Shortcut], remove: &[u32], backup: &Path) -> Result<(), String> {
    let old = match std::fs::read(file) {
        Ok(d) => Some(d),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let (name, mut values) = match &old {
        Some(d) => vdf::read(d)?,
        None => ("shortcuts".to_string(), vec![]),
    };
    let number = |v: &Value| if let Value::Map(m) = v { if let Some(Value::Int(id)) = vdf::get(m, "appid") { Some(*id) } else { None } } else { None };
    values.retain(|(_, v)| number(v).is_none_or(|id| !remove.contains(&id)));
    for s in want {
        match values.iter_mut().find(|(_, v)| number(v) == Some(s.appid)) {
            Some((_, Value::Map(m))) => s.update(m),
            _ => values.push((String::new(), Value::Map(s.new_map()))),
        }
    }
    // Steam numbers the entries 0, 1, 2, … without gaps.
    for (i, (key, _)) in values.iter_mut().enumerate() {
        *key = i.to_string();
    }
    let new = vdf::write(&name, &values);
    if old.as_deref() == Some(new.as_slice()) {
        return Ok(());
    }
    if let Some(old) = &old {
        save_backup(backup, old).map_err(|e| format!("backup: {e}"))?;
    }
    let temp = file.with_extension("vdf.new");
    std::fs::write(&temp, &new).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, file).map_err(|e| e.to_string())
}

fn save_backup(dir: &Path, data: &[u8]) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    std::fs::write(dir.join(format!("shortcuts-{stamp}.vdf")), data)?;
    let mut old: Vec<PathBuf> = std::fs::read_dir(dir)?.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "vdf")).collect();
    old.sort();
    while old.len() > 10 {
        let _ = std::fs::remove_file(old.remove(0));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        // The checksum Steam used (and tools like Steam ROM Manager use): CRC-32 of exe + name.
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        let id = appid(r#""C:\Games\Game.exe""#, "Game");
        assert!(id & 0x8000_0000 != 0);
        assert_eq!(rungameid(0x8c71_67d2), 0x8c71_67d2_0200_0000);
        assert_eq!(rungameid(0x8c71_67d2).to_string(), "10119983989352562688");
    }

    #[test]
    fn commands_of_addresses() {
        let url = "uplay://launch/410/0";
        assert_eq!(split_command(r#""C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\UbisoftConnect.exe" "%1""#, url), (r#""C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\UbisoftConnect.exe""#.into(), format!("\"{url}\"")));
        assert_eq!(split_command(r#""E:\Battlenet\Battle.net.exe" --uri="%1""#, "battlenet://S2"), (r#""E:\Battlenet\Battle.net.exe""#.into(), r#"--uri="battlenet://S2""#.into()));
        assert_eq!(split_command(r"C:\x\epic.exe %1", "com.epicgames.launcher://apps/a"), (r#""C:\x\epic.exe""#.into(), "com.epicgames.launcher://apps/a".into()));
        assert_eq!(split_command(r#""C:\x\a.exe""#, "a://b"), (r#""C:\x\a.exe""#.into(), "a://b".into()));
        assert_eq!(handler("C:\\Games\\x.exe"), None, "a path is no address");
    }

    #[test]
    fn the_list_is_brought_in_line() {
        let dir = std::env::temp_dir().join(format!("steam-shortcuts-apply-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (file, backup) = (dir.join("shortcuts.vdf"), dir.join("backup"));
        // Someone else's entry (the Ubisoft Connect one, with a tag and play time) is there already.
        std::fs::write(&file, crate::vdf::tests::sample()).unwrap();
        let theirs = read(&file).unwrap();
        assert_eq!(theirs.len(), 1);

        let game = Shortcut { appid: appid("\"C:\\G\\g.exe\"", "G"), name: "G".into(), exe: "\"C:\\G\\g.exe\"".into(), start_dir: "\"C:\\G\\\"".into(), icon: String::new(), options: "-windowed".into() };
        apply(&file, std::slice::from_ref(&game), &[], &backup).unwrap();
        let now = read(&file).unwrap();
        assert_eq!(now, vec![theirs[0].clone(), game.clone()]);
        assert_eq!(std::fs::read_dir(&backup).unwrap().count(), 1, "the old file was kept");
        let (_, values) = vdf::read(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(values.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>(), ["0", "1"]);
        let Value::Map(first) = &values[0].1 else { panic!() };
        assert_eq!(vdf::get(first, "LastPlayTime"), Some(&Value::Int(0x6afc_d504)), "their fields untouched");

        // Changed: updated in place; the same again changes nothing (no new backup).
        let renamed = Shortcut { name: "Game".into(), ..game.clone() };
        apply(&file, std::slice::from_ref(&renamed), &[], &backup).unwrap();
        apply(&file, std::slice::from_ref(&renamed), &[], &backup).unwrap();
        assert_eq!(read(&file).unwrap()[1].name, "Game");
        assert_eq!(std::fs::read_dir(&backup).unwrap().count(), 2);

        // Removed: gone, numbering without gaps, theirs still there.
        apply(&file, &[], &[game.appid], &backup).unwrap();
        assert_eq!(read(&file).unwrap(), theirs);

        // No file yet: created.
        let fresh = dir.join("fresh.vdf");
        apply(&fresh, std::slice::from_ref(&game), &[], &backup).unwrap();
        assert_eq!(read(&fresh).unwrap(), vec![game]);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Prints what was found on this PC – nothing is changed.
    #[test]
    #[ignore]
    fn steam_on_this_pc() {
        let steam = Steam::find();
        println!("{steam:?}");
        if let Some(s) = steam {
            println!("running: {}, list: {:?}", s.running(), s.shortcuts_file());
            if let Some(f) = s.shortcuts_file() {
                println!("{:#?}", read(&f));
            }
        }
        println!("uplay: {:?}", handler("uplay://launch/410/0"));
    }
}
