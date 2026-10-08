use std::io::{BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

/// Samples handed to `aplay` at a time: 20 ms at 16 kHz keeps latency low
/// without a system call per write.
const FLUSH_SAMPLES: usize = 320;

pub struct Output {
    player: Child,
    pipe: BufWriter<ChildStdin>,
    unflushed: usize,
}

impl Output {
    pub fn open(rate: u32) -> Result<Self, String> {
        let mut player = Command::new("aplay")
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
