//! Safe interface to HackRF receivers: the HackRF One and its relatives
//! (Jawbreaker, rad1o, HackRF Pro).
//!
//! This drives the device through libhackrf. On Linux and macOS the
//! system's is loaded at run time: nothing is needed to build, and a
//! missing library is an ordinary [`Error`] rather than a failure to start.
//! On Fedora it comes with the `hackrf` package, on Debian/Ubuntu with
//! `libhackrf0`, on macOS with Homebrew's `hackrf`.
//!
//! On Windows, where there is no such thing to load, the library is
//! compiled into the program from the HackRF project's source, a git
//! submodule at `vendor/hackrf` (see build.rs); the `vendored` feature does
//! the same anywhere else.
//!
//! Only receiving is done here. A HackRF can transmit; nothing in this
//! crate asks it to.

mod ffi;

use std::ffi::{CStr, c_int, c_void};
use std::fmt;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, OnceLock};

use ffi::Lib;

#[cfg(target_os = "macos")]
const INSTALL_HINT: &str = "install it with `brew install hackrf`";
#[cfg(not(target_os = "macos"))]
const INSTALL_HINT: &str = "install the hackrf package (libhackrf0 on Debian and Ubuntu)";

/// Blocks of samples that may wait for the reader before any are dropped.
const QUEUE_BLOCKS: usize = 64;
/// Sample rates worth offering, fastest first. The device goes to 20 MSPS,
/// but that is more than one processor core demodulates in real time.
const SAMPLE_RATES: [u32; 4] = [10_000_000, 8_000_000, 5_000_000, 2_500_000];
/// Fraction of the sample rate inside the baseband filter, which is set to
/// match: the device's own choice of filters comes in steps, the nearest
/// under three quarters of the rate being its custom.
const USABLE_BANDWIDTH: f64 = 0.75;
/// The most gain asked of the two receive amplifiers, in dB, and the steps
/// they are set in. The second (baseband) one goes on to 62 dB, by which
/// point its own noise is all it adds. The front-end amplifier is left off:
/// it overloads readily, and a strong signal can damage it.
const LNA_MAX_DB: u32 = 40;
const LNA_STEP_DB: u32 = 8;
const VGA_MAX_DB: u32 = 40;
const VGA_STEP_DB: u32 = 2;

#[derive(Debug)]
pub enum Error {
    /// libhackrf isn't installed, or isn't one this crate can use.
    Library(String),
    /// No HackRF is plugged in.
    NotFound,
    /// The device couldn't be opened or wouldn't do as asked: commonly it is
    /// in use by another program, or (on Linux) the user isn't allowed to
    /// use it.
    Device(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Library(why) => write!(f, "can't load libhackrf ({why}); {INSTALL_HINT}"),
            Error::NotFound => write!(f, "no HackRF found; is it plugged in?"),
            Error::Device(what) => write!(f, "HackRF error: {what}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub use receiver::Block;

/// The library, loaded and started once and kept for as long as the
/// program runs.
fn lib() -> Result<&'static Lib> {
    static LIB: OnceLock<std::result::Result<Lib, String>> = OnceLock::new();
    LIB.get_or_init(|| {
        let lib = Lib::load()?;
        // SAFETY: takes no arguments; called once, before anything else.
        let code = unsafe { (lib.init)() };
        if code != 0 {
            return Err(format!("it wouldn't start: {}", error_name(&lib, code)));
        }
        Ok(lib)
    })
    .as_ref()
    .map_err(|why| Error::Library(why.clone()))
}

/// What the library calls one of its error codes.
fn error_name(lib: &Lib, code: c_int) -> String {
    // SAFETY: returns a static string for any code.
    let name = unsafe { (lib.error_name)(code) };
    if name.is_null() {
        return format!("error {code}");
    }
    // SAFETY: not null, so a static C string.
    unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned()
}

/// The device handle, shared with the stream's callback.
struct Handle {
    lib: &'static Lib,
    device: ffi::Device,
}

// libhackrf allows a device to be controlled while its transfer thread
// delivers samples.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn call(&self, what: &str, code: c_int) -> Result<()> {
        if code != 0 {
            return Err(Error::Device(format!(
                "couldn't {what} ({})",
                error_name(self.lib, code)
            )));
        }
        Ok(())
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the handle is open and not used again.
        unsafe { (self.lib.close)(self.device) };
    }
}

/// State shared with libhackrf's sample callback.
struct Stream {
    blocks: SyncSender<Block>,
    dropped: u64,
}

/// An open HackRF. Closing it (dropping this) stops any reception.
pub struct HackRf {
    handle: Arc<Handle>,
    name: String,
    /// Frequency correction in parts per million.
    ppm: i32,
    /// The stream in progress; the library holds a pointer to it meanwhile.
    stream: Option<Box<Stream>>,
}

impl HackRf {
    /// Open the first HackRF found.
    pub fn open() -> Result<Self> {
        Self::open_all()?.into_iter().next().ok_or(Error::NotFound)
    }

    /// Open every HackRF that is attached and not in use.
    pub fn open_all() -> Result<Vec<Self>> {
        let lib = lib()?;
        // SAFETY: takes no arguments; the list is freed below.
        let list = unsafe { (lib.device_list)() };
        if list.is_null() {
            return Err(Error::NotFound);
        }
        let mut failure = Error::NotFound;
        let mut found = Vec::new();
        // SAFETY: `list` is the library's, valid until freed.
        let count = unsafe { (*list).devicecount }.max(0) as usize;
        for index in 0..count {
            // SAFETY: both arrays have `devicecount` entries; a serial
            // number may be missing (null) for a device that can't be read.
            let (serial, board) = unsafe {
                let serial = *(*list).serial_numbers.add(index);
                let serial = (!serial.is_null()).then(|| CStr::from_ptr(serial).to_string_lossy().into_owned());
                let board = CStr::from_ptr((lib.usb_board_id_name)(*(*list).usb_board_ids.add(index)));
                (serial, board.to_string_lossy().into_owned())
            };
            let mut device = std::ptr::null_mut();
            // SAFETY: `index` is within the list; `device` is a valid place
            // for the handle to be written.
            let code = unsafe { (lib.device_list_open)(list, index as c_int, &mut device) };
            if code != 0 || device.is_null() {
                // One that won't open is in use elsewhere: left alone.
                failure = Error::Device(format!(
                    "couldn't open the {board} ({}); it may be in use by another program{}",
                    error_name(lib, code),
                    if cfg!(target_os = "linux") {
                        ", or need its udev rules (the hackrf package installs them)"
                    } else if cfg!(target_os = "windows") {
                        ", or need the WinUSB driver installed with Zadig"
                    } else {
                        ""
                    }
                ));
                continue;
            }
            found.push(Self {
                handle: Arc::new(Handle { lib, device }),
                name: name(&board, serial.as_deref()),
                ppm: 0,
                stream: None,
            });
        }
        // SAFETY: `list` came from `device_list` and isn't used again.
        unsafe { (lib.device_list_free)(list) };
        if found.is_empty() { Err(failure) } else { Ok(found) }
    }

    /// The kind of board, and the end of its serial number.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sample rates to choose from, in samples per second.
    pub fn sample_rates(&self) -> Vec<u32> {
        SAMPLE_RATES.to_vec()
    }

    /// Set the sample rate, and the baseband filter to suit it.
    pub fn set_sample_rate(&mut self, hz: u32) -> Result<()> {
        let h = &self.handle;
        // SAFETY (here and in the setters below): `device` is open.
        h.call("set the sample rate", unsafe {
            (h.lib.set_sample_rate)(h.device, f64::from(hz))
        })?;
        // SAFETY: plain arithmetic in the library.
        let filter = unsafe { (h.lib.compute_baseband_filter_bw)((f64::from(hz) * USABLE_BANDWIDTH) as u32) };
        h.call("set the baseband filter", unsafe {
            (h.lib.set_baseband_filter_bandwidth)(h.device, filter)
        })
    }

    /// Tune to a centre frequency in Hz (1 MHz to 6 GHz).
    pub fn set_frequency(&mut self, hz: u32) -> Result<()> {
        let h = &self.handle;
        h.call("tune", unsafe { (h.lib.set_freq)(h.device, corrected(hz, self.ppm)) })
    }

    /// Correct for the device's crystal being off, in parts per million: a
    /// positive figure for one that runs fast, as librtlsdr has it. Applies
    /// from the next time it is tuned.
    pub fn set_frequency_correction(&mut self, ppm: i32) {
        self.ppm = ppm;
    }

    /// Set the gain as a level from 0 (lowest) to `of` (highest), spread
    /// over both receive amplifiers.
    pub fn set_gain_level(&mut self, level: u8, of: u8) -> Result<()> {
        let (lna, vga) = gains(level, of);
        let h = &self.handle;
        h.call("turn off the front-end amplifier", unsafe {
            (h.lib.set_amp_enable)(h.device, 0)
        })?;
        h.call("set the LNA gain", unsafe { (h.lib.set_lna_gain)(h.device, lna) })?;
        h.call("set the baseband gain", unsafe { (h.lib.set_vga_gain)(h.device, vga) })
    }

    /// Power an amplifier at the antenna through the coax (3.3 V, 50 mA at
    /// most on a HackRF One).
    pub fn set_bias_tee(&mut self, on: bool) -> Result<()> {
        let h = &self.handle;
        h.call("switch the antenna power", unsafe {
            (h.lib.set_antenna_enable)(h.device, on as u8)
        })
    }

    /// Start receiving. Blocks of I/Q arrive on the returned channel until
    /// [`stop`](Self::stop) is called or the receiver is dropped, at which
    /// point the channel closes.
    pub fn start(&mut self) -> Result<Receiver<Block>> {
        self.stop();
        let (blocks, receiver) = sync_channel(QUEUE_BLOCKS);
        let mut stream = Box::new(Stream { blocks, dropped: 0 });
        let context = &raw mut *stream as *mut c_void;
        let h = &self.handle;
        // SAFETY: `stream` stays where it is, in `self.stream`, until
        // `stop` has ended the stream, during which alone `on_samples` is
        // called.
        h.call("start receiving", unsafe {
            (h.lib.start_rx)(h.device, on_samples, context)
        })?;
        self.stream = Some(stream);
        Ok(receiver)
    }

    /// Stop receiving; does nothing if not started.
    pub fn stop(&mut self) {
        if self.stream.is_some() {
            // SAFETY: `device` is open. Returns once the transfer thread
            // has finished with the callback.
            unsafe { (self.handle.lib.stop_rx)(self.handle.device) };
            self.stream = None;
        }
    }
}

impl Drop for HackRf {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What to call a board: its kind and the last of its serial number, which
/// is mostly zeros at the front.
fn name(board: &str, serial: Option<&str>) -> String {
    match serial.map(|serial| serial.trim_start_matches('0')) {
        Some(serial) if !serial.is_empty() => {
            let tail = &serial[serial.len().saturating_sub(8)..];
            format!("{board} ({tail})")
        }
        _ => board.to_string(),
    }
}

/// The frequency to ask a device for, to have it at `hz`, when its crystal
/// is `ppm` parts per million fast.
fn corrected(hz: u32, ppm: i32) -> u64 {
    (f64::from(hz) * (1.0 - f64::from(ppm) * 1e-6)).round() as u64
}

/// The (LNA, baseband) gains in dB for a level from 0 to `of`.
fn gains(level: u8, of: u8) -> (u32, u32) {
    let (level, of) = (u32::from(level.min(of)), u32::from(of.max(1)));
    let spread = |max: u32, step: u32| (level * (max / step) + of / 2) / of * step;
    (spread(LNA_MAX_DB, LNA_STEP_DB), spread(VGA_MAX_DB, VGA_STEP_DB))
}

/// The device's signed 8-bit samples as signed 16-bit ones.
fn widen(bytes: &[u8]) -> Vec<i16> {
    bytes.iter().map(|&b| i16::from(b as i8) * 256).collect()
}

/// Called by libhackrf on its transfer thread for every buffer of samples.
extern "C" fn on_samples(transfer: *mut ffi::Transfer) -> c_int {
    // SAFETY: libhackrf passes a valid transfer with `valid_length` bytes
    // at `buffer`, and the `Stream` given to `start_rx`.
    let (bytes, stream) = unsafe {
        let transfer = &*transfer;
        (
            std::slice::from_raw_parts(transfer.buffer, transfer.valid_length.max(0) as usize),
            &mut *(transfer.rx_ctx as *mut Stream),
        )
    };
    let block = Block {
        iq: widen(bytes),
        dropped: stream.dropped,
    };
    match stream.blocks.try_send(block) {
        Ok(()) => stream.dropped = 0,
        Err(TrySendError::Full(block)) => stream.dropped = block.dropped + bytes.len() as u64 / 2,
        // Nobody is listening any more: end the stream.
        Err(TrySendError::Disconnected(_)) => return -1,
    }
    0
}

impl From<Error> for receiver::Error {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::Library(_) => receiver::Error::Unavailable(message),
            Error::NotFound => receiver::Error::NotFound(message),
            Error::Device(_) => receiver::Error::Failed(message),
        }
    }
}

impl receiver::Receiver for HackRf {
    fn name(&self) -> String {
        HackRf::name(self).to_string()
    }

    fn sample_rates(&self) -> receiver::Result<Vec<u32>> {
        Ok(HackRf::sample_rates(self))
    }

    fn usable_bandwidth(&self) -> f64 {
        USABLE_BANDWIDTH
    }

    fn configure(&mut self, settings: &receiver::Settings) -> receiver::Result<()> {
        self.set_sample_rate(settings.sample_rate)?;
        self.set_frequency_correction(settings.ppm);
        self.set_gain_level(settings.gain, receiver::GAIN_LEVELS)?;
        Ok(self.set_bias_tee(settings.bias_tee)?)
    }

    fn set_frequency(&mut self, hz: u32) -> receiver::Result<()> {
        Ok(HackRf::set_frequency(self, hz)?)
    }

    fn start(&mut self) -> receiver::Result<Receiver<Block>> {
        Ok(HackRf::start(self)?)
    }

    fn stop(&mut self) -> receiver::Result<()> {
        HackRf::stop(self);
        Ok(())
    }
}

/// Finds and opens HackRFs.
pub struct Driver;

impl receiver::Driver for Driver {
    fn id(&self) -> &'static str {
        "hackrf"
    }

    fn name(&self) -> &'static str {
        "HackRF"
    }

    fn open(&self) -> receiver::Result<Box<dyn receiver::Receiver>> {
        Ok(Box::new(HackRf::open()?))
    }

    fn open_all(&self) -> receiver::Result<Vec<Box<dyn receiver::Receiver>>> {
        let devices = HackRf::open_all()?;
        Ok(devices.into_iter().map(|device| Box::new(device) as _).collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widens_signed_samples() {
        assert_eq!(widen(&[0x80, 0xff, 0, 1, 0x7f]), [-32768, -256, 0, 256, 32512]);
    }

    #[test]
    fn spreads_gain_over_both_amplifiers() {
        assert_eq!(gains(0, 21), (0, 0));
        assert_eq!(gains(21, 21), (40, 40));
        assert_eq!(gains(99, 21), (40, 40));
        // The scanner's default: in the steps the amplifiers take.
        assert_eq!(gains(17, 21), (32, 32));
        for level in 0..=21 {
            let (lna, vga) = gains(level, 21);
            assert!(lna % LNA_STEP_DB == 0 && vga % VGA_STEP_DB == 0);
        }
    }

    #[test]
    fn corrects_for_a_fast_crystal() {
        assert_eq!(corrected(850_000_000, 0), 850_000_000);
        assert_eq!(corrected(850_000_000, 10), 849_991_500);
        assert_eq!(corrected(850_000_000, -10), 850_008_500);
    }

    #[test]
    fn names_a_board_by_the_end_of_its_serial() {
        assert_eq!(
            name("HackRF One", Some("0000000000000000a06063c8234e925f")),
            "HackRF One (234e925f)"
        );
        assert_eq!(name("HackRF One", Some("0000")), "HackRF One");
        assert_eq!(name("HackRF One", None), "HackRF One");
    }
}
