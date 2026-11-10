//! Line icons (24-unit grid, drawn with `currentColor`) and the program logos for pane headers
//! and the tab sidebar.

use rune::{Color, Icon};

macro_rules! icon {
    ($id:ident, $name:literal, $body:literal) => {
        pub static $id: Icon = Icon {
            name: $name,
            view: 24.0,
            body: concat!(r#"<g fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">"#, $body, "</g>"),
        };
    };
}

icon!(TERMINAL, "rt-terminal", r#"<rect x="3" y="4" width="18" height="16" rx="3"/><path d="M7.5 9.5l3 2.5-3 2.5M12.5 15h4"/>"#);
icon!(SPLIT_RIGHT, "rt-split-right", r#"<rect x="3.5" y="4.5" width="17" height="15" rx="2.5"/><path d="M12 4.5v15"/>"#);
icon!(SPLIT_DOWN, "rt-split-down", r#"<rect x="3.5" y="4.5" width="17" height="15" rx="2.5"/><path d="M3.5 12h17"/>"#);
icon!(EXPAND, "rt-expand", r#"<path d="M14 4h6v6M10 20H4v-6M20 4l-6.5 6.5M4 20l6.5-6.5"/>"#);
icon!(COLLAPSE, "rt-collapse", r#"<path d="M4 14h6v6M20 10h-6V4M10 14l-6.5 6.5M14 10l6.5-6.5"/>"#);
icon!(FLOAT, "rt-float", r#"<rect x="3.5" y="8.5" width="12" height="12" rx="2.5"/><path d="M11 4h7.5A1.5 1.5 0 0 1 20 5.5V13"/>"#);
icon!(DOCK, "rt-dock", r#"<rect x="3.5" y="4.5" width="17" height="15" rx="2.5"/><path d="M12 4.5v15"/>"#);
icon!(MINIMIZE, "rt-minimize", r#"<path d="M6 17.5h12"/>"#);
icon!(RESTORE, "rt-restore", r#"<path d="M7 14l5-5 5 5"/>"#);
icon!(CHEVRON, "rt-chevron", r#"<path d="M7 10l5 5 5-5"/>"#);
icon!(PENCIL, "rt-pencil", r#"<path d="M4 20h4L19 9a2.8 2.8 0 0 0-4-4L4 16z"/><path d="M13.5 6.5l4 4"/>"#);
icon!(TRASH, "rt-trash", r#"<path d="M4 6.5h16"/><path d="M9.5 6.5V5a1.5 1.5 0 0 1 1.5-1.5h2A1.5 1.5 0 0 1 14.5 5v1.5"/><path d="M6 6.5l.9 12.1A2 2 0 0 0 8.9 20.5h6.2a2 2 0 0 0 2-1.9l.9-12.1"/><path d="M10 10.5v6M14 10.5v6"/>"#);
icon!(BACK, "rt-back", r#"<path d="M14 7l-5 5 5 5"/>"#);
icon!(BOLT, "rt-bolt", r#"<path d="M13 3L5 13.5h6L10 21l8-10.5h-6z" fill="currentColor" stroke="none"/>"#);
icon!(SUN, "rt-sun", r#"<circle cx="12" cy="12" r="4"/><path d="M12 2.5v2M12 19.5v2M4.6 4.6L6 6M18 18l1.4 1.4M2.5 12h2M19.5 12h2M4.6 19.4L6 18M18 6l1.4-1.4"/>"#);
icon!(PLUG, "rt-plug", r#"<path d="M9 2.5v4M15 2.5v4M7 6.5h10v4a5 5 0 0 1-10 0zM12 15.5v6"/>"#);
icon!(CHECK, "rt-check", r#"<path d="M5 12.5l4.5 4.5L19 7.5"/>"#);
icon!(CLOSE, "rt-close", r#"<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/>"#);
icon!(PLUS, "rt-plus", r#"<path d="M12 5.5v13M5.5 12h13"/>"#);
icon!(HEXAGON, "rt-hexagon", r#"<path d="M12 3l7.8 4.5v9L12 21l-7.8-4.5v-9z" fill="currentColor"/>"#);
icon!(CODE, "rt-code", r#"<path d="M9 7l-5 5 5 5M15 7l5 5-5 5"/>"#);
icon!(BRANCH, "rt-branch", r#"<circle cx="6.5" cy="5.5" r="2"/><circle cx="6.5" cy="18.5" r="2"/><circle cx="17.5" cy="7.5" r="2"/><path d="M6.5 7.5v9M17.5 9.5c0 4-6 3-10 7"/>"#);
icon!(PULSE, "rt-pulse", r#"<path d="M3 12h4l2.5-6 5 12 2.5-6h4"/>"#);
icon!(SERVER, "rt-server", r#"<rect x="4" y="4" width="16" height="7" rx="2"/><rect x="4" y="13" width="16" height="7" rx="2"/><path d="M8 7.5h.01M8 16.5h.01"/>"#);
icon!(BOX, "rt-box", r#"<path d="M12 3l8 4.5v9L12 21l-8-4.5v-9zM4 7.5l8 4.5 8-4.5M12 12v9"/>"#);
icon!(SPARK, "rt-spark", r#"<path d="M12 3v18M3 12h18M5.6 5.6l12.8 12.8M18.4 5.6L5.6 18.4"/>"#);
icon!(MOON, "rt-moon", r#"<path d="M20 14.5A8 8 0 1 1 9.5 4a6.5 6.5 0 0 0 10.5 10.5z"/>"#);
icon!(PYTHON, "rt-python", r#"<path d="M12 3c-4 0-4 1.8-4 3v2h4.5v1H6c-2 0-3 1.5-3 4s1 4 3 4h2v-2.5c0-1.4 1.1-2.5 2.5-2.5h4c1.4 0 2.5-1.1 2.5-2.5V6c0-1.6-1.6-3-5-3zM12 21c4 0 4-1.8 4-3v-2h-4.5v-1H18c2 0 3-1.5 3-4s-1-4-3-4h-2"/>"#);

/// A program's logo and its color: the icon is drawn in the color on a tinted tile.
pub fn program(name: &str) -> (&'static Icon, Color) {
    let c = |h: &str| rune::color::hex(h).unwrap();
    match name {
        "zsh" | "bash" | "fish" | "sh" | "dash" | "nu" | "xonsh" => (&TERMINAL, c("#3ccf6c")),
        "node" | "npm" | "npx" | "pnpm" | "bun" | "deno" | "yarn" => (&HEXAGON, c("#7cc35b")),
        "nvim" | "vim" | "vi" | "hx" | "helix" | "nano" | "emacs" => (&CODE, c("#5fb3f0")),
        "python" | "python3" | "ipython" | "uv" => (&PYTHON, c("#f5c84c")),
        "git" | "lazygit" | "tig" | "gitui" => (&BRANCH, c("#f0703a")),
        "htop" | "btop" | "top" | "nvtop" | "bottom" => (&PULSE, c("#e86ab0")),
        "ssh" | "mosh" | "et" => (&SERVER, c("#7aa2f7")),
        "cargo" | "rustc" | "make" | "zig" | "cmake" | "ninja" | "go" | "gcc" => (&BOX, c("#e0a26f")),
        "claude" => (&SPARK, c("#d97757")),
        "sleep" => (&MOON, c("#a78bfa")),
        _ => (&TERMINAL, c("#9aa4b2")),
    }
}

/// Shells: a pane running one of these is idle.
pub fn is_shell(name: &str) -> bool {
    matches!(name, "zsh" | "bash" | "fish" | "sh" | "dash" | "nu" | "xonsh")
}

/// A program's logo on a tinted tile, as the UI shows them.
pub fn program_logo(name: &str, size: f32) -> rune::El {
    let (i, c) = program(name);
    rune::el().size(size, size).radius(size * 0.3).fill(c.alpha(0.15)).border(1.0, c.alpha(0.2)).center()
        .child(rune::icon(i, size * 0.62).color(c))
}
