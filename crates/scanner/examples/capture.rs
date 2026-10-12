//! Record channels off the air for working on decoders: each frequency's
//! 48 kHz baseband, as the scanner's own receivers would be handed it.
//!
//!     cargo run --release -p scanner --example capture -- SECONDS OUT_DIR MHZ...
//!
//! Writes OUT_DIR/<MHz>.cf32: interleaved little-endian f32 I and Q.

use std::fs::File;
use std::io::{BufWriter, Write};

use radiocore::{Channelizer, Complex32};
use scanner::plan::BIN_HZ;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [seconds, dir, freqs @ ..] = args.as_slice() else {
        return Err("usage: capture SECONDS OUT_DIR MHZ...".into());
    };
    let seconds: f64 = seconds.parse().map_err(|_| "bad number of seconds")?;
    let freqs: Vec<f64> = freqs
        .iter()
        .map(|f| f.parse::<f64>().map(|f| f * 1e6))
        .collect::<Result<_, _>>()
        .map_err(|_| "bad frequency")?;
    let (lo, hi) = freqs
        .iter()
        .fold((f64::MAX, f64::MIN), |(lo, hi), &f| (lo.min(f), hi.max(f)));
    // On the half-bin grid, off any channel's centre.
    let center = ((lo + hi) / 2.0 / BIN_HZ).round() * BIN_HZ + BIN_HZ / 2.0;

    let driver = scanner::engine::drivers().into_iter().next().ok_or("no drivers")?;
    let mut radio = driver.open().map_err(|e| e.to_string())?;
    let rate = radio
        .sample_rates()
        .map_err(|e| e.to_string())?
        .into_iter()
        .max()
        .ok_or("no sample rates")?;
    radio
        .configure(&receiver::Settings {
            sample_rate: rate,
            gain: 17,
            ppm: 0,
            bias_tee: false,
        })
        .map_err(|e| e.to_string())?;
    let blocks = radio.start().map_err(|e| e.to_string())?;
    radio.set_frequency(center as u32).map_err(|e| e.to_string())?;

    let offsets: Vec<f64> = freqs.iter().map(|f| f - center).collect();
    let mut channelizer = Channelizer::new(rate as f64, BIN_HZ, 96, 9_000.0, &offsets);
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut files: Vec<BufWriter<File>> = freqs
        .iter()
        .map(|f| File::create(format!("{dir}/{:.5}.cf32", f / 1e6)).map(BufWriter::new))
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;

    let hop = channelizer.hop();
    let (mut queue, mut input) = (Vec::<i16>::new(), vec![Complex32::ZERO; hop]);
    // The first third of a second is the tuner settling.
    let (mut skip, mut left) = ((rate as f64 * 0.3) as usize * 2, (rate as f64 * seconds) as usize);
    while left > 0 {
        let block = blocks.recv().map_err(|_| "the radio stopped")?;
        let iq = &block.iq[skip.min(block.iq.len())..];
        skip = skip.saturating_sub(block.iq.len());
        queue.extend_from_slice(iq);
        let mut used = 0;
        while queue.len() - used >= hop * 2 && left > 0 {
            for (s, pair) in input.iter_mut().zip(queue[used..].as_chunks::<2>().0) {
                *s = Complex32::new(f32::from(pair[0]) / 32768.0, f32::from(pair[1]) / 32768.0);
            }
            used += hop * 2;
            left = left.saturating_sub(hop);
            channelizer.process(&input, |i, samples| {
                for s in samples {
                    files[i].write_all(&s.re.to_le_bytes()).ok();
                    files[i].write_all(&s.im.to_le_bytes()).ok();
                }
            });
        }
        queue.drain(..used);
    }
    files.iter_mut().try_for_each(|f| f.flush()).map_err(|e| e.to_string())
}
