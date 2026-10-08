//! gpui's keyboard handling links libxkbcommon-x11 even in a Wayland-only
//! build, where nothing from it is used. When the system doesn't have that
//! library's development package (Fedora: libxkbcommon-x11-devel), give the
//! linker an empty stand-in; `--as-needed` then drops it from the binary.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let have_real = Command::new("pkg-config")
        .args(["--exists", "xkbcommon-x11"])
        .status()
        .is_ok_and(|s| s.success());
    if have_real {
        return;
    }
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src = out.join("xkbcommon_x11_stub.c");
    std::fs::write(&src, "").unwrap();
    let status = Command::new(env::var("CC").unwrap_or_else(|_| "cc".into()))
        .args(["-shared", "-o"])
        .arg(out.join("libxkbcommon-x11.so"))
        .arg(&src)
        .status()
        .expect("can't run the C compiler");
    assert!(status.success(), "can't build the libxkbcommon-x11 stand-in");
    println!("cargo:rustc-link-search=native={}", out.display());
}
