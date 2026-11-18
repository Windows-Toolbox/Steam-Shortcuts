//! Steam's two file formats, as far as this program needs them.
//!
//! - **Binary VDF** (`shortcuts.vdf`, the non-Steam games): a tree of named values. Every value starts with its
//!   type byte and its name (text ending in 0): `0x00` a map (its values follow, closed by `0x08`), `0x01` a text
//!   (ending in 0), `0x02` a 32-bit number. The file is one map named `shortcuts` whose entries are named `0`, `1`, …
//!   Anything else Steam may put in is kept as it was, byte for byte – this program only adds, changes and removes
//!   its own entries.
//! - **Text VDF** (`loginusers.vdf`): `"key" "value"` and `"key" { … }` – only read, to find the account last signed
//!   in.

/// A value of the binary format. `Other` keeps a type this program does not know (with its raw bytes).
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Map(Vec<(String, Value)>),
    Text(String),
    Int(u32),
    Other(u8, Vec<u8>),
}

const MAP: u8 = 0x00;
const TEXT: u8 = 0x01;
const INT: u8 = 0x02;
const FLOAT: u8 = 0x03;
const PTR: u8 = 0x04;
const WIDE: u8 = 0x05;
const COLOR: u8 = 0x06;
const INT64: u8 = 0x07;
const END: u8 = 0x08;

struct Reader<'a> {
    data: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.data.get(self.at).ok_or("the file ends too early")?;
        self.at += 1;
        Ok(b)
    }

    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.data.len()).ok_or("the file ends too early")?;
        let out = &self.data[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn text(&mut self) -> Result<String, String> {
        let len = self.data[self.at..].iter().position(|&b| b == 0).ok_or("a text without its end")?;
        let s = String::from_utf8_lossy(&self.data[self.at..self.at + len]).into_owned();
        self.at += len + 1;
        Ok(s)
    }

    fn map(&mut self) -> Result<Vec<(String, Value)>, String> {
        let mut out = vec![];
        loop {
            let kind = self.byte()?;
            if kind == END {
                return Ok(out);
            }
            let name = self.text()?;
            let value = match kind {
                MAP => Value::Map(self.map()?),
                TEXT => Value::Text(self.text()?),
                INT => Value::Int(u32::from_le_bytes(self.take(4)?.try_into().map_err(|_| "a number too short")?)),
                FLOAT | PTR | COLOR => Value::Other(kind, self.take(4)?.to_vec()),
                INT64 => Value::Other(kind, self.take(8)?.to_vec()),
                WIDE => {
                    // Text in UTF-16, ending in two zero bytes.
                    let start = self.at;
                    while self.take(2)? != [0, 0] {}
                    Value::Other(kind, self.data[start..self.at].to_vec())
                }
                other => return Err(format!("unknown type {other:#04x}")),
            };
            out.push((name, value));
        }
    }
}

/// Reads a binary VDF file: its top map (name and values).
pub fn read(data: &[u8]) -> Result<(String, Vec<(String, Value)>), String> {
    let mut r = Reader { data, at: 0 };
    if r.byte()? != MAP {
        return Err("no map at the start".into());
    }
    let name = r.text()?;
    let values = r.map()?;
    // Steam ends the file with one more END.
    Ok((name, values))
}

fn write_map(out: &mut Vec<u8>, values: &[(String, Value)]) {
    for (name, value) in values {
        let kind = match value {
            Value::Map(_) => MAP,
            Value::Text(_) => TEXT,
            Value::Int(_) => INT,
            Value::Other(kind, _) => *kind,
        };
        out.push(kind);
        out.extend_from_slice(name.as_bytes());
        out.push(0);
        match value {
            Value::Map(inner) => write_map(out, inner),
            Value::Text(s) => {
                out.extend_from_slice(s.as_bytes());
                out.push(0);
            }
            Value::Int(n) => out.extend_from_slice(&n.to_le_bytes()),
            Value::Other(_, raw) => out.extend_from_slice(raw),
        }
    }
    out.push(END);
}

/// Writes a binary VDF file the way Steam does (the top map, then one more END).
pub fn write(name: &str, values: &[(String, Value)]) -> Vec<u8> {
    let mut out = vec![MAP];
    out.extend_from_slice(name.as_bytes());
    out.push(0);
    write_map(&mut out, values);
    out.push(END);
    out
}

/// A value of a map by name – Steam writes the names of `shortcuts.vdf` in varying case (`appid`, `AppName`,
/// `exe` in old files), so upper and lower case do not matter.
pub fn get<'a>(map: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    map.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v)
}

/// Sets a value, keeping its place (and the spelling of its name) if it is there already.
pub fn set(map: &mut Vec<(String, Value)>, name: &str, value: Value) {
    match map.iter_mut().find(|(n, _)| n.eq_ignore_ascii_case(name)) {
        Some((_, v)) => *v = value,
        None => map.push((name.to_string(), value)),
    }
}

// ---------- text VDF ----------

/// The tokens of a text VDF file: quoted strings and the braces.
fn tokens(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                let mut s = String::new();
                while let Some(c) = chars.next() {
                    match c {
                        '\\' => {
                            if let Some(n) = chars.next() {
                                s.push(n);
                            }
                        }
                        '"' => break,
                        c => s.push(c),
                    }
                }
                out.push(s);
            }
            '{' | '}' => out.push(c.to_string()),
            '/' if chars.peek() == Some(&'/') => {
                for c in chars.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    out
}

/// The SteamID64 of the account last signed in, from `loginusers.vdf` (`"MostRecent" "1"`).
pub fn most_recent_user(loginusers: &str) -> Option<u64> {
    let t = tokens(loginusers);
    let mut current: Option<u64> = None;
    let mut depth = 0;
    for (i, tok) in t.iter().enumerate() {
        match tok.as_str() {
            "{" => depth += 1,
            "}" => depth -= 1,
            _ if depth == 1 && t.get(i + 1).map(String::as_str) == Some("{") => current = tok.parse().ok(),
            _ if depth == 2 && tok.eq_ignore_ascii_case("MostRecent") && t.get(i + 1).map(String::as_str) == Some("1") => return current,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A file as Steam writes it: one entry with every field Steam puts in (from a real file), plus a tag.
    pub fn sample() -> Vec<u8> {
        let entry = vec![
            ("appid".to_string(), Value::Int(0x8c71_67d2)),
            ("AppName".into(), Value::Text("Ubisoft Connect".into())),
            ("Exe".into(), Value::Text(r#""C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\UbisoftConnect.exe""#.into())),
            ("StartDir".into(), Value::Text(r"C:\Program Files (x86)\Ubisoft\Ubisoft Game Launcher\".into())),
            ("icon".into(), Value::Text(String::new())),
            ("ShortcutPath".into(), Value::Text(String::new())),
            ("LaunchOptions".into(), Value::Text(String::new())),
            ("IsHidden".into(), Value::Int(0)),
            ("AllowDesktopConfig".into(), Value::Int(1)),
            ("AllowOverlay".into(), Value::Int(1)),
            ("OpenVR".into(), Value::Int(0)),
            ("Devkit".into(), Value::Int(0)),
            ("DevkitGameID".into(), Value::Text(String::new())),
            ("DevkitOverrideAppID".into(), Value::Int(0)),
            ("LastPlayTime".into(), Value::Int(0x6afc_d504)),
            ("FlatpakAppID".into(), Value::Text(String::new())),
            ("sortas".into(), Value::Text(String::new())),
            ("tags".into(), Value::Map(vec![("0".into(), Value::Text("Favorites".into()))])),
        ];
        write("shortcuts", &[("0".into(), Value::Map(entry))])
    }

    #[test]
    fn steam_layout() {
        let data = sample();
        // The beginning and the end as in Steam's own file.
        assert!(data.starts_with(b"\x00shortcuts\x00\x000\x00\x02appid\x00\xd2\x67\x71\x8c\x01AppName\x00Ubisoft Connect\x00"));
        assert!(data.ends_with(b"\x08\x08\x08\x08"), "tags, entry, shortcuts, file");
    }

    #[test]
    fn read_and_write_give_the_same_bytes() {
        let data = sample();
        let (name, values) = read(&data).unwrap();
        assert_eq!(name, "shortcuts");
        let Value::Map(entry) = &values[0].1 else { panic!() };
        assert_eq!(get(entry, "APPNAME"), Some(&Value::Text("Ubisoft Connect".into())));
        assert_eq!(get(entry, "AppId"), Some(&Value::Int(0x8c71_67d2)));
        assert_eq!(write(&name, &values), data);
        // Unknown types survive.
        let odd = write("x", &[("f".into(), Value::Other(FLOAT, vec![1, 2, 3, 4])), ("w".into(), Value::Other(WIDE, vec![65, 0, 0, 0]))]);
        let (n, v) = read(&odd).unwrap();
        assert_eq!(write(&n, &v), odd);
    }

    #[test]
    fn broken_files_are_refused() {
        let data = sample();
        assert!(read(&data[..data.len() / 2]).is_err());
        assert!(read(b"").is_err());
        assert!(read(b"\x01x\x00").is_err());
        assert!(read(b"\x00s\x00\x09x\x00\x08").is_err(), "unknown type");
    }

    #[test]
    fn set_keeps_the_place() {
        let mut m = vec![("AppName".to_string(), Value::Text("a".into())), ("Exe".into(), Value::Text("b".into()))];
        set(&mut m, "appname", Value::Text("c".into()));
        set(&mut m, "LaunchOptions", Value::Text("-w".into()));
        assert_eq!(m[0], ("AppName".to_string(), Value::Text("c".into())));
        assert_eq!(m[2].0, "LaunchOptions");
    }

    #[test]
    fn the_last_user() {
        let text = r#""users"
{
	"76561198852943952"
	{
		"AccountName"		"someone"
		"MostRecent"		"0"
	}
	"76561198852943953"
	{
		"AccountName"		"other"
		"PersonaName"		"with \"quotes\""
		"MostRecent"		"1"
	}
}"#;
        assert_eq!(most_recent_user(text), Some(76561198852943953));
        assert_eq!(most_recent_user(r#""users" { "1" { "MostRecent" "0" } }"#), None);
    }

    /// The real file on this PC, read only: it reads, and writing it gives the same bytes.
    #[test]
    #[ignore]
    fn the_real_file() {
        let Some(steam) = crate::steam::Steam::find() else { return };
        let Some(file) = steam.shortcuts_file() else { return };
        let data = std::fs::read(&file).unwrap();
        let (name, values) = read(&data).unwrap();
        println!("{} entries in {}", values.len(), file.display());
        assert_eq!(write(&name, &values), data);
    }
}
