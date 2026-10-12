use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

use crate::jitter::Jitter;

pub struct Output {
    /// The audio waiting to be played, shared with the device's callback.
    waiting: Arc<Mutex<Jitter>>,
    _stream: Stream,
}

impl Output {
    pub fn open(rate: u32) -> Result<Self, String> {
        let device = cpal::default_host()
            .default_output_device()
            .ok_or("no audio output device")?;
        let supported = device
            .default_output_config()
            .map_err(|e| format!("audio output: {e}"))?;
        let format = supported.sample_format();
        let config: StreamConfig = supported.into();
        let channels = config.channels as usize;

        let waiting = Arc::new(Mutex::new(Jitter::new(rate, config.sample_rate)));
        let source = waiting.clone();
        let failed = |e| eprintln!("audio output: {e}");
        // The device chooses the sample format; shared-mode outputs want
        // their own mix format rather than ours.
        let stream = match format {
            SampleFormat::F32 => device.build_output_stream(
                config,
                {
                    let source = source.clone();
                    move |data: &mut [f32], _| source.lock().unwrap().fill(data, channels, |v| v)
                },
                failed,
                None,
            ),
            SampleFormat::I16 => device.build_output_stream(
                config,
                {
                    let source = source.clone();
                    move |data: &mut [i16], _| source.lock().unwrap().fill(data, channels, |v| (v * 32767.0) as i16)
                },
                failed,
                None,
            ),
            SampleFormat::U16 => device.build_output_stream(
                config,
                {
                    let source = source.clone();
                    move |data: &mut [u16], _| {
                        source
                            .lock()
                            .unwrap()
                            .fill(data, channels, |v| ((v + 1.0) * 32767.5) as u16)
                    }
                },
                failed,
                None,
            ),
            other => return Err(format!("audio output uses an unsupported sample format ({other})")),
        }
        .map_err(|e| format!("audio output: {e}"))?;
        stream.play().map_err(|e| format!("audio output: {e}"))?;
        Ok(Self {
            waiting,
            _stream: stream,
        })
    }

    pub fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        self.waiting.lock().unwrap().push(pcm);
        Ok(())
    }
}
