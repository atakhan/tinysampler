use std::sync::Arc;

/// Stable handle for a studio track (survives reorder/delete better than a vec index).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TrackId(pub u64);

/// Trigger of a pad chop on the studio piano roll.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct NoteId(pub u64);

/// Sequence clip (“колбаска”) on a studio track.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct SeqId(pub u64);

#[derive(Clone, Copy, Debug)]
pub struct SeqClip {
    pub id: SeqId,
    pub track_id: TrackId,
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

/// Mono samples in f32 [-1, 1] at [`Self::sample_rate`] (document rate, not the device).
#[derive(Clone)]
pub struct Sample {
    pub data: Arc<Vec<f32>>,
    /// Min/max bins for waveform drawing (not read by the audio thread).
    pub peaks: Arc<crate::waveform::PeakPyramid>,
    pub sample_rate: u32,
}

impl Sample {
    pub fn new_mono(data: Arc<Vec<f32>>, peaks: Arc<crate::waveform::PeakPyramid>, sample_rate: u32) -> Self {
        Self {
            data,
            peaks,
            sample_rate: sample_rate.max(1),
        }
    }

    pub fn rate(&self) -> u32 {
        self.sample_rate.max(1)
    }
}

/// One source file laid into the track buffer, back to back with the others.
#[derive(Clone, Debug)]
pub struct SampleSlice {
    pub label: String,
    pub start_index: usize,
    pub end_index: usize,
}
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
    pub id: TrackId,
    pub name: String,
    /// Classic-sampler pitch: playback speed `2^(n/12)`.
    pub pitch_semitones: i32,
    /// Instrument buffer (one per track). Timeline sound comes from notes, not this clip.
    pub sample: Option<Sample>,
    pub sample_label: String,
    /// Source files concatenated in [`Self::sample`], in order.
    pub sample_slices: Vec<SampleSlice>,
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
    pub track_id: TrackId,
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
            track_id: TrackId(0),
            start_secs: 0.0,
            end_secs: None,
        }
    }
}

/// A WAV/MP3 being auditioned from the load window. Not part of the document.
#[derive(Clone)]
pub struct FileAudition {
    pub sample: Option<Sample>,
    pub label: String,
    pub playing: bool,
    pub generation: u64,
    pub end_secs: f32,
}

impl Default for FileAudition {
    fn default() -> Self {
        Self {
            sample: None,
            label: String::new(),
            playing: false,
            generation: 0,
            end_secs: 0.0,
        }
    }
}

#[derive(Clone)]
pub struct Transport {
    pub is_playing: bool,
    /// Incremented on Stop so the audio thread jumps the playhead to [`Self::stop_return_secs`].
    pub stop_generation: u64,
    /// Playhead position applied when `stop_generation` changes (the pre-play cursor).
    pub stop_return_secs: f32,
}

impl Default for Transport {
    fn default() -> Self {
        Self {
            is_playing: false,
            stop_generation: 0,
            stop_return_secs: 0.0,
        }
    }
}

/// Numbered cue on one track (keys `1`–`9` apply to the selected track).
#[derive(Clone, Copy, Debug)]
pub struct CueMarker {
    pub slot: u8,
    pub time_secs: f32,
    pub track_id: TrackId,
}

#[derive(Clone)]
pub struct Project {
    pub name: String,
    pub transport: Transport,
    /// Cue slots 1–9 (`M` to place, number keys to play from).
    pub markers: Vec<CueMarker>,
    /// Piano-roll notes; times are relative to the parent sequence clip.
    pub notes: Vec<PadNote>,
    /// Sequence clips on the timeline (the “колбаски”).
    pub seq_clips: Vec<SeqClip>,
    /// Explicit studio tracks (sidebar). Sequences and markers reference [`TrackId`].
    pub tracks: Vec<Track>,
    pub next_note_id: u64,
    pub next_seq_id: u64,
    pub next_track_id: u64,
    /// Project tempo (BPM). Sole source of truth for the 4/4 grid and default chop length.
    pub tempo_bpm: f32,
    /// Preview inside the sampling instrument (takes over the output while playing).
    pub sampler_preview: SamplerPreview,
    /// File audition for the load window. Not written to disk.
    pub audition: FileAudition,
}

impl Project {
    pub fn empty() -> Self {
        Self {
            name: "Проект".into(),
            transport: Transport::default(),
            markers: Vec::new(),
            notes: Vec::new(),
            seq_clips: Vec::new(),
            tracks: Vec::new(),
            next_note_id: 1,
            next_seq_id: 1,
            next_track_id: 1,
            tempo_bpm: 120.0,
            sampler_preview: SamplerPreview::default(),
            audition: FileAudition::default(),
        }
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

    pub fn alloc_track_id(&mut self) -> TrackId {
        let id = TrackId(self.next_track_id);
        self.next_track_id = self.next_track_id.wrapping_add(1);
        if self.next_track_id == 0 {
            self.next_track_id = 1;
        }
        id
    }

    pub fn note_index(&self, id: NoteId) -> Option<usize> {
        self.notes.iter().position(|n| n.id == id)
    }

    pub fn seq_index(&self, id: SeqId) -> Option<usize> {
        self.seq_clips.iter().position(|s| s.id == id)
    }

    pub fn track_index(&self, id: TrackId) -> Option<usize> {
        self.tracks.iter().position(|t| t.id == id)
    }

    pub fn track(&self, id: TrackId) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }

    pub fn track_mut(&mut self, id: TrackId) -> Option<&mut Track> {
        self.tracks.iter_mut().find(|t| t.id == id)
    }

    pub fn track_id_at_lane(&self, lane: usize) -> Option<TrackId> {
        self.tracks.get(lane).map(|t| t.id)
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn track_speed(&self, id: TrackId) -> f32 {
        self.track(id).map(Track::playback_speed).unwrap_or(1.0)
    }

    pub fn sample_len(&self, id: TrackId) -> usize {
        self.track(id)
            .and_then(|t| t.sample.as_ref())
            .map(|s| s.data.len())
            .unwrap_or(0)
    }
}
