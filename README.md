# runeterm

A terminal built on [Rune](https://github.com/anomalyco/rune) for the window, layout and
animation, and [libghostty-vt](https://github.com/ghostty-org/ghostty) 1.0 for the terminal
itself. Each window is a [spooky](https://github.com/ghostty-org/ghostty) session: the daemon
owns every pty, so splits and tabs outlive the window and come back after a reboot.

- Splits are Rune surfaces (rounded, shadowed, animated); the grid is drawn into each one by
  its own wgpu pipeline, clipped to the surface's corners.
- Kitty graphics, box drawing drawn to the cell, colour emoji and font fallback.
- Ghostty-compatible custom shaders (`custom-shader`), including cursor shaders.
- A sidebar of tabs and their splits, a session picker (new, restore, change emoji, kill),
  minimized splits as floating pills.

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

## Use

- `runeterm` opens a new session; `runeterm attach NAME` opens an existing one.
- Settings: `~/.config/runeterm/config` (written from Ghostty's on first run). ⌥R reloads.
- Keys: ⌥[ / ⌥- split, ⌥h/j/k/l focus, ⌥⇧h/j/k/l swap, ⌥w/a/s/d resize, ⌥↩ zoom, ⌥M minimize,
  ⌥X close, ⌥T new tab, ⌥1–9 tabs, ⌥/ pin or unpin the sidebar, ⌥E open the scrollback in
  an editor (`scrollback-editor`).
- Mouse: drag to select (twice for a word, three times for a line), Ctrl+Shift+C / Ctrl+Shift+V
  copy and paste, middle-click pastes the selection, Ctrl+click opens links, drag the gap between
  splits to resize them. Programs that use the mouse get it; hold Shift to select instead.
- Crashes are logged to `~/.local/state/runeterm/crash.log`.
