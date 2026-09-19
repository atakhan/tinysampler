use std::sync::Arc;

/// Stable handle for a clip on the timeline (survives better than raw indices).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ClipId(pub u64);

/// Trigger of a pad chop on the studio piano roll.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NoteId(pub u64);

/// Sequence clip (“колбаска”) on a studio track.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SeqId(pub u64);

#[derive(Clone, Copy, Debug)]
pub struct SeqClip {
    pub id: SeqId,
    pub track_index: usize,
    pub start_time_secs: f32,
    pub duration_secs: f32,
}

impl SeqClip {
    pub fn end_time_secs(&self) -> f32 {
        self.start_time_secs + self.duration_secs
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PadNote {
    pub id: NoteId,
    pub seq_id: SeqId,
    pub slot: u8,
    /// Seconds from the start of the parent [`SeqClip`].
    pub start_time_secs: f32,
    pub duration_secs: f32,
}

impl PadNote {
    pub fn end_time_secs(&self) -> f32 {
        self.start_time_secs + self.duration_secs
    }
}

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

/// Which edge of a pad region is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PadEdge {
    Start,
    End,
}

/// Chop on the sampling-instrument waveform, bound to a pad slot `0..16`.
#[derive(Clone, Copy, Debug)]
pub struct PadMarker {
    pub slot: u8,
    /// Inclusive start in `sample.data`.
    pub start_index: usize,
    /// Exclusive end in `sample.data`.
    pub end_index: usize,
}

#[derive(Clone)]
pub struct Track {
    pub name: String,
    /// Classic-sampler pitch: playback speed `2^(n/12)`.
    pub pitch_semitones: i32,
    /// Tempo assigned in the sampling instrument (4/4 grid); 20–400 BPM.
    pub source_tempo_bpm: f32,
    /// Sample chops in the instrument; slot `0` is top-left pad (Q).
    pub pad_markers: Vec<PadMarker>,
}

impl Track {
    pub fn playback_speed(&self) -> f32 {
        2f32.powf(self.pitch_semitones as f32 / 12.0)
    }
}

#[derive(Clone)]
pub struct SamplerPreview {
    pub playing: bool,
    /// Bumped to restart preview from [`Self::start_secs`].
    pub generation: u64,
    pub track_index: usize,
    /// Wall-clock seconds into the sample when a generation bump restarts.
    pub start_secs: f32,
    /// Stop preview at this local time (`None` = until the buffer ends).
    pub end_secs: Option<f32>,
}

impl Default for SamplerPreview {
    fn default() -> Self {
        Self {
            playing: false,
            generation: 0,
            track_index: 0,
            start_secs: 0.0,
            end_secs: None,
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
    /// Piano-roll notes; times are relative to the parent sequence clip.
    pub notes: Vec<PadNote>,
    /// Sequence clips on the timeline (the “колбаски”).
    pub seq_clips: Vec<SeqClip>,
    /// Explicit studio tracks (sidebar). Clips reference `track_index`.
    pub tracks: Vec<Track>,
    /// Monotonic source for [`NoteId`].
    pub next_note_id: u64,
    /// Monotonic source for [`SeqId`].
    pub next_seq_id: u64,
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
            notes: Vec::new(),
            seq_clips: Vec::new(),
            tracks: Vec::new(),
            next_note_id: 1,
            next_seq_id: 1,
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

    pub fn alloc_note_id(&mut self) -> NoteId {
        let id = NoteId(self.next_note_id);
        self.next_note_id = self.next_note_id.wrapping_add(1);
        if self.next_note_id == 0 {
            self.next_note_id = 1;
        }
        id
    }

    pub fn alloc_seq_id(&mut self) -> SeqId {
        let id = SeqId(self.next_seq_id);
        self.next_seq_id = self.next_seq_id.wrapping_add(1);
        if self.next_seq_id == 0 {
            self.next_seq_id = 1;
        }
        id
    }

    pub fn note_index(&self, id: NoteId) -> Option<usize> {
        self.notes.iter().position(|n| n.id == id)
    }

    pub fn seq_index(&self, id: SeqId) -> Option<usize> {
        self.seq_clips.iter().position(|s| s.id == id)
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
