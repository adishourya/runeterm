//! Rune views. Each split is a real Rune surface: a rounded card with a header (program logo,
//! name, directory, split/minimize/zoom/close buttons) above an `effect` slot where the grid
//! renderer paints the terminal, clipped to the card's corners. Tabs live in a sidebar on the
//! left, each with its splits listed under it. Minimized splits float at the bottom as pills.

use crate::config::Config;
use crate::icons;
use crate::split::{Dir, Node};
use crate::state::{PickerMode, State, Tab, WAKER};
use rune::color::hex;
use rune::tok::motion;
use rune::{
    AppSpec, Color, Cursor, Cx, Cycle, Drag, El, Icon, Phase, TextStyle, Transition, Tx, col, effect, el, icon, row, text,
};

/// Colours and sizes: dark, quiet chrome around the terminals.
pub mod look {
    use super::*;

    pub fn window() -> Color { hex("#070708").unwrap() }
    pub fn text() -> Color { hex("#eef0f3").unwrap() }
    pub fn dim() -> Color { hex("#8b919b").unwrap() }
    pub fn faint() -> Color { hex("#565c66").unwrap() }
    pub fn busy() -> Color { hex("#3ccf6c").unwrap() }
    pub fn attention() -> Color { hex("#ff9a3c").unwrap() }
    pub fn hairline() -> Color { Color::WHITE.alpha(0.05) }
    pub const RADIUS: f32 = 14.0;
    pub const GAP: f32 = 8.0;
}

const TITLE: TextStyle = TextStyle::new(13.0, 600.0);
const BODY: TextStyle = TextStyle::new(12.5, 420.0);
const SMALL: TextStyle = TextStyle::new(11.5, 450.0);
const CAPS: TextStyle = TextStyle::new(10.0, 650.0);

/// The focused split's glow breathes; an attention dot pulses.
const BREATHE: Cycle = Cycle { period: 7.0, phase: 0.0, stops: &[(0.0, 0.55), (0.5, 1.0), (1.0, 0.55)], still: 0.8, fps: 30.0 };
const PULSE: Cycle = Cycle { period: 1.4, phase: 0.0, stops: &[(0.0, 1.0), (0.5, 0.35), (1.0, 1.0)], still: 1.0, fps: 30.0 };

fn pane_bg(s: &State) -> Color {
    let b = s.theme.bg;
    hex(&format!("#{:02x}{:02x}{:02x}", b.0, b.1, b.2)).unwrap()
}

/// A program's logo on a tinted, rounded tile.
fn logo(program: &str, size: f32, attention: bool) -> El {
    let (i, c) = icons::program(program);
    let c = if attention { look::attention() } else { c };
    el().size(size, size).radius(size * 0.3).fill(c.alpha(0.15)).border(1.0, c.alpha(0.2)).center()
        .child(icon(i, size * 0.62).color(c))
}

fn dot(c: Color, pulse: bool) -> El {
    let d = el().size(7.0, 7.0).radius(3.5).fill(c).shadow(0.0, 8.0, c.alpha(0.55));
    if pulse { d.cycle(PULSE) } else { d }
}

fn tool(key: String, i: &'static Icon, help: &str, f: impl Fn(&mut State) + 'static) -> El {
    el().key(key).size(26.0, 26.0).radius(7.0).center()
        .hover(Color::WHITE.alpha(0.08)).press(Color::WHITE.alpha(0.14))
        .cursor(Cursor::Pointer)
        .help(help.to_string())
        .on_press(f)
        .child(icon(i, 15.0).color(look::dim()))
}

/// One split: the Rune surface with its header, and the terminal's effect slot under it.
fn pane(s: &State, tab: &Tab, id: u32) -> El {
    let focused = id == tab.focused;
    let (program, cwd) = s.panes.get(&id).map(|p| (p.program.as_str(), p.cwd.as_str())).unwrap_or(("", ""));
    let attention = s.needs_attention(id);
    let busy = !icons::is_shell(program);
    let only = tab.root.leaves().len() == 1;

    let mut title = row().gap(9.0).items_center().grow(1.0).min_w(0.0)
        .child(logo(program, 20.0, attention))
        .child(text(program.to_string()).font(TITLE).color(if attention { look::attention() } else { look::text() }).single_line())
        .child(text(cwd.to_string()).font(BODY).color(look::dim()).single_line().truncate().shrink());
    if attention {
        title = title.child(dot(look::attention(), true));
    } else if busy {
        title = title.child(dot(look::busy(), false));
    }
    // Unfocused splits keep their buttons hidden until hovered.
    let mut tools = row().gap(1.0).ml(8.0)
        .child(tool(format!("split-r-{id}"), &icons::SPLIT_RIGHT, "Split right  ⌥[", move |s: &mut State| {
            s.focus(id);
            s.split(Dir::Right)
        }))
        .child(tool(format!("split-d-{id}"), &icons::SPLIT_DOWN, "Split down  ⌥-", move |s: &mut State| {
            s.focus(id);
            s.split(Dir::Down)
        }));
    if !only {
        tools = tools
            .child(tool(format!("min-{id}"), &icons::MINIMIZE, "Minimize  ⌥M", move |s: &mut State| s.minimize(id)))
            .child(tool(format!("zoom-{id}"), if tab.zoomed { &icons::COLLAPSE } else { &icons::EXPAND }, "Zoom  ⌥↩", move |s: &mut State| s.toggle_zoom(id)));
    }
    tools = tools.child(tool(format!("close-{id}"), &icons::CLOSE, "Close  ⌥X", move |s: &mut State| s.close(id)));
    let tools = if focused { tools } else { tools.group_opacity(0.0, 1.0) };

    let header = row().key(format!("head-{id}")).h(40.0).w_full().pl(12.0).pr(8.0).items_center()
        .opacity(if focused { 1.0 } else { 0.62 })
        .child(title)
        .child(tools);

    let tint = if attention { look::attention() } else { icons::program(program).1 };
    let mut card = col().key(format!("pane-{id}")).matched(format!("pane-{id}"))
        .w_full().h_full().min_w(0.0).min_h(0.0)
        .radius(look::RADIUS).fill(pane_bg(s)).border(1.0, look::hairline())
        .clip_rounded(look::RADIUS)
        .hover_group()
        .transition(Transition::scale(0.96).combined(Transition::opacity()))
        .on_press(move |s: &mut State| s.focus(id))
        .on_wheel(move |s: &mut State, w, _| {
            if let Some(p) = s.panes.get(&id) {
                let mut t = p.term.lock().unwrap();
                if !t.mouse_tracking() {
                    t.scroll((-w.dy / 20.0).round() as isize);
                    p.dirty.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            true
        });
    if focused || attention {
        // A soft light from the top of the card, tinted by the program, slowly breathing.
        card = card
            .shadow(1.0, 3.0, Color::BLACK.alpha(0.45))
            .shadow(14.0, 44.0, Color::BLACK.alpha(0.55))
            .child(
                el().key(format!("glow-{id}")).absolute(0.0, 0.0).w_full().h(220.0)
                    .radial_gradient([0.5, 0.0], [620.0, 220.0], [(0.0, tint.alpha(if focused { 0.13 } else { 0.09 })), (1.0, tint.alpha(0.0))])
                    .cycle(BREATHE)
                    .transition(Transition::opacity()),
            )
            .child(
                // A hairline of the same light along the top edge.
                el().key(format!("rim-{id}")).absolute(0.0, 0.0).w_full().h(1.0)
                    .linear_gradient(std::f32::consts::FRAC_PI_2, [(0.0, tint.alpha(0.0)), (0.5, tint.alpha(0.45)), (1.0, tint.alpha(0.0))])
                    .cycle(BREATHE)
                    .transition(Transition::opacity()),
            );
    }
    card.child(header).child(effect(id).key(format!("grid-{id}")).w_full().grow(1.0).min_h(0.0))
}

fn node(s: &State, tab: &Tab, n: &Node) -> El {
    match n {
        Node::Leaf(id) => pane(s, tab, *id),
        Node::Split { dir, ratio, a, b } => {
            let wrap = |e: El, g: f32| el().grow(g).min_w(0.0).min_h(0.0).w_full().h_full().child(e);
            let c = match dir {
                Dir::Right => row(),
                Dir::Down => col(),
            };
            c.w_full().h_full().gap(look::GAP).min_w(0.0).min_h(0.0)
                .child(wrap(node(s, tab, a), *ratio))
                .child(wrap(node(s, tab, b), 1.0 - ratio))
        }
    }
}

/// One split in the sidebar, under its tab.
fn leaf(s: &State, tab: &Tab, id: u32, active_tab: bool) -> El {
    let p = s.panes.get(&id);
    let program = p.map(|p| p.program.clone()).unwrap_or_default();
    let cwd = p.map(|p| p.cwd.clone()).unwrap_or_default();
    let attention = s.needs_attention(id);
    let minimized = tab.minimized.iter().any(|m| m.id == id);
    let focused = active_tab && tab.focused == id && !minimized;
    let mut r = row().key(format!("leaf-{id}")).w_full().h(30.0).px(6.0).gap(8.0).items_center().radius(8.0)
        .hover(Color::WHITE.alpha(0.04)).cursor(Cursor::Pointer)
        .transition(Transition::offset(0.0, -4.0).combined(Transition::opacity()))
        .on_press(move |s: &mut State| s.focus(id))
        .child(logo(&program, 18.0, attention))
        .child(
            text(program)
                .font(TextStyle::new(12.5, if focused { 600.0 } else { 500.0 }))
                .color(if attention { look::attention() } else if focused { look::text() } else { look::dim() })
                .single_line(),
        )
        .child(text(cwd).font(SMALL).color(look::faint()).single_line().truncate().shrink().grow(1.0));
    if focused {
        r = r.fill(Color::WHITE.alpha(0.06));
    }
    if minimized {
        r = r.opacity(0.7).child(icon(&icons::MINIMIZE, 12.0).color(look::faint()));
    }
    if attention {
        r = r.child(dot(look::attention(), true));
    } else if p.is_some_and(|p| !icons::is_shell(&p.program)) {
        r = r.child(dot(look::busy(), false));
    }
    r
}

fn tab_group(s: &State, i: usize, tab: &Tab) -> El {
    let active = i == s.active;
    let focused = s.panes.get(&tab.focused);
    let title = focused
        .map(|p| p.cwd.rsplit('/').next().filter(|x| !x.is_empty()).unwrap_or(&p.cwd).to_string())
        .unwrap_or_default();
    let ids = tab.panes();
    let attention = ids.iter().any(|id| s.needs_attention(*id));

    let mut head = row().key(format!("tab-{i}")).w_full().h(32.0).px(6.0).gap(8.0).items_center().radius(8.0)
        .cursor(Cursor::Pointer)
        .on_press(move |s: &mut State| s.select_tab(i))
        .child(
            el().size(20.0, 20.0).radius(6.0).center()
                .fill(if active { Color::WHITE.alpha(0.1) } else { Color::WHITE.alpha(0.05) })
                .child(text(format!("{}", i + 1)).font(CAPS).color(if active { look::text() } else { look::dim() })),
        )
        .child(text(title).font(TITLE).color(if active { look::text() } else { look::dim() }).single_line().truncate().shrink().grow(1.0));
    if attention && !active {
        head = head.child(dot(look::attention(), true));
    }
    if i < 9 {
        head = head.child(text(format!("⌥{}", i + 1)).font(CAPS).color(look::faint()));
    }

    // Splits hang off a thin rail, like a tree.
    let mut leaves = col().gap(2.0).grow(1.0).min_w(0.0);
    for id in ids {
        leaves = leaves.child(leaf(s, tab, id, active));
    }
    let tree = row().w_full().pl(15.0).gap(7.0).items_stretch()
        .child(el().w(1.0).fill(Color::WHITE.alpha(if active { 0.1 } else { 0.06 })).my(4.0))
        .child(leaves);

    let mut g = col().key(format!("group-{i}")).w_full().p_all(4.0).gap(2.0).radius(12.0)
        .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
        .child(head)
        .child(tree);
    if active {
        g = g.fill(Color::WHITE.alpha(0.03)).border(1.0, Color::WHITE.alpha(0.04));
    } else {
        g = g.hover(Color::WHITE.alpha(0.02));
    }
    g
}

/// `.when(cond, |e| …)`: applies a change only when `cond` holds.
trait When: Sized {
    fn when(self, cond: bool, f: impl FnOnce(Self) -> Self) -> Self {
        if cond { f(self) } else { self }
    }
}
impl When for El {}

trait PadAll {
    fn p_all(self, p: f32) -> Self;
}
impl PadAll for El {
    fn p_all(self, p: f32) -> Self {
        self.padding(p)
    }
}

/// Each session's own colour, picked from its emoji so a session keeps it everywhere.
fn session_hue(s: &State) -> Color {
    const HUES: [&str; 10] = ["#ff9f5a", "#ffd166", "#7ee081", "#5cc8ff", "#a78bfa", "#ff7ab6", "#4fd1c5", "#ff6b6b", "#c3e88d", "#82aaff"];
    let key = s.session.as_ref().map(|x| if x.animal.is_empty() { x.name.clone() } else { x.animal.clone() }).unwrap_or_default();
    let h = key.bytes().fold(2166136261u32, |h, b| (h ^ b as u32).wrapping_mul(16777619));
    hex(HUES[(h % HUES.len() as u32) as usize]).unwrap()
}

/// The session's badge: its emoji on a glowing tile of the session's colour. It opens the
/// session picker.
fn badge(s: &State, size: f32) -> El {
    let c = session_hue(s);
    let face = match &s.session {
        Some(sess) if !sess.animal.is_empty() => text(sess.animal.clone()).font(TextStyle::new(size * 0.54, 400.0)),
        _ => icon(&icons::TERMINAL, size * 0.46).color(c),
    };
    el().key("session-badge").size(size, size).radius(size * 0.32).center()
        .fill(hex("#15161a").unwrap())
        .radial_gradient([0.5, 0.0], [size * 1.1, size * 1.1], [(0.0, c.alpha(0.42)), (0.65, c.alpha(0.12)), (1.0, c.alpha(0.04))])
        .border(1.0, c.alpha(0.45))
        .shadow(2.0, 4.0, Color::BLACK.alpha(0.5))
        .shadow(6.0, 22.0, c.alpha(0.28))
        .hover(Color::WHITE.alpha(0.06)).press(Color::WHITE.alpha(0.1)).cursor(Cursor::Pointer)
        .help("Sessions")
        .on_press(|s: &mut State| s.toggle_picker())
        .child(face)
}

fn session_name(s: &State) -> String {
    match (&s.session, s.uses_spooky()) {
        (Some(sess), _) => sess.name.clone(),
        (None, true) => "…".into(),
        (None, false) => "local".into(),
    }
}

/// A battery: an outline filled to its level (amber, then red, when low; green while
/// charging), and the percentage.
fn battery(b: crate::status::Battery, scale: f32) -> El {
    let level = b.level as f32 / 100.0;
    let color = if b.charging {
        look::busy()
    } else if b.level <= 10 {
        hex("#ff6b6b").unwrap()
    } else if b.level <= 20 {
        hex("#ffb454").unwrap()
    } else {
        Color::WHITE.alpha(0.82)
    };
    let (w, h) = (22.0 * scale, 11.0 * scale);
    let body = el().size(w, h).radius(3.0 * scale).border(1.2, Color::WHITE.alpha(0.42)).p_all(1.6 * scale)
        .child(el().h_full().w(((w - 3.2 * scale - 2.4) * level).max(1.5)).radius(1.6 * scale).fill(color));
    let mut r = row().gap(2.0).items_center()
        .child(body)
        .child(el().size(2.0 * scale, 5.0 * scale).radius(1.0).fill(Color::WHITE.alpha(0.42)));
    if b.charging {
        r = r.child(icon(&icons::BOLT, 12.0 * scale).color(color));
    }
    r.child(text(format!("{}%", b.level)).font(TextStyle::new(11.0 * scale, 600.0)).color(look::dim()).ml(3.0))
}

/// The collapsed sidebar: the session's badge and name, the time and battery, then a badge
/// per tab.
fn rail(s: &State) -> El {
    let w = s.sidebar.rail;
    let mut top = col().gap(8.0).items_center().w_full()
        .child(badge(s, (w - 22.0).clamp(40.0, 64.0)))
        .child(text(session_name(s)).font(TextStyle::new(11.0, 650.0)).color(look::text()).single_line().truncate().max_w(w - 12.0));
    if s.sidebar.clock {
        top = top.child(col().items_center().gap(1.0).mt(6.0)
            .child(text(s.status.time.clone()).font(TextStyle::new(15.0, 650.0)).color(look::text()))
            .child(text(s.status.date.split(' ').take(2).collect::<Vec<_>>().join(" ")).font(TextStyle::new(9.5, 500.0)).color(look::faint())));
    }
    if let (true, Some(b)) = (s.sidebar.battery, s.status.battery) {
        top = top.child(battery(b, 0.82));
    }
    let mut tabs = col().gap(8.0).items_center();
    for (i, tab) in s.tabs.iter().enumerate() {
        let active = i == s.active;
        let attention = tab.panes().iter().any(|id| s.needs_attention(*id));
        let mut tile = el().key(format!("rail-tab-{i}")).size(42.0, 42.0).radius(13.0).center()
            .fill(if active { Color::WHITE.alpha(0.1) } else { Color::WHITE.alpha(0.035) })
            .border(1.0, if active { Color::WHITE.alpha(0.1) } else { Color::TRANSPARENT })
            .hover(Color::WHITE.alpha(0.06)).cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| s.select_tab(i))
            .child(text(format!("{}", i + 1)).font(TextStyle::new(14.0, 650.0)).color(if active { look::text() } else { look::dim() }));
        if attention {
            tile = tile.child(dot(look::attention(), true).absolute(32.0, 3.0));
        } else if tab.panes().len() > 1 {
            // A pip per split, under the number.
            let mut pips = row().gap(2.0).absolute(0.0, 33.0).w_full().justify_center();
            for _ in 0..tab.panes().len().min(4) {
                pips = pips.child(el().size(3.0, 3.0).radius(1.5).fill(if active { look::dim() } else { look::faint() }));
            }
            tile = tile.child(pips);
        }
        tabs = tabs.child(tile);
    }
    let plus = el().key("rail-new-tab").size(42.0, 42.0).radius(13.0).center()
        .hover(Color::WHITE.alpha(0.06)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.new_tab_here())
        .child(icon(&icons::PLUS, 17.0).color(look::dim()));
    col().w(w).h_full().py(14.0).gap(16.0).items_center()
        .child(top)
        .child(el().w(26.0).h(1.0).fill(Color::WHITE.alpha(0.07)))
        .child(tabs)
        .child(el().grow(1.0))
        .child(plus)
}

/// The open sidebar: a card for the session (badge, name, time, battery), then every tab
/// with its splits.
fn wide(s: &State) -> El {
    let c = session_hue(s);
    let w = s.sidebar.width;
    let panes: usize = s.tabs.iter().map(|t| t.panes().len()).sum();
    let head = row().key("session-head").gap(14.0).items_center().cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.toggle_picker())
        .child(badge(s, 60.0))
        .child(col().gap(3.0).grow(1.0).min_w(0.0)
            .child(text("SESSION").font(CAPS).color(c.alpha(0.9)))
            .child(text(session_name(s)).font(TextStyle::new(19.0, 700.0)).color(look::text()).single_line().truncate())
            .child(text(format!("{} tab{} · {} split{}", s.tabs.len(), if s.tabs.len() == 1 { "" } else { "s" }, panes, if panes == 1 { "" } else { "s" })).font(SMALL).color(look::faint()))
            .when(s.sidebar.host && !s.user_host.is_empty(), |c| {
                c.child(row().gap(5.0).items_center().mt(2.0)
                    .child(icon(&icons::SERVER, 11.0).color(look::faint()))
                    .child(text(s.user_host.clone()).font(TextStyle::new(11.5, 500.0).mono()).color(look::dim()).single_line().truncate()))
            }))
        .child(icon(&icons::CHEVRON, 15.0).color(look::faint()));
    let mut card = col().key("session-card").w_full().p_all(14.0).gap(14.0).radius(18.0)
        .fill(hex("#121317").unwrap())
        .gradient(c.alpha(0.13), c.alpha(0.02))
        .border(1.0, c.alpha(0.18))
        .child(head);
    if s.sidebar.clock || (s.sidebar.battery && s.status.battery.is_some()) {
        let mut status = row().w_full().items_end().gap(10.0);
        if s.sidebar.clock {
            status = status.child(col().gap(1.0)
                .child(text(s.status.time.clone()).font(TextStyle::new(26.0, 650.0)).color(look::text()))
                .child(text(s.status.date.clone()).font(SMALL).color(look::dim())));
        }
        status = status.child(el().grow(1.0));
        if let (true, Some(b)) = (s.sidebar.battery, s.status.battery) {
            status = status.child(battery(b, 1.0).mb(3.0));
        }
        card = card.child(el().h(1.0).w_full().fill(Color::WHITE.alpha(0.06))).child(status);
    }

    let mut list = col().gap(6.0).w_full();
    for (i, tab) in s.tabs.iter().enumerate() {
        list = list.child(tab_group(s, i, tab));
    }
    let new_tab = row().key("new-tab").h(38.0).w_full().px(10.0).gap(8.0).items_center().radius(10.0)
        .hover(Color::WHITE.alpha(0.04)).press(Color::WHITE.alpha(0.08)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.new_tab_here())
        .child(icon(&icons::PLUS, 16.0).color(look::dim()))
        .child(text("New tab").font(BODY).color(look::dim()))
        .child(el().grow(1.0))
        .child(text("⌥T").font(CAPS).color(look::faint()));
    col().w(w).h_full().py(12.0).px(12.0).gap(8.0)
        .child(card)
        .child(text("TABS").font(CAPS).color(look::faint()).px(10.0).mt(8.0))
        .child(list)
        .child(el().grow(1.0))
        .child(new_tab)
}

/// The sidebar. Pinned, it's always open and the splits make room. Otherwise it's a rail
/// floating over the splits' left edge that opens out on hover, so the splits never resize.
fn sidebar(s: &State) -> El {
    let pinned = s.sidebar.pinned;
    let open = pinned || s.sidebar_open || s.picker.is_some();
    let mut bar = el().key("sidebar").absolute(0.0, 0.0).h_full()
        .w(if open { s.sidebar.width } else { s.sidebar.rail })
        .clip_rounded(0.5)
        .raise()
        .child(if open { wide(s) } else { rail(s) });
    if !pinned {
        bar = bar.on_hover_change(|s: &mut State, hovered, _| s.sidebar_open = hovered);
    }
    if open && !pinned {
        bar = bar.fill(hex("#0b0b0d").unwrap()).border_sides([0.0, 1.0, 0.0, 0.0], Color::WHITE.alpha(0.05))
            .shadow(0.0, 48.0, Color::BLACK.alpha(0.6));
    }
    bar
}

/// The session picker: start a new session here, bring one back into this window, change a
/// session's emoji, or kill one.
fn picker(s: &State, sessions: &[crate::spooky::SessionInfo]) -> El {
    let body = match &s.picker_mode {
        PickerMode::Emoji { session, animals } => emoji_grid(sessions, session, animals),
        mode => session_list(s, sessions, mode),
    };
    let panel = col().key("picker").absolute(s.sidebar.width + 8.0, 12.0).w(356.0).max_h(600.0).p_all(8.0).gap(4.0).radius(16.0)
        .fill(hex("#131417").unwrap().alpha(0.96)).backdrop_blur(24.0)
        .border(1.0, Color::WHITE.alpha(0.08))
        .shadow(4.0, 12.0, Color::BLACK.alpha(0.45)).shadow(24.0, 64.0, Color::BLACK.alpha(0.55))
        .raise()
        .transition(Transition::offset(0.0, -6.0).combined(Transition::scale(0.97)).combined(Transition::opacity()))
        .child(body);
    // Clicking anywhere else closes it.
    el().key("picker-layer").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).raise()
        .child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).on_press(|s: &mut State| s.picker = None))
        .child(panel)
}

fn session_list(s: &State, sessions: &[crate::spooky::SessionInfo], mode: &PickerMode) -> El {
    let current = s.session.as_ref().map(|x| x.name.as_str());
    let new = row().key("picker-new").h(52.0).w_full().px(10.0).gap(12.0).items_center().radius(12.0)
        .hover(Color::WHITE.alpha(0.05)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.new_session())
        .child(el().size(34.0, 34.0).radius(10.0).center().fill(Color::WHITE.alpha(0.06)).border(1.0, Color::WHITE.alpha(0.08))
            .child(icon(&icons::PLUS, 16.0).color(look::text())))
        .child(col().gap(2.0)
            .child(text("New session").font(TITLE).color(look::text()))
            .child(text("Start fresh in this window").font(SMALL).color(look::faint())));
    let mut list = col().gap(2.0).w_full();
    for info in sessions {
        let here = current == Some(info.name.as_str());
        let (tint, label) = match (here, info.state.as_str()) {
            (true, _) => (look::busy(), "this window"),
            (_, "open") => (hex("#7aa2f7").unwrap(), "open"),
            (_, "detached") => (hex("#c0a36e").unwrap(), "detached"),
            _ => (look::faint(), "saved"),
        };
        let panes = format!("{} pane{}", info.panes, if info.panes == 1 { "" } else { "s" });
        if *mode == PickerMode::ConfirmKill(info.name.clone()) {
            list = list.child(confirm_kill(info, &panes));
            continue;
        }
        let detail = match (info.panes, info.running.is_empty()) {
            (0, _) => "no running panes".to_string(),
            (_, true) => panes,
            (_, false) => format!("{panes} · {}", info.running),
        };
        let (open, emoji, kill) = (info.name.clone(), info.name.clone(), info.name.clone());
        let tile = el().key(format!("picker-emoji-{}", info.name)).size(36.0, 36.0).radius(10.0).center()
            .fill(tint.alpha(0.14)).border(1.0, tint.alpha(0.26))
            .hover(Color::WHITE.alpha(0.08)).hover_alone().cursor(Cursor::Pointer)
            .help("Change emoji")
            .on_press(move |s: &mut State| s.choose_emoji(&emoji))
            .child(text(info.animal.clone()).font(TextStyle::new(19.0, 400.0)));
        let trash = el().key(format!("picker-kill-{}", info.name)).size(28.0, 28.0).radius(8.0).center()
            .hover(look::attention().alpha(0.16)).cursor(Cursor::Pointer)
            .help("Kill session")
            .group_opacity(0.0, 1.0)
            .on_press(move |s: &mut State| s.ask_kill(&kill))
            .child(icon(&icons::TRASH, 15.0).color(look::dim()));
        let mut r = row().key(format!("picker-{}", info.name)).h(54.0).w_full().pl(9.0).pr(6.0).gap(12.0).items_center().radius(12.0)
            .hover(Color::WHITE.alpha(0.05)).hover_group().cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| s.open_session(&open))
            .child(tile)
            .child(col().gap(2.0).grow(1.0).min_w(0.0)
                .child(text(info.name.clone()).font(TITLE).color(look::text()).single_line().truncate())
                .child(text(detail).font(SMALL).color(look::faint()).single_line().truncate()))
            .child(el().h(20.0).px(8.0).radius(10.0).center().fill(tint.alpha(0.12))
                .child(text(label).font(CAPS).color(tint)))
            .child(trash);
        if here {
            r = r.fill(Color::WHITE.alpha(0.04));
        }
        list = list.child(r);
    }
    col().w_full().gap(4.0)
        .child(row().h(28.0).px(10.0).items_center()
            .child(text("SESSIONS").font(CAPS).color(look::faint()))
            .child(el().grow(1.0))
            .child(text("spooky").font(CAPS).color(look::faint().alpha(0.7))))
        .child(new)
        .child(el().h(1.0).w_full().my(4.0).fill(Color::WHITE.alpha(0.06)))
        .child(list)
}

/// Killing ends the session's panes, so the row asks first.
fn confirm_kill(info: &crate::spooky::SessionInfo, panes: &str) -> El {
    let red = hex("#ff6b6b").unwrap();
    let name = info.name.clone();
    let button = |key: String, label: &str, fill: Color, color: Color| {
        el().key(key).h(28.0).px(12.0).radius(8.0).center().fill(fill).hover(Color::WHITE.alpha(0.08)).cursor(Cursor::Pointer)
            .child(text(label.to_string()).font(TextStyle::new(12.0, 600.0)).color(color))
    };
    row().key(format!("picker-confirm-{}", info.name)).h(54.0).w_full().px(10.0).gap(10.0).items_center().radius(12.0)
        .fill(red.alpha(0.08)).border(1.0, red.alpha(0.25))
        .transition(Transition::opacity())
        .child(col().gap(2.0).grow(1.0).min_w(0.0)
            .child(text(format!("Kill {}?", info.name)).font(TITLE).color(look::text()).single_line().truncate())
            .child(text(format!("Ends its {panes}")).font(SMALL).color(look::dim()).single_line()))
        .child(button(format!("cancel-kill-{}", info.name), "Cancel", Color::WHITE.alpha(0.06), look::text()).on_press(|s: &mut State| s.cancel_kill()))
        .child(button(format!("do-kill-{}", info.name), "Kill", red.alpha(0.9), Color::WHITE).on_press(move |s: &mut State| s.kill_session(&name)))
}

/// Every emoji spooky offers, in two sections; the session's current one is ringed.
fn emoji_grid(sessions: &[crate::spooky::SessionInfo], session: &str, animals: &[(String, String)]) -> El {
    let current = sessions.iter().find(|i| i.name == session).map(|i| i.animal.clone()).unwrap_or_default();
    let split = animals.iter().position(|(_, n)| n == "soccer").unwrap_or(animals.len());
    let section = |title: &str, items: &[(String, String)]| {
        let mut grid = row().flex_wrap().gap(4.0).w_full();
        for (glyph, name) in items {
            let (session, name) = (session.to_string(), name.clone());
            let chosen = *glyph == current;
            let mut cell = el().key(format!("emoji-{name}")).size(42.0, 42.0).radius(11.0).center()
                .hover(Color::WHITE.alpha(0.08)).press(Color::WHITE.alpha(0.12)).cursor(Cursor::Pointer)
                .help(name.clone())
                .on_press(move |s: &mut State| s.set_emoji(&session, &name))
                .child(text(glyph.clone()).font(TextStyle::new(22.0, 400.0)));
            if chosen {
                cell = cell.fill(look::busy().alpha(0.14)).border(1.5, look::busy().alpha(0.7));
            }
            grid = grid.child(cell);
        }
        col().gap(6.0).w_full()
            .child(text(title.to_uppercase()).font(CAPS).color(look::faint()).px(4.0))
            .child(grid)
    };
    let back = el().key("emoji-back").size(28.0, 28.0).radius(8.0).center()
        .hover(Color::WHITE.alpha(0.08)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.cancel_kill())
        .child(icon(&icons::BACK, 16.0).color(look::dim()));
    let mut c = col().w_full().gap(10.0).p_all(2.0)
        .child(row().h(32.0).gap(8.0).items_center()
            .child(back)
            .child(text(current.clone()).font(TextStyle::new(18.0, 400.0)))
            .child(text(format!("Emoji for {session}")).font(TITLE).color(look::text()).single_line().truncate()));
    if animals.is_empty() {
        return c.child(text("spooky didn't list any emoji").font(SMALL).color(look::faint()).px(4.0));
    }
    c = c.child(section("Animals", &animals[..split]));
    if split < animals.len() {
        c = c.child(section("Sports", &animals[split..]));
    }
    c
}

/// A minimized split, floating at the bottom: drag it anywhere, click to bring it back.
fn pill(s: &State, m: &crate::state::Minimized) -> El {
    let id = m.id;
    let p = s.panes.get(&id);
    let program = p.map(|p| p.program.clone()).unwrap_or_default();
    let cwd = p.map(|p| p.cwd.clone()).unwrap_or_default();
    let attention = s.needs_attention(id);
    let mut r = row().key(format!("pill-{id}")).matched(format!("pane-{id}"))
        .h(40.0).pl(8.0).pr(14.0).gap(9.0).items_center().radius(20.0)
        .fill(hex("#16171a").unwrap().alpha(0.92))
        .backdrop_blur(18.0)
        .border(1.0, if attention { look::attention().alpha(0.5) } else { Color::WHITE.alpha(0.09) })
        .shadow(2.0, 6.0, Color::BLACK.alpha(0.4))
        .shadow(14.0, 40.0, Color::BLACK.alpha(0.5))
        .offset(m.offset.0, m.offset.1)
        .hover(Color::WHITE.alpha(0.04))
        .cursor(Cursor::Grab)
        .transition(Transition::offset(0.0, 16.0).combined(Transition::scale(0.9)).combined(Transition::opacity()))
        .help("Drag to move · click to restore")
        .on_drag(move |s: &mut State, d: &mut Drag, tx: &mut Tx| {
            let Some(tab) = s.tabs.iter_mut().find(|t| t.minimized.iter().any(|m| m.id == id)) else { return };
            let m = tab.minimized.iter_mut().find(|m| m.id == id).unwrap();
            match d.phase {
                Phase::Down => m.drag_from = Some(m.offset),
                Phase::Move => {
                    let from = m.drag_from.unwrap_or(m.offset);
                    m.offset = (from.0 + d.dx, from.1 + d.dy);
                    tx.instant(); // follow the pointer, no spring
                }
                Phase::Up => {
                    m.drag_from = None;
                    if d.dx.abs() + d.dy.abs() < 4.0 {
                        s.focus(id);
                    }
                }
            }
        })
        .child(logo(&program, 24.0, attention))
        .child(text(program).font(TITLE).color(if attention { look::attention() } else { look::text() }).single_line())
        .child(text(cwd).font(SMALL).color(look::faint()).single_line().truncate().max_w(180.0));
    if attention {
        r = r.child(dot(look::attention(), true));
    }
    r.child(icon(&icons::RESTORE, 14.0).color(look::dim()))
}

pub fn view(s: &State, _: &mut Cx<State>) -> El {
    let main = match s.tab() {
        Some(tab) if tab.zoomed => pane(s, tab, tab.focused),
        Some(tab) => node(s, tab, &tab.root),
        None => el(),
    };
    // Each tab is its own layer, keyed by the tab: switching tabs slides the new one in from
    // below (a tab further down) or above, while the old one leaves the other way.
    let dir = s.slide;
    // Offset and fade only: a scale would resize every split on every frame of the slide.
    let slide = |d: f32| Transition::offset(0.0, 90.0 * d).combined(Transition::opacity());
    let layer = el().key(format!("tab-layer-{}", s.tab().map_or(0, |t| t.id)))
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .transition(slide(dir))
        .child(main);
    let mut stage = el().key("stage").grow(1.0).h_full().min_w(0.0)
        .clip_rounded(look::RADIUS)
        .child_exit(slide(-dir))
        .child(layer);
    if let Some(tab) = s.tab().filter(|t| !t.minimized.is_empty()) {
        let mut dock = row().key("dock").gap(10.0).justify_center();
        for m in &tab.minimized {
            dock = dock.child(pill(s, m));
        }
        stage = stage.child(el().pin(Some(0.0), None, Some(0.0), Some(18.0)).child(row().w_full().justify_center().child(dock)));
    }
    let mut root = el().w_full().h_full().fill(look::window()).motion(motion::GLIDE)
        .child(row().w_full().h_full().pl(if s.sidebar.pinned { s.sidebar.width } else { s.sidebar.rail }).py(look::GAP).pr(look::GAP).child(stage))
        .child(sidebar(s));
    if let Some(sessions) = &s.picker {
        root = root.child(picker(s, sessions));
    }
    root
}

pub fn spec() -> AppSpec<State> {
    AppSpec::<State> {
        setup: Some(|_, waker| {
            let _ = WAKER.set(waker);
        }),
        poll: Some(|s, t, tx: &mut Tx| {
            // The clock and battery change on their own: look again every few seconds.
            tx.wake_after(std::time::Duration::from_secs(5));
            s.poll(t)
        }),
        on_key: Some(|s, e, _: &mut Tx| s.key(e)),
        scenes: &[
            ("demo", demo),
            ("sidebar", |s| s.sidebar_open = true),
            ("pinned", |s| s.sidebar.pinned = true),
            ("picker", |s| s.toggle_picker()),
            ("confirm-kill", |s| {
                s.toggle_picker();
                let name = s.picker.as_ref().and_then(|l| l.first()).map(|x| x.name.clone()).unwrap_or_default();
                s.ask_kill(&name);
            }),
            ("emoji", |s| {
                s.toggle_picker();
                let name = s.picker.as_ref().and_then(|l| l.first()).map(|x| x.name.clone()).unwrap_or_default();
                s.choose_emoji(&name);
            }),
        ],
        ..AppSpec::new("runeterm", (1280.0, 780.0), init, view)
    }
}

/// `runeterm` starts a new spooky session; `runeterm attach NAME` (or `-s NAME`) opens one.
/// `RUNETERM_LOCAL=1` runs without spooky.
fn init() -> State {
    let args: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with("--")).collect();
    let attach = match args.as_slice() {
        [cmd, name, ..] if cmd == "attach" || cmd == "-s" => Some(name.clone()),
        _ => None,
    };
    let local = std::env::var("RUNETERM_LOCAL").is_ok_and(|v| v == "1");
    State::new(&Config::load(), !local, attach)
}

/// A headless scene: three splits (one minimized), two tabs and real output, for `--snap`.
/// `RUNETERM_DEMO_DIR` may hold `box.txt` and `img.kitty` to show in the panes.
fn demo(s: &mut State) {
    s.split(Dir::Right);
    let first = s.tabs[0].root.first();
    s.focus(first);
    s.split(Dir::Down);
    s.split(Dir::Right);
    s.new_tab(None);
    s.select_tab(0);
    let ids = s.tabs[0].root.leaves();
    let cmds = [
        "printf '\\033[1;32m➜\\033[0m  \\033[34mgit:(\\033[31mmain\\033[34m)\\033[0m git log --oneline -3\\n\\033[33m75af7d8\\033[0m spooky: reset modifyOtherKeys\\n\\033[33m201e863\\033[0m spooky: export SPOOKY_ANIMAL\\n'\n",
        "clear; cat \"$RUNETERM_DEMO_DIR/box.txt\"\n",
        "printf 'waiting for input\\a\\n'\n",
        "clear; cat \"$RUNETERM_DEMO_DIR/img.kitty\"\n",
    ];
    for (id, cmd) in ids.iter().zip(cmds) {
        if let Some(p) = s.panes.get(id) {
            p.send(cmd.as_bytes());
        }
    }
    s.minimize(ids[2]);
    s.focus(ids[0]);
    std::thread::sleep(std::time::Duration::from_millis(1800));
    for p in s.panes.values_mut() {
        p.refresh_meta(None);
    }
}
