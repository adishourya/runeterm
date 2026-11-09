//! Characters drawn by the terminal instead of the font, exactly one cell big so they join up
//! across cells whatever the line height: box drawing (U+2500–257F), block elements
//! (U+2580–259F), braille (U+2800–28FF) and the powerline separators (U+E0B0–E0B7). Fonts
//! draw these at their own line height, which leaves gaps once `adjust-cell-height` adds space.

/// An 8-bit coverage bitmap of one cell, `w` × `h`.
pub struct Sprite {
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// Rasterizes `inside(x, y)` (cell pixels, y down) with 4×4 supersampling.
fn raster(w: u32, h: u32, inside: impl Fn(f32, f32) -> bool) -> Sprite {
    const N: u32 = 4;
    let mut data = vec![0u8; (w * h) as usize];
    for py in 0..h {
        for px in 0..w {
            let mut hits = 0;
            for sy in 0..N {
                for sx in 0..N {
                    let x = px as f32 + (sx as f32 + 0.5) / N as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / N as f32;
                    hits += inside(x, y) as u32;
                }
            }
            data[(py * w + px) as usize] = (hits * 255 / (N * N)) as u8;
        }
    }
    Sprite { w, h, data }
}

/// Line weights of each arm: up, right, down, left. 0 none, 1 light, 2 heavy, 3 double.
fn arms(c: u32) -> Option<[u8; 4]> {
    // Indexed by c - 0x2500, for the characters that are just arms meeting at the center.
    const T: [[u8; 4]; 0x80] = [
        [0, 1, 0, 1], [0, 2, 0, 2], [1, 0, 1, 0], [2, 0, 2, 0], // ─ ━ │ ┃
        [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], [0; 4], // dashes, below
        [0, 1, 1, 0], [0, 2, 1, 0], [0, 1, 2, 0], [0, 2, 2, 0], // ┌ ┍ ┎ ┏
        [0, 0, 1, 1], [0, 0, 1, 2], [0, 0, 2, 1], [0, 0, 2, 2], // ┐ ┑ ┒ ┓
        [1, 1, 0, 0], [1, 2, 0, 0], [2, 1, 0, 0], [2, 2, 0, 0], // └ ┕ ┖ ┗
        [1, 0, 0, 1], [1, 0, 0, 2], [2, 0, 0, 1], [2, 0, 0, 2], // ┘ ┙ ┚ ┛
        [1, 1, 1, 0], [1, 2, 1, 0], [2, 1, 1, 0], [1, 1, 2, 0], // ├ ┝ ┞ ┟
        [2, 1, 2, 0], [2, 2, 1, 0], [1, 2, 2, 0], [2, 2, 2, 0], // ┠ ┡ ┢ ┣
        [1, 0, 1, 1], [1, 0, 1, 2], [2, 0, 1, 1], [1, 0, 2, 1], // ┤ ┥ ┦ ┧
        [2, 0, 2, 1], [2, 0, 1, 2], [1, 0, 2, 2], [2, 0, 2, 2], // ┨ ┩ ┪ ┫
        [0, 1, 1, 1], [0, 1, 1, 2], [0, 2, 1, 1], [0, 2, 1, 2], // ┬ ┭ ┮ ┯
        [0, 1, 2, 1], [0, 1, 2, 2], [0, 2, 2, 1], [0, 2, 2, 2], // ┰ ┱ ┲ ┳
        [1, 1, 0, 1], [1, 1, 0, 2], [1, 2, 0, 1], [1, 2, 0, 2], // ┴ ┵ ┶ ┷
        [2, 1, 0, 1], [2, 1, 0, 2], [2, 2, 0, 1], [2, 2, 0, 2], // ┸ ┹ ┺ ┻
        [1, 1, 1, 1], [1, 1, 1, 2], [1, 2, 1, 1], [1, 2, 1, 2], // ┼ ┽ ┾ ┿
        [2, 1, 1, 1], [1, 1, 2, 1], [2, 1, 2, 1], [2, 1, 1, 2], // ╀ ╁ ╂ ╃
        [2, 2, 1, 1], [1, 1, 2, 2], [1, 2, 2, 1], [2, 2, 1, 2], // ╄ ╅ ╆ ╇
        [1, 2, 2, 2], [2, 1, 2, 2], [2, 2, 2, 1], [2, 2, 2, 2], // ╈ ╉ ╊ ╋
        [0; 4], [0; 4], [0; 4], [0; 4], // ╌ ╍ ╎ ╏ dashes
        [0, 3, 0, 3], [3, 0, 3, 0], // ═ ║
        [0, 3, 1, 0], [0, 1, 3, 0], [0, 3, 3, 0], // ╒ ╓ ╔
        [0, 0, 1, 3], [0, 0, 3, 1], [0, 0, 3, 3], // ╕ ╖ ╗
        [1, 3, 0, 0], [3, 1, 0, 0], [3, 3, 0, 0], // ╘ ╙ ╚
        [1, 0, 0, 3], [3, 0, 0, 1], [3, 0, 0, 3], // ╛ ╜ ╝
        [1, 3, 1, 0], [3, 1, 3, 0], [3, 3, 3, 0], // ╞ ╟ ╠
        [1, 0, 1, 3], [3, 0, 3, 1], [3, 0, 3, 3], // ╡ ╢ ╣
        [0, 3, 1, 3], [0, 1, 3, 1], [0, 3, 3, 3], // ╤ ╥ ╦
        [1, 3, 0, 3], [3, 1, 0, 1], [3, 3, 0, 3], // ╧ ╨ ╩
        [1, 3, 1, 3], [3, 1, 3, 1], [3, 3, 3, 3], // ╪ ╫ ╬
        [0; 4], [0; 4], [0; 4], [0; 4], // ╭ ╮ ╯ ╰ arcs
        [0; 4], [0; 4], [0; 4], // ╱ ╲ ╳ diagonals
        [0, 0, 0, 1], [1, 0, 0, 0], [0, 1, 0, 0], [0, 0, 1, 0], // ╴ ╵ ╶ ╷
        [0, 0, 0, 2], [2, 0, 0, 0], [0, 2, 0, 0], [0, 0, 2, 0], // ╸ ╹ ╺ ╻
        [0, 2, 0, 1], [1, 0, 2, 0], [0, 1, 0, 2], [2, 0, 1, 0], // ╼ ╽ ╾ ╿
    ];
    let a = *T.get((c.checked_sub(0x2500)?) as usize)?;
    (a != [0; 4]).then_some(a)
}

/// Draws `c` into a `w` × `h` cell, with `t` the light line thickness. None for characters
/// the font should draw.
pub fn draw(c: char, w: u32, h: u32, t: f32) -> Option<Sprite> {
    let cp = c as u32;
    let (fw, fh) = (w as f32, h as f32);
    let (cx, cy) = ((fw / 2.0).floor(), (fh / 2.0).floor());
    let thick = |k: u8| if k == 2 { t * 2.0 } else { t };
    // A band of thickness `th` centred on `mid` (snapped so light lines stay crisp).
    let band = |v: f32, mid: f32, th: f32| {
        let lo = (mid - th / 2.0).round();
        v >= lo && v < lo + th.round().max(1.0)
    };

    if let Some([u, r, d, l]) = arms(cp) {
        let v_th = |k: u8| if k == 0 { 0.0 } else if k == 3 { t * 3.0 } else { thick(k) };
        // How far a horizontal arm reaches past the centre so it meets the vertical ones.
        let reach_h = v_th(u).max(v_th(d)) / 2.0;
        let reach_v = v_th(l).max(v_th(r)) / 2.0;
        return Some(raster(w, h, |x, y| {
            let line = |k: u8, along: f32, mid: f32| match k {
                0 => false,
                3 => band(along, mid - t, t) || band(along, mid + t, t),
                k => band(along, mid, thick(k)),
            };
            (l > 0 && x < cx + reach_h.max(thick(l) / 2.0) && line(l, y, cy))
                || (r > 0 && x >= cx - reach_h.max(thick(r) / 2.0) && line(r, y, cy))
                || (u > 0 && y < cy + reach_v.max(thick(u) / 2.0) && line(u, x, cx))
                || (d > 0 && y >= cy - reach_v.max(thick(d) / 2.0) && line(d, x, cx))
        }));
    }

    match cp {
        // Dashed lines: n dashes per cell, horizontal or vertical, light or heavy.
        0x2504..=0x250B | 0x254C..=0x254F => {
            let (n, horizontal, heavy) = match cp {
                0x2504 => (3, true, false), 0x2505 => (3, true, true),
                0x2506 => (3, false, false), 0x2507 => (3, false, true),
                0x2508 => (4, true, false), 0x2509 => (4, true, true),
                0x250A => (4, false, false), 0x250B => (4, false, true),
                0x254C => (2, true, false), 0x254D => (2, true, true),
                0x254E => (2, false, false), _ => (2, false, true),
            };
            let th = if heavy { t * 2.0 } else { t };
            Some(raster(w, h, |x, y| {
                let (along, across, len, mid) = if horizontal { (x, y, fw, cy) } else { (y, x, fh, cx) };
                let seg = len / n as f32;
                band(across, mid, th) && (along % seg) < seg * 0.6
            }))
        }
        // Rounded corners: a quarter circle joining the two arms.
        0x256D..=0x2570 => {
            let r = (fw / 2.0).min(fh / 2.0);
            // Which way the arms go: (horizontal arm to the right?, vertical arm down?)
            let (right, down) = match cp { 0x256D => (true, true), 0x256E => (false, true), 0x256F => (false, false), _ => (true, false) };
            let ox = if right { cx + r } else { cx - r };
            let oy = if down { cy + r } else { cy - r };
            Some(raster(w, h, |x, y| {
                let in_h = if right { x >= ox } else { x < ox };
                let in_v = if down { y >= oy } else { y < oy };
                let arm_h = in_h && band(y, cy, t) && !in_v;
                let arm_v = in_v && band(x, cx, t) && !in_h;
                let ring = {
                    let d = ((x - ox).powi(2) + (y - oy).powi(2)).sqrt();
                    let near_corner = (if right { x <= ox } else { x >= ox }) && (if down { y <= oy } else { y >= oy });
                    near_corner && (d - r).abs() <= t / 2.0
                };
                arm_h || arm_v || ring
            }))
        }
        // Diagonals.
        0x2571..=0x2573 => {
            let k = fh / fw;
            Some(raster(w, h, |x, y| {
                let half = t * 0.5 * (1.0 + k * k).sqrt();
                let fwd = (y - (fh - x * k)).abs() <= half; // ╱
                let back = (y - x * k).abs() <= half; // ╲
                match cp { 0x2571 => fwd, 0x2572 => back, _ => fwd || back }
            }))
        }
        // Block elements.
        0x2580..=0x259F => {
            let shade = match cp { 0x2591 => Some(64), 0x2592 => Some(128), 0x2593 => Some(192), _ => None };
            if let Some(a) = shade {
                return Some(Sprite { w, h, data: vec![a; (w * h) as usize] });
            }
            let q = |x: f32, y: f32, tl: bool, tr: bool, bl: bool, br: bool| {
                let (left, top) = (x < fw / 2.0, y < fh / 2.0);
                match (left, top) { (true, true) => tl, (false, true) => tr, (true, false) => bl, _ => br }
            };
            Some(raster(w, h, move |x, y| match cp {
                0x2580 => y < (fh / 2.0).round(),
                0x2581..=0x2588 => y >= fh - (fh * (cp - 0x2580) as f32 / 8.0).round(),
                0x2589..=0x258F => x < (fw * (0x2590 - cp) as f32 / 8.0).round(),
                0x2590 => x >= (fw / 2.0).round(),
                0x2594 => y < (fh / 8.0).round().max(1.0),
                0x2595 => x >= fw - (fw / 8.0).round().max(1.0),
                0x2596 => q(x, y, false, false, true, false),
                0x2597 => q(x, y, false, false, false, true),
                0x2598 => q(x, y, true, false, false, false),
                0x2599 => q(x, y, true, false, true, true),
                0x259A => q(x, y, true, false, false, true),
                0x259B => q(x, y, true, true, true, false),
                0x259C => q(x, y, true, true, false, true),
                0x259D => q(x, y, false, true, false, false),
                0x259E => q(x, y, false, true, true, false),
                _ => q(x, y, false, true, true, true),
            }))
        }
        // Braille: 2 × 4 dots.
        0x2801..=0x28FF => {
            let bits = cp - 0x2800;
            let dot = |i: u32| -> (u32, u32) { [(0, 0), (0, 1), (0, 2), (1, 0), (1, 1), (1, 2), (0, 3), (1, 3)][i as usize] };
            let r = (fw / 4.0).min(fh / 8.0) * 0.72;
            Some(raster(w, h, |x, y| {
                (0..8).any(|i| {
                    if bits & (1 << i) == 0 {
                        return false;
                    }
                    let (c, rw) = dot(i);
                    let (dx, dy) = (fw * (c as f32 * 2.0 + 1.0) / 4.0, fh * (rw as f32 * 2.0 + 1.0) / 8.0);
                    (x - dx).powi(2) + (y - dy).powi(2) <= r * r
                })
            }))
        }
        // Powerline: solid and thin triangles, and half circles.
        0xE0B0..=0xE0B7 => Some(raster(w, h, |x, y| {
            let ny = (y / fh - 0.5).abs() * 2.0; // 0 at the middle row, 1 at the edges
            let nx = x / fw;
            let solid_r = nx <= 1.0 - ny; // ▶ pointing right
            let solid_l = 1.0 - nx <= 1.0 - ny;
            let edge = t / fw * 1.4;
            match cp {
                0xE0B0 => solid_r,
                0xE0B2 => solid_l,
                0xE0B1 => ((1.0 - ny) - nx).abs() <= edge,
                0xE0B3 => ((1.0 - ny) - (1.0 - nx)).abs() <= edge,
                0xE0B4 | 0xE0B5 => {
                    let d = (x / fw).powi(2) + ((y - fh / 2.0) / (fh / 2.0)).powi(2);
                    if cp == 0xE0B4 { d <= 1.0 } else { (d.sqrt() - 1.0 + edge).abs() <= edge }
                }
                _ => {
                    let d = ((fw - x) / fw).powi(2) + ((y - fh / 2.0) / (fh / 2.0)).powi(2);
                    if cp == 0xE0B6 { d <= 1.0 } else { (d.sqrt() - 1.0 + edge).abs() <= edge }
                }
            }
        })),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &Sprite, x: u32, y: u32) -> u8 {
        s.data[(y * s.w + x) as usize]
    }

    #[test]
    fn lines_reach_every_edge_they_should() {
        let (w, h) = (10, 24);
        let v = draw('│', w, h, 2.0).unwrap();
        assert!(at(&v, 5, 0) > 200 && at(&v, 5, h - 1) > 200, "│ spans the full height");
        assert_eq!(at(&v, 0, 12), 0);
        let cross = draw('┼', w, h, 2.0).unwrap();
        assert!(at(&cross, 0, 12) > 200 && at(&cross, w - 1, 12) > 200 && at(&cross, 5, 0) > 200);
        let corner = draw('┌', w, h, 2.0).unwrap();
        assert!(at(&corner, w - 1, 12) > 200 && at(&corner, 5, h - 1) > 200);
        assert_eq!(at(&corner, 0, 12), 0);
        assert_eq!(at(&corner, 5, 0), 0);
    }

    #[test]
    fn rounded_corners_join_their_neighbours() {
        let (w, h) = (10, 24);
        let r = draw('╭', w, h, 2.0).unwrap();
        assert!(at(&r, w - 1, 12) > 100, "reaches the right edge on the centre row");
        assert!(at(&r, 5, h - 1) > 100, "reaches the bottom edge on the centre column");
        assert_eq!(at(&r, 0, 0), 0);
    }

    #[test]
    fn blocks_fill_exactly() {
        let full = draw('█', 8, 16, 1.0).unwrap();
        assert!(full.data.iter().all(|&a| a == 255));
        let lower = draw('▄', 8, 16, 1.0).unwrap();
        assert_eq!(at(&lower, 3, 7), 0);
        assert_eq!(at(&lower, 3, 8), 255);
        assert!(draw('a', 8, 16, 1.0).is_none());
    }
}
