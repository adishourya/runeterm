//! runeterm's settings, in `~/.config/runeterm/config` (`$XDG_CONFIG_HOME/runeterm/config`).
//! The syntax is Ghostty's: `key = value`, one per line, `#` for comments. The first run
//! writes the file from Ghostty's config (font, colours, cursor, shaders) so nothing changes
//! visually, and copies Ghostty's shaders next to it. ⌥R reloads it.

use crate::grid::{FontSpec, FontStyle, parse_features, parse_synthetic, parse_variations};
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
    /// `scrollback-editor`: what ⌥E opens a split's scrollback in.
    pub scrollback_editor: String,
    /// `theme = light` uses `light_theme` (the `light-*` keys) for the terminals and a light UI.
    pub light: bool,
    pub light_theme: Theme,
    /// `accent`: the focused split's light, selections, the active tab.
    pub accent: Rgb,
    /// `accent-colors`: the swatches the sidebar offers.
    pub accents: Vec<Rgb>,
    pub copy_on_select: bool,
    pub confirm_close: bool,
    pub notifications: bool,
    /// `keybind = CHORD=ACTION`: the keys, the defaults changed by the config's lines.
    pub keybinds: crate::keys::Keybinds,
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
    /// `sidebar-background`, `light-sidebar-background`: the sidebar's colour (rail, pinned and
    /// open alike) in dark and light mode. None keeps the default: the window shows through a
    /// pinned sidebar or the rail, and an open one is a raised surface.
    pub background: Option<Rgb>,
    pub light_background: Option<Rgb>,
}

impl Default for Sidebar {
    fn default() -> Self {
        Sidebar { pinned: false, width: 300.0, rail: 76.0, clock: true, battery: true, host: true, background: None, light_background: None }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShaderAnimation {
    Off,
    Focused,
    Always,
}

pub fn rgb(s: &str) -> Option<Rgb> {
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
            font: FontSpec::default(),
            theme: Theme { fg: Rgb(255, 255, 255), bg: Rgb(0x10, 0x10, 0x10), cursor: Rgb(0xcc, 0xcc, 0xcc), ansi, cursor_style: CursorStyle::Bar },
            cursor_style: CursorStyle::Bar,
            cursor_opacity: 1.0,
            cursor_smooth: true,
            shaders: vec![],
            shader_animation: ShaderAnimation::Focused,
            sidebar: Sidebar::default(),
            scrollback_editor: "auto".into(),
            light: false,
            light_theme: Theme {
                fg: Rgb(0x1f, 0x23, 0x28),
                bg: Rgb(0xfa, 0xfa, 0xf8),
                cursor: Rgb(0x3b, 0x3f, 0x46),
                ansi: [
                    "#24292f", "#cf222e", "#116329", "#7d4e00", "#0550ae", "#8250df", "#1b7c83", "#6e7781",
                    "#57606a", "#a40e26", "#1a7f37", "#9a6700", "#218bff", "#a475f9", "#3192aa", "#8c959f",
                ]
                .map(|h| rgb(h).unwrap()),
                cursor_style: CursorStyle::Bar,
            },
            accent: Rgb(0x8a, 0xb4, 0xf8),
            copy_on_select: false,
            confirm_close: true,
            notifications: true,
            keybinds: crate::keys::Keybinds::default(),
            accents: ["#8ab4f8", "#a78bfa", "#ff7ab6", "#ff9f5a", "#ffd166", "#7ee081", "#4fd1c5"].map(|h| rgb(h).unwrap()).to_vec(),
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
        let fit = (f.fit * 100.0).round();
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
             # Another family for bold, italic or bold italic (default: font-family).\n\
             {families}\
             # A face by its style name, as fc-list shows it (Light, Medium Italic), or false\n\
             # to draw that style with the regular face.\n\
             {styles}\
             # Make up bold and italic when the family has none (true, false, or no-bold,\n\
             # no-italic, no-bold-italic).\n\
             font-synthetic-style = {synthetic}\n\
             # OpenType features: -calt -liga turn ligatures off, ss01 turns a set on.\n\
             {features}\
             # Axes of a variable font, per style: wght=450 (or pick one: ⌥P, Font weight).\n\
             {variations}\
             # In points, as in Ghostty.\n\
             font-size = {}\n\
             # Extra space between lines: a percentage (31%) or pixels (4).\n\
             line-height = {line_height}\n\
             # Let the line height (and, if that's not enough, the font size) move by up to\n\
             # this much so the lines fill the split, with no blank strip under the last one.\n\
             # 0% keeps them exact.\n\
             font-size-fit = {fit}%\n\
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
             # Glide between cells instead of jumping (long jumps, like a paste, still jump).\n\
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
             sidebar-host = {}\n\
             # Its colour in dark and light mode (rail, pinned and open alike); unset, the\n\
             # window shows through.\n\
             {sidebar_colors}\
             \n\
             # --- Appearance ---\n\
             # dark or light. The light terminal colours are light-background,\n\
             # light-foreground, light-cursor-color and light-palette = N=#rrggbb.\n\
             theme = {theme}\n\
             # The focused split's light, selections and the active tab.\n\
             accent = {accent}\n\
             # The swatches the sidebar offers.\n\
             accent-colors = {accents}\n\
             \n\
             # --- Keys ---\n\
             # Selecting with the mouse also copies to the clipboard (middle-click pastes\n\
             # the selection either way).\n\
             copy-on-select = {copy_on_select}\n\
             # Ask before closing a split that's running something other than the shell.\n\
             confirm-close = {confirm_close}\n\
             # Desktop notifications for splits that want you while out of sight.\n\
             notifications = {notifications}\n\
             # alt+e opens the focused split's scrollback in this editor. auto: $VISUAL,\n\
             # $EDITOR, or the first of nvim, vim, hx, vi that's installed.\n\
             scrollback-editor = {editor}\n\
             \n\
             {keybinds}",
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
            sidebar_colors = [("sidebar-background", self.sidebar.background), ("light-sidebar-background", self.sidebar.light_background)]
                .iter()
                .map(|(k, c)| match c {
                    Some(c) => format!("{k} = {}\n", hex(*c)),
                    None => format!("# {k} = \n"),
                })
                .collect::<String>(),
            editor = self.scrollback_editor,
            keybinds = keybind_section(),
            synthetic = match f.synthetic {
                [true, true, true] => "true".to_string(),
                [false, false, false] => "false".to_string(),
                s => ["bold", "italic", "bold-italic"].iter().zip(s).map(|(n, on)| if on { n.to_string() } else { format!("no-{n}") }).collect::<Vec<_>>().join(","),
            },
            features = if f.features.is_empty() {
                "# font-feature = -calt, -liga\n".to_string()
            } else {
                format!("font-feature = {}\n", f.features.iter().map(|(t, v)| match v { 0 => format!("-{t}"), 1 => t.clone(), v => format!("{t}={v}") }).collect::<Vec<_>>().join(", "))
            },
            variations = ["", "-bold", "-italic", "-bold-italic"].iter().zip(&f.variations).map(|(k, vs)| if vs.is_empty() {
                if k.is_empty() { "# font-variation = wght=450\n".to_string() } else { String::new() }
            } else {
                format!("font-variation{k} = {}\n", vs.iter().map(|(t, v)| format!("{t}={v}")).collect::<Vec<_>>().join(", "))
            }).collect::<String>(),
            families = ["bold", "italic", "bold-italic"].iter().zip(&f.families).map(|(k, v)| match v {
                Some(v) => format!("font-family-{k} = {v}\n"),
                None => format!("# font-family-{k} = \n"),
            }).collect::<String>(),
            styles = ["", "-bold", "-italic", "-bold-italic"].iter().zip(&f.styles).map(|(k, v)| match v {
                FontStyle::Named(v) => format!("font-style{k} = {v}\n"),
                FontStyle::Off => format!("font-style{k} = false\n"),
                FontStyle::Default => format!("# font-style{k} = \n"),
            }).collect::<String>(),
            copy_on_select = self.copy_on_select,
            confirm_close = self.confirm_close,
            notifications = self.notifications,
            accent = hex(self.accent),
            accents = self.accents.iter().map(|c| hex(*c)).collect::<Vec<_>>().join(", "),
            theme = if self.light { "light" } else { "dark" },
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
                "font-family-bold" => self.font.families[0] = Some(v.to_string()).filter(|v| !v.is_empty()),
                "font-family-italic" => self.font.families[1] = Some(v.to_string()).filter(|v| !v.is_empty()),
                "font-family-bold-italic" => self.font.families[2] = Some(v.to_string()).filter(|v| !v.is_empty()),
                "font-style" => self.font.styles[0] = FontStyle::parse(v),
                "font-style-bold" => self.font.styles[1] = FontStyle::parse(v),
                "font-style-italic" => self.font.styles[2] = FontStyle::parse(v),
                "font-style-bold-italic" => self.font.styles[3] = FontStyle::parse(v),
                "font-synthetic-style" => self.font.synthetic = parse_synthetic(v),
                // Each line adds to the list; an empty one clears it, as in Ghostty.
                "font-feature" if v.is_empty() => self.font.features.clear(),
                "font-feature" => match parse_features(v) {
                    Some(f) => self.font.features.extend(f),
                    None => eprintln!("runeterm: font-feature = {v}: not a feature (e.g. -calt, ss01)"),
                },
                "font-variation" | "font-variation-bold" | "font-variation-italic" | "font-variation-bold-italic" => {
                    let i = match k { "font-variation" => 0, "font-variation-bold" => 1, "font-variation-italic" => 2, _ => 3 };
                    if v.is_empty() {
                        self.font.variations[i].clear();
                    } else {
                        match parse_variations(v) {
                            Some(vs) => self.font.variations[i].extend(vs),
                            None => eprintln!("runeterm: {k} = {v}: not an axis setting (e.g. wght=450)"),
                        }
                    }
                }
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
                "font-size-fit" => {
                    if let Ok(p) = v.trim_end_matches('%').trim().parse::<f32>() {
                        self.font.fit = (p / 100.0).clamp(0.0, 0.15);
                    }
                }
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
                "smooth-cursor" | "runeterm-smooth-cursor" => self.cursor_smooth = !matches!(v, "false" | "off" | "no" | "0"),
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
                // Empty clears it (back to the default look).
                "sidebar-background" => self.sidebar.background = if v.is_empty() { None } else { rgb(v).or(self.sidebar.background) },
                "light-sidebar-background" => self.sidebar.light_background = if v.is_empty() { None } else { rgb(v).or(self.sidebar.light_background) },
                "scrollback-editor" if !v.is_empty() => self.scrollback_editor = v.to_string(),
                "theme" => self.light = v == "light",
                "copy-on-select" => self.copy_on_select = matches!(v, "true" | "clipboard"),
                "confirm-close" | "confirm-close-surface" => self.confirm_close = v != "false",
                "notifications" | "desktop-notifications" => self.notifications = v != "false",
                "accent" => self.accent = rgb(v).unwrap_or(self.accent),
                "keybind" => {
                    if let Err(e) = self.keybinds.apply(v) {
                        eprintln!("runeterm: keybind = {v}: {e}");
                    }
                }
                "accent-colors" => {
                    let list: Vec<Rgb> = v.split(',').filter_map(rgb).collect();
                    if !list.is_empty() {
                        self.accents = list;
                    }
                }
                "light-background" => self.light_theme.bg = rgb(v).unwrap_or(self.light_theme.bg),
                "light-foreground" => self.light_theme.fg = rgb(v).unwrap_or(self.light_theme.fg),
                "light-cursor-color" => self.light_theme.cursor = rgb(v).unwrap_or(self.light_theme.cursor),
                "light-palette" => {
                    if let Some((i, c)) = v.split_once('=') {
                        if let (Ok(i), Some(c)) = (i.trim().parse::<usize>(), rgb(c)) {
                            if i < 16 {
                                self.light_theme.ansi[i] = c;
                            }
                        }
                    }
                }
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
        self.light_theme.cursor_style = self.cursor_style;
    }

    /// The terminal colours in use: the light set under `theme = light`.
    pub fn colors(&self) -> Theme {
        if self.light { self.light_theme } else { self.theme }
    }
}

/// Sets `key = value` in the config file: the first uncommented line with that key, or a new
/// line at the end. For choices made in the UI (theme, accent), so they stick.
pub fn set(key: &str, value: &str) -> std::io::Result<()> {
    let path = path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut done = false;
    let mut out: Vec<String> = text
        .lines()
        .map(|l| {
            let t = l.trim_start();
            let is_key = !t.starts_with('#') && t.split_once('=').is_some_and(|(k, _)| k.trim() == key);
            if is_key && !done {
                done = true;
                keep_spacing(l, value)
            } else {
                l.to_string()
            }
        })
        .collect();
    if !done {
        out.push(format!("{key} = {value}"));
    }
    std::fs::write(&path, out.join("\n") + "\n")
}

/// Line `l` (`key = value`) with a new value, its key and the spacing around the `=` as they
/// were, so a column of aligned settings stays aligned.
fn keep_spacing(l: &str, value: &str) -> String {
    let eq = l.find('=').unwrap_or(l.len());
    let gap = l[eq + 1..].len() - l[eq + 1..].trim_start().len();
    format!("{}{}{value}", &l[..=eq.min(l.len() - 1)], &l[eq + 1..eq + 1 + gap])
}

/// Like `set`, but for a key that may repeat (`font-variation`): the first uncommented line
/// gets the value and the others with that key are dropped, so it's the only one.
pub fn replace(key: &str, value: &str) -> std::io::Result<()> {
    let path = path();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut done = false;
    let mut out: Vec<String> = vec![];
    for l in text.lines() {
        let t = l.trim_start();
        let is_key = !t.starts_with('#') && t.split_once('=').is_some_and(|(k, _)| k.trim() == key);
        if !is_key {
            out.push(l.to_string());
        } else if !done {
            done = true;
            out.push(keep_spacing(l, value));
        }
    }
    if !done {
        out.push(format!("{key} = {value}"));
    }
    std::fs::write(&path, out.join("\n") + "\n")
}

/// The keybind section of a new config: how to change keys, and every default.
fn keybind_section() -> String {
    format!(
        "# --- Keybinds ---\n\
         # keybind = CHORD=ACTION binds a key, CHORD=unbind frees it for the terminal, and\n\
         # keybind = clear drops every default. Modifiers: ctrl, alt, shift, super. ⌥P lists\n\
         # every action. These are the defaults:\n\
         {}",
        crate::keys::Keybinds::default_lines()
    )
}

pub fn hex_of(c: Rgb) -> String {
    hex(c)
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
        c.apply("sidebar = pinned\nsidebar-width = 340\nsidebar-rail-width = 9000\nsidebar-battery = false\nsidebar-background = #1a1b26\nlight-sidebar-background = #eceff4\n", Path::new("/"));
        assert_eq!(c.sidebar.background, Some(Rgb(0x1a, 0x1b, 0x26)));
        assert_eq!(c.sidebar.light_background, Some(Rgb(0xec, 0xef, 0xf4)));
        let back = {
            let mut b = Config::default();
            b.apply(&c.render(&[]), Path::new("/"));
            b
        };
        assert_eq!(back.sidebar, c.sidebar, "a written config keeps the colours");
        c.apply("sidebar-background =\n", Path::new("/"));
        assert_eq!(c.sidebar.background, None, "empty goes back to the default");
        assert!(c.sidebar.pinned);
        assert_eq!(c.sidebar.width, 340.0);
        assert_eq!(c.sidebar.rail, 140.0, "clamped");
        assert!(c.sidebar.clock && !c.sidebar.battery);
    }

    #[test]
    fn reads_appearance() {
        let mut c = Config::default();
        c.apply("theme = light\naccent = #ff0000\naccent-colors = #010203, #040506\nlight-background = #ffffff\nlight-palette = 1=#aa0000\n", Path::new("/"));
        assert!(c.light);
        assert_eq!(c.accent, Rgb(255, 0, 0));
        assert_eq!(c.accents, vec![Rgb(1, 2, 3), Rgb(4, 5, 6)]);
        assert_eq!(c.colors().bg, Rgb(255, 255, 255));
        assert_eq!(c.colors().ansi[1], Rgb(0xaa, 0, 0));
        c.apply("theme = dark\n", Path::new("/"));
        assert_eq!(c.colors().bg, c.theme.bg);
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
        c.apply("font-family = X\nfont-size = 19\nline-height = 30%\ncursor-opacity = 0\nbackground = #121212\nfont-style = Light\nfont-style-italic = false\nfont-family-bold = Y\nfont-feature = -calt, ss01\nfont-variation-bold = wght=650\nfont-synthetic-style = no-italic\n", Path::new("/"));
        let text = c.render(&["shaders/a.glsl".into()]);
        let mut back = Config::default();
        back.apply(&text, Path::new("/cfg"));
        assert_eq!(back.font.family, "X");
        assert_eq!(back.font.styles, c.font.styles);
        assert_eq!(back.font.styles[0], FontStyle::Named("Light".into()));
        assert_eq!(back.font.styles[2], FontStyle::Off);
        assert_eq!(back.font.families, [Some("Y".into()), None, None]);
        assert_eq!(back.font.features, c.font.features);
        assert_eq!(back.font.variations, c.font.variations);
        assert_eq!(back.font.synthetic, [true, false, true]);

        assert_eq!(back.font.size, 19.0);
        assert!((back.font.cell_height - 0.30).abs() < 1e-6);
        assert_eq!(back.theme.bg, Rgb(0x12, 0x12, 0x12));
        assert_eq!(back.theme.ansi, c.theme.ansi);
        assert_eq!(back.cursor_opacity, 0.0);
        assert_eq!(back.shaders, vec![PathBuf::from("/cfg/shaders/a.glsl")]);
        assert_eq!(back.sidebar, c.sidebar);
        assert_eq!((back.accent, back.accents.clone(), back.scrollback_editor.clone()), (c.accent, c.accents.clone(), c.scrollback_editor.clone()));
        assert_eq!(back.light, c.light);
    }

    #[test]
    fn a_saved_value_keeps_the_lines_alignment() {
        assert_eq!(keep_spacing("font-size     = 18", "21"), "font-size     = 21");
        assert_eq!(keep_spacing("padding=0", "4"), "padding=4");
        assert_eq!(keep_spacing("  theme = dark", "light"), "  theme = light");
    }

    #[test]
    fn reads_font_features_variations_and_synthesis() {
        let mut c = Config::default();
        c.apply("font-feature = -calt\nfont-feature = -liga, ss02\nfont-variation = wght=350\nfont-variation-bold = wght=650\nfont-synthetic-style = no-italic\n", Path::new("/"));
        assert_eq!(c.font.features, vec![("calt".into(), 0), ("liga".into(), 0), ("ss02".into(), 1)]);
        assert_eq!(c.font.variations[0], vec![("wght".into(), 350.0)]);
        assert_eq!(c.font.variations[1], vec![("wght".into(), 650.0)]);
        assert_eq!(c.font.synthetic, [true, false, true]);
        c.apply("font-feature =\n", Path::new("/"));
        assert!(c.font.features.is_empty(), "an empty line clears the list");
    }
}
