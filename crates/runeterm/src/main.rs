mod accents;
mod bench;
mod config;
mod grid;
mod icons;
mod pane;
mod split;
mod shader;
mod spooky;
mod sprite;
mod state;
mod status;
mod ui;
mod vt;

/// Panics also go to `~/.local/state/runeterm/crash.log`: launched from an app menu, stderr goes
/// nowhere.
fn log_panics() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let dir = std::env::var("XDG_STATE_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state"))
            .join("runeterm");
        let _ = std::fs::create_dir_all(&dir);
        let trace = std::backtrace::Backtrace::force_capture();
        let when = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let entry = format!("--- {when}\n{info}\n{trace}\n");
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("crash.log")) {
            let _ = f.write_all(entry.as_bytes());
        }
        default(info);
    }));
}

fn main() {
    log_panics();
    vt::install_png_decoder();
    let config = config::Config::load();
    if std::env::args().any(|a| a == "--bench") {
        bench::run(&config);
        return;
    }
    eprintln!("runeterm: libghostty-vt {}, font {} {}pt", vt::version(), config.font.family, config.font.size);
    // Wayland app_id / X11 WM_CLASS: matches runeterm.desktop, and compositor window rules.
    rune::set_app_id("runeterm");
    rune::run_with(ui::spec(), grid::GridRenderer::new(&config));
}
