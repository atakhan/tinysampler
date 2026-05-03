//! Mono mixdown at a timeline instant (real-time safe: no allocation).

use crate::model::Project;

/// Sum overlapping clips at timeline time `t` (seconds). `sample_rate` is the output clock.
pub fn mix_mono_sample_at(project: &Project, t: f32, sample_rate: u32) -> f32 {
    let rate = sample_rate as f32;
    let mut acc = 0.0f32;
    for clip in &project.clips {
        if clip.placement_preview {
            continue;
        }
        if t < clip.start_time_secs {
            continue;
        }
        let local = t - clip.start_time_secs;
        let idx_in_window = (local * rate) as usize;
        let vis = clip.trim_end.saturating_sub(clip.trim_start);
        if idx_in_window < vis {
            let sample_idx = clip.trim_start + idx_in_window;
            if sample_idx < clip.sample.data.len() {
                acc += clip.sample.data[sample_idx];
            }
        }
    }
    acc.clamp(-1.0, 1.0)
}
