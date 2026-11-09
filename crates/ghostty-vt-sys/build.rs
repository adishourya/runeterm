//! Links libghostty-vt 1.x and generates bindings from its headers.
//!
//! Finds the library via pkg-config. `GHOSTTY_VT_PREFIX` (default: `vendor/ghostty-vt` in the
//! workspace) is prepended to PKG_CONFIG_PATH, so the vendored ReleaseFast build wins.

use std::{env, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let prefix = env::var("GHOSTTY_VT_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|_| manifest.join("../../vendor/ghostty-vt"));
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_PREFIX");
    println!("cargo:rerun-if-changed=wrapper.h");

    let mut path = ["lib/pkgconfig", "share/pkgconfig"]
        .map(|d| prefix.join(d).display().to_string())
        .join(":");
    if let Ok(p) = env::var("PKG_CONFIG_PATH") {
        path = format!("{path}:{p}");
    }
    // SAFETY: build scripts are single threaded.
    unsafe { env::set_var("PKG_CONFIG_PATH", path) };

    let lib = pkg_config::Config::new()
        .atleast_version("1.0.0")
        .cargo_metadata(false)
        .probe("libghostty-vt-static")
        .expect("libghostty-vt >= 1.0.0 not found; build it (see README) or set GHOSTTY_VT_PREFIX");
    // Link the static archive explicitly; pkg-config names it by path, which would otherwise
    // resolve to the shared library next to it.
    for dir in &lib.link_paths {
        println!("cargo:rustc-link-search=native={}", dir.display());
    }
    println!("cargo:rustc-link-search=native={}", prefix.join("lib").display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");

    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .allowlist_function("ghostty_.*")
        .allowlist_type("Ghostty.*")
        .allowlist_var("GHOSTTY_.*")
        .prepend_enum_name(false)
        .default_enum_style(bindgen::EnumVariation::Consts)
        .derive_default(true)
        .layout_tests(false)
        .parse_callbacks(Box::new(bindgen::CargoCallbacks::new()));
    // libclang without a clang install lacks its resource headers (limits.h, stddef.h):
    // borrow the C compiler's builtin include dir.
    if let Ok(out) = std::process::Command::new(env::var("CC").unwrap_or("cc".into()))
        .arg("-print-file-name=include")
        .output()
    {
        let dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if PathBuf::from(&dir).is_absolute() {
            builder = builder.clang_arg(format!("-isystem{dir}"));
        }
    }
    for inc in &lib.include_paths {
        builder = builder.clang_arg(format!("-I{}", inc.display()));
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    builder
        .generate()
        .expect("bindgen failed on ghostty/vt.h")
        .write_to_file(out.join("bindings.rs"))
        .unwrap();
}
