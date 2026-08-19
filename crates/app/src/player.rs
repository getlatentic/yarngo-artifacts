//! Audio playback for generated clips.
//!
//! The output device is opened once and held for the life of the app: opening
//! it per clip adds audible latency and can fail transiently while another
//! process is grabbing the device.
//!
//! Position comes from rodio's own `get_pos` rather than a wall-clock timer, so
//! the progress bar tracks the audio actually played rather than time elapsed —
//! they diverge whenever the stream stalls or the user pauses.

use std::cell::Cell;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::Duration;

use rodio::{DeviceSinkBuilder, Decoder, MixerDeviceSink, Player};

pub struct AudioPlayer {
    // Held to keep the device open; dropping it silences playback.
    _device: MixerDeviceSink,
    player: Player,
    duration: Duration,
    /// Whether this clip has been asked to play yet. rodio only refreshes its
    /// reported position while a source is being polled, so before the first
    /// play the figure it holds still belongs to the clip before this one —
    /// which showed a fresh clip as already finished.
    started: Cell<bool>,
}

impl AudioPlayer {
    pub fn new() -> Result<Self, String> {
        let device = DeviceSinkBuilder::open_default_sink()
            .map_err(|e| format!("no audio output device: {e}"))?;
        let player = Player::connect_new(device.mixer());
        player.pause();
        Ok(Self {
            _device: device,
            player,
            duration: Duration::ZERO,
            started: Cell::new(false),
        })
    }

    /// Load a clip, replacing whatever was queued. `duration` comes from the
    /// synthesis result, which already knows it exactly.
    pub fn load(&mut self, path: &Path, duration: Duration) -> Result<(), String> {
        self.player.clear();
        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let decoder = Decoder::try_from(BufReader::new(file))
            .map_err(|e| format!("cannot decode {}: {e}", path.display()))?;
        self.player.append(decoder);
        self.player.pause();
        self.duration = duration;
        self.started.set(false);
        Ok(())
    }

    pub fn play(&self) {
        self.started.set(true);
        self.player.play();
    }

    pub fn pause(&self) {
        self.player.pause();
    }

    pub fn toggle(&self) {
        if self.is_playing() {
            self.pause();
        } else {
            self.play();
        }
    }

    /// Playing means running AND not finished — an exhausted queue reports
    /// itself as unpaused, which would otherwise show as permanently playing.
    pub fn is_playing(&self) -> bool {
        !self.player.is_paused() && !self.player.empty()
    }

    pub fn finished(&self) -> bool {
        self.player.empty()
    }

    pub fn position(&self) -> Duration {
        if !self.started.get() {
            return Duration::ZERO;
        }
        self.player.get_pos().min(self.duration)
    }

    /// Jump to a fraction of the clip. Decoders can refuse — a stream without
    /// seek support returns an error rather than silently doing nothing, so the
    /// caller can say so instead of leaving the bar stuck.
    pub fn seek_to(&self, fraction: f32) -> Result<(), String> {
        let target = self.duration.mul_f32(fraction.clamp(0.0, 1.0));
        self.player.try_seek(target).map_err(|e| format!("cannot seek this clip: {e}"))?;
        self.started.set(true);
        Ok(())
    }

    /// 0.0 to 1.0 for the progress bar.
    pub fn progress(&self) -> f32 {
        if self.duration.is_zero() {
            return 0.0;
        }
        (self.position().as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0)
    }

}

pub fn format_time(d: Duration) -> String {
    let total = d.as_secs();
    format!("{}:{:02}", total / 60, total % 60)
}
