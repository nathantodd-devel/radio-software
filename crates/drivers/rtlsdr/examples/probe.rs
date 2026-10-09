//! Open the RTL-SDR, describe it, and receive for a second.

use std::time::{Duration, Instant};

fn main() -> Result<(), rtlsdr::Error> {
    println!("{} RTL-SDR device(s) found", rtlsdr::device_count()?);
    let mut device = rtlsdr::RtlSdr::open()?;
    println!("{}", device.name());
    println!("gains (tenths of a dB): {:?}", device.gains()?);
    let rate = device.sample_rates()[0];
    device.set_sample_rate(rate)?;
    device.set_gain_level(17, 21)?;
    device.set_frequency(100_000_000)?;
    let blocks = device.start()?;

    let (start, mut samples, mut dropped, mut peak) = (Instant::now(), 0u64, 0u64, 0i16);
    while start.elapsed() < Duration::from_secs(1) {
        let Ok(block) = blocks.recv() else { break };
        samples += block.iq.len() as u64 / 2;
        dropped += block.dropped;
        peak = block.iq.iter().fold(peak, |m, s| m.max(s.saturating_abs()));
    }
    device.stop();
    println!(
        "{samples} samples in {:.2} s ({dropped} dropped), peak {peak}",
        start.elapsed().as_secs_f32()
    );
    Ok(())
}
