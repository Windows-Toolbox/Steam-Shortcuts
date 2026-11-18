//! The desktop: the shortcuts on it (`.lnk` and `.url`), pointing one at Steam and putting it back.
//!
//! A `.lnk` file is read directly (Microsoft's format MS-SHLLINK): its target, arguments, working folder and icon.
//! Pointing it at Steam moves the original into `data\desktop-backup\` and puts a `.url` with
//! `steam://rungameid/…` in its place – same name, same icon. Putting it back reverses exactly that.

use std::path::{Path, PathBuf};

use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{FOLDERID_Desktop, FOLDERID_PublicDesktop, SHGetKnownFolderPath};

/// A shortcut found on a desktop.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Found {
    pub path: PathBuf,
    /// The file name without `.lnk` / `.url` – what the desktop shows.
    pub name: String,
    /// A program's path or an address (`uplay://…`).
    pub target: String,
    pub args: String,
    pub start_dir: String,
    pub icon: String,
    /// On the desktop of all users – changing it needs administrator rights.
    pub public: bool,
}

fn known_folder(id: &windows_sys::core::GUID) -> Option<PathBuf> {
    let mut out = std::ptr::null_mut();
    let r = unsafe { SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut out) };
    if r != 0 || out.is_null() {
        return None;
    }
    let len = (0..).take_while(|&i| unsafe { *out.add(i) } != 0).count();
    let path = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(out, len) });
    unsafe { CoTaskMemFree(out.cast()) };
    Some(PathBuf::from(path))
}

/// The user's desktop.
pub fn own_desktop() -> Option<PathBuf> {
    known_folder(&FOLDERID_Desktop)
}

/// Every shortcut on the user's desktop and on the desktop of all users, sorted by name. Shortcuts that already
/// start something through Steam are left out.
pub fn scan() -> Vec<Found> {
    let mut out = vec![];
    for (folder, public) in [(known_folder(&FOLDERID_Desktop), false), (known_folder(&FOLDERID_PublicDesktop), true)] {
        let Some(folder) = folder else { continue };
        let Ok(entries) = std::fs::read_dir(&folder) else { continue };
        for path in entries.filter_map(|e| e.ok().map(|e| e.path())) {
            if let Some(found) = read(&path)
                && !found.target.to_lowercase().starts_with("steam://") && !found.target.to_lowercase().ends_with("steam.exe") {
                    out.push(Found { public, ..found });
                }
        }
    }
    out.sort_by_key(|f| f.name.to_lowercase());
    out
}

/// A `.lnk` or `.url` file – `None` for anything else or a shortcut without a target (a shortcut to a Store app,
/// to "This PC", …).
pub fn read(path: &Path) -> Option<Found> {
    let ext = path.extension()?.to_string_lossy().to_lowercase();
    let name = path.file_stem()?.to_string_lossy().into_owned();
    let mut found = match ext.as_str() {
        "lnk" => parse_lnk(&std::fs::read(path).ok()?)?,
        "url" => parse_url(&std::fs::read_to_string(path).ok()?)?,
        _ => return None,
    };
    found.path = path.to_path_buf();
    found.name = name;
    Some(found)
}

/// `%ProgramFiles%\x` → `C:\Program Files\x`.
fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        let Some(len) = rest[start + 1..].find('%') else { break };
        let var = &rest[start + 1..start + 1 + len];
        out.push_str(&rest[..start]);
        match std::env::var(var) {
            Ok(v) if !var.is_empty() => out.push_str(&v),
            _ => out.push_str(&rest[start..start + len + 2]),
        }
        rest = &rest[start + len + 2..];
    }
    out.push_str(rest);
    out
}

fn u16_at(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}
fn ansi_at(d: &[u8], at: usize) -> Option<String> {
    let rest = d.get(at..)?;
    let len = rest.iter().position(|&b| b == 0)?;
    Some(String::from_utf8_lossy(&rest[..len]).into_owned())
}
fn wide_at(d: &[u8], at: usize) -> Option<String> {
    let units: Vec<u16> = d.get(at..)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
    Some(String::from_utf16_lossy(&units))
}

const HAS_ID_LIST: u32 = 0x1;
const HAS_LINK_INFO: u32 = 0x2;
const HAS_NAME: u32 = 0x4;
const HAS_RELATIVE_PATH: u32 = 0x8;
const HAS_WORKING_DIR: u32 = 0x10;
const HAS_ARGUMENTS: u32 = 0x20;
const HAS_ICON: u32 = 0x40;
const IS_UNICODE: u32 = 0x80;
const ENVIRONMENT_BLOCK: u32 = 0xa000_0001;

/// The parts of a `.lnk` file this program needs.
pub fn parse_lnk(d: &[u8]) -> Option<Found> {
    if u32_at(d, 0)? != 0x4c {
        return None;
    }
    let flags = u32_at(d, 0x14)?;
    let mut at = 0x4c;
    if flags & HAS_ID_LIST != 0 {
        at += 2 + u16_at(d, at)? as usize;
    }
    let mut target = String::new();
    if flags & HAS_LINK_INFO != 0 {
        let size = u32_at(d, at)? as usize;
        let header = u32_at(d, at + 4)? as usize;
        let info_flags = u32_at(d, at + 8)?;
        if info_flags & 1 != 0 {
            target = if header >= 0x24 && u32_at(d, at + 28)? != 0 { wide_at(d, at + u32_at(d, at + 28)? as usize)? } else { ansi_at(d, at + u32_at(d, at + 16)? as usize)? };
            let suffix = ansi_at(d, at + u32_at(d, at + 24)? as usize).unwrap_or_default();
            if !suffix.is_empty() {
                target = Path::new(&target).join(suffix).to_string_lossy().into_owned();
            }
        }
        at += size;
    }
    let unicode = flags & IS_UNICODE != 0;
    let mut string = |present: bool| -> Option<String> {
        if !present {
            return Some(String::new());
        }
        let count = u16_at(d, at)? as usize;
        at += 2;
        let s = if unicode {
            let units: Vec<u16> = d.get(at..at + count * 2)?.as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            at += count * 2;
            String::from_utf16_lossy(&units)
        } else {
            let s = String::from_utf8_lossy(d.get(at..at + count)?).into_owned();
            at += count;
            s
        };
        Some(s)
    };
    let _name = string(flags & HAS_NAME != 0)?;
    let _relative = string(flags & HAS_RELATIVE_PATH != 0)?;
    let start_dir = string(flags & HAS_WORKING_DIR != 0)?;
    let args = string(flags & HAS_ARGUMENTS != 0)?;
    let icon = string(flags & HAS_ICON != 0)?;
    // A target written with variables (%ProgramFiles%) sits in a block after the strings.
    if target.is_empty() {
        while let Some(size) = u32_at(d, at).map(|s| s as usize).filter(|&s| s >= 8) {
            if u32_at(d, at + 4)? == ENVIRONMENT_BLOCK {
                target = wide_at(d, at + 8 + 260).filter(|t| !t.is_empty()).or_else(|| ansi_at(d, at + 8))?;
                break;
            }
            at += size;
        }
    }
    if target.is_empty() {
        return None;
    }
    Some(Found { target: expand(&target), args, start_dir: expand(&start_dir), icon: expand(&icon), ..Found::default() })
}

/// `[InternetShortcut]` with `URL=` (and the icon).
pub fn parse_url(text: &str) -> Option<Found> {
    let value = |key: &str| text.lines().find_map(|l| l.trim().strip_prefix(key).and_then(|r| r.strip_prefix('=')).map(|v| v.trim().to_string()));
    let target = value("URL").filter(|u| !u.is_empty())?;
    let icon = value("IconFile").unwrap_or_default();
    Some(Found { target, icon, ..Found::default() })
}

/// The text of a `.url` that opens `url` with this icon.
fn url_file(url: &str, icon: &str) -> String {
    let mut s = format!("[InternetShortcut]\r\nURL={url}\r\n");
    if !icon.is_empty() {
        // "C:\x\game.exe,0" from a .lnk: the file and the index separately.
        let (file, index) = icon.rsplit_once(',').filter(|(_, i)| i.trim().parse::<i32>().is_ok()).unwrap_or((icon, "0"));
        s.push_str(&format!("IconFile={}\r\nIconIndex={}\r\n", file.trim(), index.trim()));
    }
    s
}

fn why(e: std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::PermissionDenied { "access denied".into() } else { e.to_string() }
}

/// Puts a `.url` that opens `url` where `original` is, and keeps the original in `backup` (a folder of its own per
/// entry). Returns the new file.
pub fn replace(original: &Path, url: &str, icon: &str, backup: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(backup).map_err(why)?;
    let kept = backup.join(original.file_name().ok_or("no file name")?);
    // Moving first: a desktop of all users refuses here, before anything changed.
    std::fs::rename(original, &kept).or_else(|_| std::fs::copy(original, &kept).and_then(|_| std::fs::remove_file(original))).map_err(why)?;
    let new = original.with_extension("url");
    if let Err(e) = std::fs::write(&new, url_file(url, icon)) {
        let _ = std::fs::rename(&kept, original);
        return Err(why(e));
    }
    Ok(new)
}

/// The reverse of `replace`: our `.url` goes, the original comes back.
pub fn restore(new: &Path, kept: &Path, original: &Path) -> Result<(), String> {
    if !kept.exists() {
        return Err(format!("the kept original is missing: {}", kept.display()));
    }
    if new.exists() && std::fs::read_to_string(new).is_ok_and(|t| t.contains("steam://rungameid/")) {
        std::fs::remove_file(new).map_err(why)?;
    }
    std::fs::rename(kept, original).or_else(|_| std::fs::copy(kept, original).and_then(|_| std::fs::remove_file(kept))).map_err(why)
}

/// A new `.url` on the user's desktop. Refused if a file of that name is there.
pub fn create(name: &str, url: &str, icon: &str) -> Result<PathBuf, String> {
    let clean: String = name.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { ' ' } else { c }).collect();
    let path = own_desktop().ok_or("no desktop")?.join(format!("{}.url", clean.trim()));
    if path.exists() {
        return Err(format!("{} exists already", path.display()));
    }
    std::fs::write(&path, url_file(url, icon)).map_err(why)?;
    Ok(path)
}

/// Removes a `.url` this program made (and nothing else).
pub fn delete(path: &Path) -> Result<(), String> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.contains("steam://rungameid/") => std::fs::remove_file(path).map_err(why),
        Ok(_) => Err("not a shortcut of this program".into()),
        Err(_) => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u8> {
        let mut out = (s.encode_utf16().count() as u16).to_le_bytes().to_vec();
        out.extend(s.encode_utf16().flat_map(|u| u.to_le_bytes()));
        out
    }

    /// A `.lnk` as Windows writes it for a program: header, an (empty) item list, link info with the local path,
    /// working folder, arguments and icon in UTF-16.
    fn lnk(path: &str, args: &str) -> Vec<u8> {
        let mut d = vec![0u8; 0x4c];
        d[0] = 0x4c;
        let flags = HAS_ID_LIST | HAS_LINK_INFO | HAS_WORKING_DIR | HAS_ARGUMENTS | HAS_ICON | IS_UNICODE;
        d[0x14..0x18].copy_from_slice(&flags.to_le_bytes());
        d.extend_from_slice(&2u16.to_le_bytes());
        d.extend_from_slice(&[0, 0]); // the item list: only its end
        // Link info: header of 0x1c bytes, then the path, then an empty suffix.
        let base = 0x1c;
        let info_size = base + path.len() + 1 + 1;
        for v in [info_size as u32, 0x1c, 1, 0, base as u32, 0, (base + path.len() + 1) as u32] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d.extend_from_slice(path.as_bytes());
        d.extend_from_slice(&[0, 0]);
        d.extend(utf16(r"C:\Games\G"));
        d.extend(utf16(args));
        d.extend(utf16(r"%SystemRoot%\icon.ico,3"));
        d.extend_from_slice(&0u32.to_le_bytes()); // end of the extra blocks
        d
    }

    #[test]
    fn a_lnk_is_read() {
        let f = parse_lnk(&lnk(r"C:\Games\G\game.exe", "-windowed -nolauncher")).unwrap();
        assert_eq!(f.target, r"C:\Games\G\game.exe");
        assert_eq!(f.args, "-windowed -nolauncher");
        assert_eq!(f.start_dir, r"C:\Games\G");
        let root = std::env::var("SystemRoot").unwrap();
        assert_eq!(f.icon, format!(r"{root}\icon.ico,3"), "variables are expanded");
        assert_eq!(parse_lnk(b"nonsense"), None);
        let mut cut = lnk("C:\\x.exe", "");
        cut.truncate(0x60);
        assert_eq!(parse_lnk(&cut), None, "a broken file is no shortcut");
    }

    #[test]
    fn a_url_is_read() {
        let f = parse_url("[{000214A0-0000-0000-C000-000000000046}]\r\nProp3=19,0\r\n[InternetShortcut]\r\nIDList=\r\nURL=uplay://launch/410/0\r\nIconFile=C:\\x\\rayman.ico\r\nIconIndex=0\r\n").unwrap();
        assert_eq!((f.target.as_str(), f.icon.as_str()), ("uplay://launch/410/0", r"C:\x\rayman.ico"));
        assert_eq!(parse_url("[InternetShortcut]\r\nURL=\r\n"), None);
        assert_eq!(url_file("steam://rungameid/1", r"C:\g.exe,2"), "[InternetShortcut]\r\nURL=steam://rungameid/1\r\nIconFile=C:\\g.exe\r\nIconIndex=2\r\n");
        assert_eq!(url_file("steam://rungameid/1", r"C:\g.ico"), "[InternetShortcut]\r\nURL=steam://rungameid/1\r\nIconFile=C:\\g.ico\r\nIconIndex=0\r\n");
    }

    #[test]
    fn replace_and_put_back() {
        let dir = std::env::temp_dir().join(format!("steam-shortcuts-desktop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let original = dir.join("Game.lnk");
        let bytes = lnk(r"C:\Games\G\game.exe", "");
        std::fs::write(&original, &bytes).unwrap();
        let backup = dir.join("backup").join("game");

        let new = replace(&original, "steam://rungameid/42", r"C:\Games\G\game.exe,0", &backup).unwrap();
        assert_eq!(new, dir.join("Game.url"));
        assert!(!original.exists());
        assert_eq!(read(&new).unwrap().target, "steam://rungameid/42");
        let kept = backup.join("Game.lnk");
        assert_eq!(std::fs::read(&kept).unwrap(), bytes);

        restore(&new, &kept, &original).unwrap();
        assert!(!new.exists() && !kept.exists());
        assert_eq!(std::fs::read(&original).unwrap(), bytes, "the very same file is back");
        assert!(restore(&new, &kept, &original).is_err(), "nothing kept, nothing to put back");

        // A .url of someone else is never deleted.
        let other = dir.join("Other.url");
        std::fs::write(&other, "[InternetShortcut]\r\nURL=https://example.com\r\n").unwrap();
        assert!(delete(&other).is_err() && other.exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Prints the shortcuts on this PC's desktops – nothing is changed.
    #[test]
    #[ignore]
    fn desktops_of_this_pc() {
        for f in scan() {
            println!("{:<40} {} {} | {}", f.name, if f.public { "(all users)" } else { "" }, f.target, f.args);
        }
    }
}
