//! Wideband public-safety scanner for an Airspy: tunes once, then monitors
//! every channel in the band plan simultaneously.

pub mod db;
pub mod engine;
pub mod plan;
pub mod radioreference;
pub mod session;
pub mod settings;
pub mod update;
mod wav;

/// Which library does the channelizer's FFTs: "FFTW" (with the `fftw`
/// feature, where libfftw3f is installed) or "RustFFT".
pub fn fft_backend() -> &'static str {
    radiocore::fft::backend()
}
