//! A pane: a terminal, the program behind it, and the thread that feeds one into the other.
//! The program normally lives in the spooky daemon, which owns the pty so the pane outlives the
//! window; without a daemon (or in headless demos) it runs on a local pty instead.

use crate::spooky;
use crate::vt::{self, Rgb, Terminal};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

pub type Waker = rune::Waker;

/// Where bytes for the program go: a pty, or `input` frames to the daemon.
type Sink = Arc<dyn Fn(&[u8]) + Send + Sync>;

/// What the terminal's callbacks need: the program to answer queries to, and flags for the UI.
struct Events {
    sink: Sink,
    changed: Arc<AtomicBool>,
    attention: Arc<AtomicBool>,
    note: Arc<Mutex<Option<(String, String)>>>,
}

impl vt::Handler for Events {
    fn write_pty(&mut self, data: &[u8]) {
        (self.sink)(data);
    }
    fn title_changed(&mut self) {
        self.changed.store(true, Ordering::Release);
    }
    fn pwd_changed(&mut self) {
        self.changed.store(true, Ordering::Release);
    }
    fn bell(&mut self) {
        self.attention.store(true, Ordering::Release);
        self.changed.store(true, Ordering::Release);
    }
    fn notify(&mut self, title: &str, body: &str) {
        *self.note.lock().unwrap() = Some((title.to_string(), body.to_string()));
        self.attention.store(true, Ordering::Release);
        self.changed.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy)]
pub struct Theme {
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub ansi: [Rgb; 16],
    pub cursor_style: vt::CursorStyle,
}

/// What a new pane runs.
pub enum Source<'a> {
    /// A new shell in the daemon, in `session` ("" for the window's session).
    New { cwd: Option<&'a str>, session: &'a str },
    /// An existing daemon pane.
    Attach(u32),
    /// A local pty, not kept by spooky: a shell, or `command` (an editor on a scrollback dump).
    Local { cwd: Option<&'a str>, command: Option<&'a [String]> },
}

enum Backend {
    Spooky { stream: Arc<Mutex<UnixStream>> },
    Local { master: Box<dyn MasterPty + Send>, shell_pid: Option<u32> },
}

pub struct Pane {
    pub id: u32,
    /// The surface id spooky knows this pane's view by.
    pub surface: u64,
    /// The daemon's pane id, once attached (0 for local panes).
    pub spooky_id: Arc<AtomicU32>,
    pub term: Arc<Mutex<Terminal>>,
    sink: Sink,
    backend: Backend,
    /// Set by the reader thread when there is something new to draw.
    pub dirty: Arc<AtomicBool>,
    /// Set when the title or working directory changed.
    pub meta_changed: Arc<AtomicBool>,
    /// The program ended (or the daemon went away).
    pub exited: Arc<AtomicBool>,
    /// Another view attached to this pane and took it over: close it here, but don't end it.
    pub stolen: Arc<AtomicBool>,
    /// Set by a bell or a desktop notification: the program wants the user (an agent waiting
    /// for input, a finished build). Cleared when the user comes to the pane.
    pub attention: Arc<AtomicBool>,
    /// The last desktop notification the program sent (title, body), for ours.
    pub note: Arc<Mutex<Option<(String, String)>>>,
    /// Shown in the header: the foreground program and the working directory.
    pub program: String,
    pub cwd: String,
    /// A file to delete when the pane goes (a scrollback dump).
    pub cleanup: Option<std::path::PathBuf>,
}

impl Drop for Pane {
    /// A pane leaving the window lets go of what it runs on. Its reader thread holds its own
    /// handle (a copy of the socket, or of the pty), so dropping ours alone would close nothing:
    /// spooky would keep a closed split's pane alive, attached to no one's view, and save it
    /// (a reboot brought it back as an extra tab), and a local shell would run on.
    fn drop(&mut self) {
        match &self.backend {
            // Shut the socket itself down: every handle to it sees the end, so the reader
            // thread stops and spooky sees this view go (and ends the pane if it was closed).
            Backend::Spooky { stream } => {
                if let Ok(s) = stream.lock() {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }
            // Hang up the shell as closing a terminal does; its pty then ends the reader.
            Backend::Local { shell_pid: Some(pid), .. } => {
                // SAFETY: kill() only sends a signal, to the shell's process group.
                unsafe { libc::kill(-(*pid as i32), libc::SIGHUP) };
            }
            Backend::Local { .. } => {}
        }
        if let Some(p) = &self.cleanup {
            let _ = std::fs::remove_file(p);
        }
    }
}

fn new_terminal(sink: Sink, theme: &Theme, changed: Arc<AtomicBool>, attention: Arc<AtomicBool>, note: Arc<Mutex<Option<(String, String)>>>) -> Arc<Mutex<Terminal>> {
    let mut term = Terminal::new(80, 24, Events { sink, changed, attention, note });
    term.set_colors(theme.fg, theme.bg, theme.cursor, &theme.ansi);
    term.set_scrollback_bytes(16 << 20);
    term.set_default_cursor(theme.cursor_style, true);
    term.enable_images(320 << 20);
    Arc::new(Mutex::new(term))
}

impl Pane {
    pub fn spawn(id: u32, source: Source<'_>, theme: &Theme, waker: Option<Waker>) -> std::io::Result<Pane> {
        let wake = move || {
            if let Some(w) = &waker {
                w();
            }
        };
        let meta_changed = Arc::new(AtomicBool::new(true));
        let attention = Arc::new(AtomicBool::new(false));
        let note = Arc::new(Mutex::new(None));
        let dirty = Arc::new(AtomicBool::new(true));
        let exited = Arc::new(AtomicBool::new(false));
        let stolen = Arc::new(AtomicBool::new(false));
        let spooky_id = Arc::new(AtomicU32::new(0));
        let surface = spooky::fresh_id();
        let home = std::env::var("HOME").ok();

        let (term, sink, backend, cwd) = match source {
            Source::New { .. } | Source::Attach(_) => {
                let mut stream = spooky::connect()?;
                let (cwd, request) = match source {
                    Source::New { cwd, session } => {
                        let cwd = cwd.map(String::from).or(home);
                        (cwd.clone(), (spooky::tag::NEW, spooky::new_payload(80, 24, cwd.as_deref().unwrap_or(""), session, surface)))
                    }
                    Source::Attach(pane) => (None, (spooky::tag::ATTACH, spooky::attach_payload(80, 24, pane, surface))),
                    Source::Local { .. } => unreachable!(),
                };
                spooky::write_frame(&mut stream, request.0, &request.1)?;
                let mut reader = stream.try_clone()?;
                let stream = Arc::new(Mutex::new(stream));
                let out = stream.clone();
                let sink: Sink = Arc::new(move |b: &[u8]| {
                    let _ = spooky::write_frame(&mut *out.lock().unwrap(), spooky::tag::INPUT, b);
                });
                let term = new_terminal(sink.clone(), theme, meta_changed.clone(), attention.clone(), note.clone());
                let (t, d, e, s, sid) = (term.clone(), dirty.clone(), exited.clone(), stolen.clone(), spooky_id.clone());
                std::thread::Builder::new().name(format!("spooky-pane-{id}")).spawn(move || {
                    let mut frames = spooky::Frames::default();
                    loop {
                        match frames.next(&mut reader) {
                            Ok(Some((spooky::tag::OUTPUT, p))) => {
                                t.lock().unwrap().write(&p);
                                if !d.swap(true, Ordering::AcqRel) {
                                    wake();
                                }
                            }
                            Ok(Some((spooky::tag::ATTACHED, p))) if p.len() >= 4 => {
                                sid.store(u32::from_le_bytes(p[..4].try_into().unwrap()), Ordering::Release);
                            }
                            Ok(Some((spooky::tag::DETACHED, _))) => {
                                s.store(true, Ordering::Release);
                                break;
                            }
                            Ok(Some((spooky::tag::ERR, p))) => {
                                eprintln!("runeterm: spooky pane: {}", String::from_utf8_lossy(&p));
                                break;
                            }
                            Ok(Some((spooky::tag::EXIT, _))) | Ok(None) | Err(_) => break,
                            Ok(Some(_)) => {}
                        }
                    }
                    e.store(true, Ordering::Release);
                    wake();
                })?;
                (term, sink, Backend::Spooky { stream }, cwd)
            }
            Source::Local { cwd, command } => {
                let pair = native_pty_system()
                    .openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 })
                    .map_err(std::io::Error::other)?;
                let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
                let mut cmd = match command {
                    Some([prog, args @ ..]) => {
                        let mut c = CommandBuilder::new(prog);
                        c.args(args);
                        c
                    }
                    _ => CommandBuilder::new(&shell),
                };
                cmd.env("TERM", "xterm-256color");
                cmd.env("COLORTERM", "truecolor");
                cmd.env("TERM_PROGRAM", "runeterm");
                let dir = cwd.map(String::from).or(home);
                if let Some(d) = &dir {
                    cmd.cwd(d);
                }
                let child = pair.slave.spawn_command(cmd).map_err(std::io::Error::other)?;
                let shell_pid = child.process_id();
                drop(pair.slave);
                let writer = Arc::new(Mutex::new(pair.master.take_writer().map_err(std::io::Error::other)?));
                let sink: Sink = Arc::new(move |b: &[u8]| {
                    let _ = writer.lock().unwrap().write_all(b);
                });
                let term = new_terminal(sink.clone(), theme, meta_changed.clone(), attention.clone(), note.clone());
                let mut reader = pair.master.try_clone_reader().map_err(std::io::Error::other)?;
                let (t, d, e) = (term.clone(), dirty.clone(), exited.clone());
                std::thread::Builder::new().name(format!("pty-{id}")).spawn(move || {
                    let mut child = child;
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                t.lock().unwrap().write(&buf[..n]);
                                if !d.swap(true, Ordering::AcqRel) {
                                    wake();
                                }
                            }
                        }
                    }
                    let _ = child.wait();
                    e.store(true, Ordering::Release);
                    wake();
                })?;
                (term, sink, Backend::Local { master: pair.master, shell_pid }, dir)
            }
        };

        let program = std::env::var("SHELL").ok().and_then(|s| s.rsplit('/').next().map(String::from)).unwrap_or("sh".into());
        Ok(Pane {
            id,
            surface,
            spooky_id,
            term,
            sink,
            backend,
            dirty,
            meta_changed,
            exited,
            stolen,
            attention,
            note,
            program,
            cwd: cwd.map(|d| tilde(&d)).unwrap_or_default(),
            cleanup: None,
        })
    }

    pub fn is_spooky(&self) -> bool {
        matches!(self.backend, Backend::Spooky { .. })
    }

    pub fn send(&self, bytes: &[u8]) {
        if !bytes.is_empty() {
            (self.sink)(bytes);
        }
    }

    /// Resizes the terminal and the program's pty, if the grid changed.
    pub fn resize(&self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) {
        let mut t = self.term.lock().unwrap();
        if t.size() == (cols, rows) {
            return;
        }
        t.resize(cols, rows, cell_w, cell_h);
        let (px, py) = ((cols as u32 * cell_w).min(u16::MAX as u32) as u16, (rows as u32 * cell_h).min(u16::MAX as u32) as u16);
        match &self.backend {
            Backend::Local { master, .. } => {
                let _ = master.resize(PtySize { rows, cols, pixel_width: px, pixel_height: py });
            }
            Backend::Spooky { stream } => {
                let _ = spooky::write_frame(&mut *stream.lock().unwrap(), spooky::tag::RESIZE, &spooky::size(cols, rows, px, py));
            }
        }
    }

    /// Re-reads the foreground program and working directory; true when either changed.
    /// Daemon panes take their program from the daemon's pane list (`commands`).
    pub fn refresh_meta(&mut self, commands: Option<&HashMap<u32, String>>) -> bool {
        let (fg_pid, mut program) = match &self.backend {
            Backend::Local { master, shell_pid } => {
                let fg = master.process_group_leader().map(|p| p as u32).or(*shell_pid);
                let p = fg.and_then(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).ok()).map(|s| s.trim().to_string());
                (fg, p)
            }
            Backend::Spooky { .. } => {
                let id = self.spooky_id.load(Ordering::Acquire);
                (None, commands.and_then(|c| c.get(&id)).map(|c| spooky::program_of(c)).filter(|p| !p.is_empty()))
            }
        };
        if program.as_deref().is_none_or(str::is_empty) {
            program = Some(self.program.clone());
        }
        let program = program.unwrap();
        let pwd = self.term.lock().unwrap().pwd();
        let cwd = if !pwd.is_empty() {
            tilde(&osc7_path(&pwd))
        } else {
            fg_pid
                .and_then(|p| std::fs::read_link(format!("/proc/{p}/cwd")).ok())
                .map(|p| tilde(&p.to_string_lossy()))
                .unwrap_or_else(|| self.cwd.clone())
        };
        let changed = program != self.program || cwd != self.cwd;
        self.program = program;
        self.cwd = cwd;
        changed
    }
}

/// The path of an OSC 7 report: `file://host/path`, `kitty-shell-cwd://host/path`, or a bare
/// path, with %-escapes decoded.
pub fn osc7_path(url: &str) -> String {
    let path = match url.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => url,
    };
    let mut out = Vec::with_capacity(path.len());
    let b = path.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&path[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `/home/me/x` → `~/x`.
pub fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && path.starts_with(&h) => format!("~{}", &path[h.len()..]),
        _ => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::osc7_path;

    #[test]
    fn reads_osc7_urls() {
        assert_eq!(osc7_path("kitty-shell-cwd://adi/home/adi/code"), "/home/adi/code");
        assert_eq!(osc7_path("file://host/tmp/a%20b"), "/tmp/a b");
        assert_eq!(osc7_path("/plain"), "/plain");
    }
}
