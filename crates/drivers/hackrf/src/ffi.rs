//! The part of libhackrf's C interface (`hackrf.h`) this crate uses.

use std::ffi::{c_char, c_int, c_void};

#[cfg(not(any(windows, feature = "vendored")))]
use libloading::Library;

pub type Device = *mut c_void;

/// `hackrf_transfer`: one buffer of samples handed to the callback.
#[repr(C)]
pub struct Transfer {
    pub device: Device,
    pub buffer: *mut u8,
    pub buffer_length: c_int,
    pub valid_length: c_int,
    pub rx_ctx: *mut c_void,
    pub tx_ctx: *mut c_void,
}

/// Returns non-zero to end the stream.
pub type SampleCallback = extern "C" fn(transfer: *mut Transfer) -> c_int;

/// The start of `hackrf_device_list_t`: the library has added to the end of
/// it over the years, but not to this.
#[repr(C)]
pub struct DeviceList {
    pub serial_numbers: *mut *mut c_char,
    pub usb_board_ids: *mut c_int,
    pub usb_device_index: *mut c_int,
    pub devicecount: c_int,
}

pub struct Lib {
    pub init: unsafe extern "C" fn() -> c_int,
    pub error_name: unsafe extern "C" fn(c_int) -> *const c_char,
    pub device_list: unsafe extern "C" fn() -> *mut DeviceList,
    pub device_list_open: unsafe extern "C" fn(*mut DeviceList, c_int, *mut Device) -> c_int,
    pub device_list_free: unsafe extern "C" fn(*mut DeviceList),
    pub usb_board_id_name: unsafe extern "C" fn(c_int) -> *const c_char,
    pub close: unsafe extern "C" fn(Device) -> c_int,
    pub set_freq: unsafe extern "C" fn(Device, u64) -> c_int,
    pub set_sample_rate: unsafe extern "C" fn(Device, f64) -> c_int,
    pub compute_baseband_filter_bw: unsafe extern "C" fn(u32) -> u32,
    pub set_baseband_filter_bandwidth: unsafe extern "C" fn(Device, u32) -> c_int,
    pub set_lna_gain: unsafe extern "C" fn(Device, u32) -> c_int,
    pub set_vga_gain: unsafe extern "C" fn(Device, u32) -> c_int,
    pub set_amp_enable: unsafe extern "C" fn(Device, u8) -> c_int,
    pub set_antenna_enable: unsafe extern "C" fn(Device, u8) -> c_int,
    pub start_rx: unsafe extern "C" fn(Device, SampleCallback, *mut c_void) -> c_int,
    pub stop_rx: unsafe extern "C" fn(Device) -> c_int,
    /// Keeps the functions above loaded.
    #[cfg(not(any(windows, feature = "vendored")))]
    _library: Library,
}

/// libhackrf as compiled into this crate (see build.rs).
#[cfg(any(windows, feature = "vendored"))]
mod linked {
    use super::{Device, DeviceList, SampleCallback};
    use std::ffi::{c_char, c_int, c_void};

    // Makes sure libusb, which libhackrf calls, is linked in.
    use libusb1_sys as _;

    unsafe extern "C" {
        pub fn hackrf_init() -> c_int;
        pub fn hackrf_error_name(code: c_int) -> *const c_char;
        pub fn hackrf_device_list() -> *mut DeviceList;
        pub fn hackrf_device_list_open(list: *mut DeviceList, index: c_int, device: *mut Device) -> c_int;
        pub fn hackrf_device_list_free(list: *mut DeviceList);
        pub fn hackrf_usb_board_id_name(id: c_int) -> *const c_char;
        pub fn hackrf_close(device: Device) -> c_int;
        pub fn hackrf_set_freq(device: Device, hz: u64) -> c_int;
        pub fn hackrf_set_sample_rate(device: Device, hz: f64) -> c_int;
        pub fn hackrf_compute_baseband_filter_bw(hz: u32) -> u32;
        pub fn hackrf_set_baseband_filter_bandwidth(device: Device, hz: u32) -> c_int;
        pub fn hackrf_set_lna_gain(device: Device, db: u32) -> c_int;
        pub fn hackrf_set_vga_gain(device: Device, db: u32) -> c_int;
        pub fn hackrf_set_amp_enable(device: Device, on: u8) -> c_int;
        pub fn hackrf_set_antenna_enable(device: Device, on: u8) -> c_int;
        pub fn hackrf_start_rx(device: Device, callback: SampleCallback, context: *mut c_void) -> c_int;
        pub fn hackrf_stop_rx(device: Device) -> c_int;
    }
}

#[cfg(any(windows, feature = "vendored"))]
impl Lib {
    pub fn load() -> Result<Self, String> {
        use linked::*;
        Ok(Self {
            init: hackrf_init,
            error_name: hackrf_error_name,
            device_list: hackrf_device_list,
            device_list_open: hackrf_device_list_open,
            device_list_free: hackrf_device_list_free,
            usb_board_id_name: hackrf_usb_board_id_name,
            close: hackrf_close,
            set_freq: hackrf_set_freq,
            set_sample_rate: hackrf_set_sample_rate,
            compute_baseband_filter_bw: hackrf_compute_baseband_filter_bw,
            set_baseband_filter_bandwidth: hackrf_set_baseband_filter_bandwidth,
            set_lna_gain: hackrf_set_lna_gain,
            set_vga_gain: hackrf_set_vga_gain,
            set_amp_enable: hackrf_set_amp_enable,
            set_antenna_enable: hackrf_set_antenna_enable,
            start_rx: hackrf_start_rx,
            stop_rx: hackrf_stop_rx,
        })
    }
}

#[cfg(not(any(windows, feature = "vendored")))]
impl Lib {
    pub fn load() -> Result<Self, String> {
        // The unversioned name only exists where development files are
        // installed. Homebrew's directories aren't searched by default.
        #[cfg(target_os = "macos")]
        let names = [
            "libhackrf.0.dylib",
            "libhackrf.dylib",
            "/opt/homebrew/lib/libhackrf.dylib",
            "/usr/local/lib/libhackrf.dylib",
        ];
        #[cfg(not(target_os = "macos"))]
        let names = ["libhackrf.so.0", "libhackrf.so"];
        let mut failure = String::new();
        // SAFETY: loading libhackrf runs no initialisers with requirements.
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
                // `hackrf.h` declares for the function of this name.
                *unsafe { library.get(concat!("hackrf_", $name, "\0").as_bytes()) }.map_err(|e| e.to_string())?
            };
        }
        Ok(Self {
            init: symbol!("init"),
            error_name: symbol!("error_name"),
            device_list: symbol!("device_list"),
            device_list_open: symbol!("device_list_open"),
            device_list_free: symbol!("device_list_free"),
            usb_board_id_name: symbol!("usb_board_id_name"),
            close: symbol!("close"),
            set_freq: symbol!("set_freq"),
            set_sample_rate: symbol!("set_sample_rate"),
            compute_baseband_filter_bw: symbol!("compute_baseband_filter_bw"),
            set_baseband_filter_bandwidth: symbol!("set_baseband_filter_bandwidth"),
            set_lna_gain: symbol!("set_lna_gain"),
            set_vga_gain: symbol!("set_vga_gain"),
            set_amp_enable: symbol!("set_amp_enable"),
            set_antenna_enable: symbol!("set_antenna_enable"),
            start_rx: symbol!("start_rx"),
            stop_rx: symbol!("stop_rx"),
            _library: library,
        })
    }
}
