//! Result of musical-time analysis. BPM is one field, not the whole result.

use super::config::AnalysisMode;

/// A later loop detector fills these from the beat grid. V1 leaves the list empty.
#[derive(Clone, Debug)]
pub struct LoopCandidate {
    pub start_secs: f32,
    pub end_secs: f32,
    pub beats: f32,
    pub bars: f32,
    pub confidence: f32,
}

/// Why a tempo was withheld. The file is left without an invented BPM.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownReason {
    InsufficientPeriodicity,
    LowRhythmicity,
    InsufficientDuration,
    AmbiguousTempo,
    Silent,
}

#[derive(Clone, Debug)]
pub struct TempoCandidate {
    pub bpm: f32,
    pub confidence: f32,
    pub periodicity_score: f32,
    pub onset_score: f32,
    pub beat_score: f32,
}

#[derive(Clone, Debug)]
pub struct TempoEstimate {
    /// `None` when the signal does not support a tempo. Not a placeholder number.
    pub bpm: Option<f32>,
    pub confidence: f32,
    /// Other readings worth showing, best first. Does not repeat [`Self::bpm`].
    pub alternatives: Vec<TempoCandidate>,
    /// `1` is a steady pulse, `0` is a drift or a grid too short to judge.
    pub stability: f32,
    pub first_beat_secs: Option<f32>,
    /// Position of the grid inside one beat, in `0..period`.
    pub phase_secs: Option<f32>,
    pub unknown_reason: Option<UnknownReason>,
}

#[derive(Clone, Debug)]
pub struct Beat {
    pub time_secs: f32,
    /// Musical beat index along the grid. `0` is the first stored beat.
    pub position: f32,
    pub strength: f32,
}

#[derive(Clone, Debug)]
pub struct Bar {
    pub start_secs: f32,
    pub end_secs: f32,
    pub downbeat_index: usize,
}

#[derive(Clone, Debug)]
pub struct TempoPoint {
    pub time_secs: f32,
    pub bpm: f32,
}

#[derive(Clone, Debug)]
pub struct LocalTempo {
    pub start_secs: f32,
    pub end_secs: f32,
    pub bpm: Option<f32>,
    pub rhythmicity: f32,
    pub confidence: f32,
}

#[derive(Clone, Debug)]
pub struct MeterOption {
    pub beats_per_bar: u8,
    pub confidence: f32,
}

#[derive(Clone, Debug)]
pub struct MeterEstimate {
    /// `None` when the bar length is ambiguous or there is not enough grid.
    pub beats_per_bar: Option<u8>,
    pub confidence: f32,
    pub alternatives: Vec<MeterOption>,
}

impl Default for MeterEstimate {
    fn default() -> Self {
        Self {
            beats_per_bar: None,
            confidence: 0.0,
            alternatives: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct WarpPoint {
    pub audio_secs: f32,
    pub beat: f32,
}

#[derive(Clone, Debug, Default)]
pub struct WarpMap {
    pub points: Vec<WarpPoint>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ConfidenceSet {
    pub tempo: f32,
    pub beat_grid: f32,
    pub phase: f32,
    pub downbeat: f32,
    pub meter: f32,
}

/// Intermediate curves kept so the detector is not a black box.
#[derive(Clone, Debug)]
pub struct Diagnostics {
    pub mode: AnalysisMode,
    pub frame_rate: f32,
    pub onset_times: Vec<f32>,
    pub onset_envelope: Vec<f32>,
    pub tempogram_bpm: Vec<f32>,
    pub tempogram: Vec<f32>,
    pub tempo_candidates: Vec<TempoCandidate>,
    pub beat_times: Vec<f32>,
    pub tempo_curve: Vec<TempoPoint>,
    pub rhythmicity: f32,
    pub confidence: ConfidenceSet,
    pub notes: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct MusicalTimeAnalysis {
    pub duration_secs: f32,
    pub tempo: TempoEstimate,
    pub beats: Vec<Beat>,
    pub downbeats: Vec<Beat>,
    pub bars: Vec<Bar>,
    pub tempo_curve: Vec<TempoPoint>,
    pub local_tempo: Vec<LocalTempo>,
    pub rhythmicity: f32,
    pub meter: MeterEstimate,
    pub warp: WarpMap,
    /// Empty until a loop layer is filled in. The field is the extension point.
    pub loops: Vec<LoopCandidate>,
    pub confidence: ConfidenceSet,
    pub diagnostics: Diagnostics,
}
