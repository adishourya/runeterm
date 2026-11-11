//! What runeterm tells the rest of the desktop, in `~/.rune_state_exports`: the session in the
//! runeterm window you were last in, and its colours. Ghostty-style `key = value` lines.
//!
//! Only the focused window writes it, so it always follows the window you're in. Written to a
//! temporary file and renamed over, so readers (the Hyprland title bars, the rune bar) never
//! see half a file.

use crate::vt::Rgb;
use std::path::PathBuf;

pub fn path() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".rune_state_exports")
}

/// What gets exported. Compared against the last write, so nothing is written twice.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Exports {
    pub session: String,
    pub animal: String,
    pub accent: Rgb,
    pub light: bool,
}

impl Exports {
    fn render(&self) -> String {
        let hex = crate::config::hex_of;
        format!(
            "# Written by runeterm: the session in the runeterm window you were last in.\n\
             # Read by the Hyprland title bars (runebars) and anything else that wants the colour.\n\
             session = {}\n\
             animal = {}\n\
             accent = {}\n\
             theme = {}\n",
            self.session,
            self.animal,
            hex(self.accent),
            if self.light { "light" } else { "dark" },
        )
    }
}

pub fn write(e: &Exports) {
    let path = path();
    let tmp = path.with_extension("tmp");
    let result = std::fs::write(&tmp, e.render()).and_then(|_| std::fs::rename(&tmp, &path));
    if let Err(err) = result {
        eprintln!("runeterm: can't write {}: {err}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_key_value_lines() {
        let e = Exports { session: "home".into(), animal: "🦊".into(), accent: Rgb(0xff, 0x7a, 0xb6), light: false };
        let text = e.render();
        assert!(text.contains("\nsession = home\n"));
        assert!(text.contains("\naccent = #ff7ab6\n"));
        assert!(text.contains("\ntheme = dark\n"));
    }
}
