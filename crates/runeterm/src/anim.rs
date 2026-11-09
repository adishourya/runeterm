//! Playing the animated session animals: frame sheets (see `animals.rs`) decoded on first use,
//! advanced on the app's clock only while something shows them.

use crate::animals::{ALL, Anim};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// An animation being played: an image per frame (Rune doesn't redraw an image replaced in
/// place, so each frame is its own), the frame showing, and the one it rests on.
struct Playing {
    frames: Vec<rune::Pixels>,
    shown: usize,
    rest: usize,
    /// When it started playing: it starts from its resting frame, as the still shows.
    start: Option<f64>,
}

/// Decoded only while playing (`tick` lets go of the rest): a minute of frames is megabytes.
static PLAYERS: Mutex<Option<HashMap<&'static str, Playing>>> = Mutex::new(None);
/// Each animal's resting frame, shown while it stands still: small, and kept.
static STILLS: Mutex<Option<HashMap<&'static str, rune::Pixels>>> = Mutex::new(None);

/// spooky's emoji for each animal name, from `spooky animal --list`, looked up once.
fn names() -> &'static HashMap<String, String> {
    static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();
    NAMES.get_or_init(|| crate::spooky::animals().into_iter().collect())
}

/// The animation for an animal, by its emoji (as spooky reports it) or its name.
pub fn find(animal: &str) -> Option<&'static Anim> {
    let name = names().get(animal).map(String::as_str).unwrap_or(animal);
    ALL.iter().find(|a| a.name == name)
}

/// How long one play through `anim` takes, in seconds.
pub fn duration(anim: &Anim) -> f64 {
    anim.frames as f64 / anim.fps.max(1.0) as f64
}

/// The frame an animal rests on: the first one well drawn (some animations fade or swim in
/// from nothing).
fn rest(frames: &[Vec<u8>]) -> usize {
    let ink: Vec<usize> = frames.iter().map(|f| f.chunks(4).filter(|p| p[3] > 32).count()).collect();
    let most = ink.iter().copied().max().unwrap_or(0);
    ink.iter().position(|&n| n * 10 >= most * 6).unwrap_or(0)
}

/// The image of `anim` playing, and the frame it shows. Its frames are decoded the first time
/// it's asked for; `tick` moves it on.
pub fn frame(anim: &'static Anim) -> (rune::Pixels, usize) {
    let mut players = PLAYERS.lock().unwrap();
    let p = players.get_or_insert_with(HashMap::new).entry(anim.name).or_insert_with(|| {
        let raw = decode(anim);
        let rest = rest(&raw);
        let frames = raw
            .into_iter()
            .map(|f| {
                let px = rune::Pixels::new();
                px.set(anim.size, anim.size, f);
                px
            })
            .collect();
        Playing { frames, shown: rest, rest, start: None }
    });
    (p.frames.get(p.shown).cloned().unwrap_or_default(), p.shown)
}

/// `anim` standing still, on its resting frame.
pub fn still(anim: &'static Anim) -> rune::Pixels {
    let mut stills = STILLS.lock().unwrap();
    stills
        .get_or_insert_with(HashMap::new)
        .entry(anim.name)
        .or_insert_with(|| {
            let pixels = rune::Pixels::new();
            let mut frames = decode(anim);
            let i = rest(&frames);
            if i < frames.len() {
                pixels.set(anim.size, anim.size, frames.swap_remove(i));
            }
            pixels
        })
        .clone()
}

/// Moves each of `playing` to its frame due at `t` (seconds), and lets go of the animations no
/// longer playing. Returns whether any frame changed (the view then shows it), and the time
/// until the next.
pub fn tick(t: f64, playing: &[&'static Anim]) -> (bool, Option<f64>) {
    let mut players = PLAYERS.lock().unwrap();
    let Some(players) = players.as_mut() else { return (false, None) };
    players.retain(|name, _| playing.iter().any(|a| a.name == *name));
    let (mut changed, mut next) = (false, None::<f64>);
    for anim in playing {
        let Some(p) = players.get_mut(anim.name) else { continue };
        let pos = (t - *p.start.get_or_insert(t)) * anim.fps as f64;
        let i = (p.rest + pos.floor() as usize) % p.frames.len().max(1);
        changed |= i != p.shown;
        p.shown = i;
        let until = (pos.floor() + 1.0 - pos) / anim.fps as f64;
        next = Some(next.map_or(until, |n| n.min(until)));
    }
    (changed, next)
}

/// Splits a frame sheet into frames.
fn decode(anim: &Anim) -> Vec<Vec<u8>> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(anim.png));
    dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
    let Ok(mut reader) = dec.read_info() else { return vec![] };
    let Some(len) = reader.output_buffer_size() else { return vec![] };
    let mut buf = vec![0u8; len];
    let Ok(info) = reader.next_frame(&mut buf) else { return vec![] };
    if info.color_type != png::ColorType::Rgba {
        return vec![];
    }
    let (w, s) = (info.width as usize, anim.size as usize);
    (0..anim.frames as usize)
        .map(|i| {
            let (cx, cy) = ((i % anim.cols as usize) * s, (i / anim.cols as usize) * s);
            let mut f = Vec::with_capacity(s * s * 4);
            for y in 0..s {
                let row = ((cy + y) * w + cx) * 4;
                f.extend_from_slice(&buf[row..row + s * 4]);
            }
            f
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_sheet_decodes_into_its_frames() {
        for a in ALL {
            let frames = decode(a);
            assert_eq!(frames.len(), a.frames as usize, "{}", a.name);
            assert!(frames.iter().all(|f| f.len() == (a.size * a.size * 4) as usize), "{}", a.name);
            // Not blank: some frame has visible pixels.
            assert!(frames.iter().any(|f| f.chunks(4).any(|p| p[3] > 0)), "{} is empty", a.name);
            // It rests on a frame with something on it.
            assert!(frames[rest(&frames)].chunks(4).any(|p| p[3] > 32), "{} rests on nothing", a.name);
        }
    }
}
