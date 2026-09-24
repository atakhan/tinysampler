//! Hook for a future neural beat estimator.
//!
//! Predictions are another evidence curve. They are mixed into the same
//! dynamic-programming decoder; they do not replace it.

#[derive(Clone, Debug, Default)]
pub struct NeuralPrediction {
    pub beat: Vec<f32>,
    pub downbeat: Vec<f32>,
    pub onset: Vec<f32>,
}

pub trait NeuralBeatEstimator: Send + Sync {
    fn predict(&self, frame_rate: f32, frame_count: usize) -> NeuralPrediction;
}
