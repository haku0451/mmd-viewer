//! Motion audio.
//!
//! Wav, mp3, ogg, and m4a are decoded when the file is chosen. Playback starts
//! at the current motion frame and pauses with the timeline.

use std::path::Path;

use kira::sound::static_sound::{StaticSoundData, StaticSoundHandle};
use kira::{AudioManager, AudioManagerSettings, DefaultBackend, Tween};

/// One decoded clip and the output stream that plays it.
pub struct StageAudio {
    manager: Option<AudioManager>,
    data: Option<StaticSoundData>,
    handle: Option<StaticSoundHandle>,
    label: String,
    paused: bool,
}

impl StageAudio {
    /// No device is opened until a file is loaded.
    pub fn new() -> Self {
        Self {
            manager: None,
            data: None,
            handle: None,
            label: String::new(),
            paused: false,
        }
    }

    /// File name shown in the panel. Empty when nothing is loaded.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Decode a clip off the UI thread.
    pub fn decode(path: &Path) -> Result<(StaticSoundData, String), String> {
        let data = StaticSoundData::from_file(path)
            .map_err(|error| format!("音声を読めませんでした: {error}"))?;
        let label = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("音声")
            .to_string();
        Ok((data, label))
    }

    /// Keep a clip decoded by [`decode`](Self::decode). Opens the output device.
    pub fn store(&mut self, data: StaticSoundData, label: String) -> Result<(), String> {
        self.stop();
        if self.manager.is_none() {
            let manager = AudioManager::<DefaultBackend>::new(AudioManagerSettings::default())
                .map_err(|error| format!("音声デバイスを開けませんでした: {error}"))?;
            self.manager = Some(manager);
        }
        self.data = Some(data);
        self.label = label;
        Ok(())
    }

    /// Start at `seconds`, replacing a clip that is already playing.
    pub fn play_from(&mut self, seconds: f64) {
        self.stop();
        let Some(manager) = self.manager.as_mut() else {
            return;
        };
        let Some(data) = self.data.clone() else {
            return;
        };
        let seconds = seconds.max(0.0);
        match manager.play(data.start_position(seconds)) {
            Ok(handle) => {
                self.handle = Some(handle);
                self.paused = false;
            }
            Err(error) => tracing::warn!(error = %error, "audio playback failed"),
        }
    }

    pub fn pause(&mut self) {
        if let Some(handle) = self.handle.as_mut() {
            handle.pause(Tween::default());
            self.paused = true;
        }
    }

    pub fn resume(&mut self) {
        if self.paused
            && let Some(handle) = self.handle.as_mut()
        {
            handle.resume(Tween::default());
            self.paused = false;
        }
    }

    /// A paused clip can continue without seeking back to the start.
    pub fn is_paused(&self) -> bool {
        self.paused
    }

    pub fn stop(&mut self) {
        if let Some(mut handle) = self.handle.take() {
            handle.stop(Tween::default());
        }
        self.paused = false;
    }
}

impl Default for StageAudio {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use kira::sound::static_sound::StaticSoundData;

    #[test]
    fn a_pcm_wav_decodes() {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&36u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8_000u32.to_le_bytes());
        bytes.extend_from_slice(&16_000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
        let size = u32::try_from(bytes.len() - 8).expect("wav size");
        bytes[4..8].copy_from_slice(&size.to_le_bytes());
        let data = StaticSoundData::from_cursor(Cursor::new(bytes)).expect("pcm wav");
        assert_eq!(data.sample_rate, 8_000);
        assert!(!data.frames.is_empty());
    }
}
