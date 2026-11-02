//! Plays a stream of mono 16-bit audio on an output device.
//!
//! On Linux this pipes to `aplay` (ALSA, and through it PipeWire or
//! PulseAudio), which also finds the devices through `pactl`; elsewhere it
//! uses the system's native audio API.

// Used wherever the native audio API is, and tested everywhere.
#[cfg_attr(target_os = "linux", allow(dead_code))]
mod jitter;

#[cfg(target_os = "linux")]
#[path = "aplay.rs"]
mod backend;
#[cfg(not(target_os = "linux"))]
#[path = "native.rs"]
mod backend;

/// A device that audio can be played on.
#[derive(Clone, Debug, PartialEq)]
pub struct Device {
    /// What to give [`Output::open`]. It stays the same between runs.
    pub id: String,
    /// What to call it.
    pub name: String,
}

/// The output devices connected now. Empty when they can't be listed, in
/// which case only the default output is available.
pub fn devices() -> Vec<Device> {
    backend::devices()
}

/// An open audio output. Audio stops when it is dropped.
pub struct Output(backend::Output);

impl Output {
    /// Open an output device for mono audio at `rate` samples per second:
    /// the one with this [`Device::id`], or the default for `None`. A device
    /// that isn't connected any more gets the default too, so unplugging it
    /// doesn't cost the sound.
    pub fn open(rate: u32, device: Option<&str>) -> Result<Self, String> {
        let connected = device.filter(|id| devices().iter().any(|d| d.id == *id));
        if let (Some(id), None) = (device, connected) {
            eprintln!("audio output {id:?} isn't connected; using the default");
        }
        backend::Output::open(rate, connected).map(Self)
    }

    /// Queue samples for playback. To keep the output running without gaps,
    /// write continuously, in real time, with silence when there is nothing
    /// to play.
    pub fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        self.0.write(pcm)
    }
}
