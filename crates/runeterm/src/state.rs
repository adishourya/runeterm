//! App state and what the user can do to it: tabs, splits, focus, swapping, minimizing, and
//! keys to the focused pane.

use crate::config::Config;
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
}

pub struct Tab {
    /// Stable for the tab's life (its view's key, so switching tabs can animate).
    pub id: u32,
    pub root: Node,
    pub focused: u32,
    pub zoomed: bool,
    pub minimized: Vec<Minimized>,
}

impl Tab {
    /// Every pane of the tab, in layout order, minimized ones last.
    pub fn panes(&self) -> Vec<u32> {
        let mut v = self.root.leaves();
        v.extend(self.minimized.iter().map(|m| m.id));
        v
    }

    pub fn has(&self, id: u32) -> bool {
        self.root.contains(id) || self.minimized.iter().any(|m| m.id == id)
    }
}

/// What the session picker shows.
#[derive(Clone, Debug, PartialEq)]
pub enum PickerMode {
    List,
    /// Choosing an emoji for a session, from spooky's list of (glyph, name).
    Emoji { session: String, animals: Vec<(String, String)> },
    /// Asking before ending a session's panes.
    ConfirmKill(String),
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
    /// Appearance: light mode, the accent, and the accents to choose from.
    pub light: bool,
    /// The accent shown: this session's own, or `default_accent` outside a session.
    pub accent: crate::vt::Rgb,
    /// The config's accent.
    pub default_accent: crate::vt::Rgb,
    pub accents: Vec<crate::vt::Rgb>,
    /// The session `accent` was looked up for.
    accent_for: Option<String>,
    /// The spooky session this window is, once the daemon has named it.
    pub session: Option<Session>,
    /// The session whose emoji was last checked for being an animal (see `give_animal`).
    animal_checked: Option<String>,
    /// Wheel movement not yet a whole line, by pane, in lines.
    wheel_rest: HashMap<u32, f32>,
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
            light: config.light,
            accent: config.accent,
            default_accent: config.accent,
            accents: config.accents.clone(),
            accent_for: None,
            session: None,
            animal_checked: None,
            wheel_rest: HashMap::new(),
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
        };
        match attach {
            Some(name) if s.controller.is_some() => s.open_session(&name),
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
        if i != self.active {
            self.slide = if i > self.active { 1.0 } else { -1.0 };
            self.active = i;
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
            self.tabs.push(Tab { id: tab, root: Node::Leaf(id), focused: id, zoomed: false, minimized: vec![] });
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
        if !tab.root.contains(id) {
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
        let Some(tab) = self.tabs.iter_mut().find(|t| t.root.contains(id)) else { return };
        let place = tab.root.place_of(id);
        let Some(rest) = tab.root.remove(id) else { return }; // never the last one in view
        tab.root = rest;
        tab.minimized.push(Minimized { id, place, offset: (0.0, 0.0), drag_from: None });
        if tab.focused == id {
            tab.focused = place.map_or(tab.root.first(), |p| p.beside);
        }
        tab.zoomed = false;
    }

    /// Puts a minimized split back where it was (or beside the focused one).
    pub fn restore(&mut self, id: u32) {
        let Some(tab) = self.tabs.iter_mut().find(|t| t.minimized.iter().any(|m| m.id == id)) else { return };
        let i = tab.minimized.iter().position(|m| m.id == id).unwrap();
        let m = tab.minimized.remove(i);
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
        changed |= self.handle_events();
        changed |= self.follow_session_accent();
        self.give_animal();
        let exited: Vec<u32> = self.panes.values().filter(|p| p.exited.load(Ordering::Acquire)).map(|p| p.id).collect();
        for id in exited {
            self.forget(id);
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
        // Minimized splits are still part of the session: report them where they were, so
        // they stay in it and come back as splits when the session is restored.
        let tabs: Vec<spooky::Node> = self
            .tabs
            .iter()
            .map(|t| {
                let mut root = t.root.clone();
                for m in &t.minimized {
                    let placed = m.place.is_some_and(|p| root.insert(p.beside, m.id, p.dir, p.share, p.first));
                    if !placed {
                        let at = root.first();
                        root.insert(at, m.id, Dir::Right, 0.5, false);
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
        if self.tabs.is_empty() && self.pending_open.is_none() {
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
        if self.controller.is_none() {
            return;
        }
        self.detach_all();
        self.new_tab(None);
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
                self.tabs.push(Tab { id: tab, root, focused, zoomed: false, minimized: vec![] });
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
            self.picker = Some(spooky::sessions());
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
        let c = Config::load();
        self.sidebar = c.sidebar;
        self.settings = Behaviour::from(&c);
        self.scrollback_editor = c.scrollback_editor.clone();
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
            None if self.controller.is_some() => Some(spooky::sessions()),
            None => None,
        };
    }

    fn refresh_picker(&mut self) {
        self.picker_mode = PickerMode::List;
        if self.picker.is_some() {
            self.picker = Some(spooky::sessions());
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

    pub fn ask_kill(&mut self, session: &str) {
        self.picker_mode = PickerMode::ConfirmKill(session.to_string());
    }

    pub fn cancel_kill(&mut self) {
        self.picker_mode = PickerMode::List;
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


    /// Keybinds first (Ghostty's, from the user's config), then everything to the focused pane.
    pub fn key(&mut self, e: &KeyEvent, tx: &mut Tx) -> bool {
        let m = e.mods;
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
        // Ctrl+Shift+C copies the selection, Ctrl+Shift+V pastes.
        if m.ctrl && m.shift && !m.alt {
            let c = match &e.key {
                Key::Command(c) => Some(c.to_ascii_lowercase()),
                Key::Text(t) => t.chars().next().map(|c| c.to_ascii_lowercase()),
                _ => None,
            };
            match c {
                Some('c') => {
                    if let Some(text) = self.focused_pane().and_then(|p| p.term.lock().unwrap().selection_text()) {
                        tx.copy(text);
                    }
                    return true;
                }
                Some('v') => {
                    tx.read_clipboard(|s: &mut State, content, _| s.paste(content.text()));
                    return true;
                }
                _ => {}
            }
        }
        if m.alt && !m.ctrl {
            let ch = match &e.key {
                Key::Text(s) => s.chars().next(),
                Key::Command(c) => Some(*c),
                Key::Enter => Some('\n'),
                _ => None,
            };
            let side = |c: char| match c.to_ascii_lowercase() {
                'h' => Some(Side::Left),
                'j' => Some(Side::Down),
                'k' => Some(Side::Up),
                'l' => Some(Side::Right),
                _ => None,
            };
            let handled = match ch {
                Some(c) if m.shift && side(c).is_some() => {
                    self.swap_dir(side(c).unwrap());
                    true
                }
                Some(_) if m.shift => false,
                Some('-') => { self.split(Dir::Down); true }
                Some('[') => { self.split(Dir::Right); true }
                Some(c) if side(c).is_some() => {
                    self.focus_dir(side(c).unwrap());
                    true
                }
                Some(c @ ('w' | 'a' | 's' | 'd')) => {
                    let side = match c { 'w' => Side::Up, 'a' => Side::Left, 's' => Side::Down, _ => Side::Right };
                    let tab = &mut self.tabs[self.active];
                    tab.root.resize(tab.focused, side, 0.04);
                    true
                }
                Some('\n' | 'z') => { let id = self.tabs[self.active].focused; self.toggle_zoom(id); true }
                Some('x') => { let id = self.tabs[self.active].focused; self.request_close(id); true }
                Some('m') => { let id = self.tabs[self.active].focused; self.minimize(id); true }
                Some('r') => { self.reload_config(); true }
                Some('e') => { self.edit_scrollback(); true }
                Some('/') => { self.toggle_sidebar(); true }
                Some('t') => { self.new_tab_here(); true }
                Some(c @ '1'..='9') => {
                    let i = c as usize - '1' as usize;
                    if i < self.tabs.len() {
                        self.select_tab(i);
                    }
                    true
                }
                _ => false,
            };
            if handled {
                return true;
            }
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
        self.focused_pane().unwrap().send(&bytes);
        true
    }

    pub fn paste(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
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
        // A trackpad sends many small deltas: keep the part short of a whole line for the next
        // one, instead of rounding each away.
        let line = crate::grid::geometry(id).map_or(20.0, |g| g.cell_h / g.scale).max(1.0);
        // A notch of a mouse wheel (Rune's 40 points) is three lines, as in most terminals.
        let lines = if precise { -dy / line } else { -dy / 40.0 * 3.0 };
        let rest = self.wheel_rest.entry(id).or_insert(0.0);
        // Turning back drops what was left of the other way.
        if *rest * lines < 0.0 {
            *rest = 0.0;
        }
        *rest += lines;
        let rows = rest.trunc() as isize;
        if rows == 0 {
            return;
        }
        *rest -= rows as f32;
        let tracking = pane.term.lock().unwrap().mouse_tracking();
        if tracking && !mods.shift {
            let Some(at) = self.pointer_at(id, x, y) else { return };
            let vmods = vt::Mods { shift: mods.shift, ctrl: mods.ctrl, alt: mods.alt, logo: mods.logo };
            let button = if rows < 0 { 4 } else { 5 };
            let mut bytes = vec![];
            for _ in 0..rows.unsigned_abs().min(10) {
                bytes.extend(self.mouse.encode(&pane.term.lock().unwrap(), vt::MouseAction::Press, button, vmods, at));
            }
            pane.send(&bytes);
        } else {
            pane.term.lock().unwrap().scroll(rows);
            pane.dirty.store(true, Ordering::Release);
        }
    }

    /// Dragging the gap between two splits: the split at `path` (lefts are false) gets `ratio`.
    pub fn set_ratio(&mut self, path: &[bool], ratio: f32) {
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.root.set_ratio(path, ratio.clamp(0.08, 0.92));
        }
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
