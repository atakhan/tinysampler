//! Mutations of [`crate::model::Project`] (invariants for tracks, pads, sequences, notes).

use std::path::Path;
use std::sync::Arc;

use crate::model::{
    CueMarker, NoteId, PadEdge, PadMarker, PadNote, Project, Sample, SampleSlice, SeqClip, SeqId,
    Track, TrackId,
};
use crate::time::{self, default_seq_duration_secs, grid_step_secs, snap_time_floor};
use crate::wav_loader;

pub fn rename_track(project: &mut Project, track_id: TrackId, name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return false;
    }
    let Some(track) = project.track_mut(track_id) else {
        return false;
    };
    if track.name == trimmed {
        return false;
    }
    track.name = trimmed.to_string();
    true
}

pub fn delete_track(project: &mut Project, track_id: TrackId) -> bool {
    if project.track_index(track_id).is_none() {
        return false;
    }
    let seq_ids: Vec<SeqId> = project
        .seq_clips
        .iter()
        .filter(|s| s.track_id == track_id)
        .map(|s| s.id)
        .collect();
    project.notes.retain(|n| !seq_ids.contains(&n.seq_id));
    project.seq_clips.retain(|s| s.track_id != track_id);
    project.markers.retain(|m| m.track_id != track_id);
    project.tracks.retain(|t| t.id != track_id);
    if project.sampler_preview.track_id == track_id {
        project.sampler_preview.playing = false;
        project.sampler_preview.end_secs = None;
    }
    true
}

pub fn add_track(project: &mut Project) -> TrackId {
    let id = project.alloc_track_id();
    let n = project.tracks.len() + 1;
    project.tracks.push(Track {
        id,
        name: format!("Трек {n}"),
        pitch_semitones: 0,
        sample: None,
        sample_label: String::new(),
        sample_slices: Vec::new(),
        pad_markers: Vec::new(),
    });
    let duration = default_seq_duration_secs(project.tempo_bpm);
    let seq_id = project.alloc_seq_id();
    project.seq_clips.push(SeqClip {
        id: seq_id,
        track_id: id,
        start_time_secs: 0.0,
        duration_secs: duration,
    });
    id
}

pub fn set_track_sample(project: &mut Project, track_id: TrackId, sample: Sample, label: String) {
    let Some(track) = project.track_mut(track_id) else {
        return;
    };
    let end = sample.data.len();
    track.pad_markers.clear();
    track.sample = Some(sample);
    track.sample_label = label.clone();
    track.sample_slices = vec![SampleSlice {
        label,
        start_index: 0,
        end_index: end,
    }];
    let drop_seq: Vec<SeqId> = project
        .seq_clips
        .iter()
        .filter(|s| s.track_id == track_id)
        .map(|s| s.id)
        .collect();
    project.notes.retain(|n| !drop_seq.contains(&n.seq_id));
}

pub fn append_audio_clip(project: &mut Project, sample: Sample, label: String) -> TrackId {
    let track_id = add_track(project);
    set_track_sample(project, track_id, sample, label);
    track_id
}

/// A file appended onto an existing instrument buffer.
pub struct AppendedSample {
    pub start_index: usize,
    pub end_index: usize,
    pub resampled: bool,
}

/// Place `sample` after the audio already on the track. Pads and notes stay.
pub fn append_track_sample(
    project: &mut Project,
    track_id: TrackId,
    sample: Sample,
    label: String,
) -> Result<AppendedSample, String> {
    let (dst_rate, resampled, start, end, data) = {
        let Some(track) = project.track(track_id) else {
            return Err("трек не найден".into());
        };
        let Some(existing) = track.sample.as_ref() else {
            return Err("на треке ещё нет сэмпла".into());
        };
        let dst_rate = existing.rate();
        let resampled = sample.rate() != dst_rate;
        let incoming = wav_loader::resample_mono(sample.data.as_slice(), sample.rate(), dst_rate)?;
        let start = existing.data.len();
        let end = start.saturating_add(incoming.len());
        if end > wav_loader::MAX_MONO_SAMPLES {
            return Err("сэмпл слишком длинный".into());
        }
        let mut data = existing.data.as_ref().clone();
        data.extend_from_slice(&incoming);
        (dst_rate, resampled, start, end, data)
    };
    let peaks = crate::waveform::PeakPyramid::build(&data);
    let Some(track) = project.track_mut(track_id) else {
        return Err("трек не найден".into());
    };
    track.sample = Some(Sample::new_mono(Arc::new(data), Arc::new(peaks), dst_rate));
    track.sample_slices.push(SampleSlice {
        label,
        start_index: start,
        end_index: end,
    });
    track.sample_label = sample_slices_label(&track.sample_slices);
    Ok(AppendedSample {
        start_index: start,
        end_index: end,
        resampled,
    })
}

/// Bind the next free pad to an exact sample range. `None` when every pad is taken.
pub fn bind_next_pad_range(
    project: &mut Project,
    track_id: TrackId,
    start_index: usize,
    end_index: usize,
) -> Option<u8> {
    if end_index <= start_index {
        return None;
    }
    let track = project.track_mut(track_id)?;
    let slot = (0u8..PAD_SLOT_COUNT).find(|&s| !track.pad_markers.iter().any(|m| m.slot == s))?;
    track.pad_markers.push(PadMarker {
        slot,
        start_index,
        end_index,
    });
    Some(slot)
}

/// Move one source file to another slot. The pieces stay back to back, with no overlap.
pub fn reorder_track_slice(
    project: &mut Project,
    track_id: TrackId,
    from: usize,
    to: usize,
) -> bool {
    let Some(track) = project.track(track_id) else {
        return false;
    };
    let n = track.sample_slices.len();
    if from >= n || to >= n || from == to {
        return false;
    }
    let mut order: Vec<usize> = (0..n).collect();
    let item = order.remove(from);
    order.insert(to, item);
    rebuild_slices(project, track_id, &order)
}

/// Drop one source file and close the gap. Pads that lived only inside it go away.
pub fn delete_track_slice(project: &mut Project, track_id: TrackId, index: usize) -> bool {
    let Some(track) = project.track(track_id) else {
        return false;
    };
    let n = track.sample_slices.len();
    if index >= n {
        return false;
    }
    if n == 1 {
        return clear_track_sample(project, track_id);
    }
    let order: Vec<usize> = (0..n).filter(|&i| i != index).collect();
    rebuild_slices(project, track_id, &order)
}

/// Where a sample index lands after [`reorder_track_slice`].
pub fn sample_index_after_slice_reorder(
    slices: &[SampleSlice],
    from: usize,
    to: usize,
    index: usize,
) -> usize {
    let n = slices.len();
    if from >= n || to >= n || from == to {
        return index;
    }
    let mut order: Vec<usize> = (0..n).collect();
    let item = order.remove(from);
    order.insert(to, item);
    map_index_through_order(slices, &order, index, false).unwrap_or(0)
}

/// Where a sample index lands after [`delete_track_slice`]. `None` when no audio remains.
pub fn sample_index_after_slice_delete(
    slices: &[SampleSlice],
    index: usize,
    sample_index: usize,
) -> Option<usize> {
    let n = slices.len();
    if index >= n {
        return Some(sample_index);
    }
    if n == 1 {
        return None;
    }
    let order: Vec<usize> = (0..n).filter(|&i| i != index).collect();
    map_index_through_order(slices, &order, sample_index, false)
}

fn clear_track_sample(project: &mut Project, track_id: TrackId) -> bool {
    let Some(track) = project.track_mut(track_id) else {
        return false;
    };
    let slots: Vec<u8> = track.pad_markers.iter().map(|m| m.slot).collect();
    track.sample = None;
    track.sample_label.clear();
    track.sample_slices.clear();
    track.pad_markers.clear();
    for slot in slots {
        forget_pad_notes(project, track_id, slot);
    }
    true
}

fn rebuild_slices(project: &mut Project, track_id: TrackId, order: &[usize]) -> bool {
    let Some(track) = project.track(track_id) else {
        return false;
    };
    let Some(sample) = track.sample.as_ref() else {
        return false;
    };
    let old = track.sample_slices.clone();
    if order.len() > old.len() || order.iter().any(|&i| i >= old.len()) {
        return false;
    }
    let rate = sample.rate();
    let data = sample.data.clone();
    let pads = track.pad_markers.clone();
    let (new_data, new_slices, origin) = place_slice_order(data.as_slice(), &old, order);
    let mut kept = Vec::new();
    let mut dropped = Vec::new();
    for pad in pads {
        match (
            map_index_with_origin(&old, &origin, pad.start_index, false),
            map_index_with_origin(&old, &origin, pad.end_index, true),
        ) {
            (Some(start), Some(end)) if end > start => {
                kept.push(PadMarker {
                    slot: pad.slot,
                    start_index: start,
                    end_index: end,
                });
            }
            _ => dropped.push(pad.slot),
        }
    }
    let peaks = crate::waveform::PeakPyramid::build(&new_data);
    let Some(track) = project.track_mut(track_id) else {
        return false;
    };
    track.sample = Some(Sample::new_mono(Arc::new(new_data), Arc::new(peaks), rate));
    track.sample_slices = new_slices;
    track.sample_label = sample_slices_label(&track.sample_slices);
    track.pad_markers = kept;
    for slot in dropped {
        forget_pad_notes(project, track_id, slot);
    }
    true
}

fn place_slice_order(
    data: &[f32],
    old: &[SampleSlice],
    order: &[usize],
) -> (Vec<f32>, Vec<SampleSlice>, Vec<Option<(usize, usize)>>) {
    let mut origin = vec![None; old.len()];
    let mut new_data = Vec::new();
    let mut new_slices = Vec::new();
    for &old_i in order {
        let slice = &old[old_i];
        let start = slice.start_index.min(data.len());
        let end = slice.end_index.min(data.len()).max(start);
        let new_start = new_data.len();
        new_data.extend_from_slice(&data[start..end]);
        let len = end - start;
        origin[old_i] = Some((new_start, len));
        new_slices.push(SampleSlice {
            label: slice.label.clone(),
            start_index: new_start,
            end_index: new_start + len,
        });
    }
    (new_data, new_slices, origin)
}

fn map_index_through_order(
    old: &[SampleSlice],
    order: &[usize],
    index: usize,
    exclusive_end: bool,
) -> Option<usize> {
    let mut origin = vec![None; old.len()];
    let mut cursor = 0usize;
    for &old_i in order {
        let len = old
            .get(old_i)
            .map(|s| s.end_index.saturating_sub(s.start_index))
            .unwrap_or(0);
        origin[old_i] = Some((cursor, len));
        cursor += len;
    }
    map_index_with_origin(old, &origin, index, exclusive_end)
}

fn map_index_with_origin(
    old: &[SampleSlice],
    origin: &[Option<(usize, usize)>],
    index: usize,
    exclusive_end: bool,
) -> Option<usize> {
    if old.is_empty() {
        return Some(0);
    }
    let (slice_i, mut off) = if exclusive_end && index > 0 {
        let (i, inner) = locate_slice(old, index - 1);
        (i, inner + 1)
    } else {
        locate_slice(old, index)
    };
    let (new_start, len) = origin.get(slice_i).copied().flatten()?;
    off = off.min(len);
    Some(new_start + off)
}

fn locate_slice(slices: &[SampleSlice], index: usize) -> (usize, usize) {
    for (i, slice) in slices.iter().enumerate() {
        if index < slice.end_index {
            return (i, index.saturating_sub(slice.start_index));
        }
    }
    let last = slices.len().saturating_sub(1);
    let len = slices
        .get(last)
        .map(|s| s.end_index.saturating_sub(s.start_index))
        .unwrap_or(0);
    (last, len)
}

fn forget_pad_notes(project: &mut Project, track_id: TrackId, slot: u8) {
    let drop_seq: Vec<SeqId> = project
        .seq_clips
        .iter()
        .filter(|s| s.track_id == track_id)
        .map(|s| s.id)
        .collect();
    project
        .notes
        .retain(|note| note.slot != slot || !drop_seq.contains(&note.seq_id));
}

fn sample_slices_label(slices: &[SampleSlice]) -> String {
    if slices.len() <= 1 {
        return slices
            .first()
            .map(|s| s.label.clone())
            .unwrap_or_default();
    }
    let mut label = String::new();
    for (i, slice) in slices.iter().enumerate() {
        if i > 0 {
            label.push_str(" + ");
        }
        if label.len() > 48 {
            label.push('…');
            break;
        }
        label.push_str(&slice.label);
    }
    label
}

pub fn load_audio_file(path: &Path) -> Result<(Sample, String), String> {
    let sample = wav_loader::load_audio_mono_f32(path)?;
    let label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clip")
        .to_string();
    Ok((sample, label))
}

pub fn set_tempo(project: &mut Project, bpm: f32) -> bool {
    if !bpm.is_finite() {
        return false;
    }
    let next = ((bpm * 10.0).round() / 10.0).clamp(20.0, 400.0);
    if (next - project.tempo_bpm).abs() < 1e-4 {
        return false;
    }
    project.tempo_bpm = next;
    true
}

pub fn nudge_tempo(project: &mut Project, delta: f32) -> bool {
    let next = (project.tempo_bpm + delta).clamp(20.0, 400.0);
    if (next - project.tempo_bpm).abs() < 1e-6 {
        return false;
    }
    project.tempo_bpm = next;
    true
}

pub const MARKER_SLOT_MAX: u8 = 9;

pub fn try_place_marker(project: &mut Project, time_secs: f32, track_id: TrackId) -> Option<u8> {
    if project.track(track_id).is_none() {
        return None;
    }
    let slot = (1u8..=MARKER_SLOT_MAX).find(|&s| {
        !project
            .markers
            .iter()
            .any(|m| m.track_id == track_id && m.slot == s)
    })?;
    let time_secs = time_secs.max(0.0);
    if !time_secs.is_finite() {
        return None;
    }
    project.markers.push(CueMarker {
        slot,
        time_secs,
        track_id,
    });
    Some(slot)
}

pub fn marker_time(project: &Project, slot: u8, track_id: TrackId) -> Option<f32> {
    project
        .markers
        .iter()
        .find(|m| m.slot == slot && m.track_id == track_id)
        .map(|m| m.time_secs)
}

pub fn move_marker(project: &mut Project, slot: u8, track_id: TrackId, time_secs: f32) -> bool {
    let Some(m) = project
        .markers
        .iter_mut()
        .find(|m| m.slot == slot && m.track_id == track_id)
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

pub fn delete_marker(project: &mut Project, slot: u8, track_id: TrackId) -> bool {
    let n = project.markers.len();
    project
        .markers
        .retain(|m| !(m.slot == slot && m.track_id == track_id));
    project.markers.len() != n
}

pub const PAD_SLOT_COUNT: u8 = 16;
pub const PAD_MIN_SAMPLES: usize = 8;

pub fn pad_min_len(sample_len: usize) -> usize {
    PAD_MIN_SAMPLES.min(sample_len).max(1)
}

/// Default chop length: one beat at the project tempo, at least [`PAD_MIN_SAMPLES`].
pub fn pad_default_len(sample_rate: u32, tempo_bpm: f32) -> usize {
    let beat = (time::beat_secs(tempo_bpm) * sample_rate.max(1) as f32).round() as usize;
    beat.max(PAD_MIN_SAMPLES)
}

pub fn pad_range_from_start(
    start: usize,
    sample_len: usize,
    default_len: usize,
) -> Option<(usize, usize)> {
    if sample_len == 0 {
        return None;
    }
    let min_len = pad_min_len(sample_len);
    let want = default_len.max(min_len).min(sample_len);
    let s = start.min(sample_len - 1);
    let e = (s + want).min(sample_len);
    if e - s >= min_len {
        return Some((s, e));
    }
    let s2 = sample_len.saturating_sub(min_len);
    Some((s2, sample_len))
}

pub fn try_place_pad_marker(
    project: &mut Project,
    track_id: TrackId,
    sample_index: usize,
    sample_len: usize,
    default_len: usize,
) -> Option<u8> {
    let (start_index, end_index) = pad_range_from_start(sample_index, sample_len, default_len)?;
    let track = project.track_mut(track_id)?;
    let slot = (0u8..PAD_SLOT_COUNT).find(|&s| !track.pad_markers.iter().any(|m| m.slot == s))?;
    track.pad_markers.push(PadMarker {
        slot,
        start_index,
        end_index,
    });
    Some(slot)
}

pub fn bind_pad_marker(
    project: &mut Project,
    track_id: TrackId,
    slot: u8,
    sample_index: usize,
    sample_len: usize,
    default_len: usize,
) -> bool {
    if slot >= PAD_SLOT_COUNT {
        return false;
    }
    let Some((start_index, end_index)) = pad_range_from_start(sample_index, sample_len, default_len)
    else {
        return false;
    };
    let Some(track) = project.track_mut(track_id) else {
        return false;
    };
    if track.pad_markers.iter().any(|m| m.slot == slot) {
        return false;
    }
    track.pad_markers.push(PadMarker {
        slot,
        start_index,
        end_index,
    });
    true
}

pub fn pad_marker_sample(project: &Project, track_id: TrackId, slot: u8) -> Option<usize> {
    pad_marker_range(project, track_id, slot).map(|(start, _)| start)
}

pub fn pad_marker_range(project: &Project, track_id: TrackId, slot: u8) -> Option<(usize, usize)> {
    project
        .track(track_id)?
        .pad_markers
        .iter()
        .find(|m| m.slot == slot)
        .map(|m| (m.start_index, m.end_index))
}

pub fn move_pad_edge(
    project: &mut Project,
    track_id: TrackId,
    slot: u8,
    edge: PadEdge,
    sample_index: usize,
    sample_len: usize,
) -> bool {
    if sample_len == 0 {
        return false;
    }
    let min_len = pad_min_len(sample_len);
    let Some(m) = project
        .track_mut(track_id)
        .and_then(|t| t.pad_markers.iter_mut().find(|m| m.slot == slot))
    else {
        return false;
    };
    match edge {
        PadEdge::Start => {
            let max_start = m.end_index.saturating_sub(min_len);
            let s = sample_index.min(max_start);
            if s == m.start_index {
                return false;
            }
            m.start_index = s;
        }
        PadEdge::End => {
            let min_end = (m.start_index + min_len).min(sample_len);
            let e = sample_index.clamp(min_end, sample_len);
            if e == m.end_index {
                return false;
            }
            m.end_index = e;
        }
    }
    true
}

pub fn delete_pad_marker(project: &mut Project, track_id: TrackId, slot: u8) -> bool {
    let Some(track) = project.track_mut(track_id) else {
        return false;
    };
    let n = track.pad_markers.len();
    track.pad_markers.retain(|m| m.slot != slot);
    if track.pad_markers.len() == n {
        return false;
    }
    let drop_seq: Vec<SeqId> = project
        .seq_clips
        .iter()
        .filter(|s| s.track_id == track_id)
        .map(|s| s.id)
        .collect();
    project
        .notes
        .retain(|note| note.slot != slot || !drop_seq.contains(&note.seq_id));
    true
}

pub fn sample_index_to_preview_secs(sample_index: usize, sample_rate: u32, speed: f32) -> f32 {
    sample_index as f32 / (sample_rate.max(1) as f32 * speed.max(0.05))
}

pub fn pad_chop_sounding_secs(project: &Project, track_id: TrackId, slot: u8) -> Option<f32> {
    let (start, end) = pad_marker_range(project, track_id, slot)?;
    let speed = project.track_speed(track_id).max(0.05);
    let rate = project
        .track(track_id)
        .and_then(|t| t.sample.as_ref())
        .map(|s| s.rate() as f32)
        .unwrap_or(1.0);
    Some((end.saturating_sub(start) as f32) / (rate * speed))
}

fn default_note_duration(project: &Project, track_id: TrackId, slot: u8) -> f32 {
    let step = grid_step_secs(project.tempo_bpm).max(1e-4);
    let chop = pad_chop_sounding_secs(project, track_id, slot).unwrap_or(step);
    let steps = (chop / step).ceil().max(1.0);
    steps * step
}

/// Place a piano-roll note for a bound pad. Duration follows the chop, snapped to 16ths.
pub fn place_pad_note(
    project: &mut Project,
    seq_id: SeqId,
    slot: u8,
    start_time_secs: f32,
) -> Option<NoteId> {
    if slot >= PAD_SLOT_COUNT {
        return None;
    }
    let seq = *project.seq_clips.iter().find(|s| s.id == seq_id)?;
    pad_marker_range(project, seq.track_id, slot)?;
    let step = grid_step_secs(project.tempo_bpm);
    let start = snap_time_floor(start_time_secs, step).max(0.0);
    let duration = default_note_duration(project, seq.track_id, slot)
        .min((seq.duration_secs - start).max(step));
    if start >= seq.duration_secs {
        return None;
    }
    let id = project.alloc_note_id();
    project.notes.push(PadNote {
        id,
        seq_id,
        slot,
        start_time_secs: start,
        duration_secs: duration,
    });
    Some(id)
}

pub fn move_pad_note(project: &mut Project, id: NoteId, start_time_secs: f32, slot: u8) -> bool {
    if slot >= PAD_SLOT_COUNT {
        return false;
    }
    let Some(note) = project.notes.iter().find(|n| n.id == id).copied() else {
        return false;
    };
    let Some(seq) = project.seq_clips.iter().find(|s| s.id == note.seq_id).copied() else {
        return false;
    };
    if pad_marker_range(project, seq.track_id, slot).is_none() {
        return false;
    }
    let start = start_time_secs.max(0.0);
    let (seq_id, end) = {
        let Some(note) = project.notes.iter_mut().find(|n| n.id == id) else {
            return false;
        };
        if (note.start_time_secs - start).abs() < 1e-4 && note.slot == slot {
            return false;
        }
        note.start_time_secs = start;
        note.slot = slot;
        (note.seq_id, start + note.duration_secs)
    };
    if let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == seq_id) {
        if seq.duration_secs < end {
            seq.duration_secs = end;
        }
    }
    true
}

/// Copy `id` to `start_time_secs` / `slot`. The source note is left where it was.
pub fn duplicate_pad_note(
    project: &mut Project,
    id: NoteId,
    start_time_secs: f32,
    slot: u8,
) -> Option<NoteId> {
    if slot >= PAD_SLOT_COUNT {
        return None;
    }
    let note = project.notes.iter().find(|n| n.id == id).copied()?;
    let seq = project.seq_clips.iter().find(|s| s.id == note.seq_id).copied()?;
    if pad_marker_range(project, seq.track_id, slot).is_none() {
        return None;
    }
    let start = start_time_secs.max(0.0);
    let new_id = project.alloc_note_id();
    let end = start + note.duration_secs;
    project.notes.push(PadNote {
        id: new_id,
        seq_id: note.seq_id,
        slot,
        start_time_secs: start,
        duration_secs: note.duration_secs,
    });
    if let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == note.seq_id) {
        if seq.duration_secs < end {
            seq.duration_secs = end;
        }
    }
    Some(new_id)
}

pub fn resize_pad_note(project: &mut Project, id: NoteId, end_time_secs: f32) -> bool {
    let Some(note) = project.notes.iter_mut().find(|n| n.id == id) else {
        return false;
    };
    let start = note.start_time_secs;
    let end = end_time_secs.max(start + MIN_NOTE_DURATION_SECS);
    let duration = end - start;
    if (note.duration_secs - duration).abs() < 1e-4 {
        return false;
    }
    let seq_id = note.seq_id;
    note.duration_secs = duration;
    let grown = end;
    if let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == seq_id) {
        if seq.duration_secs < grown {
            seq.duration_secs = grown;
        }
    }
    true
}

/// Move the start and keep the end where it was, so the length changes from the left.
pub fn resize_pad_note_start(project: &mut Project, id: NoteId, start_time_secs: f32) -> bool {
    let Some(note) = project.notes.iter_mut().find(|n| n.id == id) else {
        return false;
    };
    let end = note.end_time_secs();
    let start = start_time_secs
        .max(0.0)
        .min(end - MIN_NOTE_DURATION_SECS);
    let duration = end - start;
    if (note.start_time_secs - start).abs() < 1e-4 && (note.duration_secs - duration).abs() < 1e-4 {
        return false;
    }
    note.start_time_secs = start;
    note.duration_secs = duration;
    true
}

pub fn delete_pad_note(project: &mut Project, id: NoteId) -> bool {
    let n = project.notes.len();
    project.notes.retain(|note| note.id != id);
    project.notes.len() != n
}

pub fn seq_clip_on_track(project: &Project, track_id: TrackId) -> Option<SeqId> {
    project
        .seq_clips
        .iter()
        .find(|s| s.track_id == track_id)
        .map(|s| s.id)
}

pub fn add_seq_clip(project: &mut Project, track_id: TrackId, start_time_secs: f32) -> Option<SeqId> {
    if project.track(track_id).is_none() {
        return None;
    }
    let step = grid_step_secs(project.tempo_bpm);
    let start = snap_time_floor(start_time_secs, step);
    let duration = default_seq_duration_secs(project.tempo_bpm);
    let id = project.alloc_seq_id();
    project.seq_clips.push(SeqClip {
        id,
        track_id,
        start_time_secs: start,
        duration_secs: duration,
    });
    Some(id)
}

pub fn move_seq_clip(project: &mut Project, id: SeqId, start_time_secs: f32) -> bool {
    let start = start_time_secs.max(0.0);
    let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == id) else {
        return false;
    };
    if (seq.start_time_secs - start).abs() < 1e-4 {
        return false;
    }
    seq.start_time_secs = start;
    true
}

/// Copy a sausage and every note inside it. Note times stay relative to the clip.
pub fn duplicate_seq_clip(project: &mut Project, id: SeqId, start_time_secs: f32) -> Option<SeqId> {
    let seq = project.seq_clips.iter().find(|s| s.id == id).copied()?;
    let notes: Vec<_> = project
        .notes
        .iter()
        .filter(|n| n.seq_id == id)
        .copied()
        .collect();
    let new_id = project.alloc_seq_id();
    project.seq_clips.push(SeqClip {
        id: new_id,
        track_id: seq.track_id,
        start_time_secs: start_time_secs.max(0.0),
        duration_secs: seq.duration_secs,
    });
    for note in notes {
        let note_id = project.alloc_note_id();
        project.notes.push(PadNote {
            id: note_id,
            seq_id: new_id,
            ..note
        });
    }
    Some(new_id)
}

pub fn resize_seq_end(project: &mut Project, id: SeqId, end_time_secs: f32) -> bool {
    let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == id) else {
        return false;
    };
    let end = end_time_secs.max(seq.start_time_secs + MIN_SEQ_DURATION_SECS);
    let duration = end - seq.start_time_secs;
    if (seq.duration_secs - duration).abs() < 1e-4 {
        return false;
    }
    seq.duration_secs = duration;
    true
}

const MIN_SEQ_DURATION_SECS: f32 = 0.05;
const MIN_NOTE_DURATION_SECS: f32 = 0.001;

pub fn resize_seq_start(project: &mut Project, id: SeqId, start_time_secs: f32) -> bool {
    let Some(seq) = project.seq_clips.iter().find(|s| s.id == id).copied() else {
        return false;
    };
    let end = seq.end_time_secs();
    let start = start_time_secs
        .max(0.0)
        .min(end - MIN_SEQ_DURATION_SECS);
    let delta = start - seq.start_time_secs;
    if delta.abs() < 1e-4 {
        return false;
    }
    let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == id) else {
        return false;
    };
    seq.start_time_secs = start;
    seq.duration_secs = (end - start).max(MIN_SEQ_DURATION_SECS);
    for note in project.notes.iter_mut().filter(|n| n.seq_id == id) {
        note.start_time_secs -= delta;
    }
    project.notes.retain(|n| n.seq_id != id || n.end_time_secs() > 0.0);
    for note in project.notes.iter_mut().filter(|n| n.seq_id == id) {
        if note.start_time_secs < 0.0 {
            note.duration_secs += note.start_time_secs;
            note.start_time_secs = 0.0;
        }
    }
    true
}

pub fn delete_seq_clip(project: &mut Project, id: SeqId) -> bool {
    let n = project.seq_clips.len();
    project.seq_clips.retain(|s| s.id != id);
    if project.seq_clips.len() == n {
        return false;
    }
    project.notes.retain(|note| note.seq_id != id);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dummy_sample() -> Sample {
        let data = vec![0.0f32; 64];
        let peaks = crate::waveform::PeakPyramid::build(&data);
        Sample::new_mono(Arc::new(data), Arc::new(peaks), 48_000)
    }

    #[test]
    fn each_imported_clip_gets_the_next_track() {
        let mut p = Project::empty();
        let a = append_audio_clip(&mut p, dummy_sample(), "a".into());
        let b = append_audio_clip(&mut p, dummy_sample(), "b".into());
        let c = append_audio_clip(&mut p, dummy_sample(), "c".into());
        assert_eq!(p.tracks[0].id, a);
        assert_eq!(p.tracks[1].id, b);
        assert_eq!(p.tracks[2].id, c);
        assert_eq!(p.tracks.len(), 3);
        assert_eq!(p.track_count(), 3);
    }

    #[test]
    fn set_track_sample_replaces_existing() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "a".into());
        set_track_sample(&mut p, id, dummy_sample(), "b".into());
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].sample_label, "b");
        assert!(p.tracks[0].sample.is_some());
        assert_eq!(p.tracks[0].sample_slices.len(), 1);
        assert_eq!(p.tracks[0].sample_slices[0].label, "b");
    }

    #[test]
    fn append_track_sample_places_the_next_sound_after_the_current_one() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "kick.wav".into());
        assert!(bind_next_pad_range(&mut p, id, 0, 64).is_some());
        let added = append_track_sample(&mut p, id, dummy_sample(), "snare.wav".into()).unwrap();
        assert_eq!(added.start_index, 64);
        assert_eq!(added.end_index, 128);
        assert!(!added.resampled);
        let track = &p.tracks[0];
        assert_eq!(track.sample.as_ref().unwrap().data.len(), 128);
        assert_eq!(track.sample_slices.len(), 2);
        assert_eq!(track.sample_slices[1].label, "snare.wav");
        assert_eq!(track.pad_markers.len(), 1);
        let slot = bind_next_pad_range(&mut p, id, added.start_index, added.end_index);
        assert_eq!(slot, Some(1));
        assert_eq!(p.tracks[0].pad_markers[1].start_index, 64);
        assert_eq!(p.tracks[0].pad_markers[1].end_index, 128);
    }

    fn tone(n: usize, v: f32) -> Sample {
        let data = vec![v; n];
        let peaks = crate::waveform::PeakPyramid::build(&data);
        Sample::new_mono(Arc::new(data), Arc::new(peaks), 48_000)
    }

    #[test]
    fn reorder_keeps_files_back_to_back_and_moves_their_pads() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, tone(4, 0.25), "a.wav".into());
        append_track_sample(&mut p, id, tone(2, 0.5), "b.wav".into()).unwrap();
        append_track_sample(&mut p, id, tone(3, 0.75), "c.wav".into()).unwrap();
        assert!(bind_next_pad_range(&mut p, id, 4, 6).is_some());
        assert!(reorder_track_slice(&mut p, id, 2, 0));
        let track = &p.tracks[0];
        let data = track.sample.as_ref().unwrap().data.as_slice();
        assert_eq!(data, &[0.75, 0.75, 0.75, 0.25, 0.25, 0.25, 0.25, 0.5, 0.5]);
        assert_eq!(track.sample_slices[0].label, "c.wav");
        assert_eq!(track.sample_slices[0].end_index, 3);
        assert_eq!(track.sample_slices[1].start_index, 3);
        assert_eq!(track.sample_slices[2].start_index, 7);
        assert_eq!(track.sample_slices[2].end_index, data.len());
        let pad = track.pad_markers.iter().find(|m| m.slot == 0).unwrap();
        assert_eq!((pad.start_index, pad.end_index), (7, 9));
    }

    #[test]
    fn delete_slice_closes_the_gap_and_drops_a_pad_that_lived_inside_it() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, tone(4, 0.25), "a.wav".into());
        append_track_sample(&mut p, id, tone(2, 0.5), "b.wav".into()).unwrap();
        bind_next_pad_range(&mut p, id, 0, 4);
        bind_next_pad_range(&mut p, id, 4, 6);
        assert!(delete_track_slice(&mut p, id, 0));
        let track = &p.tracks[0];
        assert_eq!(track.sample.as_ref().unwrap().data.as_slice(), &[0.5, 0.5]);
        assert_eq!(track.sample_slices.len(), 1);
        assert_eq!(track.sample_slices[0].label, "b.wav");
        assert_eq!(track.pad_markers.len(), 1);
        assert_eq!(track.pad_markers[0].slot, 1);
        assert_eq!(
            (track.pad_markers[0].start_index, track.pad_markers[0].end_index),
            (0, 2)
        );
        assert!(delete_track_slice(&mut p, id, 0));
        assert!(p.tracks[0].sample.is_none());
        assert!(p.tracks[0].pad_markers.is_empty());
    }

    #[test]
    fn markers_fill_slots_1_to_9_then_stop() {
        let mut p = Project::empty();
        let t0 = add_track(&mut p);
        let t1 = add_track(&mut p);
        for i in 1u8..=9 {
            assert_eq!(try_place_marker(&mut p, i as f32, t0), Some(i));
        }
        assert_eq!(try_place_marker(&mut p, 99.0, t0), None);
        assert_eq!(try_place_marker(&mut p, 0.0, t1), Some(1));
        assert_eq!(p.markers.len(), 10);
        assert_eq!(marker_time(&p, 3, t0), Some(3.0));
        assert!(move_marker(&mut p, 3, t0, 1.5));
        assert_eq!(marker_time(&p, 3, t0), Some(1.5));
        assert!(delete_marker(&mut p, 3, t0));
        assert_eq!(marker_time(&p, 3, t0), None);
        assert_eq!(try_place_marker(&mut p, 0.0, t0), Some(3));
    }

    #[test]
    fn pad_markers_fill_16_slots_then_stop() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "a".into());
        for s in 0u8..16 {
            assert_eq!(try_place_pad_marker(&mut p, id, s as usize, 64, 8), Some(s));
        }
        assert_eq!(try_place_pad_marker(&mut p, id, 99, 64, 8), None);
        assert_eq!(pad_marker_range(&p, id, 3), Some((3, 11)));
        assert!(move_pad_edge(&mut p, id, 3, PadEdge::End, 20, 64));
        assert_eq!(pad_marker_range(&p, id, 3), Some((3, 20)));
        assert!(move_pad_edge(&mut p, id, 3, PadEdge::Start, 10, 64));
        assert_eq!(pad_marker_range(&p, id, 3), Some((10, 20)));
        assert!(move_pad_edge(&mut p, id, 3, PadEdge::Start, 18, 64));
        assert_eq!(pad_marker_range(&p, id, 3), Some((12, 20)));
        assert!(!move_pad_edge(&mut p, id, 3, PadEdge::Start, 18, 64));
        assert!(delete_pad_marker(&mut p, id, 3));
        assert_eq!(pad_marker_sample(&p, id, 3), None);
        assert_eq!(try_place_pad_marker(&mut p, id, 0, 64, 8), Some(3));
    }

    #[test]
    fn bind_pad_marker_keeps_slot_if_free() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "a".into());
        assert!(bind_pad_marker(&mut p, id, 8, 12, 64, 8));
        assert_eq!(pad_marker_range(&p, id, 8), Some((12, 20)));
        assert!(!bind_pad_marker(&mut p, id, 8, 20, 64, 8));
        assert_eq!(pad_marker_range(&p, id, 8), Some((12, 20)));
        assert!(bind_pad_marker(&mut p, id, 0, 1, 64, 8));
        assert_eq!(pad_marker_range(&p, id, 0), Some((1, 9)));
    }

    #[test]
    fn replacing_sample_clears_pad_markers() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "a".into());
        assert_eq!(try_place_pad_marker(&mut p, id, 4, 64, 8), Some(0));
        let seq = seq_clip_on_track(&p, id).unwrap();
        assert!(place_pad_note(&mut p, seq, 0, 0.0).is_some());
        assert_eq!(p.notes.len(), 1);
        set_track_sample(&mut p, id, dummy_sample(), "b".into());
        assert!(p.tracks[0].pad_markers.is_empty());
        assert!(p.notes.is_empty());
    }

    #[test]
    fn piano_roll_note_requires_bound_pad() {
        let mut p = Project::empty();
        p.tempo_bpm = 120.0;
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "a".into());
        let seq = seq_clip_on_track(&p, id).unwrap();
        assert!(place_pad_note(&mut p, seq, 0, 0.25).is_none());
        assert!(bind_pad_marker(&mut p, id, 0, 0, 64, 8));
        let note_id = place_pad_note(&mut p, seq, 0, 0.25).unwrap();
        let n = p.notes.iter().find(|n| n.id == note_id).unwrap();
        assert_eq!(n.slot, 0);
        assert_eq!(n.seq_id, seq);
        assert!(n.duration_secs > 0.0);
        assert!(move_pad_note(&mut p, note_id, 1.0, 0));
        assert!(resize_pad_note(&mut p, note_id, 2.0));
        assert!(delete_pad_note(&mut p, note_id));
        assert!(p.notes.is_empty());
    }

    #[test]
    fn seq_clip_move_resize_delete() {
        let mut p = Project::empty();
        p.tempo_bpm = 120.0;
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "a".into());
        assert!(bind_pad_marker(&mut p, id, 0, 0, 64, 8));
        let seq = seq_clip_on_track(&p, id).unwrap();
        let note_id = place_pad_note(&mut p, seq, 0, 0.5).unwrap();
        let note_start = p.notes.iter().find(|n| n.id == note_id).unwrap().start_time_secs;
        assert!(move_seq_clip(&mut p, seq, 1.0));
        assert!((p.seq_clips[0].start_time_secs - 1.0).abs() < 1e-4);
        assert!(
            (p.notes.iter().find(|n| n.id == note_id).unwrap().start_time_secs - note_start).abs()
                < 1e-4
        );
        let end = p.seq_clips[0].end_time_secs();
        assert!(resize_seq_end(&mut p, seq, end - 0.5));
        assert!(p.seq_clips[0].duration_secs < end - 1.0 + 0.6);
        let start = p.seq_clips[0].start_time_secs;
        assert!(resize_seq_start(&mut p, seq, start + 0.25));
        let shifted = p.notes.iter().find(|n| n.id == note_id).unwrap().start_time_secs;
        assert!((shifted - (note_start - 0.25)).abs() < 1e-3);
        assert!(delete_seq_clip(&mut p, seq));
        assert!(p.seq_clips.is_empty());
        assert!(p.notes.is_empty());
    }

    #[test]
    fn seq_and_note_resize_follow_the_pointer_inside_one_grid_step() {
        let mut p = Project::empty();
        p.tempo_bpm = 120.0;
        let id = add_track(&mut p);
        set_track_sample(&mut p, id, dummy_sample(), "a".into());
        assert!(bind_pad_marker(&mut p, id, 0, 0, 64, 8));
        let seq = seq_clip_on_track(&p, id).unwrap();
        let before = p.seq_clips[0].duration_secs;
        let end = p.seq_clips[0].end_time_secs();
        assert!(resize_seq_end(&mut p, seq, end + 0.03));
        assert!((p.seq_clips[0].duration_secs - before - 0.03).abs() < 1e-3);
        let note_id = place_pad_note(&mut p, seq, 0, 0.0).unwrap();
        let note_end = p.notes.iter().find(|n| n.id == note_id).unwrap().end_time_secs();
        assert!(resize_pad_note(&mut p, note_id, note_end + 0.03));
        let grown = p.notes.iter().find(|n| n.id == note_id).unwrap().end_time_secs();
        assert!((grown - note_end - 0.03).abs() < 1e-3);
        assert!(resize_pad_note(&mut p, note_id, 1.0));
        assert!(resize_pad_note_start(&mut p, note_id, 0.2));
        let note = p.notes.iter().find(|n| n.id == note_id).unwrap();
        assert!((note.start_time_secs - 0.2).abs() < 1e-3);
        assert!((note.end_time_secs() - 1.0).abs() < 1e-3);
        let original = note.start_time_secs;
        let duration = note.duration_secs;
        let copy = duplicate_pad_note(&mut p, note_id, 1.5, 0).unwrap();
        assert_ne!(copy, note_id);
        let source = p.notes.iter().find(|n| n.id == note_id).unwrap();
        assert!((source.start_time_secs - original).abs() < 1e-3);
        assert!((source.duration_secs - duration).abs() < 1e-3);
        let copied = p.notes.iter().find(|n| n.id == copy).unwrap();
        assert!((copied.start_time_secs - 1.5).abs() < 1e-3);
        assert!((copied.duration_secs - duration).abs() < 1e-3);
        let seq = p.notes.iter().find(|n| n.id == note_id).unwrap().seq_id;
        let original_start = p.seq_clips.iter().find(|s| s.id == seq).unwrap().start_time_secs;
        let copy_seq = duplicate_seq_clip(&mut p, seq, 3.0).unwrap();
        assert_ne!(copy_seq, seq);
        assert_eq!(p.notes.iter().filter(|n| n.seq_id == seq).count(), 2);
        assert_eq!(p.notes.iter().filter(|n| n.seq_id == copy_seq).count(), 2);
        assert!(
            (p.seq_clips.iter().find(|s| s.id == seq).unwrap().start_time_secs - original_start).abs()
                < 1e-3
        );
        assert!((p.seq_clips.iter().find(|s| s.id == copy_seq).unwrap().start_time_secs - 3.0).abs() < 1e-3);
    }

    #[test]
    fn nudge_tempo_is_project_source_of_truth() {
        let mut p = Project::empty();
        p.tempo_bpm = 120.0;
        assert!(nudge_tempo(&mut p, -10.0));
        assert!((p.tempo_bpm - 110.0).abs() < 1e-4);
    }

    #[test]
    fn set_tempo_rounds_and_clamps() {
        let mut p = Project::empty();
        assert!(set_tempo(&mut p, 97.44));
        assert!((p.tempo_bpm - 97.4).abs() < 1e-4);
        assert!(set_tempo(&mut p, 12.0));
        assert!((p.tempo_bpm - 20.0).abs() < 1e-4);
        assert!(!set_tempo(&mut p, f32::NAN));
    }

    #[test]
    fn rename_track_trims_and_rejects_empty() {
        let mut p = Project::empty();
        let id = add_track(&mut p);
        assert!(rename_track(&mut p, id, "  Кик  "));
        assert_eq!(p.tracks[0].name, "Кик");
        assert!(!rename_track(&mut p, id, "Кик"));
        assert!(!rename_track(&mut p, id, "   "));
        assert_eq!(p.tracks[0].name, "Кик");
    }

    #[test]
    fn delete_track_drops_clips_notes_and_markers() {
        let mut p = Project::empty();
        let a = add_track(&mut p);
        let b = add_track(&mut p);
        assert_eq!(try_place_marker(&mut p, 1.0, a), Some(1));
        set_track_sample(&mut p, a, dummy_sample(), "a".into());
        assert!(bind_pad_marker(&mut p, a, 0, 0, 64, 8));
        let seq = seq_clip_on_track(&p, a).unwrap();
        assert!(place_pad_note(&mut p, seq, 0, 0.0).is_some());
        assert!(delete_track(&mut p, a));
        assert_eq!(p.tracks.len(), 1);
        assert_eq!(p.tracks[0].id, b);
        assert!(p.seq_clips.iter().all(|s| s.track_id == b));
        assert!(p.markers.iter().all(|m| m.track_id == b));
        assert!(p.notes.is_empty());
        assert!(!delete_track(&mut p, a));
    }
}
