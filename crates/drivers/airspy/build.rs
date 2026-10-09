//! On Android, compiles the copy of libairspy in vendor/libairspy into this
//! crate. Everywhere else the system's library is loaded at run time and
//! there is nothing to build.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=vendor/libairspy");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("android") {
        return;
    }
    // libusb1-sys builds libusb and tells us where its header is.
    let libusb_include = std::env::var("DEP_USB_1.0_INCLUDE").expect("libusb1-sys didn't say where libusb.h is");
    cc::Build::new()
        .files(["airspy.c", "iqconverter_float.c", "iqconverter_int16.c"].map(|f| format!("vendor/libairspy/{f}")))
        .include("vendor/libairspy")
        .include(libusb_include)
        // Upstream's code, not ours to tidy.
        .warnings(false)
        .compile("airspy");
}
