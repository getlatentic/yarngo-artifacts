//! Looking at what the engine produced before calling it a take.
//!
//! The engine reports what it made; this reads the file and says whether the
//! file agrees. A truncated write, a run that died between the header and the
//! samples, or a path that ended up holding something else all look like
//! success from the other side of the protocol.

use std::path::Path;

/// Why a staged file is not audio the person can have.
#[derive(Clone, Debug, PartialEq)]
pub enum Unusable {
    Missing,
    NotWave(String),
    Empty,
    /// The file holds materially less audio than the engine said it wrote.
    Truncated { reported_s: f64, actual_s: f64 },
}

impl std::fmt::Display for Unusable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no file was written"),
            Self::NotWave(why) => write!(f, "not a wave file: {why}"),
            Self::Empty => write!(f, "the file holds no audio"),
            Self::Truncated { reported_s, actual_s } => write!(
                f,
                "the engine reported {reported_s:.2}s and the file holds {actual_s:.2}s"
            ),
        }
    }
}

/// What the file itself says it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wave {
    pub seconds: f64,
    pub sample_rate: u32,
    pub channels: u16,
}

/// A generation can be a little longer or shorter than reported — the report is
/// rounded — but not half of it. This catches a write that stopped early
/// without failing an honest rounding difference.
const SHORTFALL: f64 = 0.5;

pub fn inspect(path: &Path, reported_s: Option<f64>) -> Result<Wave, Unusable> {
    let bytes = std::fs::read(path).map_err(|_| Unusable::Missing)?;
    let wave = parse(&bytes).map_err(Unusable::NotWave)?;
    if wave.seconds <= 0.0 {
        return Err(Unusable::Empty);
    }
    if let Some(reported_s) = reported_s {
        if reported_s > 0.0 && wave.seconds < reported_s * SHORTFALL {
            return Err(Unusable::Truncated {
                reported_s,
                actual_s: wave.seconds,
            });
        }
    }
    Ok(wave)
}

fn parse(bytes: &[u8]) -> Result<Wave, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("no RIFF/WAVE header".into());
    }
    let mut at = 12;
    let mut format: Option<(u16, u32, u16)> = None;
    let mut data_bytes: Option<u32> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap());
        let body = at + 8;
        match id {
            b"fmt " if body + 16 <= bytes.len() => {
                format = Some((
                    u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap()),
                    u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap()),
                    u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap()),
                ));
            }
            b"data" => {
                // What is actually there, not what the header claims: a write
                // that stopped early leaves the declared size behind.
                data_bytes = Some(size.min((bytes.len() - body) as u32));
            }
            _ => {}
        }
        // Chunks are word-aligned, and an odd size is followed by a pad byte.
        at = body + size as usize + (size as usize & 1);
    }
    let (channels, sample_rate, bits) = format.ok_or("no fmt chunk")?;
    let data_bytes = data_bytes.ok_or("no data chunk")?;
    let frame = (channels as u32) * (bits as u32 / 8);
    if frame == 0 || sample_rate == 0 {
        return Err("the format chunk describes no audio".into());
    }
    Ok(Wave {
        seconds: data_bytes as f64 / (frame * sample_rate) as f64,
        sample_rate,
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::{inspect, Unusable};

    /// A wave file holding `samples` frames of 16-bit mono at 24 kHz.
    fn wave(samples: usize) -> Vec<u8> {
        let data = samples * 2;
        let mut out = Vec::new();
        out.extend(b"RIFF");
        out.extend(((36 + data) as u32).to_le_bytes());
        out.extend(b"WAVEfmt ");
        out.extend(16u32.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(1u16.to_le_bytes());
        out.extend(24_000u32.to_le_bytes());
        out.extend(48_000u32.to_le_bytes());
        out.extend(2u16.to_le_bytes());
        out.extend(16u16.to_le_bytes());
        out.extend(b"data");
        out.extend((data as u32).to_le_bytes());
        out.extend(std::iter::repeat_n(0u8, data));
        out
    }

    fn written(dir: &std::path::Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, bytes).expect("write");
        path
    }

    #[test]
    fn a_whole_file_reads_as_what_it_is() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = written(dir.path(), "ok.wav", &wave(24_000));
        let wave = inspect(&path, Some(1.0)).expect("usable");
        assert_eq!(wave.sample_rate, 24_000);
        assert_eq!(wave.channels, 1);
        assert!((wave.seconds - 1.0).abs() < 0.01);
    }

    #[test]
    fn a_file_that_was_never_written_is_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        assert_eq!(inspect(&dir.path().join("nothing.wav"), None), Err(Unusable::Missing));
    }

    #[test]
    fn something_that_is_not_audio_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = written(dir.path(), "no.wav", b"this is not a wave file at all");
        assert!(matches!(inspect(&path, None), Err(Unusable::NotWave(_))));
    }

    #[test]
    fn a_header_with_no_samples_holds_no_audio() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = written(dir.path(), "empty.wav", &wave(0));
        assert_eq!(inspect(&path, None), Err(Unusable::Empty));
    }

    /// The case the report alone cannot catch: the engine finished and said so,
    /// and the bytes stopped early.
    #[test]
    fn a_write_that_stopped_early_disagrees_with_the_report() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut bytes = wave(24_000);
        bytes.truncate(bytes.len() / 4);
        let path = written(dir.path(), "cut.wav", &bytes);
        assert!(matches!(
            inspect(&path, Some(1.0)),
            Err(Unusable::Truncated { .. })
        ));
    }

    /// And rounding in the report is not a truncation.
    #[test]
    fn a_rounded_report_is_not_a_shortfall() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = written(dir.path(), "round.wav", &wave(23_998));
        assert!(inspect(&path, Some(1.0)).is_ok());
    }
}
