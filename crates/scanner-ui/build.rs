//! On Windows, gives the program its icon. On Linux:
//!
//! gpui's keyboard handling links libxkbcommon-x11 even in a Wayland-only
//! build, where nothing from it is used. When the system doesn't have that
//! library's development package (Fedora: libxkbcommon-x11-devel), give the
//! linker an empty stand-in; `--as-needed` then drops it from the binary.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    // The icon Explorer and the taskbar show, and the window's own: GPUI
    // uses the program's first icon. Only when building on Windows, where
    // the tool that compiles it into the program is to be found.
    if target == "windows" && cfg!(windows) {
        const ICON: &str = "../../icons/dmps.ico";
        println!("cargo:rerun-if-changed={ICON}");
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(ICON);
        resource.set("ProductName", "Digital Mobile Police Scanner");
        resource.set("FileDescription", "Digital Mobile Police Scanner");
        // A program without its icon is better than no program.
        if let Err(e) = resource.compile() {
            println!("cargo:warning=can't add the icon to the program: {e}");
        }
    }
    if target != "linux" {
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
