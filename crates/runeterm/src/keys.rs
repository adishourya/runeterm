//! What runeterm's keys do: every action it has, the keys each is bound to, and the config's
//! `keybind = CHORD=ACTION` lines (Ghostty's syntax) that change them. The command palette
//! (⌥P) lists the same actions.

use rune::{Chord, KeyEvent, KeyPlatform, Tx};

/// Something runeterm can do from a key or the command palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    SplitRight,
    SplitDown,
    FocusLeft,
    FocusDown,
    FocusUp,
    FocusRight,
    SwapLeft,
    SwapDown,
    SwapUp,
    SwapRight,
    ResizeUp,
    ResizeLeft,
    ResizeDown,
    ResizeRight,
    Zoom,
    Close,
    Minimize,
    RestoreMinimized,
    Float,
    PinFloat,
    NewTab,
    NextTab,
    PrevTab,
    /// Tab 1 to 9.
    GotoTab(u8),
    Sessions,
    NewSession,
    RenameSession,
    ToggleSidebar,
    ToggleTheme,
    ToggleSmoothCursor,
    /// Every split of every tab at once, as miniatures of the tabs; choosing one goes there.
    Expose,
    /// Move the focused split to tab 1 to 9 (a new tab past the last one), or to a new tab.
    MoveToTab(u8),
    MoveToNewTab,
    /// Open one of the palette's lists of a setting's values (font weight, size…).
    Pick(Setting),
    /// A row of those lists: the setting and its value, in tenths for a number (87.5 is 875)
    /// or the choice's index. Not in the config or the main palette.
    Set(Setting, u32),
    /// The font size for now, not saved (Ghostty's ctrl+= and ctrl+-), and back to the config's.
    FontBigger,
    FontSmaller,
    FontReset,
    ToggleCopyOnSelect,
    ToggleConfirmClose,
    ToggleNotifications,
    /// Open the config in the editor beside the split; it reloads when the editor closes.
    EditConfig,
    EditScrollback,
    ReloadConfig,
    Copy,
    Paste,
    CommandPalette,
}

/// A setting the palette lists the values of, to try each and keep one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Setting {
    /// Regular text's weight (`font-variation`'s wght).
    Weight,
    /// Bold text's (`font-variation-bold`'s wght).
    BoldWeight,
    /// The width every style shares (`font-variation`'s wdth).
    Width,
    FontSize,
    /// `line-height` as a percent of the font's own…
    LinePercent,
    /// …or in pixels.
    LinePixels,
    /// `font-size-fit`, in percent.
    Fit,
    Padding,
    CursorStyle,
}

impl Setting {
    /// Its variable font axis, for the ones that are one.
    pub fn axis(self) -> Option<&'static str> {
        match self {
            Setting::Weight | Setting::BoldWeight => Some("wght"),
            Setting::Width => Some("wdth"),
            _ => None,
        }
    }

    /// A short name for keys and ids.
    pub fn name(self) -> &'static str {
        match self {
            Setting::Weight => "weight",
            Setting::BoldWeight => "bold-weight",
            Setting::Width => "width",
            Setting::FontSize => "font-size",
            Setting::LinePercent => "line-percent",
            Setting::LinePixels => "line-pixels",
            Setting::Fit => "fit",
            Setting::Padding => "padding",
            Setting::CursorStyle => "cursor-style",
        }
    }
}

/// Each action's name in the config and its title in the palette, in the palette's order.
const TABLE: &[(Action, &str, &str)] = &[
    (Action::CommandPalette, "command-palette", "Command palette"),
    (Action::SplitRight, "split-right", "Split right"),
    (Action::SplitDown, "split-down", "Split down"),
    (Action::Close, "close", "Close split"),
    (Action::Zoom, "zoom", "Full screen (zoom split)"),
    (Action::Minimize, "minimize", "Minimize split"),
    (Action::RestoreMinimized, "restore-minimized", "Restore minimized split"),
    (Action::Float, "float", "Float or dock split"),
    (Action::PinFloat, "pin-float", "Pin floating split across tabs"),
    (Action::FocusLeft, "focus-left", "Focus split left"),
    (Action::FocusDown, "focus-down", "Focus split below"),
    (Action::FocusUp, "focus-up", "Focus split above"),
    (Action::FocusRight, "focus-right", "Focus split right"),
    (Action::SwapLeft, "swap-left", "Swap split left"),
    (Action::SwapDown, "swap-down", "Swap split down"),
    (Action::SwapUp, "swap-up", "Swap split up"),
    (Action::SwapRight, "swap-right", "Swap split right"),
    (Action::ResizeUp, "resize-up", "Grow split upward"),
    (Action::ResizeLeft, "resize-left", "Grow split left"),
    (Action::ResizeDown, "resize-down", "Grow split downward"),
    (Action::ResizeRight, "resize-right", "Grow split right"),
    (Action::Expose, "expose", "Exposé: every split in every tab"),
    (Action::NewTab, "new-tab", "New tab"),
    (Action::NextTab, "next-tab", "Next tab"),
    (Action::PrevTab, "prev-tab", "Previous tab"),
    (Action::GotoTab(1), "goto-tab-1", "Go to tab 1"),
    (Action::GotoTab(2), "goto-tab-2", "Go to tab 2"),
    (Action::GotoTab(3), "goto-tab-3", "Go to tab 3"),
    (Action::GotoTab(4), "goto-tab-4", "Go to tab 4"),
    (Action::GotoTab(5), "goto-tab-5", "Go to tab 5"),
    (Action::GotoTab(6), "goto-tab-6", "Go to tab 6"),
    (Action::GotoTab(7), "goto-tab-7", "Go to tab 7"),
    (Action::GotoTab(8), "goto-tab-8", "Go to tab 8"),
    (Action::GotoTab(9), "goto-tab-9", "Go to tab 9"),
    (Action::MoveToTab(1), "move-to-tab-1", "Move split to tab 1"),
    (Action::MoveToTab(2), "move-to-tab-2", "Move split to tab 2"),
    (Action::MoveToTab(3), "move-to-tab-3", "Move split to tab 3"),
    (Action::MoveToTab(4), "move-to-tab-4", "Move split to tab 4"),
    (Action::MoveToTab(5), "move-to-tab-5", "Move split to tab 5"),
    (Action::MoveToTab(6), "move-to-tab-6", "Move split to tab 6"),
    (Action::MoveToTab(7), "move-to-tab-7", "Move split to tab 7"),
    (Action::MoveToTab(8), "move-to-tab-8", "Move split to tab 8"),
    (Action::MoveToTab(9), "move-to-tab-9", "Move split to tab 9"),
    (Action::MoveToNewTab, "move-to-new-tab", "Move split to a new tab"),
    (Action::Sessions, "sessions", "Sessions"),
    (Action::NewSession, "new-session", "New session"),
    (Action::RenameSession, "rename-session", "Rename session"),
    (Action::ToggleSidebar, "toggle-sidebar", "Pin or unpin the sidebar"),
    (Action::ToggleTheme, "toggle-theme", "Light or dark mode"),
    (Action::ToggleSmoothCursor, "toggle-smooth-cursor", "Smooth cursor on or off"),
    (Action::Pick(Setting::FontSize), "font-size", "Font size…"),
    (Action::FontBigger, "increase-font-size", "Bigger text (for now)"),
    (Action::FontSmaller, "decrease-font-size", "Smaller text (for now)"),
    (Action::FontReset, "reset-font-size", "Text back to the config's size"),
    (Action::Pick(Setting::LinePixels), "line-height", "Line height…"),
    (Action::Pick(Setting::Fit), "font-size-fit", "Font size fit (relax the size to fill splits)…"),
    (Action::Pick(Setting::Weight), "font-weight", "Font weight…"),
    (Action::Pick(Setting::BoldWeight), "bold-font-weight", "Bold font weight…"),
    (Action::Pick(Setting::Width), "font-width", "Font width (condensed, expanded)…"),
    (Action::Pick(Setting::Padding), "padding", "Padding around the text…"),
    (Action::Pick(Setting::CursorStyle), "cursor-style", "Cursor style…"),
    (Action::ToggleCopyOnSelect, "toggle-copy-on-select", "Copy on select on or off"),
    (Action::ToggleConfirmClose, "toggle-confirm-close", "Confirm closing busy splits on or off"),
    (Action::ToggleNotifications, "toggle-notifications", "Desktop notifications on or off"),
    (Action::EditConfig, "edit-config", "Edit config"),
    (Action::EditScrollback, "edit-scrollback", "Open scrollback in editor"),
    (Action::ReloadConfig, "reload-config", "Reload config"),
    (Action::Copy, "copy", "Copy selection"),
    (Action::Paste, "paste", "Paste"),
];

/// The keys runeterm starts with.
const DEFAULTS: &[(&str, Action)] = &[
    ("alt+p", Action::CommandPalette),
    ("alt+[", Action::SplitRight),
    ("alt+-", Action::SplitDown),
    ("alt+x", Action::Close),
    ("alt+z", Action::Zoom),
    ("alt+enter", Action::Zoom),
    ("alt+m", Action::Minimize),
    ("alt+shift+m", Action::RestoreMinimized),
    ("alt+f", Action::Float),
    ("alt+shift+f", Action::PinFloat),
    ("alt+h", Action::FocusLeft),
    ("alt+j", Action::FocusDown),
    ("alt+k", Action::FocusUp),
    ("alt+l", Action::FocusRight),
    ("alt+shift+h", Action::SwapLeft),
    ("alt+shift+j", Action::SwapDown),
    ("alt+shift+k", Action::SwapUp),
    ("alt+shift+l", Action::SwapRight),
    ("alt+w", Action::ResizeUp),
    ("alt+a", Action::ResizeLeft),
    ("alt+s", Action::ResizeDown),
    ("alt+d", Action::ResizeRight),
    ("alt+t", Action::NewTab),
    ("alt+tab", Action::Expose),
    ("alt+shift+1", Action::MoveToTab(1)),
    ("alt+shift+2", Action::MoveToTab(2)),
    ("alt+shift+3", Action::MoveToTab(3)),
    ("alt+shift+4", Action::MoveToTab(4)),
    ("alt+shift+5", Action::MoveToTab(5)),
    ("alt+shift+6", Action::MoveToTab(6)),
    ("alt+shift+7", Action::MoveToTab(7)),
    ("alt+shift+8", Action::MoveToTab(8)),
    ("alt+shift+9", Action::MoveToTab(9)),
    ("alt+shift+0", Action::MoveToNewTab),
    ("ctrl+=", Action::FontBigger),
    ("ctrl+plus", Action::FontBigger),
    ("ctrl+-", Action::FontSmaller),
    ("ctrl+0", Action::FontReset),
    ("alt+1", Action::GotoTab(1)),
    ("alt+2", Action::GotoTab(2)),
    ("alt+3", Action::GotoTab(3)),
    ("alt+4", Action::GotoTab(4)),
    ("alt+5", Action::GotoTab(5)),
    ("alt+6", Action::GotoTab(6)),
    ("alt+7", Action::GotoTab(7)),
    ("alt+8", Action::GotoTab(8)),
    ("alt+9", Action::GotoTab(9)),
    ("alt+/", Action::ToggleSidebar),
    ("alt+e", Action::EditScrollback),
    ("alt+r", Action::ReloadConfig),
    ("ctrl+shift+c", Action::Copy),
    ("ctrl+shift+v", Action::Paste),
];

impl Action {
    /// Every action, in the palette's order.
    pub fn all() -> impl Iterator<Item = Action> {
        TABLE.iter().map(|r| r.0)
    }

    pub fn name(self) -> &'static str {
        TABLE.iter().find(|r| r.0 == self).map_or("", |r| r.1)
    }

    pub fn title(self) -> &'static str {
        TABLE.iter().find(|r| r.0 == self).map_or("", |r| r.2)
    }

    /// A key for the palette's row: the action's name, or its weight for a weight row.
    pub fn id(self) -> String {
        match self {
            Action::Set(setting, v) => format!("{}-{v}", setting.name()),
            a => a.name().to_string(),
        }
    }

    pub fn named(name: &str) -> Option<Action> {
        TABLE.iter().find(|r| r.1 == name).map(|r| r.0)
    }
}

/// The keys bound to actions: the defaults, changed by the config's `keybind` lines.
#[derive(Clone, Debug)]
pub struct Keybinds {
    binds: Vec<(Chord, Action)>,
}

impl Default for Keybinds {
    fn default() -> Self {
        Keybinds { binds: DEFAULTS.iter().map(|(c, a)| (Chord::parse(c).expect("default chords parse"), *a)).collect() }
    }
}

impl Keybinds {
    /// One `keybind = …` value: `CHORD=ACTION` binds (replacing what the chord did),
    /// `CHORD=unbind` frees the chord for the terminal, `clear` drops every binding.
    pub fn apply(&mut self, value: &str) -> Result<(), String> {
        let value = value.trim();
        if value == "clear" {
            self.binds.clear();
            return Ok(());
        }
        // The last `=`, so a chord can end in one ("alt+==zoom").
        let (chord, action) = value.rsplit_once('=').ok_or_else(|| format!("`{value}` isn't CHORD=ACTION"))?;
        let chord = Chord::parse(chord.trim()).map_err(|e| e.to_string())?;
        let action = action.trim();
        self.binds.retain(|(c, _)| *c != chord);
        if action == "unbind" {
            return Ok(());
        }
        let action = Action::named(action).ok_or_else(|| format!("no action called `{action}`"))?;
        self.binds.push((chord, action));
        Ok(())
    }

    /// The action `e` is bound to.
    pub fn action(&self, e: &KeyEvent, tx: &Tx) -> Option<Action> {
        self.binds.iter().find(|(c, _)| c.matches_tx(e, tx)).map(|(_, a)| *a)
    }

    /// Whether `e` is one of `action`'s keys.
    pub fn is(&self, action: Action, e: &KeyEvent, tx: &Tx) -> bool {
        self.binds.iter().any(|(c, a)| *a == action && c.matches_tx(e, tx))
    }

    /// The first key bound to `action`, as glyphs ("⌥⇧F"), or "" when it has none.
    pub fn label(&self, action: Action) -> String {
        self.binds.iter().find(|(_, a)| *a == action).map(|(c, _)| c.label_on(KeyPlatform::Mac)).unwrap_or_default()
    }

    /// "Title  ⌥X", for a button's tooltip.
    pub fn hint(&self, title: &str, action: Action) -> String {
        let key = self.label(action);
        if key.is_empty() { title.to_string() } else { format!("{title}  {key}") }
    }

    /// The default bindings as config lines, grouped under each action.
    pub fn default_lines() -> String {
        let mut out = String::new();
        for a in Action::all() {
            for (c, b) in DEFAULTS {
                if *b == a {
                    out += &format!("keybind = {c}={}\n", a.name());
                }
            }
        }
        out
    }
}

/// How well `query` matches `title`: None when it doesn't, higher when better. A run of the
/// title scores highest (more at the start of a word); otherwise each typed character must
/// carry on from the last one matched or start a later word ("pfs" finds "Pin floating split").
pub fn fuzzy(query: &str, title: &str) -> Option<i32> {
    let title = title.to_lowercase();
    let query: String = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(0);
    }
    let starts = |chars: &[char], i: usize| i == 0 || matches!(chars[i - 1], ' ' | '-' | '(' | '·');
    let chars: Vec<char> = title.chars().collect();
    if let Some(at) = title.find(&query) {
        let at = title[..at].chars().count();
        return Some(1000 - at as i32 + if starts(&chars, at) { 500 } else { 0 });
    }
    let mut score = 500;
    let mut next = 0usize;
    for q in query.chars().filter(|c| !c.is_whitespace()) {
        let found = if next > 0 && chars.get(next) == Some(&q) {
            next
        } else {
            (next..chars.len()).find(|&i| chars[i] == q && starts(&chars, i))?
        };
        score -= (found - next) as i32;
        next = found + 1;
    }
    Some(score)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_default_names_a_listed_action() {
        let k = Keybinds::default();
        for (_, a) in &k.binds {
            assert!(!a.name().is_empty(), "{a:?}");
        }
        assert_eq!(k.label(Action::Minimize), "⌥M");
        assert_eq!(k.label(Action::PinFloat), "⌥⇧F");
        assert_eq!(k.label(Action::CommandPalette), "⌥P");
    }

    #[test]
    fn names_round_trip() {
        for a in Action::all() {
            assert_eq!(Action::named(a.name()), Some(a));
        }
    }

    #[test]
    fn keybind_lines_rebind_unbind_and_clear() {
        let mut k = Keybinds::default();
        k.apply("alt+n=minimize").unwrap();
        assert_eq!(k.label(Action::Minimize), "⌥M", "the default stays as well");
        k.apply("alt+m=unbind").unwrap();
        assert_eq!(k.label(Action::Minimize), "⌥N");
        k.apply("alt+n=float").unwrap();
        assert_eq!(k.label(Action::Minimize), "");
        assert!(k.apply("alt+n=nonsense").is_err());
        assert!(k.apply("nonsense").is_err());
        k.apply("clear").unwrap();
        assert!(k.binds.is_empty());
    }

    #[test]
    fn default_lines_read_back_as_the_defaults() {
        let mut k = Keybinds { binds: vec![] };
        for line in Keybinds::default_lines().lines() {
            k.apply(line.strip_prefix("keybind = ").unwrap()).unwrap();
        }
        let mut want = Keybinds::default().binds;
        let mut got = k.binds;
        let key = |b: &(Chord, Action)| format!("{}{:?}", b.0, b.1);
        want.sort_by_key(key);
        got.sort_by_key(key);
        assert_eq!(got, want);
    }

    #[test]
    fn fuzzy_prefers_word_starts() {
        assert!(fuzzy("min", "Minimize split").unwrap() > fuzzy("min", "Restore minimized split").unwrap());
        assert!(fuzzy("fl", "Float or dock split").is_some());
        assert!(fuzzy("flo", "Full screen (zoom split)").is_none());
        assert!(fuzzy("flsp", "Float or dock split").is_some());
        assert!(fuzzy("pfs", "Pin floating split across tabs").is_some());
        assert!(fuzzy("zzz", "Float or dock split").is_none());
        assert_eq!(fuzzy("", "anything"), Some(0));
    }
}
