//! Point rust-embed at `web/build` when the UI has been built, otherwise at an
//! empty directory under OUT_DIR, so the server compiles either way without
//! this script ever writing into `web/`.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let build = manifest.join("..").join("..").join("web").join("build");

    println!("cargo:rerun-if-changed=build.rs");
    // Watching the path also notices it appearing later (a missing path
    // counts as changed, so until the UI is built this script reruns).
    println!("cargo:rerun-if-changed={}", build.display());

    let dir = if build.is_dir() {
        build
    } else {
        let empty = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("web-empty");
        std::fs::create_dir_all(&empty).unwrap();
        empty
    };
    let dir = dir.canonicalize().unwrap_or(dir);
    let mut s = dir.display().to_string();
    // rust-embed dislikes Windows verbatim prefixes.
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        s = rest.to_string();
    }
    println!("cargo:rustc-env=YTP_WEB_DIR={s}");
}
