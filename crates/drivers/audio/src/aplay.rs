use std::io::{BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use crate::Device;

/// Samples handed to `aplay` at a time: 20 ms at 16 kHz keeps latency low
/// without a system call per write.
const FLUSH_SAMPLES: usize = 320;

/// The sound server's outputs. `pactl` speaks to PipeWire as well as to
/// PulseAudio. ALSA's own devices aren't offered: a sound server holds the
/// cards they belong to, so they wouldn't open.
pub fn devices() -> Vec<Device> {
    Command::new("pactl")
        .args(["list", "sinks"])
        .env("LC_ALL", "C") // The labels are translated otherwise.
        .output()
        .map_or(Vec::new(), |out| parse_sinks(&String::from_utf8_lossy(&out.stdout)))
}

/// The devices in the output of `pactl list sinks`.
fn parse_sinks(listing: &str) -> Vec<Device> {
    let mut sinks = Vec::new();
    let mut id = None;
    for line in listing.lines() {
        if let Some(name) = line.strip_prefix("\tName: ") {
            id = Some(name.to_string());
        } else if let (Some(name), true) = (line.strip_prefix("\tDescription: "), id.is_some()) {
            sinks.push(Device {
                id: id.take().unwrap(),
                name: name.to_string(),
            });
        }
    }
    sinks
}

pub struct Output {
    player: Child,
    pipe: BufWriter<ChildStdin>,
    unflushed: usize,
}

impl Output {
    pub fn open(rate: u32, device: Option<&str>) -> Result<Self, String> {
        let mut command = Command::new("aplay");
        if let Some(sink) = device {
            // Each sound server reads its own and ignores the other's.
            command.env("PIPEWIRE_NODE", sink).env("PULSE_SINK", sink);
        }
        let mut player = command
            .args([
                "-q",
                "-t",
                "raw",
                "-f",
                "S16_LE",
                "-c",
                "1",
                "-r",
                &rate.to_string(),
                "-",
            ])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| format!("can't run aplay ({e}); install alsa-utils"))?;
        let pipe = BufWriter::new(player.stdin.take().unwrap());
        Ok(Self {
            player,
            pipe,
            unflushed: 0,
        })
    }

    pub fn write(&mut self, pcm: &[i16]) -> Result<(), String> {
        let stopped = |e| format!("audio player stopped: {e}");
        for s in pcm {
            self.pipe.write_all(&s.to_le_bytes()).map_err(stopped)?;
        }
        self.unflushed += pcm.len();
        if self.unflushed >= FLUSH_SAMPLES {
            self.unflushed = 0;
            self.pipe.flush().map_err(stopped)?;
        }
        Ok(())
    }
}

impl Drop for Output {
    fn drop(&mut self) {
        self.player.kill().ok();
        self.player.wait().ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_each_sink_from_a_listing() {
        let listing = "Sink #50\n\
            \tState: RUNNING\n\
            \tName: alsa_output.hdmi-stereo\n\
            \tDescription: Radeon Audio (HDMI)\n\
            \tDriver: PipeWire\n\
            \tProperties:\n\
            \t\tdevice.description = \"Radeon Audio\"\n\
            \tPorts:\n\
            \t\thdmi-output-0: HDMI / DisplayPort (type: HDMI, priority: 5900)\n\
            \n\
            Sink #51\n\
            \tName: bluez_output.00_11.1\n\
            \tDescription: Headphones\n";
        let device = |id: &str, name: &str| Device {
            id: id.into(),
            name: name.into(),
        };
        assert_eq!(
            parse_sinks(listing),
            [
                device("alsa_output.hdmi-stereo", "Radeon Audio (HDMI)"),
                device("bluez_output.00_11.1", "Headphones"),
            ]
        );
    }

    #[test]
    fn an_empty_listing_has_no_sinks() {
        assert!(parse_sinks("").is_empty());
    }
}
