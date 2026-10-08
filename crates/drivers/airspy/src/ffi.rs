//! The part of libairspy's C interface (`airspy.h`) this crate uses.

use std::ffi::{c_char, c_int, c_void};

use libloading::Library;

pub const SUCCESS: c_int = 0;
pub const ERROR_NOT_FOUND: c_int = -5;
pub const ERROR_BUSY: c_int = -6;
/// `AIRSPY_SAMPLE_INT16_IQ`
pub const SAMPLE_INT16_IQ: c_int = 2;

/// `airspy_transfer_t`
#[repr(C)]
pub struct Transfer {
    pub device: *mut c_void,
    pub ctx: *mut c_void,
    pub samples: *mut c_void,
    pub sample_count: c_int,
    pub dropped_samples: u64,
    pub sample_type: c_int,
}

pub type BlockCallback = extern "C" fn(*mut Transfer) -> c_int;
type Device = *mut c_void;

pub struct Lib {
    pub open: unsafe extern "C" fn(*mut Device) -> c_int,
    pub close: unsafe extern "C" fn(Device) -> c_int,
    pub get_samplerates: unsafe extern "C" fn(Device, *mut u32, u32) -> c_int,
    pub set_samplerate: unsafe extern "C" fn(Device, u32) -> c_int,
    pub set_sample_type: unsafe extern "C" fn(Device, c_int) -> c_int,
    pub set_freq: unsafe extern "C" fn(Device, u32) -> c_int,
    pub set_linearity_gain: unsafe extern "C" fn(Device, u8) -> c_int,
    pub set_sensitivity_gain: unsafe extern "C" fn(Device, u8) -> c_int,
    pub set_rf_bias: unsafe extern "C" fn(Device, u8) -> c_int,
    pub start_rx: unsafe extern "C" fn(Device, BlockCallback, *mut c_void) -> c_int,
    pub stop_rx: unsafe extern "C" fn(Device) -> c_int,
    pub error_name: unsafe extern "C" fn(c_int) -> *const c_char,
    /// Keeps the functions above loaded.
    _library: Library,
}

impl Lib {
    pub fn load() -> Result<Self, String> {
        // On Linux the unversioned name only exists where development files
        // are installed; the versioned one is what the runtime package ships.
        // Windows looks beside the program first, which is where a release
        // puts the library. Homebrew's directories aren't searched by default.
        #[cfg(target_os = "linux")]
        let names = ["libairspy.so.0", "libairspy.so"];
        #[cfg(target_os = "windows")]
        let names = ["airspy.dll", "libairspy.dll"];
        #[cfg(target_os = "macos")]
        let names = [
            "libairspy.0.dylib",
            "libairspy.dylib",
            "/opt/homebrew/lib/libairspy.dylib",
            "/usr/local/lib/libairspy.dylib",
        ];
        let mut failure = String::new();
        // SAFETY: loading libairspy runs no initialisers with requirements.
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
                // `airspy.h` declares for the function of this name.
                *unsafe { library.get(concat!("airspy_", $name, "\0").as_bytes()) }.map_err(|e| e.to_string())?
            };
        }
        Ok(Self {
            open: symbol!("open"),
            close: symbol!("close"),
            get_samplerates: symbol!("get_samplerates"),
            set_samplerate: symbol!("set_samplerate"),
            set_sample_type: symbol!("set_sample_type"),
            set_freq: symbol!("set_freq"),
            set_linearity_gain: symbol!("set_linearity_gain"),
            set_sensitivity_gain: symbol!("set_sensitivity_gain"),
            set_rf_bias: symbol!("set_rf_bias"),
            start_rx: symbol!("start_rx"),
            stop_rx: symbol!("stop_rx"),
            error_name: symbol!("error_name"),
            _library: library,
        })
    }
}
