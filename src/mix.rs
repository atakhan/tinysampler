//! Mono mixdown at a timeline instant (real-time safe: no allocation).

use crate::model::Project;

/// Sum overlapping clips at timeline time `t` (seconds).
/// PCM is addressed at [`Project::device_sample_rate`], independent of the current output clock.
pub fn mix_mono_sample_at(project: &Project, t: f32) -> f32 {
    let rate = project.device_sample_rate.max(1) as f32;
    let mut acc = 0.0f32;
    for clip in &project.clips {
        if clip.placement_preview {
            continue;
        }
        if t < clip.start_time_secs {
            continue;
        }
        let speed = project.track_speed(clip.track_index).max(0.05);
        let local = t - clip.start_time_secs;
        let idx_in_window = (local * rate * speed) as usize;
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

/// Preview of the sampling instrument for `track_index` at local time (seconds of wall clock).
pub fn mix_sampler_preview_at(project: &Project, t_local: f32) -> f32 {
    let track_i = project.sampler_preview.track_index;
    let speed = project.track_speed(track_i).max(0.05);
    let rate = project.device_sample_rate.max(1) as f32;
    let Some(clip) = project.clips.iter().find(|c| c.track_index == track_i) else {
        return 0.0;
    };
    let idx_in_window = (t_local * rate * speed) as usize;
    let vis = clip.trim_end.saturating_sub(clip.trim_start);
    if idx_in_window >= vis {
        return 0.0;
    }
    let sample_idx = clip.trim_start + idx_in_window;
    if sample_idx < clip.sample.data.len() {
        clip.sample.data[sample_idx]
    } else {
        0.0
    }
}
