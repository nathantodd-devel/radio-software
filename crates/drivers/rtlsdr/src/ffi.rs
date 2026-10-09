//! The part of librtlsdr's C interface (`rtl-sdr.h`) this crate uses.

use std::ffi::{c_char, c_int, c_void};

use libloading::Library;

type Device = *mut c_void;
pub type ReadCallback = extern "C" fn(buf: *mut u8, len: u32, ctx: *mut c_void);

pub struct Lib {
    pub get_device_count: unsafe extern "C" fn() -> u32,
    pub get_device_name: unsafe extern "C" fn(u32) -> *const c_char,
    pub open: unsafe extern "C" fn(*mut Device, u32) -> c_int,
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
    _library: Library,
}

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
        Ok(Self {
            get_device_count: symbol!("get_device_count"),
            get_device_name: symbol!("get_device_name"),
            open: symbol!("open"),
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
