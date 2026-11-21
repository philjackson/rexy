//! What this window takes from Ghostty's configuration, where that is on this
//! machine: the colours it gives the terminals it owns, and the fonts their
//! text is drawn in. Rex has each client say its own colours, and a pane shows
//! its owner's. Without Ghostty's this one says none, and a pane then keeps the
//! colours it has.
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    thread,
    time::Duration,
};

use serde_json::{Map, Value, json};
use tokio::sync::watch;

/// The colours Ghostty's configuration gives the terminals, as they are now.
/// None, where it gives none.
pub type Theme = watch::Receiver<Option<Value>>;

/// How often the configuration is read again for the colours it gives.
const AGAIN: Duration = Duration::from_secs(1);
/// How long a theme read anew is given to be read the same again: a file
/// caught being written is read half-way.
const SETTLE: Duration = Duration::from_millis(200);

fn var(name: &str) -> Option<OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

/// Ghostty's configuration, and the directory that the user's are kept under.
/// It is in `config.ghostty`, or `config` as it was first named, or both: Ghostty
/// reads the old name first, so that what the new one says stands over it.
fn config() -> Option<(PathBuf, String)> {
    let home = home()?;
    let config = read_config(&home)?;
    Some((home, config))
}

/// The directory the user's configuration is kept under.
fn home() -> Option<PathBuf> {
    let home = var("XDG_CONFIG_HOME").map(PathBuf::from);
    home.or_else(|| Some(PathBuf::from(var("HOME")?).join(".config")))
}

/// The files of Ghostty's configuration under `home`, read as one, in the order
/// Ghostty reads them. None, where there is neither.
fn read_config(home: &Path) -> Option<String> {
    let files = ["ghostty/config", "ghostty/config.ghostty"];
    let files: Vec<_> =
        files.iter().filter_map(|file| fs::read_to_string(home.join(file)).ok()).collect();
    (!files.is_empty()).then(|| files.join("\n"))
}

/// The theme Ghostty would use here, as it is now and as it comes to be: Ghostty
/// reads its configuration again as it is written, and so does this, each
/// second on a thread of its own, whether the configuration changed or the file
/// of the theme it names. A theme that goes is not followed: what the terminals
/// have, they keep, and those still to come are given the same.
pub fn theme() -> Theme {
    let (sender, theme) = watch::channel(home().and_then(|home| theme_in(&home)));
    let watching = thread::Builder::new().name("ghostty-theme".into()).spawn(move || {
        while !sender.is_closed() {
            thread::sleep(AGAIN);
            let read = || home().and_then(|home| theme_in(&home));
            let Some(now) = read() else { continue };
            if sender.borrow().as_ref() == Some(&now) {
                continue;
            }
            thread::sleep(SETTLE);
            if read().as_ref() == Some(&now) {
                sender.send_replace(Some(now));
            }
        }
    });
    if let Err(error) = watching {
        eprintln!("Cannot follow Ghostty's theme: {error}");
    }
    theme
}

/// The theme Ghostty's configuration under `home` comes to, as Rex's
/// `set_theme` takes it.
fn theme_in(home: &Path) -> Option<Value> {
    let config = read_config(home)?;
    // A theme is a file of that name: the user's own, or one Ghostty came with.
    let shared = var("XDG_DATA_DIRS").unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let shared: Vec<_> = env::split_paths(&shared).collect();
    let named = |name: &str| {
        let own = [PathBuf::from(name), home.join("ghostty/themes").join(name)];
        let came = shared.iter().map(|dir| dir.join("ghostty/themes").join(name));
        let mut files = own.into_iter().chain(came);
        files.find_map(|file| fs::read_to_string(file).ok())
    };
    read(&config, named)
}

/// The families of font Ghostty would draw its text in here, each standing
/// behind the one before it for what that cannot draw. None, where Ghostty is
/// left to its own.
pub fn fonts() -> Vec<String> {
    config().map(|(_, config)| families(&config)).unwrap_or_default()
}

/// The families a configuration names. One named as nothing begins the list
/// again, as it does for Ghostty.
fn families(config: &str) -> Vec<String> {
    let mut named = Vec::new();
    for (_, name) in settings(config).filter(|(key, _)| *key == "font-family") {
        if name.is_empty() {
            named.clear();
        } else {
            named.push(name.to_owned());
        }
    }
    named
}

/// The settings of a file of Ghostty's: a key and its value to a line, with
/// `#` before what is said in passing.
fn settings(text: &str) -> impl Iterator<Item = (&str, &str)> {
    let lines = text.lines().filter(|line| !line.trim_start().starts_with('#'));
    lines.filter_map(|line| {
        let (key, value) = line.split_once('=')?;
        Some((key.trim(), value.trim().trim_matches('"')))
    })
}

/// A colour as Rex takes one, `#rrggbb`, if it is written so. One by name, or
/// that stands for another, is left to the terminal's own.
fn color(value: &str) -> Option<String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    let whole = hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit());
    whole.then(|| format!("#{hex}"))
}

/// The theme a configuration comes to: the colours of the theme it names, as
/// `named` finds that, with those it sets itself in place of them. Nothing, if
/// it comes to no colour at all.
fn read(config: &str, named: impl Fn(&str) -> Option<String>) -> Option<Value> {
    // Of a theme for the light of day and one for the dark, the dark: the
    // window's own colours are dark.
    let theme = settings(config).filter(|(key, _)| *key == "theme").last();
    let theme = theme.map(|(_, name)| {
        let mut parts = name.split(',').map(str::trim);
        let dark = parts.find_map(|part| part.strip_prefix("dark:"));
        dark.map_or(name, str::trim)
    });
    let theme = theme.filter(|name| !name.is_empty()).and_then(named);
    let mut colors = Map::new();
    let mut palette = BTreeMap::new();
    for (key, value) in settings(theme.as_deref().unwrap_or_default()).chain(settings(config)) {
        let name = match key {
            "background" | "foreground" => key,
            "cursor-color" => "cursor",
            "palette" => {
                let entry = value.split_once('=');
                let entry = entry.and_then(|(index, value)| {
                    Some((index.trim().parse::<u8>().ok()?, color(value.trim())?))
                });
                palette.extend(entry);
                continue;
            }
            _ => continue,
        };
        // One set to nothing is the terminal's own again.
        match color(value) {
            Some(color) => colors.insert(name.to_owned(), json!(color)),
            None => colors.remove(name),
        };
    }
    if !palette.is_empty() {
        let entries = palette.iter().map(|(index, color)| format!("{index}={color}"));
        colors.insert("palette".into(), entries.collect());
    }
    (!colors.is_empty()).then_some(Value::Object(colors))
}

#[cfg(test)]
#[path = "../tests/unit/ghostty.rs"]
mod tests;
