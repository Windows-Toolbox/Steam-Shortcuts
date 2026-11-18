# Steam Shortcuts

Adds programs and games to Steam, so the Steam Controller works in them, and points desktop icons at Steam – and
back. A program of the [Windows Toolbox](https://github.com/Windows-Toolbox/Toolbox).

> **Downloaded from the releases?** The `.exe` is not signed (signing costs money). Windows SmartScreen may warn
> once at the first start: *More info → Run anyway*.

Steam Input – what makes the Steam Controller work – only reaches games that Steam starts. A game from another
store or a launcher has to be added to Steam as a "non-Steam game" by hand, one by one, and its desktop icon still
starts it without Steam. This program does both from one list: add to Steam, point the desktop icon at Steam, and
undo either.

## Using it

Add the folder in the Toolbox (**Add program…**), press **Start**, and switch on **Start with Windows** if waiting
changes should be written whenever Steam is closed. Everything else is on the program's page there:

- **+ Add**: the shortcuts on your desktop (Steam games left out) – a click puts one on the list. Or an entry of
  your own: a program's path or an address (`uplay://launch/410/0`), a name, launch parameters.
- **Add to Steam**: Steam rewrites its list of non-Steam games when it quits, so the list can only be written while
  Steam is closed. Steam closed: written at once. Steam running: **Restart Steam and add** (Steam quits, the list is
  written, Steam starts again) or **Add when Steam is closed** – the entry waits, a line above the list names
  everything that waits (with **Restart Steam now**), and it is written the moment Steam is closed. Steam is never
  quit without a click.
- **In Steam**: start it through Steam, copy its `steam://rungameid/…` address (for example for Screen Zones'
  "Play opens"), remove it from Steam again.
- **Desktop**: **Point the desktop icon at Steam** puts a `.url` with the Steam address in place of the shortcut –
  same name, same icon, same place on the desktop – and keeps the original; **Put the desktop icon back** puts the
  very same file back, at that place again. An entry without a desktop icon can get a new one. (With "Auto arrange
  icons" switched on, Explorer arranges them itself.)
- **Addresses** (`uplay://`, `battlenet://`, `com.epicgames.launcher://`): Steam gets the program Windows opens them
  with, the address as option. Ubisoft Connect started through Steam passes the controller on to the game it starts
  (tried with Rayman Legends). It should not be running outside Steam already.

Entries in Steam's list that this program did not make stay untouched; before every write the list is copied into
`data\steam-backup\` (the last ten). Shortcuts on the desktop of all users need administrator rights to be changed –
the page says so.

## How it works

| File | What it holds |
|---|---|
| `src\main.rs` | settings, the page, the buttons, the worker (looks at Steam every two seconds, writes what waits the moment Steam is closed, restarts Steam when asked) |
| `src\steam.rs` | finding Steam (registry `HKCU\Software\Valve\Steam`) and the account (signed in now, else last – `loginusers.vdf`), quitting (`steam.exe -shutdown`) and starting it, reading and writing `userdata\<account>\config\shortcuts.vdf`, the program behind an address |
| `src\vdf.rs` | Steam's binary VDF (the list; unknown parts kept byte for byte) and a reader for its text VDF |
| `src\desktop.rs` | the desktops (yours and all users'), reading `.lnk` (format MS-SHLLINK) and `.url`, pointing at Steam and back |
| `src\icons.rs` | where the desktop shows an icon: read before a shortcut is replaced, given to the new file after (Explorer's `IFolderView`, through the `windows` crate) |
| `assets\i18n.json` | every text, German and English |

An entry's number in Steam is made once, as Steam made them for years (CRC-32 of exe and name, top bit set) and
kept afterwards; `steam://rungameid/<number << 32 | 0x02000000>` starts it.

## Building

`build.ps1` builds the program and installs it into `dist\` – that folder is the installed program. Requirement:
[Rust](https://rustup.rs). `release.ps1` publishes a signed release (needs the Toolbox repo next to this one). The
shared library (`toolbox-kit`) comes from the Toolbox repo (Toolbox `DESIGN.md`, section 3).

**Tests**: `cargo test` – both file formats (Steam's list read and written back byte for byte, with an entry of
someone else in it that must stay untouched), adding, changing and removing entries, the numbers, `.lnk` and `.url`,
pointing a desktop icon at Steam and back (in a temporary folder), the page and its buttons, the texts.
`cargo test -- --ignored --nocapture --test-threads=1` reads this PC – Steam, its list (read and rebuilt in memory,
compared byte for byte, never written), the desktops and their icon places – and changes nothing except one
throw-away shortcut that is on the desktop for a moment (the place test).

## Checklist

✔ covered by a test · ◐ built, **not yet tried** on the real PC

- ✔ reading Steam's real list and rebuilding it byte for byte (read only)
- ✔ finding Steam, the account, the program behind `uplay://` on the real PC (read only)
- ✔ reading every shortcut on the real desktops
- ✔ a replaced desktop icon keeps its place, both ways – `cargo test a_replaced_icon_keeps_its_place -- --ignored`
  puts a throw-away shortcut on the real desktop for a moment, away from the first free place, and deletes it after
- ✔ by hand (2026-11-18): Rayman through Ubisoft Connect added to the real Steam, Steam restarted, its desktop icon
  pointed at Steam and back, the controller in the game
