//! Rune views. Each split is a real Rune surface: a rounded card with a header (program logo,
//! name, directory, split/minimize/zoom/close buttons) above an `effect` slot where the grid
//! renderer paints the terminal, clipped to the card's corners. Tabs live in a sidebar on the
//! left, each with its splits listed under it. Minimized splits float at the bottom as pills.

use crate::config::Config;
use crate::icons;
use crate::keys::Action;
use crate::split::{Dir, Node};
use crate::state::{PickerMode, State, Tab, WAKER};
use rune::color::hex;
use rune::tok::motion;
use rune::{
    PointerButton, Spring,
    AppSpec, Color, Cursor, Cx, Cycle, Drag, El, Icon, Phase, TextStyle, Transition, Tx, col, effect, el, icon, row, text,
};

/// Colours and sizes: dark, quiet chrome around the terminals.
pub mod look {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        static LIGHT: Cell<bool> = const { Cell::new(false) };
        static ACCENT: Cell<Color> = const { Cell::new(Color { r: 0.54, g: 0.71, b: 0.97, a: 1.0 }) };
    }

    /// Set at the start of every view: light or dark, and the accent.
    pub fn set(light: bool, accent: Color) {
        LIGHT.with(|l| l.set(light));
        ACCENT.with(|a| a.set(accent));
    }

    pub fn light() -> bool {
        LIGHT.with(|l| l.get())
    }

    fn pick(dark: &str, light_: &str) -> Color {
        hex(if light() { light_ } else { dark }).unwrap()
    }

    pub fn accent() -> Color { ACCENT.with(|a| a.get()) }
    pub fn window() -> Color { pick("#070708", "#e8e8eb") }
    pub fn text() -> Color { pick("#eef0f3", "#1c1f24") }
    pub fn dim() -> Color { pick("#8b919b", "#5b616b") }
    pub fn faint() -> Color { pick("#565c66", "#959ba5") }
    pub fn busy() -> Color { pick("#3ccf6c", "#1f9d4c") }
    pub fn attention() -> Color { pick("#ff9a3c", "#e07a12") }
    /// Overlays and hairlines: white on dark, black on light.
    pub fn ink(a: f32) -> Color {
        if light() { Color::BLACK.alpha(a * 0.9) } else { Color::WHITE.alpha(a) }
    }
    /// Shadows, softer on light.
    pub fn shade(a: f32) -> Color {
        Color::BLACK.alpha(if light() { a * 0.32 } else { a })
    }
    pub fn hairline() -> Color { ink(0.05) }
    /// Raised surfaces: 0 the open sidebar, 1 cards, 2 popovers, 3 tiles and pills.
    pub fn surface(level: u8) -> Color {
        match level {
            0 => pick("#0b0b0d", "#f3f3f5"),
            1 => pick("#121317", "#ffffff"),
            2 => pick("#131417", "#fbfbfc"),
            _ => pick("#16171a", "#ffffff"),
        }
    }
    pub const RADIUS: f32 = 14.0;
    pub const GAP: f32 = 8.0;
}

/// Splits resize on this: quick, so text never sits scaled for long.
const FAST: Spring = Spring::new(0.1, 0.0);

const TITLE: TextStyle = TextStyle::new(13.0, 600.0);
const BODY: TextStyle = TextStyle::new(12.5, 420.0);
const SMALL: TextStyle = TextStyle::new(11.5, 450.0);
const CAPS: TextStyle = TextStyle::new(10.0, 650.0);

/// The focused split's glow breathes; an attention dot pulses.
const BREATHE: Cycle = Cycle { period: 7.0, phase: 0.0, stops: &[(0.0, 0.78), (0.5, 1.0), (1.0, 0.78)], still: 0.9, fps: 30.0 };
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

/// While a split is full screen, its zoom button is a glowing circle in the accent; clicking
/// it (or ⌥Z) brings the other splits back.
fn zoomed_button(s: &State, id: u32) -> El {
    let c = look::accent();
    el().key(format!("zoom-{id}")).size(26.0, 26.0).radius(13.0).center()
        .fill(c.alpha(0.9))
        .border(1.0, c)
        .shadow(0.0, 14.0, c.alpha(0.55))
        .hover(look::ink(0.12)).press(look::ink(0.2)).cursor(Cursor::Pointer)
        .transition(Transition::scale(0.6).combined(Transition::opacity()))
        .cycle(BREATHE)
        .help(s.keybinds.hint("Exit full screen", Action::Zoom))
        .on_press(move |s: &mut State| s.toggle_zoom(id))
        .child(icon(&icons::COLLAPSE, 13.0).color(hex("#0b0b0d").unwrap()))
}

fn tool(key: String, i: &'static Icon, help: String, f: impl Fn(&mut State) + 'static) -> El {
    el().key(key).size(26.0, 26.0).radius(7.0).center()
        .hover(look::ink(0.08)).press(look::ink(0.14))
        .cursor(Cursor::Pointer)
        .help(help)
        .on_press(f)
        .child(icon(i, 15.0).color(look::dim()))
}

/// A floating split's pin: lit in the accent while it's pinned across tabs.
fn pin_tool(s: &State, id: u32) -> El {
    let pinned = s.is_pinned(id);
    let c = look::accent();
    let help = s.keybinds.hint(if pinned { "Unpin (stay in this tab)" } else { "Pin across tabs" }, Action::PinFloat);
    let b = tool(format!("pin-{id}"), &icons::PIN, help, move |s: &mut State| s.toggle_pin(id));
    if pinned {
        el().key(format!("pin-{id}")).size(26.0, 26.0).radius(7.0).center()
            .fill(c.alpha(0.18)).border(1.0, c.alpha(0.45))
            .hover(c.alpha(0.28)).cursor(Cursor::Pointer)
            .help(s.keybinds.hint("Unpin (stay in this tab)", Action::PinFloat))
            .on_press(move |s: &mut State| s.toggle_pin(id))
            .child(icon(&icons::PIN, 15.0).color(c))
    } else {
        b
    }
}

/// One split: the Rune surface with its header, and the terminal's effect slot under it.
fn pane(s: &State, tab: &Tab, id: u32) -> El {
    let focused = id == tab.focused;
    let (program, cwd) = s.panes.get(&id).map(|p| (p.program.as_str(), p.cwd.as_str())).unwrap_or(("", ""));
    let attention = s.needs_attention(id);
    let floating = tab.is_floating(id);
    let only = tab.root.leaves().len() == 1;

    let mut title = row().gap(9.0).items_center().grow(1.0).min_w(0.0)
        .child(logo(program, 20.0, attention))
        .child(text(program.to_string()).font(TITLE).color(if attention { look::attention() } else { look::text() }).single_line())
        .child(text(cwd.to_string()).font(BODY).color(look::dim()).single_line().truncate().shrink());
    if attention {
        title = title.child(dot(look::attention(), true));
    }
    // Unfocused splits keep their buttons hidden until hovered.
    let k = &s.keybinds;
    let mut tools = row().gap(1.0).ml(8.0);
    if floating {
        tools = tools
            .child(pin_tool(s, id))
            .child(tool(format!("min-{id}"), &icons::MINIMIZE, k.hint("Minimize", Action::Minimize), move |s: &mut State| s.minimize(id)))
            .child(tool(format!("dock-{id}"), &icons::DOCK, k.hint("Dock", Action::Float), move |s: &mut State| s.toggle_float(id)));
    } else {
        tools = tools
            .child(tool(format!("split-r-{id}"), &icons::SPLIT_RIGHT, k.hint("Split right", Action::SplitRight), move |s: &mut State| {
                s.focus(id);
                s.split(Dir::Right)
            }))
            .child(tool(format!("split-d-{id}"), &icons::SPLIT_DOWN, k.hint("Split down", Action::SplitDown), move |s: &mut State| {
                s.focus(id);
                s.split(Dir::Down)
            }));
    }
    if !only && !floating {
        tools = tools
            .child(tool(format!("float-{id}"), &icons::FLOAT, k.hint("Float", Action::Float), move |s: &mut State| s.toggle_float(id)))
            .child(tool(format!("min-{id}"), &icons::MINIMIZE, k.hint("Minimize", Action::Minimize), move |s: &mut State| s.minimize(id)))
            .child(if tab.zoomed {
                zoomed_button(s, id)
            } else {
                tool(format!("zoom-{id}"), &icons::EXPAND, k.hint("Full screen", Action::Zoom), move |s: &mut State| s.toggle_zoom(id))
            });
    }
    tools = tools.child(tool(format!("close-{id}"), &icons::CLOSE, k.hint("Close", Action::Close), move |s: &mut State| s.request_close(id)));
    let tools = if focused { tools } else { tools.group_opacity(0.0, 1.0) };

    let mut header = row().key(format!("head-{id}")).h(40.0).w_full().pl(12.0).pr(8.0).items_center()
        .opacity(if focused { 1.0 } else { 0.62 })
        .child(title)
        .child(tools);
    if floating {
        // A floating split moves by its header.
        header = header.cursor(Cursor::Grab).on_drag(move |s: &mut State, d: &mut Drag, tx: &mut Tx| float_drag(s, id, d, tx, false));
    }

    let tint = if attention { look::attention() } else { look::accent() };
    let mut card = col().key(format!("pane-{id}")).matched(format!("pane-{id}"))
        .w_full().h_full().min_w(0.0).min_h(0.0)
        .radius(look::RADIUS).fill(pane_bg(s)).border(1.0, look::hairline())
        .clip_rounded(look::RADIUS)
        .hover_group()
        .transition(Transition::scale(0.96).combined(Transition::opacity()))
        // Resizes land fast: text scaled mid-spring looks smeared.
        .motion(FAST)
        .cursor(Cursor::Default)
        .on_press(move |s: &mut State| s.focus(id))
        // Taken here so a drag on the header doesn't move the dividers around it.
        .on_drag(|_: &mut State, _: &mut Drag, _: &mut Tx| {});
    if focused || attention {
        // A band of the accent across the header strip only (it never reaches the terminal):
        // full strength across almost all of it, fading out just at the two ends. Steady, so
        // the focused split reads at a glance.
        card = card
            .shadow(1.0, 3.0, look::shade(0.45))
            .shadow(14.0, 44.0, look::shade(0.55))
            .child(
                el().key(format!("glow-{id}")).absolute(0.0, 0.0).w_full().h(40.0)
                    .linear_gradient(
                        std::f32::consts::FRAC_PI_2,
                        {
                            let peak = if focused { 0.34 } else { 0.18 };
                            [
                                (0.0, tint.alpha(peak * 0.15)),
                                (0.06, tint.alpha(peak * 0.75)),
                                (0.16, tint.alpha(peak)),
                                (0.84, tint.alpha(peak)),
                                (0.94, tint.alpha(peak * 0.75)),
                                (1.0, tint.alpha(peak * 0.15)),
                            ]
                        },
                    )
                    .transition(Transition::opacity()),
            )
            .child(
                // A hairline of the same light along the top edge.
                el().key(format!("rim-{id}")).absolute(0.0, 0.0).w_full().h(1.0)
                    .linear_gradient(std::f32::consts::FRAC_PI_2, [(0.0, tint.alpha(0.1)), (0.12, tint.alpha(0.85)), (0.88, tint.alpha(0.85)), (1.0, tint.alpha(0.1))])
                    .cycle(BREATHE)
                    .transition(Transition::opacity()),
            );
    }
    // The grid takes the mouse: selection, links, and mouse reports for programs that ask.
    let grid = effect(id).key(format!("grid-{id}")).w_full().grow(1.0).min_h(0.0)
        .cursor(Cursor::Text)
        .pointer_buttons([PointerButton::Middle])
        .on_drag(move |s: &mut State, d: &mut Drag, tx: &mut Tx| {
            let (button, mods) = (tx.button(), tx.mods());
            s.mouse(id, d.phase, d.x, d.y, button, mods, tx)
        })
        .on_pointer_move_local(move |s: &mut State, p, tx: &mut Tx| {
            let mods = tx.mods();
            s.mouse_moved(id, p.x, p.y, mods)
        })
        .on_wheel(move |s: &mut State, w, _| {
            s.wheel(id, w.x, w.y, w.dy, w.precise, w.mods);
            true
        });
    card = card.child(header).child(grid);
    if floating {
        // The corner it resizes from.
        card = card.child(
            el().key(format!("resize-{id}")).pin(None, None, Some(0.0), Some(0.0)).size(22.0, 22.0)
                .cursor(Cursor::Move).help("Drag to resize")
                .on_drag(move |s: &mut State, d: &mut Drag, tx: &mut Tx| float_drag(s, id, d, tx, true))
                .child(el().pin(None, None, Some(6.0), Some(6.0)).size(8.0, 8.0).radius(2.0)
                    .border(1.5, look::ink(0.25))),
        );
    }
    if s.confirm_close == Some(id) {
        card = card.child(row().pin(Some(0.0), None, Some(0.0), Some(18.0)).justify_center().child(confirm_close(s, id, program)));
    }
    card
}

/// Dragging a floating split's header (or, with `resize`, its corner).
fn float_drag(s: &mut State, id: u32, d: &mut Drag, tx: &mut Tx, resize: bool) {
    let [w, h] = s.stage;
    let Some(f) = s.tabs.iter_mut().flat_map(|t| t.floating.iter_mut()).find(|f| f.id == id) else { return };
    match d.phase {
        Phase::Down => f.drag_from = Some(f.rect),
        Phase::Move => {
            f.drag(d.dx / w.max(1.0), d.dy / h.max(1.0), resize);
            tx.instant(); // follow the pointer, no spring
        }
        Phase::Up => {
            f.drag_from = None;
            s.focus(id);
        }
    }
}

/// The floating splits of `tab` over its layout: the pinned ones (`pinned`), which stay put while
/// tabs slide, or the tab's own.
fn floats(s: &State, tab: &Tab, pinned: bool) -> El {
    let [w, h] = s.stage;
    let key = if pinned { "floats-pinned".to_string() } else { format!("floats-{}", tab.id) };
    let mut layer = el().key(key).pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0));
    for f in tab.floating.iter().filter(|f| f.pinned == pinned) {
        let [x, y, fw, fh] = f.rect;
        let card = el().key(format!("float-{}", f.id)).absolute(x * w, y * h).size(fw * w, fh * h)
            .radius(look::RADIUS)
            .shadow(6.0, 20.0, look::shade(0.5)).shadow(24.0, 70.0, look::shade(0.6));
        layer = layer.child(card.raise().child(pane(s, tab, f.id)));
    }
    layer
}

/// Asking before closing a split that's running something: Enter closes, Escape keeps it.
fn confirm_close(_s: &State, id: u32, program: &str) -> El {
    let red = hex("#ff6b6b").unwrap();
    let button = |key: String, label: &str, fill: Color, color: Color| {
        el().key(key).h(30.0).px(14.0).radius(9.0).center().fill(fill).hover(look::ink(0.08)).cursor(Cursor::Pointer)
            .child(text(label.to_string()).font(TextStyle::new(12.5, 600.0)).color(color))
    };
    row().key(format!("confirm-close-{id}")).h(50.0).pl(16.0).pr(8.0).gap(10.0).items_center().radius(16.0)
        .fill(look::surface(2).alpha(0.97)).backdrop_blur(18.0)
        .border(1.0, red.alpha(0.35))
        .shadow(4.0, 12.0, look::shade(0.4)).shadow(18.0, 48.0, look::shade(0.5))
        .transition(Transition::offset(0.0, 10.0).combined(Transition::opacity()))
        .on_press(|_: &mut State| {})
        .child(icons::program_logo(program, 22.0))
        .child(col().gap(1.0)
            .child(text(format!("Close {program}?")).font(TITLE).color(look::text()))
            .child(text("It's still running").font(SMALL).color(look::dim())))
        .child(el().w(12.0))
        .child(button(format!("keep-{id}"), "Cancel  esc", look::ink(0.07), look::text()).on_press(|s: &mut State| s.confirm_close = None))
        .child(button(format!("do-close-{id}"), "Close  ↩", red.alpha(0.9), Color::WHITE).on_press(move |s: &mut State| {
            s.confirm_close = None;
            s.close(id);
        }))
}

fn node(s: &State, tab: &Tab, n: &Node, path: Vec<bool>) -> El {
    match n {
        Node::Leaf(id) => pane(s, tab, *id),
        Node::Split { dir, ratio, a, b } => {
            let wrap = |e: El, g: f32| el().grow(g).min_w(0.0).min_h(0.0).w_full().h_full().motion(FAST).child(e);
            let across = *dir == Dir::Right;
            let c = if across { row() } else { col() };
            let (pa, pb) = ([path.clone(), vec![false]].concat(), [path.clone(), vec![true]].concat());
            // The gap between the two is a handle: drag it to move the divider.
            c.key(format!("split-{}", path.iter().map(|b| if *b { '1' } else { '0' }).collect::<String>()))
                .w_full().h_full().gap(look::GAP).min_w(0.0).min_h(0.0).motion(FAST)
                .cursor(if across { Cursor::ResizeColumn } else { Cursor::ResizeRow })
                .on_drag({
                    let path = path.clone();
                    move |s: &mut State, d: &mut Drag, tx: &mut Tx| {
                        let (pos, len) = if across { (d.x, d.w) } else { (d.y, d.h) };
                        let r = (pos - look::GAP / 2.0) / (len - look::GAP).max(1.0);
                        s.set_ratio(&path, r);
                        s.dragging_divider = !matches!(d.phase, Phase::Up);
                        tx.instant();
                    }
                })
                // Its length, for snapping the divider to whole lines.
                .on_layout({
                    let key = (tab.id, path.clone());
                    move |s: &mut State, r: [f32; 4]| {
                        let len = if across { r[2] } else { r[3] } - look::GAP;
                        if s.split_lens.get(&key) != Some(&len) {
                            s.split_lens.insert(key.clone(), len);
                        }
                    }
                })
                .child(wrap(node(s, tab, a, pa), *ratio))
                .child(wrap(node(s, tab, b, pb), 1.0 - ratio))
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
        .hover(look::ink(0.04)).cursor(Cursor::Pointer)
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
        r = r.fill(look::ink(0.06));
    }
    if minimized {
        r = r.opacity(0.7).child(icon(&icons::MINIMIZE, 12.0).color(look::faint()));
    }
    if s.is_pinned(id) {
        r = r.child(icon(&icons::PIN, 12.0).color(look::accent()));
    } else if tab.is_floating(id) {
        r = r.child(icon(&icons::FLOAT, 12.0).color(look::faint()));
    }
    if attention {
        r = r.child(dot(look::attention(), true));
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
                .fill(if active { look::ink(0.1) } else { look::ink(0.05) })
                .child(text(format!("{}", i + 1)).font(CAPS).color(if active { look::text() } else { look::dim() })),
        )
        .child(text(title).font(TITLE).color(if active { look::text() } else { look::dim() }).single_line().truncate().shrink().grow(1.0));
    if attention && !active {
        head = head.child(dot(look::attention(), true));
    }
    if i < 9 {
        head = head.child(text(s.keybinds.label(Action::GotoTab(i as u8 + 1))).font(CAPS).color(look::faint()));
    }

    // Splits hang off a thin rail, like a tree.
    let mut leaves = col().gap(2.0).grow(1.0).min_w(0.0);
    for id in ids {
        leaves = leaves.child(leaf(s, tab, id, active));
    }
    let tree = row().w_full().pl(15.0).gap(7.0).items_stretch()
        .child(el().w(1.0).fill(look::ink(if active { 0.1 } else { 0.06 })).my(4.0))
        .child(leaves);

    let mut g = col().key(format!("group-{i}")).w_full().p_all(4.0).gap(2.0).radius(12.0)
        .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()))
        .child(head)
        .child(tree);
    if active {
        let accent = look::accent();
        g = g.fill(look::ink(0.03)).gradient(accent.alpha(0.07), accent.alpha(0.0)).border(1.0, accent.alpha(0.16))
            .shadow(4.0, 18.0, accent.alpha(0.08));
    } else {
        g = g.hover(look::ink(0.02));
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

/// The session's colour: the accent, so the badge, the selected tab and the focused split
/// glow alike.
fn session_hue(_: &State) -> Color {
    look::accent()
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
        .fill(look::surface(3))
        .radial_gradient([0.5, 0.0], [size * 1.1, size * 1.1], [(0.0, c.alpha(0.42)), (0.65, c.alpha(0.12)), (1.0, c.alpha(0.04))])
        .border(1.0, c.alpha(0.45))
        .shadow(2.0, 4.0, look::shade(0.5))
        .shadow(6.0, 22.0, c.alpha(0.28))
        .hover(look::ink(0.06)).press(look::ink(0.1)).cursor(Cursor::Pointer)
        .help("Sessions · right-click for the emoji")
        .on_press(|s: &mut State| s.toggle_picker())
        .on_secondary_press(|s: &mut State, _, _: &mut Tx| s.choose_own_emoji())
        .child(face)
}

fn session_name(s: &State) -> String {
    match (&s.session, s.uses_spooky()) {
        (Some(sess), _) => sess.name.clone(),
        (None, true) => "…".into(),
        (None, false) => "local".into(),
    }
}

/// A battery: an outline filled to its level (amber, then red, when low; green on the
/// charger, with a bolt while charging or a plug when it's full), and the percentage.
fn battery(b: crate::status::Battery, scale: f32) -> El {
    let level = b.level as f32 / 100.0;
    let color = if b.charging || b.plugged {
        look::busy()
    } else if b.level <= 10 {
        hex("#ff6b6b").unwrap()
    } else if b.level <= 20 {
        hex("#ffb454").unwrap()
    } else {
        look::ink(0.82)
    };
    let (w, h) = (22.0 * scale, 11.0 * scale);
    let body = el().size(w, h).radius(3.0 * scale).border(1.2, look::ink(0.42)).p_all(1.6 * scale)
        .child(el().h_full().w(((w - 3.2 * scale - 2.4) * level).max(1.5)).radius(1.6 * scale).fill(color));
    let mut r = row().gap(2.0).items_center()
        .child(body)
        .child(el().size(2.0 * scale, 5.0 * scale).radius(1.0).fill(look::ink(0.42)));
    if b.charging {
        r = r.child(icon(&icons::BOLT, 12.0 * scale).color(color));
    } else if b.plugged {
        // On the charger but not charging (full, or held by the firmware).
        r = r.child(icon(&icons::PLUG, 12.0 * scale).color(color));
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
        let accent = look::accent();
        let mut tile = el().key(format!("rail-tab-{i}")).size(42.0, 42.0).radius(13.0).center()
            .fill(if active { look::ink(0.08) } else { look::ink(0.035) })
            .when(active, |t| {
                t.radial_gradient([0.5, 0.0], [46.0, 46.0], [(0.0, accent.alpha(0.22)), (1.0, accent.alpha(0.03))])
                    .border(1.0, accent.alpha(0.3))
                    .shadow(4.0, 14.0, accent.alpha(0.16))
            })
            .hover(look::ink(0.06)).cursor(Cursor::Pointer)
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
        .hover(look::ink(0.06)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.new_tab_here())
        .child(icon(&icons::PLUS, 17.0).color(look::dim()));
    let light = s.light;
    let mode = el().key("rail-mode").size(42.0, 42.0).radius(13.0).center()
        .hover(look::ink(0.06)).cursor(Cursor::Pointer)
        .help(if light { "Dark mode" } else { "Light mode" })
        .on_press(move |s: &mut State| s.set_light(!light))
        .child(icon(if light { &icons::MOON } else { &icons::SUN }, 16.0).color(look::dim()));
    col().w(w).h_full().py(14.0).gap(16.0).items_center()
        .child(top)
        .child(el().w(26.0).h(1.0).fill(look::ink(0.07)))
        .child(tabs)
        .child(el().grow(1.0))
        .child(col().gap(4.0).items_center().child(mode).child(plus))
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
            .child(text(session_name(s)).font(TextStyle::new(19.0, 700.0)).color(look::text()).single_line().truncate()
                .key("session-title").help("Double-click to rename")
                .on_double_press_tx(|s: &mut State, tx: &mut Tx| {
                    if let Some(name) = s.session.as_ref().map(|x| x.name.clone()) {
                        s.start_rename(&name, tx);
                    }
                }))
            .child(text(format!("{} tab{} · {} split{}", s.tabs.len(), if s.tabs.len() == 1 { "" } else { "s" }, panes, if panes == 1 { "" } else { "s" })).font(SMALL).color(look::faint()))
            .when(s.sidebar.host && !s.user_host.is_empty(), |c| {
                c.child(row().gap(5.0).items_center().mt(2.0)
                    .child(icon(&icons::SERVER, 11.0).color(look::faint()))
                    .child(text(s.user_host.clone()).font(TextStyle::new(11.5, 500.0).mono()).color(look::dim()).single_line().truncate()))
            }))
        .child(icon(&icons::CHEVRON, 15.0).color(look::faint()));
    let mut card = col().key("session-card").w_full().p_all(14.0).gap(14.0).radius(18.0)
        .fill(look::surface(1))
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
        card = card.child(el().h(1.0).w_full().fill(look::ink(0.06))).child(status);
    }

    let mut list = col().gap(6.0).w_full();
    for (i, tab) in s.tabs.iter().enumerate() {
        list = list.child(tab_group(s, i, tab));
    }
    let new_tab = row().key("new-tab").h(38.0).w_full().px(10.0).gap(8.0).items_center().radius(10.0)
        .hover(look::ink(0.04)).press(look::ink(0.08)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.new_tab_here())
        .child(icon(&icons::PLUS, 16.0).color(look::dim()))
        .child(text("New tab").font(BODY).color(look::dim()))
        .child(el().grow(1.0))
        .child(text(s.keybinds.label(Action::NewTab)).font(CAPS).color(look::faint()));
    col().w(w).h_full().py(12.0).px(12.0).gap(8.0)
        .child(card)
        .child(text("TABS").font(CAPS).color(look::faint()).px(10.0).mt(8.0))
        .child(list)
        .child(el().grow(1.0))
        .child(new_tab)
        .child(appearance(s))
}

fn rgb_color(c: crate::vt::Rgb) -> Color {
    hex(&format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)).unwrap()
}

/// Light or dark: a sun and a moon, the current one raised.
fn mode_switch(s: &State) -> El {
    let half = |light: bool| {
        let on = s.light == light;
        el().key(format!("mode-{}", if light { "light" } else { "dark" })).size(30.0, 26.0).radius(8.0).center()
            .fill(if on { look::surface(3) } else { Color::TRANSPARENT })
            .border(1.0, if on { look::ink(0.08) } else { Color::TRANSPARENT })
            .shadow(1.0, 3.0, look::shade(if on { 0.25 } else { 0.0 }))
            .cursor(Cursor::Pointer)
            .help(if light { "Light" } else { "Dark" })
            .on_press(move |s: &mut State| s.set_light(light))
            .child(icon(if light { &icons::SUN } else { &icons::MOON }, 14.0).color(if on { look::text() } else { look::faint() }))
    };
    row().p_all(2.0).gap(2.0).radius(10.0).fill(look::ink(0.05)).child(half(false)).child(half(true))
}

/// The bottom of the open sidebar: light or dark, and (opened by clicking the row) the
/// accent swatches.
fn appearance(s: &State) -> El {
    let open = s.appearance_open;
    let head = row().key("appearance-head").h(30.0).gap(8.0).items_center().cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.appearance_open = !s.appearance_open)
        .child(el().size(10.0, 10.0).radius(5.0).fill(look::accent()).shadow(0.0, 8.0, look::accent().alpha(0.6)))
        .child(text("APPEARANCE").font(CAPS).color(look::faint()))
        .child(icon(&icons::CHEVRON, 12.0).color(look::faint()).icon_rotation(if open { std::f32::consts::PI } else { 0.0 }))
        .child(el().grow(1.0))
        .child(mode_switch(s));
    let mut c = col().key("appearance").w_full().gap(8.0).px(10.0).py(6.0).radius(14.0)
        .fill(look::ink(0.03)).border(1.0, look::ink(0.04))
        .child(head);
    if open {
        let mut swatches = row().key("swatches").gap(7.0).items_center().w_full().flex_wrap().pb(4.0)
            .transition(Transition::offset(0.0, 6.0).combined(Transition::opacity()));
        for &col in &s.accents {
            let on = col == s.accent;
            let color = rgb_color(col);
            swatches = swatches.child(
                el().key(format!("accent-{}", crate::config::hex_of(col))).size(20.0, 20.0).radius(10.0).center()
                    .fill(color)
                    .border(2.0, if on { look::text() } else { Color::TRANSPARENT })
                    .shadow(0.0, if on { 12.0 } else { 0.0 }, color.alpha(0.6))
                    .hover(look::ink(0.15)).cursor(Cursor::Pointer)
                    .help(crate::config::hex_of(col))
                    .on_press(move |s: &mut State| s.set_accent(col)),
            );
        }
        c = c.child(swatches);
        // `font-size-fit` moved the size or spacing so the lines fill the splits: say so.
        if let Some((pt, line)) = crate::grid::fitted() {
            let mut what = format!("Fitted: {pt:.1} pt");
            if line.abs() >= 0.5 {
                what += &format!(" · line {line:+.0}%");
            }
            c = c.child(text(what).font(SMALL).color(look::faint()).pb(2.0));
        }
    }
    c
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
    // `sidebar-background` / `light-sidebar-background`: one colour for the rail, the pinned
    // sidebar and the open one, edged with a hairline where it meets the splits.
    let own = if s.light { s.sidebar.light_background } else { s.sidebar.background };
    if let Some(c) = own {
        bar = bar.fill(rgb_color(c)).border_sides([0.0, 1.0, 0.0, 0.0], look::ink(0.05));
    } else if open && !pinned {
        bar = bar.fill(look::surface(0)).border_sides([0.0, 1.0, 0.0, 0.0], look::ink(0.05));
    }
    if open && !pinned {
        bar = bar.shadow(0.0, 48.0, look::shade(0.6));
    }
    bar
}

/// The session picker: start a new session here, bring one back into this window, change a
/// session's emoji, or kill one.
fn picker(s: &State, sessions: &[crate::spooky::SessionInfo]) -> El {
    let body = match &s.picker_mode {
        PickerMode::Emoji { session, animals } => emoji_grid(sessions, session, animals, session_accent(s, session)),
        mode => session_list(s, sessions, mode),
    };
    let choosing = s.chooser.is_some();
    let mut panel = col().key("picker").w(if choosing { 420.0 } else { 356.0 }).max_h(600.0).p_all(8.0).gap(4.0).radius(16.0)
        .fill(look::surface(2).alpha(0.96)).backdrop_blur(24.0)
        .border(1.0, look::ink(0.08))
        .shadow(4.0, 12.0, look::shade(0.45)).shadow(24.0, 64.0, look::shade(0.55))
        .raise()
        .transition(Transition::offset(0.0, -6.0).combined(Transition::scale(0.97)).combined(Transition::opacity()));
    if choosing {
        // A new window asking which session to be: in the middle, and it stays until answered.
        panel = panel.child(col().px(10.0).pt(8.0).pb(4.0).gap(3.0)
            .child(text("Open a session").font(TextStyle::new(17.0, 700.0)).color(look::text()))
            .child(text("↑↓ to choose · Enter to open · Esc for a new one").font(SMALL).color(look::faint())));
        return el().key("picker-layer").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).raise().center()
            .child(panel.child(body));
    }
    // Clicking anywhere else closes it.
    el().key("picker-layer").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).raise()
        .child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).on_press(|s: &mut State| s.picker = None))
        .child(panel.absolute(s.sidebar.width + 8.0, 12.0).child(body))
}

fn session_list(s: &State, sessions: &[crate::spooky::SessionInfo], mode: &PickerMode) -> El {
    let current = s.session.as_ref().map(|x| x.name.as_str());
    let chosen = |i: usize, r: El| if s.chooser == Some(i) { r.fill(look::accent().alpha(0.12)).border(1.0, look::accent().alpha(0.35)) } else { r };
    let new = row().key("picker-new").h(52.0).w_full().px(10.0).gap(12.0).items_center().radius(12.0)
        .hover(look::ink(0.05)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.new_session())
        .child(el().size(34.0, 34.0).radius(10.0).center().fill(look::ink(0.06)).border(1.0, look::ink(0.08))
            .child(icon(&icons::PLUS, 16.0).color(look::text())))
        .child(col().gap(2.0)
            .child(text("New session").font(TITLE).color(look::text()))
            .child(text("Start fresh in this window").font(SMALL).color(look::faint())));
    let mut list = col().gap(2.0).w_full();
    let new = chosen(0, new);
    for (i, info) in sessions.iter().enumerate() {
        let here = current == Some(info.name.as_str());
        let (tint, label) = match (here, info.state.as_str()) {
            (true, _) => (session_accent(s, &info.name), "this window"),
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
        let (open, emoji, kill, rename) = (info.name.clone(), info.name.clone(), info.name.clone(), info.name.clone());
        let renaming = match mode {
            PickerMode::Rename { session, error } if *session == info.name => Some(error.clone()),
            _ => None,
        };
        // The session's tile glows in its own accent, as its badge does.
        let own = session_accent(s, &info.name);
        let tile = el().key(format!("picker-emoji-{}", info.name)).size(36.0, 36.0).radius(10.0).center()
            .fill(look::surface(3))
            .radial_gradient([0.5, 0.0], [40.0, 40.0], [(0.0, own.alpha(0.42)), (0.65, own.alpha(0.12)), (1.0, own.alpha(0.04))])
            .border(1.0, own.alpha(0.45))
            .shadow(4.0, 14.0, own.alpha(0.22))
            .hover(look::ink(0.08)).hover_alone().cursor(Cursor::Pointer)
            .help("Change emoji")
            .on_press(move |s: &mut State| s.choose_emoji(&emoji))
            .child(text(info.animal.clone()).font(TextStyle::new(19.0, 400.0)));
        let trash = el().key(format!("picker-kill-{}", info.name)).size(28.0, 28.0).radius(8.0).center()
            .hover(look::attention().alpha(0.16)).cursor(Cursor::Pointer)
            .help("Kill session")
            .group_opacity(0.0, 1.0)
            .on_press(move |s: &mut State| s.ask_kill(&kill))
            .child(icon(&icons::TRASH, 15.0).color(look::dim()));
        let pencil = el().key(format!("picker-rename-{}", info.name)).size(28.0, 28.0).radius(8.0).center()
            .hover(look::ink(0.08)).cursor(Cursor::Pointer)
            .help("Rename session")
            .group_opacity(0.0, 1.0)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.start_rename(&rename, tx))
            .child(icon(&icons::PENCIL, 15.0).color(look::dim()));
        if let Some(error) = renaming {
            list = list.child(rename_row(s, tile, error));
            continue;
        }
        let mut r = row().key(format!("picker-{}", info.name)).h(54.0).w_full().pl(9.0).pr(6.0).gap(12.0).items_center().radius(12.0)
            .hover(look::ink(0.05)).hover_group().cursor(Cursor::Pointer)
            .on_press(move |s: &mut State| s.open_session(&open))
            .child(tile)
            .child(col().gap(2.0).grow(1.0).min_w(0.0)
                .child(text(info.name.clone()).font(TITLE).color(look::text()).single_line().truncate())
                .child(text(detail).font(SMALL).color(look::faint()).single_line().truncate()))
            .child(el().h(20.0).px(8.0).radius(10.0).center().fill(tint.alpha(0.12))
                .child(text(label).font(CAPS).color(tint)))
            .child(pencil)
            .child(trash);
        if here {
            r = r.fill(look::ink(0.04));
        }
        list = list.child(chosen(i + 1, r));
    }
    col().w_full().gap(4.0)
        .child(row().h(28.0).px(10.0).items_center()
            .child(text("SESSIONS").font(CAPS).color(look::faint()))
            .child(el().grow(1.0))
            .child(text("spooky").font(CAPS).color(look::faint().alpha(0.7))))
        .child(new)
        .child(el().h(1.0).w_full().my(4.0).fill(look::ink(0.06)))
        .child(list)
}

/// A session's row while it's renamed: its name is a text field. Enter keeps the new name,
/// Escape (or the cancel button) the old.
fn rename_row(s: &State, tile: El, error: Option<String>) -> El {
    let field = rune::text_input(&s.rename_edit, TITLE)
        .key("rename-field")
        .single_line()
        .caret(look::accent(), look::accent().alpha(0.3))
        .color(look::text())
        .on_edit(|s: &mut State| &mut s.rename_edit)
        .on_submit(|s: &mut State, _: &mut Tx| s.finish_rename())
        .w_full();
    let (hint, hint_color) = match error {
        Some(e) => (e, look::attention()),
        None => ("Enter to rename · Esc to cancel".to_string(), look::faint()),
    };
    let button = |key: &str, glyph: &'static rune::Icon, help: &str| {
        el().key(key.to_string()).size(28.0, 28.0).radius(8.0).center()
            .hover(look::ink(0.08)).cursor(Cursor::Pointer).help(help.to_string())
            .child(icon(glyph, 15.0).color(look::dim()))
    };
    row().key("picker-renaming").h(54.0).w_full().pl(9.0).pr(6.0).gap(12.0).items_center().radius(12.0)
        .fill(look::accent().alpha(0.08)).border(1.0, look::accent().alpha(0.35))
        .child(tile)
        .child(col().gap(3.0).grow(1.0).min_w(0.0)
            .child(el().h(22.0).w_full().px(6.0).radius(6.0).fill(look::ink(0.06)).items_center().child(field))
            .child(text(hint).font(SMALL).color(hint_color).single_line().truncate()))
        .child(button("rename-ok", &icons::CHECK, "Rename").on_press(|s: &mut State| s.finish_rename()))
        .child(button("rename-cancel", &icons::CLOSE, "Cancel").on_press(|s: &mut State| s.cancel_rename()))
}

/// Killing ends the session's panes, so the row asks first.
fn confirm_kill(info: &crate::spooky::SessionInfo, panes: &str) -> El {
    let red = hex("#ff6b6b").unwrap();
    let name = info.name.clone();
    let button = |key: String, label: &str, fill: Color, color: Color| {
        el().key(key).h(28.0).px(12.0).radius(8.0).center().fill(fill).hover(look::ink(0.08)).cursor(Cursor::Pointer)
            .child(text(label.to_string()).font(TextStyle::new(12.0, 600.0)).color(color))
    };
    row().key(format!("picker-confirm-{}", info.name)).h(54.0).w_full().px(10.0).gap(10.0).items_center().radius(12.0)
        .fill(red.alpha(0.08)).border(1.0, red.alpha(0.25))
        .transition(Transition::opacity())
        .child(col().gap(2.0).grow(1.0).min_w(0.0)
            .child(text(format!("Kill {}?", info.name)).font(TITLE).color(look::text()).single_line().truncate())
            .child(text(format!("Ends its {panes}")).font(SMALL).color(look::dim()).single_line()))
        .child(button(format!("cancel-kill-{}", info.name), "Cancel", look::ink(0.06), look::text()).on_press(|s: &mut State| s.cancel_kill()))
        .child(button(format!("do-kill-{}", info.name), "Kill", red.alpha(0.9), Color::WHITE).on_press(move |s: &mut State| s.kill_session(&name)))
}

/// Every animal spooky offers; the session's current one is ringed.
/// A listed session's own accent (this window's follows a swatch click at once).
fn session_accent(s: &State, name: &str) -> Color {
    let c = match &s.session {
        Some(sess) if sess.name == name => s.accent,
        _ => s.picker_accents.get(name).copied().unwrap_or(s.default_accent),
    };
    hex(&crate::config::hex_of(c)).unwrap()
}

fn emoji_grid(sessions: &[crate::spooky::SessionInfo], session: &str, animals: &[(String, String)], accent: Color) -> El {
    let current = sessions.iter().find(|i| i.name == session).map(|i| i.animal.clone()).unwrap_or_default();
    let section = |title: &str, items: &[(String, String)]| {
        let mut grid = row().flex_wrap().gap(4.0).w_full();
        for (glyph, name) in items {
            let (session, name) = (session.to_string(), name.clone());
            let chosen = *glyph == current;
            let mut cell = el().key(format!("emoji-{name}")).size(42.0, 42.0).radius(11.0).center()
                .hover(look::ink(0.08)).press(look::ink(0.12)).cursor(Cursor::Pointer)
                .help(name.clone())
                .on_press(move |s: &mut State| s.set_emoji(&session, &name))
                .child(text(glyph.clone()).font(TextStyle::new(22.0, 400.0)));
            if chosen {
                cell = cell.fill(accent.alpha(0.16)).border(1.5, accent.alpha(0.75));
            }
            grid = grid.child(cell);
        }
        col().gap(6.0).w_full()
            .child(text(title.to_uppercase()).font(CAPS).color(look::faint()).px(4.0))
            .child(grid)
    };
    let back = el().key("emoji-back").size(28.0, 28.0).radius(8.0).center()
        .hover(look::ink(0.08)).cursor(Cursor::Pointer)
        .on_press(|s: &mut State| s.cancel_kill())
        .child(icon(&icons::BACK, 16.0).color(look::dim()));
    let c = col().w_full().gap(10.0).p_all(2.0)
        .child(row().h(32.0).gap(8.0).items_center()
            .child(back)
            .child(text(current.clone()).font(TextStyle::new(18.0, 400.0)))
            .child(text(format!("Emoji for {session}")).font(TITLE).color(look::text()).single_line().truncate()));
    if animals.is_empty() {
        return c.child(text("spooky didn't list any emoji").font(SMALL).color(look::faint()).px(4.0));
    }
    c.child(section("Animals", animals))
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
        .fill(look::surface(3).alpha(0.92))
        .backdrop_blur(18.0)
        .border(1.0, if attention { look::attention().alpha(0.5) } else { look::ink(0.09) })
        .shadow(2.0, 6.0, look::shade(0.4))
        .shadow(14.0, 40.0, look::shade(0.5))
        .offset(m.offset.0, m.offset.1)
        .hover(look::ink(0.04))
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
    let a = s.accent;
    look::set(s.light, hex(&format!("#{:02x}{:02x}{:02x}", a.0, a.1, a.2)).unwrap());
    let main = match s.tab() {
        Some(tab) if tab.zoomed => pane(s, tab, tab.focused),
        Some(tab) => node(s, tab, &tab.root, vec![]),
        None => el(),
    };
    // Each tab is its own layer, keyed by the tab: switching tabs slides the new one in from
    // below (a tab further down) or above, while the old one leaves the other way.
    let dir = s.slide;
    // Offset and fade only: a scale would resize every split on every frame of the slide.
    let slide = |d: f32| Transition::offset(0.0, 90.0 * d).combined(Transition::opacity());
    let layer = el().key(format!("tab-layer-{}", s.tab().map_or(0, |t| t.id)))
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .transition(slide(dir));
    let layer = layer.child(main);
    let mut layer = layer;
    if let Some(tab) = s.tab().filter(|t| t.floating.iter().any(|f| !f.pinned) && !t.zoomed) {
        layer = layer.child(floats(s, tab, false));
    }
    let mut stage = el().key("stage").grow(1.0).h_full().min_w(0.0)
        .clip_rounded(look::RADIUS)
        // Floating splits are placed as fractions of it.
        .on_layout(|s: &mut State, r: [f32; 4]| {
            if s.stage != [r[2], r[3]] {
                s.stage = [r[2], r[3]];
            }
        })
        .child_exit(slide(-dir))
        .child(layer);
    // Pinned floating splits sit above the tabs, still while they slide.
    if let Some(tab) = s.tab().filter(|t| t.floating.iter().any(|f| f.pinned) && !t.zoomed) {
        stage = stage.child(floats(s, tab, true));
    }
    if let Some(tab) = s.tab().filter(|t| !t.minimized.is_empty()) {
        let mut dock = row().key("dock").gap(10.0).justify_center();
        for m in &tab.minimized {
            dock = dock.child(pill(s, m));
        }
        stage = stage.child(el().pin(Some(0.0), None, Some(0.0), Some(18.0)).child(row().w_full().justify_center().child(dock)));
    }
    let mut root = el().w_full().h_full().fill(look::window()).motion(motion::GLIDE)
        .on_window_focus(|s: &mut State, focused, _: &mut Tx| s.window_focused = focused)
        .child(row().w_full().h_full().pl(if s.sidebar.pinned { s.sidebar.width } else { s.sidebar.rail }).py(look::GAP).pr(look::GAP).child(stage))
        .child(sidebar(s));
    if let Some(sessions) = &s.picker {
        root = root.child(picker(s, sessions));
    }
    if s.palette.is_some() {
        root = root.child(palette(s));
    }
    root
}

/// The command palette (⌥P): every action, filtered as you type. ↑↓ choose, Enter runs, Esc
/// closes; clicking a row runs it, clicking outside closes.
fn palette(s: &State) -> El {
    let Some(p) = &s.palette else { return el() };
    let items = s.palette_items();
    let chosen = p.chosen.min(items.len().saturating_sub(1));
    let field = rune::text_input(&p.edit, TextStyle::new(15.0, 500.0))
        .key("palette-field")
        .single_line()
.placeholder(p.pick.as_ref().map_or("Type a command…", |w| w.prompt()), look::faint())
        .caret(look::accent(), look::accent().alpha(0.3))
        .color(look::text())
        .on_edit(|s: &mut State| &mut s.palette.as_mut().expect("palette open").edit)
        .on_input_key(|s: &mut State, e, tx: &mut Tx| s.palette_key(e, tx))
        .w_full();
    let search = row().h(46.0).px(14.0).gap(10.0).items_center()
        .child(icon(&icons::SEARCH, 17.0).color(look::dim()))
        .child(el().grow(1.0).min_w(0.0).child(field));
    let mut list = col().key("palette-list").w_full().gap(1.0).px(6.0).pb(6.0).max_h(420.0).scroll(rune::ScrollSpec { stick_bottom: false });
    if items.is_empty() {
        list = list.child(text(s.palette_empty()).font(BODY).color(look::faint()).px(10.0).py(12.0));
    }
    for (i, (action, title, keys)) in items.into_iter().enumerate() {
        let on = i == chosen;
        let mut r = row().key(format!("palette-{}", action.id())).h(36.0).w_full().px(10.0).gap(10.0).items_center().radius(9.0)
            .hover(look::ink(0.05)).cursor(Cursor::Pointer)
            .on_press_tx(move |s: &mut State, tx: &mut Tx| s.run_chosen_at(i, tx))
            .child(text(title).font(TextStyle::new(13.0, if on { 600.0 } else { 480.0 })).color(if on { look::text() } else { look::dim() })
                .single_line().truncate().grow(1.0).shrink());
        if !keys.is_empty() {
            r = r.child(el().h(22.0).px(7.0).radius(6.0).center().fill(look::ink(0.06)).border(1.0, look::ink(0.07))
                .child(text(keys).font(TextStyle::new(11.5, 600.0)).color(look::dim())));
        }
        if on {
            r = r.fill(look::accent().alpha(0.14)).border(1.0, look::accent().alpha(0.35));
        }
        list = list.child(r);
    }
    let panel = col().key("palette").w(560.0).radius(16.0)
        .fill(look::surface(2).alpha(0.97)).backdrop_blur(24.0)
        .border(1.0, look::ink(0.08))
        .shadow(4.0, 12.0, look::shade(0.45)).shadow(24.0, 64.0, look::shade(0.55))
        .transition(Transition::offset(0.0, -8.0).combined(Transition::scale(0.98)).combined(Transition::opacity()))
        .on_press(|_: &mut State| {})
        .child(search)
        .child(el().h(1.0).w_full().fill(look::ink(0.06)).mb(6.0))
        .child(list)
        .child(row().h(30.0).px(16.0).gap(14.0).items_center().border_sides([1.0, 0.0, 0.0, 0.0], look::ink(0.05))
            .child(text(if p.pick.is_some() { "↑↓ try · ↩ keep · esc put back" } else { "↑↓ choose · ↩ run · esc close" }).font(SMALL).color(look::faint()))
            .child(el().grow(1.0))
            .child(text(if p.pick.is_some() { "saved in the config" } else { "keybind = CHORD=ACTION in the config" }).font(SMALL).color(look::faint())));
    el().key("palette-layer").pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).raise()
        .child(el().pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0)).fill(look::shade(0.25)).on_press(|s: &mut State| s.close_palette()))
        .child(row().w_full().justify_center().items_start().pt(90.0).child(panel))
}

pub fn spec() -> AppSpec<State> {
    AppSpec::<State> {
        setup: Some(|_, waker| {
            let _ = WAKER.set(waker);
        }),
        poll: Some(|s, t, tx: &mut Tx| {
            let changed = s.poll(t);
            // The clock and battery change on their own: look again every few seconds. A pane
            // scrolling smoothly wants every frame.
            let next = if s.scrolling() { 1.0 / 120.0 } else if s.snap_pending() { 0.1 } else { 5.0 };
            tx.wake_after(std::time::Duration::from_secs_f64(next));
            changed
        }),
        on_key: Some(|s, e, tx: &mut Tx| s.key(e, tx)),
        scenes: &[
            ("demo", demo),
            ("sidebar", |s| s.sidebar_open = true),
            ("pinned", |s| s.sidebar.pinned = true),
            ("scrollback", |s| {
                // A screenful and more of output, to scroll back through.
                if let Some(p) = s.focused_id().and_then(|id| s.panes.get(&id)) {
                    p.send(b"seq 1 300\n");
                }
            }),
            ("stacked", |s| {
                // Two splits, one above the other, both full of numbers.
                s.split(Dir::Down);
                for id in s.tabs[0].root.leaves() {
                    if let Some(p) = s.panes.get(&id) {
                        p.send(b"seq 1 300\n");
                    }
                }
            }),
            ("floating", |s| {
                // Two splits, the second floating above the first.
                s.split(Dir::Right);
                let id = s.tabs[0].focused;
                s.toggle_float(id);
            }),
            ("follow", |s| {
                // Output that goes on after a while, to check the bottom still follows it.
                if let Some(p) = s.focused_id().and_then(|id| s.panes.get(&id)) {
                    p.send(b"seq 1 300; sleep 2; seq 301 320\n");
                }
            }),
            ("animal", |s| {
                // A session with an animated animal (by name: no daemon in headless runs).
                s.session = Some(crate::state::Session { name: "headless-panda".into(), animal: "🐼".into() });
                s.sidebar.pinned = true;
                s.split(Dir::Right);
            }),
            ("readme", |s| showcase(s, false)),
            ("readme-rail", |s| {
                showcase(s, false);
                s.sidebar.pinned = false;
            }),
            ("readme-float", |s| showcase(s, true)),
            ("readme-palette", |s| {
                showcase(s, false);
                s.palette = Some(crate::state::Palette { edit: rune::TextEdit::new("split"), chosen: 0, pick: None });
            }),
            ("picker", |s| s.toggle_picker()),
            ("palette", |s| {
                s.palette = Some(crate::state::Palette { edit: rune::TextEdit::new(""), chosen: 2, pick: None });
            }),
            ("palette-weights", |s| {
                // The weight list for a variable font's regular text, 450 in use.
                let pick = crate::state::Pick { setting: crate::keys::Setting::Weight, family: "Geist Mono".into(), range: Some((100.0, 400.0, 900.0)), current: 450.0, previewed: false };
                s.palette = Some(crate::state::Palette { edit: rune::TextEdit::new(""), chosen: 4, pick: Some(pick) });
            }),
            ("palette-widths", |s| {
                // The width list for Martian Mono, SemiCondensed chosen.
                let pick = crate::state::Pick { setting: crate::keys::Setting::Width, family: "Martian Mono".into(), range: Some((75.0, 112.5, 112.5)), current: 100.0, previewed: false };
                s.palette = Some(crate::state::Palette { edit: rune::TextEdit::new(""), chosen: 1, pick: Some(pick) });
            }),
            ("palette-font", |s| {
                // The palette's font and text settings.
                s.palette = Some(crate::state::Palette { edit: rune::TextEdit::new("font"), chosen: 0, pick: None });
            }),
            ("palette-sizes", |s| {
                // The font size list, 20 pt in use.
                let pick = crate::state::Pick { setting: crate::keys::Setting::FontSize, family: "Martian Mono".into(), range: Some((4.0, 13.0, 96.0)), current: 20.0, previewed: false };
                s.palette = Some(crate::state::Palette { edit: rune::TextEdit::new(""), chosen: 12, pick: Some(pick) });
            }),
            ("palette-filtered", |s| {
                s.palette = Some(crate::state::Palette { edit: rune::TextEdit::new("flo"), chosen: 0, pick: None });
            }),
            ("pinned-float", |s| {
                // A split floating and pinned, carried along to a second tab.
                s.split(Dir::Right);
                let id = s.tabs[0].focused;
                s.toggle_pin(id);
                s.new_tab(None);
                s.focus(id);
            }),
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

/// The README's screenshots (`--scene readme`, `readme-rail`, `readme-float`, `readme-palette`): runeterm's
/// own code in helix, its git log and its tests, in a session with its animal, a second tab,
/// and a fourth split minimized to a pill (or, with `float`, floating and pinned).
/// The programs run in `RUNETERM_DEMO_REPO`, or the current directory.
fn showcase(s: &mut State, float: bool) {
    s.session = Some(crate::state::Session { name: "quiet-river".into(), animal: "🦉".into() });
    s.sidebar.pinned = true;
    let repo = std::env::var("RUNETERM_DEMO_REPO")
        .ok()
        .or_else(|| std::env::current_dir().ok().map(|d| d.display().to_string()))
        .unwrap_or_default();
    let editor = s.tabs[0].focused;
    s.split(Dir::Right);
    let git = s.tabs[0].focused;
    s.split(Dir::Down);
    let tests = s.tabs[0].focused;
    s.split(Dir::Down);
    let log = s.tabs[0].focused;
    s.tabs[0].root.resize(editor, crate::split::Side::Right, -0.04);
    s.new_tab(None);
    s.select_tab(0);
    let run = |s: &State, id: u32, cmd: &str| {
        if let Some(p) = s.panes.get(&id) {
            p.send(format!("cd '{repo}' && clear && {cmd}\n").as_bytes());
        }
    };
    run(s, editor, "hx crates/runeterm/src/keys.rs:96");
    run(s, git, "git --no-pager log --graph --format='%C(yellow)%h%C(reset) %<(52,trunc)%s' --color -n 40");
    run(s, tests, "cargo test --release -p runeterm 2>&1 | grep -E '^test |test result' | sed -E 's/^test ([a-z_]+::)+/test /' | tail -n 40");
    run(s, log, "git --no-pager log --color --format='%C(yellow)%h%C(reset) %s%n' --stat=60 -n 1 HEAD~1");
    std::thread::sleep(std::time::Duration::from_millis(6000));
    if float {
        s.toggle_pin(log);
        if let Some(f) = s.tabs[0].floating.iter_mut().find(|f| f.id == log) {
            f.rect = [0.4, 0.24, 0.5, 0.34];
        }
    } else {
        s.minimize(log);
        s.focus(editor);
    }
    for p in s.panes.values_mut() {
        p.refresh_meta(None);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use rune::edit::{Key, KeyEvent};
    use rune::{Env, Mods, Runtime};

    fn press(rt: &mut Runtime<State>, key: Key, mods: Mods) {
        rt.key(KeyEvent { key, mods });
        rt.frame(0.0);
    }

    fn alt() -> Mods {
        Mods { alt: true, ..Mods::default() }
    }

    /// ⌥⇧2 through Rune's key routing moves the focused split to tab 2, though the key types
    /// "@"; ⌥⇧0 moves it to a new tab.
    #[test]
    fn alt_shift_digits_move_the_split_between_tabs() {
        let mut s = State::new(&Config::default(), false, None);
        s.split(Dir::Right);
        s.new_tab(None);
        s.select_tab(0);
        let moving = s.tabs[0].focused;
        let mut rt = Runtime::new(s, view, (1280.0, 780.0), Env::new(true, 1.0));
        rt.on_key = Some(|s, e, tx| s.key(e, tx));
        rt.frame(0.0);
        let alt_shift = Mods { alt: true, shift: true, ..Mods::default() };
        press(&mut rt, Key::Text("@".into()), alt_shift);
        assert!(rt.state.tabs[1].has(moving), "in tab 2");
        assert_eq!(rt.state.active, 1, "and followed there");
        press(&mut rt, Key::Text(")".into()), alt_shift);
        assert_eq!(rt.state.tabs.len(), 3);
        assert!(rt.state.tabs[2].has(moving), "in a new tab");
    }

    /// ctrl+= and ctrl+- change the size for now, ctrl+0 puts it back.
    #[test]
    fn ctrl_equals_and_minus_size_the_text_for_now() {
        let mut rt = Runtime::new(State::new(&Config::default(), false, None), view, (1280.0, 780.0), Env::new(true, 1.0));
        rt.on_key = Some(|s, e, tx| s.key(e, tx));
        rt.frame(0.0);
        let ctrl = Mods { ctrl: true, ..Mods::default() };
        press(&mut rt, Key::Text("=".into()), ctrl);
        press(&mut rt, Key::Text("=".into()), ctrl);
        assert_eq!(rt.state.font_delta, 2.0);
        press(&mut rt, Key::Text("-".into()), ctrl);
        assert_eq!(rt.state.font_delta, 1.0);
        press(&mut rt, Key::Text("0".into()), ctrl);
        assert_eq!(rt.state.font_delta, 0.0);
    }

    /// The palette end to end through Rune's key routing: ⌥P opens it with its field focused,
    /// typing filters, ↓ chooses, Enter runs the choice and closes it.
    #[test]
    fn the_palette_opens_filters_and_runs() {
        let mut s = State::new(&Config::default(), false, None);
        s.split(Dir::Right);
        let mut rt = Runtime::new(s, view, (1280.0, 780.0), Env::new(true, 1.0));
        rt.on_key = Some(|s, e, tx| s.key(e, tx));
        rt.frame(0.0);
        press(&mut rt, Key::Text("p".into()), alt());
        assert!(rt.state.palette.is_some(), "⌥P opens it");
        for c in "min".chars() {
            press(&mut rt, Key::Text(c.to_string()), Mods::default());
        }
        assert_eq!(rt.state.palette.as_ref().unwrap().edit.text, "min", "the field takes the typing");
        let rows = rt.state.palette_items();
        assert_eq!(rows[0].0, Action::Minimize);
        press(&mut rt, Key::Down, Mods::default());
        assert_eq!(rt.state.palette.as_ref().unwrap().chosen, 1);
        press(&mut rt, Key::Up, Mods::default());
        press(&mut rt, Key::Enter, Mods::default());
        assert!(rt.state.palette.is_none(), "Enter closes it");
        assert_eq!(rt.state.tabs[0].minimized.len(), 1, "and minimized the focused split");
        // Escape and ⌥P both close it again.
        press(&mut rt, Key::Text("p".into()), alt());
        press(&mut rt, Key::Escape, Mods::default());
        assert!(rt.state.palette.is_none());
        press(&mut rt, Key::Text("p".into()), alt());
        press(&mut rt, Key::Text("p".into()), alt());
        assert!(rt.state.palette.is_none());
    }

    /// The default keys still reach their actions, and unbound ones reach the terminal.
    #[test]
    fn keybinds_drive_floating_and_pinning() {
        let mut s = State::new(&Config::default(), false, None);
        s.split(Dir::Right);
        let id = s.tabs[0].focused;
        let mut rt = Runtime::new(s, view, (1280.0, 780.0), Env::new(true, 1.0));
        rt.on_key = Some(|s, e, tx| s.key(e, tx));
        rt.frame(0.0);
        press(&mut rt, Key::Text("f".into()), alt());
        assert!(rt.state.tabs[0].is_floating(id), "⌥F floats");
        press(&mut rt, Key::Text("F".into()), Mods { alt: true, shift: true, ..Mods::default() });
        assert!(rt.state.is_pinned(id), "⌥⇧F pins");
        press(&mut rt, Key::Text("m".into()), alt());
        assert!(rt.state.tabs[0].floating.is_empty() && rt.state.tabs[0].minimized.len() == 1, "⌥M minimizes a float");
        press(&mut rt, Key::Text("M".into()), Mods { alt: true, shift: true, ..Mods::default() });
        assert!(rt.state.is_pinned(id), "⌥⇧M brings it back, pinned");
    }
}
