//! Open the Airspy, list its sample rates, and receive for a second.

use std::time::{Duration, Instant};

fn main() -> Result<(), airspy::Error> {
    let mut device = airspy::Airspy::open()?;
    let rates = device.sample_rates()?;
    println!("sample rates: {rates:?}");
    let rate = rates.iter().copied().max().unwrap_or(0);
    device.set_sample_rate(rate)?;
    device.set_linearity_gain(17)?;
    let blocks = device.start()?;
    device.set_frequency(100_000_000)?;

    let (start, mut samples, mut dropped, mut peak) = (Instant::now(), 0u64, 0u64, 0i16);
    while start.elapsed() < Duration::from_secs(1) {
        let Ok(block) = blocks.recv() else { break };
        samples += block.iq.len() as u64 / 2;
        dropped += block.dropped;
        peak = block.iq.iter().fold(peak, |m, s| m.max(s.saturating_abs()));
    }
    device.stop()?;
    println!(
        "{samples} samples in {:.2} s ({dropped} dropped), peak {peak}",
        start.elapsed().as_secs_f32()
    );
    Ok(())
}
