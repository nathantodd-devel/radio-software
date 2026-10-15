//! On Windows, or with the `vendored` feature, compiles libhackrf from
//! vendor/hackrf (a git submodule of the HackRF project) into this crate.
//! Otherwise the system's library is loaded at run time and there is
//! nothing to build.

use std::path::Path;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows");
    println!("cargo:rerun-if-changed=vendor/hackrf/host/libhackrf/src");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    if !windows && std::env::var_os("CARGO_FEATURE_VENDORED").is_none() {
        return;
    }
    let source = Path::new("vendor/hackrf/host/libhackrf/src");
    assert!(
        source.join("hackrf.c").exists(),
        "libhackrf's source isn't here: fetch it with \
         `git submodule update --init crates/drivers/hackrf/vendor/hackrf`"
    );
    // libusb1-sys builds libusb and tells us where its header is.
    let libusb_include = std::env::var("DEP_USB_1.0_INCLUDE").expect("libusb1-sys didn't say where libusb.h is");
    let mut build = cc::Build::new();
    build
        .file(source.join("hackrf.c"))
        .include(source)
        .include(libusb_include);
    if windows {
        // Microsoft's compiler has no <pthread.h>; ours stands in for it.
        build.include("windows");
    }
    // Upstream's code, not ours to tidy.
    build.warnings(false).compile("hackrf");
}
