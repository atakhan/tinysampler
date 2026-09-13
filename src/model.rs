use std::sync::Arc;

/// Stable handle for a clip on the timeline (survives better than raw indices).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClipId(pub u64);

/// Mono samples in f32 [-1, 1] at **device** sample rate.
#[derive(Clone)]
pub struct Sample {
    pub data: Arc<Vec<f32>>,
    /// Min/max bins for waveform drawing (not read by the audio thread).
    pub peaks: Arc<crate::waveform::PeakPyramid>,
}

impl Sample {
    pub fn new_mono(data: Arc<Vec<f32>>, peaks: Arc<crate::waveform::PeakPyramid>) -> Self {
        Self { data, peaks }
    }

    /// Full buffer length in seconds (ignores per-clip trim).
    #[allow(dead_code)]
    pub fn duration_secs(&self, sample_rate: u32) -> f32 {
        self.data.len() as f32 / sample_rate as f32
    }
}

#[derive(Clone)]
pub struct Clip {
    pub id: ClipId,
    pub start_time_secs: f32,
    /// File name only (no path), for UI on the clip.
    pub label: String,
    pub sample: Sample,
    /// First sample index in `sample.data` (inclusive).
    pub trim_start: usize,
    /// One past last sample index in `sample.data` (exclusive).
    pub trim_end: usize,
    /// Lane index (0 = top). Overlap is only forbidden within the same lane.
    pub track_index: usize,
    /// Alt-duplicate preview: may overlap others, omitted from mix until cleared after drop.
    pub placement_preview: bool,
}

impl Clip {
    pub fn visible_sample_len(&self) -> usize {
        self.trim_end.saturating_sub(self.trim_start)
    }

    /// Timeline duration after trim.
    pub fn timeline_duration_secs(&self, sample_rate: u32) -> f32 {
        self.visible_sample_len() as f32 / sample_rate.max(1) as f32
    }
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

/// Numbered cue on one track (keys `1`–`9` apply to the selected track).
#[derive(Clone, Copy, Debug)]
pub struct CueMarker {
    pub slot: u8,
    pub time_secs: f32,
    pub track_index: usize,
}

#[derive(Clone)]
pub struct Project {
    pub clips: Vec<Clip>,
    pub transport: Transport,
    pub device_sample_rate: u32,
    /// Monotonic source for [`ClipId`] (not serialized yet).
    pub next_clip_id: u64,
    /// Cue slots 1–9 (`M` to place, number keys to play from).
    pub markers: Vec<CueMarker>,
    /// Project tempo (BPM). Used by the tempo ruler; 4/4.
    pub tempo_bpm: f32,
}

impl Project {
    pub fn empty(device_sample_rate: u32) -> Self {
        Self {
            clips: Vec::new(),
            transport: Transport::default(),
            device_sample_rate,
            next_clip_id: 1,
            markers: Vec::new(),
            tempo_bpm: 120.0,
        }
    }

    pub fn alloc_clip_id(&mut self) -> ClipId {
        let id = ClipId(self.next_clip_id);
        self.next_clip_id = self.next_clip_id.wrapping_add(1);
        if self.next_clip_id == 0 {
            self.next_clip_id = 1;
        }
        id
    }

    pub fn clip_index(&self, id: ClipId) -> Option<usize> {
        self.clips.iter().position(|c| c.id == id)
    }

    /// Number of lanes to draw (at least one).
    pub fn track_count(&self) -> usize {
        self.clips
            .iter()
            .map(|c| c.track_index)
            .max()
            .map(|m| m + 1)
            .unwrap_or(1)
    }

    /// Lane for the next imported file (0, then 1, then 2, …).
    pub fn next_track_index(&self) -> usize {
        self.clips
            .iter()
            .map(|c| c.track_index)
            .max()
            .map(|m| m + 1)
            .unwrap_or(0)
    }
}
