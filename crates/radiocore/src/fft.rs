//! Fourier transforms of one length and direction. With the `fftw` feature
//! they are done by FFTW, loaded at run time, when libfftw3f can be found;
//! otherwise, and always without the feature, by RustFFT.

use std::sync::Arc;

use rustfft::FftPlanner;
use rustfft::num_complex::Complex32;

pub struct Fft {
    imp: Imp,
}

enum Imp {
    Rust {
        fft: Arc<dyn rustfft::Fft<f32>>,
        scratch: Vec<Complex32>,
    },
    #[cfg(feature = "fftw")]
    Fftw(fftw::Plan),
}

impl Fft {
    pub fn forward(n: usize) -> Self {
        Self::new(n, false)
    }

    pub fn inverse(n: usize) -> Self {
        Self::new(n, true)
    }

    fn new(n: usize, inverse: bool) -> Self {
        #[cfg(feature = "fftw")]
        if let Some(plan) = fftw::Plan::new(n, inverse) {
            return Self { imp: Imp::Fftw(plan) };
        }
        Self::rust(n, inverse)
    }

    fn rust(n: usize, inverse: bool) -> Self {
        let mut planner = FftPlanner::new();
        let fft = if inverse {
            planner.plan_fft_inverse(n)
        } else {
            planner.plan_fft_forward(n)
        };
        let scratch = vec![Complex32::ZERO; fft.get_inplace_scratch_len()];
        Self {
            imp: Imp::Rust { fft, scratch },
        }
    }

    /// Transform `buf`, which must be the planned length, in place. Neither
    /// direction is normalised.
    pub fn process(&mut self, buf: &mut [Complex32]) {
        match &mut self.imp {
            Imp::Rust { fft, scratch } => fft.process_with_scratch(buf, scratch),
            #[cfg(feature = "fftw")]
            Imp::Fftw(plan) => plan.process(buf),
        }
    }
}

/// Which library new transforms are done by: "FFTW" or "RustFFT".
pub fn backend() -> &'static str {
    #[cfg(feature = "fftw")]
    if fftw::lib().is_some() {
        return "FFTW";
    }
    "RustFFT"
}

/// The part of libfftw3f's C interface (`fftw3.h`) this crate uses.
#[cfg(feature = "fftw")]
mod fftw {
    use std::ffi::{c_int, c_uint, c_void};
    use std::sync::{Mutex, OnceLock};

    use libloading::Library;
    use rustfft::num_complex::Complex32;

    const FORWARD: c_int = -1;
    const BACKWARD: c_int = 1;
    /// `FFTW_MEASURE`: time a few ways of doing the transform and keep the
    /// fastest. FFTW remembers what it learnt for the rest of the run, so
    /// planning the same length again is quick.
    const MEASURE: c_uint = 0;

    type RawPlan = *mut c_void;

    pub struct Lib {
        /// `fftwf_complex` is `float[2]`, laid out as `Complex32` is.
        plan_dft_1d: unsafe extern "C" fn(c_int, *mut Complex32, *mut Complex32, c_int, c_uint) -> RawPlan,
        execute: unsafe extern "C" fn(RawPlan),
        destroy_plan: unsafe extern "C" fn(RawPlan),
        malloc: unsafe extern "C" fn(usize) -> *mut c_void,
        free: unsafe extern "C" fn(*mut c_void),
        /// Making and destroying plans isn't thread-safe; executing them is.
        planner: Mutex<()>,
        /// Keeps the functions above loaded.
        _library: Library,
    }

    /// libfftw3f, loaded the first time it is asked for, if it can be.
    pub fn lib() -> Option<&'static Lib> {
        static LIB: OnceLock<Option<Lib>> = OnceLock::new();
        LIB.get_or_init(|| Lib::load().ok()).as_ref()
    }

    impl Lib {
        fn load() -> Result<Self, libloading::Error> {
            // On Linux the unversioned name only exists where development
            // files are installed. Windows looks beside the program first.
            // Homebrew's directories aren't searched by default.
            #[cfg(target_os = "windows")]
            let names = ["libfftw3f-3.dll", "fftw3f.dll"];
            #[cfg(target_os = "macos")]
            let names = [
                "libfftw3f.3.dylib",
                "libfftw3f.dylib",
                "/opt/homebrew/lib/libfftw3f.dylib",
                "/usr/local/lib/libfftw3f.dylib",
            ];
            #[cfg(not(any(target_os = "windows", target_os = "macos")))]
            let names = ["libfftw3f.so.3", "libfftw3f.so"];
            let mut result = Err(None);
            for name in names {
                // SAFETY: loading libfftw3f runs no initialisers with requirements.
                match unsafe { Library::new(name) } {
                    Ok(library) => {
                        result = Ok(library);
                        break;
                    }
                    Err(e) => result = Err(Some(e)),
                }
            }
            let library = result.map_err(|e| e.expect("there are names to try"))?;
            macro_rules! symbol {
                ($name:literal) => {
                    // SAFETY: the field this initialises has the signature
                    // `fftw3.h` declares for the function of this name.
                    *unsafe { library.get(concat!("fftwf_", $name, "\0").as_bytes()) }?
                };
            }
            Ok(Self {
                plan_dft_1d: symbol!("plan_dft_1d"),
                execute: symbol!("execute"),
                destroy_plan: symbol!("destroy_plan"),
                malloc: symbol!("malloc"),
                free: symbol!("free"),
                planner: Mutex::new(()),
                _library: library,
            })
        }
    }

    /// An in-place plan on a buffer of FFTW's own, so it is aligned for
    /// FFTW's SIMD code whatever the caller's buffer is.
    pub struct Plan {
        lib: &'static Lib,
        raw: RawPlan,
        buf: *mut Complex32,
        n: usize,
    }

    // SAFETY: the plan and its buffer belong to this value alone, and are
    // only used through `&mut self` or when it is dropped.
    unsafe impl Send for Plan {}
    // SAFETY: as above; nothing is done through `&self`.
    unsafe impl Sync for Plan {}

    impl Plan {
        pub fn new(n: usize, inverse: bool) -> Option<Self> {
            let lib = lib()?;
            let len = c_int::try_from(n).ok()?;
            let sign = if inverse { BACKWARD } else { FORWARD };
            let _planning = lib.planner.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: a plain allocation, freed in `drop` (or below).
            let buf = unsafe { (lib.malloc)(n * size_of::<Complex32>()) }.cast::<Complex32>();
            if buf.is_null() {
                return None;
            }
            // SAFETY: `buf` holds `n` complex values; planning may write to it.
            let raw = unsafe { (lib.plan_dft_1d)(len, buf, buf, sign, MEASURE) };
            if raw.is_null() {
                // SAFETY: `buf` came from `fftwf_malloc` and nothing else has it.
                unsafe { (lib.free)(buf.cast()) };
                return None;
            }
            Some(Self { lib, raw, buf, n })
        }

        pub fn process(&mut self, data: &mut [Complex32]) {
            assert_eq!(data.len(), self.n, "wrong length for this FFT");
            // SAFETY: `buf` holds `n` complex values and only this plan uses it.
            let buf = unsafe { std::slice::from_raw_parts_mut(self.buf, self.n) };
            buf.copy_from_slice(data);
            // SAFETY: the plan is live and transforms `buf`, which it was made on.
            unsafe { (self.lib.execute)(self.raw) };
            data.copy_from_slice(buf);
        }
    }

    impl Drop for Plan {
        fn drop(&mut self) {
            let _planning = self.lib.planner.lock().unwrap_or_else(|e| e.into_inner());
            // SAFETY: both were made in `new`, and nothing uses them after this.
            unsafe {
                (self.lib.destroy_plan)(self.raw);
                (self.lib.free)(self.buf.cast());
            }
        }
    }
}

#[cfg(all(test, feature = "fftw"))]
mod tests {
    use super::*;

    #[test]
    fn fftw_agrees_with_rustfft() {
        if fftw::lib().is_none() {
            eprintln!("libfftw3f isn't installed; nothing to compare");
            return;
        }
        // A length with factors other than 2, as the channelizer's can be.
        let n = 1500;
        let input: Vec<Complex32> = (0..n)
            .map(|i| Complex32::new((i as f32 * 0.37).sin(), (i as f32 * 0.11).cos()))
            .collect();
        for inverse in [false, true] {
            let mut ours = Fft::new(n, inverse);
            assert!(matches!(ours.imp, Imp::Fftw(_)));
            let (mut a, mut b) = (input.clone(), input.clone());
            ours.process(&mut a);
            Fft::rust(n, inverse).process(&mut b);
            let worst = a.iter().zip(&b).map(|(x, y)| (x - y).norm()).fold(0.0, f32::max);
            assert!(worst < 1e-2, "differs by {worst}");
        }
    }
}
