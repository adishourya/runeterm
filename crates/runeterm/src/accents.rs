//! Each session's own accent, in `~/.local/state/runeterm/accents` (one `name #rrggbb` per
//! line), shared by every runeterm window.

use crate::vt::Rgb;
use std::path::PathBuf;

fn path() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"))
        .join("runeterm/accents")
}

fn load() -> Vec<(String, Rgb)> {
    let text = std::fs::read_to_string(path()).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let (name, color) = l.trim().rsplit_once(' ')?;
            Some((name.trim().to_string(), crate::config::rgb(color.trim())?))
        })
        .collect()
}

fn save(all: &[(String, Rgb)]) {
    let path = path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let out: String = all.iter().map(|(n, c)| format!("{n} {}\n", crate::config::hex_of(*c))).collect();
    if let Err(e) = std::fs::write(&path, out) {
        eprintln!("runeterm: can't save the session accents: {e}");
    }
}

/// The accent picked for a palette, given the ones taken: the first one nobody has, or else the
/// least used.
fn pick(palette: &[Rgb], taken: &[Rgb]) -> Option<Rgb> {
    palette.iter().copied().min_by_key(|c| taken.iter().filter(|t| *t == c).count())
}

/// Session `name`'s accent. A session without one gets the palette's least used colour.
pub fn of(name: &str, palette: &[Rgb], default: Rgb) -> Rgb {
    let mut all = load();
    if let Some((_, c)) = all.iter().find(|(n, _)| n == name) {
        return *c;
    }
    let taken: Vec<Rgb> = all.iter().map(|(_, c)| *c).collect();
    let c = pick(palette, &taken).unwrap_or(default);
    all.push((name.to_string(), c));
    save(&all);
    c
}

pub fn set(name: &str, c: Rgb) {
    let mut all = load();
    all.retain(|(n, _)| n != name);
    all.push((name.to_string(), c));
    save(&all);
}

/// Forgets a killed session, freeing its colour.
pub fn remove(name: &str) {
    let mut all = load();
    let before = all.len();
    all.retain(|(n, _)| n != name);
    if all.len() != before {
        save(&all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_an_unused_colour_then_the_least_used() {
        let (a, b, c) = (Rgb(1, 0, 0), Rgb(0, 1, 0), Rgb(0, 0, 1));
        assert_eq!(pick(&[a, b, c], &[a]), Some(b));
        assert_eq!(pick(&[a, b, c], &[a, b, c, a, c]), Some(b));
        assert_eq!(pick(&[], &[a]), None);
    }
}
