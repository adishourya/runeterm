# runeterm

A terminal built on [Rune](https://github.com/anomalyco/rune) for the window, layout and
animation, and [libghostty-vt](https://github.com/ghostty-org/ghostty) 1.0 for the terminal
itself. Each window is a [spooky](https://github.com/ghostty-org/ghostty) session: the daemon
owns every pty, so splits and tabs outlive the window and come back after a reboot.

- Splits are Rune surfaces (rounded, shadowed, animated); the grid is drawn into each one by
  its own wgpu pipeline, clipped to the surface's corners.
- Kitty graphics, box drawing drawn to the cell, colour emoji and font fallback.
- Ghostty-compatible custom shaders (`custom-shader`), including cursor shaders.
- A sidebar of tabs and their splits, a session picker (new, restore, change mascot, kill),
  minimized splits as floating pills.
- Each session has its own accent (picked so no two sessions share one, kept in
  `~/.local/state/runeterm/accents`) and an animated mascot.

## Mascots

Every session gets a little animated mascot, one for each of spooky's animals:

bee · bird · butterfly · cat · crab · dino · dog · dolphin · duck · elephant · fox · frog ·
hedgehog · jellyfish · koala · lion · monkey · octopus · otter · owl · panda · parrot · paw ·
penguin · rabbit · shark · sloth · snail · tiger · turtle · unicorn · whale

- A mascot rests on a still frame and plays when you hover it, and plays through once every
  10 seconds on its own. At rest it costs one small image and no redraws; its frames are
  decoded only while it plays.
- Change one in the session picker (click a session's mascot), or right-click the badge for
  your own session's. A
  session whose emoji has no mascot (an old one with a sports emoji, say) is given one nobody
  else has.
- They're free animations from [LottieFiles](https://lottiefiles.com) under the Lottie Simple
  License, pre-rendered to frame sheets in `assets/animations/` (sources in
  `assets/animations/SOURCES.md`).

## Build

libghostty-vt is built once into `vendor/`:

```sh
git clone --depth 1 --branch vt-1.0.0 https://github.com/ghostty-org/ghostty vendor/ghostty
(cd vendor/ghostty && zig build -Demit-lib-vt -Doptimize=ReleaseFast --prefix "$PWD/../ghostty-vt")
cargo build --release -p runeterm
```

Set `GHOSTTY_VT_PREFIX` to use a libghostty-vt installed elsewhere. Building the bindings needs
libclang.

Sessions need a `spooky` that accepts several controllers (the `spooky-multi-controller`
branch); without a daemon, runeterm runs plain local shells (`RUNETERM_LOCAL=1` forces that).

## Performance

`runeterm --bench` measures the hot paths in a few seconds, without a window. On a laptop
(release build, libghostty-vt 1.0.0):

| | |
|---|---|
| Parsing plain lines | 252 MB/s |
| Parsing coloured `ls`-style lines | 159 MB/s |
| Parsing full-screen redraws (htop-like) | 412 MB/s |
| Building a full 200×60 frame (6,400 quads) | 0.77 ms |
| Building a frame with one changed line | 0.014 ms |
| `seq 1 2000000` through a bare pty to /dev/null | 26.9 MB/s |
| … into runeterm on a local pty | 28.3 MB/s |
| … into runeterm through spooky | 26.8 MB/s |

Parsing runs 6–15× faster than a pty delivers, a full redraw uses under a tenth of a 120 Hz
frame (GPU time not included), and end to end runeterm keeps up with the pty itself, through
spooky too.

## Use

- `runeterm` opens a new session; `runeterm attach NAME` opens an existing one.
- Settings: `~/.config/runeterm/config` (written from Ghostty's on first run). ⌥R reloads.
- Keys: ⌥[ / ⌥- split, ⌥h/j/k/l focus, ⌥⇧h/j/k/l swap, ⌥w/a/s/d resize, ⌥Z (or ⌥↩) full screen, ⌥M minimize,
  ⌥X close, ⌥T new tab, ⌥1–9 tabs, ⌥/ pin or unpin the sidebar, ⌥E open the scrollback in
  an editor (`scrollback-editor`).
- Mouse: drag to select (twice for a word, three times for a line), Ctrl+Shift+C / Ctrl+Shift+V
  copy and paste, middle-click pastes the selection, Ctrl+click opens links, drag the gap between
  splits to resize them. Programs that use the mouse get it; hold Shift to select instead.
- Crashes are logged to `~/.local/state/runeterm/crash.log`.
