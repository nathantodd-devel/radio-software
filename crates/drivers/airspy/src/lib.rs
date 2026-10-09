//! Safe interface to Airspy R2 / Mini receivers.
//!
//! This drives the device through libairspy, the vendor's C library. On
//! desktops it is loaded at run time: nothing is needed to build, and a
//! missing library is an ordinary [`Error`] rather than a failure to start.
//! On Android, which has no such library, a copy is compiled in, and the
//! device is opened with [`Airspy::open_fd`] from a file descriptor that
//! Android's USB manager provides. On Fedora the
//! library comes with the `airspyone_host` package, on Debian and Ubuntu
//! with `libairspy0`, on macOS with Homebrew's `airspy`; on Windows
//! `airspy.dll` and the libraries it needs go beside the program.

mod ffi;

use std::ffi::{CStr, c_int, c_void};
use std::fmt;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};

use ffi::{Lib, Transfer};

#[cfg(target_os = "linux")]
const INSTALL_HINT: &str = "install airspyone_host (Fedora) or libairspy0 (Debian/Ubuntu)";
#[cfg(target_os = "macos")]
const INSTALL_HINT: &str = "install it with `brew install airspy`";
#[cfg(target_os = "windows")]
const INSTALL_HINT: &str = "airspy.dll, libusb-1.0.dll and pthreadVC2.dll belong beside this program";
#[cfg(target_os = "android")]
const INSTALL_HINT: &str = "it is part of the app and should always be there";

/// Blocks of samples that may wait for the reader before any are dropped.
const QUEUE_BLOCKS: usize = 32;

#[derive(Debug)]
pub enum Error {
    /// libairspy isn't installed, or isn't one this crate can use.
    Library(String),
    /// No Airspy could be opened: none is plugged in, or the one that is
    /// is already in use (libairspy doesn't tell these apart).
    NotFound,
    /// The Airspy is busy with another operation.
    Busy,
    /// Any other failure reported by libairspy, by its name there.
    Device(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Library(why) => write!(f, "can't load libairspy ({why}); {INSTALL_HINT}"),
            Error::NotFound => write!(f, "no Airspy available: not plugged in, or in use by another program"),
            Error::Busy => write!(f, "the Airspy is busy"),
            Error::Device(name) => write!(f, "Airspy error: {name}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub use receiver::Block;

/// Fraction of the sample rate the Airspy's filters pass cleanly.
const USABLE_BANDWIDTH: f64 = 0.9;

/// State shared with libairspy's streaming thread.
struct Stream {
    blocks: SyncSender<Block>,
    dropped: u64,
}

/// An open Airspy. Closing it (dropping this) stops any reception.
pub struct Airspy {
    lib: Lib,
    device: *mut c_void,
    stream: Option<Box<Stream>>,
}

// The handle is only a pointer; libairspy doesn't tie it to a thread.
unsafe impl Send for Airspy {}

impl Airspy {
    /// Open an Airspy the operating system has already opened, by the file
    /// descriptor of that connection: on Android, what `UsbDeviceConnection`
    /// gives. The descriptor stays the caller's to close, after this is
    /// dropped.
    pub fn open_fd(fd: i32) -> Result<Self> {
        let lib = Lib::load().map_err(Error::Library)?;
        let open_fd = lib
            .open_fd
            .ok_or_else(|| Error::Library("it is too old to open a device by file descriptor".into()))?;
        let mut device = std::ptr::null_mut();
        // SAFETY: `device` is a valid place for the handle to be written.
        let code = unsafe { open_fd(&mut device, fd) };
        check(&lib, code)?;
        Ok(Self {
            lib,
            device,
            stream: None,
        })
    }

    /// Open the first Airspy found.
    pub fn open() -> Result<Self> {
        let lib = Lib::load().map_err(Error::Library)?;
        let mut device = std::ptr::null_mut();
        // SAFETY: `device` is a valid place for the handle to be written.
        let code = unsafe { (lib.open)(&mut device) };
        check(&lib, code)?;
        Ok(Self {
            lib,
            device,
            stream: None,
        })
    }

    fn call(&self, code: c_int) -> Result<()> {
        check(&self.lib, code)
    }

    /// Sample rates the device offers for I/Q, in samples per second.
    pub fn sample_rates(&self) -> Result<Vec<u32>> {
        let mut count = 0u32;
        // SAFETY: a length of 0 asks for the count, written to one u32.
        self.call(unsafe { (self.lib.get_samplerates)(self.device, &mut count, 0) })?;
        let mut rates = vec![0u32; count as usize];
        // SAFETY: `rates` has room for `count` rates.
        self.call(unsafe { (self.lib.get_samplerates)(self.device, rates.as_mut_ptr(), count) })?;
        Ok(rates)
    }

    /// Select one of the rates from [`sample_rates`](Self::sample_rates).
    pub fn set_sample_rate(&mut self, hz: u32) -> Result<()> {
        // SAFETY (here and in the setters below): `device` is open.
        self.call(unsafe { (self.lib.set_samplerate)(self.device, hz) })
    }

    /// Tune to a centre frequency in Hz (24 MHz to 1.8 GHz).
    pub fn set_frequency(&mut self, hz: u32) -> Result<()> {
        self.call(unsafe { (self.lib.set_freq)(self.device, hz) })
    }

    /// Overall gain, 0-21, favouring freedom from overload.
    pub fn set_linearity_gain(&mut self, gain: u8) -> Result<()> {
        self.call(unsafe { (self.lib.set_linearity_gain)(self.device, gain) })
    }

    /// Overall gain, 0-21, favouring weak signals.
    pub fn set_sensitivity_gain(&mut self, gain: u8) -> Result<()> {
        self.call(unsafe { (self.lib.set_sensitivity_gain)(self.device, gain) })
    }

    /// Power an amplifier at the antenna through the coax.
    pub fn set_bias_tee(&mut self, on: bool) -> Result<()> {
        self.call(unsafe { (self.lib.set_rf_bias)(self.device, on as u8) })
    }

    /// Start receiving. Blocks of 16-bit I/Q arrive on the returned channel
    /// until [`stop`](Self::stop) is called, the receiver is dropped, or the
    /// device goes away, at which point the channel closes.
    pub fn start(&mut self) -> Result<Receiver<Block>> {
        self.stop()?;
        self.call(unsafe { (self.lib.set_sample_type)(self.device, ffi::SAMPLE_INT16_IQ) })?;
        let (blocks, receiver) = sync_channel(QUEUE_BLOCKS);
        let stream = self.stream.insert(Box::new(Stream { blocks, dropped: 0 }));
        let context = &raw mut **stream as *mut c_void;
        // SAFETY: `context` points into a box that lives until `stop` has
        // returned, which is after libairspy's last call to `on_block`.
        let code = unsafe { (self.lib.start_rx)(self.device, on_block, context) };
        if code != ffi::SUCCESS {
            self.stream = None;
        }
        self.call(code)?;
        Ok(receiver)
    }

    /// Stop receiving; does nothing if not started.
    pub fn stop(&mut self) -> Result<()> {
        if self.stream.is_some() {
            // Returns once the streaming threads have finished.
            let code = unsafe { (self.lib.stop_rx)(self.device) };
            self.stream = None;
            self.call(code)?;
        }
        Ok(())
    }
}

impl Drop for Airspy {
    fn drop(&mut self) {
        self.stop().ok();
        // SAFETY: the handle is open and not used again.
        unsafe { (self.lib.close)(self.device) };
    }
}

fn check(lib: &Lib, code: c_int) -> Result<()> {
    match code {
        ffi::SUCCESS => Ok(()),
        ffi::ERROR_NOT_FOUND => Err(Error::NotFound),
        ffi::ERROR_BUSY => Err(Error::Busy),
        _ => {
            // SAFETY: returns a pointer to a static string for any code.
            let name = unsafe { CStr::from_ptr((lib.error_name)(code)) };
            Err(Error::Device(name.to_string_lossy().into_owned()))
        }
    }
}

/// Called by libairspy on its own thread for every block of samples.
/// Returning non-zero ends the stream.
extern "C" fn on_block(transfer: *mut Transfer) -> c_int {
    // SAFETY: libairspy passes a valid transfer whose `ctx` is the `Stream`
    // given to `start_rx` and whose `samples` hold `sample_count` I/Q pairs
    // in the sample type selected there.
    let (transfer, stream) = unsafe { (&*transfer, &mut *((*transfer).ctx as *mut Stream)) };
    let count = transfer.sample_count.max(0) as usize;
    let iq = unsafe { std::slice::from_raw_parts(transfer.samples as *const i16, count * 2) };
    let block = Block {
        iq: iq.to_vec(),
        dropped: stream.dropped + transfer.dropped_samples,
    };
    match stream.blocks.try_send(block) {
        Ok(()) => stream.dropped = 0,
        Err(TrySendError::Full(block)) => stream.dropped = block.dropped + count as u64,
        Err(TrySendError::Disconnected(_)) => return 1,
    }
    0
}

impl From<Error> for receiver::Error {
    fn from(error: Error) -> Self {
        let message = error.to_string();
        match error {
            Error::Library(_) => receiver::Error::Unavailable(message),
            Error::NotFound => receiver::Error::NotFound(message),
            Error::Busy | Error::Device(_) => receiver::Error::Failed(message),
        }
    }
}

impl receiver::Receiver for Airspy {
    fn name(&self) -> String {
        "Airspy".into()
    }

    fn sample_rates(&self) -> receiver::Result<Vec<u32>> {
        Ok(Airspy::sample_rates(self)?)
    }

    fn usable_bandwidth(&self) -> f64 {
        USABLE_BANDWIDTH
    }

    fn configure(&mut self, settings: &receiver::Settings) -> receiver::Result<()> {
        self.set_sample_rate(settings.sample_rate)?;
        // The scale is the Airspy's own linearity gain.
        self.set_linearity_gain(settings.gain)?;
        // Left alone when off, so a failure to switch it can't get in the way.
        if settings.bias_tee {
            self.set_bias_tee(true)?;
        }
        Ok(())
    }

    fn set_frequency(&mut self, hz: u32) -> receiver::Result<()> {
        Ok(Airspy::set_frequency(self, hz)?)
    }

    fn start(&mut self) -> receiver::Result<Receiver<Block>> {
        Ok(Airspy::start(self)?)
    }

    fn stop(&mut self) -> receiver::Result<()> {
        Ok(Airspy::stop(self)?)
    }
}

/// Finds and opens the first Airspy attached.
pub struct Driver;

impl receiver::Driver for Driver {
    fn id(&self) -> &'static str {
        "airspy"
    }

    fn name(&self) -> &'static str {
        "Airspy"
    }

    fn open(&self) -> receiver::Result<Box<dyn receiver::Receiver>> {
        Ok(Box::new(Airspy::open()?))
    }
}

/// Opens an Airspy the operating system has already opened, by the file
/// descriptor of that connection: see [`Airspy::open_fd`].
pub struct FdDriver(pub i32);

impl receiver::Driver for FdDriver {
    fn id(&self) -> &'static str {
        "airspy"
    }

    fn name(&self) -> &'static str {
        "Airspy"
    }

    fn open(&self) -> receiver::Result<Box<dyn receiver::Receiver>> {
        Ok(Box::new(Airspy::open_fd(self.0)?))
    }
}
