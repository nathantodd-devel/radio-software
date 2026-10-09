//! Wideband public-safety scanner for an Airspy: tunes once, then monitors
//! every channel in the band plan simultaneously.

pub mod db;
pub mod engine;
pub mod plan;
pub mod radioreference;
mod wav;
