//! runeterm's settings, in `~/.config/runeterm/config` (`$XDG_CONFIG_HOME/runeterm/config`).
//! The syntax is Ghostty's: `key = value`, one per line, `#` for comments. The first run
//! writes the file from Ghostty's config (font, colours, cursor, shaders) so nothing changes
//! visually, and copies Ghostty's shaders next to it. ⌥R reloads it.

use crate::grid::FontSpec;
use crate::pane::Theme;
use crate::vt::{CursorStyle, Rgb};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone)]
pub struct Config {
    pub font: FontSpec,
    pub theme: Theme,
    pub cursor_style: CursorStyle,
    /// `cursor-opacity`: 0 hides the drawn cursor (a shader draws its own).
    pub cursor_opacity: f32,
    /// `smooth-cursor`: glide the cursor between cells.
    pub cursor_smooth: bool,
    /// `custom-shader` files, in order, resolved against the config's directory.
    pub shaders: Vec<PathBuf>,
    /// `custom-shader-animation`: keep redrawing shaders while focused (`true`), always, or
    /// only when something changed (`false`).
    pub shader_animation: ShaderAnimation,
    pub sidebar: Sidebar,
}

/// The sidebar's settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sidebar {
    /// `sidebar = pinned`: always open, and the splits make room. `hover` (the default) shows
    /// a rail that opens over the splits on hover.
    pub pinned: bool,
    /// `sidebar-width`: open width, in points.
    pub width: f32,
    /// `sidebar-rail-width`: closed width, in points.
    pub rail: f32,
    /// `sidebar-clock`, `sidebar-battery`: the time and battery under the session.
    pub clock: bool,
    pub battery: bool,
    /// `sidebar-host`: `user@host` under the session's name.
    pub host: bool,
}

impl Default for Sidebar {
    fn default() -> Self {
        Sidebar { pinned: false, width: 300.0, rail: 76.0, clock: true, battery: true, host: true }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderAnimation {
    Off,
    Focused,
    Always,
}

fn rgb(s: &str) -> Option<Rgb> {
    let s = s.trim().trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).ok()?;
    (s.len() == 6).then(|| Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

impl Default for Config {
    fn default() -> Self {
        let ansi = [
            "#000000", "#cc0403", "#19cb00", "#cecb00", "#0d73cc", "#cb1ed1", "#0dcdcd", "#dddddd",
            "#767676", "#f2201f", "#23fd00", "#fffd00", "#1a8fff", "#fd28ff", "#14ffff", "#ffffff",
        ]
        .map(|h| rgb(h).unwrap());
        Config {
            font: FontSpec { family: "monospace".into(), size: 13.0, cell_height: 0.0, cell_height_px: 0.0, padding: 10.0 },
            theme: Theme { fg: Rgb(255, 255, 255), bg: Rgb(0x10, 0x10, 0x10), cursor: Rgb(0xcc, 0xcc, 0xcc), ansi, cursor_style: CursorStyle::Bar },
            cursor_style: CursorStyle::Bar,
            cursor_opacity: 1.0,
            cursor_smooth: true,
            shaders: vec![],
            shader_animation: ShaderAnimation::Focused,
            sidebar: Sidebar::default(),
        }
    }
}

fn config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"))
}

/// `~/.config/runeterm/config`.
pub fn path() -> PathBuf {
    config_home().join("runeterm/config")
}

impl Config {
    /// Reads runeterm's config, writing it from Ghostty's on the first run.
    pub fn load() -> Config {
        let path = path();
        let dir = path.parent().unwrap_or(Path::new("/")).to_path_buf();
        let mut c = Config::default();
        match std::fs::read_to_string(&path) {
            Ok(text) => c.apply(&text, &dir),
            Err(_) => {
                if let Err(e) = Config::create(&path) {
                    eprintln!("runeterm: can't write {}: {e}", path.display());
                }
                if let Ok(text) = std::fs::read_to_string(&path) {
                    c.apply(&text, &dir);
                }
            }
        }
        c
    }

    /// Writes a commented config holding Ghostty's current settings, and copies its shaders.
    fn create(path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir)?;
        let ghostty = config_home().join("ghostty");
        let mut from = Config::default();
        if let Ok(text) = std::fs::read_to_string(ghostty.join("config")) {
            from.apply(&text, &ghostty);
        }
        // Earlier runeterm read Ghostty's bare `adjust-cell-height = 31` as a percentage; keep
        // the line height people already see.
        if from.font.cell_height_px != 0.0 {
            from.font.cell_height = from.font.cell_height_px / 100.0;
            from.font.cell_height_px = 0.0;
        }
        // Shaders come along, so the config doesn't point into Ghostty's.
        let mut shaders = vec![];
        if let Ok(entries) = std::fs::read_dir(ghostty.join("shaders")) {
            std::fs::create_dir_all(dir.join("shaders"))?;
            for e in entries.flatten() {
                let _ = std::fs::copy(e.path(), dir.join("shaders").join(e.file_name()));
            }
        }
        for s in &from.shaders {
            let local = dir.join("shaders").join(s.file_name().unwrap_or_default());
            shaders.push(if local.is_file() { format!("shaders/{}", s.file_name().unwrap().to_string_lossy()) } else { s.display().to_string() });
        }
        std::fs::write(path, from.render(&shaders))
    }

    /// This config as a commented file.
    fn render(&self, shaders: &[String]) -> String {
        let f = &self.font;
        let line_height = if f.cell_height_px != 0.0 {
            format!("{}", f.cell_height_px)
        } else {
            format!("{}%", (f.cell_height * 100.0).round())
        };
        let style = match self.cursor_style {
            CursorStyle::Block => "block",
            CursorStyle::Underline => "underline",
            CursorStyle::BlockHollow => "block_hollow",
            CursorStyle::Bar => "bar",
        };
        let palette: String = self.theme.ansi.iter().enumerate().map(|(i, c)| format!("palette = {i}={}\n", hex(*c))).collect();
        let shader_lines: String = if shaders.is_empty() {
            "# custom-shader = shaders/cursor_glide.glsl\n".into()
        } else {
            shaders.iter().map(|s| format!("custom-shader = {s}\n")).collect()
        };
        let animation = match self.shader_animation {
            ShaderAnimation::Off => "false",
            ShaderAnimation::Focused => "true",
            ShaderAnimation::Always => "always",
        };
        format!(
            "# runeterm settings. Press ⌥R in runeterm to reload after editing.\n\
             # Same syntax as Ghostty: `key = value`, one per line.\n\
             \n\
             # --- Font ---\n\
             font-family = {}\n\
             # In points, as in Ghostty.\n\
             font-size = {}\n\
             # Extra space between lines: a percentage (31%) or pixels (4).\n\
             line-height = {line_height}\n\
             # Space between a split's edge and its text, in points.\n\
             padding = {}\n\
             \n\
             # --- Colours ---\n\
             background = {}\n\
             foreground = {}\n\
             cursor-color = {}\n\
             {palette}\
             \n\
             # --- Cursor ---\n\
             # bar, block, underline or block_hollow\n\
             cursor-style = {style}\n\
             # 0 hides runeterm's cursor, for a shader that draws its own (cursor_glide.glsl).\n\
             cursor-opacity = {}\n\
             # Glide between cells instead of jumping.\n\
             smooth-cursor = {}\n\
             \n\
             # --- Shaders (Ghostty/Shadertoy GLSL, paths relative to this folder) ---\n\
             {shader_lines}\
             # true: animate while focused, always, or false: only when something changes.\n\
             custom-shader-animation = {animation}\n\
             \n\
             # --- Sidebar ---\n\
             # hover: a rail that opens over the splits when hovered. pinned: always open.\n\
             sidebar = {sidebar}\n\
             # Open and closed widths, in points.\n\
             sidebar-width = {}\n\
             sidebar-rail-width = {}\n\
             # The time and battery under the session.\n\
             sidebar-clock = {}\n\
             sidebar-battery = {}\n\
             # user@host under the session's name.\n\
             sidebar-host = {}\n",
            f.family,
            f.size,
            f.padding,
            hex(self.theme.bg),
            hex(self.theme.fg),
            hex(self.theme.cursor),
            self.cursor_opacity,
            self.cursor_smooth,
            self.sidebar.width,
            self.sidebar.rail,
            self.sidebar.clock,
            self.sidebar.battery,
            self.sidebar.host,
            sidebar = if self.sidebar.pinned { "pinned" } else { "hover" },
        )
    }

    pub fn apply(&mut self, text: &str, dir: &Path) {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim().trim_matches('"'));
            match k {
                "font-family" if !v.is_empty() => self.font.family = v.to_string(),
                "font-size" => self.font.size = v.parse().unwrap_or(self.font.size),
                // A percentage of the font's line height, or pixels (Ghostty's meaning of a
                // bare number).
                "line-height" | "adjust-cell-height" => {
                    if let Some(pct) = v.strip_suffix('%') {
                        if let Ok(p) = pct.trim().parse::<f32>() {
                            self.font.cell_height = p / 100.0;
                            self.font.cell_height_px = 0.0;
                        }
                    } else if let Ok(px) = v.parse::<f32>() {
                        self.font.cell_height_px = px;
                        self.font.cell_height = 0.0;
                    }
                }
                "padding" | "window-padding" => self.font.padding = v.parse().unwrap_or(self.font.padding),
                "background" => self.theme.bg = rgb(v).unwrap_or(self.theme.bg),
                "foreground" => self.theme.fg = rgb(v).unwrap_or(self.theme.fg),
                "cursor-color" => self.theme.cursor = rgb(v).unwrap_or(self.theme.cursor),
                "palette" => {
                    if let Some((i, c)) = v.split_once('=') {
                        if let (Ok(i), Some(c)) = (i.trim().parse::<usize>(), rgb(c)) {
                            if i < 16 {
                                self.theme.ansi[i] = c;
                            }
                        }
                    }
                }
                "cursor-opacity" => self.cursor_opacity = v.parse().unwrap_or(self.cursor_opacity),
                "smooth-cursor" | "runeterm-smooth-cursor" => self.cursor_smooth = v != "false",
                "custom-shader" if v.is_empty() => self.shaders.clear(),
                "custom-shader" => {
                    let p = v.strip_prefix("~/").map_or_else(
                        || dir.join(v),
                        |rest| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
                    );
                    self.shaders.push(p);
                }
                "custom-shader-animation" => {
                    self.shader_animation = match v {
                        "false" => ShaderAnimation::Off,
                        "always" => ShaderAnimation::Always,
                        _ => ShaderAnimation::Focused,
                    }
                }
                "sidebar" => self.sidebar.pinned = matches!(v, "pinned" | "open" | "always"),
                "sidebar-width" => self.sidebar.width = v.parse::<f32>().map(|w| w.clamp(200.0, 600.0)).unwrap_or(self.sidebar.width),
                "sidebar-rail-width" => self.sidebar.rail = v.parse::<f32>().map(|w| w.clamp(56.0, 140.0)).unwrap_or(self.sidebar.rail),
                "sidebar-clock" => self.sidebar.clock = v != "false",
                "sidebar-battery" => self.sidebar.battery = v != "false",
                "sidebar-host" => self.sidebar.host = v != "false",
                "cursor-style" => {
                    self.cursor_style = match v {
                        "block" => CursorStyle::Block,
                        "underline" => CursorStyle::Underline,
                        "block_hollow" => CursorStyle::BlockHollow,
                        _ => CursorStyle::Bar,
                    }
                }
                _ => {}
            }
        }
        self.theme.cursor_style = self.cursor_style;
    }
}

/// A reloaded config for the renderer, which lives outside the app state.
static PUBLISHED: Mutex<Option<Config>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);

pub fn publish(c: Config) {
    *PUBLISHED.lock().unwrap() = Some(c);
    GENERATION.fetch_add(1, Ordering::Release);
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn published() -> Option<Config> {
    PUBLISHED.lock().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_settings() {
        let mut c = Config::default();
        c.apply("font-family = DankMono Nerd Font Mono\nfont-size = 22\nline-height = 31%\n# x\nbackground = #101010\npalette = 1=#cc0403\ncursor-style = block\ncursor-opacity = 0\ncustom-shader  = \"./shaders/cursor_glide.glsl\"\n#custom-shader = x.glsl\n", Path::new("/cfg"));
        assert_eq!(c.font.family, "DankMono Nerd Font Mono");
        assert_eq!(c.font.size, 22.0);
        assert!((c.font.cell_height - 0.31).abs() < 1e-6);
        assert_eq!(c.theme.ansi[1], Rgb(0xcc, 0x04, 0x03));
        assert_eq!(c.cursor_style, CursorStyle::Block);
        assert_eq!(c.cursor_opacity, 0.0);
        assert_eq!(c.shaders, vec![PathBuf::from("/cfg/./shaders/cursor_glide.glsl")]);
    }

    #[test]
    fn reads_sidebar_settings() {
        let mut c = Config::default();
        c.apply("sidebar = pinned\nsidebar-width = 340\nsidebar-rail-width = 9000\nsidebar-battery = false\n", Path::new("/"));
        assert!(c.sidebar.pinned);
        assert_eq!(c.sidebar.width, 340.0);
        assert_eq!(c.sidebar.rail, 140.0, "clamped");
        assert!(c.sidebar.clock && !c.sidebar.battery);
    }

    #[test]
    fn line_height_takes_percent_or_pixels() {
        let mut c = Config::default();
        c.apply("line-height = 4\n", Path::new("/"));
        assert_eq!((c.font.cell_height, c.font.cell_height_px), (0.0, 4.0));
        c.apply("adjust-cell-height = 20%\n", Path::new("/"));
        assert!((c.font.cell_height - 0.2).abs() < 1e-6 && c.font.cell_height_px == 0.0);
    }

    #[test]
    fn the_written_file_reads_back_the_same() {
        let mut c = Config::default();
        c.apply("font-family = X\nfont-size = 19\nline-height = 30%\ncursor-opacity = 0\nbackground = #121212\n", Path::new("/"));
        let text = c.render(&["shaders/a.glsl".into()]);
        let mut back = Config::default();
        back.apply(&text, Path::new("/cfg"));
        assert_eq!(back.font.family, "X");
        assert_eq!(back.font.size, 19.0);
        assert!((back.font.cell_height - 0.30).abs() < 1e-6);
        assert_eq!(back.theme.bg, Rgb(0x12, 0x12, 0x12));
        assert_eq!(back.theme.ansi, c.theme.ansi);
        assert_eq!(back.cursor_opacity, 0.0);
        assert_eq!(back.shaders, vec![PathBuf::from("/cfg/shaders/a.glsl")]);
        assert_eq!(back.sidebar, c.sidebar);
    }
}
