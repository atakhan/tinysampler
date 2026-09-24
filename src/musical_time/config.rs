//! Tunable constants for the tempo pipeline. None of these are musical laws.

/// How hard the analyzer should work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnalysisMode {
    /// Coarser features for a quick pass (for example after a drop).
    Fast,
    /// Multi-resolution features, local windows, and a tighter beat grid.
    Deep,
}

#[derive(Clone, Debug)]
pub struct FeatureWeights {
    pub flux: f32,
    pub log_flux: f32,
    pub energy: f32,
    pub phase: f32,
    pub complex: f32,
}

#[derive(Clone, Debug)]
pub struct BandEdges {
    pub low: (f32, f32),
    pub mid: (f32, f32),
    pub high: (f32, f32),
}

#[derive(Clone, Debug)]
pub struct Config {
    pub mode: AnalysisMode,
    /// Analysis sample rate. Higher-rate files are low-passed and resampled down.
    pub target_sample_rate: u32,
    /// STFT window lengths at [`Self::target_sample_rate`], in samples.
    pub windows: Vec<usize>,
    pub hop: usize,
    pub min_bpm: f32,
    pub max_bpm: f32,
    pub bpm_step: f32,
    pub bands: BandEdges,
    pub features: FeatureWeights,
    pub min_duration_secs: f32,
    /// Below this, the file is treated as non-rhythmic instead of assigned a BPM.
    pub rhythmicity_min: f32,
    pub periodicity_min: f32,
    pub beat_score_min: f32,
    /// Ellis dynamic-programming tightness. Lower follows tempo drift more easily.
    pub tightness: f32,
}

impl Config {
    pub fn for_mode(mode: AnalysisMode) -> Self {
        match mode {
            AnalysisMode::Fast => Self {
                mode,
                target_sample_rate: 22_050,
                windows: vec![1024],
                hop: 256,
                ..Self::shared()
            },
            AnalysisMode::Deep => Self {
                mode,
                target_sample_rate: 22_050,
                windows: vec![512, 1024, 2048],
                hop: 128,
                ..Self::shared()
            },
        }
    }

    fn shared() -> Self {
        Self {
            mode: AnalysisMode::Deep,
            target_sample_rate: 22_050,
            windows: vec![1024],
            hop: 128,
            min_bpm: 40.0,
            max_bpm: 240.0,
            bpm_step: 0.25,
            bands: BandEdges {
                low: (20.0, 150.0),
                mid: (150.0, 2_000.0),
                high: (2_000.0, 12_000.0),
            },
            features: FeatureWeights {
                flux: 0.32,
                log_flux: 0.24,
                energy: 0.22,
                phase: 0.08,
                complex: 0.14,
            },
            min_duration_secs: 0.75,
            rhythmicity_min: 0.33,
            periodicity_min: 0.28,
            beat_score_min: 0.36,
            tightness: 22.0,
        }
    }
}
