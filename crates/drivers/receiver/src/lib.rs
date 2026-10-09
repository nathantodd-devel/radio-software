//! The interface between the scanner and the radios it can use.
//!
//! Supporting a new kind of radio means implementing two traits:
//!
//! - [`Receiver`], for one open device: say what it can do, set it up, tune
//!   it, and stream samples from it;
//! - [`Driver`], for the kind of device: a name, and how to find and open
//!   one.
//!
//! The scanner works with `dyn Driver` and `dyn Receiver` only, so anything
//! that implements them can be scanned with. The `airspy` and `rtlsdr`
//! crates are the two implementations that come with it.

use std::fmt;
use std::sync::mpsc;

/// The top of the gain scale in [`Settings::gain`].
pub const GAIN_LEVELS: u8 = 21;

/// Why a receiver couldn't be found, opened or controlled. Each carries a
/// message fit to show the user.
#[derive(Debug)]
pub enum Error {
    /// The driver can't work on this computer at all: typically the
    /// library it needs isn't installed. Says how to fix that.
    Unavailable(String),
    /// No device of this kind is attached, or none that is free.
    NotFound(String),
    /// The device was found but wouldn't do as asked.
    Failed(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (Error::Unavailable(message) | Error::NotFound(message) | Error::Failed(message)) = self;
        f.write_str(message)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// One delivery of samples from a running receiver.
pub struct Block {
    /// Interleaved I and Q, 16 bits each, full scale at ±32768. Devices with
    /// fewer bits scale theirs up.
    pub iq: Vec<i16>,
    /// Samples lost since the previous block because the reader fell behind.
    pub dropped: u64,
}

/// How the scanner wants a receiver set up before it starts.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    /// One of the rates from [`Receiver::sample_rates`], in samples per
    /// second.
    pub sample_rate: u32,
    /// Overall gain, from 0 (least) to [`GAIN_LEVELS`] (most), to be spread
    /// over whatever range the device has. Any automatic gain control
    /// should be turned off.
    pub gain: u8,
    /// Correction for the device's frequency reference being off, in parts
    /// per million. Devices accurate enough not to need it ignore it.
    pub ppm: i32,
    /// Power an amplifier at the antenna through the coax, if the device can.
    pub bias_tee: bool,
}

/// One open radio.
pub trait Receiver: Send {
    /// What the device is, for showing to the user: "Airspy", or a model
    /// and tuner.
    fn name(&self) -> String;

    /// The sample rates it offers, in samples per second. The scanner needs
    /// rates that are whole multiples of 1000.
    fn sample_rates(&self) -> Result<Vec<u32>>;

    /// The fraction of the sample rate, centred on the tuned frequency,
    /// that the device's filters pass cleanly: channels are only placed
    /// within it.
    fn usable_bandwidth(&self) -> f64;

    /// Apply `settings`. Called once, before [`start`](Self::start).
    fn configure(&mut self, settings: &Settings) -> Result<()>;

    /// Tune to a centre frequency in Hz. Called before and while receiving.
    fn set_frequency(&mut self, hz: u32) -> Result<()>;

    /// Start receiving. Blocks of samples arrive on the returned channel
    /// until [`stop`](Self::stop) is called, the receiving end is dropped,
    /// or the device goes away, at which point the channel closes. Falling
    /// behind must lose samples (counted in [`Block::dropped`]), not stall
    /// the device.
    fn start(&mut self) -> Result<mpsc::Receiver<Block>>;

    /// Stop receiving; does nothing if not started. Dropping the receiver
    /// stops it too.
    fn stop(&mut self) -> Result<()>;
}

/// A kind of radio the scanner can look for.
pub trait Driver: Send + Sync {
    /// A short, stable, lower-case name with no spaces, used on the command
    /// line and in saved settings: "airspy".
    fn id(&self) -> &'static str;

    /// What to call this kind of radio when talking to the user: "Airspy".
    fn name(&self) -> &'static str;

    /// Find a device of this kind and open it.
    fn open(&self) -> Result<Box<dyn Receiver>>;
}
