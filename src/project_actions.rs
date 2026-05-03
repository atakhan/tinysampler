//! Mutations of [`crate::model::Project`] used from the UI (single place for clip invariants).

use std::path::Path;

use crate::model::{Clip, ClipId, Project, Sample};
use crate::theme::MIN_TRIM_DURATION_SECS;
use crate::timeline::{TrimDrag, TrimSide};
use crate::wav_loader;

pub fn append_wav_clip(project: &mut Project, sample: Sample, label: String) {
    let id = project.alloc_clip_id();
    let n = sample.data.len();
    project.clips.push(Clip {
        id,
        start_time_secs: 0.0,
        label,
        sample,
        trim_start: 0,
        trim_end: n,
        placement_preview: false,
    });
}

pub fn try_load_wav_clip(project: &mut Project, path: &Path) -> Result<(), String> {
    let sr = project.device_sample_rate;
    let sample = wav_loader::load_wav_mono_f32(path, sr)?;
    let label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clip")
        .to_string();
    append_wav_clip(project, sample, label);
    Ok(())
}

/// Split the clip under `playhead_secs` into two clips. Left clip keeps the original [`ClipId`].
pub fn split_clip_at_playhead(
    project: &mut Project,
    playhead_secs: f32,
) -> Result<ClipId, String> {
    let sr = project.device_sample_rate;
    let sr_f = sr as f32;
    let min_samples = ((MIN_TRIM_DURATION_SECS * sr_f).ceil() as usize).max(1);

    let idx = project.clips.iter().position(|clip| {
        let t0 = clip.start_time_secs;
        let t1 = t0 + clip.timeline_duration_secs(sr);
        playhead_secs > t0 && playhead_secs < t1
    });
    let Some(i) = idx else {
        return Err("Разрез: поставьте плейхед внутри клипа.".into());
    };

    let clip = project.clips.remove(i);
    let kept_left_id = clip.id;
    let mut split_at =
        clip.trim_start + ((playhead_secs - clip.start_time_secs) * sr_f).round() as usize;
    split_at = split_at
        .max(clip.trim_start + min_samples)
        .min(clip.trim_end.saturating_sub(min_samples));
    if split_at <= clip.trim_start || split_at >= clip.trim_end {
        project.clips.insert(i, clip);
        return Err("Нельзя разрезать: слишком короткий фрагмент.".into());
    }

    let split_time = clip.start_time_secs + (split_at - clip.trim_start) as f32 / sr_f;
    let right_id = project.alloc_clip_id();

    let left = Clip {
        id: clip.id,
        start_time_secs: clip.start_time_secs,
        label: clip.label.clone(),
        sample: clip.sample.clone(),
        trim_start: clip.trim_start,
        trim_end: split_at,
        placement_preview: false,
    };
    let right = Clip {
        id: right_id,
        start_time_secs: split_time,
        label: clip.label,
        sample: clip.sample,
        trim_start: split_at,
        trim_end: clip.trim_end,
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
    let ds_samples = ((dx_px / pps) * sr).round() as i64;
    if ds_samples == 0 {
        return false;
    }
    let Some(idx) = project.clip_index(drag.clip_id) else {
        return false;
    };
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
            clip.start_time_secs += actual as f32 / sr;
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
    let sr = project.device_sample_rate;
    let mut v: Vec<(f32, f32)> = project
        .clips
        .iter()
        .enumerate()
        .filter(|&(i, _)| i != exclude_idx)
        .map(|(_, c)| {
            let d = c.timeline_duration_secs(sr);
            (c.start_time_secs, c.start_time_secs + d)
        })
        .collect();
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
    let sr = project.device_sample_rate;
    let d = project.clips[idx].timeline_duration_secs(sr);
    let ranges = feasible_start_ranges(project, idx, d);
    clamp_start_to_feasible_ranges(proposed_start, &ranges, hint_old)
}

/// True if clip `idx` overlaps any other clip on the timeline (positive-length intersection).
pub fn clip_overlaps_others(project: &Project, idx: usize) -> bool {
    let sr = project.device_sample_rate;
    let c = &project.clips[idx];
    let t0 = c.start_time_secs;
    let t1 = t0 + c.timeline_duration_secs(sr);
    for (j, o) in project.clips.iter().enumerate() {
        if j == idx {
            continue;
        }
        let o0 = o.start_time_secs;
        let o1 = o0 + o.timeline_duration_secs(sr);
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
    let (start, label, sample, trim_start, trim_end) = {
        let orig = project.clips.get(idx)?;
        (
            orig.start_time_secs,
            orig.label.clone(),
            orig.sample.clone(),
            orig.trim_start,
            orig.trim_end,
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
