//! Run the Phase 2 receiver over a recording: `p2decode CONTROL.cf32 VOICE.cf32 [OUT_PREFIX]`.
use radiocore::{Complex32, P25Channel, P25p2Channel, P25p2Frame};

fn load(path: &str) -> Vec<Complex32> {
    let bytes = std::fs::read(path).expect("read");
    bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|b| {
            Complex32::new(
                f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
                f32::from_le_bytes([b[4], b[5], b[6], b[7]]),
            )
        })
        .collect()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut cc = P25Channel::new();
    for block in load(&args[0]).chunks(48) {
        cc.process(block, |_| {});
        if cc.identity().is_some() {
            break;
        }
    }
    let (wacn, system, nac) = cc.identity().expect("no network status on the control channel");
    let mut channel = P25p2Channel::new();
    channel.set_identity(wacn, system, nac);
    let mut pcm: [Vec<i16>; 2] = [Vec::new(), Vec::new()];
    let (mut bursts, mut ends) = ([0usize; 2], [0usize; 2]);
    let started = std::time::Instant::now();
    let x = load(&args[1]);
    for block in x.chunks(48) {
        channel.process(block, |frame| match frame {
            P25p2Frame::Voice { slot, pcm: speech } => {
                bursts[slot as usize] += 1;
                pcm[slot as usize].extend(speech);
            }
            P25p2Frame::End { slot } => ends[slot as usize] += 1,
        });
    }
    let took = started.elapsed().as_secs_f64();
    println!(
        "{:.0} s of channel in {took:.2} s ({:.0}x real time)",
        x.len() as f64 / 48000.0,
        x.len() as f64 / 48000.0 / took
    );
    for slot in 0..2 {
        let v = &pcm[slot];
        let rms = (v.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt();
        println!(
            "slot {slot}: {:.1} s of speech in {} bursts, {} calls ended, rms {rms:.0}, peak {}",
            v.len() as f64 / 8000.0,
            bursts[slot],
            ends[slot],
            v.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0)
        );
        if let Some(prefix) = args.get(2) {
            let mut wav = Vec::new();
            let n = (v.len() * 2) as u32;
            wav.extend(b"RIFF");
            wav.extend((36 + n).to_le_bytes());
            wav.extend(b"WAVEfmt ");
            wav.extend(16u32.to_le_bytes());
            wav.extend(1u16.to_le_bytes());
            wav.extend(1u16.to_le_bytes());
            wav.extend(8000u32.to_le_bytes());
            wav.extend(16000u32.to_le_bytes());
            wav.extend(2u16.to_le_bytes());
            wav.extend(16u16.to_le_bytes());
            wav.extend(b"data");
            wav.extend(n.to_le_bytes());
            for s in v {
                wav.extend(s.to_le_bytes());
            }
            std::fs::write(format!("{prefix}-slot{slot}.wav"), wav).unwrap();
        }
    }
}
