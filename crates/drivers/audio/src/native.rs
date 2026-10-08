use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, Stream, StreamConfig};

type Queue = Arc<Mutex<VecDeque<i16>>>;

pub struct Output {
    queue: Queue,
    /// Most samples allowed to wait: a second's worth, so a stalled device
    /// can't build up an ever-growing delay.
    max_queued: usize,
    _stream: Stream,
}

/// Feeds the device from the queue, converting to its sample rate.
struct Resampler {
    queue: Queue,
    /// Input samples per output sample.
    step: f32,
    /// Position between `prev` and `next`, 0 to 1.
    pos: f32,
    prev: f32,
    next: f32,
}

impl Resampler {
    /// Fill one device buffer of interleaved frames, every channel alike.
    fn fill<T: Copy>(&mut self, data: &mut [T], channels: usize, convert: impl Fn(f32) -> T) {
        let mut queue = self.queue.lock().unwrap();
        for frame in data.chunks_mut(channels) {
            self.pos += self.step;
            while self.pos >= 1.0 {
                self.pos -= 1.0;
                self.prev = self.next;
                self.next = queue.pop_front().map_or(0.0, |s| s as f32 / 32768.0);
            }
            frame.fill(convert(self.prev + (self.next - self.prev) * self.pos));
        }
    }
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

        let queue = Queue::default();
        let mut source = Resampler {
            queue: queue.clone(),
            step: rate as f32 / config.sample_rate as f32,
            pos: 0.0,
            prev: 0.0,
            next: 0.0,
        };
        let failed = |e| eprintln!("audio output: {e}");
        // The device chooses the sample format; shared-mode outputs want
        // their own mix format rather than ours.
        let stream = match format {
            SampleFormat::F32 => device.build_output_stream(
                config,
                move |data: &mut [f32], _| source.fill(data, channels, |v| v),
                failed,
                None,
            ),
            SampleFormat::I16 => device.build_output_stream(
                config,
                move |data: &mut [i16], _| source.fill(data, channels, |v| (v * 32767.0) as i16),
                failed,
                None,
            ),
            SampleFormat::U16 => device.build_output_stream(
                config,
                move |data: &mut [u16], _| source.fill(data, channels, |v| ((v + 1.0) * 32767.5) as u16),
                failed,
                None,
            ),
            other => return Err(format!("audio output uses an unsupported sample format ({other})")),
        }
        .map_err(|e| format!("audio output: {e}"))?;
        stream.play().map_err(|e| format!("audio output: {e}"))?;
        Ok(Self {
            queue,
            max_queued: rate as usize,
            _stream: stream,
        })
    }

    pub fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        let mut queue = self.queue.lock().unwrap();
        queue.extend(pcm);
        let excess = queue.len().saturating_sub(self.max_queued);
        queue.drain(..excess);
        Ok(())
    }
}
