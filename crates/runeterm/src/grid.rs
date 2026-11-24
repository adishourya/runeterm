//! The terminal grid renderer: a Rune `Compositor` that draws Rune's frame and, at each pane's
//! effect slot, that pane's cells, glyphs and cursor with its own pipeline and glyph atlas.

use crate::config::{Config, ShaderAnimation};
use crate::shader;
use crate::state::State;
use crate::vt::{Cell, CursorStyle, Frame, Layer, Placement, RenderState, Rgb};
use bytemuck::{Pod, Zeroable};
use rune::compose::{Compositor, DeviceKey, Frame as RuneFrame};
use rune::render::{CLIP_WGSL, EffectSite};
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::scale::image::Content;
use swash::zeno::Format;
use swash::FontRef;

const ATLAS: u32 = 2048;
/// `smooth-cursor` glides over moves up to this far; longer ones jump.
const GLIDE_MAX_COLS: f32 = 12.0;
const GLIDE_MAX_ROWS: f32 = 2.0;

/// Where a pane's grid is, for turning pointer positions into cells: device pixels per point,
/// the padding and cell size in device pixels, and the grid's size.
#[derive(Clone, Copy, Debug, Default)]
pub struct Geometry {
    pub scale: f32,
    pub pad: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    pub cols: u16,
    pub rows: u16,
    pub width: f32,
    pub height: f32,
}

static GEOMETRY: std::sync::Mutex<Option<HashMap<u32, Geometry>>> = std::sync::Mutex::new(None);

/// A pane's grid geometry, as last drawn.
pub fn geometry(pane: u32) -> Option<Geometry> {
    GEOMETRY.lock().unwrap().as_ref()?.get(&pane).copied()
}

/// Font settings, in Rune points.
#[derive(Clone)]
pub struct FontSpec {
    pub family: String,
    pub size: f32,
    /// Extra line height, as a fraction (`line-height = 31%` is 0.31)…
    pub cell_height: f32,
    /// …or in pixels (`line-height = 4`).
    pub cell_height_px: f32,
    /// Space between the pane's edge and the grid.
    pub padding: f32,
    /// `font-size-fit`: how far (a fraction) the line height and font size may move so the
    /// lines fill the grid with nothing left under the last one; 0 keeps them exact.
    pub fit: f32,
    /// `font-family-bold`, `-italic`, `-bold-italic`: another family for that style.
    pub families: [Option<String>; 3],
    /// `font-style`, `-bold`, `-italic`, `-bold-italic`: the named face for regular, bold,
    /// italic and bold italic.
    pub styles: [FontStyle; 4],
    /// `font-feature`: OpenType features to turn on or off when shaping ("-calt", "ss01").
    pub features: Vec<(String, u16)>,
    /// `font-variation`, `-bold`, `-italic`, `-bold-italic`: variable font axes per style
    /// ("wght=450").
    pub variations: [Vec<(String, f32)>; 4],
    /// `font-synthetic-style`: whether bold, italic and bold italic may be made up (emboldened,
    /// slanted) when the family has no such face.
    pub synthetic: [bool; 3],
}

impl Default for FontSpec {
    fn default() -> Self {
        FontSpec {
            family: "monospace".into(),
            size: 13.0,
            cell_height: 0.0,
            cell_height_px: 0.0,
            padding: 10.0,
            fit: 0.0,
            families: Default::default(),
            styles: Default::default(),
            features: vec![],
            variations: Default::default(),
            synthetic: [true; 3],
        }
    }
}

/// One `font-feature` value: a comma-separated list of `-calt` (off), `+liga` or `liga` (on),
/// `ss01=2` or `ss01 2` (a value), `calt=off`. None for a tag that isn't four characters.
pub fn parse_features(v: &str) -> Option<Vec<(String, u16)>> {
    v.split(',').map(str::trim).filter(|f| !f.is_empty()).map(|f| {
        let f = f.trim_matches('"');
        let (tag, value) = if let Some(t) = f.strip_prefix('-') {
            (t, 0)
        } else if let Some(t) = f.strip_prefix('+') {
            (t, 1)
        } else if let Some((t, v)) = f.split_once(['=', ' ', ':']) {
            let v = v.trim();
            (t, match v { "on" | "true" => 1, "off" | "false" => 0, n => n.parse().ok()? })
        } else {
            (f, 1)
        };
        let tag = tag.trim().trim_matches('"');
        (tag.len() == 4 && tag.is_ascii()).then(|| (tag.to_string(), value))
    }).collect()
}

/// One `font-variation` value: `wght=450`, or several comma-separated.
pub fn parse_variations(v: &str) -> Option<Vec<(String, f32)>> {
    v.split(',').map(str::trim).filter(|f| !f.is_empty()).map(|f| {
        let (tag, value) = f.split_once(['=', ' ', ':'])?;
        let tag = tag.trim().trim_matches('"');
        (tag.len() == 4 && tag.is_ascii()).then_some(())?;
        Some((tag.to_string(), value.trim().parse().ok()?))
    }).collect()
}

/// `font-synthetic-style`: `true`, `false`, or a list like `no-bold,italic` changing the
/// defaults (all on). The order is bold, italic, bold italic.
pub fn parse_synthetic(v: &str) -> [bool; 3] {
    match v.trim() {
        "false" => return [false; 3],
        "true" | "" => return [true; 3],
        _ => {}
    }
    let mut out = [true; 3];
    for item in v.split(',').map(str::trim) {
        let (on, name) = match item.strip_prefix("no-") { Some(n) => (false, n), None => (true, item) };
        match name {
            "bold" => out[0] = on,
            "italic" => out[1] = on,
            "bold-italic" => out[2] = on,
            _ => {}
        }
    }
    out
}

fn tag(name: &str) -> swash::Tag {
    let b = name.as_bytes();
    swash::tag_from_bytes(&[b[0], b[1], b[2], b[3]])
}

/// Which face of a family a style uses (Ghostty's `font-style*`).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum FontStyle {
    /// The family's own: Regular, Bold, Italic, Bold Italic.
    #[default]
    Default,
    /// A face by its style name: "Light", "Medium Italic".
    Named(String),
    /// `false`: none; the regular face draws it.
    Off,
}

impl FontStyle {
    pub fn parse(v: &str) -> FontStyle {
        match v.trim() {
            "" | "default" => FontStyle::Default,
            "false" => FontStyle::Off,
            s => FontStyle::Named(s.to_string()),
        }
    }
}

/// Face `i` (0 regular, 1 bold, 2 italic, 3 bold italic) as a fontconfig pattern: its family
/// (the style's own or `font-family`) with the style named, or the family's default one. None
/// when the style is off.
pub fn font_pattern(spec: &FontSpec, i: usize) -> Option<String> {
    let family = i.checked_sub(1).and_then(|j| spec.families[j].as_deref()).unwrap_or(&spec.family);
    match &spec.styles[i] {
        FontStyle::Off if i > 0 => None,
        FontStyle::Named(style) => Some(format!("{family}:style={style}")),
        _ => Some(default_pattern(family, i)),
    }
}

/// The family face `i` is drawn from: the style's own or `font-family`.
fn family_of(spec: &FontSpec, i: usize) -> &str {
    i.checked_sub(1).and_then(|j| spec.families[j].as_deref()).unwrap_or(&spec.family)
}

/// The variable font file of `family`, if one is installed. Fontconfig prefers a family's
/// static faces, and with `variable=true` gives some variable font even of another family, so
/// its answer is kept only when it's this family (and a format swash reads).
fn variable_file(family: &str) -> Option<String> {
    let out = fc_match(&format!("{family}:variable=true"), "%{variable}\t%{file}\t%{family}")?;
    let mut parts = out.splitn(3, '\t');
    let (variable, file, families) = (parts.next()?, parts.next()?, parts.next()?);
    let readable = !file.ends_with(".woff") && !file.ends_with(".woff2");
    (variable == "True" && readable && has_style(families, family)).then(|| file.to_string())
}

fn default_pattern(family: &str, i: usize) -> String {
    match i {
        1 => format!("{family}:bold"),
        2 => format!("{family}:italic"),
        3 => format!("{family}:bold:italic"),
        _ => family.to_string(),
    }
}

/// Axis `name` (`wght`, `wdth`) of style `i`'s face (0 regular, 1 bold) as (min, default,
/// max), from the family's variable file; None when it has no such axis (or the style is off).
pub fn font_axis(spec: &FontSpec, i: usize, name: &str) -> Option<(f32, f32, f32)> {
    let pattern = font_pattern(spec, i)?;
    let path = variable_file(family_of(spec, i)).or_else(|| fc_match(&pattern, "%{file}"))?;
    let data = std::fs::read(path).ok()?;
    let font = FontRef::from_index(&data, 0)?;
    let v = font.variations().find_by_tag(tag(name))?;
    Some((v.min_value(), v.default_value(), v.max_value()))
}

/// `vs` (one style's `font-variation` axes) with axis `name` set to `v`, other axes kept.
pub fn with_axis(vs: &[(String, f32)], name: &str, v: f32) -> Vec<(String, f32)> {
    let mut out = vs.to_vec();
    match out.iter_mut().find(|(t, _)| t == name) {
        Some(a) => a.1 = v,
        None => out.push((name.into(), v)),
    }
    out
}

/// The width every style is drawn at, since they share the cells: regular's `wdth`, or the
/// first style's that sets one.
pub fn shared_width(vs: &[Vec<(String, f32)>; 4]) -> Option<f32> {
    vs.iter().find_map(|s| s.iter().find(|(t, _)| t == "wdth").map(|(_, v)| *v))
}

/// Each style's axes as drawn: the shared width for all, and regular's other axes (a slant)
/// added to the styles that don't set them. Weights stay each style's own.
fn inherit_axes(vs: &[Vec<(String, f32)>; 4]) -> [Vec<(String, f32)>; 4] {
    let width = shared_width(vs);
    let mut out = vs.clone();
    for (i, style) in out.iter_mut().enumerate() {
        if i > 0 {
            for (t, v) in vs[0].iter().filter(|(t, _)| t != "wght" && t != "wdth") {
                if !style.iter().any(|(s, _)| s == t) {
                    style.push((t.clone(), *v));
                }
            }
        }
        if let Some(w) = width {
            *style = with_axis(style, "wdth", w);
        }
    }
    out
}

/// The cell size that fills a grid `target` device pixels tall best: a font size (px) and a
/// cell height, each within `pct` of the configured `base_px` and the cell height `line` (a
/// fraction) and `line_px` give at that size. `natural(px)` is the font's own line height at
/// `px`. The smallest strip left under the last line wins; between equal strips, the smaller
/// change, a font size change counting three times a line height change (so the letters keep
/// their size when spacing alone does it).
pub fn fit(target: f32, base_px: f32, line: f32, line_px: f32, pct: f32, natural: impl Fn(f32) -> f32) -> (f32, f32) {
    let cell = |px: f32| natural(px) * (1.0 + line) + line_px;
    let base_h = cell(base_px).round().max(1.0);
    let mut best = (base_px, base_h);
    if pct <= 0.0 || target < base_h {
        return best;
    }
    let left = |h: f32| target - (target / h).floor() * h;
    let cost = |px: f32, h: f32| 3.0 * (px / base_px - 1.0).abs() + (h / cell(px) - 1.0).abs();
    let mut best_key = (left(base_h), cost(base_px, base_h));
    let px_lo = (base_px * (1.0 - pct)).ceil().max(1.0) as i32;
    let px_hi = (base_px * (1.0 + pct)).floor() as i32;
    for px in px_lo..=px_hi {
        let px = px as f32;
        let own = cell(px);
        let lo = (own * (1.0 - pct)).ceil().max(natural(px).ceil()).max(1.0) as i32;
        let hi = (own * (1.0 + pct)).floor() as i32;
        for h in lo..=hi {
            let h = h as f32;
            let key = (left(h), cost(px, h));
            // Half a pixel of strip is nothing; then the smaller change.
            let better = key.0 + 0.5 < best_key.0 || ((key.0 - best_key.0).abs() <= 0.5 && key.1 < best_key.1);
            if better {
                best = (px, h);
                best_key = key;
            }
        }
    }
    best
}

/// The fit in use, for the UI: the font size (points) and the line height's change (percent).
static FITTED: std::sync::Mutex<Option<(f32, f32)>> = std::sync::Mutex::new(None);

pub fn fitted() -> Option<(f32, f32)> {
    *FITTED.lock().unwrap()
}

/// Regular, bold, italic and bold italic, then fallbacks found per character (emoji, CJK,
/// symbols the font lacks), by asking fontconfig which installed font covers it.
struct Fonts {
    data: [Option<Box<[u8]>>; 4],
    /// What each style's face lacks and is made up when drawn: (embolden, slant).
    synth: [(bool, bool); 4],
    features: Vec<swash::Setting<u16>>,
    variations: [Vec<swash::Setting<f32>>; 4],
    extra: Vec<Box<[u8]>>,
    paths: HashMap<String, usize>,
    by_char: HashMap<char, Option<usize>>,
    /// `resolve`'s answers, by character and style.
    resolved: HashMap<(char, usize), Option<(usize, u16)>>,
}

fn read_font(pattern: &str) -> Option<(String, Box<[u8]>)> {
    let path = fc_match(pattern, "%{file}")?;
    let data = std::fs::read(&path).ok()?.into_boxed_slice();
    Some((path, data))
}

/// What fontconfig says about the best match for `pattern`, in `format`.
fn fc_match(pattern: &str, format: &str) -> Option<String> {
    let out = std::process::Command::new("fc-match").args(["-f", format, pattern]).output().ok()?;
    Some(String::from_utf8(out.stdout).ok()?.trim().to_string())
}

/// Whether fontconfig's styles for a face (`Light,Regular`) include `want`, ignoring case.
fn has_style(styles: &str, want: &str) -> bool {
    styles.split(',').any(|s| s.trim().eq_ignore_ascii_case(want.trim()))
}

impl Fonts {
    fn load(spec: &FontSpec) -> Fonts {
        let mut data: [Option<Box<[u8]>>; 4] = Default::default();
        let mut synth = [(false, false); 4];
        let mut read: HashMap<String, usize> = HashMap::new();
        let mut axes = inherit_axes(&spec.variations);
        for i in 0..4 {
            let Some(mut pattern) = font_pattern(spec, i) else { continue };
            // A named style the family doesn't have would get fontconfig's nearest face, which
            // can be anything: say so and use the family's own style instead.
            if let FontStyle::Named(want) = &spec.styles[i] {
                let got = fc_match(&pattern, "%{style}").unwrap_or_default();
                if !has_style(&got, want) {
                    let family = pattern.split(":style=").next().unwrap_or_default().to_string();
                    eprintln!("runeterm: no \"{want}\" style in {family}; using its default");
                    pattern = default_pattern(&family, i);
                }
            }
            // Axes for the style need the family's variable file, which fontconfig passes over
            // for its static faces: the style's own (`font-variation = wght=…`), or the width
            // all styles share when the variable file has a width axis.
            let own = !spec.variations[i].is_empty();
            let width = axes[i].iter().any(|(t, _)| t == "wdth");
            if let Some(path) = (own || width).then(|| variable_file(family_of(spec, i))).flatten() {
                let vf = match read.get(&path) {
                    Some(&j) => data[j].clone(),
                    None => std::fs::read(&path).ok().map(Vec::into_boxed_slice),
                };
                let has = |t: &str| vf.as_deref().and_then(|d| FontRef::from_index(d, 0)).is_some_and(|f| f.variations().find_by_tag(tag(t)).is_some());
                if own || has("wdth") {
                    // With no weight of its own, the style keeps the one its static face has
                    // (Light, the family's Bold) rather than the variable font's default.
                    if !axes[i].iter().any(|(t, _)| t == "wght") && has("wght") {
                        let face = read_font(&pattern).and_then(|(_, d)| FontRef::from_index(&d, 0).map(|f| f.attributes().weight().0));
                        if let Some(w) = face {
                            axes[i].insert(0, ("wght".into(), w as f32));
                        }
                    }
                    data[i] = vf;
                    read.entry(path).or_insert(i);
                    continue;
                }
            }
            // Faces that come out the same are read once.
            if let Some(&j) = read.get(&pattern) {
                data[i] = data[j].clone();
                continue;
            }
            data[i] = read_font(&pattern).map(|(_, d)| d);
            read.insert(pattern, i);
        }
        // What fontconfig found, against what the style wants (read from the font files, since
        // fontconfig reports the weight it was asked for): a family with no bold (or italic)
        // face gets the nearest one, made bold (or slanted) when drawn, as Ghostty does.
        let attrs = |d: &Option<Box<[u8]>>| d.as_deref().and_then(|d| FontRef::from_index(d, 0)).map(|f| f.attributes());
        let wght = |i: usize| axes[i].iter().find(|(t, _)| t == "wght").map(|(_, v)| *v as u16);
        let regular = wght(0).or(attrs(&data[0]).map(|a| a.weight().0)).unwrap_or(400);
        for i in 1..4 {
            let Some(a) = attrs(&data[i]) else { continue };
            if matches!(spec.styles[i], FontStyle::Default) && spec.synthetic[i - 1] {
                let (bold, italic) = (i & 1 != 0, i & 2 != 0);
                // A bold drawn at a weight of its own (`font-variation-bold = wght=…`) is drawn
                // at it, made up only if that isn't heavier.
                let weight = wght(i).unwrap_or(a.weight().0);
                synth[i] = (bold && weight <= regular, italic && a.style() == swash::Style::Normal);
            }
        }
        let features = spec.features.iter().map(|(t, v)| swash::Setting { tag: tag(t), value: *v }).collect();
        let variations = axes.map(|vs| vs.iter().map(|(t, v)| swash::Setting { tag: tag(t), value: *v }).collect());
        Fonts { data, synth, features, variations, extra: vec![], paths: HashMap::new(), by_char: HashMap::new(), resolved: HashMap::new() }
    }

    /// The variable font axes for font `i` (styles only; fallbacks keep their defaults).
    fn variations(&self, i: usize) -> &[swash::Setting<f32>] {
        self.variations.get(i).map_or(&[], |v| v.as_slice())
    }

    /// Style `i`'s axes as normalized coordinates, for measuring it as it's drawn.
    fn coords(&self, i: usize) -> Vec<swash::NormalizedCoord> {
        match self.get(i) {
            Some(font) => font.variations().normalized_coords(self.variations(i).iter().copied()).collect(),
            None => vec![],
        }
    }

    fn get(&self, style: usize) -> Option<FontRef<'_>> {
        let d = self.data[style].as_deref().or(self.data[0].as_deref())?;
        FontRef::from_index(d, 0)
    }

    /// A font with a glyph for `ch`: the styled one, the regular one, or a fallback.
    /// Returns a font index for `font` (0–3 styles, 4+ fallbacks) and the glyph id.
    fn resolve(&mut self, ch: char, style: usize) -> Option<(usize, u16)> {
        if let Some(r) = self.resolved.get(&(ch, style)) {
            return *r;
        }
        let r = self.resolve_uncached(ch, style);
        self.resolved.insert((ch, style), r);
        r
    }

    fn resolve_uncached(&mut self, ch: char, style: usize) -> Option<(usize, u16)> {
        for i in [style, 0] {
            if let Some(f) = self.get(i) {
                let gid = f.charmap().map(ch);
                if gid != 0 {
                    return Some((i, gid));
                }
            }
        }
        let fallback = match self.by_char.get(&ch) {
            Some(f) => *f,
            None => {
                let found = self.find_fallback(ch);
                self.by_char.insert(ch, found);
                found
            }
        }?;
        let gid = FontRef::from_index(&self.extra[fallback], 0)?.charmap().map(ch);
        (gid != 0).then_some((4 + fallback, gid))
    }

    fn find_fallback(&mut self, ch: char) -> Option<usize> {
        // Emoji first ask for a colour font.
        let emoji = (ch as u32) >= 0x1F000 || matches!(ch as u32, 0x2600..=0x27BF | 0x2B00..=0x2BFF);
        let pattern = if emoji { format!(":charset={:x}:color=true", ch as u32) } else { format!(":charset={:x}", ch as u32) };
        let (path, data) = read_font(&pattern)?;
        let i = match self.paths.get(&path) {
            Some(&i) => i,
            None => {
                self.extra.push(data);
                self.paths.insert(path, self.extra.len() - 1);
                self.extra.len() - 1
            }
        };
        let gid = FontRef::from_index(&self.extra[i], 0)?.charmap().map(ch);
        (gid != 0).then_some(i)
    }

    fn font(&self, i: usize) -> Option<FontRef<'_>> {
        if i < 4 { self.get(i) } else { FontRef::from_index(self.extra.get(i - 4)?, 0) }
    }
}

/// Cell size and baseline in device pixels.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Metrics {
    pub w: f32,
    pub h: f32,
    baseline: f32,
    px: f32,
}

#[derive(Clone, Copy)]
struct Slot {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    left: i32,
    top: i32,
    /// In the colour (RGBA) atlas: an emoji, drawn scaled into its cells.
    color: bool,
}

/// One shelf packer over an `ATLAS`-sized texture.
#[derive(Default)]
struct Shelves {
    pen: (u32, u32),
    shelf: u32,
}

impl Shelves {
    fn place(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if self.pen == (0, 0) {
            self.pen = (1, 1);
        }
        if self.pen.0 + w + 1 > ATLAS {
            self.pen = (1, self.pen.1 + self.shelf + 1);
            self.shelf = 0;
        }
        if self.pen.1 + h + 1 > ATLAS || w + 2 > ATLAS {
            return None; // Full. A terminal rarely gets here; a clear-and-refill comes later.
        }
        let at = self.pen;
        self.pen.0 += w + 1;
        self.shelf = self.shelf.max(h);
        Some(at)
    }
}

/// What the atlas holds: a character drawn by `sprite` (box drawing, blocks, braille,
/// powerline), or a font's glyph (0–3 the styles, 4+ fallbacks).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum GlyphKey {
    Sprite(char),
    Glyph { font: u16, id: u16 },
}

/// What one cell draws.
#[derive(Clone, Debug, PartialEq)]
enum Draw {
    /// Its character on its own (sprites, emoji, fallback fonts, blanks).
    Char,
    /// One shaped glyph (the usual case): font, glyph, and offset from the cell's left edge and
    /// baseline (px).
    One(usize, u16, f32, f32),
    /// Several shaped glyphs, as `One`.
    Glyphs(Vec<(usize, u16, f32, f32)>),
    /// Part of a ligature an earlier cell draws.
    Covered,
}

/// Characters `sprite::draw` may draw itself (it says no to the rest of these ranges).
fn maybe_sprite(c: char) -> bool {
    matches!(c as u32, 0x2500..=0x259F | 0x2800..=0x28FF | 0xE0B0..=0xE0BF)
}

/// Glyphs rasterized into textures packed on shelves: coverage (R8) for text, and RGBA for
/// colour glyphs (emoji).
struct Atlas {
    glyphs: HashMap<GlyphKey, Option<Slot>>,
    /// Shaped runs by font and text: what each of the run's cells draws.
    runs: HashMap<(usize, String), Vec<Draw>>,
    mask: Shelves,
    color: Shelves,
    uploads: Vec<(Slot, Vec<u8>)>,
}

impl Atlas {
    fn new() -> Atlas {
        Atlas { glyphs: HashMap::new(), runs: HashMap::new(), mask: Shelves::default(), color: Shelves::default(), uploads: vec![] }
    }

    /// Finds room for a `w` × `h` bitmap and queues its upload.
    fn pack(&mut self, w: u32, h: u32, left: i32, top: i32, data: Vec<u8>, color: bool) -> Option<Slot> {
        if w == 0 || h == 0 {
            return None;
        }
        let (x, y) = if color { self.color.place(w, h)? } else { self.mask.place(w, h)? };
        let s = Slot { x, y, w, h, left, top, color };
        self.uploads.push((s, data));
        Some(s)
    }

    /// A cell's character in style `style` (bold 1, italic 2), unshaped: drawn as a sprite, or
    /// the glyph of whichever font has it.
    fn char(&mut self, ctx: &mut ScaleContext, fonts: &mut Fonts, m: &Metrics, ch: char, style: u8) -> Option<Slot> {
        if maybe_sprite(ch) {
            let key = GlyphKey::Sprite(ch);
            if let Some(s) = self.glyphs.get(&key) {
                if s.is_some() {
                    return *s;
                }
            } else if let Some(sp) = crate::sprite::draw(ch, m.w as u32, m.h as u32, (m.px / 16.0).round().max(1.0)) {
                // Drawn by us at exactly cell size; its top is the cell's top.
                let slot = self.pack(sp.w, sp.h, 0, m.baseline as i32, sp.data, false);
                self.glyphs.insert(key, slot);
                return slot;
            } else {
                self.glyphs.insert(key, None);
            }
        }
        let (fi, gid) = fonts.resolve(ch, style as usize)?;
        self.glyph(ctx, fonts, m, fi, gid)
    }

    /// Glyph `gid` of font `fi`, rasterized once: in its style's variation, and emboldened or
    /// slanted where the family had no such face.
    fn glyph(&mut self, ctx: &mut ScaleContext, fonts: &Fonts, m: &Metrics, fi: usize, gid: u16) -> Option<Slot> {
        let key = GlyphKey::Glyph { font: fi as u16, id: gid };
        if let Some(s) = self.glyphs.get(&key) {
            return *s;
        }
        let slot = (|| {
            let font = fonts.font(fi)?;
            let mut scaler = ctx.builder(font).size(m.px).hint(true).variations(fonts.variations(fi).iter().copied()).build();
            if scaler.has_color_bitmaps() || scaler.has_color_outlines() {
                // Colour emoji: an RGBA image, scaled into its cells when drawn.
                let img = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline]).render(&mut scaler, gid)?;
                let p = img.placement;
                return self.pack(p.width, p.height, p.left, p.top, img.data, img.content == Content::Color);
            }
            let (bold, italic) = fonts.synth.get(fi).copied().unwrap_or_default();
            let mut render = Render::new(&[Source::Outline]);
            render.format(Format::Alpha);
            if bold {
                render.embolden(synthetic_bold(m.px));
            }
            if italic {
                render.transform(Some(swash::zeno::Transform::skew(swash::zeno::Angle::from_degrees(SYNTHETIC_SLANT), swash::zeno::Angle::ZERO)));
            }
            let img = render.render(&mut scaler, gid)?;
            let p = img.placement;
            self.pack(p.width, p.height, p.left, p.top, img.data, false)
        })();
        self.glyphs.insert(key, slot);
        slot
    }
}

/// How far a made-up bold thickens each side of a stroke, in pixels at `px`: about a quarter
/// more ink, as a real bold face has over its regular.
fn synthetic_bold(px: f32) -> f32 {
    (px / 110.0).max(0.2)
}

/// How far a made-up italic leans, in degrees (Ghostty's, about 12°).
const SYNTHETIC_SLANT: f32 = 12.0;

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Default)]
struct Inst {
    pos: [f32; 2],
    size: [f32; 2],
    uv: [f32; 2],
    color: [f32; 4],
    kind: u32,
    _pad: u32,
    /// Images only: the source rectangle's size, in normalized texture coordinates.
    uv_size: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Uniforms {
    origin: [f32; 4],
    target: [f32; 4],
    clip: [f32; 4],
    fade: [u32; 4],
    flags: [u32; 4],
}

fn rgba(c: Rgb, a: f32) -> [f32; 4] {
    [c.0 as f32 / 255.0, c.1 as f32 / 255.0, c.2 as f32 / 255.0, a]
}

/// A row's instances: backgrounds first, then glyphs and decorations.
#[derive(Default, Clone)]
struct Row {
    bg: Vec<Inst>,
    fg: Vec<Inst>,
}

/// Where a pane's slot was last drawn.
#[derive(Clone, Copy)]
struct Site {
    rect: [f32; 4],
    scale: f32,
}

/// A pane's offscreen targets for its shader chain: the grid goes into `tex[0]`, and each
/// shader reads one and writes the other.
struct Offscreen {
    size: (u32, u32),
    views: [wgpu::TextureView; 2],
    /// Shader bind groups reading `views[i]`.
    reads: [wgpu::BindGroup; 2],
    /// Image-slot bind groups showing `views[i]` in the pane.
    shows: [wgpu::BindGroup; 2],
    globals: wgpu::Buffer,
    /// Which view holds the result after the chain.
    result: usize,
}

/// GPU and cached state for one pane.
struct PaneGpu {
    rs: RenderState,
    rows: Vec<Row>,
    colors: (Rgb, Rgb),
    /// Cell backgrounds then text, rebuilt when rows change.
    body: Vec<Inst>,
    body_bg: usize,
    /// While scrolling smoothly: the row above the viewport (drawn at the top of the grid, a
    /// line up), read through a render state of its own.
    above_rs: RenderState,
    above: Option<Row>,
    /// Where this frame's backgrounds end in `instances`.
    bg_end: u32,
    placements: Vec<Placement>,
    /// Pixels read under the terminal lock, waiting for a texture.
    pending: Vec<(u32, u64, u32, u32, Vec<u8>)>,
    /// This frame's draw list: body, cursor, then images.
    instances: Vec<Inst>,
    text_end: u32,
    image_draws: Vec<(Layer, u32, u32)>,
    /// The grid size the terminal has, and a size waiting for the layout to settle.
    grid: Option<(u16, u16)>,
    pending_grid: Option<((u16, u16), f64)>,
    /// Cursor: what the terminal says, where we draw it (gliding), and for shaders where it was.
    cursor: Option<crate::vt::Cursor>,
    cursor_rect: [f32; 4],
    cursor_drawn: [f32; 4],
    cursor_prev: [f32; 4],
    cursor_style: i32,
    cursor_prev_style: i32,
    cursor_changed: f64,
    site: Option<Site>,
    prepared: bool,
    buf: wgpu::Buffer,
    cap: usize,
    /// Uniforms and bind group for drawing into the window, and for drawing offscreen.
    slot_u: wgpu::Buffer,
    slot_bind: wgpu::BindGroup,
    off_u: wgpu::Buffer,
    off_bind: wgpu::BindGroup,
    offscreen: Option<Offscreen>,
}

/// One image's texture, made for a generation of its pixels.
struct ImageTex {
    generation: u64,
    size: (u32, u32),
    bind: wgpu::BindGroup,
}

struct Gpu {
    key: DeviceKey,
    bgl: wgpu::BindGroupLayout,
    image_bgl: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    /// Bound for draws without an image, as the layout always has an image slot.
    no_image: wgpu::BindGroup,
    layout: wgpu::PipelineLayout,
    module: wgpu::ShaderModule,
    pipelines: HashMap<wgpu::TextureFormat, wgpu::RenderPipeline>,
    atlas_tex: wgpu::Texture,
    atlas_view: wgpu::TextureView,
    color_tex: wgpu::Texture,
    color_view: wgpu::TextureView,
    chain: Option<shader::Chain>,
}

/// A texture holding RGBA pixels, bound for the image slot.
fn image_bind_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    sampler: &wgpu::Sampler,
    queue: Option<&wgpu::Queue>,
    w: u32,
    h: u32,
    rgba: &[u8],
) -> wgpu::BindGroup {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("runeterm image"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    if let Some(queue) = queue {
        queue.write_texture(
            tex.as_image_copy(),
            rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * 4), rows_per_image: None },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
    }
    let view = tex.create_view(&Default::default());
    view_bind_group(device, layout, sampler, &view)
}

fn view_bind_group(device: &wgpu::Device, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, view: &wgpu::TextureView) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("runeterm image"),
        layout,
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) },
            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) },
        ],
    })
}

fn uniform_buffer(device: &wgpu::Device, size: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("runeterm uniforms"),
        size: size as u64,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl Gpu {
    fn new(device: &wgpu::Device, key: DeviceKey, shaders: &[String]) -> Gpu {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("runeterm grid"),
            source: wgpu::ShaderSource::Wgsl(format!("{CLIP_WGSL}\n{}", include_str!("grid.wgsl")).into()),
        });
        let entry = |binding, ty| wgpu::BindGroupLayoutEntry { binding, visibility: wgpu::ShaderStages::VERTEX_FRAGMENT, ty, count: None };
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("runeterm grid"),
            entries: &[
                entry(0, wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None }),
                entry(1, wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                }),
                entry(2, wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                }),
                entry(3, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let image_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("runeterm image"),
            entries: &[
                entry(0, wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                }),
                entry(1, wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering)),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("runeterm grid"),
            bind_group_layouts: &[Some(&bgl), Some(&image_bgl)],
            immediate_size: 0,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        let atlas_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("runeterm atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let atlas_view = atlas_tex.create_view(&Default::default());
        let color_tex = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("runeterm colour atlas"),
            size: wgpu::Extent3d { width: ATLAS, height: ATLAS, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let color_view = color_tex.create_view(&Default::default());
        let no_image = image_bind_group(device, &image_bgl, &sampler, None, 1, 1, &[0; 4]);
        let chain = (!shaders.is_empty()).then(|| shader::Chain::new(device, shaders));
        Gpu { key, bgl, image_bgl, sampler, no_image, layout, module, pipelines: HashMap::new(), atlas_tex, atlas_view, color_tex, color_view, chain }
    }

    fn image(&self, device: &wgpu::Device, queue: &wgpu::Queue, w: u32, h: u32, rgba: &[u8]) -> wgpu::BindGroup {
        image_bind_group(device, &self.image_bgl, &self.sampler, Some(queue), w, h, rgba)
    }

    fn pipeline(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) -> &wgpu::RenderPipeline {
        let (layout, module) = (&self.layout, &self.module);
        self.pipelines.entry(format).or_insert_with(|| {
            let attrs = wgpu::vertex_attr_array![0 => Float32x2, 1 => Float32x2, 2 => Float32x2, 3 => Float32x4, 4 => Uint32, 5 => Uint32, 6 => Float32x2];
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("runeterm grid"),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module,
                    entry_point: Some("vs"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout {
                        array_stride: std::mem::size_of::<Inst>() as u64,
                        step_mode: wgpu::VertexStepMode::Instance,
                        attributes: &attrs,
                    })],
                },
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                fragment: Some(wgpu::FragmentState {
                    module,
                    entry_point: Some("fs"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend: Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                multiview_mask: None,
                cache: None,
            })
        })
    }

    fn grid_bind(&self, device: &wgpu::Device, u: &wgpu::Buffer) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("runeterm grid"),
            layout: &self.bgl,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: u.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.atlas_view) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&self.color_view) },
                wgpu::BindGroupEntry { binding: 3, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        })
    }

    fn pane(&self, device: &wgpu::Device) -> PaneGpu {
        let cap = 4096;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("runeterm cells"),
            size: (cap * std::mem::size_of::<Inst>()) as u64,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let slot_u = uniform_buffer(device, std::mem::size_of::<Uniforms>());
        let off_u = uniform_buffer(device, std::mem::size_of::<Uniforms>());
        PaneGpu {
            rs: RenderState::new(),
            rows: vec![],
            colors: (Rgb(255, 255, 255), Rgb(0, 0, 0)),
            body: vec![],
            body_bg: 0,
            above_rs: RenderState::new(),
            above: None,
            bg_end: 0,
            placements: vec![],
            pending: vec![],
            instances: vec![],
            text_end: 0,
            image_draws: vec![],
            grid: None,
            pending_grid: None,
            cursor: None,
            cursor_rect: [0.0; 4],
            cursor_drawn: [0.0; 4],
            cursor_prev: [0.0; 4],
            cursor_style: 2,
            cursor_prev_style: 2,
            cursor_changed: 0.0,
            site: None,
            prepared: false,
            slot_bind: self.grid_bind(device, &slot_u),
            off_bind: self.grid_bind(device, &off_u),
            buf,
            cap,
            slot_u,
            off_u,
            offscreen: None,
        }
    }

    /// Offscreen targets of `size` for a pane's shader chain.
    fn offscreen(&self, device: &wgpu::Device, size: (u32, u32)) -> Offscreen {
        let chain = self.chain.as_ref().unwrap();
        let globals = uniform_buffer(device, std::mem::size_of::<shader::Globals>());
        let make = || {
            device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("runeterm offscreen"),
                    size: wgpu::Extent3d { width: size.0.max(1), height: size.1.max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: shader::FORMAT,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::RENDER_ATTACHMENT,
                    view_formats: &[],
                })
                .create_view(&Default::default())
        };
        let views = [make(), make()];
        let read = |v: &wgpu::TextureView| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("runeterm shader input"),
                layout: &chain.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: globals.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(v) },
                    wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&chain.sampler) },
                ],
            })
        };
        let reads = [read(&views[0]), read(&views[1])];
        let shows = [
            view_bind_group(device, &self.image_bgl, &self.sampler, &views[0]),
            view_bind_group(device, &self.image_bgl, &self.sampler, &views[1]),
        ];
        Offscreen { size, views, reads, shows, globals, result: 0 }
    }
}

/// Draws Rune's frame plus every pane's grid.
pub struct GridRenderer {
    spec: FontSpec,
    fonts: Fonts,
    ctx: ScaleContext,
    shaper: swash::shape::ShapeContext,
    metrics: Option<Metrics>,
    atlas: Atlas,
    gpu: Option<Gpu>,
    panes: HashMap<u32, PaneGpu>,
    /// Image textures by (pane, image id).
    textures: HashMap<(u32, u32), ImageTex>,
    theme: crate::pane::Theme,
    /// Selections are tinted with it.
    accent: Rgb,
    cursor_opacity: f32,
    cursor_smooth: bool,
    shaders: Vec<String>,
    shader_animation: ShaderAnimation,
    /// The config generation applied (see `config::publish`).
    generation: u64,
    /// Seconds since the first frame.
    time: f64,
    frame: i32,
    /// Something is still moving: keep frames coming.
    moving: bool,
    /// The panes drawn last frame: the ones to update (and run shaders for) before the next.
    last_seen: Vec<u32>,
    /// `font-size-fit`: the tallest grid's height and since when it's been that, and the fit
    /// made for a height: (height, font px, cell height).
    fit_seen: Option<(f32, f64)>,
    fitted: Option<(f32, f32, f32)>,
}

impl GridRenderer {
    pub fn new(config: &Config) -> GridRenderer {
        let fonts = Fonts::load(&config.font);
        let shaders = config.shaders.iter().filter_map(|p| shader::load(p)).collect();
        GridRenderer {
            generation: crate::config::generation(),
            spec: config.font.clone(),
            fonts,
            ctx: ScaleContext::new(),
            shaper: swash::shape::ShapeContext::new(),
            metrics: None,
            atlas: Atlas::new(),
            gpu: None,
            panes: HashMap::new(),
            textures: HashMap::new(),
            theme: config.colors(),
            accent: config.accent,
            cursor_opacity: config.cursor_opacity,
            cursor_smooth: config.cursor_smooth,
            shaders,
            shader_animation: config.shader_animation,
            time: 0.0,
            frame: 0,
            moving: true,
            last_seen: vec![],
            fit_seen: None,
            fitted: None,
        }
    }

    /// Takes a reloaded config: new font, colours, cursor and shaders. Everything GPU-side is
    /// rebuilt on the next frame, and every pane redraws (and resizes, if the cell size changed).
    fn reload(&mut self, config: &Config) {
        self.spec = config.font.clone();
        self.fonts = Fonts::load(&config.font);
        self.metrics = None;
        self.fitted = None;
        self.fit_seen = None;
        self.atlas = Atlas::new();
        self.theme = config.colors();
        self.accent = config.accent;
        self.cursor_opacity = config.cursor_opacity;
        self.cursor_smooth = config.cursor_smooth;
        self.shaders = config.shaders.iter().filter_map(|p| shader::load(p)).collect();
        self.shader_animation = config.shader_animation;
        self.gpu = None;
        self.panes.clear();
        self.textures.clear();
    }

    /// The configured font size in device pixels at `scale`. Ghostty sizes fonts in points at
    /// 96 dpi.
    fn base_px(&self, scale: f32) -> f32 {
        (self.spec.size * 96.0 / 72.0 * scale).round()
    }

    /// The font's own line height (ascent, descent, leading) at `px`.
    fn natural(&self, px: f32) -> f32 {
        match self.fonts.get(0) {
            Some(font) => {
                let m = font.metrics(&self.fonts.coords(0)).scale(px);
                m.ascent + m.descent + m.leading
            }
            None => px * 1.3,
        }
    }

    /// The font size and cell height to draw at: the configured ones, or the fit for the
    /// tallest grid when `font-size-fit` is on.
    fn cell_size(&self, scale: f32) -> (f32, f32) {
        if let Some((_, px, h)) = self.fitted.filter(|_| self.spec.fit > 0.0) {
            return (px, h);
        }
        let px = self.base_px(scale);
        let h = (self.natural(px) * (1.0 + self.spec.cell_height) + self.spec.cell_height_px * scale).round().max(1.0);
        (px, h)
    }

    /// Cell metrics for a font size and cell height, both in device pixels.
    fn metrics_with(&self, px: f32, h: f32) -> Metrics {
        let Some(font) = self.fonts.get(0) else {
            return Metrics { w: (px * 0.6).round(), h, baseline: px, px };
        };
        // Measured with the regular style's axes: a narrower width (wdth) makes narrower cells.
        let coords = self.fonts.coords(0);
        let m = font.metrics(&coords).scale(px);
        let gid = font.charmap().map('M');
        let w = font.glyph_metrics(&coords).scale(px).advance_width(gid).round().max(1.0);
        let baseline = ((h - (m.ascent + m.descent)) / 2.0 + m.ascent).round();
        Metrics { w, h, baseline, px }
    }

    /// The height of text a column of the shown tab holds (device px): a split's own, splits
    /// stacked one above another added up, the tallest of splits side by side. Moving a divider
    /// doesn't change it, so once the dividers snap to whole lines, every split in the column
    /// comes out whole.
    fn column_height(&self, s: &State) -> Option<(f32, f32)> {
        let tab = s.tab()?;
        let slot = |id: u32| {
            let site = self.panes.get(&id).filter(|_| self.last_seen.contains(&id))?.site?;
            Some((site.rect[3] - 2.0 * (self.spec.padding * site.scale).round(), site.scale))
        };
        fn walk(n: &crate::split::Node, slot: &dyn Fn(u32) -> Option<(f32, f32)>) -> Option<(f32, f32)> {
            use crate::split::{Dir, Node};
            match n {
                Node::Leaf(id) => slot(*id),
                Node::Split { dir: Dir::Down, a, b, .. } => {
                    let (a, b) = (walk(a, slot)?, walk(b, slot)?);
                    Some((a.0 + b.0, a.1))
                }
                Node::Split { a, b, .. } => match (walk(a, slot), walk(b, slot)) {
                    (Some(a), Some(b)) => Some(if a.0 >= b.0 { a } else { b }),
                    (a, b) => a.or(b),
                },
            }
        }
        if tab.zoomed { slot(tab.focused) } else { walk(&tab.root, &slot) }
    }

    /// Fits the cell size to the shown tab's columns (see `column_height`), once their height
    /// has held still for a moment (a window resize doesn't make the text shimmer).
    fn refit(&mut self, state: &State) {
        if self.spec.fit <= 0.0 {
            self.fitted = None;
            self.fit_seen = None;
            *FITTED.lock().unwrap() = None;
            return;
        }
        let Some((target, scale)) = self.column_height(state).filter(|t| t.0 > 0.0) else { return };
        match self.fit_seen {
            Some((t, since)) if (t - target).abs() < 0.5 => {
                let done = self.fitted.is_some_and(|f| (f.0 - target).abs() < 0.5);
                if done {
                    return;
                }
                if self.time - since < 0.15 {
                    self.moving = true; // come back to fit once it settles
                    return;
                }
                let base = self.base_px(scale);
                let (spec, extra) = (&self.spec, self.spec.cell_height_px * scale);
                let (px, h) = fit(target, base, spec.cell_height, extra, spec.fit, |px| self.natural(px));
                self.fitted = Some((target, px, h));
                let own = (self.natural(base) * (1.0 + spec.cell_height) + extra).round().max(1.0);
                // Only worth showing when it changed something.
                let changed = px != base || (h - own).abs() >= 0.5;
                *FITTED.lock().unwrap() = changed.then(|| (px / (96.0 / 72.0 * scale), (h / own - 1.0) * 100.0));
            }
            _ => {
                self.fit_seen = Some((target, self.time));
                self.moving = true;
            }
        }
    }

    /// What each cell of a row draws. Runs of cells the same font draws are shaped together
    /// (`font-feature`: ligatures, contextual alternates), each glyph kept on its cluster's
    /// cell; a ligature is drawn from its first cell and the cells it covers draw nothing.
    /// Everything else (sprites, emoji, fallback fonts, lone characters) is drawn per cell.
    fn shape_row(&mut self, m: &Metrics, cells: &[Cell]) -> Vec<Draw> {
        let mut out = vec![Draw::Char; cells.len()];
        // The font a cell's character is shaped with, or None: blank, or drawn on its own.
        let fonts = &mut self.fonts;
        let mut font_of = |c: &Cell| -> Option<usize> {
            let ch = c.text.chars().next().filter(|ch| !ch.is_whitespace())?;
            if maybe_sprite(ch) {
                return None;
            }
            let style = c.bold as usize | (c.italic as usize) << 1;
            fonts.resolve(ch, style).map(|(fi, _)| fi).filter(|&fi| fi < 4)
        };
        let blank = |c: &Cell| c.text.chars().next().is_none_or(char::is_whitespace);
        let shaped: Vec<Option<usize>> = cells.iter().map(&mut font_of).collect();
        let mut x = 0;
        while x < cells.len() {
            let Some(fi) = shaped[x] else {
                x += 1;
                continue;
            };
            // The run: cells in this font, and blanks between them.
            let mut end = x + 1;
            let mut last = x;
            while end < cells.len() {
                match shaped[end] {
                    Some(f) if f == fi => last = end,
                    None if blank(&cells[end]) => {}
                    _ => break,
                }
                end += 1;
            }
            let end = last + 1;
            if end - x >= 2 {
                self.shape_run(m, cells, x, end, fi, &mut out);
            }
            x = end;
        }
        out
    }

    /// Shapes cells `from..to` with font `fi` into `out`, or takes the same run's shaping from
    /// the cache (rows repeat: prompts, scrolling, redraws).
    fn shape_run(&mut self, m: &Metrics, cells: &[Cell], from: usize, to: usize, fi: usize, out: &mut [Draw]) {
        let text: String = cells[from..to].iter().map(|c| c.text.chars().next().filter(|ch| !ch.is_whitespace()).unwrap_or(' ')).collect();
        let key = (fi, text);
        if let Some(run) = self.atlas.runs.get(&key) {
            out[from..to].clone_from_slice(run);
            return;
        }
        let mut run = vec![Draw::Char; to - from];
        self.shape_text(m, &key.1, fi, &mut run);
        out[from..to].clone_from_slice(&run);
        if self.atlas.runs.len() > 8192 {
            self.atlas.runs.clear();
        }
        self.atlas.runs.insert(key, run);
    }

    /// Shapes `text`, one character per cell, with font `fi`: what each cell draws into `out`.
    fn shape_text(&mut self, m: &Metrics, text: &str, fi: usize, out: &mut [Draw]) {
        let Some(font) = self.fonts.font(fi) else { return };
        // The cell each byte of `text` came from.
        let mut cell_of: Vec<usize> = vec![];
        for (i, (at, ch)) in text.char_indices().enumerate() {
            cell_of.resize(at + ch.len_utf8(), i);
        }
        let mut shaper = self
            .shaper
            .builder(font)
            .script(swash::text::Script::Latin)
            .size(m.px)
            .features(self.fonts.features.iter().copied())
            .variations(self.fonts.variations(fi).iter().copied())
            .build();
        shaper.add_str(&text);
        shaper.shape_with(|c| {
            let (start, end) = (c.source.start as usize, c.source.end as usize);
            let Some(&cell) = cell_of.get(start) else { return };
            out[cell] = match c.glyphs {
                [g] => Draw::One(fi, g.id, g.x, g.y),
                glyphs => {
                    let mut pen = 0.0;
                    Draw::Glyphs(glyphs.iter().map(|g| {
                        let at = (fi, g.id, pen + g.x, g.y);
                        pen += g.advance;
                        at
                    }).collect())
                }
            };
            for b in start + 1..end.min(cell_of.len()) {
                if cell_of[b] != cell {
                    out[cell_of[b]] = Draw::Covered;
                }
            }
        });
    }

    /// Builds one row's instances.
    fn build_row(&mut self, m: Metrics, pad: f32, y: usize, cells: &[Cell], frame_fg: Rgb) -> Row {
        let mut row = Row::default();
        let top = pad + y as f32 * m.h;
        // The accent, mixed into the background.
        let mix = |a: u8, b: u8| ((a as f32) * 0.38 + (b as f32) * 0.62).round() as u8;
        let (ac, bg) = (self.accent, self.theme.bg);
        let sel_bg = Rgb(mix(ac.0, bg.0), mix(ac.1, bg.1), mix(ac.2, bg.2));
        let draws = self.shape_row(&m, cells);
        for (x, c) in cells.iter().enumerate() {
            let left = pad + x as f32 * m.w;
            let bg = if c.selected { Some(sel_bg) } else { c.bg };
            if let Some(bg) = bg {
                // Extend the previous run if it's the same color and adjacent.
                match row.bg.last_mut() {
                    Some(last) if last.color == rgba(bg, 1.0) && (last.pos[0] + last.size[0] - left).abs() < 0.5 && last.pos[1] == top => {
                        last.size[0] += m.w;
                    }
                    _ => row.bg.push(Inst { pos: [left, top], size: [m.w, m.h], color: rgba(bg, 1.0), ..Default::default() }),
                }
            }
            let _ = frame_fg;
            let fg = c.fg; // selected text keeps its colour on the tinted background
            let one;
            let shaped: &[(usize, u16, f32, f32)] = match &draws[x] {
                Draw::One(fi, gid, dx, dy) => {
                    one = [(*fi, *gid, *dx, *dy)];
                    &one
                }
                Draw::Glyphs(g) => g,
                _ => &[],
            };
            match &draws[x] {
                Draw::Covered => {}
                Draw::One(..) | Draw::Glyphs(_) => {
                    for &(fi, gid, dx, dy) in shaped {
                        if let Some(s) = self.atlas.glyph(&mut self.ctx, &self.fonts, &m, fi, gid) {
                            row.fg.push(Inst {
                                pos: [(left + dx + s.left as f32).round(), (top + m.baseline - dy - s.top as f32).round()],
                                size: [s.w as f32, s.h as f32],
                                uv: [s.x as f32, s.y as f32],
                                color: rgba(fg, 1.0),
                                kind: if s.color { 4 } else { 1 },
                                ..Default::default()
                            });
                            if s.color {
                                let i = row.fg.last_mut().unwrap();
                                i.uv = [s.x as f32 / ATLAS as f32, s.y as f32 / ATLAS as f32];
                                i.uv_size = [s.w as f32 / ATLAS as f32, s.h as f32 / ATLAS as f32];
                                i.color = [1.0; 4];
                            }
                        }
                    }
                }
                Draw::Char => {
            if let Some(ch) = c.text.chars().next().filter(|ch| !ch.is_whitespace()) {
                let style = c.bold as u8 | (c.italic as u8) << 1;
                if let Some(s) = self.atlas.char(&mut self.ctx, &mut self.fonts, &m, ch, style).filter(|s| s.color) {
                    // An emoji fills the height of its two cells, centred, keeping its shape.
                    let span = if cells.get(x + 1).is_some_and(|n| n.text.is_empty()) { 2.0 } else { 1.0 };
                    let h = (m.h * 0.86).min(span * m.w * s.h as f32 / s.w as f32);
                    let w = h * s.w as f32 / s.h as f32;
                    row.fg.push(Inst {
                        pos: [(left + (span * m.w - w) / 2.0).round(), (top + (m.h - h) / 2.0).round()],
                        size: [w, h],
                        uv: [s.x as f32 / ATLAS as f32, s.y as f32 / ATLAS as f32],
                        uv_size: [s.w as f32 / ATLAS as f32, s.h as f32 / ATLAS as f32],
                        color: [1.0; 4],
                        kind: 4,
                        _pad: 0,
                    });
                } else if let Some(s) = self.atlas.char(&mut self.ctx, &mut self.fonts, &m, ch, style) {
                    row.fg.push(Inst {
                        pos: [(left + s.left as f32).round(), (top + m.baseline - s.top as f32).round()],
                        size: [s.w as f32, s.h as f32],
                        uv: [s.x as f32, s.y as f32],
                        color: rgba(fg, 1.0),
                        kind: 1,
                        ..Default::default()
                    });
                }
            }
                }
            }
            if c.underline {
                let t = (m.px / 14.0).round().max(1.0);
                row.fg.push(Inst { pos: [left, top + m.baseline + t * 2.0], size: [m.w, t], color: rgba(fg, 1.0), ..Default::default() });
            }
        }
        row
    }

    /// For `runeterm --bench`: builds every row of `frame` the way a frame does (glyph lookup,
    /// rasterizing what's new, instances), and returns how many instances that made.
    pub fn bench_rows(&mut self, frame: &Frame, scale: f32) -> usize {
        let m = self.metrics_at(scale);
        let pad = (self.spec.padding * scale).round();
        let mut n = 0;
        for (y, r) in frame.dirty_rows.iter().enumerate() {
            if let Some(cells) = r {
                let row = self.build_row(m, pad, y, cells, frame.bg);
                n += row.bg.len() + row.fg.len();
            }
        }
        self.atlas.uploads.clear();
        n
    }

    fn metrics_at(&mut self, scale: f32) -> Metrics {
        let (px, h) = self.cell_size(scale);
        match self.metrics {
            Some(m) if (m.px - px).abs() < 0.5 && (m.h - h).abs() < 0.5 => m,
            _ => {
                // New scale (or fit): new glyphs, and every pane redraws in full.
                let m = self.metrics_with(px, h);
                self.metrics = Some(m);
                self.atlas = Atlas::new();
                for p in self.panes.values_mut() {
                    p.rs = RenderState::new();
                    p.rows.clear();
                }
                m
            }
        }
    }

    /// Brings a pane's draw list up to date for this frame and uploads it. False if the pane
    /// is gone.
    fn prepare(&mut self, s: &State, id: u32, site: Site, device: &wgpu::Device, queue: &wgpu::Queue, dt: f32) -> bool {
        let Some(pane) = s.panes.get(&id) else { return false };
        let m = self.metrics_at(site.scale);
        let rect = site.rect;
        let pad = (self.spec.padding * site.scale).round();
        let want = (
            (((rect[2] - 2.0 * pad) / m.w).floor() as i32).clamp(2, 1000) as u16,
            (((rect[3] - 2.0 * pad) / m.h).floor() as i32).clamp(1, 500) as u16,
        );

        let gpu = self.gpu.as_ref().unwrap();
        let p = self.panes.entry(id).or_insert_with(|| gpu.pane(device));
        p.site = Some(site);
        // Resize the terminal once the layout settles, not on every frame of a spring.
        match (p.grid, p.pending_grid) {
            (None, _) => {
                pane.resize(want.0, want.1, m.w as u32, m.h as u32);
                p.grid = Some(want);
            }
            (Some(g), _) if g == want => p.pending_grid = None,
            (Some(_), Some((w, since))) if w == want && self.time - since >= 0.08 => {
                pane.resize(want.0, want.1, m.w as u32, m.h as u32);
                p.grid = Some(want);
                p.pending_grid = None;
            }
            (Some(_), Some((w, _))) if w == want => {}
            _ => p.pending_grid = Some((want, self.time)),
        }

        if let Some((cols, rows)) = p.grid {
            let g = Geometry { scale: site.scale, pad, cell_w: m.w, cell_h: m.h, cols, rows, width: rect[2], height: rect[3] };
            GEOMETRY.lock().unwrap().get_or_insert_with(HashMap::new).insert(id, g);
        }
        let textures = &self.textures;
        // Scrolling smoothly: the grid is drawn this much lower, the row above showing on top.
        let off = (s.scroll_frac(id) * m.h).round();
        let mut above_frame = None;
        let frame: Frame = {
            let mut t = pane.term.lock().unwrap();
            pane.dirty.store(false, Ordering::Release);
            p.rs.begin(&t);
            if off > 0.0 && t.scroll_offset() > 0 {
                // The viewport one line back has that row first; then straight back.
                t.scroll(-1);
                p.above_rs.begin(&t);
                t.scroll(1);
                above_frame = Some(p.above_rs.finish());
            }
            let frame = p.rs.finish();
            // Pixels are only readable while we hold the terminal: copy the ones we lack.
            if let Some(list) = &frame.images {
                for pl in list {
                    let have = textures.get(&(id, pl.image)).is_some_and(|x| x.generation == pl.generation)
                        || p.pending.iter().any(|q| q.0 == pl.image && q.1 == pl.generation);
                    if !have {
                        if let Some((w, h, px)) = t.image_rgba(pl.image) {
                            p.pending.push((pl.image, pl.generation, w, h, px));
                        }
                    }
                }
            }
            frame
        };
        let mut p = self.panes.remove(&id).unwrap();
        if let Some(list) = frame.images.clone() {
            p.placements = list;
        }
        if p.rows.len() != frame.rows as usize {
            p.rows.resize(frame.rows as usize, Row::default());
        }
        let mut body_changed = p.colors != (frame.fg, frame.bg) || p.body.is_empty();
        p.colors = (frame.fg, frame.bg);
        for (y, r) in frame.dirty_rows.iter().enumerate() {
            if let Some(cells) = r {
                p.rows[y] = self.build_row(m, pad, y, cells, frame.bg);
                body_changed = true;
            }
        }
        if let Some(Some(cells)) = above_frame.as_ref().and_then(|f| f.dirty_rows.first()) {
            p.above = Some(self.build_row(m, pad, 0, cells, frame.bg));
        }
        if body_changed {
            p.body.clear();
            for r in &p.rows {
                p.body.extend_from_slice(&r.bg);
            }
            p.body_bg = p.body.len();
            for r in &p.rows {
                p.body.extend_from_slice(&r.fg);
            }
        }

        // The cursor: an unfocused pane shows a hollow block, as Ghostty does.
        let focused = s.focused_id() == Some(id);
        let c = frame.cursor;
        let style = if focused { c.style } else { CursorStyle::BlockHollow };
        let bar = (site.scale * 2.0).round().max(1.0);
        let (x, y) = (pad + c.x as f32 * m.w, pad + c.y as f32 * m.h);
        let target = match style {
            CursorStyle::Bar => [x, y, bar, m.h],
            CursorStyle::Underline => [x, y + m.h - bar, m.w, bar],
            _ => [x, y, m.w, m.h],
        };
        if target != p.cursor_rect || p.cursor.is_none() {
            p.cursor_prev = if p.cursor.is_none() { target } else { p.cursor_rect };
            p.cursor_prev_style = p.cursor_style;
            p.cursor_changed = self.time;
            p.cursor_rect = target;
            // Glide over short moves (typing, a word, a line up or down). A long jump (a paste,
            // output, an editor jumping across the file) lands at once: gliding there would drag
            // the cursor over everything in between.
            let (dx, dy) = ((target[0] - p.cursor_rect[0]).abs() / m.w, (target[1] - p.cursor_rect[1]).abs() / m.h);
            let far = dx > GLIDE_MAX_COLS || dy > GLIDE_MAX_ROWS;
            if p.cursor.is_none() || !self.cursor_smooth || far {
                p.cursor_drawn = target;
            }
        }
        p.cursor_style = shader::style_number(style);
        p.cursor = Some(c);
        // Glide toward the target: fast, critically damped, snapping when close.
        let k = 1.0 - (-dt * 30.0).exp();
        let mut moving = false;
        for i in 0..4 {
            let d = p.cursor_rect[i] - p.cursor_drawn[i];
            if d.abs() < 0.5 {
                p.cursor_drawn[i] = p.cursor_rect[i];
            } else {
                p.cursor_drawn[i] += d * k;
                moving = true;
            }
        }
        self.moving |= moving || p.pending_grid.is_some();

        // This frame's list: body, cursor, images.
        p.instances.clear();
        let shift = |i: &Inst, dy: f32| Inst { pos: [i.pos[0], i.pos[1] + dy], ..*i };
        let above = p.above.as_ref().filter(|_| above_frame.is_some());
        if let Some(a) = above {
            p.instances.extend(a.bg.iter().map(|i| shift(i, off - m.h)));
        }
        p.instances.extend(p.body[..p.body_bg].iter().map(|i| shift(i, off)));
        p.bg_end = p.instances.len() as u32;
        if let Some(a) = above {
            p.instances.extend(a.fg.iter().map(|i| shift(i, off - m.h)));
        }
        p.instances.extend(p.body[p.body_bg..].iter().map(|i| shift(i, off)));
        if c.visible && self.cursor_opacity > 0.0 {
            let [x, y, w, h] = p.cursor_drawn;
            let y = y + off;
            let col = self.theme.cursor;
            let quad = |pos: [f32; 2], size: [f32; 2], a: f32| Inst { pos, size, color: rgba(col, a * self.cursor_opacity), ..Default::default() };
            match style {
                CursorStyle::BlockHollow => {
                    p.instances.push(quad([x, y], [w, bar], 1.0));
                    p.instances.push(quad([x, y + h - bar], [w, bar], 1.0));
                    p.instances.push(quad([x, y], [bar, h], 1.0));
                    p.instances.push(quad([x + w - bar, y], [bar, h], 1.0));
                }
                CursorStyle::Block => p.instances.push(quad([x, y], [w, h], 0.75)),
                _ => p.instances.push(quad([x, y], [w, h], 1.0)),
            }
        }
        p.text_end = p.instances.len() as u32;
        p.image_draws.clear();
        for pl in &p.placements {
            let (iw, ih) = self
                .textures
                .get(&(id, pl.image))
                .map(|t| t.size)
                .or_else(|| p.pending.iter().find(|q| q.0 == pl.image).map(|q| (q.2, q.3)))
                .unwrap_or((1, 1));
            let x = pad + pl.col as f32 * m.w + pl.offset[0] as f32;
            let y = pad + pl.row as f32 * m.h + pl.offset[1] as f32 + off;
            p.image_draws.push((pl.layer, p.instances.len() as u32, pl.image));
            p.instances.push(Inst {
                pos: [x, y],
                size: [pl.size[0] as f32, pl.size[1] as f32],
                uv: [pl.src[0] as f32 / iw as f32, pl.src[1] as f32 / ih as f32],
                uv_size: [pl.src[2] as f32 / iw as f32, pl.src[3] as f32 / ih as f32],
                color: [1.0; 4],
                kind: 2,
                _pad: 0,
            });
        }
        // The composite quad of a shader chain goes last.
        p.instances.push(Inst { pos: [0.0, 0.0], size: [rect[2], rect[3]], uv: [0.0, 0.0], uv_size: [1.0, 1.0], color: [1.0; 4], kind: 3, _pad: 0 });

        // Upload: glyphs, new images, the list.
        let gpu = self.gpu.as_mut().unwrap();
        for (s, data) in self.atlas.uploads.drain(..) {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture: if s.color { &gpu.color_tex } else { &gpu.atlas_tex },
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: s.x, y: s.y, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                &data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(s.w * if s.color { 4 } else { 1 }), rows_per_image: None },
                wgpu::Extent3d { width: s.w, height: s.h, depth_or_array_layers: 1 },
            );
        }
        for (image, generation, w, h, px) in p.pending.drain(..) {
            let bind = gpu.image(device, queue, w, h, &px);
            self.textures.insert((id, image), ImageTex { generation, size: (w, h), bind });
        }
        let live: Vec<u32> = p.placements.iter().map(|pl| pl.image).collect();
        self.textures.retain(|(pane, image), _| *pane != id || live.contains(image));
        if p.instances.len() > p.cap {
            p.cap = p.instances.len().next_power_of_two();
            p.buf = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("runeterm cells"),
                size: (p.cap * std::mem::size_of::<Inst>()) as u64,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&p.buf, 0, bytemuck::cast_slice(&p.instances));
        p.prepared = true;
        self.panes.insert(id, p);
        true
    }

    /// Draws a pane's images, backgrounds and text (and cursor) into an open pass.
    fn draw_layers(&self, pass: &mut wgpu::RenderPass<'_>, id: u32, p: &PaneGpu, gpu: &Gpu) {
        let images = |pass: &mut wgpu::RenderPass<'_>, layer: Layer| {
            for &(l, i, image) in &p.image_draws {
                if l == layer {
                    if let Some(t) = self.textures.get(&(id, image)) {
                        pass.set_bind_group(1, &t.bind, &[]);
                        pass.draw(0..6, i..i + 1);
                    }
                }
            }
        };
        images(pass, Layer::BelowBackground);
        pass.set_bind_group(1, &gpu.no_image, &[]);
        pass.draw(0..6, 0..p.bg_end);
        images(pass, Layer::BelowText);
        pass.set_bind_group(1, &gpu.no_image, &[]);
        pass.draw(0..6, p.bg_end..p.text_end);
        images(pass, Layer::AboveText);
    }

    /// Renders a pane offscreen and runs the shader chain over it.
    fn run_chain(&mut self, encoder: &mut wgpu::CommandEncoder, device: &wgpu::Device, queue: &wgpu::Queue, id: u32, focused: bool) {
        let gpu = self.gpu.as_mut().unwrap();
        let Some(p) = self.panes.get(&id) else { return };
        let Some(site) = p.site else { return };
        let size = (site.rect[2].round().max(1.0) as u32, site.rect[3].round().max(1.0) as u32);
        if p.offscreen.as_ref().map(|o| o.size) != Some(size) {
            let off = gpu.offscreen(device, size);
            self.panes.get_mut(&id).unwrap().offscreen = Some(off);
        }
        let gpu = self.gpu.as_mut().unwrap();
        gpu.pipeline(device, shader::FORMAT);
        let gpu = self.gpu.as_ref().unwrap();
        let p = self.panes.get(&id).unwrap();
        let off = p.offscreen.as_ref().unwrap();

        // The grid, upside down (Shadertoy's y points up), on the terminal's background colour
        // at zero alpha: effect shaders sampling it see the real colour (not black), cursor
        // shaders can tell text (alpha 1) from background, and what a shader leaves untouched
        // stays clear, so the split's own light shows through it.
        let lin = |v: u8| {
            let c = v as f64 / 255.0;
            if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
        };
        let b = self.theme.bg;
        let bg_clear = wgpu::Color { r: lin(b.0), g: lin(b.1), b: lin(b.2), a: 0.0 };
        let u = Uniforms {
            origin: [0.0; 4],
            target: [size.0 as f32, size.1 as f32, 0.0, 0.0],
            clip: [-1e6, -1e6, 1e6, 1e6],
            fade: [0; 4],
            flags: [0, 1, 0, 0],
        };
        queue.write_buffer(&p.off_u, 0, bytemuck::bytes_of(&u));
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("runeterm grid offscreen"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &off.views[0],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(bg_clear), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(gpu.pipelines.get(&shader::FORMAT).unwrap());
            pass.set_bind_group(0, &p.off_bind, &[]);
            pass.set_vertex_buffer(0, p.buf.slice(..));
            self.draw_layers(&mut pass, id, p, gpu);
        }

        // Ghostty's uniforms, in pane pixels with y up.
        let h = size.1 as f32;
        let up = |r: [f32; 4]| [r[0], h - r[1], r[2], r[3]];
        let lin = |c: Rgb| [c.0 as f32 / 255.0, c.1 as f32 / 255.0, c.2 as f32 / 255.0, 1.0];
        let t = &self.theme;
        let g = shader::Globals {
            res: [size.0 as f32, h, 1.0, 0.0],
            time: [self.time as f32, 1.0 / 60.0, 60.0, p.cursor_changed as f32],
            ints: [self.frame, p.cursor_style, p.cursor_prev_style, p.cursor.is_some_and(|c| c.visible) as i32],
            focus: [0.0, focused as i32 as f32, 0.0, 0.0],
            cursor: up(p.cursor_rect),
            prev_cursor: up(p.cursor_prev),
            cursor_color: lin(t.cursor),
            prev_cursor_color: lin(t.cursor),
            bg: lin(t.bg),
            fg: lin(t.fg),
            cursor_rgb: lin(t.cursor),
            cursor_text: lin(t.bg),
            sel_fg: lin(t.fg),
            sel_bg: lin(Rgb(0x3b, 0x4a, 0x63)),
            ..Default::default()
        };
        queue.write_buffer(&off.globals, 0, bytemuck::bytes_of(&g));
        let chain = gpu.chain.as_ref().unwrap();
        let mut src = 0;
        for pipeline in &chain.pipelines {
            let dst = 1 - src;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("runeterm custom shader"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &off.views[dst],
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(pipeline);
            pass.set_bind_group(0, &off.reads[src], &[]);
            pass.draw(0..3, 0..1);
            src = dst;
        }
        self.panes.get_mut(&id).unwrap().offscreen.as_mut().unwrap().result = src;
    }
}

impl Compositor<State> for GridRenderer {
    fn render(&mut self, f: &mut RuneFrame<'_, State>) {
        if crate::config::generation() != self.generation {
            self.generation = crate::config::generation();
            if let Some(c) = crate::config::published() {
                self.reload(&c);
            }
        }
        let key = f.device_key();
        if self.gpu.as_ref().map(|g| g.key) != Some(key) {
            self.gpu = Some(Gpu::new(f.device, key, &self.shaders));
            self.atlas = Atlas::new();
            self.metrics = None;
            self.panes.clear();
            self.textures.clear();
        }
        let dt = (f.dt as f32).clamp(0.0, 0.1);
        self.time += f.dt.clamp(0.0, 0.1);
        self.frame += 1;
        self.moving = false;
        self.refit(&f.rt.state);
        let clear = f.rt.clear_color();
        f.ui.prepare(f.device, f.queue, &mut f.rt.text.atlas, &f.rt.instances, f.size);

        let (device, queue) = (f.device, f.queue);
        let state = &f.rt.state;
        // Selections take the session's accent.
        self.accent = state.accent;
        let focused = state.focused_id();
        let shaders = self.gpu.as_ref().is_some_and(|g| g.chain.is_some());

        // Panes drawn last frame: update them, and run their shaders, before the frame's pass.
        for p in self.panes.values_mut() {
            p.prepared = false;
        }
        // Only the ones on screen last frame: a hidden tab's panes keep their GPU state (so
        // coming back to them is instant) but cost nothing while hidden.
        let known: Vec<(u32, Site)> = self
            .panes
            .iter()
            .filter(|(id, _)| self.last_seen.contains(id))
            .filter_map(|(id, p)| p.site.map(|s| (*id, s)))
            .collect();
        for (id, site) in known {
            if self.prepare(state, id, site, device, queue, dt) && shaders {
                self.run_chain(f.encoder, device, queue, id, focused == Some(id));
            }
        }

        let mut seen = Vec::new();
        let mut fresh = false;
        f.ui.draw_with(f.encoder, f.target, clear, &mut |pass, site: &EffectSite<'_>| {
            let id = site.mark;
            let r = site.instance.rect;
            let here = Site { rect: r, scale: site.scale };
            // A pane new this frame is drawn directly; from the next frame on it goes through
            // its shaders.
            let moved = self.panes.get(&id).and_then(|p| p.site).is_none_or(|s| s.rect != r || s.scale != site.scale);
            let ready = self.panes.get(&id).is_some_and(|p| p.prepared);
            if !ready {
                if !self.prepare(state, id, here, device, queue, dt) {
                    return;
                }
                fresh = true;
            } else if let Some(p) = self.panes.get_mut(&id) {
                p.site = Some(here);
            }
            fresh |= moved;
            seen.push(id);
            let gpu = self.gpu.as_mut().unwrap();
            gpu.pipeline(device, site.format);
            let gpu = self.gpu.as_ref().unwrap();
            let p = self.panes.get(&id).unwrap();
            let u = Uniforms {
                origin: [r[0] - site.origin[0], r[1] - site.origin[1], site.origin[0], site.origin[1]],
                target: [site.size[0] as f32, site.size[1] as f32, 0.0, 0.0],
                clip: site.instance.clip,
                fade: site.instance.fade,
                flags: [site.srgb_blend as u32, 0, 0, 0],
            };
            queue.write_buffer(&p.slot_u, 0, bytemuck::bytes_of(&u));
            pass.set_pipeline(gpu.pipelines.get(&site.format).unwrap());
            pass.set_bind_group(0, &p.slot_bind, &[]);
            pass.set_vertex_buffer(0, p.buf.slice(..));
            match (&p.offscreen, shaders) {
                // The shader's result, even while the slot is mid-animation and its size is a
                // frame behind: stretching it is invisible, falling back to the plain grid is not.
                (Some(off), true) if ready => {
                    let last = p.instances.len() as u32 - 1;
                    pass.set_bind_group(1, &off.shows[off.result], &[]);
                    pass.draw(0..6, last..last + 1);
                }
                // Shown for the first time this frame: its shaders run from the next one. It's
                // still fading in, so drawing nothing beats a frame without the shader.
                (_, true) => {}
                (_, false) => self.draw_layers(pass, id, p, gpu),
            }
        });
        // Panes that still exist keep their GPU state, visible or not.
        self.panes.retain(|id, _| state.panes.contains_key(id));
        self.textures.retain(|(pane, _), _| state.panes.contains_key(pane));
        self.last_seen = seen;
        let animate_shaders = shaders
            && match self.shader_animation {
                ShaderAnimation::Off => false,
                ShaderAnimation::Focused => focused.is_some(),
                ShaderAnimation::Always => true,
            };
        // Shaders without continuous animation still get a second to play out a change.
        let recent = self.panes.values().any(|p| self.time - p.cursor_changed < 1.0);
        self.moving |= fresh || animate_shaders || (shaders && recent);
    }

    fn animating(&self) -> bool {
        self.moving
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(family: &str) -> FontSpec {
        FontSpec { family: family.into(), padding: 0.0, ..FontSpec::default() }
    }

    #[test]
    fn font_patterns_follow_styles_and_families() {
        let mut s = spec("Spline Sans Mono");
        assert_eq!(font_pattern(&s, 0).as_deref(), Some("Spline Sans Mono"));
        assert_eq!(font_pattern(&s, 3).as_deref(), Some("Spline Sans Mono:bold:italic"));
        s.styles = [FontStyle::parse("Light"), FontStyle::parse("Medium"), FontStyle::parse("false"), FontStyle::parse("")];
        assert_eq!(font_pattern(&s, 0).as_deref(), Some("Spline Sans Mono:style=Light"));
        assert_eq!(font_pattern(&s, 1).as_deref(), Some("Spline Sans Mono:style=Medium"));
        assert_eq!(font_pattern(&s, 2), None, "italic is off");
        assert_eq!(font_pattern(&s, 3).as_deref(), Some("Spline Sans Mono:bold:italic"));
        s.families[2] = Some("Victor Mono".into());
        assert_eq!(font_pattern(&s, 3).as_deref(), Some("Victor Mono:bold:italic"));
        // Regular can't be turned off.
        s.styles[0] = FontStyle::Off;
        assert_eq!(font_pattern(&s, 0).as_deref(), Some("Spline Sans Mono"));
    }

    #[test]
    fn font_features_read_like_ghosttys() {
        assert_eq!(parse_features("-calt"), Some(vec![("calt".into(), 0)]));
        assert_eq!(parse_features("+liga, ss01=2, zero, dlig=off"), Some(vec![("liga".into(), 1), ("ss01".into(), 2), ("zero".into(), 1), ("dlig".into(), 0)]));
        assert_eq!(parse_features("toolong"), None);
        assert_eq!(parse_variations("wght=450, slnt=-10"), Some(vec![("wght".into(), 450.0), ("slnt".into(), -10.0)]));
        assert_eq!(parse_variations("wght"), None);
        let vs = vec![("slnt".to_string(), -10.0), ("wght".to_string(), 450.0)];
        let inherited = inherit_axes(&[vec![("wght".into(), 300.0), ("wdth".into(), 87.5), ("slnt".into(), -5.0)], vec![("wght".into(), 600.0)], vec![("wdth".into(), 100.0)], vec![]]);
        assert_eq!(inherited[1], vec![("wght".into(), 600.0), ("slnt".into(), -5.0), ("wdth".into(), 87.5)], "bold keeps its weight, takes the width");
        assert_eq!(inherited[2], vec![("wdth".into(), 87.5), ("slnt".into(), -5.0)], "regular's width is every style's");
        assert_eq!(inherited[3], vec![("slnt".into(), -5.0), ("wdth".into(), 87.5)]);
        // A width set on one style alone is every style's too.
        let only_bold = inherit_axes(&[vec![], vec![("wdth".into(), 75.0)], vec![], vec![]]);
        assert!(only_bold.iter().all(|s| s == &vec![("wdth".to_string(), 75.0)]));
        assert_eq!(with_axis(&vs, "wght", 300.0), vec![("slnt".into(), -10.0), ("wght".into(), 300.0)]);
        assert_eq!(with_axis(&vs, "wdth", 87.5)[2], ("wdth".into(), 87.5));
        assert_eq!(parse_synthetic("false"), [false; 3]);
        assert_eq!(parse_synthetic("no-bold,no-bold-italic"), [false, true, false]);
        assert_eq!(parse_synthetic("true"), [true; 3]);
    }

    fn cells(text: &str) -> Vec<Cell> {
        text.chars().map(|c| Cell { text: c.to_string(), ..Default::default() }).collect()
    }

    /// Shaping on a real ligature font (skipped where it isn't installed): JetBrains Mono turns
    /// `->` into an arrow made of its own glyphs, one per cell, and `-calt` keeps the plain ones.
    /// Plain words keep one glyph per cell, at the cell's own left edge.
    #[test]
    fn runs_are_shaped_with_the_configured_features() {
        let family = "JetBrainsMono Nerd Font Mono";
        if !fc_match(family, "%{family}").is_some_and(|f| f.contains("JetBrains")) {
            return;
        }
        let shape = |extra: &str| {
            let mut c = Config::default();
            c.apply(&format!("font-family = {family}\n{extra}"), std::path::Path::new("/"));
            let mut g = GridRenderer::new(&c);
            let m = g.metrics_at(1.0);
            g.shape_row(&m, &cells("a -> b"))
        };
        let on = shape("");
        let off = shape("font-feature = -calt, -liga\n");
        let ids = |d: &[Draw]| d.iter().map(|d| match d { Draw::One(_, id, ..) => *id, _ => 0 }).collect::<Vec<_>>();
        assert_ne!(ids(&on)[2..4], ids(&off)[2..4], "the arrow's glyphs change with calt");
        assert_eq!(ids(&on)[0], ids(&off)[0], "a letter stays itself");
        assert!(matches!(on[0], Draw::One(0, _, x, _) if x == 0.0), "at its cell's left edge");
        // A lone character isn't shaped.
        assert_eq!(shape("")[..1], [Draw::One(0, ids(&on)[0], 0.0, 0.0)]);
        let lone = {
            let mut c = Config::default();
            c.apply(&format!("font-family = {family}\n"), std::path::Path::new("/"));
            let mut g = GridRenderer::new(&c);
            let m = g.metrics_at(1.0);
            g.shape_row(&m, &cells("x"))
        };
        assert_eq!(lone, vec![Draw::Char]);
    }

    /// A family with static and variable files (Geist Mono) gets its variable one for a weight;
    /// one that has none isn't swapped for another family's.
    #[test]
    fn a_weight_finds_the_familys_variable_file() {
        assert_eq!(variable_file("NoSuchFamilyAtAll"), None);
        let Some(file) = variable_file("Geist Mono") else { return };
        assert!(file.contains("Geist"));
        let mut c = Config::default();
        c.apply("font-family = Geist Mono\n", std::path::Path::new("/"));
        let (lo, _, hi) = font_axis(&c.font, 0, "wght").expect("Geist Mono's variable file has a wght axis");
        assert!(lo <= 300.0 && hi >= 700.0, "{lo}..{hi}");
        // The weight is drawn: heavier text has more ink.
        let ink = |w: &str| {
            let mut c = Config::default();
            c.apply(&format!("font-family = Geist Mono\nfont-variation = wght={w}\n"), std::path::Path::new("/"));
            let mut g = GridRenderer::new(&c);
            let m = g.metrics_at(1.0);
            let (fi, gid) = g.fonts.resolve('M', 0).unwrap();
            let font = g.fonts.get(fi).unwrap();
            let mut scaler = g.ctx.builder(font).size(m.px).variations(g.fonts.variations(fi).iter().copied()).build();
            let img = swash::scale::Render::new(&[swash::scale::Source::Outline]).render(&mut scaler, gid).unwrap();
            img.data.iter().map(|&a| a as u32).sum::<u32>()
        };
        assert!(ink("200") < ink("800"));
    }

    /// A narrower width (`wdth`) narrows the cells too, not just the letters, and loads the
    /// variable file without a weight set.
    #[test]
    fn a_width_narrows_the_cells() {
        if variable_file("Martian Mono").is_none() {
            return;
        }
        let cell_w = |extra: &str| {
            let mut c = Config::default();
            c.apply(&format!("font-family = Martian Mono\n{extra}"), std::path::Path::new("/"));
            let mut g = GridRenderer::new(&c);
            let font = g.fonts.get(0).unwrap();
            let axis = font.variations().find_by_tag(tag("wdth")).map(|v| (v.min_value(), v.default_value(), v.max_value()));
            eprintln!("wdth axis {axis:?}");
            g.metrics_at(1.0).w
        };
        let normal = cell_w("font-variation = wdth=100\n");
        let condensed = cell_w("font-variation = wdth=75\n");
        eprintln!("cell width: wdth=100 {normal}, wdth=75 {condensed}");
        assert!(condensed < normal);
        // Set on bold alone, the width is regular's too; regular's Light face keeps its weight
        // in the variable file, and the family's Bold its own.
        let mut c = Config::default();
        c.apply("font-family = Martian Mono\nfont-style = Light\nfont-variation-bold = wdth=75\n", std::path::Path::new("/"));
        let mut g = GridRenderer::new(&c);
        assert_eq!(g.metrics_at(1.0).w, condensed);
        let axis = |i: usize, t: &str| g.fonts.variations(i).iter().find(|s| s.tag == tag(t)).map(|s| s.value);
        assert_eq!(axis(0, "wght"), Some(300.0));
        assert_eq!(axis(1, "wght"), Some(700.0));
        assert_eq!(axis(0, "wdth"), Some(75.0));
        assert!(!g.fonts.synth[1].0, "a real bold weight isn't emboldened too");
    }

    #[test]
    fn styles_are_matched_by_name() {
        assert!(has_style("Light,Regular", "light"));
        assert!(has_style("Medium Italic,Italic", "Medium Italic"));
        assert!(!has_style("Regular", "Light"));
    }

    /// A font whose own line height is 1.2× its size.
    fn natural(px: f32) -> f32 {
        px * 1.2
    }

    #[test]
    fn no_fit_keeps_the_configured_size() {
        // 30 px at 30% more line height: a 47 px cell (46.8 rounded).
        assert_eq!(fit(1000.0, 30.0, 0.3, 0.0, 0.0, natural), (30.0, 47.0));
    }

    #[test]
    fn a_fit_leaves_no_strip_when_it_can() {
        // No cell within 5% divides 1000 exactly: the fit leaves the least of any of them.
        let (_, h) = fit(1000.0, 30.0, 0.3, 0.0, 0.05, natural);
        let left = 1000.0 % h;
        for other in 45..=49 {
            assert!(left <= 1000.0 % other as f32 + 0.5, "{h} leaves {left}, {other} leaves less");
        }
        // 1288 = 28 × 46: reachable, so nothing is left.
        let (_, h) = fit(1288.0, 30.0, 0.3, 0.0, 0.05, natural);
        assert_eq!(1288.0 % h, 0.0, "{h}");
    }

    #[test]
    fn spacing_moves_before_the_letters_do() {
        // 940 = 20 × 47: the configured cell already fits; nothing changes.
        assert_eq!(fit(940.0, 30.0, 0.3, 0.0, 0.05, natural), (30.0, 47.0));
        // 900 = 20 × 45: a line height change alone does it, so the font size stays.
        assert_eq!(fit(900.0, 30.0, 0.3, 0.0, 0.05, natural).0, 30.0);
    }

    #[test]
    fn a_fit_stays_within_its_bounds() {
        for target in (300..2000).step_by(37) {
            let (px, h) = fit(target as f32, 30.0, 0.3, 0.0, 0.03, natural);
            assert!((29.0..=30.9).contains(&px), "{px}");
            let own = natural(px) * 1.3;
            let inside = h >= (own * 0.97).ceil() && h <= (own * 1.03).floor();
            assert!(inside || (px, h) == (30.0, 47.0), "{target}: {h} vs {own}");
        }
    }
}
