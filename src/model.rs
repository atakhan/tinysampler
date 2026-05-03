use std::sync::Arc;

/// RGBA spectrogram for UI (time → X, frequency → Y, low freq at bottom row).
#[derive(Clone)]
pub struct Spectrogram {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

/// Mono samples in f32 [-1, 1] at **device** sample rate.
#[derive(Clone)]
pub struct Sample {
    pub data: Arc<Vec<f32>>,
    /// Precomputed for timeline (not read by audio thread).
    pub spectrogram: Option<Arc<Spectrogram>>,
}

impl Sample {
    pub fn new_mono(data: Arc<Vec<f32>>, spectrogram: Option<Arc<Spectrogram>>) -> Self {
        Self { data, spectrogram }
    }

    pub fn duration_secs(&self, sample_rate: u32) -> f32 {
        self.data.len() as f32 / sample_rate as f32
    }
}

#[derive(Clone)]
pub struct Clip {
    pub start_time_secs: f32,
    /// File name only (no path), for UI on the clip.
    pub label: String,
    pub sample: Sample,
}

#[derive(Clone)]
pub struct Transport {
    pub is_playing: bool,
    /// Incremented on Stop so the audio thread resets playhead to 0.
    pub stop_generation: u64,
}

impl Default for Transport {
    fn default() -> Self {
        Self {
            is_playing: false,
            stop_generation: 0,
        }
    }
}

#[derive(Clone)]
pub struct Project {
    pub clips: Vec<Clip>,
    pub transport: Transport,
    pub device_sample_rate: u32,
}

impl Project {
    pub fn empty(device_sample_rate: u32) -> Self {
        Self {
            clips: Vec::new(),
            transport: Transport::default(),
            device_sample_rate,
        }
    }
}
