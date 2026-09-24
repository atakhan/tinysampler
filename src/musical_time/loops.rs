//! Loop layer. V1 returns no candidates. [`super::analyze`] still calls it,
//! so a real detector can start filling [`super::LoopCandidate`] without a new pipeline.

use super::types::{LoopCandidate, MusicalTimeAnalysis};

pub fn detect_loops(_analysis: &MusicalTimeAnalysis) -> Vec<LoopCandidate> {
    Vec::new()
}
