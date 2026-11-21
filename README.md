# runeterm

A terminal built on [Rune](https://github.com/anomalyco/rune) for the window, layout and
animation, and [libghostty-vt](https://github.com/ghostty-org/ghostty) 1.0 for the terminal
itself. Each window is a [spooky](https://github.com/ghostty-org/ghostty) session: the daemon
owns every pty, so splits and tabs outlive the window and come back after a reboot.

- Splits are Rune surfaces (rounded, shadowed, animated); the grid is drawn into each one by
  its own wgpu pipeline, clipped to the surface's corners.
- Kitty graphics, box drawing drawn to the cell, colour emoji and font fallback.
- Ghostty-compatible custom shaders (`custom-shader`), including cursor shaders.
- A sidebar of tabs and their splits, a session picker (new, restore, rename, change animal,
  kill), minimized splits as pills, and splits that float above the others.
- Each session has its own animal emoji (spooky's animals, in colour) and its own accent
  (picked so no two sessions share one, kept in `~/.local/state/runeterm/accents`).

## Build

libghostty-vt is built once into `vendor/`:

```sh
git clone --depth 1 --branch vt-1.0.0 https://github.com/ghostty-org/ghostty vendor/ghostty
(cd vendor/ghostty && zig build -Demit-lib-vt -Doptimize=ReleaseFast --prefix "$PWD/../ghostty-vt")
cargo build --release -p runeterm
```

Set `GHOSTTY_VT_PREFIX` to use a libghostty-vt installed elsewhere. Building the bindings needs
libclang.

Sessions need the `spooky` in [`spooky/`](spooky/), which accepts several controllers; build it
after libghostty-vt with `cd spooky && zig build -Doptimize=ReleaseFast` (see its README). Without
a daemon, runeterm runs plain local shells (`RUNETERM_LOCAL=1` forces that).

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

- `runeterm` opens a new session, first asking whether to open one of the detached or saved
  sessions instead if there are any (↑↓, Enter, Esc for a new one); `runeterm attach NAME`
  opens one straight away. New sessions get two-word names (`quiet-river`).
- Rename a session from the picker (the pencil on its row) or by double-clicking its name in
  the sidebar; change its animal by clicking its emoji (or right-clicking the badge).
- Settings: `~/.config/runeterm/config` (written from Ghostty's on first run). ⌥R reloads.
- Fonts as in Ghostty: `font-family`, and per style `font-style` / `font-style-bold` /
  `font-style-italic` / `font-style-bold-italic` (a face by name, `Light`, `Medium Italic`, or
  `false` for the regular face) and `font-family-bold` / `-italic` / `-bold-italic`.
  Runs of text are shaped, so ligatures and contextual alternates show as in Ghostty;
  `font-feature = -calt, -liga` turns them off, `ss01` turns a stylistic set on.
  `font-variation` (and `-bold`, `-italic`, `-bold-italic`) sets a variable font's axes
  (`wght=450`). A family with no bold or italic face gets a made-up one (emboldened, slanted);
  `font-synthetic-style = false` (or `no-bold`, `no-italic`, `no-bold-italic`) turns that off.
- `font-size-fit = 3%` lets the line height (and, if that's not enough, the font size) move by
  up to that much so the lines fill the splits with no blank strip under the last one; the
  appearance section shows what it chose. 0 (the default) keeps them exact. Dividers snap to
  whole lines and columns once you let go of them (or the window settles), so a split never
  ends in a part of a line; with `font-size-fit` the last split in a column comes out whole too.
- Keys: ⌥P opens the command palette, which lists every action with its key (type to filter,
  ↑↓ and Enter to run). ⌥[ / ⌥- split, ⌥h/j/k/l focus, ⌥⇧h/j/k/l swap, ⌥w/a/s/d resize,
  ⌥Z (or ⌥↩) full screen, ⌥M minimize (⌥⇧M brings the last one back), ⌥F float or dock (drag a
  floating split by its header, resize it from its corner), ⌥⇧F pin a floating split so it stays
  in view on every tab, ⌥X close, ⌥T new tab, ⌥1–9 tabs, ⌥/ pin or unpin the sidebar, ⌥E open
  the scrollback in an editor (`scrollback-editor`).
- Rebind them in the config, Ghostty-style: `keybind = alt+n=minimize`, `keybind = alt+m=unbind`
  to give a key back to the terminal, `keybind = clear` to drop every default. A new config
  lists every default under `Keybinds`, with each action's name.
- Mouse: drag to select (twice for a word, three times for a line), Ctrl+Shift+C / Ctrl+Shift+V
  copy and paste, middle-click pastes the selection, Ctrl+click opens links, drag the gap between
  splits to resize them. Programs that use the mouse get it; hold Shift to select instead.
- Scrolling is smooth: the grid follows a trackpad to the pixel and coasts on after a flick, a
  mouse wheel's notches glide in (three lines each), and it settles on a whole line.
- Crashes are logged to `~/.local/state/runeterm/crash.log`.
