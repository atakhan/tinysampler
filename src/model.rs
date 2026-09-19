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

#[derive(Clone)]
pub struct Track {
    pub name: String,
    /// Classic-sampler pitch: playback speed `2^(n/12)`.
    pub pitch_semitones: i32,
    /// Tempo assigned in the sampling instrument (4/4 grid); 20–400 BPM.
    pub source_tempo_bpm: f32,
}

impl Track {
    pub fn playback_speed(&self) -> f32 {
        2f32.powf(self.pitch_semitones as f32 / 12.0)
    }
}

#[derive(Clone)]
pub struct SamplerPreview {
    pub playing: bool,
    /// Bumped to restart preview from the beginning.
    pub generation: u64,
    pub track_index: usize,
}

impl Default for SamplerPreview {
    fn default() -> Self {
        Self {
            playing: false,
            generation: 0,
            track_index: 0,
        }
    }
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
    pub name: String,
    pub clips: Vec<Clip>,
    pub transport: Transport,
    pub device_sample_rate: u32,
    /// Monotonic source for [`ClipId`] (not serialized yet).
    pub next_clip_id: u64,
    /// Cue slots 1–9 (`M` to place, number keys to play from).
    pub markers: Vec<CueMarker>,
    /// Explicit studio tracks (sidebar). Clips reference `track_index`.
    pub tracks: Vec<Track>,
    /// Project tempo (BPM). Used by the tempo ruler; 4/4.
    pub tempo_bpm: f32,
    /// Preview inside the sampling instrument (takes over the output while playing).
    pub sampler_preview: SamplerPreview,
}

impl Project {
    pub fn empty(device_sample_rate: u32) -> Self {
        Self {
            name: "Проект".into(),
            clips: Vec::new(),
            transport: Transport::default(),
            device_sample_rate,
            next_clip_id: 1,
            markers: Vec::new(),
            tracks: Vec::new(),
            tempo_bpm: 120.0,
            sampler_preview: SamplerPreview::default(),
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

    /// Number of studio tracks.
    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Index for a newly created track.
    #[allow(dead_code)]
    pub fn next_track_index(&self) -> usize {
        self.tracks.len()
    }

    pub fn track_speed(&self, track_index: usize) -> f32 {
        self.tracks
            .get(track_index)
            .map(Track::playback_speed)
            .unwrap_or(1.0)
    }

    #[allow(dead_code)]
    pub fn clip_sounding_secs(&self, clip: &Clip) -> f32 {
        clip.timeline_duration_secs(self.device_sample_rate)
            / self.track_speed(clip.track_index).max(0.05)
    }

    pub fn clip_sounding_secs_at(&self, idx: usize) -> f32 {
        let (raw, ti) = match self.clips.get(idx) {
            Some(c) => (c.timeline_duration_secs(self.device_sample_rate), c.track_index),
            None => return 0.0,
        };
        raw / self.track_speed(ti).max(0.05)
    }
}
