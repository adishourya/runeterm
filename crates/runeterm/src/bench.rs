//! `runeterm --bench`: quick, light numbers for the hot paths, without a window. Each part
//! runs for a few seconds at most.

use crate::config::Config;
use crate::grid::GridRenderer;
use crate::pane::{Pane, Source};
use crate::vt::{Handler, RenderState, Terminal};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

struct Quiet;
impl Handler for Quiet {}

fn mb(n: usize) -> f64 {
    n as f64 / (1024.0 * 1024.0)
}

/// About `target` bytes of `line(i)` lines.
fn corpus(target: usize, line: impl Fn(usize) -> String) -> Vec<u8> {
    let mut out = Vec::with_capacity(target + 256);
    let mut i = 0;
    while out.len() < target {
        out.extend_from_slice(line(i).as_bytes());
        i += 1;
    }
    out
}

fn parse(name: &str, data: &[u8]) {
    let mut t = Terminal::new(200, 60, Quiet);
    t.set_scrollback_bytes(16 << 20);
    let start = Instant::now();
    for chunk in data.chunks(64 * 1024) {
        t.write(chunk);
    }
    let s = start.elapsed().as_secs_f64();
    println!("  {name:<28} {:>7.1} MB/s   ({:.1} MB in {:.2} s)", mb(data.len()) / s, mb(data.len()), s);
}

fn frames(config: &Config) {
    let mut grid = GridRenderer::new(config);
    let scale = 1.0;
    let mut t = Terminal::new(200, 60, Quiet);
    // A full screen of coloured text.
    let screen: String = (0..60)
        .map(|y| {
            (0..20)
                .map(|x| format!("\x1b[38;5;{}m{:<9}\x1b[0m ", 16 + (x * 7 + y * 3) % 216, format!("w{x}r{y}")))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\r\n");
    t.write(b"\x1b[H\x1b[2J");
    t.write(screen.as_bytes());
    let mut rs = RenderState::new();
    // Warm the glyph cache once, as a running terminal would have.
    rs.begin(&t);
    grid.bench_rows(&rs.finish(), scale);

    let runs = 120;
    let mut full = Duration::ZERO;
    let mut instances = 0;
    for i in 0..runs {
        // Touch every row so the whole screen is dirty, like a full redraw.
        t.write(format!("\x1b[H\x1b[{}m", 31 + i % 6).as_bytes());
        t.write(screen.as_bytes());
        let start = Instant::now();
        rs.begin(&t);
        let f = rs.finish();
        instances = grid.bench_rows(&f, scale);
        full += start.elapsed();
    }
    let mut line = Duration::ZERO;
    for i in 0..runs {
        // One line changes (typing at a prompt).
        t.write(format!("\x1b[60;1Hprompt {i}").as_bytes());
        let start = Instant::now();
        rs.begin(&t);
        let f = rs.finish();
        grid.bench_rows(&f, scale);
        line += start.elapsed();
    }
    let ms = |d: Duration| d.as_secs_f64() * 1000.0 / runs as f64;
    println!("  full 200×60 screen          {:>7.2} ms/frame ({instances} quads)", ms(full));
    println!("  one changed line            {:>7.3} ms/frame", ms(line));
}

/// Runs `seq 1 N` and times until the pane has drained it and the program exited.
fn end_to_end(config: &Config, label: &str, source: Source<'_>, then_type: Option<&str>) {
    let theme = config.colors();
    let start = Instant::now();
    let pane = match Pane::spawn(1, source, &theme, None) {
        Ok(p) => p,
        Err(e) => {
            println!("  {label:<28} skipped ({e})");
            return;
        }
    };
    if let Some(cmd) = then_type {
        std::thread::sleep(Duration::from_millis(300)); // let the shell come up
        pane.send(cmd.as_bytes());
    }
    let deadline = Instant::now() + Duration::from_secs(60);
    while !pane.exited.load(Ordering::Acquire) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let s = start.elapsed().as_secs_f64() - if then_type.is_some() { 0.3 } else { 0.0 };
    let bytes = seq_bytes(SEQ_N);
    println!("  {label:<28} {:>7.1} MB/s   (seq 1 {SEQ_N}, {:.1} MB, {:.2} s)", mb(bytes) / s, mb(bytes), s);
}

const SEQ_N: usize = 2_000_000;

fn seq_bytes(n: usize) -> usize {
    (1..=n).map(|i| i.to_string().len() + 2).sum() // "\r\n" through a pty
}

fn baseline() {
    // The same output through a bare pty to /dev/null: the ceiling for this machine.
    let start = Instant::now();
    let ok = std::process::Command::new("script")
        .args(["-qc", &format!("seq 1 {SEQ_N}"), "/dev/null"])
        .stdout(std::process::Stdio::null())
        .status()
        .is_ok();
    let s = start.elapsed().as_secs_f64();
    if ok {
        println!("  {:<28} {:>7.1} MB/s   ({:.2} s)", "bare pty → /dev/null", mb(seq_bytes(SEQ_N)) / s, s);
    }
}

pub fn run(config: &Config) {
    println!("runeterm bench (libghostty-vt {})", crate::vt::version());
    println!("\nparsing (into a 200×60 terminal)");
    parse("plain lines", &corpus(20 << 20, |i| format!("{i:>8} the quick brown fox jumps over the lazy dog\r\n")));
    parse("coloured ls-style lines", &corpus(20 << 20, |i| {
        format!("\x1b[1;34m{i}\x1b[0m  \x1b[32mexec\x1b[0m  \x1b[38;2;200;120;{}msrc/file_{i}.rs\x1b[0m\r\n", i % 255)
    }));
    parse("full-screen redraws (htop)", &corpus(20 << 20, |i| {
        format!("\x1b[{};1H\x1b[48;5;236m{:<60}\x1b[0m\x1b[K", 1 + i % 60, format!("cpu{} [{}]", i % 8, "|".repeat(i % 40)))
    }));
    println!("\nbuilding a frame (CPU side: render state, glyphs, quads)");
    frames(config);
    println!("\nend to end: seq 1 {SEQ_N} through a pty into runeterm's terminal");
    baseline();
    let cmd = vec!["seq".to_string(), "1".to_string(), SEQ_N.to_string()];
    end_to_end(config, "local pty", Source::Local { cwd: None, command: Some(&cmd) }, None);
    end_to_end(config, "through spooky", Source::New { cwd: None, session: "bench" }, Some(&format!("seq 1 {SEQ_N}; exit\r")));
}
