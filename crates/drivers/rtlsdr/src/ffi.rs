//! The part of librtlsdr's C interface (`rtl-sdr.h`) this crate uses.

use std::ffi::{c_char, c_int, c_void};

#[cfg(not(target_os = "android"))]
use libloading::Library;

type Device = *mut c_void;
pub type ReadCallback = extern "C" fn(buf: *mut u8, len: u32, ctx: *mut c_void);

pub struct Lib {
    pub get_device_count: unsafe extern "C" fn() -> u32,
    pub get_device_name: unsafe extern "C" fn(u32) -> *const c_char,
    pub open: unsafe extern "C" fn(*mut Device, u32) -> c_int,
    /// Opens a device the operating system has already opened for us, by its
    /// file descriptor. Only in the copy of the library compiled in on
    /// Android (see vendor/librtlsdr).
    pub open_fd: Option<unsafe extern "C" fn(*mut Device, c_int) -> c_int>,
    pub close: unsafe extern "C" fn(Device) -> c_int,
    pub get_tuner_type: unsafe extern "C" fn(Device) -> c_int,
    pub set_center_freq: unsafe extern "C" fn(Device, u32) -> c_int,
    pub set_sample_rate: unsafe extern "C" fn(Device, u32) -> c_int,
    pub set_freq_correction: unsafe extern "C" fn(Device, c_int) -> c_int,
    pub set_tuner_gain_mode: unsafe extern "C" fn(Device, c_int) -> c_int,
    pub get_tuner_gains: unsafe extern "C" fn(Device, *mut c_int) -> c_int,
    pub set_tuner_gain: unsafe extern "C" fn(Device, c_int) -> c_int,
    pub set_agc_mode: unsafe extern "C" fn(Device, c_int) -> c_int,
    /// Only in newer libraries.
    pub set_bias_tee: Option<unsafe extern "C" fn(Device, c_int) -> c_int>,
    pub reset_buffer: unsafe extern "C" fn(Device) -> c_int,
    pub read_async: unsafe extern "C" fn(Device, ReadCallback, *mut c_void, u32, u32) -> c_int,
    pub cancel_async: unsafe extern "C" fn(Device) -> c_int,
    /// Keeps the functions above loaded.
    #[cfg(not(target_os = "android"))]
    _library: Library,
}

/// librtlsdr as compiled into this crate (see build.rs).
#[cfg(target_os = "android")]
mod linked {
    use super::{Device, ReadCallback};
    use std::ffi::{c_char, c_int, c_void};

    // Makes sure libusb, which librtlsdr calls, is linked in.
    use libusb1_sys as _;

    unsafe extern "C" {
        pub fn rtlsdr_get_device_count() -> u32;
        pub fn rtlsdr_get_device_name(index: u32) -> *const c_char;
        pub fn rtlsdr_open(device: *mut Device, index: u32) -> c_int;
        pub fn rtlsdr_open_fd(device: *mut Device, fd: c_int) -> c_int;
        pub fn rtlsdr_close(device: Device) -> c_int;
        pub fn rtlsdr_get_tuner_type(device: Device) -> c_int;
        pub fn rtlsdr_set_center_freq(device: Device, hz: u32) -> c_int;
        pub fn rtlsdr_set_sample_rate(device: Device, hz: u32) -> c_int;
        pub fn rtlsdr_set_freq_correction(device: Device, ppm: c_int) -> c_int;
        pub fn rtlsdr_set_tuner_gain_mode(device: Device, manual: c_int) -> c_int;
        pub fn rtlsdr_get_tuner_gains(device: Device, gains: *mut c_int) -> c_int;
        pub fn rtlsdr_set_tuner_gain(device: Device, gain: c_int) -> c_int;
        pub fn rtlsdr_set_agc_mode(device: Device, on: c_int) -> c_int;
        pub fn rtlsdr_set_bias_tee(device: Device, on: c_int) -> c_int;
        pub fn rtlsdr_reset_buffer(device: Device) -> c_int;
        pub fn rtlsdr_read_async(device: Device, cb: ReadCallback, ctx: *mut c_void, bufs: u32, len: u32) -> c_int;
        pub fn rtlsdr_cancel_async(device: Device) -> c_int;
    }
}

#[cfg(target_os = "android")]
impl Lib {
    pub fn load() -> Result<Self, String> {
        use linked::*;
        Ok(Self {
            get_device_count: rtlsdr_get_device_count,
            get_device_name: rtlsdr_get_device_name,
            open: rtlsdr_open,
            open_fd: Some(rtlsdr_open_fd),
            close: rtlsdr_close,
            get_tuner_type: rtlsdr_get_tuner_type,
            set_center_freq: rtlsdr_set_center_freq,
            set_sample_rate: rtlsdr_set_sample_rate,
            set_freq_correction: rtlsdr_set_freq_correction,
            set_tuner_gain_mode: rtlsdr_set_tuner_gain_mode,
            get_tuner_gains: rtlsdr_get_tuner_gains,
            set_tuner_gain: rtlsdr_set_tuner_gain,
            set_agc_mode: rtlsdr_set_agc_mode,
            set_bias_tee: Some(rtlsdr_set_bias_tee),
            reset_buffer: rtlsdr_reset_buffer,
            read_async: rtlsdr_read_async,
            cancel_async: rtlsdr_cancel_async,
        })
    }
}

#[cfg(not(target_os = "android"))]
impl Lib {
    pub fn load() -> Result<Self, String> {
        // The library's version number has changed over the years, and the
        // unversioned name only exists where development files are installed.
        // Windows looks beside the program first, which is where a release
        // puts the library. Homebrew's directories aren't searched by default.
        #[cfg(target_os = "windows")]
        let names = ["rtlsdr.dll", "librtlsdr.dll"];
        #[cfg(target_os = "macos")]
        let names = [
            "librtlsdr.2.dylib",
            "librtlsdr.0.dylib",
            "librtlsdr.dylib",
            "/opt/homebrew/lib/librtlsdr.dylib",
            "/usr/local/lib/librtlsdr.dylib",
        ];
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let names = ["librtlsdr.so.2", "librtlsdr.so.0", "librtlsdr.so"];
        let mut failure = String::new();
        // SAFETY: loading librtlsdr runs no initialisers with requirements.
        let library = names.iter().find_map(|name| match unsafe { Library::new(name) } {
            Ok(library) => Some(library),
            Err(e) => {
                failure = e.to_string();
                None
            }
        });
        let library = library.ok_or(failure)?;
        macro_rules! symbol {
            ($name:literal) => {
                // SAFETY: the field this initialises has the signature
                // `rtl-sdr.h` declares for the function of this name.
                *unsafe { library.get(concat!("rtlsdr_", $name, "\0").as_bytes()) }.map_err(|e| e.to_string())?
            };
        }
        // SAFETY: as above; absent from older libraries, which is fine.
        let set_bias_tee = unsafe { library.get(b"rtlsdr_set_bias_tee\0") }.ok().map(|f| *f);
        // SAFETY: as above; only in a library built from this crate's copy.
        let open_fd = unsafe { library.get(b"rtlsdr_open_fd\0") }.ok().map(|f| *f);
        Ok(Self {
            get_device_count: symbol!("get_device_count"),
            get_device_name: symbol!("get_device_name"),
            open: symbol!("open"),
            open_fd,
            close: symbol!("close"),
            get_tuner_type: symbol!("get_tuner_type"),
            set_center_freq: symbol!("set_center_freq"),
            set_sample_rate: symbol!("set_sample_rate"),
            set_freq_correction: symbol!("set_freq_correction"),
            set_tuner_gain_mode: symbol!("set_tuner_gain_mode"),
            get_tuner_gains: symbol!("get_tuner_gains"),
            set_tuner_gain: symbol!("set_tuner_gain"),
            set_agc_mode: symbol!("set_agc_mode"),
            set_bias_tee,
            reset_buffer: symbol!("reset_buffer"),
            read_async: symbol!("read_async"),
            cancel_async: symbol!("cancel_async"),
            _library: library,
        })
    }
}
