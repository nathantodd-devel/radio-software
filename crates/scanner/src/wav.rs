//! Minimal 16-bit mono WAV writer.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::Path;

pub struct Writer {
    file: BufWriter<File>,
    samples: u32,
}

impl Writer {
    pub fn create(path: &Path, rate: u32) -> io::Result<Self> {
        let mut file = BufWriter::new(File::create(path)?);
        file.write_all(b"RIFF\0\0\0\0WAVEfmt ")?;
        file.write_all(&16u32.to_le_bytes())?;
        file.write_all(&1u16.to_le_bytes())?; // PCM
        file.write_all(&1u16.to_le_bytes())?; // mono
        file.write_all(&rate.to_le_bytes())?;
        file.write_all(&(rate * 2).to_le_bytes())?;
        file.write_all(&2u16.to_le_bytes())?;
        file.write_all(&16u16.to_le_bytes())?;
        file.write_all(b"data\0\0\0\0")?;
        Ok(Self { file, samples: 0 })
    }

    pub fn write(&mut self, pcm: &[i16]) {
        for s in pcm {
            self.file.write_all(&s.to_le_bytes()).ok();
        }
        self.samples += pcm.len() as u32;
    }

    /// Patch the header sizes and close the file.
    pub fn finish(mut self) {
        let bytes = self.samples * 2;
        let _ = (|| -> io::Result<()> {
            self.file.seek(SeekFrom::Start(4))?;
            self.file.write_all(&(36 + bytes).to_le_bytes())?;
            self.file.seek(SeekFrom::Start(40))?;
            self.file.write_all(&bytes.to_le_bytes())?;
            self.file.flush()
        })();
    }
}
