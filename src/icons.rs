//! Where the desktop shows its icons. A shortcut that is replaced is a new file to Explorer, and a new file lands on
//! the next free place – so the old one's place is read before and given to the new one after.
//!
//! Explorer's desktop is a folder view: `IShellWindows` finds it, `IFolderView` reads (`GetItemPosition`) and sets
//! (`SelectAndPositionItems`) the place of an item. Every call runs on a thread of its own with COM set up there, so
//! it does not matter how the caller's thread uses COM. With "Auto arrange icons" switched on, Explorer arranges
//! them itself and a place cannot be set.

use std::path::Path;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::System::Com::{CLSCTX_LOCAL_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, IServiceProvider};
use windows::Win32::System::Variant::VARIANT;
use windows::Win32::UI::Shell::Common::{ITEMIDLIST, STRRET};
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, IEnumIDList, IFolderView, IShellBrowser, IShellFolder, IShellWindows, SHGDN_FORPARSING, SID_STopLevelBrowser, SVGIO_ALLVIEW, SVSI_POSITIONITEM, SWC_DESKTOP, SWFO_NEEDDISPATCH, ShellWindows, StrRetToStrW,
};
use windows::core::{Interface, PWSTR};

/// A place on the desktop, in the desktop's own coordinates.
pub type Place = (i32, i32);

/// Runs `f` on a thread of its own with COM set up.
fn on_com_thread<R: Send>(f: impl FnOnce() -> Option<R> + Send) -> Option<R> {
    std::thread::scope(|s| {
        s.spawn(|| {
            let ok = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
            let r = if ok { f() } else { None };
            if ok {
                unsafe { CoUninitialize() };
            }
            r
        })
        .join()
        .ok()
        .flatten()
    })
}

struct Desktop {
    view: IFolderView,
    folder: IShellFolder,
}

impl Desktop {
    fn find() -> Option<Desktop> {
        unsafe {
            let windows: IShellWindows = CoCreateInstance(&ShellWindows, None, CLSCTX_LOCAL_SERVER).ok()?;
            let (location, root) = (VARIANT::from(CSIDL_DESKTOP as i32), VARIANT::default());
            let mut hwnd = 0;
            let dispatch = windows.FindWindowSW(&location, &root, SWC_DESKTOP, &mut hwnd, SWFO_NEEDDISPATCH).ok()?;
            let browser: IShellBrowser = dispatch.cast::<IServiceProvider>().ok()?.QueryService(&SID_STopLevelBrowser).ok()?;
            let view: IFolderView = browser.QueryActiveShellView().ok()?.cast().ok()?;
            let folder: IShellFolder = view.GetFolder().ok()?;
            Some(Desktop { view, folder })
        }
    }

    /// Calls `f` with the item whose full path is `path` (upper and lower case alike); `None` if it is not shown.
    fn with_item<R>(&self, path: &Path, f: impl FnOnce(*const ITEMIDLIST) -> Option<R>) -> Option<R> {
        let want = path.to_string_lossy().to_lowercase();
        unsafe {
            let items: IEnumIDList = self.view.Items(SVGIO_ALLVIEW).ok()?;
            let mut f = Some(f);
            let mut out = None;
            loop {
                let mut one = [std::ptr::null_mut::<ITEMIDLIST>()];
                let mut got = 0u32;
                if items.Next(&mut one, Some(&mut got)).is_err() || got == 0 {
                    break;
                }
                let pidl = one[0];
                if out.is_none() && self.name(pidl).is_some_and(|n| n.to_lowercase() == want) {
                    out = f.take().and_then(|f| f(pidl));
                }
                CoTaskMemFree(Some(pidl as *const _));
            }
            out
        }
    }

    fn name(&self, pidl: *const ITEMIDLIST) -> Option<String> {
        unsafe {
            let mut s = STRRET::default();
            self.folder.GetDisplayNameOf(pidl, SHGDN_FORPARSING, &mut s).ok()?;
            let mut text = PWSTR::null();
            StrRetToStrW(&mut s, Some(pidl), &mut text).ok()?;
            let name = text.to_string().ok();
            CoTaskMemFree(Some(text.0 as *const _));
            name
        }
    }
}

/// Where the desktop shows this file – `None` if it is not on the desktop (or Explorer is not running).
pub fn place_of(path: &Path) -> Option<Place> {
    on_com_thread(|| {
        let d = Desktop::find()?;
        d.with_item(path, |pidl| unsafe { d.view.GetItemPosition(pidl).ok() }.map(|p| (p.x, p.y)))
    })
}

/// Puts this file's icon at `place` – waiting up to `wait` for Explorer to show a file that was just made.
/// `true` = done.
pub fn put(path: &Path, place: Place, wait: Duration) -> bool {
    on_com_thread(|| {
        let d = Desktop::find()?;
        let until = Instant::now() + wait;
        loop {
            let point = POINT { x: place.0, y: place.1 };
            let done = d.with_item(path, |pidl| unsafe { d.view.SelectAndPositionItems(1, &pidl, Some(&point), SVSI_POSITIONITEM.0 as u32).ok() });
            if done.is_some() {
                return Some(());
            }
            if Instant::now() >= until {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })
    .is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real thing with a throw-away shortcut on this PC's desktop (shown for a moment, then deleted): pointed
    /// at Steam and back, it keeps its place both times. Other icons are not moved.
    #[test]
    #[ignore]
    fn a_replaced_icon_keeps_its_place() {
        let desktop = crate::desktop::own_desktop().unwrap();
        let original = desktop.join("zz Steam Shortcuts test.url");
        std::fs::write(&original, "[InternetShortcut]
URL=https://example.com
").unwrap();
        let wait = Duration::from_secs(3);
        // Wait until Explorer shows it, then note its place.
        let start = Instant::now();
        let first = loop {
            if let Some(p) = place_of(&original) {
                break p;
            }
            assert!(start.elapsed() < wait, "Explorer never showed it");
            std::thread::sleep(Duration::from_millis(100));
        };
        // Away from the first free place – where a new file would land by itself anyway.
        assert!(put(&original, (first.0 + 1200, first.1 + 400), wait));
        let place = place_of(&original).unwrap();
        assert_ne!(place, first, "it moved");
        let backup = std::env::temp_dir().join(format!("steam-shortcuts-icon-{}", std::process::id()));
        let link = crate::desktop::replace(&original, "steam://rungameid/1", "", &backup).unwrap();
        let kept = backup.join("zz Steam Shortcuts test.url");
        let moved = put(&link, place, wait);
        let after = place_of(&link);
        crate::desktop::restore(&link, &kept, &original).unwrap();
        let back = put(&original, place, wait);
        let again = place_of(&original);
        std::fs::remove_file(&original).unwrap();
        std::fs::remove_dir_all(&backup).ok();
        println!("first free {first:?}, moved to {place:?} → replaced {after:?} → back {again:?}");
        assert!(moved && back);
        assert_eq!((after, again), (Some(place), Some(place)));
    }

    /// Prints the place of every shortcut on this PC's desktops – nothing is moved.
    #[test]
    #[ignore]
    fn places_on_this_pc() {
        for f in crate::desktop::scan() {
            println!("{:<40} {:?}", f.name, place_of(&f.path));
        }
    }
}
