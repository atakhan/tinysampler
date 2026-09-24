//! Musical time analysis for sampling.
//!
//! The pipeline does not guess a BPM from a single autocorrelation peak.
//! It builds onset evidence, proposes tempo hypotheses, recovers a beat grid,
//! then picks the metrical reading that best explains those events.
//!
//! Audio decoding and UI stay outside this module. [`analyze`] only borrows
//! the caller's samples.

#![allow(dead_code)]
mod analyze;
mod beat;
mod config;
mod fft;
mod loops;
mod meter;
mod neural;
mod onset;
mod periodicity;
mod preprocess;
mod types;
mod util;
mod warp;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use analyze::{analyze, analyze_with_estimator};
#[allow(unused_imports)]
pub use config::{AnalysisMode, Config};
#[allow(unused_imports)]
pub use loops::detect_loops;
#[allow(unused_imports)]
pub use neural::{NeuralBeatEstimator, NeuralPrediction};
#[allow(unused_imports)]
pub use types::*;
