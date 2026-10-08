//! Plays a stream of mono 16-bit audio on the default output device.
//!
//! On Linux this pipes to `aplay` (ALSA, and through it PipeWire or
//! PulseAudio); elsewhere it uses the system's native audio API.

#[cfg(target_os = "linux")]
#[path = "aplay.rs"]
mod backend;
#[cfg(not(target_os = "linux"))]
#[path = "native.rs"]
mod backend;

/// An open audio output. Audio stops when it is dropped.
pub struct Output(backend::Output);

impl Output {
    /// Open the default output device for mono audio at `rate` samples per
    /// second.
    pub fn open(rate: u32) -> Result<Self, String> {
        backend::Output::open(rate).map(Self)
    }

    /// Queue samples for playback. To keep the output running without gaps,
    /// write continuously, in real time, with silence when there is nothing
    /// to play.
    pub fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        self.0.write(pcm)
    }
}
