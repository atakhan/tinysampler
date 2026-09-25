//! Mono mixdown at a timeline instant (real-time safe: no allocation).

use crate::model::{Project, TrackId};

/// Sum overlapping piano-roll notes at timeline time `t` (seconds).
/// Notes live inside sequence clips; the track sample is only a source buffer.
pub fn mix_mono_sample_at(project: &Project, t: f32) -> f32 {
    let mut acc = 0.0f32;
    for seq in &project.seq_clips {
        if t < seq.start_time_secs || t >= seq.end_time_secs() {
            continue;
        }
        if !project.track_audible(seq.track_id) {
            continue;
        }
        let local = t - seq.start_time_secs;
        let Some(track) = project.track(seq.track_id) else {
            continue;
        };
        let Some(sample) = track.sample.as_ref() else {
            continue;
        };
        let speed = track.playback_speed().max(0.05);
        let rate = sample.rate() as f32;
        for note in project.notes.iter().filter(|n| n.seq_id == seq.id) {
            if local < note.start_time_secs || local >= note.end_time_secs() {
                continue;
            }
            let Some(marker) = track.pad_markers.iter().find(|m| m.slot == note.slot) else {
                continue;
            };
            let into = local - note.start_time_secs;
            let idx = marker.start_index + (into * rate * speed) as usize;
            if idx < marker.end_index && idx < sample.data.len() {
                acc += sample.data[idx];
            }
        }
    }
    acc.clamp(-1.0, 1.0)
}

/// Preview of a file from the load window. Playback speed is 1 (not the track pitch).
pub fn mix_file_audition_at(sample: &crate::model::Sample, t_local: f32, end_secs: f32) -> f32 {
    if !t_local.is_finite() || t_local < 0.0 || (end_secs.is_finite() && t_local >= end_secs) {
        return 0.0;
    }
    let idx = (t_local * sample.rate() as f32) as usize;
    sample.data.get(idx).copied().unwrap_or(0.0)
}

pub fn mix_sampler_preview_at(project: &Project, t_local: f32) -> f32 {
    mix_track_preview_at(project, project.sampler_preview.track_id, t_local)
}

fn mix_track_preview_at(project: &Project, track_id: TrackId, t_local: f32) -> f32 {
    let speed = project.track_speed(track_id).max(0.05);
    let Some(track) = project.track(track_id) else {
        return 0.0;
    };
    let Some(sample) = track.sample.as_ref() else {
        return 0.0;
    };
    let rate = sample.rate() as f32;
    let idx = (t_local * rate * speed) as usize;
    if let Some(end) = project.sampler_preview.end_secs {
        if end.is_finite() && t_local >= end {
            return 0.0;
        }
    }
    if idx >= sample.data.len() {
        return 0.0;
    }
    sample.data[idx]
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
        let mut p = crate::model::Project::empty();
        let id = project_actions::add_track(&mut p);
        let data = vec![0.5f32; 64];
        let peaks = PeakPyramid::build(&data);
        project_actions::set_track_sample(
            &mut p,
            id,
            Sample::new_mono(Arc::new(data), Arc::new(peaks), 48_000),
            "a".into(),
        );
        p.track_mut(id).unwrap().pad_markers.push(PadMarker {
            slot: 0,
            start_index: 0,
            end_index: 8,
        });
        assert_eq!(mix_mono_sample_at(&p, 0.0), 0.0);
        let seq = project_actions::seq_clip_on_track(&p, id).unwrap();
        assert!(project_actions::place_pad_note(&mut p, seq, 0, 0.0).is_some());
        let v = mix_mono_sample_at(&p, 0.0);
        assert!(v > 0.4, "{v}");
    }

    #[test]
    fn mix_indexes_using_sample_rate_not_a_device_rate() {
        let mut p = crate::model::Project::empty();
        let id = project_actions::add_track(&mut p);
        let data = vec![0.0f32; 48_000];
        let mut data = data;
        data[24_000] = 1.0;
        let peaks = PeakPyramid::build(&data);
        project_actions::set_track_sample(
            &mut p,
            id,
            Sample::new_mono(Arc::new(data), Arc::new(peaks), 48_000),
            "a".into(),
        );
        p.track_mut(id).unwrap().pad_markers.push(PadMarker {
            slot: 0,
            start_index: 0,
            end_index: 48_000,
        });
        let seq = project_actions::seq_clip_on_track(&p, id).unwrap();
        assert!(project_actions::place_pad_note(&mut p, seq, 0, 0.0).is_some());
        let v = mix_mono_sample_at(&p, 0.5);
        assert!(v > 0.5, "{v}");
    }

    fn track_with_note() -> (crate::model::Project, crate::model::TrackId) {
        let mut p = crate::model::Project::empty();
        let id = project_actions::add_track(&mut p);
        let data = vec![0.5f32; 64];
        let peaks = PeakPyramid::build(&data);
        project_actions::set_track_sample(
            &mut p,
            id,
            Sample::new_mono(Arc::new(data), Arc::new(peaks), 48_000),
            "a".into(),
        );
        p.track_mut(id).unwrap().pad_markers.push(PadMarker {
            slot: 0,
            start_index: 0,
            end_index: 8,
        });
        let seq = project_actions::seq_clip_on_track(&p, id).unwrap();
        assert!(project_actions::place_pad_note(&mut p, seq, 0, 0.0).is_some());
        (p, id)
    }

    #[test]
    fn mute_silences_a_track() {
        let (mut p, id) = track_with_note();
        assert!(mix_mono_sample_at(&p, 0.0) > 0.4);
        assert!(project_actions::toggle_track_mute(&mut p, id));
        assert_eq!(mix_mono_sample_at(&p, 0.0), 0.0);
        assert!(!p.track_audible(id));
    }

    #[test]
    fn solo_plays_only_soloed_tracks() {
        let (mut p, a) = track_with_note();
        let b = project_actions::add_track(&mut p);
        let data = vec![0.5f32; 64];
        let peaks = PeakPyramid::build(&data);
        project_actions::set_track_sample(
            &mut p,
            b,
            Sample::new_mono(Arc::new(data), Arc::new(peaks), 48_000),
            "b".into(),
        );
        p.track_mut(b).unwrap().pad_markers.push(PadMarker {
            slot: 0,
            start_index: 0,
            end_index: 8,
        });
        let seq = project_actions::seq_clip_on_track(&p, b).unwrap();
        assert!(project_actions::place_pad_note(&mut p, seq, 0, 0.0).is_some());
        assert!(mix_mono_sample_at(&p, 0.0) > 0.8);
        assert!(project_actions::toggle_track_solo(&mut p, a));
        let soloed = mix_mono_sample_at(&p, 0.0);
        assert!(soloed > 0.4 && soloed < 0.7, "{soloed}");
        assert!(p.track_audible(a));
        assert!(!p.track_audible(b));
        assert!(project_actions::toggle_track_mute(&mut p, a));
        assert_eq!(mix_mono_sample_at(&p, 0.0), 0.0);
    }
}
