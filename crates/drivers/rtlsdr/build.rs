//! On Android, compiles the copy of librtlsdr in vendor/librtlsdr into this
//! crate. Everywhere else the system's library is loaded at run time and
//! there is nothing to build.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/librtlsdr");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    // libusb1-sys builds libusb and tells us where its header is.
    let libusb_include = std::env::var("DEP_USB_1.0_INCLUDE").expect("libusb1-sys didn't say where libusb.h is");
    let sources = [
        "librtlsdr",
        "tuner_e4k",
        "tuner_fc0012",
        "tuner_fc0013",
        "tuner_fc2580",
        "tuner_r82xx",
    ];
    cc::Build::new()
        .files(sources.map(|f| format!("vendor/librtlsdr/{f}.c")))
        .include("vendor/librtlsdr")
        .include(libusb_include)
        // Upstream's code, not ours to tidy.
        .warnings(false)
        .compile("rtlsdr");
}
