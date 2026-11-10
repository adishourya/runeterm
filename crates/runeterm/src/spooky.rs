//! The spooky daemon's protocol (`spooky/src/protocol.zig`), from runeterm's side. The daemon
//! owns every pty; runeterm connects one client per pane, plus one controller connection per
//! window that reports its tabs and splits so the session's layout survives restarts.
//!
//! Frames are a one-byte tag, a little-endian u32 length, then the payload.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub mod tag {
    pub const NEW: u8 = 1;
    pub const ATTACH: u8 = 2;
    pub const INPUT: u8 = 3;
    pub const RESIZE: u8 = 4;
    pub const LIST: u8 = 5;
    pub const CONTROLLER: u8 = 8;
    pub const LAYOUT: u8 = 9;
    pub const CLOSED: u8 = 10;
    pub const OPEN: u8 = 11;
    pub const RENAME: u8 = 12;
    pub const REMOVE: u8 = 13;
    pub const ANIMAL: u8 = 15;
    pub const ATTACHED: u8 = 64;
    pub const OUTPUT: u8 = 65;
    pub const EXIT: u8 = 66;
    pub const ERR: u8 = 67;
    pub const TEXT: u8 = 68;
    pub const DETACHED: u8 = 69;
    pub const OPEN_WINDOW: u8 = 70;
    pub const PRESENT_WINDOW: u8 = 71;
    pub const SURFACES: u8 = 72;
}

const MAX_PAYLOAD: usize = 1024 * 1024;

/// `$SPOOKY_SOCKET`, else `$XDG_RUNTIME_DIR/spooky/default.sock`, else `/tmp/spooky-$UID/…`.
pub fn socket_path() -> PathBuf {
    if let Ok(p) = std::env::var("SPOOKY_SOCKET") {
        return p.into();
    }
    let dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(d) => PathBuf::from(d).join("spooky"),
        Err(_) => PathBuf::from(format!("/tmp/spooky-{}", unsafe { libc_getuid() })),
    };
    dir.join("default.sock")
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// The `spooky` binary: `$RUNETERM_SPOOKY`, else `spooky` on the PATH, else
/// `~/.local/bin/spooky` (an app launcher's PATH often lacks it).
pub fn binary() -> PathBuf {
    if let Ok(p) = std::env::var("RUNETERM_SPOOKY") {
        return p.into();
    }
    let on_path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join("spooky").is_file()))
        .unwrap_or(false);
    if on_path {
        return "spooky".into();
    }
    let local = PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/bin/spooky");
    if local.is_file() { local } else { "spooky".into() }
}

/// Connects to the daemon, starting it if it isn't running (any spooky command does).
pub fn connect() -> io::Result<UnixStream> {
    let path = socket_path();
    match UnixStream::connect(&path) {
        Ok(s) => Ok(s),
        Err(_) => {
            std::process::Command::new(binary()).arg("ls").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status()?;
            UnixStream::connect(&path)
        }
    }
}

pub fn write_frame(w: &mut impl Write, tag: u8, payload: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(tag);
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)
}

/// Reassembles frames from a byte stream.
#[derive(Default)]
pub struct Frames {
    buf: Vec<u8>,
}

impl Frames {
    /// Reads until at least one frame is complete; None at end of stream.
    pub fn next(&mut self, r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
        loop {
            if self.buf.len() >= 5 {
                let len = u32::from_le_bytes(self.buf[1..5].try_into().unwrap()) as usize;
                if len > MAX_PAYLOAD {
                    return Err(io::Error::other("spooky frame too large"));
                }
                if self.buf.len() >= 5 + len {
                    let tag = self.buf[0];
                    let payload = self.buf[5..5 + len].to_vec();
                    self.buf.drain(..5 + len);
                    return Ok(Some((tag, payload)));
                }
            }
            let mut chunk = [0u8; 64 * 1024];
            let n = r.read(&mut chunk)?;
            if n == 0 {
                return Ok(None);
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}

/// `protocol.Size`.
pub fn size(cols: u16, rows: u16, xpixel: u16, ypixel: u16) -> [u8; 8] {
    let mut b = [0u8; 8];
    b[0..2].copy_from_slice(&cols.to_le_bytes());
    b[2..4].copy_from_slice(&rows.to_le_bytes());
    b[4..6].copy_from_slice(&xpixel.to_le_bytes());
    b[6..8].copy_from_slice(&ypixel.to_le_bytes());
    b
}

/// `Tag.new`: `New`, the cwd, then the session name (empty: the window's session).
pub fn new_payload(cols: u16, rows: u16, cwd: &str, session: &str, surface: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(24 + cwd.len() + session.len());
    b.extend_from_slice(&size(cols, rows, 0, 0));
    b.extend_from_slice(&(cwd.len() as u32).to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes()); // flags
    b.extend_from_slice(&surface.to_le_bytes());
    b.extend_from_slice(cwd.as_bytes());
    b.extend_from_slice(session.as_bytes());
    b
}

/// `Tag.attach` to pane `pane`.
pub fn attach_payload(cols: u16, rows: u16, pane: u32, surface: u64) -> Vec<u8> {
    let mut b = Vec::with_capacity(24);
    b.extend_from_slice(&size(cols, rows, 0, 0));
    b.extend_from_slice(&pane.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(&surface.to_le_bytes());
    b
}

/// `Tag.open`: `Prepare{kind, pane}` then a session name.
pub fn open_payload(kind: u32, name: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(8 + name.len());
    b.extend_from_slice(&kind.to_le_bytes());
    b.extend_from_slice(&0u32.to_le_bytes());
    b.extend_from_slice(name.as_bytes());
    b
}
pub const OPEN_NEW: u32 = 0;
pub const OPEN_SESSION: u32 = 2;

/// A one-shot request (the daemon answers with `text` or `err` and hangs up).
pub fn request(tag: u8, payload: &[u8]) -> io::Result<String> {
    let mut s = connect()?;
    write_frame(&mut s, tag, payload)?;
    let mut frames = Frames::default();
    while let Some((t, p)) = frames.next(&mut s)? {
        match t {
            tag::TEXT => return Ok(String::from_utf8_lossy(&p).into_owned()),
            tag::ERR => return Err(io::Error::other(String::from_utf8_lossy(&p).into_owned())),
            _ => {}
        }
    }
    Err(io::Error::other("spooky hung up"))
}

/// One line of `spooky ls`.
#[derive(Clone, Debug, PartialEq)]
pub struct SessionInfo {
    pub animal: String,
    pub name: String,
    /// "open", "detached" or "saved".
    pub state: String,
    pub tabs: Option<u32>,
    pub panes: u32,
    pub running: String,
}

pub fn parse_sessions(text: &str) -> Vec<SessionInfo> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut w = line.split_whitespace();
            let animal = w.next()?.to_string();
            let name = w.next()?.to_string();
            let state = w.next()?.to_string();
            let tabs = w.next()?.parse().ok();
            let panes = w.next()?.parse().ok()?;
            let running = w.collect::<Vec<_>>().join(" ");
            Some(SessionInfo { animal, name, state, tabs, panes, running })
        })
        .collect()
}

pub fn sessions() -> Vec<SessionInfo> {
    request(tag::LIST, b"").map(|t| parse_sessions(&t)).unwrap_or_default()
}

/// The animal emojis a session can have, as (glyph, name), in spooky's order (not the sports).
pub fn animals() -> Vec<(String, String)> {
    let mono = mono_coverage();
    ANIMALS
        .iter()
        .filter(|(glyph, _)| in_colour(glyph, mono))
        .map(|(glyph, name)| (glyph.to_string(), name.to_string()))
        .collect()
}

/// spooky's animals, as (glyph, name), in its order (`spooky/src/animals.zig`, before the
/// sports). spooky has no command to list them; it takes the name to set one.
const ANIMALS: &[(&str, &str)] = &[
    ("🐾", "paw"),
    ("🐶", "dog"),
    ("🐱", "cat"),
    ("🐰", "rabbit"),
    ("🦉", "owl"),
    ("🐼", "panda"),
    ("🐧", "penguin"),
    ("🐢", "turtle"),
    ("🦆", "duck"),
    ("🐘", "elephant"),
    ("🐨", "koala"),
    ("🐦", "bird"),
    ("🪼", "jellyfish"),
    ("🐝", "bee"),
    ("🐬", "dolphin"),
    ("🐌", "snail"),
    ("🦊", "fox"),
    ("🐯", "tiger"),
    ("🦁", "lion"),
    ("🐸", "frog"),
    ("🐙", "octopus"),
    ("🦄", "unicorn"),
    ("🐳", "whale"),
    ("🦔", "hedgehog"),
    ("🦥", "sloth"),
    ("🦦", "otter"),
    ("🦜", "parrot"),
    ("🦀", "crab"),
    ("🦋", "butterfly"),
    ("🦕", "dino"),
    ("🐵", "monkey"),
    ("🦈", "shark"),
];

/// What fonts without colour glyphs cover, as codepoint ranges, from fontconfig, looked up
/// once. An emoji one of them covers may be drawn from it, in black and white (DejaVu Sans
/// has a cat and a monkey face, for one).
fn mono_coverage() -> &'static [(u32, u32)] {
    static RANGES: std::sync::OnceLock<Vec<(u32, u32)>> = std::sync::OnceLock::new();
    RANGES.get_or_init(|| {
        let Ok(out) = std::process::Command::new("fc-list").args(["--format", "%{color}|%{charset}\n"]).output() else {
            return vec![];
        };
        parse_mono(&String::from_utf8_lossy(&out.stdout))
    })
}

fn parse_mono(text: &str) -> Vec<(u32, u32)> {
    text.lines()
        .filter_map(|l| l.strip_prefix("False|"))
        .flat_map(|cs| cs.split_whitespace())
        .filter_map(|r| {
            let (a, b) = r.split_once('-').unwrap_or((r, r));
            Some((u32::from_str_radix(a, 16).ok()?, u32::from_str_radix(b, 16).ok()?))
        })
        .collect()
}

/// Whether `glyph` can only come from a colour font: no colourless font covers it (the
/// variation selector and joiner aside).
fn in_colour(glyph: &str, mono: &[(u32, u32)]) -> bool {
    glyph
        .chars()
        .map(|c| c as u32)
        .filter(|&c| c != 0xfe0f && c != 0x200d)
        .all(|c| !mono.iter().any(|&(a, b)| a <= c && c <= b))
}

/// Sets session `session`'s emoji by name (`Tag.animal`: `Rename{pane}`, the session, a
/// zero byte, the animal).
pub fn set_animal(session: &str, animal: &str) -> io::Result<()> {
    let mut b = 0u32.to_le_bytes().to_vec();
    b.extend_from_slice(session.as_bytes());
    b.push(0);
    b.extend_from_slice(animal.as_bytes());
    request(tag::ANIMAL, &b).map(|_| ())
}

/// Renames session `old` to `new` (`spooky rename`; spooky refuses a name that's taken).
pub fn rename(old: &str, new: &str) -> io::Result<()> {
    let mut b = 0u32.to_le_bytes().to_vec();
    b.extend_from_slice(old.as_bytes());
    b.push(0);
    b.extend_from_slice(new.as_bytes());
    request(tag::RENAME, &b).map(|_| ())
}

/// Ends every pane of session `name` and forgets it (`spooky rm`).
pub fn remove(name: &str) -> io::Result<()> {
    request(tag::REMOVE, name.as_bytes()).map(|_| ())
}

/// `spooky ls -p`: each pane's foreground command, by pane id.
pub fn parse_panes(text: &str) -> HashMap<u32, String> {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let mut w = line.split_whitespace();
            let _session = w.next()?;
            let id: u32 = w.next()?.parse().ok()?;
            let _state = w.next()?;
            let _size = w.next()?;
            Some((id, w.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

pub fn pane_commands() -> HashMap<u32, String> {
    request(tag::LIST, b"panes").map(|t| parse_panes(&t)).unwrap_or_default()
}

/// The program a command line runs: `/usr/bin/nvim -p x` → `nvim`, `-zsh` → `zsh`.
pub fn program_of(command: &str) -> String {
    let first = command.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first).trim_start_matches('-').to_string()
}

/// A layout tree node (`layout.zig`): a leaf is a surface, a pane or an argv; a split has
/// `split` ("h" side by side, "v" stacked), `ratio` (the left/top share), `left` and `right`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Node {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split: Option<String>,
    #[serde(default = "half")]
    pub ratio: f32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub left: Option<Box<Node>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub right: Option<Box<Node>>,
}

fn half() -> f32 {
    0.5
}

#[derive(Serialize)]
pub struct Window {
    pub id: u64,
    pub active: u32,
    pub tabs: Vec<Node>,
}

#[derive(Serialize)]
pub struct Report {
    pub windows: Vec<Window>,
}

#[derive(Deserialize, Debug)]
pub struct Open {
    pub session: String,
    #[serde(default)]
    pub active: u32,
    pub tabs: Vec<Node>,
}

#[derive(Deserialize, Debug, Default)]
pub struct Surfaces {
    #[serde(default)]
    pub windows: Vec<SurfaceEntry>,
}

#[derive(Deserialize, Debug)]
pub struct SurfaceEntry {
    pub id: u64,
    pub session: String,
    #[serde(default)]
    pub animal: Option<String>,
}

/// What a leaf argv from `open_window` asks for.
#[derive(Debug, PartialEq)]
pub enum Leaf {
    Attach(u32),
    New(String),
}

pub fn leaf_of(argv: &[String]) -> Option<Leaf> {
    let i = argv.iter().position(|a| a == "attach" || a == "new")?;
    match argv[i].as_str() {
        "attach" => {
            let p = argv.iter().position(|a| a == "-p")?;
            argv.get(p + 1)?.parse().ok().map(Leaf::Attach)
        }
        _ => {
            let name = argv.iter().position(|a| a == "-s").and_then(|s| argv.get(s + 1)).cloned().unwrap_or_default();
            Some(Leaf::New(name))
        }
    }
}

/// Unique ids for this process's windows and surfaces: the pid in the high bits keeps them
/// apart from other runeterm processes and ghostty's.
pub fn fresh_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    ((std::process::id() as u64) << 32) | NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What the daemon tells a controller.
#[derive(Debug)]
pub enum Event {
    OpenWindow(Open),
    PresentWindow(u64),
    Surfaces(Surfaces),
    Gone,
}

/// A window's controller connection: reports layouts and closed splits, asks for sessions,
/// and receives windows to open.
pub struct Controller {
    out: Mutex<UnixStream>,
    pub events: Arc<Mutex<Vec<Event>>>,
}

impl Controller {
    pub fn connect(wake: impl Fn() + Send + 'static) -> io::Result<Controller> {
        let mut s = connect()?;
        write_frame(&mut s, tag::CONTROLLER, b"")?;
        let mut r = s.try_clone()?;
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        std::thread::Builder::new().name("spooky-controller".into()).spawn(move || {
            let mut frames = Frames::default();
            loop {
                let e = match frames.next(&mut r) {
                    Ok(Some((tag::OPEN_WINDOW, p))) => match serde_json::from_slice(&p) {
                        Ok(open) => Event::OpenWindow(open),
                        Err(e) => {
                            eprintln!("runeterm: bad open_window from spooky: {e}");
                            continue;
                        }
                    },
                    Ok(Some((tag::PRESENT_WINDOW, p))) if p.len() >= 8 => Event::PresentWindow(u64::from_le_bytes(p[..8].try_into().unwrap())),
                    Ok(Some((tag::SURFACES, p))) => Event::Surfaces(serde_json::from_slice(&p).unwrap_or_default()),
                    Ok(Some((tag::ERR, p))) => {
                        eprintln!("runeterm: spooky: {}", String::from_utf8_lossy(&p));
                        continue;
                    }
                    Ok(Some(_)) => continue,
                    Ok(None) | Err(_) => Event::Gone,
                };
                let gone = matches!(e, Event::Gone);
                ev.lock().unwrap().push(e);
                wake();
                if gone {
                    break;
                }
            }
        })?;
        Ok(Controller { out: Mutex::new(s), events })
    }

    fn send(&self, tag: u8, payload: &[u8]) {
        let _ = write_frame(&mut *self.out.lock().unwrap(), tag, payload);
    }

    pub fn report(&self, r: &Report) {
        if let Ok(json) = serde_json::to_vec(r) {
            self.send(tag::LAYOUT, &json);
        }
    }

    /// The user closed these surfaces: their panes end.
    pub fn closed(&self, surfaces: &[u64]) {
        let b: Vec<u8> = surfaces.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.send(tag::CLOSED, &b);
    }

    /// Asks for session `name`'s window; it comes back as `Event::OpenWindow` (or, if another
    /// window shows it, that window is raised).
    pub fn open_session(&self, name: &str) {
        self.send(tag::OPEN, &open_payload(OPEN_SESSION, name));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animals_a_colourless_font_covers_are_left_out() {
        let mono = parse_mono("False|20-7e 1f431 1f435-1f436\nTrue|20 1f431-1f43e\n");
        assert_eq!(mono, vec![(0x20, 0x7e), (0x1f431, 0x1f431), (0x1f435, 0x1f436)]);
        assert!(!in_colour("🐱", &mono));
        assert!(!in_colour("🐶", &mono));
        assert!(in_colour("🦊", &mono));
        assert!(in_colour("🐼\u{fe0f}", &mono));
    }

    #[test]
    fn frames_round_trip_in_pieces() {
        let mut wire = Vec::new();
        write_frame(&mut wire, tag::OUTPUT, b"hello").unwrap();
        write_frame(&mut wire, tag::EXIT, &0i32.to_le_bytes()).unwrap();
        // Delivered a byte at a time.
        struct Slow(Vec<u8>, usize);
        impl Read for Slow {
            fn read(&mut self, b: &mut [u8]) -> io::Result<usize> {
                if self.1 >= self.0.len() {
                    return Ok(0);
                }
                b[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let mut r = Slow(wire, 0);
        let mut f = Frames::default();
        assert_eq!(f.next(&mut r).unwrap(), Some((tag::OUTPUT, b"hello".to_vec())));
        assert_eq!(f.next(&mut r).unwrap().unwrap().0, tag::EXIT);
        assert_eq!(f.next(&mut r).unwrap(), None);
    }

    #[test]
    fn structs_match_the_zig_layouts() {
        // `New` and `Attach` are 24-byte extern structs: Size (8), two u32s, a u64.
        let n = new_payload(80, 24, "/tmp", "work", 0x1122);
        assert_eq!(&n[..8], &[80, 0, 24, 0, 0, 0, 0, 0]);
        assert_eq!(u32::from_le_bytes(n[8..12].try_into().unwrap()), 4);
        assert_eq!(u64::from_le_bytes(n[16..24].try_into().unwrap()), 0x1122);
        assert_eq!(&n[24..], b"/tmpwork");
        let a = attach_payload(80, 24, 7, 9);
        assert_eq!(a.len(), 24);
        assert_eq!(u32::from_le_bytes(a[8..12].try_into().unwrap()), 7);
    }

    #[test]
    fn reads_listings() {
        let ls = "SESSION     STATE     TABS  PANES  RUNNING\n\u{F011B} home      open         2      3  claude, hx\n\u{F03D2} work      detached     -      1  \n";
        let s = parse_sessions(ls);
        assert_eq!(s.len(), 2);
        assert_eq!((s[0].name.as_str(), s[0].state.as_str(), s[0].tabs, s[0].panes), ("home", "open", Some(2), 3));
        assert_eq!(s[0].running, "claude, hx");
        assert_eq!(s[1].tabs, None);
        let p = parse_panes("SESSION   ID  STATE     SIZE     COMMAND\nhome*     25  attached  158x23   claude --resume\nwork      26  attached  78x23    -zsh\n");
        assert_eq!(program_of(&p[&25]), "claude");
        assert_eq!(program_of(&p[&26]), "zsh");
    }

    #[test]
    fn the_animals_are_spookys_and_each_once() {
        assert_eq!(ANIMALS.len(), 32);
        assert_eq!(ANIMALS[0], ("🐾", "paw"));
        for (i, (g, n)) in ANIMALS.iter().enumerate() {
            assert!(ANIMALS[i + 1..].iter().all(|(g2, n2)| g2 != g && n2 != n), "{n} twice");
        }
    }

    #[test]
    fn reads_open_windows() {
        let json = r#"{"session":"work","active":1,"tabs":[{"argv":["/x/spooky","attach","-p","4"]},
            {"split":"h","ratio":0.3,"left":{"argv":["spooky","attach","-p","5"]},"right":{"argv":["spooky","new","-s","work"]}}]}"#;
        let o: Open = serde_json::from_str(json).unwrap();
        assert_eq!(o.tabs.len(), 2);
        assert_eq!(leaf_of(o.tabs[0].argv.as_ref().unwrap()), Some(Leaf::Attach(4)));
        let right = o.tabs[1].right.as_ref().unwrap();
        assert_eq!(leaf_of(right.argv.as_ref().unwrap()), Some(Leaf::New("work".into())));
        assert_eq!(o.tabs[1].ratio, 0.3);
    }
}
