//! Steam Shortcuts – adds programs and games to Steam, so the Steam Controller works in them (Steam Input only
//! reaches what Steam starts), and points desktop icons at Steam – and back.
//!
//! The files:
//!   main.rs      settings, the page, the buttons, the worker
//!   steam.rs     Steam itself: finding it, quitting and starting it, its list of non-Steam games
//!   vdf.rs       Steam's file formats
//!   desktop.rs   the shortcuts on the desktop: reading, pointing at Steam, putting back
//!
//! Everything else – the pipe to the Toolbox, loading and saving, the tray menu, single instance – is the kit's job
//! (Toolbox\DESIGN.md 5).
#![windows_subsystem = "windows"]

mod desktop;
mod icons;
mod steam;
mod vdf;

use std::path::{Path, PathBuf};
use std::time::Duration;

use kit::app::{self, Program, Runtime};
use kit::page::{Builder, Button, Item};
use kit::proto::{Health, MenuEntry, Status, Tray};
use kit::{process, t};
use serde::{Deserialize, Serialize};

use steam::{Shortcut, Steam};

/// One program or game.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Entry {
    /// Made when the entry is added; never changes.
    pub id: String,
    pub name: String,
    /// A program's path, or an address another program opens (`uplay://launch/410/0`).
    pub target: String,
    /// Launch parameters (`-windowed`).
    pub args: String,
    pub start_dir: String,
    pub icon: String,
    /// Wanted in Steam. What Steam's list really holds is looked up; the difference is what waits for Steam.
    pub steam: bool,
    /// Its number in Steam's list – made the first time it is added, kept afterwards.
    pub appid: u32,
    /// The desktop shortcut it came from.
    pub shortcut: String,
    /// The `.url` this program put on the desktop (instead of `shortcut`, or new).
    pub link: String,
    /// While `link` stands in for `shortcut`: where the original is kept.
    pub kept: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Default)]
#[serde(default)]
pub struct Settings {
    pub entries: Vec<Entry>,
}

/// Where an entry stands with Steam.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Standing {
    NotInSteam,
    InSteam,
    /// Wanted in Steam (or changed) – written when Steam is closed.
    WaitsAdd,
    /// Not wanted any more – removed when Steam is closed.
    WaitsRemove,
}

struct App {
    settings: Settings,
    running: bool,
    steam: Option<Steam>,
    steam_running: bool,
    /// Steam's list as last read.
    present: Vec<Shortcut>,
    /// "Restart Steam now" was pressed; the worker does it.
    restart: bool,
    restarting: bool,
    error: Option<String>,
    /// The desktop shortcuts not in the list yet – for "Add".
    found: Vec<desktop::Found>,
}

fn is_address(target: &str) -> bool {
    target.split_once("://").is_some_and(|(scheme, _)| scheme.len() > 1 && !scheme.contains(['\\', '/']))
}

fn quoted(s: &str) -> String {
    format!("\"{}\"", s.trim_matches('"'))
}

/// "C:\x\game.exe,0" → "C:\x\game.exe" – Steam wants a file.
fn icon_file(icon: &str) -> &str {
    icon.rsplit_once(',').filter(|(_, i)| i.trim().parse::<i32>().is_ok()).map_or(icon, |(f, _)| f).trim()
}

impl Entry {
    /// What Steam's list gets for this entry. An address goes to the program that opens it, with the address as
    /// option – Ubisoft Connect started through Steam passes the controller on to the game it starts.
    fn shortcut(&self) -> Shortcut {
        let target = self.target.trim();
        let (exe, start_dir, mut options) = if is_address(target) {
            match steam::handler(target) {
                Some((exe, options)) => {
                    let dir = Path::new(exe.trim_matches('"')).parent().map(|d| quoted(&format!("{}\\", d.display()))).unwrap_or_default();
                    (exe, dir, options)
                }
                None => (quoted(target), String::new(), String::new()),
            }
        } else {
            let dir = if self.start_dir.trim().is_empty() { Path::new(target).parent().map(|d| d.display().to_string()).unwrap_or_default() } else { self.start_dir.trim().to_string() };
            (quoted(target), if dir.is_empty() { dir } else { quoted(&format!("{}\\", dir.trim_end_matches('\\'))) }, String::new())
        };
        if !self.args.trim().is_empty() {
            options = format!("{options} {}", self.args.trim()).trim().to_string();
        }
        let icon = if self.icon.trim().is_empty() && !is_address(target) { target.to_string() } else { icon_file(&self.icon).to_string() };
        Shortcut { appid: self.appid, name: self.name.trim().to_string(), exe, start_dir, icon, options }
    }

    fn address(&self) -> String {
        format!("steam://rungameid/{}", steam::rungameid(self.appid))
    }

    fn title(&self) -> String {
        if self.name.trim().is_empty() { t!("entry.unnamed") } else { self.name.trim().to_string() }
    }
}

impl App {
    fn new() -> App {
        App { settings: Settings::default(), running: false, steam: Steam::find(), steam_running: false, present: vec![], restart: false, restarting: false, error: None, found: vec![] }
    }

    fn standing(&self, e: &Entry) -> Standing {
        let there = self.present.iter().find(|p| e.appid != 0 && p.appid == e.appid);
        match (e.steam, there) {
            (true, Some(p)) if *p == e.shortcut() => Standing::InSteam,
            (true, _) => Standing::WaitsAdd,
            (false, Some(_)) => Standing::WaitsRemove,
            (false, None) => Standing::NotInSteam,
        }
    }

    fn waiting<'a>(&self, s: &'a Settings) -> Vec<&'a Entry> {
        s.entries.iter().filter(|e| matches!(self.standing(e), Standing::WaitsAdd | Standing::WaitsRemove)).collect()
    }

    /// Looks at Steam: running? its list?
    fn look(&mut self) {
        let Some(steam) = &self.steam else { return };
        self.steam_running = steam.running();
        if let Some(file) = steam.shortcuts_file() {
            match steam::read(&file) {
                Ok(list) => self.present = list,
                Err(e) => self.error = Some(t!("error.read", error = e)),
            }
        }
    }

    /// Writes Steam's list so it holds what the entries want – only while Steam is closed.
    fn sync(&mut self, s: &Settings) {
        let Some(steam) = self.steam.clone() else { return };
        self.look();
        if self.steam_running || self.waiting(s).is_empty() {
            return;
        }
        let Some(file) = steam.shortcuts_file() else {
            self.error = Some(t!("error.no_account"));
            return;
        };
        let want: Vec<Shortcut> = s.entries.iter().filter(|e| e.steam && e.appid != 0).map(Entry::shortcut).collect();
        let remove: Vec<u32> = s.entries.iter().filter(|e| !e.steam && e.appid != 0).map(|e| e.appid).collect();
        match steam::apply(&file, &want, &remove, &app::data_dir().join("steam-backup")) {
            Ok(()) => {
                self.error = None;
                app::log(&format!("Steam's list written: {} of ours in it", want.len()));
            }
            Err(e) => {
                app::log(&format!("writing Steam's list failed: {e}"));
                self.error = Some(t!("error.write", error = e));
            }
        }
        self.look();
    }

    fn rescan(&mut self, s: &Settings) {
        self.found = desktop::scan().into_iter().filter(|f| !s.entries.iter().any(|e| Path::new(&e.shortcut) == f.path || Path::new(&e.link) == f.path)).collect();
    }
}

/// How long a replaced desktop icon may take to show up before its place is set.
const ICON_WAIT: Duration = Duration::from_secs(3);

fn new_id() -> String {
    let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("{n:x}")
}

impl Program for App {
    type Settings = Settings;
    const META: kit::Meta = kit::meta!();

    fn page(&self, p: &mut Builder, s: &Settings) {
        if !self.running {
            p.section(t!("page.status"), |p| p.text(t!("page.stopped")));
        }
        let Some(steam) = &self.steam else {
            p.status(t!("page.no_steam"), Health::Bad);
            return;
        };
        if let Some(e) = &self.error {
            p.status(e.clone(), Health::Bad);
        }
        let waiting = self.waiting(s);
        if self.restarting {
            p.status(t!("page.restarting"), Health::Warn);
        } else if !waiting.is_empty() {
            let names: Vec<String> = waiting.iter().map(|e| e.title()).collect();
            p.status(t!("page.waiting", names = names.join(", ")), Health::Warn);
            // Only the worker can restart Steam.
            if self.running && self.steam_running {
                p.buttons(|b| {
                    b.button(t!("page.restart"), "restart");
                });
            }
        }
        p.list("steam_shortcuts.entries", |l| {
            for (id, label) in [("waiting", t!("group.waiting")), ("in", t!("group.in")), ("not", t!("group.not"))] {
                l.group(id, label, true);
            }
            l.empty(t!("page.empty"));
            l.add(t!("page.add"), |p| {
                p.text(t!("page.add.text"));
                for f in &self.found {
                    let label = if f.public { t!("page.add.public", name = f.name) } else { t!("page.add.item", name = f.name) };
                    p.buttons(|b| {
                        b.button(label, &format!("add:{}", f.path.display()));
                    });
                }
                if self.found.is_empty() {
                    p.text(t!("page.add.none"));
                }
                p.buttons(|b| {
                    b.button(t!("page.add.own"), "new").button(t!("page.add.refresh"), "refresh");
                });
            });
            for (i, e) in s.entries.iter().enumerate() {
                let k = |field: &str| format!("entries.{i}.{field}");
                let standing = self.standing(e);
                let (group, state, text) = match standing {
                    Standing::InSteam => ("in", Health::Ok, t!("entry.in")),
                    Standing::NotInSteam => ("not", Health::Unknown, t!("entry.not")),
                    Standing::WaitsAdd if self.restarting => ("waiting", Health::Warn, t!("page.restarting")),
                    Standing::WaitsAdd => ("waiting", Health::Warn, t!("entry.waits_add")),
                    Standing::WaitsRemove => ("waiting", Health::Warn, t!("entry.waits_remove")),
                };
                let mut tags = vec![];
                if !e.kept.is_empty() {
                    tags.push(t!("tag.desktop"));
                }
                let item = Item {
                    id: e.id.clone(),
                    title: e.title(),
                    subtitle: e.target.clone(),
                    state,
                    text: text.clone(),
                    group: group.into(),
                    tags,
                    button: (standing == Standing::InSteam).then(|| Button { label: t!("entry.play"), action: format!("play:{}", e.id) }),
                    ..Item::default()
                };
                l.item(item, |p| {
                    p.status(text, if state == Health::Unknown { Health::Ok } else { state });
                    p.entry(t!("entry.name"), &k("name"));
                    p.entry(t!("entry.target"), &k("target"));
                    p.entry(t!("entry.args"), &k("args"));
                    let sc = e.shortcut();
                    if is_address(&e.target) {
                        match steam::handler(e.target.trim()) {
                            Some((exe, _)) => p.text(t!("entry.launcher", launcher = Path::new(exe.trim_matches('"')).file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())),
                            None => p.status(t!("entry.no_handler"), Health::Warn),
                        }
                    }
                    p.text(t!("entry.steam_starts", exe = sc.exe, options = sc.options));
                    // Steam.
                    p.buttons(|b| match standing {
                        Standing::NotInSteam if self.steam_running => {
                            b.button(t!("entry.add_now"), &format!("add_now:{}", e.id)).button(t!("entry.add_later"), &format!("add_later:{}", e.id));
                        }
                        Standing::NotInSteam => {
                            b.button(t!("entry.add"), &format!("add_later:{}", e.id));
                        }
                        Standing::WaitsAdd if e.appid != 0 && self.present.iter().any(|p| p.appid == e.appid) => {
                            b.button(t!("entry.unsteam"), &format!("unsteam:{}", e.id));
                        }
                        Standing::WaitsAdd => {
                            b.button(t!("entry.dont_add"), &format!("unsteam:{}", e.id));
                        }
                        Standing::WaitsRemove => {
                            b.button(t!("entry.keep"), &format!("add_later:{}", e.id));
                        }
                        Standing::InSteam => {
                            b.button(t!("entry.play"), &format!("play:{}", e.id)).button(t!("entry.copy"), &format!("copy:{}", e.id)).button(t!("entry.unsteam"), &format!("unsteam:{}", e.id));
                        }
                    });
                    // The desktop.
                    if !e.kept.is_empty() {
                        p.text(t!("entry.desktop.replaced", file = e.link));
                        p.buttons(|b| {
                            b.button(t!("entry.desktop.restore"), &format!("restore:{}", e.id));
                        });
                    } else if !e.link.is_empty() {
                        p.text(t!("entry.desktop.made", file = e.link));
                        p.buttons(|b| {
                            b.button(t!("entry.desktop.delete"), &format!("unlink:{}", e.id));
                        });
                    } else if standing == Standing::InSteam {
                        p.buttons(|b| {
                            if !e.shortcut.is_empty() && Path::new(&e.shortcut).exists() {
                                b.button(t!("entry.desktop.point"), &format!("point:{}", e.id));
                            } else {
                                b.button(t!("entry.desktop.create"), &format!("link:{}", e.id));
                            }
                        });
                    } else if !e.shortcut.is_empty() {
                        p.text(t!("entry.desktop.from", file = e.shortcut));
                    }
                    p.buttons(|b| {
                        b.button(t!("entry.remove"), &format!("remove:{}", e.id));
                    });
                });
            }
        });
        p.section_closed(t!("page.steam"), |p| {
            let account = steam.account.map_or_else(|| t!("page.steam.no_account"), |a| a.to_string());
            p.text(t!("page.steam.text", dir = steam.dir.display(), account = account, running = if self.steam_running { t!("page.steam.running") } else { t!("page.steam.closed") }));
            p.buttons(|b| {
                b.button(t!("page.open_backup"), "open_backup");
            });
        });
        p.section_closed(t!("page.guide"), |p| p.text(t!("page.guide.text")));
        p.section_closed(t!("page.advanced"), |p| {
            p.buttons(|b| {
                b.button(t!("page.open_log"), "open_log").button(t!("page.open_settings"), "open_settings");
            });
        });
    }

    fn status(&self, running: bool) -> Status {
        if !running {
            return Status::default();
        }
        if self.steam.is_none() {
            return Status { running, state: Health::Bad, text: t!("page.no_steam") };
        }
        let s = &self.settings;
        let waiting = self.waiting(s);
        let in_steam = s.entries.iter().filter(|e| self.standing(e) == Standing::InSteam).count();
        let (state, text) = if self.restarting {
            (Health::Warn, t!("page.restarting"))
        } else if !waiting.is_empty() {
            (Health::Warn, t!("status.waiting", n = waiting.len()))
        } else {
            (if self.error.is_some() { Health::Bad } else { Health::Ok }, t!("status.in", n = in_steam))
        };
        Status { running, state, text }
    }

    fn tray(&self) -> Tray {
        let mut menu = vec![];
        if !self.waiting(&self.settings).is_empty() && self.steam_running {
            menu.push(MenuEntry { label: t!("page.restart"), action: "restart".into() });
        }
        Tray { line: self.status(self.running).text, menu, live: vec![] }
    }

    fn applied(&mut self, s: &Settings) {
        self.settings = s.clone();
        let found = std::mem::take(&mut self.found);
        self.found = found.into_iter().filter(|f| !s.entries.iter().any(|e| Path::new(&e.shortcut) == f.path)).collect();
    }

    fn action(&mut self, name: &str, s: &mut Settings) -> Option<String> {
        let (verb, arg) = name.split_once(':').unwrap_or((name, ""));
        let index = s.entries.iter().position(|e| e.id == arg);
        match verb {
            "add" => {
                let f = desktop::read(Path::new(arg))?;
                let entry = Entry { id: new_id(), name: f.name.clone(), target: f.target, args: f.args, start_dir: f.start_dir, icon: f.icon, shortcut: arg.to_string(), ..Entry::default() };
                s.entries.push(entry);
                self.found.retain(|x| x.path != Path::new(arg));
                return Some(t!("toast.added", name = f.name));
            }
            "new" => {
                s.entries.push(Entry { id: new_id(), name: t!("entry.new_name"), ..Entry::default() });
                return Some(t!("toast.new"));
            }
            "refresh" => self.rescan(s),
            "restart" => {
                if !self.running {
                    return Some(t!("toast.start_first"));
                }
                self.restart = true;
            }
            "open_log" => {
                process::open(&app::data_dir().join("log.txt").to_string_lossy());
            }
            "open_settings" => {
                process::open(&app::data_dir().join("settings.json").to_string_lossy());
            }
            "open_backup" => {
                let dir = app::data_dir().join("steam-backup");
                let _ = std::fs::create_dir_all(&dir);
                process::open(&dir.to_string_lossy());
            }
            _ => {}
        }
        let i = index?;
        match verb {
            "add_now" | "add_later" => {
                let e = &mut s.entries[i];
                if e.target.trim().is_empty() {
                    return Some(t!("toast.no_target"));
                }
                if e.appid == 0 {
                    let sc = e.shortcut();
                    e.appid = steam::appid(&sc.exe, &sc.name);
                }
                e.steam = true;
                let name = e.title();
                if verb == "add_now" && self.steam_running {
                    if !self.running {
                        return Some(t!("toast.start_first"));
                    }
                    self.restart = true;
                    return None;
                }
                self.sync(s);
                return Some(if self.standing(&s.entries[i]) == Standing::InSteam { t!("toast.in_steam", name = name) } else { t!("toast.later", name = name) });
            }
            "unsteam" => {
                if !s.entries[i].kept.is_empty() {
                    return Some(t!("toast.restore_first"));
                }
                if !s.entries[i].link.is_empty() {
                    let _ = desktop::delete(Path::new(&s.entries[i].link));
                    s.entries[i].link.clear();
                }
                s.entries[i].steam = false;
                self.sync(s);
            }
            "play" => {
                let e = &s.entries[i];
                process::open(&e.address());
            }
            "copy" => {
                process::copy_text(&s.entries[i].address());
                return Some(t!("toast.copied"));
            }
            "point" => {
                let e = &s.entries[i];
                let backup = app::data_dir().join("desktop-backup").join(&e.id);
                let original = PathBuf::from(&e.shortcut);
                let icon = if e.icon.is_empty() { e.target.clone() } else { e.icon.clone() };
                // The new file would land on the next free place – it gets the old one's.
                let place = icons::place_of(&original);
                match desktop::replace(&original, &e.address(), &icon, &backup) {
                    Ok(link) => {
                        if let Some(place) = place {
                            icons::put(&link, place, ICON_WAIT);
                        }
                        let kept = backup.join(original.file_name().unwrap_or_default());
                        let e = &mut s.entries[i];
                        e.link = link.display().to_string();
                        e.kept = kept.display().to_string();
                        return Some(t!("toast.pointed"));
                    }
                    Err(err) => return Some(t!("toast.desktop_failed", error = err)),
                }
            }
            "restore" => {
                let e = &s.entries[i];
                let place = icons::place_of(Path::new(&e.link));
                match desktop::restore(Path::new(&e.link), Path::new(&e.kept), Path::new(&e.shortcut)) {
                    Ok(()) => {
                        if let Some(place) = place {
                            icons::put(Path::new(&s.entries[i].shortcut), place, ICON_WAIT);
                        }
                        let e = &mut s.entries[i];
                        e.link.clear();
                        e.kept.clear();
                        return Some(t!("toast.restored"));
                    }
                    Err(err) => return Some(t!("toast.desktop_failed", error = err)),
                }
            }
            "link" => {
                let e = &s.entries[i];
                let icon = if e.icon.is_empty() { e.target.clone() } else { e.icon.clone() };
                match desktop::create(&e.title(), &e.address(), &icon) {
                    Ok(link) => {
                        s.entries[i].link = link.display().to_string();
                        return Some(t!("toast.linked"));
                    }
                    Err(err) => return Some(t!("toast.desktop_failed", error = err)),
                }
            }
            "unlink" => {
                if let Err(err) = desktop::delete(Path::new(&s.entries[i].link)) {
                    return Some(t!("toast.desktop_failed", error = err));
                }
                s.entries[i].link.clear();
            }
            "remove" => {
                let e = &s.entries[i];
                if !e.kept.is_empty() {
                    return Some(t!("toast.restore_first"));
                }
                if self.standing(e) != Standing::NotInSteam {
                    return Some(t!("toast.unsteam_first"));
                }
                let e = s.entries.remove(i);
                self.rescan(s);
                return Some(t!("toast.removed", name = e.title()));
            }
            _ => {}
        }
        None
    }
}

/// The worker: looks at Steam every two seconds; writes the waiting changes the moment Steam is closed; restarts
/// Steam when asked to.
fn work(rt: Runtime<App>) {
    loop {
        rt.wait_running();
        rt.with(|p, s| {
            p.running = true;
            p.rescan(s);
        });
        rt.changed();
        while rt.running() {
            if rt.with(|p, _| std::mem::take(&mut p.restart)) {
                restart(&rt);
            }
            let changed = rt.with(|p, s| {
                let before = (p.steam_running, p.present.clone(), p.error.clone());
                p.look();
                if !p.steam_running && !p.waiting(s).is_empty() {
                    p.sync(s);
                }
                before != (p.steam_running, p.present.clone(), p.error.clone())
            });
            if changed {
                rt.changed();
            }
            rt.sleep(Duration::from_secs(2));
        }
        rt.with(|p, _| p.running = false);
        rt.changed();
    }
}

/// Quit Steam, write, start it again.
fn restart(rt: &Runtime<App>) {
    let Some(steam) = rt.with(|p, _| {
        p.restarting = true;
        p.steam.clone()
    }) else {
        return;
    };
    rt.changed();
    app::log("restarting Steam to write its list");
    let closed = steam.quit(Duration::from_secs(60));
    rt.with(|p, s| {
        if closed {
            p.sync(s);
        } else {
            p.error = Some(t!("error.quit"));
        }
    });
    if closed {
        steam.start();
    }
    rt.with(|p, _| p.restarting = false);
    rt.changed();
}

fn main() {
    kit::texts!();
    app::init(&App::META);
    app::run(App::new(), work);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        kit::texts!();
        kit::i18n::set_language("en");
        App { steam: None, ..App::new() }
    }

    fn game() -> Entry {
        Entry { id: "1".into(), name: "Game".into(), target: r"C:\Games\G\game.exe".into(), icon: r"C:\Games\G\game.exe,0".into(), ..Entry::default() }
    }

    #[test]
    fn page_keys_are_settings() {
        let a = App { steam: Some(Steam { dir: "C:\\Steam".into(), exe: "C:\\Steam\\steam.exe".into(), account: Some(1) }), ..app() };
        let s = Settings { entries: vec![game(), Entry::default()] };
        kit::page::check_keys_with(&s, |p| a.page(p, &s)).unwrap();
        kit::page::check_keys::<Settings>(|p| a.page(p, &Settings::default())).unwrap();
    }

    #[test]
    fn texts_are_complete() {
        kit::i18n::check_ids(env!("CARGO_MANIFEST_DIR")).unwrap();
    }

    #[test]
    fn what_steam_gets() {
        let e = Entry { args: "-windowed".into(), appid: 7, ..game() };
        assert_eq!(e.shortcut(), Shortcut { appid: 7, name: "Game".into(), exe: r#""C:\Games\G\game.exe""#.into(), start_dir: r#""C:\Games\G\""#.into(), icon: r"C:\Games\G\game.exe".into(), options: "-windowed".into() });
        // A working folder from the shortcut wins.
        let e = Entry { start_dir: r"D:\Data\".into(), ..game() };
        assert_eq!(e.shortcut().start_dir, r#""D:\Data\""#);
        assert!(is_address("uplay://launch/410/0") && !is_address(r"C:\x.exe") && !is_address(r"C:\a://b"));
        assert_eq!(e.address(), "steam://rungameid/33554432");
    }

    #[test]
    fn standing_follows_steams_list() {
        let mut a = app();
        let mut e = game();
        assert_eq!(a.standing(&e), Standing::NotInSteam);
        e.steam = true;
        e.appid = 5;
        assert_eq!(a.standing(&e), Standing::WaitsAdd);
        a.present = vec![e.shortcut()];
        assert_eq!(a.standing(&e), Standing::InSteam);
        e.args = "-x".into();
        assert_eq!(a.standing(&e), Standing::WaitsAdd, "changed: written again");
        e.steam = false;
        assert_eq!(a.standing(&e), Standing::WaitsRemove);
        a.present.clear();
        assert_eq!(a.standing(&e), Standing::NotInSteam);
    }

    #[test]
    fn buttons() {
        let mut a = app();
        let mut s = Settings { entries: vec![game()] };
        // Without Steam nothing is written, the entry waits.
        assert!(a.action("add_later:1", &mut s).unwrap().contains("Steam"));
        assert!(s.entries[0].steam && s.entries[0].appid & 0x8000_0000 != 0);
        let appid = s.entries[0].appid;
        a.action("unsteam:1", &mut s);
        a.action("add_later:1", &mut s);
        assert_eq!(s.entries[0].appid, appid, "the number stays");
        assert!(a.action("remove:1", &mut s).unwrap().contains("Steam"), "first out of Steam");
        a.action("unsteam:1", &mut s);
        assert!(a.action("remove:1", &mut s).is_some());
        assert!(s.entries.is_empty());
        a.action("new", &mut s);
        assert_eq!(s.entries.len(), 1);
        assert!(a.action(&format!("add_later:{}", s.entries[0].id), &mut s).unwrap().contains("path"), "nothing to start");
        // "Restart now" needs the worker.
        assert!(a.action("restart", &mut s).unwrap().contains("Start"));
    }

    #[test]
    fn the_page_tells_the_standing() {
        let mut a = App { steam: Some(Steam { dir: "C:\\Steam".into(), exe: "C:\\Steam\\steam.exe".into(), account: Some(1) }), running: true, ..app() };
        let mut e = game();
        let page = |a: &App, s: &Settings| {
            let v = serde_json::to_value(s).unwrap();
            let mut b = Builder::new(&v);
            a.page(&mut b, s);
            serde_json::to_string(&b.finish()).unwrap()
        };
        let s = Settings { entries: vec![e.clone()] };
        let text = page(&a, &s);
        assert!(text.contains("add_later:1") && !text.contains("add_now:1"), "Steam closed: one button");
        a.steam_running = true;
        let text = page(&a, &s);
        assert!(text.contains("add_now:1") && text.contains("add_later:1"), "Steam running: both");
        e.steam = true;
        e.appid = 9;
        let s = Settings { entries: vec![e.clone()] };
        let text = page(&a, &s);
        assert!(text.contains(r#""group":"waiting""#) && text.contains("\"restart\""), "waiting, with the restart button");
        a.present = vec![e.shortcut()];
        let text = page(&a, &s);
        assert!(text.contains(r#""group":"in""#) && text.contains("play:1") && text.contains("link:1"));
        assert!(!text.contains("\"restart\""));
        a.steam = None;
        assert!(page(&a, &s).contains("Steam was not found"));
    }
}
