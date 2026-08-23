//! Microphone capture for voice enrolment.
//!
//! Guided capture rather than file upload, for two reasons that happen to
//! agree: reading a known script gives us the reference transcript for free —
//! no ASR in the critical path — and recording in the app is what makes consent
//! verifiable, since we know the speaker was present.

use std::path::Path;
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Stream, SupportedStreamConfig};

/// The sentence the user is asked to read. Chosen for phonetic spread rather
/// than meaning: varied vowels, plosives, fricatives and nasals, long enough to
/// give the model 10-20 seconds of speech.
pub const ENROLMENT_SCRIPT: &str = "My name is spoken here, and this is how I sound when I speak \
naturally. The quick brown fox jumps over the lazy dog, while five wizards judge my calm voice. \
I am recording this so the app can learn my accent, my rhythm, and the way I shape my words.";

/// Bars the recording must clear before it becomes a voice profile.
const MIN_SECONDS: f32 = 6.0;
const MAX_SECONDS: f32 = 60.0;
const MIN_PEAK: f32 = 0.02;
const CLIPPING_PEAK: f32 = 0.99;
const MIN_SNR_DB: f32 = 15.0;

pub struct Recorder {
    stream: Option<Stream>,
    samples: Arc<Mutex<Vec<f32>>>,
    config: SupportedStreamConfig,
    channels: u16,
}

impl Recorder {
    pub fn new() -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no microphone found".to_string())?;
        let config = device
            .default_input_config()
            .map_err(|e| format!("cannot read microphone config: {e}"))?;
        let channels = config.channels();
        Ok(Self { stream: None, samples: Arc::new(Mutex::new(Vec::new())), config, channels })
    }

    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate()
    }

    pub fn start(&mut self) -> Result<(), String> {
        if self.stream.is_some() {
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| "no microphone found".to_string())?;

        self.samples.lock().map_err(|_| "recorder busy")?.clear();
        let sink = Arc::clone(&self.samples);
        let channels = self.channels as usize;

        // Mixed to mono on the way in: the model wants one channel, and doing
        // it here avoids a second pass over the buffer later.
        let stream = device
            .build_input_stream(
                self.config.config(),
                move |data: &[f32], _: &cpal::InputCallbackInfo| {
                    if let Ok(mut buffer) = sink.lock() {
                        if channels <= 1 {
                            buffer.extend_from_slice(data);
                        } else {
                            buffer.extend(
                                data.chunks(channels)
                                    .map(|frame| frame.iter().sum::<f32>() / channels as f32),
                            );
                        }
                    }
                },
                move |err| eprintln!("microphone error: {err}"),
                None,
            )
            .map_err(|e| format!("cannot open microphone: {e}"))?;

        stream.play().map_err(|e| format!("cannot start recording: {e}"))?;
        self.stream = Some(stream);
        Ok(())
    }

    pub fn stop(&mut self) {
        self.stream = None;
    }

    /// Seconds captured so far, for a live duration readout.
    pub fn elapsed_seconds(&self) -> f32 {
        self.samples
            .lock()
            .map(|s| s.len() as f32 / self.sample_rate() as f32)
            .unwrap_or(0.0)
    }

    pub fn write_wav(&self, path: &Path) -> Result<(), String> {
        let samples = self.samples.lock().map_err(|_| "recorder busy")?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate(),
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let mut writer =
            hound::WavWriter::create(path, spec).map_err(|e| format!("cannot write wav: {e}"))?;
        for sample in samples.iter() {
            let clamped = (sample.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            writer.write_sample(clamped).map_err(|e| e.to_string())?;
        }
        writer.finalize().map_err(|e| format!("cannot finish wav: {e}"))?;
        Ok(())
    }

    /// The last half-second as `bars` levels, oldest on the left. A meter that
    /// moves with the voice, rather than one block that swells: a dead
    /// microphone and a quiet room look different this way.
    pub fn meter(&self, bars: usize) -> Vec<f32> {
        let Ok(samples) = self.samples.lock() else { return vec![0.0; bars] };
        let chunk = (self.sample_rate() as usize / 40).max(1);
        let recent = &samples[samples.len().saturating_sub(chunk * bars)..];
        (0..bars)
            .map(|i| {
                let from = i * chunk;
                let to = ((i + 1) * chunk).min(recent.len());
                if from >= to {
                    return 0.0;
                }
                let window = &recent[from..to];
                let rms =
                    (window.iter().map(|s| s * s).sum::<f32>() / window.len() as f32).sqrt();
                (rms * 4.0).clamp(0.0, 1.0)
            })
            .collect()
    }

    /// The take's shape, as `bars` amplitudes. Peak rather than RMS per bucket:
    /// a waveform is read for where the speech is, and peaks are what the eye
    /// picks out of it.
    pub fn envelope(&self, bars: usize) -> Vec<f32> {
        let Ok(samples) = self.samples.lock() else { return Vec::new() };
        envelope(&samples, bars)
    }

    /// Reject a bad recording here, with advice, rather than letting the user
    /// discover it through a disappointing clone they blame on the app.
    pub fn check_quality(&self) -> Result<Quality, String> {
        let samples = self.samples.lock().map_err(|_| "recorder busy")?;
        assess(&samples, self.sample_rate())
    }
}

/// Read an audio file into mono samples at its own sample rate.
///
/// Used to bring in a reading of the script made somewhere else — on another
/// machine, or by the person whose voice it is — which is possible only because
/// the script is fixed, so the transcript is known without transcribing.
///
/// Deliberately not WAV-only. The file a person reaches for is a voice memo,
/// which is `.m4a`, and the platform file picker has no extension filter to
/// steer them away from it. Refusing that file is refusing the common case, so
/// the decoder covers what people actually have: wav, m4a, mp3, flac, ogg.
pub fn load_audio(path: &std::path::Path) -> Result<(Vec<f32>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::errors::Error;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| format!("cannot open that file: {e}"))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    // The extension is a hint only — a mislabelled file is still probed by
    // content, so a .wav that is really an m4a opens rather than misleads.
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, stream, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|_| unreadable(path))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or_else(|| "that file has no audio in it".to_string())?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| unreadable(path))?;

    let mut samples: Vec<f32> = Vec::new();
    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channels = 0usize;
    let mut buffer: Option<SampleBuffer<f32>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // Both of these are how a file ends, not how one fails.
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(format!("that file could not be decoded: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) => {
                let spec = *decoded.spec();
                sample_rate = spec.rate;
                channels = spec.channels.count();
                let buffer = buffer.get_or_insert_with(|| {
                    SampleBuffer::new(decoded.capacity() as u64, spec)
                });
                buffer.copy_interleaved_ref(decoded);
                samples.extend_from_slice(buffer.samples());
            }
            // A damaged packet mid-file loses that packet, not the recording.
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("that file could not be decoded: {e}")),
        }
    }

    if samples.is_empty() || sample_rate == 0 {
        return Err(unreadable(path));
    }

    // Downmix rather than refuse: a stereo reading is still a reading.
    let samples = if channels <= 1 {
        samples
    } else {
        samples.chunks(channels).map(|f| f.iter().sum::<f32>() / channels as f32).collect()
    };
    Ok((samples, sample_rate))
}

/// What to say when a file will not open. Names the format the person chose,
/// because "unsupported" leaves them guessing which part was wrong.
fn unreadable(path: &std::path::Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!(
            "That .{} file could not be read. Try a wav, m4a, mp3, flac or ogg recording.",
            ext.to_lowercase()
        ),
        None => "That file could not be read. Try a wav, m4a, mp3, flac or ogg recording.".into(),
    }
}

/// The same bar for a recording and an imported file. Shared deliberately: a
/// voice brought in from elsewhere is not exempt from the checks that decide
/// whether a clone will sound like anyone.
pub fn assess(samples: &[f32], sample_rate: u32) -> Result<Quality, String> {
    {
        if samples.is_empty() {
            return Err("Nothing was recorded. Check your microphone and try again.".into());
        }

        let seconds = samples.len() as f32 / sample_rate as f32;
        let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));

        if seconds < MIN_SECONDS {
            return Err(format!(
                "Only {seconds:.0} seconds recorded. Read the whole script — about {MIN_SECONDS:.0} seconds minimum."
            ));
        }
        if seconds > MAX_SECONDS {
            return Err(format!("That is {seconds:.0} seconds. Keep it under {MAX_SECONDS:.0}."));
        }
        if peak < MIN_PEAK {
            return Err("That was too quiet. Move closer to the microphone and try again.".into());
        }
        if peak >= CLIPPING_PEAK {
            return Err("The recording is clipping. Move back from the microphone and try again.".into());
        }

        // Speech versus noise floor, from loud and quiet frames rather than an
        // absolute threshold, so it holds across different microphones.
        let window = (sample_rate as usize / 50).max(1);
        let mut frames: Vec<f32> = samples
            .chunks(window)
            .map(|c| (c.iter().map(|s| s * s).sum::<f32>() / c.len() as f32).sqrt())
            .filter(|r| *r > 0.0)
            .collect();
        if frames.len() < 10 {
            return Err("The recording is too short to check. Try again.".into());
        }
        frames.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let floor = frames[frames.len() / 10];
        let speech = frames[frames.len() * 9 / 10];
        let snr_db = if floor > 0.0 { 20.0 * (speech / floor).log10() } else { 0.0 };

        if snr_db < MIN_SNR_DB {
            return Err(format!(
                "Too much background noise (signal-to-noise {snr_db:.0} dB). Find a quieter room and try again."
            ));
        }

        Ok(Quality { seconds, snr_db })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Quality {
    pub seconds: f32,
    pub snr_db: f32,
}

impl Quality {
    pub fn summary(&self) -> String {
        format!("{:.0}s recorded · {:.0} dB signal-to-noise", self.seconds, self.snr_db)
    }
}

/// Reduce a waveform to `bars` peak amplitudes, normalised so the loudest bar
/// fills the row. Shared by the recorder and by files brought in from disk.
pub fn envelope(samples: &[f32], bars: usize) -> Vec<f32> {
    if samples.is_empty() || bars == 0 {
        return vec![0.0; bars];
    }
    let width = samples.len() as f32 / bars as f32;
    let peaks: Vec<f32> = (0..bars)
        .map(|i| {
            let from = (i as f32 * width) as usize;
            let to = (((i + 1) as f32 * width) as usize).min(samples.len()).max(from + 1);
            samples[from..to.min(samples.len())]
                .iter()
                .fold(0.0f32, |peak, s| peak.max(s.abs()))
        })
        .collect();
    let loudest = peaks.iter().cloned().fold(0.0f32, f32::max);
    if loudest <= 0.0 {
        return peaks;
    }
    peaks.iter().map(|p| p / loudest).collect()
}


#[cfg(test)]
mod tests {
    use super::*;

    /// A tone at a given amplitude, which is enough to exercise every check:
    /// the bars are about length, peak and the gap between speech and silence.
    fn tone(seconds: f32, amplitude: f32, rate: u32) -> Vec<f32> {
        let n = (seconds * rate as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                (t * 220.0 * std::f32::consts::TAU).sin() * amplitude
            })
            .collect()
    }

    /// Speech with a noise floor under it, so the signal-to-noise figure has
    /// something to measure. Alternating loud and quiet halves of a second.
    fn speech_over_noise(seconds: f32, speech: f32, noise: f32, rate: u32) -> Vec<f32> {
        tone(seconds, 1.0, rate)
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let loud = (i / (rate as usize / 2)).is_multiple_of(2);
                s * if loud { speech } else { noise }
            })
            .collect()
    }

    #[test]
    fn nothing_recorded_is_rejected_by_name() {
        let err = assess(&[], 24_000).unwrap_err();
        assert!(err.contains("microphone"), "{err}");
    }

    #[test]
    fn a_take_shorter_than_the_script_is_rejected() {
        let err = assess(&tone(2.0, 0.4, 24_000), 24_000).unwrap_err();
        assert!(err.contains("seconds recorded"), "{err}");
    }

    #[test]
    fn a_take_longer_than_the_bar_is_rejected() {
        let err = assess(&tone(MAX_SECONDS + 5.0, 0.4, 24_000), 24_000).unwrap_err();
        assert!(err.contains("Keep it under"), "{err}");
    }

    #[test]
    fn a_silent_take_is_rejected_as_too_quiet() {
        let err = assess(&tone(20.0, 0.001, 24_000), 24_000).unwrap_err();
        assert!(err.contains("too quiet"), "{err}");
    }

    #[test]
    fn a_clipping_take_is_rejected() {
        let mut samples = tone(20.0, 0.5, 24_000);
        samples[100] = 1.0;
        let err = assess(&samples, 24_000).unwrap_err();
        assert!(err.contains("clipping"), "{err}");
    }

    #[test]
    fn a_good_take_passes_and_reports_what_it_measured() {
        let quality = speech_over_noise(20.0, 0.5, 0.002, 24_000);
        let quality = assess(&quality, 24_000).expect("a clean take passes");
        assert!((quality.seconds - 20.0).abs() < 0.1, "{}", quality.seconds);
        assert!(quality.snr_db > MIN_SNR_DB, "snr {} dB", quality.snr_db);
    }

    #[test]
    fn a_noisy_room_scores_worse_than_a_quiet_one() {
        let rate = 24_000;
        let quiet = assess(&speech_over_noise(20.0, 0.5, 0.002, rate), rate).unwrap();
        let noisy = assess(&speech_over_noise(20.0, 0.5, 0.05, rate), rate).unwrap();
        assert!(
            noisy.snr_db < quiet.snr_db,
            "noisy {} dB should score under quiet {} dB",
            noisy.snr_db,
            quiet.snr_db
        );
    }

    #[test]
    fn the_bars_do_not_depend_on_the_sample_rate() {
        // The same twenty seconds at two rates is the same take, and a check
        // written in samples rather than seconds would disagree.
        let low = assess(&speech_over_noise(20.0, 0.5, 0.002, 16_000), 16_000).unwrap();
        let high = assess(&speech_over_noise(20.0, 0.5, 0.002, 48_000), 48_000).unwrap();
        assert!((low.seconds - high.seconds).abs() < 0.1);
    }

    #[test]
    fn an_envelope_has_one_value_per_bar_and_peaks_at_one() {
        let levels = envelope(&tone(3.0, 0.7, 24_000), 34);
        assert_eq!(levels.len(), 34);
        assert!(levels.iter().all(|l| (0.0..=1.0).contains(l)), "{levels:?}");
        let loudest = levels.iter().cloned().fold(0.0f32, f32::max);
        assert!((loudest - 1.0).abs() < 1e-3, "the loudest bar should fill the row: {loudest}");
    }

    #[test]
    fn an_envelope_follows_the_shape_of_the_take() {
        // Loud first half, quiet second: the drawing should say so.
        let rate = 24_000;
        let mut samples = tone(4.0, 0.9, rate);
        let half = samples.len() / 2;
        for s in samples[half..].iter_mut() {
            *s *= 0.05;
        }
        let levels = envelope(&samples, 20);
        let (front, back) = levels.split_at(10);
        let mean = |xs: &[f32]| xs.iter().sum::<f32>() / xs.len() as f32;
        assert!(mean(front) > mean(back) * 5.0, "{levels:?}");
    }

    #[test]
    fn an_empty_take_still_draws_the_bars_it_was_asked_for() {
        // The player draws before anything is loaded, so this must not panic
        // or return a short row.
        assert_eq!(envelope(&[], 34).len(), 34);
        assert!(envelope(&[0.0; 1000], 34).iter().all(|l| *l == 0.0));
    }

    /// The same second of speech, encoded four ways. Real files rather than
    /// synthesised bytes: the decoder's job is to open what a person actually
    /// has, and an m4a is what comes off a phone.
    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn a_voice_memo_opens() {
        // The case that used to fail: the picker has no extension filter, so
        // this is the file people were choosing and being refused.
        let (samples, rate) = load_audio(&fixture("voice-memo.m4a")).expect("m4a should decode");
        assert_eq!(rate, 16_000);
        assert!(!samples.is_empty(), "decoded to silence");
        assert!(samples.iter().any(|s| s.abs() > 0.01), "decoded to nothing audible");
    }

    #[test]
    fn every_format_gives_back_the_same_recording() {
        // One source encoded four ways, so a decoder that opens a file but
        // mangles it — wrong channel count, wrong scaling — is still a failure.
        let reference = load_audio(&fixture("tone.wav")).expect("wav should decode");
        for name in ["voice-memo.m4a", "voice-memo.mp3", "voice-memo.flac"] {
            let (samples, rate) = load_audio(&fixture(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(rate, reference.1, "{name} changed the sample rate");

            let seconds = samples.len() as f32 / rate as f32;
            let expected = reference.0.len() as f32 / reference.1 as f32;
            // Lossy formats pad with encoder delay; a tenth of a second of
            // slack catches a dropped channel without failing on that.
            assert!(
                (seconds - expected).abs() < 0.12,
                "{name} is {seconds:.2}s against {expected:.2}s"
            );

            let peak = samples.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            let want = reference.0.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!((peak - want).abs() < 0.25, "{name} peaks at {peak:.2}, wanted {want:.2}");
        }
    }

    #[test]
    fn a_file_that_is_not_audio_says_so_in_words() {
        let dir = std::env::temp_dir().join("yarngo-load-audio");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, b"this is not a recording").unwrap();

        let err = load_audio(&path).expect_err("a text file is not audio");
        // The old failure was a hound RIFF parse error shown verbatim. What a
        // person needs is the format they picked and the ones that work.
        assert!(err.contains(".txt"), "should name what was picked: {err}");
        assert!(err.contains("m4a"), "should name what would work: {err}");
    }
}
