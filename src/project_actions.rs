//! Mutations of [`crate::model::Project`] used from the UI (single place for clip invariants).

use std::path::Path;

use crate::model::{Clip, ClipId, CueMarker, Project, Sample, Track};
use crate::theme::MIN_TRIM_DURATION_SECS;
use crate::timeline::{TrimDrag, TrimSide};
use crate::wav_loader;

pub fn add_track(project: &mut Project) -> usize {
    let i = project.tracks.len();
    project.tracks.push(Track {
        name: format!("Трек {}", i + 1),
        pitch_semitones: 0,
        source_tempo_bpm: project.tempo_bpm.clamp(20.0, 400.0),
    });
    i
}

pub fn set_track_sample(project: &mut Project, track_index: usize, sample: Sample, label: String) {
    let n = sample.data.len();
    if let Some(clip) = project
        .clips
        .iter_mut()
        .find(|c| c.track_index == track_index)
    {
        clip.sample = sample;
        clip.label = label;
        clip.trim_start = 0;
        clip.trim_end = n;
        clip.start_time_secs = 0.0;
        clip.placement_preview = false;
        return;
    }
    let id = project.alloc_clip_id();
    project.clips.push(Clip {
        id,
        start_time_secs: 0.0,
        label,
        sample,
        trim_start: 0,
        trim_end: n,
        track_index,
        placement_preview: false,
    });
}

pub fn append_audio_clip(project: &mut Project, sample: Sample, label: String) -> usize {
    let track_index = add_track(project);
    set_track_sample(project, track_index, sample, label);
    track_index
}

pub fn load_audio_file(path: &Path, sample_rate: u32) -> Result<(Sample, String), String> {
    let sample = wav_loader::load_audio_mono_f32(path, sample_rate)?;
    let label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clip")
        .to_string();
    Ok((sample, label))
}

/// Split the clip under `playhead_secs` into two clips. Left clip keeps the original [`ClipId`].
pub fn split_clip_at_playhead(
    project: &mut Project,
    playhead_secs: f32,
    preferred: Option<ClipId>,
) -> Result<ClipId, String> {
    let sr = project.device_sample_rate;
    let sr_f = sr as f32;
    let min_samples = ((MIN_TRIM_DURATION_SECS * sr_f).ceil() as usize).max(1);

    let in_clip_at = |project: &Project, i: usize| {
        let speed = project.track_speed(project.clips[i].track_index).max(0.05);
        let t0 = project.clips[i].start_time_secs;
        let t1 = t0 + project.clips[i].timeline_duration_secs(sr) / speed;
        playhead_secs > t0 && playhead_secs < t1
    };
    let idx = preferred
        .and_then(|id| project.clip_index(id))
        .filter(|&i| in_clip_at(project, i))
        .or_else(|| (0..project.clips.len()).find(|&i| in_clip_at(project, i)));
    let Some(i) = idx else {
        return Err("Разрез: поставьте плейхед внутри клипа.".into());
    };

    let clip = project.clips.remove(i);
    let kept_left_id = clip.id;
    let speed = project
        .tracks
        .get(clip.track_index)
        .map(Track::playback_speed)
        .unwrap_or(1.0)
        .max(0.05);
    let mut split_at = clip.trim_start
        + ((playhead_secs - clip.start_time_secs) * sr_f * speed).round() as usize;
    split_at = split_at
        .max(clip.trim_start + min_samples)
        .min(clip.trim_end.saturating_sub(min_samples));
    if split_at <= clip.trim_start || split_at >= clip.trim_end {
        project.clips.insert(i, clip);
        return Err("Нельзя разрезать: слишком короткий фрагмент.".into());
    }

    let split_time = clip.start_time_secs + (split_at - clip.trim_start) as f32 / (sr_f * speed);
    let right_id = project.alloc_clip_id();

    let left = Clip {
        id: clip.id,
        start_time_secs: clip.start_time_secs,
        label: clip.label.clone(),
        sample: clip.sample.clone(),
        trim_start: clip.trim_start,
        trim_end: split_at,
        track_index: clip.track_index,
        placement_preview: false,
    };
    let right = Clip {
        id: right_id,
        start_time_secs: split_time,
        label: clip.label,
        sample: clip.sample,
        trim_start: split_at,
        trim_end: clip.trim_end,
        track_index: clip.track_index,
        placement_preview: false,
    };

    project.clips.insert(i, left);
    project.clips.insert(i + 1, right);

    Ok(kept_left_id)
}

/// Returns `true` if a clip was removed.
pub fn delete_clip(project: &mut Project, id: ClipId) -> bool {
    let Some(i) = project.clip_index(id) else {
        return false;
    };
    project.clips.remove(i);
    true
}

pub fn apply_trim_delta(project: &mut Project, drag: TrimDrag, dx_px: f32, pps: f32) -> bool {
    let sr = project.device_sample_rate as f32;
    let min_samples = ((MIN_TRIM_DURATION_SECS * sr).ceil() as usize).max(1);
    let Some(idx) = project.clip_index(drag.clip_id) else {
        return false;
    };
    let speed = project.track_speed(project.clips[idx].track_index).max(0.05);
    let ds_samples = ((dx_px / pps) * sr * speed).round() as i64;
    if ds_samples == 0 {
        return false;
    }
    let clip = match project.clips.get_mut(idx) {
        Some(c) => c,
        None => return false,
    };
    let data_len = clip.sample.data.len() as i64;
    let ts = clip.trim_start as i64;
    let te = clip.trim_end as i64;
    match drag.side {
        TrimSide::Left => {
            let ts_new = (ts + ds_samples).max(0).min(te - min_samples as i64);
            let actual = ts_new - ts;
            if actual == 0 {
                return false;
            }
            clip.trim_start = ts_new as usize;
            clip.start_time_secs += actual as f32 / (sr * speed);
            true
        }
        TrimSide::Right => {
            let te_new = (te + ds_samples).max(ts + min_samples as i64).min(data_len);
            let actual = te_new - te;
            if actual == 0 {
                return false;
            }
            clip.trim_end = te_new as usize;
            true
        }
    }
}

fn merge_other_clip_intervals(project: &Project, exclude_idx: usize) -> Vec<(f32, f32)> {
    let track = project.clips[exclude_idx].track_index;
    let mut v: Vec<(f32, f32)> = Vec::new();
    for i in 0..project.clips.len() {
        if i == exclude_idx || project.clips[i].track_index != track {
            continue;
        }
        let t0 = project.clips[i].start_time_secs;
        let d = project.clip_sounding_secs_at(i);
        v.push((t0, t0 + d));
    }
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut out: Vec<(f32, f32)> = Vec::new();
    for (s, e) in v {
        if let Some(last) = out.last_mut() {
            if s < last.1 {
                last.1 = last.1.max(e);
            } else {
                out.push((s, e));
            }
        } else {
            out.push((s, e));
        }
    }
    out
}

/// Ranges of valid `start_time_secs` for `clips[idx]` so it does not overlap any other clip.
fn feasible_start_ranges(project: &Project, idx: usize, d: f32) -> Vec<(f32, f32)> {
    let merged = merge_other_clip_intervals(project, idx);
    let mut ranges = Vec::new();
    let mut prev_end = 0.0f32;
    for (m0, m1) in &merged {
        let gap = m0 - prev_end;
        if gap >= d {
            ranges.push((prev_end, m0 - d));
        }
        prev_end = *m1;
    }
    ranges.push((prev_end, f32::INFINITY));
    ranges
}

/// Snap `proposed` to the nearest point in the union of `ranges` (inclusive ends per range).
fn clamp_start_to_feasible_ranges(proposed: f32, ranges: &[(f32, f32)], hint_old: f32) -> f32 {
    let proposed = proposed.max(0.0);
    let mut best = hint_old;
    let mut best_dist = f32::INFINITY;
    for &(lo, hi) in ranges {
        let lo = lo.max(0.0);
        if hi < lo {
            continue;
        }
        let c = proposed.clamp(lo, hi);
        let dist = (c - proposed).abs();
        if dist < best_dist - 1e-6 {
            best = c;
            best_dist = dist;
        } else if (dist - best_dist).abs() <= 1e-6 {
            if (c - hint_old).signum() == (proposed - hint_old).signum() {
                best = c;
            }
        }
    }
    best
}

pub fn clip_start_respecting_no_overlap(
    project: &Project,
    idx: usize,
    proposed_start: f32,
    hint_old: f32,
) -> f32 {
    let d = project.clip_sounding_secs_at(idx);
    let ranges = feasible_start_ranges(project, idx, d);
    clamp_start_to_feasible_ranges(proposed_start, &ranges, hint_old)
}

/// True if clip `idx` overlaps any other clip on the timeline (positive-length intersection).
pub fn clip_overlaps_others(project: &Project, idx: usize) -> bool {
    let track = project.clips[idx].track_index;
    let t0 = project.clips[idx].start_time_secs;
    let t1 = t0 + project.clip_sounding_secs_at(idx);
    for j in 0..project.clips.len() {
        if j == idx || project.clips[j].track_index != track {
            continue;
        }
        let o0 = project.clips[j].start_time_secs;
        let o1 = o0 + project.clip_sounding_secs_at(j);
        if t0 < o1 && o0 < t1 {
            return true;
        }
    }
    false
}

/// Snap every preview clip to a non-overlapping position and clear the flag (no animation).
pub fn resolve_all_placement_previews(project: &mut Project) {
    for i in 0..project.clips.len() {
        if !project.clips[i].placement_preview {
            continue;
        }
        let cur = project.clips[i].start_time_secs;
        let target = clip_start_respecting_no_overlap(project, i, cur, cur);
        project.clips[i].start_time_secs = target;
        project.clips[i].placement_preview = false;
    }
}

/// Clone `id` to a new clip (same trim, shared sample buffer), same start as source — preview
/// until drop; may overlap while dragging.
pub fn duplicate_clip(project: &mut Project, id: ClipId) -> Option<ClipId> {
    let idx = project.clip_index(id)?;
    let (start, label, sample, trim_start, trim_end, track_index) = {
        let orig = project.clips.get(idx)?;
        (
            orig.start_time_secs,
            orig.label.clone(),
            orig.sample.clone(),
            orig.trim_start,
            orig.trim_end,
            orig.track_index,
        )
    };
    let new_id = project.alloc_clip_id();
    let new_clip = Clip {
        id: new_id,
        start_time_secs: start,
        label: format!("{} copy", label),
        sample,
        trim_start,
        trim_end,
        track_index,
        placement_preview: true,
    };
    project.clips.push(new_clip);
    Some(new_id)
}

/// Move clip start by `dx_px`. If `allow_overlap`, only clamps to `>= 0` (preview drag).
pub fn nudge_clip_time_by_drag(
    project: &mut Project,
    clip_id: ClipId,
    dx_px: f32,
    pps: f32,
    allow_overlap: bool,
) -> bool {
    if dx_px == 0.0 {
        return false;
    }
    let Some(idx) = project.clip_index(clip_id) else {
        return false;
    };
    let old_start = project.clips[idx].start_time_secs;
    let proposed = (old_start + dx_px / pps).max(0.0);
    let new_start = if allow_overlap {
        proposed
    } else {
        clip_start_respecting_no_overlap(project, idx, proposed, old_start)
    };
    if (new_start - old_start).abs() <= 1e-6 {
        return false;
    }
    project.clips[idx].start_time_secs = new_start;
    true
}

pub fn set_clip_track(project: &mut Project, clip_id: ClipId, track_index: usize) -> bool {
    let Some(idx) = project.clip_index(clip_id) else {
        return false;
    };
    if project.clips[idx].track_index == track_index {
        return false;
    }
    project.clips[idx].track_index = track_index;
    true
}

pub const MARKER_SLOT_MAX: u8 = 9;

/// Next free slot `1..=9`, or `None` if all digits are taken.
pub fn try_place_marker(project: &mut Project, time_secs: f32, track_index: usize) -> Option<u8> {
    let slot = (1u8..=MARKER_SLOT_MAX).find(|&s| {
        !project
            .markers
            .iter()
            .any(|m| m.track_index == track_index && m.slot == s)
    })?;
    let time_secs = time_secs.max(0.0);
    if !time_secs.is_finite() {
        return None;
    }
    project.markers.push(CueMarker {
        slot,
        time_secs,
        track_index,
    });
    Some(slot)
}

pub fn marker_time(project: &Project, slot: u8, track_index: usize) -> Option<f32> {
    project
        .markers
        .iter()
        .find(|m| m.slot == slot && m.track_index == track_index)
        .map(|m| m.time_secs)
}

pub fn move_marker(project: &mut Project, slot: u8, track_index: usize, time_secs: f32) -> bool {
    let Some(m) = project
        .markers
        .iter_mut()
        .find(|m| m.slot == slot && m.track_index == track_index)
    else {
        return false;
    };
    let t = time_secs.max(0.0);
    if !t.is_finite() || (m.time_secs - t).abs() <= 1e-6 {
        return false;
    }
    m.time_secs = t;
    true
}

pub fn delete_marker(project: &mut Project, slot: u8, track_index: usize) -> bool {
    let n = project.markers.len();
    project
        .markers
        .retain(|m| !(m.slot == slot && m.track_index == track_index));
    project.markers.len() != n
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dummy_sample() -> Sample {
        let data = vec![0.0f32; 64];
        let peaks = crate::waveform::PeakPyramid::build(&data);
        Sample::new_mono(Arc::new(data), Arc::new(peaks))
    }

    #[test]
    fn each_imported_clip_gets_the_next_track() {
        let mut p = Project::empty(48_000);
        assert_eq!(append_audio_clip(&mut p, dummy_sample(), "a".into()), 0);
        assert_eq!(append_audio_clip(&mut p, dummy_sample(), "b".into()), 1);
        assert_eq!(append_audio_clip(&mut p, dummy_sample(), "c".into()), 2);
        assert_eq!(p.clips[0].track_index, 0);
        assert_eq!(p.clips[1].track_index, 1);
        assert_eq!(p.clips[2].track_index, 2);
        assert_eq!(p.tracks.len(), 3);
        assert_eq!(p.track_count(), 3);
    }

    #[test]
    fn set_track_sample_replaces_existing_clip() {
        let mut p = Project::empty(48_000);
        let i = add_track(&mut p);
        set_track_sample(&mut p, i, dummy_sample(), "a".into());
        set_track_sample(&mut p, i, dummy_sample(), "b".into());
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.clips.len(), 1);
        assert_eq!(p.clips[0].label, "b");
        assert_eq!(p.clips[0].track_index, 0);
    }

    #[test]
    fn markers_fill_slots_1_to_9_then_stop() {
        let mut p = Project::empty(48_000);
        for i in 1u8..=9 {
            assert_eq!(try_place_marker(&mut p, i as f32, 0), Some(i));
        }
        assert_eq!(try_place_marker(&mut p, 99.0, 0), None);
        assert_eq!(try_place_marker(&mut p, 0.0, 1), Some(1));
        assert_eq!(p.markers.len(), 10);
        assert_eq!(marker_time(&p, 3, 0), Some(3.0));
        assert!(move_marker(&mut p, 3, 0, 1.5));
        assert_eq!(marker_time(&p, 3, 0), Some(1.5));
        assert!(delete_marker(&mut p, 3, 0));
        assert_eq!(marker_time(&p, 3, 0), None);
        assert_eq!(try_place_marker(&mut p, 0.0, 0), Some(3));
    }
}
