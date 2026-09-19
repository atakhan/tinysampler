//! Mono mixdown at a timeline instant (real-time safe: no allocation).

use crate::model::Project;

/// Sum overlapping piano-roll notes at timeline time `t` (seconds).
/// Notes live inside sequence clips; source sample clips are not mixed.
pub fn mix_mono_sample_at(project: &Project, t: f32) -> f32 {
    let rate = project.device_sample_rate.max(1) as f32;
    let mut acc = 0.0f32;
    for seq in &project.seq_clips {
        if t < seq.start_time_secs || t >= seq.end_time_secs() {
            continue;
        }
        let local = t - seq.start_time_secs;
        let Some(track) = project.tracks.get(seq.track_index) else {
            continue;
        };
        let Some(clip) = project.clips.iter().find(|c| c.track_index == seq.track_index) else {
            continue;
        };
        let speed = track.playback_speed().max(0.05);
        for note in project.notes.iter().filter(|n| n.seq_id == seq.id) {
            if local < note.start_time_secs || local >= note.end_time_secs() {
                continue;
            }
            let Some(marker) = track.pad_markers.iter().find(|m| m.slot == note.slot) else {
                continue;
            };
            let into = local - note.start_time_secs;
            let idx = marker.start_index + (into * rate * speed) as usize;
            if idx < marker.end_index && idx < clip.sample.data.len() {
                acc += clip.sample.data[idx];
            }
        }
    }
    acc.clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::model::{PadMarker, Sample};
    use crate::project_actions;
    use crate::waveform::PeakPyramid;

    #[test]
    fn studio_mix_plays_notes_not_the_source_clip() {
        let mut p = crate::model::Project::empty(48_000);
        let i = project_actions::add_track(&mut p);
        let data = vec![0.5f32; 64];
        let peaks = PeakPyramid::build(&data);
        project_actions::set_track_sample(
            &mut p,
            i,
            Sample::new_mono(Arc::new(data), Arc::new(peaks)),
            "a".into(),
        );
        p.tracks[i].pad_markers.push(PadMarker {
            slot: 0,
            start_index: 0,
            end_index: 8,
        });
        assert_eq!(mix_mono_sample_at(&p, 0.0), 0.0);
        let seq = project_actions::seq_clip_on_track(&p, i).unwrap();
        assert!(project_actions::place_pad_note(&mut p, seq, 0, 0.0).is_some());
        let v = mix_mono_sample_at(&p, 0.0);
        assert!(v > 0.4, "{v}");
    }
}

/// Preview of the sampling instrument for `track_index` at local time (seconds of wall clock).
/// Indexes the full sample buffer (chops live on the instrument waveform, not timeline trim).
pub fn mix_sampler_preview_at(project: &Project, t_local: f32) -> f32 {
    let track_i = project.sampler_preview.track_index;
    let speed = project.track_speed(track_i).max(0.05);
    let rate = project.device_sample_rate.max(1) as f32;
    let Some(clip) = project.clips.iter().find(|c| c.track_index == track_i) else {
        return 0.0;
    };
    let idx = (t_local * rate * speed) as usize;
    if let Some(end) = project.sampler_preview.end_secs {
        if end.is_finite() && t_local >= end {
            return 0.0;
        }
    }
    if idx >= clip.sample.data.len() {
        return 0.0;
    }
    clip.sample.data[idx]
}
