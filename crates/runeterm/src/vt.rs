//! Safe wrappers over libghostty-vt 1.x: a `Terminal` fed with pty bytes, and a `RenderState`
//! that copies what a frame needs out of it. All FFI lives here.

use ghostty_vt_sys as sys;
use std::{ffi::c_void, mem, ptr};

/// Lets the terminal accept PNG images (Kitty `f=100`). Call once at startup, before any
/// terminal exists.
pub fn install_png_decoder() {
    unsafe extern "C" fn decode(
        _: *mut c_void,
        alloc: *const sys::GhosttyAllocator,
        data: *const u8,
        len: usize,
        out: *mut sys::GhosttySysImage,
    ) -> bool {
        // SAFETY: the library passes valid input and output pointers for this call.
        unsafe {
            let bytes = std::slice::from_raw_parts(data, len);
            let mut dec = png::Decoder::new(std::io::Cursor::new(bytes));
            dec.set_transformations(png::Transformations::normalize_to_color8() | png::Transformations::ALPHA);
            let Ok(mut reader) = dec.read_info() else { return false };
            let Some(size) = reader.output_buffer_size() else { return false };
            let mut buf = vec![0u8; size];
            let Ok(info) = reader.next_frame(&mut buf) else { return false };
            let (w, h) = (info.width, info.height);
            let rgba: Vec<u8> = match info.color_type {
                png::ColorType::Rgba => buf[..(w * h * 4) as usize].to_vec(),
                png::ColorType::GrayscaleAlpha => buf[..(w * h * 2) as usize].chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
                _ => return false,
            };
            let dst = sys::ghostty_alloc(alloc, rgba.len());
            if dst.is_null() {
                return false;
            }
            ptr::copy_nonoverlapping(rgba.as_ptr(), dst, rgba.len());
            (*out).width = w;
            (*out).height = h;
            (*out).data = dst;
            (*out).data_len = rgba.len();
            true
        }
    }
    // SAFETY: called once at startup before other threads use the library.
    unsafe { sys::ghostty_sys_set(sys::GHOSTTY_SYS_OPT_DECODE_PNG, decode as *const c_void) };
}

/// The library's version string, e.g. "1.0.0".
pub fn version() -> String {
    let mut s = sys::GhosttyString::default();
    // SAFETY: writes a borrowed static string into `s`.
    unsafe { sys::ghostty_build_info(sys::GHOSTTY_BUILD_INFO_VERSION_STRING, &mut s as *mut _ as *mut c_void) };
    string(s)
}

fn string(s: sys::GhosttyString) -> String {
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    // SAFETY: the library hands out ptr/len valid until the next mutation; we copy at once.
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(s.ptr, s.len) }).into_owned()
}

fn check(r: sys::GhosttyResult, what: &str) {
    assert!(r == sys::GHOSTTY_SUCCESS, "{what} failed: {r}");
}

/// Events the terminal raises while bytes are written into it.
pub trait Handler: Send {
    /// Bytes to send back to the program (answers to queries such as DA or DSR).
    fn write_pty(&mut self, _data: &[u8]) {}
    fn title_changed(&mut self) {}
    fn pwd_changed(&mut self) {}
    fn bell(&mut self) {}
    /// OSC 9 / OSC 777 desktop notifications (e.g. an agent waiting for input).
    fn notify(&mut self, _title: &str, _body: &str) {}
}

/// What the callbacks reach through the userdata pointer.
struct Ud {
    handler: Box<dyn Handler>,
    size: sys::GhosttySizeReportSize,
}

/// One terminal: screen, scrollback, modes. Feed it with `write`.
pub struct Terminal {
    raw: sys::GhosttyTerminal,
    // Boxed so the userdata pointer stays put while `Terminal` moves.
    ud: Box<Ud>,
}

// SAFETY: the terminal is only touched through `&mut self` (callers wrap it in a Mutex).
unsafe impl Send for Terminal {}

impl Terminal {
    pub fn new(cols: u16, rows: u16, handler: impl Handler + 'static) -> Self {
        let mut raw = ptr::null_mut();
        // SAFETY: a null allocator selects the library's default.
        check(unsafe { sys::ghostty_terminal_new(ptr::null(), &mut raw, cols, rows) }, "terminal_new");
        let size = sys::GhosttySizeReportSize { rows, columns: cols, cell_width: 0, cell_height: 0 };
        let mut t = Terminal { raw, ud: Box::new(Ud { handler: Box::new(handler), size }) };
        let ud: *mut Ud = &mut *t.ud;
        t.set(sys::GHOSTTY_TERMINAL_OPT_USERDATA, ud as *const c_void);
        t.set(sys::GHOSTTY_TERMINAL_OPT_SIZE, on_size as *const c_void);
        t.set(sys::GHOSTTY_TERMINAL_OPT_WRITE_PTY, on_write_pty as *const c_void);
        t.set(sys::GHOSTTY_TERMINAL_OPT_TITLE_CHANGED, on_title as *const c_void);
        t.set(sys::GHOSTTY_TERMINAL_OPT_PWD_CHANGED, on_pwd as *const c_void);
        t.set(sys::GHOSTTY_TERMINAL_OPT_BELL, on_bell as *const c_void);
        t.set(sys::GHOSTTY_TERMINAL_OPT_DESKTOP_NOTIFICATION, on_notify as *const c_void);
        t
    }

    fn set(&mut self, opt: sys::GhosttyTerminalOption, value: *const c_void) {
        // SAFETY: each option's value type matches the header's "Input type".
        check(unsafe { sys::ghostty_terminal_set(self.raw, opt, value) }, "terminal_set");
    }

    /// Sets the scrollback limit in bytes.
    pub fn set_scrollback_bytes(&mut self, bytes: usize) {
        self.set(sys::GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &bytes as *const usize as *const c_void);
    }

    /// Parses bytes the program wrote.
    pub fn write(&mut self, data: &[u8]) {
        // SAFETY: data is valid for len bytes; callbacks run synchronously on this thread.
        unsafe { sys::ghostty_terminal_vt_write(self.raw, data.as_ptr(), data.len()) }
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) {
        // SAFETY: plain values.
        check(unsafe { sys::ghostty_terminal_resize(self.raw, cols, rows, cell_w, cell_h) }, "terminal_resize");
        self.ud.size = sys::GhosttySizeReportSize { rows, columns: cols, cell_width: cell_w, cell_height: cell_h };
    }

    fn get_string(&self, data: sys::GhosttyTerminalData) -> String {
        let mut s = sys::GhosttyString::default();
        // SAFETY: TITLE and PWD write a GhosttyString borrowed from the terminal.
        let r = unsafe { sys::ghostty_terminal_get(self.raw, data, &mut s as *mut _ as *mut c_void) };
        if r == sys::GHOSTTY_SUCCESS { string(s) } else { String::new() }
    }

    pub fn title(&self) -> String {
        self.get_string(sys::GHOSTTY_TERMINAL_DATA_TITLE)
    }

    pub fn pwd(&self) -> String {
        self.get_string(sys::GHOSTTY_TERMINAL_DATA_PWD)
    }

    /// Default colors: foreground, background, cursor and the first 16 palette entries.
    pub fn set_colors(&mut self, fg: Rgb, bg: Rgb, cursor: Rgb, ansi: &[Rgb; 16]) {
        let c = |c: Rgb| sys::GhosttyColorRgb { r: c.0, g: c.1, b: c.2 };
        self.set(sys::GHOSTTY_TERMINAL_OPT_COLOR_FOREGROUND, &c(fg) as *const _ as *const c_void);
        self.set(sys::GHOSTTY_TERMINAL_OPT_COLOR_BACKGROUND, &c(bg) as *const _ as *const c_void);
        self.set(sys::GHOSTTY_TERMINAL_OPT_COLOR_CURSOR, &c(cursor) as *const _ as *const c_void);
        let mut palette = [sys::GhosttyColorRgb::default(); 256];
        // SAFETY: PALETTE_DEFAULT writes 256 colors.
        unsafe {
            sys::ghostty_terminal_get(
                self.raw,
                sys::GHOSTTY_TERMINAL_DATA_COLOR_PALETTE_DEFAULT,
                palette.as_mut_ptr() as *mut c_void,
            )
        };
        for (slot, &rgb) in palette.iter_mut().zip(ansi) {
            *slot = c(rgb);
        }
        self.set(sys::GHOSTTY_TERMINAL_OPT_COLOR_PALETTE, &palette as *const _ as *const c_void);
    }

    /// Turns on Kitty graphics: a storage limit, and loading from files, temp files and shared
    /// memory (which local programs use to send large images quickly).
    pub fn enable_images(&mut self, limit: u64) {
        self.set(sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_STORAGE_LIMIT, &limit as *const _ as *const c_void);
        let on = true;
        self.set(sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_FILE, &on as *const _ as *const c_void);
        self.set(sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_SHARED_MEM, &on as *const _ as *const c_void);
        let tmp = std::env::temp_dir().to_string_lossy().into_owned();
        let dir = sys::GhosttyString { ptr: tmp.as_ptr(), len: tmp.len() };
        self.set(sys::GHOSTTY_TERMINAL_OPT_KITTY_IMAGE_MEDIUM_TEMP_FILE, &dir as *const _ as *const c_void);
    }

    /// An image's pixels as RGBA, from the active screen's storage.
    pub fn image_rgba(&self, id: u32) -> Option<(u32, u32, Vec<u8>)> {
        // SAFETY: handles are borrowed from the terminal and read before it changes.
        unsafe {
            let mut g: sys::GhosttyKittyGraphics = ptr::null_mut();
            sys::ghostty_terminal_get(self.raw, sys::GHOSTTY_TERMINAL_DATA_KITTY_GRAPHICS, &mut g as *mut _ as *mut c_void);
            if g.is_null() {
                return None;
            }
            let img = sys::ghostty_kitty_graphics_image(g, id);
            if img.is_null() {
                return None;
            }
            let get = |k, out: *mut c_void| sys::ghostty_kitty_graphics_image_get(img, k, out);
            let (mut w, mut h) = (0u32, 0u32);
            let mut format: sys::GhosttyKittyImageFormat = 0;
            let mut px = sys::GhosttyString::default();
            get(sys::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_WIDTH, &mut w as *mut _ as *mut c_void);
            get(sys::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_HEIGHT, &mut h as *mut _ as *mut c_void);
            get(sys::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_FORMAT, &mut format as *mut _ as *mut c_void);
            if get(sys::GHOSTTY_KITTY_GRAPHICS_IMAGE_DATA_PIXELS, &mut px as *mut _ as *mut c_void) != sys::GHOSTTY_SUCCESS
                || px.ptr.is_null()
            {
                return None;
            }
            let src = std::slice::from_raw_parts(px.ptr, px.len);
            let n = (w * h) as usize;
            let rgba: Vec<u8> = match format {
                sys::GHOSTTY_KITTY_IMAGE_FORMAT_RGBA => src.to_vec(),
                sys::GHOSTTY_KITTY_IMAGE_FORMAT_RGB => src.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
                sys::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY_ALPHA => src.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
                sys::GHOSTTY_KITTY_IMAGE_FORMAT_GRAY => src.iter().flat_map(|&g| [g, g, g, 255]).collect(),
                _ => return None,
            };
            (rgba.len() == n * 4).then_some((w, h, rgba))
        }
    }

    /// Everything the terminal holds as plain text, scrollback included, with soft-wrapped
    /// lines joined and trailing blanks trimmed: what ⌥E opens in an editor.
    pub fn text(&self) -> String {
        // SAFETY: the formatter borrows the terminal for this call only; the buffer it
        // allocates is copied and freed here.
        unsafe {
            let mut f: sys::GhosttyFormatter = ptr::null_mut();
            if sys::ghostty_formatter_terminal_new(ptr::null(), &mut f, self.raw) != sys::GHOSTTY_SUCCESS {
                return String::new();
            }
            let yes = true;
            sys::ghostty_formatter_set(f, sys::GHOSTTY_FORMATTER_OPT_UNWRAP, &yes as *const _ as *const c_void);
            let (mut out, mut len) = (ptr::null_mut(), 0usize);
            let r = sys::ghostty_formatter_format_alloc(f, ptr::null(), &mut out, &mut len);
            let text = if r == sys::GHOSTTY_SUCCESS && !out.is_null() {
                let t = String::from_utf8_lossy(std::slice::from_raw_parts(out, len)).into_owned();
                sys::ghostty_free(ptr::null(), out, len);
                t
            } else {
                String::new()
            };
            sys::ghostty_formatter_free(f);
            text
        }
    }

    /// Scrolls the viewport by `rows` (negative is up, into scrollback).
    pub fn scroll(&mut self, rows: isize) {
        let mut b = sys::GhosttyTerminalScrollViewport::default();
        b.tag = sys::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_DELTA;
        b.value.delta = rows;
        // SAFETY: a tagged value of a known tag.
        unsafe { sys::ghostty_terminal_scroll_viewport(self.raw, b) };
    }

    pub fn scroll_to_bottom(&mut self) {
        let mut b = sys::GhosttyTerminalScrollViewport::default();
        b.tag = sys::GHOSTTY_TERMINAL_SCROLL_VIEWPORT_BOTTOM;
        // SAFETY: a tagged value of a known tag.
        unsafe { sys::ghostty_terminal_scroll_viewport(self.raw, b) };
    }

    /// A DEC private mode (e.g. 2004 bracketed paste).
    pub fn dec_mode(&self, n: u16) -> bool {
        let mut m = sys::GhosttyTerminalModeConfig { mode: n & 0x7fff, value: false };
        // SAFETY: MODE reads and writes a GhosttyTerminalModeConfig.
        unsafe { sys::ghostty_terminal_get(self.raw, sys::GHOSTTY_TERMINAL_DATA_MODE, &mut m as *mut _ as *mut c_void) };
        m.value
    }

    /// Text to paste, encoded for the program: control bytes stripped, framed when it asked for
    /// bracketed paste.
    pub fn paste_bytes(&self, text: &str) -> Vec<u8> {
        let mut data = text.as_bytes().to_vec();
        let bracketed = self.dec_mode(2004);
        let mut need = 0usize;
        // SAFETY: data is valid for its length; the first call only measures.
        unsafe {
            sys::ghostty_paste_encode(data.as_mut_ptr() as *mut _, data.len(), bracketed, ptr::null_mut(), 0, &mut need);
            let mut out = vec![0u8; need];
            let mut n = 0usize;
            sys::ghostty_paste_encode(data.as_mut_ptr() as *mut _, data.len(), bracketed, out.as_mut_ptr(), out.len(), &mut n);
            out.truncate(n);
            out
        }
    }

    pub fn set_default_cursor(&mut self, style: CursorStyle, blink: bool) {
        let s: sys::GhosttyCursorStyle = match style {
            CursorStyle::Bar => sys::GHOSTTY_CURSOR_STYLE_BAR,
            CursorStyle::Block => sys::GHOSTTY_CURSOR_STYLE_BLOCK,
            CursorStyle::Underline => sys::GHOSTTY_CURSOR_STYLE_UNDERLINE,
            CursorStyle::BlockHollow => sys::GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW,
        };
        self.set(sys::GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_STYLE, &s as *const _ as *const c_void);
        self.set(sys::GHOSTTY_TERMINAL_OPT_DEFAULT_CURSOR_BLINK, &blink as *const _ as *const c_void);
    }

    /// Whether the program asked for mouse events (then the wheel is the program's).
    pub fn mouse_tracking(&self) -> bool {
        let mut on = false;
        // SAFETY: MOUSE_TRACKING writes a bool.
        unsafe { sys::ghostty_terminal_get(self.raw, sys::GHOSTTY_TERMINAL_DATA_MOUSE_TRACKING, &mut on as *mut _ as *mut c_void) };
        on
    }

    pub fn size(&self) -> (u16, u16) {
        let (mut c, mut r) = (0u16, 0u16);
        // SAFETY: COLS/ROWS write a u16.
        unsafe {
            sys::ghostty_terminal_get(self.raw, sys::GHOSTTY_TERMINAL_DATA_COLS, &mut c as *mut _ as *mut c_void);
            sys::ghostty_terminal_get(self.raw, sys::GHOSTTY_TERMINAL_DATA_ROWS, &mut r as *mut _ as *mut c_void);
        }
        (c, r)
    }
}

/// Modifier keys, as the key encoder wants them.
#[derive(Clone, Copy, Debug, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub logo: bool,
}

impl Mods {
    fn bits(self) -> sys::GhosttyMods {
        let mut m = 0;
        if self.shift { m |= sys::GHOSTTY_MODS_SHIFT; }
        if self.ctrl { m |= sys::GHOSTTY_MODS_CTRL; }
        if self.alt { m |= sys::GHOSTTY_MODS_ALT; }
        if self.logo { m |= sys::GHOSTTY_MODS_SUPER; }
        m as sys::GhosttyMods
    }
}

/// Turns key presses into the bytes the program expects, following its modes (kitty keyboard,
/// application cursor keys, modifyOtherKeys).
pub struct KeyEncoder {
    enc: sys::GhosttyKeyEncoder,
    ev: sys::GhosttyKeyEvent,
}

// SAFETY: owned handles, used from one thread at a time.
unsafe impl Send for KeyEncoder {}

impl Default for KeyEncoder {
    fn default() -> Self {
        let (mut enc, mut ev) = (ptr::null_mut(), ptr::null_mut());
        // SAFETY: default allocator; freed in Drop.
        unsafe {
            check(sys::ghostty_key_encoder_new(ptr::null(), &mut enc), "key_encoder_new");
            check(sys::ghostty_key_event_new(ptr::null(), &mut ev), "key_event_new");
        }
        KeyEncoder { enc, ev }
    }
}

impl KeyEncoder {
    /// Encodes one press. `key` is a `GHOSTTY_KEY_*`, `text` what the layout typed (empty for
    /// keys that type nothing), `unshifted` the key's unshifted codepoint (0 if none).
    pub fn encode(&mut self, t: &Terminal, key: sys::GhosttyKey, mods: Mods, text: &str, unshifted: u32) -> Vec<u8> {
        let mut buf = [0u8; 128];
        let mut n = 0usize;
        // SAFETY: `text` outlives the encode call; the event only borrows it until then.
        unsafe {
            sys::ghostty_key_encoder_set_from_terminal(self.enc, t.raw);
            sys::ghostty_key_event_set_action(self.ev, sys::GHOSTTY_KEY_ACTION_PRESS);
            sys::ghostty_key_event_set_key(self.ev, key);
            sys::ghostty_key_event_set_mods(self.ev, mods.bits());
            let consumed = if mods.shift && !text.is_empty() { sys::GHOSTTY_MODS_SHIFT as sys::GhosttyMods } else { 0 };
            sys::ghostty_key_event_set_consumed_mods(self.ev, consumed);
            sys::ghostty_key_event_set_utf8(self.ev, sys::GhosttyString { ptr: text.as_ptr(), len: text.len() });
            sys::ghostty_key_event_set_unshifted_codepoint(self.ev, unshifted);
            sys::ghostty_key_encoder_encode(self.enc, self.ev, buf.as_mut_ptr(), buf.len(), &mut n);
            sys::ghostty_key_event_set_utf8(self.ev, sys::GhosttyString { ptr: ptr::null(), len: 0 });
        }
        buf[..n.min(buf.len())].to_vec()
    }
}

impl Drop for KeyEncoder {
    fn drop(&mut self) {
        // SAFETY: freed once.
        unsafe {
            sys::ghostty_key_event_free(self.ev);
            sys::ghostty_key_encoder_free(self.enc);
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: raw came from terminal_new and is freed once.
        unsafe { sys::ghostty_terminal_free(self.raw) }
    }
}

fn handler<'a>(ud: *mut c_void) -> &'a mut dyn Handler {
    // SAFETY: ud is the `Ud` set as USERDATA, alive as long as the terminal.
    unsafe { &mut *(*(ud as *mut Ud)).handler }
}

/// Answers size queries (CSI 14/16/18 t, and what programs use to size images).
unsafe extern "C" fn on_size(_: sys::GhosttyTerminal, ud: *mut c_void, out: *mut sys::GhosttySizeReportSize) -> bool {
    // SAFETY: ud is our `Ud`; out is valid for one write.
    unsafe {
        let size = (*(ud as *mut Ud)).size;
        *out = size;
        size.cell_width > 0
    }
}

unsafe extern "C" fn on_write_pty(_: sys::GhosttyTerminal, ud: *mut c_void, data: *const u8, len: usize) {
    // SAFETY: the library passes a valid slice for the duration of the call.
    handler(ud).write_pty(unsafe { std::slice::from_raw_parts(data, len) });
}
unsafe extern "C" fn on_title(_: sys::GhosttyTerminal, ud: *mut c_void) {
    handler(ud).title_changed();
}
unsafe extern "C" fn on_pwd(_: sys::GhosttyTerminal, ud: *mut c_void) {
    handler(ud).pwd_changed();
}
unsafe extern "C" fn on_bell(_: sys::GhosttyTerminal, ud: *mut c_void) {
    handler(ud).bell();
}
unsafe extern "C" fn on_notify(_: sys::GhosttyTerminal, ud: *mut c_void, n: *const sys::GhosttyTerminalDesktopNotification) {
    // SAFETY: the notification is valid for this call.
    let (title, body) = unsafe { (string((*n).title), string((*n).body)) };
    handler(ud).notify(&title, &body);
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<sys::GhosttyColorRgb> for Rgb {
    fn from(c: sys::GhosttyColorRgb) -> Self {
        Rgb(c.r, c.g, c.b)
    }
}

/// One cell, ready to draw: its text and resolved colors.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    /// The grapheme (empty for a blank cell).
    pub text: String,
    pub fg: Rgb,
    /// None draws the default background (lets the pane surface show through).
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorStyle {
    #[default]
    Bar,
    Block,
    Underline,
    BlockHollow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cursor {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub blinking: bool,
    pub style: CursorStyle,
}

/// Where an image layer goes relative to the cells.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layer {
    BelowBackground,
    BelowText,
    AboveText,
}

/// One rectangle of one image to draw, already scaled and positioned by the terminal.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub image: u32,
    pub generation: u64,
    pub layer: Layer,
    pub z: i32,
    /// Top-left cell (may be negative when scrolled partly out of view) plus a pixel offset.
    pub col: i32,
    pub row: i32,
    pub offset: [u32; 2],
    /// Size on screen, in pixels.
    pub size: [u32; 2],
    /// The part of the image shown, in image pixels.
    pub src: [u32; 4],
}

/// What a renderer needs from one frame. Rows are only filled when dirty.
pub struct Frame {
    /// The images on screen, sorted by layer and z; `Some` only when the list changed.
    pub images: Option<Vec<Placement>>,
    pub cols: u16,
    pub rows: u16,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Cursor,
    /// `rows[y]` is `Some` when that row changed since the last `clean`.
    pub dirty_rows: Vec<Option<Vec<Cell>>>,
    pub full: bool,
}

/// Copies what to draw out of a terminal. Keep one per pane and reuse it: updates only redo what
/// changed.
pub struct RenderState {
    raw: sys::GhosttyRenderState,
    rows: sys::GhosttyRenderStateRowIterator,
    cells: sys::GhosttyRenderStateRowCells,
    images: sys::GhosttyRenderStateImageIterator,
    images_generation: Option<u64>,
}

// SAFETY: owned handles, used from one thread at a time.
unsafe impl Send for RenderState {}

impl Default for RenderState {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderState {
    pub fn new() -> Self {
        let (mut raw, mut rows, mut cells, mut images) = (ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), ptr::null_mut());
        // SAFETY: default allocator; handles freed in Drop.
        unsafe {
            check(sys::ghostty_render_state_new(ptr::null(), &mut raw), "render_state_new");
            check(sys::ghostty_render_state_row_iterator_new(ptr::null(), &mut rows), "row_iterator_new");
            check(sys::ghostty_render_state_row_cells_new(ptr::null(), &mut cells), "row_cells_new");
            check(sys::ghostty_render_state_image_iterator_new(ptr::null(), &mut images), "image_iterator_new");
        }
        RenderState { raw, rows, cells, images, images_generation: None }
    }

    /// The part that needs the terminal; hold its lock only around this.
    pub fn begin(&mut self, t: &Terminal) {
        // SAFETY: both handles are live.
        check(unsafe { sys::ghostty_render_state_begin_update(self.raw, t.raw) }, "begin_update");
    }

    /// Finishes the update off the terminal lock and reads out the frame, then marks it clean.
    pub fn finish(&mut self) -> Frame {
        // SAFETY: all reads below use out-params of the types the header documents.
        unsafe {
            check(sys::ghostty_render_state_end_update(self.raw), "end_update");
            let raw = self.raw;
            let get = |d, out: *mut c_void| sys::ghostty_render_state_get(raw, d, out);

            let mut dirty: sys::GhosttyRenderStateDirty = 0;
            get(sys::GHOSTTY_RENDER_STATE_DATA_DIRTY, &mut dirty as *mut _ as *mut c_void);
            let (mut cols, mut rows) = (0u16, 0u16);
            get(sys::GHOSTTY_RENDER_STATE_DATA_COLS, &mut cols as *mut _ as *mut c_void);
            get(sys::GHOSTTY_RENDER_STATE_DATA_ROWS, &mut rows as *mut _ as *mut c_void);

            let mut colors: sys::GhosttyRenderStateColors = mem::zeroed();
            colors.size = mem::size_of_val(&colors);
            get(sys::GHOSTTY_RENDER_STATE_DATA_COLORS, &mut colors as *mut _ as *mut c_void);

            let mut c: sys::GhosttyRenderStateCursor = mem::zeroed();
            c.size = mem::size_of_val(&c);
            get(sys::GHOSTTY_RENDER_STATE_DATA_CURSOR, &mut c as *mut _ as *mut c_void);
            let cursor = Cursor {
                x: c.viewport_x,
                y: c.viewport_y,
                visible: c.visible && c.viewport_has_value,
                blinking: c.blinking,
                style: match c.visual_style {
                    sys::GHOSTTY_CURSOR_STYLE_BLOCK => CursorStyle::Block,
                    sys::GHOSTTY_CURSOR_STYLE_UNDERLINE => CursorStyle::Underline,
                    sys::GHOSTTY_CURSOR_STYLE_BLOCK_HOLLOW => CursorStyle::BlockHollow,
                    _ => CursorStyle::Bar,
                },
            };

            let mut generation = 0u64;
            get(sys::GHOSTTY_RENDER_STATE_DATA_IMAGE_PLACEMENTS_GENERATION, &mut generation as *mut _ as *mut c_void);
            let images = if self.images_generation != Some(generation) {
                self.images_generation = Some(generation);
                Some(self.read_images())
            } else {
                None
            };

            let mut frame = Frame {
                images,
                cols,
                rows,
                fg: colors.foreground.into(),
                bg: colors.background.into(),
                cursor,
                dirty_rows: vec![None; rows as usize],
                full: dirty == sys::GHOSTTY_RENDER_STATE_DIRTY_FULL,
            };

            if dirty != sys::GHOSTTY_RENDER_STATE_DIRTY_FALSE {
                get(sys::GHOSTTY_RENDER_STATE_DATA_ROW_ITERATOR, &mut self.rows as *mut _ as *mut c_void);
                let mut y = 0u16;
                loop {
                    let more = if frame.full {
                        let ok = sys::ghostty_render_state_row_iterator_next(self.rows);
                        if ok && y < rows { true } else { false }
                    } else {
                        sys::ghostty_render_state_row_iterator_next_dirty(self.rows, &mut y)
                    };
                    if !more {
                        break;
                    }
                    let row = self.read_row(cols, colors.foreground.into(), colors.background.into());
                    if let Some(slot) = frame.dirty_rows.get_mut(y as usize) {
                        *slot = Some(row);
                    }
                    if frame.full {
                        y += 1;
                    }
                }
            }
            check(sys::ghostty_render_state_clean(self.raw), "clean");
            frame
        }
    }

    /// Reads the image list of the last update.
    unsafe fn read_images(&mut self) -> Vec<Placement> {
        let mut out = vec![];
        // SAFETY (whole fn): out-params match the documented image data types.
        unsafe {
            sys::ghostty_render_state_get(
                self.raw,
                sys::GHOSTTY_RENDER_STATE_DATA_IMAGE_ITERATOR,
                &mut self.images as *mut _ as *mut c_void,
            );
            while sys::ghostty_render_state_image_iterator_next(self.images) {
                let it = self.images;
                let get = |k, out: *mut c_void| sys::ghostty_render_state_image_get(it, k, out);
                let mut protocol: sys::GhosttyImageProtocol = 0;
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_PROTOCOL, &mut protocol as *mut _ as *mut c_void);
                if protocol != sys::GHOSTTY_IMAGE_PROTOCOL_KITTY {
                    continue;
                }
                let mut p = Placement {
                    image: 0, generation: 0, layer: Layer::AboveText, z: 0, col: 0, row: 0,
                    offset: [0; 2], size: [0; 2], src: [0; 4],
                };
                let mut layer: sys::GhosttyImageLayer = 0;
                let u = |v: &mut u32| v as *mut u32 as *mut c_void;
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_ID, u(&mut p.image));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_IMAGE_GENERATION, &mut p.generation as *mut _ as *mut c_void);
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_LAYER, &mut layer as *mut _ as *mut c_void);
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_Z, &mut p.z as *mut _ as *mut c_void);
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_COL, &mut p.col as *mut _ as *mut c_void);
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_VIEWPORT_ROW, &mut p.row as *mut _ as *mut c_void);
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_X, u(&mut p.offset[0]));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_CELL_OFFSET_Y, u(&mut p.offset[1]));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_WIDTH, u(&mut p.size[0]));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_PIXEL_HEIGHT, u(&mut p.size[1]));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_X, u(&mut p.src[0]));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_Y, u(&mut p.src[1]));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_WIDTH, u(&mut p.src[2]));
                get(sys::GHOSTTY_RENDER_STATE_IMAGE_DATA_SOURCE_HEIGHT, u(&mut p.src[3]));
                p.layer = match layer {
                    sys::GHOSTTY_IMAGE_LAYER_BELOW_BACKGROUND => Layer::BelowBackground,
                    sys::GHOSTTY_IMAGE_LAYER_BELOW_TEXT => Layer::BelowText,
                    _ => Layer::AboveText,
                };
                out.push(p);
            }
        }
        out.sort_by_key(|p| (p.layer, p.z));
        out
    }

    /// Reads the cells of the row the iterator is on.
    unsafe fn read_row(&mut self, cols: u16, default_fg: Rgb, default_bg: Rgb) -> Vec<Cell> {
        let mut out = Vec::with_capacity(cols as usize);
        // SAFETY (whole fn): out-params match the documented cell data types.
        unsafe {
            sys::ghostty_render_state_row_get(
                self.rows,
                sys::GHOSTTY_RENDER_STATE_ROW_DATA_CELLS,
                &mut self.cells as *mut _ as *mut c_void,
            );
            let cget = |d, out: *mut c_void| sys::ghostty_render_state_row_cells_get(self.cells, d, out);
            while sys::ghostty_render_state_row_cells_next(self.cells) {
                let mut cell = Cell { fg: default_fg, ..Cell::default() };
                let mut len = 0u32;
                cget(sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_LEN, &mut len as *mut _ as *mut c_void);
                if len > 0 {
                    let mut cps = [0u32; 16];
                    let mut buf = sys::GhosttyCodepointBuffer { ptr: cps.as_mut_ptr(), cap: cps.len(), len: 0 };
                    cget(sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_GRAPHEMES_BUF, &mut buf as *mut _ as *mut c_void);
                    cell.text = cps[..buf.len.min(buf.cap)].iter().filter_map(|&c| char::from_u32(c)).collect();
                }
                let mut fg = sys::GhosttyColorRgb::default();
                if cget(sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_FG_COLOR, &mut fg as *mut _ as *mut c_void)
                    == sys::GHOSTTY_SUCCESS
                {
                    cell.fg = fg.into();
                }
                let mut bg = sys::GhosttyColorRgb::default();
                if cget(sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_BG_COLOR, &mut bg as *mut _ as *mut c_void)
                    == sys::GHOSTTY_SUCCESS
                {
                    cell.bg = Some(bg.into());
                }
                let mut style = sys::GhosttyStyle::default();
                cget(sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_STYLE, &mut style as *mut _ as *mut c_void);
                cell.bold = style.flags & sys::GHOSTTY_STYLE_FLAG_BOLD != 0;
                cell.italic = style.flags & sys::GHOSTTY_STYLE_FLAG_ITALIC != 0;
                cell.underline = sys::ghostty_style_underline(&style) != sys::GHOSTTY_SGR_UNDERLINE_NONE;
                // Reverse video (SGR 7) is the renderer's to apply: swap the resolved colours,
                // defaults included. Editors draw block cursors this way.
                if style.flags & sys::GHOSTTY_STYLE_FLAG_INVERSE != 0 {
                    let (fg, bg) = (cell.fg, cell.bg.unwrap_or(default_bg));
                    cell.fg = bg;
                    cell.bg = Some(fg);
                }
                if style.flags & sys::GHOSTTY_STYLE_FLAG_INVISIBLE != 0 {
                    cell.text.clear();
                }
                let mut sel = false;
                cget(sys::GHOSTTY_RENDER_STATE_ROW_CELLS_DATA_SELECTED, &mut sel as *mut _ as *mut c_void);
                cell.selected = sel;
                out.push(cell);
            }
        }
        out
    }
}

impl Drop for RenderState {
    fn drop(&mut self) {
        // SAFETY: handles came from their _new functions and are freed once.
        unsafe {
            sys::ghostty_render_state_row_cells_free(self.cells);
            sys::ghostty_render_state_image_iterator_free(self.images);
            sys::ghostty_render_state_row_iterator_free(self.rows);
            sys::ghostty_render_state_free(self.raw);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct Rec(Arc<Mutex<(Vec<u8>, usize)>>);
    impl Handler for Rec {
        fn write_pty(&mut self, d: &[u8]) {
            self.0.lock().unwrap().0.extend_from_slice(d);
        }
        fn title_changed(&mut self) {
            self.0.lock().unwrap().1 += 1;
        }
        fn notify(&mut self, _: &str, body: &str) {
            self.0.lock().unwrap().0.extend_from_slice(format!("[notify:{body}]").as_bytes());
        }
    }

    fn text(row: &[Cell]) -> String {
        row.iter().map(|c| if c.text.is_empty() { " " } else { &c.text }).collect::<String>().trim_end().into()
    }

    #[test]
    fn reversed_cells_swap_default_colors() {
        let mut t = Terminal::new(10, 2, Rec::default());
        t.write(b"a\x1b[7mX\x1b[0mb\x1b[31;44;7mY\x1b[0m");
        let mut rs = RenderState::new();
        rs.begin(&t);
        let f = rs.finish();
        let row = f.dirty_rows[0].as_ref().unwrap();
        assert_eq!(row[0].bg, None, "plain cells keep the default background");
        assert_eq!(row[1].bg, Some(f.fg), "a reversed cell's background is the default foreground");
        assert_eq!(row[1].fg, f.bg, "and its text is the default background");
        let plain_red_on_blue = (row[3].bg.unwrap(), row[3].fg);
        assert_eq!(plain_red_on_blue.0.0 < plain_red_on_blue.0.2, false, "explicit colours swap too: red background");
        assert!(row[3].fg.2 > row[3].fg.0, "and blue text");
    }

    #[test]
    fn text_includes_the_scrollback() {
        let mut t = Terminal::new(20, 3, Rec::default());
        for i in 0..10 {
            t.write(format!("line {i}\r\n").as_bytes());
        }
        let text = t.text();
        assert!(text.contains("line 0"), "scrolled-off lines are there: {text:?}");
        assert!(text.contains("line 9"));
    }

    #[test]
    fn version_is_1x() {
        assert!(version().starts_with("1."), "{}", version());
    }

    #[test]
    fn writes_render_with_colors_and_dirty_rows() {
        let mut t = Terminal::new(20, 4, Rec::default());
        t.write(b"Hello, \x1b[1;32mworld\x1b[0m!\r\nsecond");
        let mut rs = RenderState::new();
        rs.begin(&t);
        let f = rs.finish();
        assert_eq!((f.cols, f.rows), (20, 4));
        assert_eq!(text(f.dirty_rows[0].as_ref().unwrap()), "Hello, world!");
        assert_eq!(text(f.dirty_rows[1].as_ref().unwrap()), "second");
        let w = &f.dirty_rows[0].as_ref().unwrap()[7];
        assert!(w.bold && w.text == "w");
        assert_ne!(w.fg, f.fg, "green differs from the default foreground");
        assert_eq!(f.dirty_rows[0].as_ref().unwrap()[0].fg, f.fg, "unstyled text uses the default foreground");
        assert_eq!((f.cursor.x, f.cursor.y), (6, 1));

        // Nothing changed: no rows. One row changed: only that row.
        rs.begin(&t);
        assert!(rs.finish().dirty_rows.iter().all(Option::is_none));
        t.write(b"!");
        rs.begin(&t);
        let f = rs.finish();
        let dirty: Vec<_> = f.dirty_rows.iter().enumerate().filter(|(_, r)| r.is_some()).map(|(i, _)| i).collect();
        assert_eq!(dirty, vec![1]);
    }

    #[test]
    fn encodes_keys_by_mode() {
        let t = Terminal::new(20, 4, Rec::default());
        let mut k = KeyEncoder::default();
        let none = Mods::default();
        let ctrl = Mods { ctrl: true, ..none };
        assert_eq!(k.encode(&t, sys::GHOSTTY_KEY_A, none, "a", 'a' as u32), b"a");
        assert_eq!(k.encode(&t, sys::GHOSTTY_KEY_C, ctrl, "", 'c' as u32), b"\x03");
        assert_eq!(k.encode(&t, sys::GHOSTTY_KEY_ENTER, none, "", 0), b"\r");
        assert_eq!(k.encode(&t, sys::GHOSTTY_KEY_ARROW_UP, none, "", 0), b"\x1b[A");
        let mut t = t;
        t.write(b"\x1b[?1h"); // application cursor keys
        assert_eq!(k.encode(&t, sys::GHOSTTY_KEY_ARROW_UP, none, "", 0), b"\x1bOA");
    }

    #[test]
    fn pastes_bracketed_only_when_asked() {
        let mut t = Terminal::new(20, 4, Rec::default());
        assert_eq!(t.paste_bytes("a\nb"), b"a\rb");
        t.write(b"\x1b[?2004h");
        assert_eq!(t.paste_bytes("a\x1bb"), b"\x1b[200~a b\x1b[201~");
    }

    #[test]
    fn kitty_images_reach_the_render_state() {
        let mut t = Terminal::new(20, 6, Rec::default());
        t.resize(20, 6, 10, 20);
        t.enable_images(16 << 20);
        // A 2x2 RGBA image, transmitted and displayed at the cursor (base64 of 16 bytes).
        t.write(b"\x1b_Ga=T,f=32,s=2,v=2,i=7;/wAA/wD/AP8AAP///////w==\x1b\\");
        let mut rs = RenderState::new();
        rs.begin(&t);
        let f = rs.finish();
        let imgs = f.images.expect("the list changed");
        assert_eq!(imgs.len(), 1);
        assert_eq!(imgs[0].image, 7);
        assert_eq!((imgs[0].col, imgs[0].row), (0, 0));
        let (w, h, px) = t.image_rgba(7).unwrap();
        assert_eq!((w, h, px.len()), (2, 2, 16));
        assert_eq!(&px[..4], &[255, 0, 0, 255]);
        rs.begin(&t);
        assert!(rs.finish().images.is_none(), "unchanged list is not re-sent");
    }

    #[test]
    fn reports_desktop_notifications() {
        let rec = Rec::default();
        let mut t = Terminal::new(20, 4, rec.clone());
        t.write(b"\x1b]777;notify;Claude;needs your input\x07");
        assert_eq!(rec.0.lock().unwrap().0, b"[notify:needs your input]");
    }

    #[test]
    fn answers_cell_size_queries() {
        let rec = Rec::default();
        let mut t = Terminal::new(20, 4, rec.clone());
        t.resize(20, 4, 10, 20);
        t.write(b"\x1b[16t");
        assert_eq!(rec.0.lock().unwrap().0, b"\x1b[6;20;10t");
    }

    #[test]
    fn answers_queries_and_reports_titles() {
        let rec = Rec::default();
        let mut t = Terminal::new(20, 4, rec.clone());
        t.write(b"\x1b[6n"); // where is the cursor?
        t.write(b"\x1b]2;hi there\x07");
        assert_eq!(rec.0.lock().unwrap().0, b"\x1b[1;1R");
        assert_eq!(rec.0.lock().unwrap().1, 1);
        assert_eq!(t.title(), "hi there");
    }
}
