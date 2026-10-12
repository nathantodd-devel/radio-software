//! Safe interface to RTL-SDR receivers: USB dongles built on the RTL2832U,
//! usually with an R820T2 or R860 tuner.
//!
//! This drives the device through librtlsdr, which is loaded at run time:
//! nothing is needed to build, and a missing library is an ordinary
//! [`Error`] rather than a failure to start. On Fedora and Debian/Ubuntu the
//! library comes with the `rtl-sdr` package, on macOS with Homebrew's
//! `librtlsdr`; on Windows `rtlsdr.dll` goes beside the program.

mod ffi;

use std::ffi::{CStr, c_int, c_void};
use std::fmt;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;

use ffi::Lib;

#[cfg(target_os = "windows")]
const INSTALL_HINT: &str = "rtlsdr.dll belongs beside this program";
#[cfg(target_os = "macos")]
const INSTALL_HINT: &str = "install it with `brew install librtlsdr`";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const INSTALL_HINT: &str = "install the rtl-sdr package";

/// Blocks of samples that may wait for the reader before any are dropped.
const QUEUE_BLOCKS: usize = 64;
/// Bytes per read from the device: about 14 ms at 2.4 MSPS. Must be a
/// multiple of 512.
const READ_BYTES: u32 = 65_536;
/// Sample rates worth offering, fastest first. The RTL2832U manages up to
/// 3.2 MSPS on paper, but above 2.4 most computers' USB drops samples.
const SAMPLE_RATES: [u32; 3] = [2_400_000, 2_048_000, 1_024_000];

/// The USB (vendor, product) ids of the dongles librtlsdr knows: its
/// `known_devices` table, as of the version in vendor/librtlsdr.
pub const USB_IDS: [(u16, u16); 42] = [
    (0x0bda, 0x2832), // Generic RTL2832U
    (0x0bda, 0x2838), // Generic RTL2832U OEM
    (0x0413, 0x6680), // DigitalNow Quad DVB-T PCI-E card
    (0x0413, 0x6f0f), // Leadtek WinFast DTV Dongle mini D
    (0x0458, 0x707f), // Genius TVGo DVB-T03 USB dongle (Ver. B)
    (0x0ccd, 0x00a9), // Terratec Cinergy T Stick Black (rev 1)
    (0x0ccd, 0x00b3), // Terratec NOXON DAB/DAB+ USB dongle (rev 1)
    (0x0ccd, 0x00b4), // Terratec Deutschlandradio DAB Stick
    (0x0ccd, 0x00b5), // Terratec NOXON DAB Stick - Radio Energy
    (0x0ccd, 0x00b7), // Terratec Media Broadcast DAB Stick
    (0x0ccd, 0x00b8), // Terratec BR DAB Stick
    (0x0ccd, 0x00b9), // Terratec WDR DAB Stick
    (0x0ccd, 0x00c0), // Terratec MuellerVerlag DAB Stick
    (0x0ccd, 0x00c6), // Terratec Fraunhofer DAB Stick
    (0x0ccd, 0x00d3), // Terratec Cinergy T Stick RC (Rev.3)
    (0x0ccd, 0x00d7), // Terratec T Stick PLUS
    (0x0ccd, 0x00e0), // Terratec NOXON DAB/DAB+ USB dongle (rev 2)
    (0x1554, 0x5020), // PixelView PV-DT235U(RN)
    (0x15f4, 0x0131), // Astrometa DVB-T/DVB-T2
    (0x15f4, 0x0133), // HanfTek DAB+FM+DVB-T
    (0x185b, 0x0620), // Compro Videomate U620F
    (0x185b, 0x0650), // Compro Videomate U650F
    (0x185b, 0x0680), // Compro Videomate U680F
    (0x1b80, 0xd393), // GIGABYTE GT-U7300
    (0x1b80, 0xd394), // DIKOM USB-DVBT HD
    (0x1b80, 0xd395), // Peak 102569AGPK
    (0x1b80, 0xd397), // KWorld KW-UB450-T USB DVB-T Pico TV
    (0x1b80, 0xd398), // Zaapa ZT-MINDVBZP
    (0x1b80, 0xd39d), // SVEON STV20 DVB-T USB & FM
    (0x1b80, 0xd3a4), // Twintech UT-40
    (0x1b80, 0xd3a8), // ASUS U3100MINI_PLUS_V2
    (0x1b80, 0xd3af), // SVEON STV27 DVB-T USB & FM
    (0x1b80, 0xd3b0), // SVEON STV21 DVB-T USB & FM
    (0x1d19, 0x1101), // Dexatek DK DVB-T Dongle (Logilink VG0002A)
    (0x1d19, 0x1102), // Dexatek DK DVB-T Dongle (MSI DigiVox mini II V3.0)
    (0x1d19, 0x1103), // Dexatek Technology Ltd. DK 5217 DVB-T Dongle
    (0x1d19, 0x1104), // MSI DigiVox Micro HD
    (0x1f4d, 0xa803), // Sweex DVB-T USB
    (0x1f4d, 0xb803), // GTek T803
    (0x1f4d, 0xc803), // Lifeview LV5TDeluxe
    (0x1f4d, 0xd286), // MyGica TD312
    (0x1f4d, 0xd803), // PROlectrix DV107669
];

#[derive(Debug)]
pub enum Error {
    /// librtlsdr isn't installed, or isn't one this crate can use.
    Library(String),
    /// No RTL-SDR is plugged in.
    NotFound,
    /// The device couldn't be opened or wouldn't do as asked: commonly it is
    /// in use by another program, or (on Linux) held by the kernel's DVB-T
    /// driver, or (on Windows) lacks the WinUSB driver.
    Device(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Library(why) => write!(f, "can't load librtlsdr ({why}); {INSTALL_HINT}"),
            Error::NotFound => write!(f, "no RTL-SDR found; is it plugged in?"),
            Error::Device(what) => write!(f, "RTL-SDR error: {what}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub use receiver::Block;

/// Fraction of the sample rate the filters pass cleanly; an RTL-SDR's roll
/// off some way short of the edges.
const USABLE_BANDWIDTH: f64 = 0.8;

/// The device handle, shared with the thread that reads from it.
struct Handle {
    lib: Lib,
    device: *mut c_void,
}

// librtlsdr allows a device to be controlled while another thread reads.
unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Handle {
    fn call(&self, what: &str, code: c_int) -> Result<()> {
        if code < 0 {
            return Err(Error::Device(format!("couldn't {what} (librtlsdr error {code})")));
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

/// State shared with librtlsdr's read callback.
struct Stream {
    blocks: SyncSender<Block>,
    dropped: u64,
    handle: Arc<Handle>,
}

/// An open RTL-SDR. Closing it (dropping this) stops any reception.
pub struct RtlSdr {
    handle: Arc<Handle>,
    name: String,
    reader: Option<JoinHandle<()>>,
}

/// How many RTL-SDRs are plugged in.
pub fn device_count() -> Result<u32> {
    let lib = Lib::load().map_err(Error::Library)?;
    // SAFETY: takes no arguments.
    Ok(unsafe { (lib.get_device_count)() })
}

impl RtlSdr {
    /// Open the first RTL-SDR found.
    pub fn open() -> Result<Self> {
        Self::open_nth(0)
    }

    /// Open every RTL-SDR that is attached and not in use.
    pub fn open_all() -> Result<Vec<Self>> {
        let mut failure = Error::NotFound;
        let mut found = Vec::new();
        for index in 0..device_count()? {
            match Self::open_nth(index) {
                Ok(device) => found.push(device),
                // One that won't open is in use elsewhere: left alone.
                Err(e) => failure = e,
            }
        }
        if found.is_empty() { Err(failure) } else { Ok(found) }
    }

    /// Open the RTL-SDR at `index` in the order the library lists them.
    fn open_nth(index: u32) -> Result<Self> {
        let lib = Lib::load().map_err(Error::Library)?;
        // SAFETY: takes no arguments.
        if unsafe { (lib.get_device_count)() } <= index {
            return Err(Error::NotFound);
        }
        // SAFETY: the device exists; the name is a static string.
        let name = unsafe { CStr::from_ptr((lib.get_device_name)(index)) }
            .to_string_lossy()
            .into_owned();
        let mut device = std::ptr::null_mut();
        // SAFETY: `device` is a valid place for the handle to be written.
        let code = unsafe { (lib.open)(&mut device, index) };
        if code < 0 || device.is_null() {
            return Err(Error::Device(format!(
                "couldn't open {name} (librtlsdr error {code}); it may be in use by another program{}",
                if cfg!(target_os = "linux") {
                    ", or held by the kernel's DVB-T driver (blacklist dvb_usb_rtl28xxu)"
                } else if cfg!(target_os = "windows") {
                    ", or need the WinUSB driver installed with Zadig"
                } else {
                    ""
                }
            )));
        }
        Ok(Self::opened(lib, device, &name))
    }

    /// Open an RTL-SDR that the operating system has already opened, by the
    /// file descriptor of that connection: the way on Android, where an app
    /// may not open USB devices itself. The descriptor stays the caller's to
    /// close, after this is dropped.
    pub fn open_fd(fd: i32) -> Result<Self> {
        let lib = Lib::load().map_err(Error::Library)?;
        let open_fd = lib
            .open_fd
            .ok_or_else(|| Error::Library("it can't open a device by file descriptor".into()))?;
        let mut device = std::ptr::null_mut();
        // SAFETY: `device` is a valid place for the handle to be written.
        let code = unsafe { open_fd(&mut device, fd) };
        if code < 0 || device.is_null() {
            return Err(Error::Device(format!(
                "couldn't open the RTL-SDR (librtlsdr error {code})"
            )));
        }
        Ok(Self::opened(lib, device, "RTL-SDR"))
    }

    /// Wrap a device that has just been opened.
    fn opened(lib: Lib, device: *mut c_void, name: &str) -> Self {
        let handle = Arc::new(Handle { lib, device });
        // SAFETY: `device` is open.
        let tuner = match unsafe { (handle.lib.get_tuner_type)(device) } {
            1 => "E4000",
            2 => "FC0012",
            3 => "FC0013",
            4 => "FC2580",
            5 => "R820T/R820T2/R860",
            6 => "R828D",
            _ => "unknown",
        };
        Self {
            handle,
            name: format!("{name} ({tuner} tuner)"),
            reader: None,
        }
    }

    /// The device's name and tuner chip.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Sample rates to choose from, in samples per second.
    pub fn sample_rates(&self) -> Vec<u32> {
        SAMPLE_RATES.to_vec()
    }

    pub fn set_sample_rate(&mut self, hz: u32) -> Result<()> {
        let h = &self.handle;
        // SAFETY (here and in the setters below): `device` is open.
        h.call("set the sample rate", unsafe { (h.lib.set_sample_rate)(h.device, hz) })
    }

    /// Tune to a centre frequency in Hz (about 24 MHz to 1.76 GHz with an
    /// R820T2 or R860).
    pub fn set_frequency(&mut self, hz: u32) -> Result<()> {
        let h = &self.handle;
        h.call("tune", unsafe { (h.lib.set_center_freq)(h.device, hz) })
    }

    /// Correct for the dongle's crystal being off, in parts per million.
    pub fn set_frequency_correction(&mut self, ppm: i32) -> Result<()> {
        let h = &self.handle;
        // librtlsdr reports -2 when asked for the correction already in
        // force, which a fresh device's 0 is.
        match unsafe { (h.lib.set_freq_correction)(h.device, ppm) } {
            -2 => Ok(()),
            code => h.call("set the frequency correction", code),
        }
    }

    /// The gains the tuner can be set to, in tenths of a dB, lowest first.
    pub fn gains(&self) -> Result<Vec<i32>> {
        let h = &self.handle;
        // SAFETY: a null buffer asks for the count.
        let count = unsafe { (h.lib.get_tuner_gains)(h.device, std::ptr::null_mut()) };
        h.call("read the tuner's gains", count)?;
        let mut gains = vec![0; count as usize];
        // SAFETY: `gains` has room for `count` values.
        h.call("read the tuner's gains", unsafe {
            (h.lib.get_tuner_gains)(h.device, gains.as_mut_ptr())
        })?;
        gains.sort_unstable();
        Ok(gains)
    }

    /// Set the tuner's gain in tenths of a dB, to the nearest it supports,
    /// with the automatic gain controls off.
    pub fn set_gain(&mut self, tenths_db: i32) -> Result<()> {
        let nearest = self
            .gains()?
            .into_iter()
            .min_by_key(|g| (g - tenths_db).abs())
            .unwrap_or(tenths_db);
        let h = &self.handle;
        h.call("switch to manual gain", unsafe {
            (h.lib.set_tuner_gain_mode)(h.device, 1)
        })?;
        h.call("turn off the digital AGC", unsafe { (h.lib.set_agc_mode)(h.device, 0) })?;
        h.call("set the gain", unsafe { (h.lib.set_tuner_gain)(h.device, nearest) })
    }

    /// Set the gain as a level from 0 (lowest) to `of` (highest), spread
    /// over whatever range of gains the tuner has.
    pub fn set_gain_level(&mut self, level: u8, of: u8) -> Result<()> {
        let gains = self.gains()?;
        let Some(top) = gains.len().checked_sub(1) else {
            return Ok(());
        };
        let index = (level.min(of) as usize * top + of as usize / 2) / of.max(1) as usize;
        self.set_gain(gains[index])
    }

    /// Power an amplifier at the antenna through the coax, on dongles that
    /// can (RTL-SDR Blog V3 and V4).
    pub fn set_bias_tee(&mut self, on: bool) -> Result<()> {
        let h = &self.handle;
        let set = h
            .lib
            .set_bias_tee
            .ok_or_else(|| Error::Library("it is too old to control the bias tee".into()))?;
        h.call("switch the bias tee", unsafe { set(h.device, on as c_int) })
    }

    /// Start receiving. Blocks of I/Q arrive on the returned channel until
    /// [`stop`](Self::stop) is called, the receiver is dropped, or the
    /// device goes away, at which point the channel closes.
    pub fn start(&mut self) -> Result<Receiver<Block>> {
        self.stop();
        let h = self.handle.clone();
        h.call("reset the sample buffer", unsafe { (h.lib.reset_buffer)(h.device) })?;
        let (blocks, receiver) = sync_channel(QUEUE_BLOCKS);
        self.reader = Some(std::thread::spawn(move || {
            let mut stream = Stream {
                blocks,
                dropped: 0,
                handle: h.clone(),
            };
            let context = &raw mut stream as *mut c_void;
            // Returns when cancelled or the device fails. SAFETY: `stream`
            // outlives the call, during which alone `on_read` is called.
            unsafe { (h.lib.read_async)(h.device, on_read, context, 0, READ_BYTES) };
        }));
        Ok(receiver)
    }

    /// Stop receiving; does nothing if not started.
    pub fn stop(&mut self) {
        if let Some(reader) = self.reader.take() {
            // SAFETY: `device` is open.
            unsafe { (self.handle.lib.cancel_async)(self.handle.device) };
            reader.join().ok();
        }
    }
}

impl Drop for RtlSdr {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The device's unsigned 8-bit samples as signed 16-bit ones.
fn widen(bytes: &[u8]) -> Vec<i16> {
    // 127.5 is the middle of the 8-bit range.
    bytes.iter().map(|&b| (b as i16 * 2 - 255) * 128).collect()
}

/// Called by librtlsdr on the reading thread for every buffer of samples.
extern "C" fn on_read(buf: *mut u8, len: u32, ctx: *mut c_void) {
    // SAFETY: librtlsdr passes `len` bytes at `buf`, and the `Stream` given
    // to `read_async`.
    let (bytes, stream) = unsafe {
        (
            std::slice::from_raw_parts(buf, len as usize),
            &mut *(ctx as *mut Stream),
        )
    };
    let block = Block {
        iq: widen(bytes),
        dropped: stream.dropped,
    };
    match stream.blocks.try_send(block) {
        Ok(()) => stream.dropped = 0,
        Err(TrySendError::Full(block)) => stream.dropped = block.dropped + len as u64 / 2,
        // Nobody is listening any more: end the read.
        Err(TrySendError::Disconnected(_)) => {
            // SAFETY: `device` is open for as long as `handle` is held.
            unsafe { (stream.handle.lib.cancel_async)(stream.handle.device) };
        }
    }
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

impl receiver::Receiver for RtlSdr {
    fn name(&self) -> String {
        RtlSdr::name(self).to_string()
    }

    fn sample_rates(&self) -> receiver::Result<Vec<u32>> {
        Ok(RtlSdr::sample_rates(self))
    }

    fn usable_bandwidth(&self) -> f64 {
        USABLE_BANDWIDTH
    }

    fn configure(&mut self, settings: &receiver::Settings) -> receiver::Result<()> {
        self.set_sample_rate(settings.sample_rate)?;
        self.set_frequency_correction(settings.ppm)?;
        self.set_gain_level(settings.gain, receiver::GAIN_LEVELS)?;
        // Left alone when off: older libraries can't switch it at all.
        if settings.bias_tee {
            self.set_bias_tee(true)?;
        }
        Ok(())
    }

    fn set_frequency(&mut self, hz: u32) -> receiver::Result<()> {
        Ok(RtlSdr::set_frequency(self, hz)?)
    }

    fn start(&mut self) -> receiver::Result<Receiver<Block>> {
        Ok(RtlSdr::start(self)?)
    }

    fn stop(&mut self) -> receiver::Result<()> {
        RtlSdr::stop(self);
        Ok(())
    }
}

/// Finds and opens the first RTL-SDR attached.
pub struct Driver;

impl receiver::Driver for Driver {
    fn id(&self) -> &'static str {
        "rtlsdr"
    }

    fn name(&self) -> &'static str {
        "RTL-SDR"
    }

    fn open(&self) -> receiver::Result<Box<dyn receiver::Receiver>> {
        Ok(Box::new(RtlSdr::open()?))
    }

    fn open_all(&self) -> receiver::Result<Vec<Box<dyn receiver::Receiver>>> {
        let devices = RtlSdr::open_all()?;
        Ok(devices.into_iter().map(|device| Box::new(device) as _).collect())
    }

    fn usb_ids(&self) -> Vec<(u16, u16)> {
        USB_IDS.to_vec()
    }

    fn open_fd(&self, fd: i32) -> receiver::Result<Box<dyn receiver::Receiver>> {
        Ok(Box::new(RtlSdr::open_fd(fd)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widens_around_the_middle() {
        assert_eq!(widen(&[0, 127, 128, 255]), [-32640, -128, 128, 32640]);
    }
}
