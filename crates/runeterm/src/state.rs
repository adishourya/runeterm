//! App state and what the user can do to it: tabs, splits, focus, swapping, minimizing, and
//! keys to the focused pane.

use crate::config::Config;
use crate::keys::{Action, Keybinds, Setting};
use crate::pane::{Pane, Source, Theme};
use crate::spooky::{self, Controller, Event, SessionInfo};
use crate::split::{Dir, Node, Place, Side};
use crate::vt::{self, KeyEncoder};
use ghostty_vt_sys as sys;
use rune::{Tx, Waker};
use rune::edit::{Key, KeyEvent};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::sync::atomic::Ordering;

/// Wakes the UI from pty reader threads. Set once the window exists.
pub static WAKER: OnceLock<Waker> = OnceLock::new();

/// A split taken out of the layout, still running, shown as a pill until it's put back.
pub struct Minimized {
    pub id: u32,
    pub place: Option<Place>,
    /// Where the user dragged its pill, from its resting spot.
    pub offset: (f32, f32),
    /// The offset when the current drag started.
    pub drag_from: Option<(f32, f32)>,
    /// It floated before (where, and whether pinned): it comes back floating.
    pub floated: Option<([f32; 4], bool)>,
}

/// A split floating above the others in its tab: dragged by its header, resized from its
/// corner, docked back where it was.
pub struct Floating {
    pub id: u32,
    pub place: Option<Place>,
    /// x, y, width, height as fractions of the tab's area.
    pub rect: [f32; 4],
    /// The rect when the current drag started.
    pub drag_from: Option<[f32; 4]>,
    /// Pinned: it stays in view on every tab (it moves to whichever tab is shown).
    pub pinned: bool,
}

impl Floating {
    /// Moves (or, with `resize`, resizes) it by (`dx`, `dy`) fractions of the tab's area from
    /// where the drag started, kept inside the tab and no smaller than a fifth of it.
    pub fn drag(&mut self, dx: f32, dy: f32, resize: bool) {
        let [x, y, w, h] = self.drag_from.unwrap_or(self.rect);
        self.rect = if resize {
            [x, y, (w + dx).clamp(0.2, 1.0 - x), (h + dy).clamp(0.2, 1.0 - y)]
        } else {
            [(x + dx).clamp(0.0, 1.0 - w), (y + dy).clamp(0.0, 1.0 - h), w, h]
        };
    }
}

pub struct Tab {
    /// Stable for the tab's life (its view's key, so switching tabs can animate).
    pub id: u32,
    pub root: Node,
    pub focused: u32,
    pub zoomed: bool,
    pub minimized: Vec<Minimized>,
    pub floating: Vec<Floating>,
}

impl Tab {
    /// Every pane of the tab, in layout order, minimized ones last.
    pub fn panes(&self) -> Vec<u32> {
        let mut v = self.root.leaves();
        v.extend(self.floating.iter().map(|f| f.id));
        v.extend(self.minimized.iter().map(|m| m.id));
        v
    }

    pub fn has(&self, id: u32) -> bool {
        self.root.contains(id) || self.minimized.iter().any(|m| m.id == id) || self.is_floating(id)
    }

    pub fn is_floating(&self, id: u32) -> bool {
        self.floating.iter().any(|f| f.id == id)
    }
}

/// The command palette (⌥P): what's typed, and the row chosen with the arrows.
pub struct Palette {
    pub edit: rune::TextEdit,
    pub chosen: usize,
    /// Set while the palette lists a setting's values (font weights, sizes…) instead of
    /// actions.
    pub pick: Option<Pick>,
}

/// The palette's list of a setting's values, each tried as it's chosen and kept with Enter.
pub struct Pick {
    pub setting: Setting,
    /// The font's family, for saying it has no such axis.
    pub family: String,
    /// The values it takes: (min, default, max). None for a font axis the font lacks.
    pub range: Option<(f32, f32, f32)>,
    /// The value in the config now (a choice's index for the cursor style).
    pub current: f32,
    /// A value is being shown that isn't saved: put the config's back when closed.
    pub previewed: bool,
}

/// `cursor-style`'s values and their names in the palette.
const CURSOR_STYLES: [(&str, &str); 4] = [("bar", "Bar"), ("block", "Block"), ("underline", "Underline"), ("block_hollow", "Hollow block")];

fn cursor_index(c: vt::CursorStyle) -> usize {
    match c {
        vt::CursorStyle::Bar => 0,
        vt::CursorStyle::Block => 1,
        vt::CursorStyle::Underline => 2,
        vt::CursorStyle::BlockHollow => 3,
    }
}

/// A font axis value's usual name ("Light" for wght 300, "SemiCondensed" for wdth 87.5), or
/// the axis's between them.
fn axis_name(setting: Setting, v: f32) -> &'static str {
    let (named, between): (&[(f32, &str)], &str) = match setting {
        Setting::Width => (&[(50.0, "UltraCondensed"), (62.5, "ExtraCondensed"), (75.0, "Condensed"), (87.5, "SemiCondensed"),
            (100.0, "Normal"), (112.5, "SemiExpanded"), (125.0, "Expanded"), (150.0, "ExtraExpanded"), (200.0, "UltraExpanded")], "Width"),
        _ => (&[(100.0, "Thin"), (200.0, "ExtraLight"), (300.0, "Light"), (400.0, "Regular"), (500.0, "Medium"),
            (600.0, "SemiBold"), (700.0, "Bold"), (800.0, "ExtraBold"), (900.0, "Black")], "Weight"),
    };
    named.iter().find(|(n, _)| *n == v).map_or(between, |(_, name)| name)
}

/// Tenths, as the palette's rows carry values (87.5 is 875).
fn tenths(v: f32) -> u32 {
    (v * 10.0).round().max(0.0) as u32
}

impl Pick {
    /// What the palette's field says while it lists this setting.
    pub fn prompt(&self) -> &'static str {
        match self.setting {
            Setting::Weight => "Weight for regular text (or type one)…",
            Setting::BoldWeight => "Weight for bold text (or type one)…",
            Setting::Width => "Width for all text (or type one)…",
            Setting::FontSize => "Font size in points (or type one)…",
            Setting::LinePercent => "Extra line height, % of the font's (or type 4px)…",
            Setting::LinePixels => "Extra line height in pixels (or type 20%)…",
            Setting::Fit => "How far the size may relax so lines fill a split…",
            Setting::Padding => "Space around the text, in points (or type one)…",
            Setting::CursorStyle => "Cursor style…",
        }
    }

    /// The values to offer: the usual ones in range, then (for a font axis) the range's ends
    /// and default.
    fn values(&self) -> Vec<f32> {
        let Some((lo, def, hi)) = self.range else { return vec![] };
        let usual: Vec<f32> = match self.setting {
            Setting::Weight | Setting::BoldWeight => (1..=9).map(|n| n as f32 * 100.0).collect(),
            Setting::Width => vec![50.0, 62.5, 75.0, 87.5, 100.0, 112.5, 125.0, 150.0, 200.0],
            Setting::FontSize => vec![8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0, 17.0, 18.0, 19.0, 20.0, 21.0, 22.0, 24.0, 26.0, 28.0, 32.0, 36.0, 40.0, 48.0],
            Setting::LinePercent => (0..=20).map(|n| n as f32 * 5.0).collect(),
            Setting::LinePixels => (0..=24).map(|n| n as f32 * 2.0).collect(),
            Setting::Fit => vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0, 12.0, 15.0],
            Setting::Padding => vec![0.0, 2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 14.0, 16.0, 20.0, 24.0, 32.0],
            Setting::CursorStyle => (0..CURSOR_STYLES.len()).map(|i| i as f32).collect(),
        };
        let mut vs: Vec<f32> = usual.into_iter().filter(|v| (lo..=hi).contains(v)).collect();
        if self.setting.axis().is_some() {
            vs.extend([lo, def, hi]);
        }
        vs
    }

    /// The default to mark: the font's for an axis, runeterm's for the rest.
    fn default(&self) -> Option<f32> {
        match self.setting {
            Setting::LinePixels => None,
            Setting::FontSize => Some(13.0),
            Setting::Padding => Some(10.0),
            Setting::CursorStyle => Some(0.0),
            _ => self.range.map(|r| r.1),
        }
    }
}

/// A row's title: what the value is called.
fn value_title(setting: Setting, v: f32) -> String {
    match setting {
        Setting::Weight | Setting::BoldWeight | Setting::Width => format!("{} · {v}", axis_name(setting, v)),
        Setting::FontSize | Setting::Padding => format!("{v} pt"),
        Setting::LinePercent => format!("{v}% taller than the font's own"),
        Setting::LinePixels => format!("{v} px"),
        Setting::Fit if v == 0.0 => "Off: the exact size".into(),
        Setting::Fit => format!("Up to {v}%"),
        Setting::CursorStyle => CURSOR_STYLES.get(v as usize).map_or("", |c| c.1).into(),
    }
}

/// `c` with `setting` at `v`, as it'd be once saved.
fn apply_setting(c: &mut Config, setting: Setting, v: f32) {
    let f = &mut c.font;
    match setting {
        Setting::Weight => f.variations[0] = crate::grid::with_axis(&f.variations[0], "wght", v),
        Setting::BoldWeight => f.variations[1] = crate::grid::with_axis(&f.variations[1], "wght", v),
        Setting::Width => f.variations[0] = crate::grid::with_axis(&f.variations[0], "wdth", v),
        Setting::FontSize => f.size = v,
        Setting::LinePercent => (f.cell_height, f.cell_height_px) = (v / 100.0, 0.0),
        Setting::LinePixels => (f.cell_height, f.cell_height_px) = (0.0, v),
        Setting::Fit => f.fit = v / 100.0,
        Setting::Padding => f.padding = v,
        Setting::CursorStyle => {
            c.cursor_style = match v as usize {
                1 => vt::CursorStyle::Block,
                2 => vt::CursorStyle::Underline,
                3 => vt::CursorStyle::BlockHollow,
                _ => vt::CursorStyle::Bar,
            }
        }
    }
}

/// Writes `setting` at `v` to the config file: font axes into their `font-variation` line
/// (other axes kept), the rest as their own key.
fn save_setting(setting: Setting, v: f32) -> std::io::Result<()> {
    let axes = |i: usize, tag: &str| {
        let vs = crate::grid::with_axis(&Config::load().font.variations[i], tag, v);
        vs.iter().map(|(t, v)| format!("{t}={v}")).collect::<Vec<_>>().join(", ")
    };
    match setting {
        Setting::Weight => crate::config::replace("font-variation", &axes(0, "wght")),
        Setting::BoldWeight => crate::config::replace("font-variation-bold", &axes(1, "wght")),
        Setting::Width => crate::config::replace("font-variation", &axes(0, "wdth")),
        Setting::FontSize => crate::config::set("font-size", &v.to_string()),
        Setting::LinePercent => crate::config::set("line-height", &format!("{v}%")),
        Setting::LinePixels => crate::config::set("line-height", &v.to_string()),
        Setting::Fit => crate::config::set("font-size-fit", &format!("{v}%")),
        Setting::Padding => crate::config::set("padding", &v.to_string()),
        Setting::CursorStyle => crate::config::set("cursor-style", CURSOR_STYLES.get(v as usize).map_or("bar", |c| c.0)),
    }
}

/// How many tabs the exposé puts side by side: a square-ish grid.
pub fn expose_cols(tabs: usize) -> usize {
    ((tabs.max(1) as f32).sqrt().ceil() as usize).max(1)
}

/// What the session picker shows.
#[derive(Clone, Debug, PartialEq)]
pub enum PickerMode {
    List,
    /// Choosing an emoji for a session, from spooky's list of (glyph, name).
    Emoji { session: String, animals: Vec<(String, String)> },
    /// Asking before ending a session's panes.
    ConfirmKill(String),
    /// Renaming a session (the new name is typed into `State::rename_edit`), and why the last
    /// try was refused.
    Rename { session: String, error: Option<String> },
}

/// Why `name` can't be a session's name, if it can't.
pub fn session_name_problem(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("A name can't be empty")
    } else if name.chars().count() > 32 {
        Some("At most 32 characters")
    } else if name.chars().any(|c| c.is_whitespace() || c == '/' || c.is_control()) {
        Some("No spaces or slashes")
    } else if name.starts_with('-') {
        Some("It can't start with a dash")
    } else {
        None
    }
}

/// The spooky session this window shows.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Session {
    pub name: String,
    /// An emoji.
    pub animal: String,
}

pub struct State {
    pub theme: Theme,
    pub panes: HashMap<u32, Pane>,
    pub tabs: Vec<Tab>,
    pub active: usize,
    /// Which way the last tab switch went: 1 down the list, -1 up.
    pub slide: f32,
    next_tab: u32,
    /// `user@host`, for the sidebar.
    pub user_host: String,
    /// The editor ⌥E opens the scrollback in.
    pub scrollback_editor: String,
    /// Mouse selection per pane, mouse reports for programs that asked, and the last selected
    /// text (middle-click pastes it, like X's primary selection).
    gestures: HashMap<u32, vt::Gesture>,
    mouse: vt::MouseEncoder,
    primary: String,
    /// The sidebar's appearance section shows its swatches.
    pub appearance_open: bool,
    /// A split asked to close while running something, waiting for a yes.
    pub confirm_close: Option<u32>,
    /// Panes a desktop notification went out for, until they're seen.
    notified: std::collections::HashSet<u32>,
    pub settings: Behaviour,
    /// The exposé (⌥Tab), while it's open: the split chosen in it.
    pub expose: Option<u32>,
    /// Points added to the config's font size for now (ctrl+= and ctrl+-), not saved.
    pub font_delta: f32,
    /// The split editing the config (Edit config): the config reloads when it closes.
    pub config_editor: Option<u32>,
    /// Appearance: light mode, the accent, and the accents to choose from.
    pub light: bool,
    /// The accent shown: this session's own, or `default_accent` outside a session.
    pub accent: crate::vt::Rgb,
    /// The config's accent.
    pub default_accent: crate::vt::Rgb,
    pub accents: Vec<crate::vt::Rgb>,
    /// The session `accent` was looked up for.
    accent_for: Option<String>,
    /// What this window last wrote to `~/.rune_state_exports`; cleared when it loses focus, so
    /// coming back rewrites it over whatever another window wrote meanwhile.
    exported: Option<crate::exports::Exports>,
    /// The spooky session this window is, once the daemon has named it.
    pub session: Option<Session>,
    /// The session whose emoji was last checked for being an animal (see `give_animal`).
    animal_checked: Option<String>,
    /// The name being typed while renaming a session.
    pub rename_edit: rune::TextEdit,
    /// Wheel movement not yet a whole line, by pane, in lines.
    wheel_rest: HashMap<u32, f32>,
    /// The size of the area the splits are laid out in (points), for floating splits.
    pub stage: [f32; 2],
    /// Each split's length along its direction, less the gap (points), by (tab, path): to turn a
    /// distance into a ratio when snapping dividers to whole lines.
    pub split_lens: HashMap<(u32, Vec<bool>), f32>,
    /// A divider is being dragged: no snapping until it's let go.
    pub dragging_divider: bool,
    /// The snaps planned, and since when the plan has held (they wait for the layout to rest).
    snap_seen: Option<(Vec<(Vec<bool>, i32)>, f64)>,
    /// Smooth scrolling through each pane's scrollback, while it moves.
    scrollers: HashMap<u32, crate::smooth::Scroller>,
    /// The app's clock at the last poll (seconds), for scrolling's timing.
    now: f64,
    /// Whether the window has keyboard focus (for the cursor and shaders' `iFocus`).
    pub window_focused: bool,
    /// The sidebar is expanded (hovered, or the session picker is open).
    pub sidebar_open: bool,
    /// The sidebar's settings, and the time and battery it shows.
    pub sidebar: crate::config::Sidebar,
    pub status: crate::status::Status,
    last_status: f64,
    /// The session picker, when open: every session the daemon knows, and what it shows.
    pub picker: Option<Vec<SessionInfo>>,
    /// Each listed session's own accent, for the picker's tiles.
    pub picker_accents: HashMap<String, crate::vt::Rgb>,
    /// A new window asking which session to be: the picker shows centred, and this is the row
    /// chosen with the arrow keys (0 is "New session").
    pub chooser: Option<usize>,
    pub picker_mode: PickerMode,
    /// The daemon connection; None runs panes on local ptys instead.
    controller: Option<Controller>,
    /// This window's id for spooky. A window that switches sessions takes a new one, so the
    /// daemon sees the old session's window close.
    window_id: u64,
    /// The session this window last asked for and hasn't opened yet.
    pending_open: Option<String>,
    /// Every session asked for whose window hasn't come back: replies to superseded requests
    /// are dropped, and only a window nobody here asked for opens a new runeterm.
    requested: Vec<String>,
    last_report: String,
    /// Each daemon pane's foreground command, refreshed in the background.
    commands: Arc<Mutex<HashMap<u32, String>>>,
    next_id: u32,
    keys: KeyEncoder,
    last_meta: f64,
    /// What runeterm's own keys do (the config's `keybind` lines).
    pub keybinds: Keybinds,
    /// The command palette, while it's open.
    pub palette: Option<Palette>,
}

impl State {
    /// With `use_spooky`, panes live in the spooky daemon and this window is a session: `attach`
    /// names one to open, otherwise a new one starts.
    pub fn new(config: &Config, use_spooky: bool, attach: Option<String>) -> State {
        let theme = config.colors();
        let controller = if use_spooky {
            match Controller::connect(wake) {
                Ok(c) => Some(c),
                Err(e) => {
                    eprintln!("runeterm: spooky isn't available ({e}); splits won't outlive this window");
                    None
                }
            }
        } else {
            None
        };
        let commands = Arc::new(Mutex::new(HashMap::new()));
        if controller.is_some() {
            let c = commands.clone();
            std::thread::Builder::new()
                .name("spooky-panes".into())
                .spawn(move || loop {
                    let next = spooky::pane_commands();
                    let changed = *c.lock().unwrap() != next;
                    if changed {
                        *c.lock().unwrap() = next;
                        wake();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(800));
                })
                .ok();
        }
        let mut s = State {
            theme,
            panes: HashMap::new(),
            tabs: vec![],
            active: 0,
            slide: 1.0,
            next_tab: 0,
            user_host: user_host(),
            scrollback_editor: config.scrollback_editor.clone(),
            gestures: HashMap::new(),
            mouse: vt::MouseEncoder::default(),
            primary: String::new(),
            confirm_close: None,
            appearance_open: false,
            notified: Default::default(),
            settings: Behaviour::from(config),
            font_delta: 0.0,
            config_editor: None,
            expose: None,
            light: config.light,
            accent: config.accent,
            default_accent: config.accent,
            accents: config.accents.clone(),
            accent_for: None,
            exported: None,
            session: None,
            animal_checked: None,
            rename_edit: rune::TextEdit::new(""),
            chooser: None,
            picker_accents: HashMap::new(),
            wheel_rest: HashMap::new(),
            scrollers: HashMap::new(),
            split_lens: HashMap::new(),
            dragging_divider: false,
            snap_seen: None,
            stage: [1.0, 1.0],
            now: 0.0,
            window_focused: true,
            sidebar_open: false,
            sidebar: config.sidebar,
            status: crate::status::read(),
            last_status: 0.0,
            picker: None,
            picker_mode: PickerMode::List,
            controller,
            window_id: spooky::fresh_id(),
            pending_open: None,
            requested: vec![],
            last_report: String::new(),
            commands,
            next_id: 1,
            keys: KeyEncoder::default(),
            last_meta: -1.0,
            keybinds: config.keybinds.clone(),
            palette: None,
        };
        match attach {
            Some(name) if s.controller.is_some() => s.open_session(&name),
            // Sessions to go back to: ask which (or a new one) first.
            _ if s.controller.is_some() && spooky::sessions().iter().any(|i| i.state != "open") => {
                s.picker = Some(s.load_sessions());
                s.chooser = Some(0);
            }
            _ => s.new_tab(None),
        }
        s
    }

    pub fn uses_spooky(&self) -> bool {
        self.controller.is_some()
    }

    fn spawn(&mut self, cwd: Option<String>) -> Option<u32> {
        let session = self.session.as_ref().map(|s| s.name.clone()).unwrap_or_default();
        let source = if self.controller.is_some() {
            Source::New { cwd: cwd.as_deref(), session: &session }
        } else {
            Source::Local { cwd: cwd.as_deref(), command: None }
        };
        let id = self.next_id;
        self.next_id += 1;
        match Pane::spawn(id, source, &self.theme, Some(Arc::new(wake))) {
            Ok(p) => {
                self.panes.insert(id, p);
                Some(id)
            }
            Err(e) => {
                eprintln!("runeterm: can't start a shell: {e}");
                None
            }
        }
    }

    fn tab_id(&mut self) -> u32 {
        self.next_tab += 1;
        self.next_tab
    }

    /// Switches tabs. The new one slides in from below when it's further down the list, from
    /// above when it's further up.
    pub fn select_tab(&mut self, i: usize) {
        if i != self.active && i < self.tabs.len() {
            self.slide = if i > self.active { 1.0 } else { -1.0 };
            self.carry_pinned(self.active, i);
            self.active = i;
        }
    }

    /// Moves the pinned floating splits from tab `from` to tab `to`, so they stay in view. One
    /// that had focus keeps it.
    fn carry_pinned(&mut self, from: usize, to: usize) {
        let Some(tab) = self.tabs.get_mut(from) else { return };
        if !tab.floating.iter().any(|f| f.pinned) {
            return;
        }
        let (pinned, rest): (Vec<Floating>, Vec<Floating>) = std::mem::take(&mut tab.floating).into_iter().partition(|f| f.pinned);
        tab.floating = rest;
        let had_focus = pinned.iter().any(|f| f.id == tab.focused).then_some(tab.focused);
        if had_focus.is_some() {
            tab.focused = tab.root.first();
            tab.zoomed = false;
        }
        let Some(dest) = self.tabs.get_mut(to) else { return };
        dest.floating.extend(pinned);
        if let Some(id) = had_focus {
            dest.focused = id;
            dest.zoomed = false;
        }
    }

    /// Switches to the tab `by` away from this one, wrapping around.
    pub fn cycle_tab(&mut self, by: isize) {
        let n = self.tabs.len() as isize;
        if n > 1 {
            let i = (self.active as isize + by).rem_euclid(n) as usize;
            self.select_tab(i);
        }
    }

    pub fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn focused_id(&self) -> Option<u32> {
        self.tab().map(|t| t.focused)
    }

    pub fn focused_pane(&self) -> Option<&Pane> {
        self.panes.get(&self.focused_id()?)
    }

    /// The focused pane's directory, for new panes to start in.
    fn focused_cwd(&self) -> Option<String> {
        let p = self.focused_pane()?;
        let home = std::env::var("HOME").unwrap_or_default();
        Some(p.cwd.replacen('~', &home, 1))
    }

    pub fn new_tab_here(&mut self) {
        let cwd = self.focused_cwd();
        self.new_tab(cwd);
    }

    pub fn new_tab(&mut self, cwd: Option<String>) {
        if let Some(id) = self.spawn(cwd) {
            let tab = self.tab_id();
            self.tabs.push(Tab { id: tab, root: Node::Leaf(id), focused: id, zoomed: false, minimized: vec![], floating: vec![] });
            self.select_tab(self.tabs.len() - 1);
        }
    }

    /// Focuses a pane wherever it is: its tab becomes active, and a minimized one comes back.
    pub fn focus(&mut self, id: u32) {
        let Some(t) = self.tabs.iter().position(|t| t.has(id)) else { return };
        self.select_tab(t);
        if self.tabs[t].minimized.iter().any(|m| m.id == id) {
            self.restore(id);
        }
        let tab = &mut self.tabs[t];
        if tab.focused != id {
            tab.zoomed = false;
        }
        tab.focused = id;
        if let Some(p) = self.panes.get(&id) {
            p.attention.store(false, Ordering::Release);
        }
    }

    pub fn split(&mut self, dir: Dir) {
        let cwd = self.focused_cwd();
        let Some(id) = self.spawn(cwd) else { return };
        let tab = &mut self.tabs[self.active];
        tab.root.split(tab.focused, id, dir);
        tab.focused = id;
        tab.zoomed = false;
    }

    fn attach(&mut self, pane: u32) -> Option<u32> {
        let id = self.next_id;
        self.next_id += 1;
        match Pane::spawn(id, Source::Attach(pane), &self.theme, Some(Arc::new(wake))) {
            Ok(p) => {
                self.panes.insert(id, p);
                Some(id)
            }
            Err(e) => {
                eprintln!("runeterm: can't attach to spooky pane {pane}: {e}");
                None
            }
        }
    }

    /// Closes a split, asking first when something other than the shell is running in it.
    pub fn request_close(&mut self, id: u32) {
        let busy = self.panes.get(&id).is_some_and(|p| !crate::icons::is_shell(&p.program));
        if busy && self.settings.confirm_close {
            self.focus(id);
            self.confirm_close = Some(id);
        } else {
            self.close(id);
        }
    }

    /// Closes a split: spooky ends its pane too. Its tab closes with its last split, and the
    /// app with its last tab.
    pub fn close(&mut self, id: u32) {
        if let (Some(c), Some(p)) = (&self.controller, self.panes.get(&id)) {
            if !p.stolen.load(Ordering::Acquire) {
                c.closed(&[p.surface]);
            }
        }
        self.forget(id);
    }

    /// Drops a split from the window without ending its pane (it ended, or another window
    /// took it).
    fn forget(&mut self, id: u32) {
        self.wheel_rest.remove(&id);
        self.scrollers.remove(&id);
        if let (Some(g), Some(p)) = (self.gestures.remove(&id), self.panes.get(&id)) {
            g.free(&p.term.lock().unwrap());
        }
        self.notified.remove(&id);
        if self.confirm_close == Some(id) {
            self.confirm_close = None;
        }
        self.panes.remove(&id); // Dropping the connection detaches (or hangs up a local shell).
        let Some(t) = self.tabs.iter().position(|t| t.has(id)) else { return };
        let tab = &mut self.tabs[t];
        tab.minimized.retain(|m| m.id != id);
        tab.floating.retain(|f| f.id != id);
        if !tab.root.contains(id) {
            if tab.focused == id {
                tab.focused = tab.root.first();
            }
            return;
        }
        match tab.root.remove(id) {
            Some(rest) => {
                tab.root = rest;
                if tab.focused == id {
                    tab.focused = tab.root.first();
                }
                tab.zoomed = false;
            }
            None => {
                // The last split in view: bring back a minimized one, or close the tab.
                if let Some(m) = tab.minimized.pop() {
                    tab.root = Node::Leaf(m.id);
                    tab.focused = m.id;
                    return;
                }
                if let Some(f) = tab.floating.pop() {
                    tab.root = Node::Leaf(f.id);
                    tab.focused = f.id;
                    return;
                }
                self.tabs.remove(t);
                if self.tabs.is_empty() {
                    std::process::exit(0);
                }
                self.active = self.active.min(self.tabs.len() - 1);
            }
        }
    }

    /// Takes a split out of the layout; it keeps running and waits as a pill.
    pub fn minimize(&mut self, id: u32) {
        // A floating split waits as a pill too, and comes back floating where it was.
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.is_floating(id)) {
            let i = tab.floating.iter().position(|f| f.id == id).unwrap();
            let f = tab.floating.remove(i);
            tab.minimized.push(Minimized { id, place: f.place, offset: (0.0, 0.0), drag_from: None, floated: Some((f.rect, f.pinned)) });
            if tab.focused == id {
                tab.focused = tab.root.first();
            }
            tab.zoomed = false;
            return;
        }
        let Some(tab) = self.tabs.iter_mut().find(|t| t.root.contains(id)) else { return };
        let place = tab.root.place_of(id);
        let Some(rest) = tab.root.remove(id) else { return }; // never the last one in view
        tab.root = rest;
        tab.minimized.push(Minimized { id, place, offset: (0.0, 0.0), drag_from: None, floated: None });
        if tab.focused == id {
            tab.focused = place.map_or(tab.root.first(), |p| p.beside);
        }
        tab.zoomed = false;
    }

    /// Floats split `id` above the others (never the last one in the layout), or docks it back
    /// if it floats.
    pub fn toggle_float(&mut self, id: u32) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.has(id)) else { return };
        if let Some(i) = tab.floating.iter().position(|f| f.id == id) {
            let f = tab.floating.remove(i);
            let placed = f.place.is_some_and(|p| tab.root.insert(p.beside, id, p.dir, p.share, p.first));
            if !placed {
                let at = tab.root.first();
                tab.root.insert(at, id, Dir::Right, 0.5, false);
            }
        } else {
            if !tab.root.contains(id) {
                return;
            }
            let place = tab.root.place_of(id);
            let Some(rest) = tab.root.remove(id) else { return };
            tab.root = rest;
            // Each new one a little lower and to the right of the last.
            let k = (tab.floating.len() % 5) as f32 * 0.04;
            tab.floating.push(Floating { id, place, rect: [0.18 + k, 0.14 + k, 0.6, 0.62], drag_from: None, pinned: false });
        }
        tab.focused = id;
        tab.zoomed = false;
    }

    /// Pins a floating split so it stays in view on every tab, or unpins it (it stays in the
    /// tab shown). A split in the layout floats first.
    pub fn toggle_pin(&mut self, id: u32) {
        let floating = self.tabs.iter().any(|t| t.is_floating(id));
        if !floating {
            self.toggle_float(id);
        }
        let Some(f) = self.tabs.iter_mut().flat_map(|t| t.floating.iter_mut()).find(|f| f.id == id) else { return };
        // Floated just now: pinning is why.
        f.pinned = !floating || !f.pinned;
    }

    pub fn is_pinned(&self, id: u32) -> bool {
        self.tabs.iter().flat_map(|t| &t.floating).any(|f| f.id == id && f.pinned)
    }

    /// Brings back the active tab's most recently minimized split.
    pub fn restore_last(&mut self) {
        if let Some(id) = self.tab().and_then(|t| t.minimized.last()).map(|m| m.id) {
            self.restore(id);
        }
    }

    /// Puts a minimized split back where it was (or beside the focused one), floating again if
    /// it floated.
    pub fn restore(&mut self, id: u32) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.minimized.iter().any(|m| m.id == id)) else { return };
        let i = tab.minimized.iter().position(|m| m.id == id).unwrap();
        let m = tab.minimized.remove(i);
        if let Some((rect, pinned)) = m.floated {
            tab.floating.push(Floating { id, place: m.place, rect, drag_from: None, pinned });
            tab.focused = id;
            tab.zoomed = false;
            return;
        }
        let placed = m.place.is_some_and(|p| tab.root.insert(p.beside, id, p.dir, p.share, p.first));
        if !placed {
            let at = tab.focused;
            tab.root.insert(at, id, Dir::Right, 0.5, false);
        }
        tab.focused = id;
        tab.zoomed = false;
    }

    fn focus_dir(&mut self, side: Side) {
        let tab = &self.tabs[self.active];
        // From a floating split, arrows go back to the layout.
        if tab.is_floating(tab.focused) {
            let first = tab.root.first();
            self.focus(first);
            return;
        }
        if let Some(id) = tab.root.neighbor(tab.focused, side) {
            self.focus(id);
        }
    }

    /// Swaps the focused split with its neighbour on `side`; focus moves with it.
    fn swap_dir(&mut self, side: Side) {
        let tab = &mut self.tabs[self.active];
        if let Some(other) = tab.root.neighbor(tab.focused, side) {
            tab.root.swap(tab.focused, other);
            tab.zoomed = false;
        }
    }

    pub fn toggle_zoom(&mut self, id: u32) {
        self.focus(id);
        let t = &mut self.tabs[self.active];
        t.zoomed = !t.zoomed && t.root.leaves().len() > 1;
    }

    pub fn needs_attention(&self, id: u32) -> bool {
        self.panes.get(&id).is_some_and(|p| p.attention.load(Ordering::Acquire))
    }

    /// Before each frame: new output, exited shells, and (twice a second) programs and directories.
    pub fn poll(&mut self, t: f64) -> bool {
        let mut changed = false;
        self.now = t;
        changed |= self.handle_events();
        changed |= self.step_scrolling();
        changed |= self.snap_dividers(t);
        changed |= self.follow_session_accent();
        self.give_animal();
        self.export();
        let exited: Vec<u32> = self.panes.values().filter(|p| p.exited.load(Ordering::Acquire)).map(|p| p.id).collect();
        for id in exited {
            self.forget(id);
            if self.config_editor == Some(id) {
                self.config_editor = None;
                self.reload_config();
            }
            changed = true;
        }
        for p in self.panes.values() {
            changed |= p.dirty.load(Ordering::Acquire);
        }
        let meta_due = t - self.last_meta > 0.5;
        let commands = self.commands.lock().unwrap().clone();
        for p in self.panes.values_mut() {
            let flagged = p.meta_changed.swap(false, Ordering::AcqRel);
            if meta_due || flagged {
                changed |= p.refresh_meta(Some(&commands)) || flagged;
            }
        }
        if meta_due {
            self.last_meta = t;
        }
        if t - self.last_status >= 10.0 || t < self.last_status {
            self.last_status = t;
            let next = crate::status::read();
            if next != self.status {
                self.status = next;
                changed = true;
            }
        }
        self.report();
        self.notify();
        changed
    }

    /// Tells spooky this window's tabs and splits whenever they change, so the session's
    /// layout is remembered (and restored after a reboot).
    fn report(&mut self) {
        let Some(c) = &self.controller else { return };
        fn node(n: &Node, panes: &HashMap<u32, Pane>) -> spooky::Node {
            match n {
                Node::Leaf(id) => spooky::Node { surface: Some(panes.get(id).map_or(0, |p| p.surface)), ratio: 0.5, ..Default::default() },
                Node::Split { dir, ratio, a, b } => spooky::Node {
                    split: Some(if *dir == Dir::Right { "h" } else { "v" }.into()),
                    ratio: *ratio,
                    left: Some(Box::new(node(a, panes))),
                    right: Some(Box::new(node(b, panes))),
                    ..Default::default()
                },
            }
        }
        // Minimized and floating splits are still part of the session: report them where they
        // were, so they stay in it and come back as splits when the session is restored.
        let tabs: Vec<spooky::Node> = self
            .tabs
            .iter()
            .map(|t| {
                let mut root = t.root.clone();
                let away = t.minimized.iter().map(|m| (m.id, m.place)).chain(t.floating.iter().map(|f| (f.id, f.place)));
                for (id, place) in away {
                    let placed = place.is_some_and(|p| root.insert(p.beside, id, p.dir, p.share, p.first));
                    if !placed {
                        let at = root.first();
                        root.insert(at, id, Dir::Right, 0.5, false);
                    }
                }
                node(&root, &self.panes)
            })
            .collect();
        let windows = if tabs.is_empty() {
            vec![]
        } else {
            vec![spooky::Window { id: self.window_id, active: self.active as u32, tabs }]
        };
        let report = spooky::Report { windows };
        let json = serde_json::to_string(&report).unwrap_or_default();
        if json != self.last_report {
            c.report(&report);
            self.last_report = json;
        }
    }

    /// Handles what the daemon sent: a session's window to open here, a window to raise,
    /// this window's session name.
    fn handle_events(&mut self) -> bool {
        let Some(c) = &self.controller else { return false };
        let events = std::mem::take(&mut *c.events.lock().unwrap());
        let changed = !events.is_empty();
        for e in events {
            match e {
                Event::OpenWindow(open) => {
                    let asked = self.requested.iter().position(|r| *r == open.session);
                    if let Some(i) = asked {
                        self.requested.remove(i);
                    }
                    if self.pending_open.as_deref() == Some(open.session.as_str()) {
                        self.pending_open = None;
                        self.take_over(open);
                    } else if asked.is_none() {
                        // Nobody here asked (e.g. `spooky attach NAME` in a shell): it gets a
                        // window of its own.
                        if let Ok(exe) = std::env::current_exe() {
                            let _ = std::process::Command::new(exe).args(["attach", &open.session]).spawn();
                        }
                    }
                }
                Event::PresentWindow(_) => {
                    // The session is shown in a window already (that one comes forward): the
                    // request is answered, and this window stays as it is.
                    if let Some(p) = self.pending_open.take() {
                        self.requested.retain(|r| *r != p);
                    }
                    // Still choosing: ask again.
                    if self.chooser.is_some() && self.tabs.is_empty() {
                        self.picker = Some(self.load_sessions());
                    }
                }
                Event::Surfaces(s) => {
                    if let Some(w) = s.windows.iter().find(|w| w.id == self.window_id) {
                        let next = Session { name: w.session.clone(), animal: w.animal.clone().unwrap_or_default() };
                        if self.session.as_ref() != Some(&next) {
                            self.session = Some(next);
                        }
                    }
                }
                Event::Gone => {
                    eprintln!("runeterm: lost the spooky daemon");
                    self.controller = None;
                    break;
                }
            }
        }
        if self.tabs.is_empty() && self.pending_open.is_none() && self.chooser.is_none() {
            self.new_tab(None);
        }
        changed
    }

    /// A session with a sport for its emoji gets an animal instead, one no other session has if
    /// there's one left. Once per session; the daemon then reports the new emoji.
    fn give_animal(&mut self) {
        let Some(sess) = &self.session else { return };
        if sess.animal.is_empty() || self.animal_checked.as_ref() == Some(&sess.name) || self.controller.is_none() {
            return;
        }
        self.animal_checked = Some(sess.name.clone());
        let animals = spooky::animals();
        if animals.is_empty() || animals.iter().any(|(glyph, _)| *glyph == sess.animal) {
            return;
        }
        let taken: Vec<String> = spooky::sessions().into_iter().map(|i| i.animal).collect();
        let Some((_, pick)) = animals.iter().find(|(glyph, _)| !taken.contains(glyph)).or(animals.first()) else { return };
        if let Err(e) = spooky::set_animal(&sess.name, pick) {
            eprintln!("runeterm: can't give {} an animal: {e}", sess.name);
        }
    }

    /// Takes the session's own accent once the session is known (a new one is given a colour no
    /// other session has), and the config's outside a session.
    /// While this window has focus, keeps `~/.rune_state_exports` on its session and colours.
    fn export(&mut self) {
        if !self.window_focused {
            self.exported = None;
            return;
        }
        let Some(session) = &self.session else { return };
        let now = crate::exports::Exports { session: session.name.clone(), animal: session.animal.clone(), accent: self.accent, light: self.light };
        if self.exported.as_ref() != Some(&now) {
            crate::exports::write(&now);
            self.exported = Some(now);
        }
    }

    fn follow_session_accent(&mut self) -> bool {
        let name = self.session.as_ref().map(|s| s.name.clone());
        if name == self.accent_for {
            return false;
        }
        self.accent = match &name {
            Some(n) => crate::accents::of(n, &self.accents, self.default_accent),
            None => self.default_accent,
        };
        self.accent_for = name;
        true
    }

    /// Starts a new session in this window; the current one stays in spooky, detached.
    pub fn new_session(&mut self) {
        self.picker = None;
        self.chooser = None;
        if self.controller.is_none() {
            return;
        }
        self.detach_all();
        self.new_tab(None);
    }

    /// The chooser's row: a new session, or one to open.
    fn take_chosen(&mut self) {
        let Some(at) = self.chooser else { return };
        let name = at.checked_sub(1).and_then(|i| self.picker.as_ref()?.get(i)).map(|i| i.name.clone());
        match name {
            Some(name) => self.open_session(&name),
            None => self.new_session(),
        }
    }

    /// Opens session `name` in this window; the current one stays in spooky, detached. If
    /// another window shows it, that window comes forward instead.
    pub fn open_session(&mut self, name: &str) {
        self.picker = None;
        if self.session.as_ref().is_some_and(|s| s.name == name) {
            return;
        }
        if let Some(c) = &self.controller {
            self.pending_open = Some(name.to_string());
            self.requested.push(name.to_string());
            c.open_session(name);
        }
    }

    /// Lets go of every pane without ending them, and becomes a new window to spooky.
    fn detach_all(&mut self) {
        self.panes.clear();
        self.tabs.clear();
        self.active = 0;
        self.session = None;
        self.window_id = spooky::fresh_id();
        self.last_report.clear();
    }

    /// Rebuilds this window from a session's layout, attaching each of its panes.
    fn take_over(&mut self, open: spooky::Open) {
        self.detach_all();
        self.chooser = None;
        self.session = Some(Session { name: open.session.clone(), animal: String::new() });
        fn build(s: &mut State, n: &spooky::Node) -> Option<Node> {
            if let Some(argv) = &n.argv {
                let id = match spooky::leaf_of(argv)? {
                    spooky::Leaf::Attach(pane) => s.attach(pane)?,
                    spooky::Leaf::New(_) => s.spawn(None)?,
                };
                return Some(Node::Leaf(id));
            }
            let a = n.left.as_deref().and_then(|l| build(s, l));
            let b = n.right.as_deref().and_then(|r| build(s, r));
            match (a, b) {
                (Some(a), Some(b)) => Some(Node::Split {
                    dir: if n.split.as_deref() == Some("v") { Dir::Down } else { Dir::Right },
                    ratio: n.ratio.clamp(0.05, 0.95),
                    a: Box::new(a),
                    b: Box::new(b),
                }),
                (one, None) | (None, one) => one,
            }
        }
        for t in &open.tabs {
            if let Some(root) = build(self, t) {
                let focused = root.first();
                let tab = self.tab_id();
                self.tabs.push(Tab { id: tab, root, focused, zoomed: false, minimized: vec![], floating: vec![] });
            }
        }
        self.active = (open.active as usize).min(self.tabs.len().saturating_sub(1));
    }

    /// Light or dark, saved to the config and applied to every terminal now.
    pub fn set_light(&mut self, light: bool) {
        if let Err(e) = crate::config::set("theme", if light { "light" } else { "dark" }) {
            eprintln!("runeterm: can't save the theme: {e}");
        }
        self.reload_config();
    }

    /// Turns the cursor glide on or off, and saves it as `smooth-cursor` so it sticks.
    pub fn set_smooth_cursor(&mut self, on: bool) {
        if let Err(e) = crate::config::set("smooth-cursor", if on { "true" } else { "false" }) {
            eprintln!("runeterm: can't save smooth-cursor: {e}");
        }
        self.reload_config();
    }

    pub fn set_accent(&mut self, c: crate::vt::Rgb) {
        if let Some(s) = &self.session {
            // In a session the swatches colour the session; the config's accent is for windows
            // outside one.
            crate::accents::set(&s.name, c);
            self.accent = c;
            return;
        }
        if let Err(e) = crate::config::set("accent", &crate::config::hex_of(c)) {
            eprintln!("runeterm: can't save the accent: {e}");
        }
        self.reload_config();
    }

    /// Right-click on the session badge: straight to this session's emoji.
    pub fn choose_own_emoji(&mut self) {
        let Some(name) = self.session.as_ref().map(|s| s.name.clone()) else { return };
        if self.picker.is_none() {
            self.picker = Some(self.load_sessions());
        }
        self.choose_emoji(&name);
    }

    /// ⌥/: pins the sidebar open, or back to a rail that opens on hover.
    pub fn toggle_sidebar(&mut self) {
        self.sidebar.pinned = !self.sidebar.pinned;
        self.sidebar_open = false;
    }

    /// ⌥E: the focused split's whole scrollback, in an editor, in a split zoomed over it.
    /// Quitting the editor closes that split and brings the terminal back.
    pub fn edit_scrollback(&mut self) {
        let Some(pane) = self.focused_pane() else { return };
        let text = pane.term.lock().unwrap().text();
        let cwd = Some(pane.cwd.replacen('~', &std::env::var("HOME").unwrap_or_default(), 1));
        let dir = std::env::var("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join("runeterm");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(format!("scrollback-{}-{}.txt", pane.id, chrono::Local::now().format("%H%M%S")));
        if let Err(e) = std::fs::write(&path, &text) {
            eprintln!("runeterm: can't write the scrollback: {e}");
            return;
        }
        let command = editor_command(&self.scrollback_editor, &path, text.lines().count());
        let id = self.next_id;
        self.next_id += 1;
        match Pane::spawn(id, Source::Local { cwd: cwd.as_deref(), command: Some(&command) }, &self.theme, Some(Arc::new(wake))) {
            Ok(mut p) => {
                p.cleanup = Some(path);
                p.program = command[0].rsplit('/').next().unwrap_or("vim").to_string();
                self.panes.insert(id, p);
                let tab = &mut self.tabs[self.active];
                tab.root.split(tab.focused, id, Dir::Right);
                tab.focused = id;
                tab.zoomed = true;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&path);
                eprintln!("runeterm: can't start {}: {e}", self.scrollback_editor);
            }
        }
    }

    /// Re-reads `~/.config/runeterm/config` (⌥R): new colours for every terminal now, and the
    /// renderer picks up the font, cursor and shaders on its next frame.
    pub fn reload_config(&mut self) {
        self.apply_config(Config::load());
    }

    /// Puts config `c` in effect: the window's settings, every terminal's colours and cursor,
    /// and the renderer's font (with the font size for now, ctrl+=, on top).
    fn apply_config(&mut self, mut c: Config) {
        c.font.size = (c.font.size + self.font_delta).max(4.0);
        self.sidebar = c.sidebar;
        self.settings = Behaviour::from(&c);
        self.scrollback_editor = c.scrollback_editor.clone();
        self.keybinds = c.keybinds.clone();
        self.theme = c.colors();
        self.light = c.light;
        self.default_accent = c.accent;
        self.accents = c.accents.clone();
        self.accent_for = None;
        self.follow_session_accent();
        for p in self.panes.values() {
            let mut t = p.term.lock().unwrap();
            t.set_colors(self.theme.fg, self.theme.bg, self.theme.cursor, &self.theme.ansi);
            t.set_default_cursor(self.theme.cursor_style, true);
            p.dirty.store(true, Ordering::Release);
        }
        crate::config::publish(c);
    }

    /// Opens the session picker with a fresh list from the daemon.
    pub fn toggle_picker(&mut self) {
        self.picker_mode = PickerMode::List;
        self.picker = match self.picker {
            Some(_) => None,
            None if self.controller.is_some() => Some(self.load_sessions()),
            None => None,
        };
    }

    /// The daemon's sessions for the picker, each with its accent (a session that has none
    /// yet gets one, as it would when a window opens it).
    fn load_sessions(&mut self) -> Vec<SessionInfo> {
        let list = spooky::sessions();
        self.picker_accents = list
            .iter()
            .map(|i| (i.name.clone(), crate::accents::of(&i.name, &self.accents, self.default_accent)))
            .collect();
        if let Some(s) = &self.session {
            // This window's may have just changed.
            self.picker_accents.insert(s.name.clone(), self.accent);
        }
        list
    }

    fn refresh_picker(&mut self) {
        self.picker_mode = PickerMode::List;
        if self.picker.is_some() {
            self.picker = Some(self.load_sessions());
        }
    }

    /// Shows the emoji grid for `session`.
    pub fn choose_emoji(&mut self, session: &str) {
        let animals = spooky::animals();
        self.picker_mode = PickerMode::Emoji { session: session.to_string(), animals };
    }

    pub fn set_emoji(&mut self, session: &str, animal: &str) {
        if let Err(e) = spooky::set_animal(session, animal) {
            eprintln!("runeterm: can't set {session}'s emoji: {e}");
        }
        self.refresh_picker();
    }

    /// Starts renaming `session` in the picker (opening it if it's closed), its name selected.
    pub fn start_rename(&mut self, session: &str, tx: &mut Tx) {
        if self.controller.is_none() {
            return;
        }
        if self.picker.is_none() {
            self.picker = Some(self.load_sessions());
        }
        let mut edit = rune::TextEdit::new(session);
        edit.anchor = 0;
        edit.focus = edit.text.len();
        self.rename_edit = edit;
        self.picker_mode = PickerMode::Rename { session: session.to_string(), error: None };
        tx.focus("rename-field", false);
    }

    /// Renames the session being renamed to what was typed, or says why it can't.
    pub fn finish_rename(&mut self) {
        let PickerMode::Rename { session, .. } = &self.picker_mode else { return };
        let old = session.clone();
        let new = self.rename_edit.text.trim().to_string();
        if new == old {
            self.cancel_rename();
            return;
        }
        let refuse = |s: &mut State, why: String| s.picker_mode = PickerMode::Rename { session: old.clone(), error: Some(why) };
        if let Some(why) = session_name_problem(&new) {
            return refuse(self, why.into());
        }
        if let Err(e) = spooky::rename(&old, &new) {
            return refuse(self, e.to_string());
        }
        crate::accents::rename(&old, &new);
        if let Some(sess) = self.session.as_mut().filter(|s| s.name == old) {
            // spooky reports it too; show it now.
            sess.name = new.clone();
            self.accent_for = Some(new);
        }
        self.refresh_picker();
    }

    pub fn cancel_rename(&mut self) {
        if matches!(self.picker_mode, PickerMode::Rename { .. }) {
            self.picker_mode = PickerMode::List;
        }
    }

    pub fn ask_kill(&mut self, session: &str) {
        self.picker_mode = PickerMode::ConfirmKill(session.to_string());
    }

    pub fn cancel_kill(&mut self) {
        self.picker_mode = PickerMode::List;
    }

    /// Does `action`, from its key or the command palette.
    pub fn run(&mut self, action: Action, tx: &mut Tx) {
        let focused = self.tab().map(|t| t.focused);
        match action {
            Action::CommandPalette => self.toggle_palette(tx),
            Action::SplitRight => self.split(Dir::Right),
            Action::SplitDown => self.split(Dir::Down),
            Action::FocusLeft => self.focus_dir(Side::Left),
            Action::FocusDown => self.focus_dir(Side::Down),
            Action::FocusUp => self.focus_dir(Side::Up),
            Action::FocusRight => self.focus_dir(Side::Right),
            Action::SwapLeft => self.swap_dir(Side::Left),
            Action::SwapDown => self.swap_dir(Side::Down),
            Action::SwapUp => self.swap_dir(Side::Up),
            Action::SwapRight => self.swap_dir(Side::Right),
            Action::ResizeUp | Action::ResizeLeft | Action::ResizeDown | Action::ResizeRight => {
                let side = match action {
                    Action::ResizeUp => Side::Up,
                    Action::ResizeLeft => Side::Left,
                    Action::ResizeDown => Side::Down,
                    _ => Side::Right,
                };
                if let Some(tab) = self.tabs.get_mut(self.active) {
                    tab.root.resize(tab.focused, side, 0.04);
                }
            }
            Action::Zoom => focused.into_iter().for_each(|id| self.toggle_zoom(id)),
            Action::Close => focused.into_iter().for_each(|id| self.request_close(id)),
            Action::Minimize => focused.into_iter().for_each(|id| self.minimize(id)),
            Action::RestoreMinimized => self.restore_last(),
            Action::Float => focused.into_iter().for_each(|id| self.toggle_float(id)),
            Action::PinFloat => focused.into_iter().for_each(|id| self.toggle_pin(id)),
            Action::NewTab => self.new_tab_here(),
            Action::NextTab => self.cycle_tab(1),
            Action::PrevTab => self.cycle_tab(-1),
            Action::GotoTab(n) => self.select_tab(n as usize - 1),
            Action::Sessions => self.toggle_picker(),
            Action::NewSession => self.new_session(),
            Action::RenameSession => {
                if let Some(name) = self.session.as_ref().map(|s| s.name.clone()) {
                    self.start_rename(&name, tx);
                }
            }
            Action::ToggleSidebar => self.toggle_sidebar(),
            Action::ToggleTheme => self.set_light(!self.light),
            Action::ToggleSmoothCursor => self.set_smooth_cursor(!Config::load().cursor_smooth),
            Action::Expose => self.open_expose(tx),
            Action::Pick(setting) => self.open_pick(setting, tx),
            Action::Set(setting, v) => self.set_setting(setting, v),
            Action::FontBigger => self.nudge_font(1.0),
            Action::FontSmaller => self.nudge_font(-1.0),
            Action::FontReset => self.nudge_font(0.0),
            Action::ToggleCopyOnSelect => self.toggle_setting("copy-on-select", self.settings.copy_on_select),
            Action::ToggleConfirmClose => self.toggle_setting("confirm-close", self.settings.confirm_close),
            Action::ToggleNotifications => self.toggle_setting("notifications", self.settings.notifications),
            Action::EditConfig => self.edit_config(),
            Action::MoveToTab(n) => focused.into_iter().for_each(|id| self.move_to_tab(id, Some(n as usize - 1))),
            Action::MoveToNewTab => focused.into_iter().for_each(|id| self.move_to_tab(id, None)),
            Action::EditScrollback => self.edit_scrollback(),
            Action::ReloadConfig => self.reload_config(),
            Action::Copy => {
                if let Some(text) = self.focused_pane().and_then(|p| p.term.lock().unwrap().selection_text()) {
                    tx.copy(text);
                }
            }
            Action::Paste => tx.read_clipboard(|s: &mut State, content, _| s.paste(content.text())),
        }
    }

    /// Opens the command palette with its field focused, or closes it.
    pub fn toggle_palette(&mut self, tx: &mut Tx) {
        if self.palette.is_some() {
            self.close_palette();
        } else {
            self.palette = Some(Palette { edit: rune::TextEdit::new(""), chosen: 0, pick: None });
            tx.focus("palette-field", false);
        }
    }

    /// Closes the palette, undoing a value it was showing but didn't save.
    pub fn close_palette(&mut self) {
        if self.palette.take().is_some_and(|p| p.pick.is_some_and(|w| w.previewed)) {
            self.reload_config();
        }
    }

    /// Opens the palette on `setting`'s values, on the one in use now. Line height is listed
    /// in the config's unit (pixels or percent).
    pub fn open_pick(&mut self, setting: Setting, tx: &mut Tx) {
        let c = Config::load();
        let f = &c.font;
        let style = (setting == Setting::BoldWeight) as usize;
        let axis = |tag: &str| crate::grid::font_axis(f, style, tag);
        let own = |tag: &str| f.variations[style].iter().find(|(t, _)| t == tag).map(|(_, v)| *v);
        let (setting, range, current) = match setting {
            Setting::Weight | Setting::BoldWeight => {
                let range = axis("wght");
                (setting, range, own("wght").or(range.map(|r| r.1)).unwrap_or(400.0))
            }
            Setting::Width => {
                let range = axis("wdth");
                (setting, range, crate::grid::shared_width(&f.variations).or(range.map(|r| r.1)).unwrap_or(100.0))
            }
            // Picking a size starts from the config's, not one for now (ctrl+=).
            Setting::FontSize => (setting, Some((4.0, 13.0, 96.0)), f.size),
            Setting::LinePercent | Setting::LinePixels if f.cell_height_px != 0.0 => (Setting::LinePixels, Some((0.0, 0.0, 100.0)), f.cell_height_px),
            Setting::LinePercent | Setting::LinePixels => (Setting::LinePercent, Some((0.0, 0.0, 200.0)), f.cell_height * 100.0),
            Setting::Fit => (setting, Some((0.0, 0.0, 15.0)), f.fit * 100.0),
            Setting::Padding => (setting, Some((0.0, 10.0, 100.0)), f.padding),
            Setting::CursorStyle => (setting, Some((0.0, 0.0, 3.0)), cursor_index(c.cursor_style) as f32),
        };
        if setting == Setting::FontSize && self.font_delta != 0.0 {
            self.font_delta = 0.0;
            self.reload_config();
        }
        let family = if style == 1 { f.families[0].clone().unwrap_or_else(|| f.family.clone()) } else { f.family.clone() };
        let pick = Pick { setting, family, range, current, previewed: false };
        self.palette = Some(Palette { edit: rune::TextEdit::new(""), chosen: 0, pick: Some(pick) });
        let items = self.palette_items();
        let at = items.iter().position(|r| matches!(r.0, Action::Set(_, v) if v == tenths(current)));
        if let Some(p) = self.palette.as_mut() {
            p.chosen = at.unwrap_or(0);
        }
        tx.focus("palette-field", false);
        // The one in use may be below the fold (20 pt in the size list).
        if let Some((a, ..)) = at.and_then(|i| items.get(i)) {
            tx.scroll_into_view_in("palette-list", format!("palette-{}", a.id()));
        }
    }

    /// Saves `setting` at `v` tenths to the config and applies it.
    pub fn set_setting(&mut self, setting: Setting, v: u32) {
        if let Err(e) = save_setting(setting, v as f32 / 10.0) {
            eprintln!("runeterm: can't save {}: {e}", setting.name());
        }
        if setting == Setting::FontSize {
            self.font_delta = 0.0;
        }
        self.reload_config();
    }

    /// Shows the chosen row's value in every terminal, without saving it.
    fn preview_pick(&mut self) {
        let Some(Action::Set(setting, v)) = self.palette.as_ref().and_then(|p| self.palette_items().get(p.chosen).map(|r| r.0)) else { return };
        let mut c = Config::load();
        apply_setting(&mut c, setting, v as f32 / 10.0);
        self.apply_config(c);
        if let Some(pick) = self.palette.as_mut().and_then(|p| p.pick.as_mut()) {
            pick.previewed = true;
        }
    }

    /// The font size for now (`by` points bigger, or back to the config's at 0), as Ghostty's
    /// ctrl+= and ctrl+- do: not saved, and kept across config reloads.
    pub fn nudge_font(&mut self, by: f32) {
        let size = Config::load().font.size;
        self.font_delta = if by == 0.0 { 0.0 } else { (self.font_delta + by).clamp(4.0 - size, 96.0 - size) };
        self.reload_config();
    }

    /// Flips an on/off setting in the config, and applies it.
    fn toggle_setting(&mut self, key: &str, on: bool) {
        if let Err(e) = crate::config::set(key, if on { "false" } else { "true" }) {
            eprintln!("runeterm: can't save {key}: {e}");
        }
        self.reload_config();
    }

    /// Opens the config in the scrollback editor, in a split beside this one; closing the
    /// editor reloads it.
    pub fn edit_config(&mut self) {
        let path = crate::config::path();
        let cwd = path.parent().map(|d| d.display().to_string());
        let command = editor_command(&self.scrollback_editor, &path, 1);
        let id = self.next_id;
        self.next_id += 1;
        match Pane::spawn(id, Source::Local { cwd: cwd.as_deref(), command: Some(&command) }, &self.theme, Some(Arc::new(wake))) {
            Ok(mut p) => {
                p.program = command[0].rsplit('/').next().unwrap_or("vim").to_string();
                self.panes.insert(id, p);
                let tab = &mut self.tabs[self.active];
                tab.root.split(tab.focused, id, Dir::Right);
                tab.focused = id;
                tab.zoomed = false;
                self.config_editor = Some(id);
            }
            Err(e) => eprintln!("runeterm: can't start {}: {e}", self.scrollback_editor),
        }
    }

    /// Opens the exposé on the focused split, or (open already, as ⌥Tab is pressed again while
    /// ⌥ is held in an app switcher) chooses the next split. It swaps in at once: the splits
    /// are drawn small there, and a frame of both sizes would remake every glyph twice.
    pub fn open_expose(&mut self, tx: &mut Tx) {
        match self.expose {
            Some(_) => self.expose_step(1),
            None => {
                self.palette = None;
                self.expose = self.focused_id().or_else(|| self.expose_order().first().copied());
                tx.instant();
            }
        }
    }

    /// Every split the exposé shows, tab by tab: each tab's layout, then its floating and
    /// minimized ones.
    pub fn expose_order(&self) -> Vec<u32> {
        self.tabs.iter().flat_map(|t| t.panes()).collect()
    }

    /// Chooses the split `by` along in the exposé, wrapping around.
    pub fn expose_step(&mut self, by: isize) {
        let order = self.expose_order();
        if order.is_empty() {
            return;
        }
        let at = self.expose.and_then(|id| order.iter().position(|&o| o == id)).unwrap_or(0) as isize;
        self.expose = Some(order[(at + by).rem_euclid(order.len() as isize) as usize]);
    }

    /// Where each split sits in the exposé, in tab widths: the tabs in the view's grid (see
    /// `expose_cols`), each split where it is in its tab, floating ones where they float, and
    /// minimized ones in a row along the tab's label.
    pub fn expose_rects(&self) -> Vec<(u32, [f32; 4])> {
        let [w, h] = self.stage;
        let aspect = if w > 0.0 && h > 0.0 { h / w } else { 0.6 };
        let (gap, label) = (0.1, 0.08);
        let cols = expose_cols(self.tabs.len());
        let mut out = vec![];
        for (i, tab) in self.tabs.iter().enumerate() {
            let (c, r) = ((i % cols) as f32, (i / cols) as f32);
            let (x0, y0) = (c * (1.0 + gap), r * (aspect + gap + label) + label);
            let at = |q: [f32; 4]| [x0 + q[0], y0 + q[1] * aspect, q[2], q[3] * aspect];
            out.extend(tab.root.layout().into_iter().map(|(id, q)| (id, at(q))));
            out.extend(tab.floating.iter().map(|f| (f.id, at(f.rect))));
            for (j, m) in tab.minimized.iter().enumerate() {
                out.push((m.id, [x0 + 0.3 + 0.1 * j as f32, y0 - label, 0.08, label * 0.8]));
            }
        }
        out
    }

    /// Chooses the split next to the chosen one on `side` in the exposé, across tabs too: of
    /// the ones past its edge that way, those level with it (overlapping across the move)
    /// first, then the nearest.
    pub fn expose_dir(&mut self, side: Side) {
        let Some(id) = self.expose else { return };
        let rects = self.expose_rects();
        let Some(&(_, me)) = rects.iter().find(|(i, _)| *i == id) else { return };
        let centre = |r: [f32; 4]| (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        let (mx, my) = centre(me);
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| a1.min(b1) - a0.max(b0) > 1e-4;
        let best = rects.iter()
            .filter(|(i, _)| *i != id)
            .filter_map(|&(i, r)| {
                let (x, y) = centre(r);
                let (along, across, level) = match side {
                    Side::Right => (x - mx, (y - my).abs(), overlap(me[1], me[1] + me[3], r[1], r[1] + r[3])),
                    Side::Left => (mx - x, (y - my).abs(), overlap(me[1], me[1] + me[3], r[1], r[1] + r[3])),
                    Side::Down => (y - my, (x - mx).abs(), overlap(me[0], me[0] + me[2], r[0], r[0] + r[2])),
                    Side::Up => (my - y, (x - mx).abs(), overlap(me[0], me[0] + me[2], r[0], r[0] + r[2])),
                };
                // Past my edge on that side; or, for splits drawn over one another (a floating
                // one over the layout), its centre past mine.
                let eps = 1e-3;
                let past = match side {
                    Side::Right => r[0] >= me[0] + me[2] - eps,
                    Side::Left => r[0] + r[2] <= me[0] + eps,
                    Side::Down => r[1] >= me[1] + me[3] - eps,
                    Side::Up => r[1] + r[3] <= me[1] + eps,
                };
                let over = overlap(me[0], me[0] + me[2], r[0], r[0] + r[2]) && overlap(me[1], me[1] + me[3], r[1], r[1] + r[3]);
                ((past || over) && along > 1e-4).then_some((i, !level, along + 2.0 * across))
            })
            .min_by(|a, b| a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)));
        if let Some((i, ..)) = best {
            self.expose = Some(i);
        }
    }

    /// Closes the exposé, going to split `id` (its tab, focused) when given.
    pub fn close_expose(&mut self, id: Option<u32>, tx: &mut Tx) {
        self.expose = None;
        if let Some(id) = id {
            self.focus(id);
        }
        tx.instant();
    }

    /// A key while the exposé is open: arrows (or hjkl) move to the split that way, Tab steps
    /// through them in order, Enter goes, Escape closes. It takes every key.
    fn expose_key(&mut self, e: &KeyEvent, tx: &mut Tx) -> bool {
        // Its key again steps on, as in an app switcher; ⌥⇧Tab steps back.
        if self.keybinds.is(Action::Expose, e, tx) || (e.key == Key::Tab && e.mods.alt) {
            self.expose_step(if e.mods.shift { -1 } else { 1 });
            return true;
        }
        let ch = match &e.key {
            Key::Text(t) => t.chars().next(),
            _ => None,
        };
        match (&e.key, ch) {
            (Key::Escape, _) => self.close_expose(None, tx),
            (Key::Enter, _) | (_, Some(' ')) => {
                let id = self.expose;
                self.close_expose(id, tx);
            }
            (Key::Tab, _) if e.mods.shift => self.expose_step(-1),
            (Key::Tab, _) => self.expose_step(1),
            (Key::Right, _) | (_, Some('l')) => self.expose_dir(Side::Right),
            (Key::Left, _) | (_, Some('h')) => self.expose_dir(Side::Left),
            (Key::Down, _) | (_, Some('j')) => self.expose_dir(Side::Down),
            (Key::Up, _) | (_, Some('k')) => self.expose_dir(Side::Up),
            _ => {}
        }
        true
    }

    /// Moves split `id` to tab `to` (0-based; past the last tab, or None, a new one), beside
    /// that tab's focused split, and follows it there. A floating split floats there too. The
    /// tab it leaves closes if it was its last split.
    pub fn move_to_tab(&mut self, id: u32, to: Option<usize>) {
        let Some(from) = self.tabs.iter().position(|t| t.has(id)) else { return };
        let to = to.filter(|&t| t < self.tabs.len());
        let alone = self.tabs[from].panes().len() == 1;
        if to == Some(from) || (alone && to.is_none()) {
            return;
        }
        // Out of its tab.
        let tab = &mut self.tabs[from];
        let floating = tab.floating.iter().position(|f| f.id == id).map(|i| tab.floating.remove(i));
        tab.minimized.retain(|m| m.id != id);
        let mut closed = false;
        if tab.root.contains(id) {
            match tab.root.remove(id) {
                Some(rest) => tab.root = rest,
                None => {
                    if let Some(m) = tab.minimized.pop() {
                        tab.root = Node::Leaf(m.id);
                    } else if let Some(f) = tab.floating.pop() {
                        tab.root = Node::Leaf(f.id);
                    } else {
                        closed = true;
                    }
                }
            }
        }
        if !closed {
            if tab.focused == id {
                tab.focused = tab.root.first();
            }
            tab.zoomed = false;
        }
        // Into the other.
        let dest = match to {
            Some(t) => {
                let d = &mut self.tabs[t];
                match floating {
                    Some(f) => d.floating.push(Floating { place: None, ..f }),
                    None => {
                        let at = if d.root.contains(d.focused) { d.focused } else { d.root.first() };
                        d.root.insert(at, id, Dir::Right, 0.5, false);
                    }
                }
                d.focused = id;
                d.zoomed = false;
                t
            }
            None => {
                // In a tab of its own, a floating split is the tab's layout.
                let tab = self.tab_id();
                self.tabs.push(Tab { id: tab, root: Node::Leaf(id), focused: id, zoomed: false, minimized: vec![], floating: vec![] });
                self.tabs.len() - 1
            }
        };
        if closed {
            self.tabs.remove(from);
            let dest = if dest > from { dest - 1 } else { dest };
            self.slide = if dest >= from { 1.0 } else { -1.0 };
            self.active = dest;
        } else {
            self.select_tab(dest);
        }
    }

    /// What the palette says when it has no rows.
    pub fn palette_empty(&self) -> String {
        match self.palette.as_ref().and_then(|p| p.pick.as_ref()) {
            Some(p) if p.range.is_none() && p.setting == Setting::Width => format!("{} has no width axis", p.family),
            Some(p) if p.range.is_none() => format!("{} has no weight axis (pick a face with font-style instead)", p.family),
            Some(_) => "Not a value it takes".into(),
            None => "No matching command".into(),
        }
    }

    /// A setting's rows: its usual values in range, its current one and a number typed,
    /// smallest first. The chip says which is in use, and which is the default. A line height
    /// typed with % or px is listed in that unit.
    fn pick_items(p: &Pick, query: &str) -> Vec<(Action, String, String)> {
        let Some((lo, _, hi)) = p.range else { return vec![] };
        let query = query.trim();
        let number = query.trim_end_matches(['%', 'p', 't', 'x']).trim();
        let typed = (p.setting != Setting::CursorStyle).then(|| number.parse::<f32>().ok()).flatten();
        let mut rows: Vec<(Setting, f32)> = p.values().into_iter().map(|v| (p.setting, v)).collect();
        rows.push((p.setting, p.current.clamp(lo, hi)));
        if let Some(t) = typed {
            let unit = match p.setting {
                Setting::LinePixels | Setting::LinePercent if query.ends_with('%') => Setting::LinePercent,
                Setting::LinePixels | Setting::LinePercent if query.ends_with("px") => Setting::LinePixels,
                s => s,
            };
            if unit != p.setting || (lo..=hi).contains(&t) {
                rows.push((unit, t.max(0.0)));
            }
        }
        rows.sort_by(|a, b| (a.0 != p.setting).cmp(&(b.0 != p.setting)).then(a.1.total_cmp(&b.1)));
        rows.dedup_by(|a, b| a.0 == b.0 && tenths(a.1) == tenths(b.1));
        let mark = |s: Setting, v: f32| {
            if s == p.setting && tenths(v) == tenths(p.current) {
                "in use"
            } else if s == p.setting && p.default().is_some_and(|d| tenths(d) == tenths(v)) {
                if p.setting.axis().is_some() { "font default" } else { "default" }
            } else {
                ""
            }
        };
        rows.into_iter()
            .filter(|(s, v)| match typed {
                Some(_) => v.to_string().starts_with(number) || *s != p.setting,
                None => crate::keys::fuzzy(query, &value_title(*s, *v)).is_some(),
            })
            .map(|(s, v)| (Action::Set(s, tenths(v)), value_title(s, v), mark(s, v).to_string()))
            .collect()
    }

    /// The palette's rows for what's typed, best first: (action, title, keys). Tabs are listed
    /// only while they exist, with their directory.
    pub fn palette_items(&self) -> Vec<(Action, String, String)> {
        let query = self.palette.as_ref().map_or(String::new(), |p| p.edit.text.clone());
        if let Some(pick) = self.palette.as_ref().and_then(|p| p.pick.as_ref()) {
            return State::pick_items(pick, &query);
        }
        let mut rows: Vec<(i32, usize, Action, String)> = Action::all()
            .enumerate()
            .filter(|(_, a)| *a != Action::CommandPalette)
            .filter_map(|(i, a)| {
                let title = match a {
                    Action::GotoTab(n) => {
                        let tab = self.tabs.get(n as usize - 1)?;
                        let cwd = self.panes.get(&tab.focused).map(|p| p.cwd.clone()).unwrap_or_default();
                        format!("{} · {cwd}", a.title())
                    }
                    // To another tab that's there, with its directory; a new tab unless the
                    // split is its tab's only one.
                    Action::MoveToTab(n) => {
                        let i = n as usize - 1;
                        let tab = self.tabs.get(i).filter(|_| i != self.active)?;
                        let cwd = self.panes.get(&tab.focused).map(|p| p.cwd.clone()).unwrap_or_default();
                        format!("{} · {cwd}", a.title())
                    }
                    Action::MoveToNewTab if self.tab().is_none_or(|t| t.panes().len() < 2) => return None,
                    Action::FontReset if self.font_delta == 0.0 => return None,
                    Action::FontReset => format!("{} ({:+} pt now)", a.title(), self.font_delta),
                    Action::ToggleCopyOnSelect | Action::ToggleConfirmClose | Action::ToggleNotifications => {
                        let on = match a {
                            Action::ToggleCopyOnSelect => self.settings.copy_on_select,
                            Action::ToggleConfirmClose => self.settings.confirm_close,
                            _ => self.settings.notifications,
                        };
                        format!("{} · now {}", a.title(), if on { "on" } else { "off" })
                    }
                    Action::Float if self.tab().is_some_and(|t| t.is_floating(t.focused)) => "Dock floating split".into(),
                    Action::PinFloat if self.focused_id().is_some_and(|id| self.is_pinned(id)) => "Unpin floating split".into(),
                    _ => a.title().to_string(),
                };
                let score = crate::keys::fuzzy(&query, &title).or_else(|| crate::keys::fuzzy(&query, a.name()).map(|s| s - 50))?;
                Some((score, i, a, title))
            })
            .collect();
        rows.sort_by(|x, y| y.0.cmp(&x.0).then(x.1.cmp(&y.1)));
        rows.into_iter().map(|(_, _, a, title)| (a, title, self.keybinds.label(a))).collect()
    }

    /// A key for the palette: arrows choose, Enter runs, Escape (or its own key) closes. Others
    /// are left to its field, and the choice goes back to the best match.
    pub fn palette_key(&mut self, e: &KeyEvent, tx: &mut Tx) -> bool {
        if e.key == Key::Escape || self.keybinds.is(Action::CommandPalette, e, tx) {
            self.close_palette();
            return true;
        }
        let items = self.palette_items();
        let n = items.len().max(1);
        let Some(p) = self.palette.as_mut() else { return false };
        match e.key {
            Key::Up => p.chosen = (p.chosen.min(n - 1) + n - 1) % n,
            Key::Down | Key::Tab => p.chosen = (p.chosen + 1) % n,
            Key::Enter => {
                self.run_chosen(tx);
                return true;
            }
            _ => {
                p.chosen = 0;
                return false;
            }
        }
        let picking = p.pick.is_some();
        if let Some((a, ..)) = items.get(p.chosen) {
            tx.scroll_into_view_in("palette-list", format!("palette-{}", a.id()));
        }
        if picking {
            self.preview_pick();
        }
        true
    }

    /// Runs the palette's chosen row (or row `at`), closing it first.
    pub fn run_chosen_at(&mut self, at: usize, tx: &mut Tx) {
        let action = self.palette_items().get(at).map(|r| r.0);
        match action {
            Some(a) => {
                self.palette = None;
                self.run(a, tx);
            }
            None => self.close_palette(),
        }
    }

    fn run_chosen(&mut self, tx: &mut Tx) {
        let at = self.palette.as_ref().map_or(0, |p| p.chosen);
        self.run_chosen_at(at, tx);
    }

    /// Ends every pane of `session` and forgets it. If it's this window's, the window starts a
    /// new session instead of closing.
    pub fn kill_session(&mut self, session: &str) {
        let here = self.session.as_ref().is_some_and(|s| s.name == session);
        if here {
            self.detach_all();
        }
        if let Err(e) = spooky::remove(session) {
            eprintln!("runeterm: can't kill session {session}: {e}");
        } else {
            crate::accents::remove(session);
        }
        if here {
            self.new_tab(None);
        }
        self.refresh_picker();
    }


    /// The session chooser, a rename or a close waiting for an answer first, then runeterm's
    /// keybinds, then everything else to the focused pane.
    pub fn key(&mut self, e: &KeyEvent, tx: &mut Tx) -> bool {
        let m = e.mods;
        // A new window asking which session to be: arrows choose, Enter takes it, Escape starts
        // a new one.
        if let Some(at) = self.chooser.filter(|_| self.picker_mode == PickerMode::List) {
            let n = self.picker.as_ref().map_or(0, |l| l.len()) + 1;
            match e.key {
                Key::Up => self.chooser = Some((at + n - 1) % n),
                Key::Down | Key::Tab => self.chooser = Some((at + 1) % n),
                Key::Enter => self.take_chosen(),
                Key::Escape => self.new_session(),
                _ => {}
            }
            return true;
        }
        // Renaming a session: its field takes the typing; Escape gives up.
        if self.picker.is_some() && matches!(self.picker_mode, PickerMode::Rename { .. }) {
            if e.key == Key::Escape {
                self.cancel_rename();
            }
            return true;
        }
        // A close waiting for a yes takes Enter and Escape.
        if let Some(id) = self.confirm_close {
            match e.key {
                Key::Enter => {
                    self.confirm_close = None;
                    self.close(id);
                    return true;
                }
                Key::Escape => {
                    self.confirm_close = None;
                    return true;
                }
                _ => {}
            }
        }
        if self.expose.is_some() {
            return self.expose_key(e, tx);
        }
        // The palette's field has focus and takes its own keys; these are the ones it passed
        // on, or all of them once it lost focus.
        if self.palette.is_some() {
            return self.palette_key(e, tx);
        }
        // runeterm's own keys (`keybind` in the config), before the terminal sees them.
        if let Some(action) = self.keybinds.action(e, tx) {
            self.run(action, tx);
            return true;
        }
        let Some(pane) = self.focused_pane() else { return false };
        let term = pane.term.clone();
        pane.attention.store(false, Ordering::Release);
        // Typing ends a selection, as in other terminals.
        if !matches!(e.key, Key::Command(_)) || !m.shift {
            let mut t = term.lock().unwrap();
            if t.selection().is_some() {
                t.set_selection(None);
                pane.dirty.store(true, Ordering::Release);
            }
        }
        let bytes = {
            let t = term.lock().unwrap();
            let mods = vt::Mods { shift: m.shift, ctrl: m.ctrl, alt: m.alt, logo: m.logo };
            let none = String::new;
            let (key, text, unshifted): (sys::GhosttyKey, String, u32) = match &e.key {
                Key::Text(s) => {
                    let c = s.chars().next().unwrap_or('\0');
                    let (k, base) = ascii_key(c);
                    (k, if m.ctrl { none() } else { s.clone() }, base)
                }
                Key::Command(c) => {
                    let (k, base) = ascii_key(*c);
                    (k, none(), base)
                }
                Key::Enter => (sys::GHOSTTY_KEY_ENTER, none(), 0),
                Key::Backspace => (sys::GHOSTTY_KEY_BACKSPACE, none(), 0),
                Key::Delete => (sys::GHOSTTY_KEY_DELETE, none(), 0),
                Key::Left => (sys::GHOSTTY_KEY_ARROW_LEFT, none(), 0),
                Key::Right => (sys::GHOSTTY_KEY_ARROW_RIGHT, none(), 0),
                Key::Up => (sys::GHOSTTY_KEY_ARROW_UP, none(), 0),
                Key::Down => (sys::GHOSTTY_KEY_ARROW_DOWN, none(), 0),
                Key::Home => (sys::GHOSTTY_KEY_HOME, none(), 0),
                Key::End => (sys::GHOSTTY_KEY_END, none(), 0),
                Key::PageUp => (sys::GHOSTTY_KEY_PAGE_UP, none(), 0),
                Key::PageDown => (sys::GHOSTTY_KEY_PAGE_DOWN, none(), 0),
                Key::Insert => (sys::GHOSTTY_KEY_INSERT, none(), 0),
                Key::Escape => (sys::GHOSTTY_KEY_ESCAPE, none(), 0),
                Key::Tab => (sys::GHOSTTY_KEY_TAB, none(), 0),
                Key::Function(n @ 1..=25) => (sys::GHOSTTY_KEY_F1 + (*n as sys::GhosttyKey - 1), none(), 0),
                _ => return false,
            };
            self.keys.encode(&t, key, mods, &text, unshifted)
        };
        term.lock().unwrap().scroll_to_bottom();
        self.stop_scrolling();
        self.focused_pane().unwrap().send(&bytes);
        true
    }

    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        self.stop_scrolling();
        if let Some(p) = self.focused_pane() {
            let bytes = {
                let mut t = p.term.lock().unwrap();
                t.scroll_to_bottom();
                t.paste_bytes(text)
            };
            p.send(&bytes);
        }
    }

    /// Where pointer `(x, y)` (the pane's grid element's points) falls in pane `id`'s terminal.
    fn pointer_at(&self, id: u32, x: f32, y: f32) -> Option<vt::PointerAt> {
        let g = crate::grid::geometry(id)?;
        let (px, py) = (x * g.scale, y * g.scale);
        let col = (((px - g.pad) / g.cell_w).floor().max(0.0) as u16).min(g.cols.saturating_sub(1));
        // While scrolling, the grid is drawn part of a line lower.
        let py = py - self.scroll_frac(id) * g.cell_h;
        let row = (((py - g.pad) / g.cell_h).floor().max(0.0) as u16).min(g.rows.saturating_sub(1));
        Some(vt::PointerAt {
            col,
            row,
            px: px as f64,
            py: py as f64,
            cols: g.cols,
            rows: g.rows,
            cell_w: g.cell_w as u32,
            cell_h: g.cell_h as u32,
            pad: g.pad as u32,
            width: g.width as u32,
            height: g.height as u32,
        })
    }

    /// A press, drag or release on a pane's grid. Programs that track the mouse get it (Shift
    /// takes it back for selecting); otherwise the left button selects (twice for a word,
    /// three times for a line), Ctrl+click opens a link, and the middle button pastes the
    /// last selection.
    pub fn mouse(&mut self, id: u32, phase: rune::Phase, x: f32, y: f32, button: rune::PointerButton, mods: rune::Mods, tx: &mut Tx) {
        let Some(at) = self.pointer_at(id, x, y) else { return };
        if phase == rune::Phase::Down {
            self.focus(id);
        }
        let Some(pane) = self.panes.get(&id) else { return };
        let term = pane.term.clone();
        let tracking = term.lock().unwrap().mouse_tracking();
        let code = match button {
            rune::PointerButton::Primary => 1,
            rune::PointerButton::Secondary => 2,
            rune::PointerButton::Middle => 3,
            _ => return,
        };
        let vmods = vt::Mods { shift: mods.shift, ctrl: mods.ctrl, alt: mods.alt, logo: mods.logo };
        if tracking && !mods.shift {
            let action = match phase {
                rune::Phase::Down => vt::MouseAction::Press,
                rune::Phase::Move => vt::MouseAction::Motion,
                rune::Phase::Up => vt::MouseAction::Release,
            };
            let bytes = self.mouse.encode(&term.lock().unwrap(), action, code, vmods, at);
            pane.send(&bytes);
            return;
        }
        match (code, phase) {
            (3, rune::Phase::Down) => {
                let text = self.primary.clone();
                self.paste(&text);
            }
            (1, rune::Phase::Down) if mods.ctrl => {
                let link = {
                    let t = term.lock().unwrap();
                    t.hyperlink_at(at.col, at.row).or_else(|| url_at(&t.row_text(at.row), at.col as usize))
                };
                if let Some(url) = link {
                    open_link(&url);
                }
            }
            (1, _) if !mods.ctrl => {
                let kind = match phase {
                    rune::Phase::Down => vt::GestureKind::Press,
                    rune::Phase::Move => vt::GestureKind::Drag,
                    rune::Phase::Up => vt::GestureKind::Release,
                };
                let mut t = term.lock().unwrap();
                let g = self.gestures.entry(id).or_default();
                if phase == rune::Phase::Down {
                    t.set_selection(None);
                }
                if let Some(sel) = g.event(&t, kind, at) {
                    t.set_selection(Some(&sel));
                }
                if phase == rune::Phase::Up {
                    if let Some(text) = t.selection_text() {
                        if self.settings.copy_on_select {
                            tx.copy(text.clone());
                        }
                        self.primary = text;
                    }
                }
                drop(t);
                pane.dirty.store(true, Ordering::Release);
            }
            _ => {}
        }
    }

    /// Pointer motion with no button held, for programs that track all motion.
    pub fn mouse_moved(&mut self, id: u32, x: f32, y: f32, mods: rune::Mods) {
        let Some(at) = self.pointer_at(id, x, y) else { return };
        let Some(pane) = self.panes.get(&id) else { return };
        let vmods = vt::Mods { shift: mods.shift, ctrl: mods.ctrl, alt: mods.alt, logo: mods.logo };
        let bytes = {
            let t = pane.term.lock().unwrap();
            if !t.mouse_tracking() {
                return;
            }
            self.mouse.encode(&t, vt::MouseAction::Motion, 0, vmods, at)
        };
        pane.send(&bytes);
    }

    /// The wheel: to the program when it tracks the mouse, else through the scrollback.
    pub fn wheel(&mut self, id: u32, x: f32, y: f32, dy: f32, precise: bool, mods: rune::Mods) {
        let Some(pane) = self.panes.get(&id) else { return };
        let line = crate::grid::geometry(id).map_or(20.0, |g| g.cell_h / g.scale).max(1.0);
        // Lines back into the scrollback. A notch of a mouse wheel (Rune's 40 points) is three
        // lines, as in most terminals.
        let lines = if precise { dy / line } else { dy / 40.0 * 3.0 };
        let tracking = pane.term.lock().unwrap().mouse_tracking();
        if !tracking || mods.shift {
            let s = self.scrollers.entry(id).or_default();
            match (precise, dy == 0.0) {
                (true, true) => s.lift(),
                (true, false) => s.touch(self.now, lines as f64),
                (false, _) => s.notch(lines as f64),
            }
            return;
        }
        // To the program, in whole lines: keep the part short of one for the next delta.
        let rest = self.wheel_rest.entry(id).or_insert(0.0);
        if *rest * lines < 0.0 {
            *rest = 0.0;
        }
        *rest += lines;
        let rows = rest.trunc() as isize;
        if rows == 0 {
            return;
        }
        *rest -= rows as f32;
        let Some(at) = self.pointer_at(id, x, y) else { return };
        let vmods = vt::Mods { shift: mods.shift, ctrl: mods.ctrl, alt: mods.alt, logo: mods.logo };
        let button = if rows > 0 { 4 } else { 5 };
        let mut bytes = vec![];
        for _ in 0..rows.unsigned_abs().min(10) {
            bytes.extend(self.mouse.encode(&pane.term.lock().unwrap(), vt::MouseAction::Press, button, vmods, at));
        }
        pane.send(&bytes);
    }

    /// Moves each scrolling pane on: whole lines move its viewport, the rest offsets its grid.
    /// Returns whether any moved (to draw).
    fn step_scrolling(&mut self) -> bool {
        let t = self.now;
        let mut moved = false;
        for (id, s) in self.scrollers.iter_mut() {
            let Some(pane) = self.panes.get(id) else { continue };
            let before = s.frac();
            let n = s.step(t);
            {
                let mut term = pane.term.lock().unwrap();
                if n != 0 {
                    let from = term.scroll_offset() as i64;
                    term.scroll(-n as isize);
                    let went = from - term.scroll_offset() as i64;
                    if went != n {
                        s.stop_short(n - went);
                    }
                }
                if term.scroll_offset() == 0 {
                    s.at_top();
                }
            }
            if n != 0 || s.frac() != before {
                pane.dirty.store(true, Ordering::Release);
                moved = true;
            }
        }
        self.scrollers.retain(|_, s| s.active());
        moved
    }

    /// Whether a pane is scrolling: frames should keep coming.
    pub fn scrolling(&self) -> bool {
        !self.scrollers.is_empty()
    }

    /// The part of a line pane `id`'s grid is drawn further back than its viewport.
    pub fn scroll_frac(&self, id: u32) -> f32 {
        self.scrollers.get(&id).map_or(0.0, |s| s.frac())
    }

    /// Typing or pasting goes back to the bottom: the focused pane stops scrolling.
    fn stop_scrolling(&mut self) {
        if let Some(id) = self.focused_id() {
            self.scrollers.remove(&id);
        }
    }

    /// Dragging the gap between two splits: the split at `path` (lefts are false) gets `ratio`.
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.root.set_ratio(path, ratio.clamp(0.08, 0.92));
        }
    }

    /// Where each divider of the shown tab would go so the split before it holds a whole number
    /// of lines (or, across, columns): (path, new ratio), for those that are off by a pixel or
    /// more. The split after a divider takes up the difference.
    fn planned_snaps(&self) -> Vec<(Vec<bool>, f32)> {
        let Some(tab) = self.tab().filter(|t| !t.zoomed) else { return vec![] };
        fn walk(s: &State, tab: &Tab, n: &Node, path: Vec<bool>, out: &mut Vec<(Vec<bool>, f32)>) {
            let Node::Split { dir, ratio, a, b } = n else { return };
            let leaf = a.edge_leaf(*dir);
            if let (Some(g), Some(len)) = (crate::grid::geometry(leaf), s.split_lens.get(&(tab.id, path.clone()))) {
                let (extent, cell) = match dir {
                    Dir::Down => (g.height - 2.0 * g.pad, g.cell_h),
                    Dir::Right => (g.width - 2.0 * g.pad, g.cell_w),
                };
                if cell > 0.0 && extent > cell {
                    let left = extent - (extent / cell).floor() * cell;
                    // The nearer whole line: give up the leftover, or grow to the next line.
                    let by = if left > cell / 2.0 { cell - left } else { -left };
                    let to = (ratio + by / g.scale / len.max(1.0)).clamp(0.08, 0.92);
                    if by.abs() >= 1.0 && (to - ratio).abs() > 1e-4 {
                        out.push((path.clone(), to));
                    }
                }
            }
            walk(s, tab, a, [path.clone(), vec![false]].concat(), out);
            walk(s, tab, b, [path, vec![true]].concat(), out);
        }
        let mut out = vec![];
        walk(self, tab, &tab.root, vec![], &mut out);
        out
    }

    /// Moves the dividers onto whole lines once the layout has rested for a moment (not while
    /// one is dragged, nor mid-animation). Returns whether any moved.
    fn snap_dividers(&mut self, t: f64) -> bool {
        let plan = if self.dragging_divider { vec![] } else { self.planned_snaps() };
        if plan.is_empty() {
            self.snap_seen = None;
            return false;
        }
        let key: Vec<(Vec<bool>, i32)> = plan.iter().map(|(p, r)| (p.clone(), (r * 10_000.0).round() as i32)).collect();
        match &self.snap_seen {
            Some((seen, since)) if *seen == key => {
                if t - since < 0.25 {
                    return false;
                }
                for (path, r) in plan {
                    self.set_ratio(&path, r);
                }
                self.snap_seen = None;
                true
            }
            _ => {
                self.snap_seen = Some((key, t));
                false
            }
        }
    }

    /// Dividers are waiting to snap: come back soon.
    pub fn snap_pending(&self) -> bool {
        self.snap_seen.is_some()
    }

    /// Desktop notifications for splits that want attention while the user can't see them:
    /// the window isn't focused, or the split is in another tab or minimized.
    fn notify(&mut self) {
        if !self.settings.notifications {
            return;
        }
        let visible: Vec<u32> = self.tab().map(|t| t.root.leaves()).unwrap_or_default();
        let mut send = vec![];
        for (id, p) in &self.panes {
            let wants = p.attention.load(Ordering::Acquire);
            if !wants {
                self.notified.remove(id);
                continue;
            }
            if self.notified.contains(id) {
                continue;
            }
            if self.window_focused && visible.contains(id) {
                continue;
            }
            let tab = self.tabs.iter().position(|t| t.has(*id)).map_or(0, |i| i + 1);
            let session = self.session.as_ref().map(|s| format!("{} {}", s.animal, s.name)).unwrap_or_else(|| "runeterm".into());
            let (title, body) = match p.note.lock().unwrap().clone() {
                Some((t, b)) if !t.is_empty() || !b.is_empty() => (if t.is_empty() { p.program.clone() } else { t }, b),
                _ => (format!("{} needs you", p.program), String::new()),
            };
            let body = if body.is_empty() { format!("{session} · tab {tab}") } else { format!("{body}\n{session} · tab {tab}") };
            send.push((*id, title, body));
        }
        for (id, title, body) in send {
            self.notified.insert(id);
            let _ = std::process::Command::new("notify-send")
                .args(["--app-name=runeterm", "--icon=runeterm", &title, &body])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    }
}

/// The physical key a US layout types `c` with, and its unshifted codepoint.
fn ascii_key(c: char) -> (sys::GhosttyKey, u32) {
    let lower = c.to_ascii_lowercase();
    let k = match lower {
        'a'..='z' => sys::GHOSTTY_KEY_A + (lower as sys::GhosttyKey - 'a' as sys::GhosttyKey),
        '0'..='9' => sys::GHOSTTY_KEY_DIGIT_0 + (lower as sys::GhosttyKey - '0' as sys::GhosttyKey),
        ' ' => sys::GHOSTTY_KEY_SPACE,
        '-' | '_' => sys::GHOSTTY_KEY_MINUS,
        '=' | '+' => sys::GHOSTTY_KEY_EQUAL,
        '[' | '{' => sys::GHOSTTY_KEY_BRACKET_LEFT,
        ']' | '}' => sys::GHOSTTY_KEY_BRACKET_RIGHT,
        '\\' | '|' => sys::GHOSTTY_KEY_BACKSLASH,
        ';' | ':' => sys::GHOSTTY_KEY_SEMICOLON,
        '\'' | '"' => sys::GHOSTTY_KEY_QUOTE,
        ',' | '<' => sys::GHOSTTY_KEY_COMMA,
        '.' | '>' => sys::GHOSTTY_KEY_PERIOD,
        '/' | '?' => sys::GHOSTTY_KEY_SLASH,
        '`' | '~' => sys::GHOSTTY_KEY_BACKQUOTE,
        _ => sys::GHOSTTY_KEY_UNIDENTIFIED,
    };
    let unshifted = if c.is_ascii_alphanumeric() {
        lower as u32
    } else if k == sys::GHOSTTY_KEY_UNIDENTIFIED {
        0
    } else {
        c as u32
    };
    (k, unshifted)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_with(n: usize) -> State {
        let mut s = State::new(&Config::default(), false, None);
        for _ in 1..n {
            s.split(Dir::Right);
        }
        s
    }

    #[test]
    fn minimize_and_restore_put_a_split_back_in_place() {
        let mut s = state_with(3);
        let before = s.tabs[0].root.clone();
        let ids = before.leaves();
        s.minimize(ids[1]);
        assert_eq!(s.tabs[0].root.leaves(), vec![ids[0], ids[2]]);
        assert_eq!(s.tabs[0].minimized.len(), 1);
        s.focus(ids[1]); // focusing it (e.g. from the sidebar) restores it
        assert_eq!(s.tabs[0].root, before);
        assert!(s.tabs[0].minimized.is_empty());
        assert_eq!(s.tabs[0].focused, ids[1]);
    }

    #[test]
    fn a_floating_split_docks_back_in_place() {
        let mut s = state_with(3);
        let before = s.tabs[0].root.clone();
        let ids = before.leaves();
        s.toggle_float(ids[1]);
        assert_eq!(s.tabs[0].root.leaves(), vec![ids[0], ids[2]]);
        assert!(s.tabs[0].is_floating(ids[1]) && s.tabs[0].has(ids[1]));
        assert_eq!(s.tabs[0].focused, ids[1]);
        s.toggle_float(ids[1]);
        assert_eq!(s.tabs[0].root, before);
        assert!(s.tabs[0].floating.is_empty());
    }

    #[test]
    fn the_last_split_in_the_layout_does_not_float() {
        let mut s = state_with(1);
        let id = s.tabs[0].focused;
        s.toggle_float(id);
        assert!(s.tabs[0].floating.is_empty());
        assert_eq!(s.tabs[0].root.leaves(), vec![id]);
    }

    #[test]
    fn a_floating_split_stays_inside_the_tab() {
        let mut f = Floating { id: 1, place: None, rect: [0.2, 0.2, 0.5, 0.5], drag_from: None, pinned: false };
        f.drag_from = Some(f.rect);
        f.drag(-1.0, 2.0, false);
        assert_eq!(f.rect, [0.0, 0.5, 0.5, 0.5]);
        f.drag_from = Some(f.rect);
        f.drag(1.0, -1.0, true);
        assert_eq!(f.rect, [0.0, 0.5, 1.0, 0.2]);
    }

    #[test]
    fn a_pinned_float_follows_the_tab_shown() {
        let mut s = state_with(2);
        let ids = s.tabs[0].root.leaves();
        let (pinned, plain) = (ids[1], ids[0]);
        s.toggle_pin(pinned); // floats it and pins it
        assert!(s.is_pinned(pinned) && s.tabs[0].is_floating(pinned));
        s.new_tab(None);
        assert_eq!(s.active, 1);
        assert!(s.tabs[1].is_floating(pinned), "carried to the new tab");
        assert!(!s.tabs[0].has(pinned));
        assert_eq!(s.tabs[1].focused, pinned, "it had focus, and keeps it");
        assert_eq!(s.tabs[0].focused, plain);
        s.select_tab(0);
        assert!(s.tabs[0].is_floating(pinned) && !s.tabs[1].has(pinned));
        // Unpinned, it stays behind.
        s.toggle_pin(pinned);
        assert!(!s.is_pinned(pinned));
        s.select_tab(1);
        assert!(s.tabs[0].is_floating(pinned) && !s.tabs[1].has(pinned));
    }

    #[test]
    fn pinning_a_lone_split_does_nothing() {
        let mut s = state_with(1);
        let id = s.tabs[0].focused;
        s.toggle_pin(id);
        assert!(!s.is_pinned(id) && s.tabs[0].floating.is_empty());
    }

    #[test]
    fn a_minimized_float_comes_back_floating() {
        let mut s = state_with(2);
        let id = s.tabs[0].root.leaves()[1];
        s.toggle_pin(id);
        let rect = s.tabs[0].floating[0].rect;
        s.minimize(id);
        assert!(s.tabs[0].floating.is_empty() && s.tabs[0].minimized.len() == 1);
        assert_ne!(s.tabs[0].focused, id);
        s.restore_last();
        assert!(s.tabs[0].minimized.is_empty());
        assert_eq!(s.tabs[0].floating[0].rect, rect);
        assert!(s.is_pinned(id), "still pinned");
        assert_eq!(s.tabs[0].focused, id);
    }

    #[test]
    fn the_weight_list_spans_the_axis_and_takes_a_typed_number() {
        let w = Pick { setting: Setting::Weight, family: "X".into(), range: Some((200.0, 400.0, 800.0)), current: 450.0, previewed: false };
        let rows = State::pick_items(&w, "");
        let ws: Vec<u32> = rows.iter().map(|r| match r.0 { Action::Set(Setting::Weight, n) => n / 10, _ => panic!() }).collect();
        assert_eq!(ws, [200, 300, 400, 450, 500, 600, 700, 800]);
        assert_eq!(rows[3].2, "in use");
        assert_eq!(rows[2].2, "font default");
        assert_eq!(rows[1].1, "Light · 300");
        assert_eq!(State::pick_items(&w, "35"), vec![]);
        let typed: Vec<_> = State::pick_items(&w, "350").into_iter().map(|r| r.0).collect();
        assert_eq!(typed, [Action::Set(Setting::Weight, 3500)]);
        assert_eq!(State::pick_items(&w, "900"), vec![]);
        assert_eq!(State::pick_items(&w, "semi")[0].0, Action::Set(Setting::Weight, 6000));
        let none = Pick { range: None, ..w };
        assert!(State::pick_items(&none, "").is_empty());
    }

    #[test]
    fn the_width_list_names_condensed_to_expanded() {
        // Martian Mono's width axis.
        let w = Pick { setting: Setting::Width, family: "X".into(), range: Some((75.0, 112.5, 112.5)), current: 100.0, previewed: false };
        let titles: Vec<String> = State::pick_items(&w, "").into_iter().map(|r| r.1).collect();
        assert_eq!(titles, ["Condensed · 75", "SemiCondensed · 87.5", "Normal · 100", "SemiExpanded · 112.5"]);
        assert_eq!(State::pick_items(&w, "")[2].2, "in use");
        assert_eq!(State::pick_items(&w, "semic")[0].0, Action::Set(Setting::Width, 875));
        assert_eq!(State::pick_items(&w, "90")[0].1, "Width · 90");
    }

    #[test]
    fn size_line_height_fit_and_cursor_lists() {
        let pick = |setting, range, current| Pick { setting, family: "X".into(), range: Some(range), current, previewed: false };
        // Font size: the usual sizes and the config's, which is in use; 13 is the default.
        let size = pick(Setting::FontSize, (4.0, 13.0, 96.0), 23.0);
        let rows = State::pick_items(&size, "");
        assert!(rows.iter().any(|r| r.1 == "23 pt" && r.2 == "in use"));
        assert!(rows.iter().any(|r| r.1 == "13 pt" && r.2 == "default"));
        assert_eq!(State::pick_items(&size, "19.5")[0].0, Action::Set(Setting::FontSize, 195));
        // Line height in the config's unit, or in the one typed.
        let px = pick(Setting::LinePixels, (0.0, 0.0, 100.0), 32.0);
        assert!(State::pick_items(&px, "").iter().any(|r| r.1 == "32 px" && r.2 == "in use"));
        let typed: Vec<_> = State::pick_items(&px, "20%").into_iter().map(|r| r.0).collect();
        assert!(typed.contains(&Action::Set(Setting::LinePercent, 200)), "{typed:?}");
        // Fit: off, then up to 15%.
        let fit = pick(Setting::Fit, (0.0, 0.0, 15.0), 5.0);
        let rows = State::pick_items(&fit, "");
        assert_eq!(rows[0].1, "Off: the exact size");
        assert_eq!(rows.last().unwrap().1, "Up to 15%");
        assert!(rows.iter().any(|r| r.1 == "Up to 5%" && r.2 == "in use"));
        // Cursor styles by name; numbers aren't values there.
        let cursor = pick(Setting::CursorStyle, (0.0, 0.0, 3.0), 0.0);
        let names: Vec<String> = State::pick_items(&cursor, "").into_iter().map(|r| r.1).collect();
        assert_eq!(names, ["Bar", "Block", "Underline", "Hollow block"]);
        assert_eq!(State::pick_items(&cursor, "holl")[0].0, Action::Set(Setting::CursorStyle, 30));
    }

    #[test]
    fn a_setting_applies_as_it_would_be_saved() {
        let mut c = Config::default();
        apply_setting(&mut c, Setting::LinePixels, 4.0);
        assert_eq!((c.font.cell_height, c.font.cell_height_px), (0.0, 4.0));
        apply_setting(&mut c, Setting::LinePercent, 30.0);
        assert_eq!((c.font.cell_height, c.font.cell_height_px), (0.3, 0.0));
        apply_setting(&mut c, Setting::Fit, 5.0);
        assert_eq!(c.font.fit, 0.05);
        apply_setting(&mut c, Setting::Width, 87.5);
        apply_setting(&mut c, Setting::Weight, 200.0);
        assert_eq!(c.font.variations[0], vec![("wdth".to_string(), 87.5), ("wght".to_string(), 200.0)]);
        apply_setting(&mut c, Setting::CursorStyle, 3.0);
        assert_eq!(c.cursor_style, vt::CursorStyle::BlockHollow);
    }

    /// Three tabs sit two to a row in the exposé: ↓ goes down a row, → along one.
    #[test]
    fn expose_arrows_cross_rows_of_tabs() {
        let mut s = state_with(1);
        s.new_tab(None);
        s.new_tab(None);
        s.stage = [1000.0, 600.0];
        let [t0, t1, t2] = [0, 1, 2].map(|i| s.tabs[i].root.first());
        s.expose = Some(t0);
        s.expose_dir(Side::Right);
        assert_eq!(s.expose, Some(t1));
        s.expose_dir(Side::Down);
        assert_eq!(s.expose, Some(t2), "below the row, though not under tab 2");
        s.expose_dir(Side::Up);
        assert_eq!(s.expose, Some(t0), "back up: the one level with it");
        s.expose_dir(Side::Left);
        assert_eq!(s.expose, Some(t0), "nothing left of the first");
    }

    #[test]
    fn a_split_moves_to_another_tab_or_a_new_one() {
        let mut s = state_with(2);
        let ids = s.tabs[0].root.leaves();
        s.new_tab(None);
        let other = s.tabs[1].root.first();
        s.select_tab(0);
        // To tab 2: beside its split, and focused there.
        s.move_to_tab(ids[1], Some(1));
        assert_eq!(s.tabs[0].root.leaves(), vec![ids[0]]);
        assert_eq!(s.tabs[1].root.leaves(), vec![other, ids[1]]);
        assert_eq!((s.active, s.tabs[1].focused), (1, ids[1]));
        // To a new tab.
        s.move_to_tab(ids[1], None);
        assert_eq!(s.tabs.len(), 3);
        assert_eq!(s.tabs[2].root.leaves(), vec![ids[1]]);
        assert_eq!(s.active, 2);
        // A tab's only split doesn't move to a new tab, nor to its own.
        s.move_to_tab(ids[1], None);
        s.move_to_tab(ids[1], Some(2));
        assert_eq!(s.tabs.len(), 3);
        // Moving a tab's last split away closes that tab.
        s.move_to_tab(ids[1], Some(0));
        assert_eq!(s.tabs.len(), 2);
        assert_eq!(s.tabs[0].root.leaves(), vec![ids[0], ids[1]]);
        assert_eq!(s.active, 0);
        // A floating split floats in its new tab.
        s.toggle_float(ids[1]);
        s.move_to_tab(ids[1], Some(1));
        assert!(s.tabs[1].is_floating(ids[1]));
        assert!(!s.tabs[0].has(ids[1]));
        // A tab number past the last is a new tab.
        s.move_to_tab(ids[1], Some(8));
        assert_eq!(s.tabs.len(), 3);
        assert_eq!(s.tabs[2].root.leaves(), vec![ids[1]]);
    }

    #[test]
    fn the_palette_filters_and_skips_missing_tabs() {
        let mut s = state_with(1);
        s.palette = Some(Palette { edit: rune::TextEdit::new(""), chosen: 0, pick: None });
        let all = s.palette_items();
        assert!(all.iter().any(|r| r.0 == Action::GotoTab(1)));
        assert!(!all.iter().any(|r| r.0 == Action::GotoTab(2)), "only tabs that exist");
        assert!(!all.iter().any(|r| r.0 == Action::CommandPalette));
        s.palette.as_mut().unwrap().edit = rune::TextEdit::new("minim");
        let rows = s.palette_items();
        assert_eq!(rows[0].0, Action::Minimize);
        assert_eq!(rows[0].2, "⌥M");
        s.palette.as_mut().unwrap().edit = rune::TextEdit::new("pin");
        assert_eq!(s.palette_items()[0].0, Action::PinFloat);
    }

    #[test]
    fn session_names_are_checked() {
        assert!(session_name_problem("cozy-otter").is_none());
        assert!(session_name_problem("").is_some());
        assert!(session_name_problem("two words").is_some());
        assert!(session_name_problem("a/b").is_some());
        assert!(session_name_problem("-x").is_some());
        assert!(session_name_problem(&"x".repeat(33)).is_some());
    }

    #[test]
    fn finds_urls_under_the_pointer() {
        let l = "see https://ghostty.org/docs. or (http://a.b/c_(d)) now";
        assert_eq!(url_at(l, 10).as_deref(), Some("https://ghostty.org/docs"));
        assert_eq!(url_at(l, 40).as_deref(), Some("http://a.b/c_(d)"));
        assert_eq!(url_at(l, 1), None);
    }

    #[test]
    fn editors_open_at_the_end() {
        let p = std::path::Path::new("/tmp/s.txt");
        let base = |c: Vec<String>| -> Vec<String> {
            let mut c = c;
            c[0] = c[0].rsplit('/').next().unwrap().to_string();
            c
        };
        assert_eq!(base(editor_command("vim", p, 40)), vec!["vim", "+", "/tmp/s.txt"]);
        assert_eq!(base(editor_command("nvim -R", p, 40)), vec!["nvim", "-R", "+", "/tmp/s.txt"]);
        assert_eq!(base(editor_command("hx", p, 40)), vec!["hx", "/tmp/s.txt:40"]);
        assert_eq!(base(editor_command("/usr/bin/less", p, 40)), vec!["less", "/tmp/s.txt"]);
    }

    #[test]
    fn the_last_split_in_view_cannot_be_minimized() {
        let mut s = state_with(1);
        let id = s.tabs[0].focused;
        s.minimize(id);
        assert_eq!(s.tabs[0].root, Node::Leaf(id));
    }
}

fn wake() {
    if let Some(w) = WAKER.get() {
        w();
    }
}

/// How to open `path` (of `lines` lines) in `editor` at its last line, where the user was.
fn editor_command(editor: &str, path: &std::path::Path, lines: usize) -> Vec<String> {
    let editor = if editor.is_empty() || editor == "auto" { auto_editor() } else { editor.to_string() };
    let mut words: Vec<String> = editor.split_whitespace().map(String::from).collect();
    if words.is_empty() {
        words.push("vi".into());
    }
    words[0] = find_program(&words[0]).unwrap_or_else(|| words[0].clone());
    let name = words[0].rsplit('/').next().unwrap_or("").to_string();
    let file = path.display().to_string();
    match name.as_str() {
        "vim" | "nvim" | "vi" | "view" => words.extend(["+".to_string(), file]),
        "hx" | "helix" => words.push(format!("{file}:{}", lines.max(1))),
        _ => words.push(file),
    }
    words
}

/// Settings that change how things behave rather than look.
#[derive(Clone, Copy, Debug)]
pub struct Behaviour {
    /// `copy-on-select`: selecting also copies to the clipboard (middle-click pastes the
    /// selection either way).
    pub copy_on_select: bool,
    /// `confirm-close`: ask before closing a split that's running something.
    pub confirm_close: bool,
    /// `notifications`: desktop notifications for splits that want attention out of sight.
    pub notifications: bool,
}

impl Behaviour {
    fn from(c: &Config) -> Behaviour {
        Behaviour { copy_on_select: c.copy_on_select, confirm_close: c.confirm_close, notifications: c.notifications }
    }
}

/// The URL around column `col` of a line, if there is one.
pub fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if col >= chars.len() {
        return None;
    }
    let stop = |c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '`' | '|');
    let (mut a, mut b) = (col, col);
    while a > 0 && !stop(chars[a - 1]) {
        a -= 1;
    }
    while b < chars.len() && !stop(chars[b]) {
        b += 1;
    }
    let word: String = chars[a..b].iter().collect();
    let start = ["https://", "http://", "file://", "mailto:", "ftp://"].iter().filter_map(|p| word.find(p)).min()?;
    // Trailing punctuation is the sentence's, not the URL's (a closing paren only when unopened).
    let mut url = word[start..].to_string();
    while let Some(last) = url.chars().last() {
        let unbalanced = last == ')' && url.matches('(').count() < url.matches(')').count();
        if matches!(last, '.' | ',' | ';' | ':' | '!' | '?' | ']' | '}') || unbalanced {
            url.pop();
        } else {
            break;
        }
    }
    (url.len() > 8).then_some(url)
}

/// Opens a link with the desktop's handler.
pub fn open_link(url: &str) {
    let _ = std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// `$VISUAL`, `$EDITOR`, or the first of nvim, vim, hx, vi that's installed.
fn auto_editor() -> String {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(v) = std::env::var(var) {
            if !v.trim().is_empty() {
                return v;
            }
        }
    }
    ["nvim", "vim", "hx", "vi"].iter().find(|e| find_program(e).is_some()).unwrap_or(&"vi").to_string()
}

/// A program's full path: on the PATH, or in `~/.local/bin` (an app launcher's PATH often lacks
/// it).
fn find_program(name: &str) -> Option<String> {
    if name.contains('/') {
        return Some(name.to_string());
    }
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.push(std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin"));
    dirs.iter().map(|d| d.join(name)).find(|p| p.is_file()).map(|p| p.display().to_string())
}

/// `$USER@hostname`.
fn user_host() -> String {
    let user = std::env::var("USER").unwrap_or_default();
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|h| h.trim().to_string())
        .unwrap_or_default();
    match (user.is_empty(), host.is_empty()) {
        (false, false) => format!("{user}@{host}"),
        (false, true) => user,
        (true, _) => host,
    }
}

