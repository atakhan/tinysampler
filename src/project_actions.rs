//! Mutations of [`crate::model::Project`] (invariants for tracks, pads, sequences, notes).

use std::path::Path;

use crate::model::{
    CueMarker, NoteId, PadEdge, PadMarker, PadNote, Project, Sample, SeqClip, SeqId, Track, TrackId,
};
use crate::time::{self, default_seq_duration_secs, grid_step_secs, snap_time_floor, snap_time_round};
use crate::wav_loader;

pub fn add_track(project: &mut Project) -> TrackId {
    let id = project.alloc_track_id();
    let n = project.tracks.len() + 1;
    project.tracks.push(Track {
        id,
        name: format!("Трек {n}"),
        pitch_semitones: 0,
        sample: None,
        sample_label: String::new(),
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
    track.pad_markers.clear();
    track.sample = Some(sample);
    track.sample_label = label;
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

pub fn load_audio_file(path: &Path) -> Result<(Sample, String), String> {
    let sample = wav_loader::load_audio_mono_f32(path)?;
    let label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("clip")
        .to_string();
    Ok((sample, label))
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
    let step = grid_step_secs(project.tempo_bpm);
    let start = snap_time_round(start_time_secs, step).clamp(0.0, (seq.duration_secs - step).max(0.0));
    let Some(note) = project.notes.iter_mut().find(|n| n.id == id) else {
        return false;
    };
    if note.start_time_secs == start && note.slot == slot {
        return false;
    }
    note.start_time_secs = start;
    note.slot = slot;
    true
}

pub fn resize_pad_note(project: &mut Project, id: NoteId, end_time_secs: f32) -> bool {
    let step = grid_step_secs(project.tempo_bpm).max(1e-4);
    let Some(note) = project.notes.iter().find(|n| n.id == id).copied() else {
        return false;
    };
    let max_end = project
        .seq_clips
        .iter()
        .find(|s| s.id == note.seq_id)
        .map(|s| s.duration_secs)
        .unwrap_or(note.end_time_secs());
    let Some(note) = project.notes.iter_mut().find(|n| n.id == id) else {
        return false;
    };
    let end = snap_time_round(end_time_secs, step)
        .max(note.start_time_secs + step)
        .min(max_end);
    let duration = (end - note.start_time_secs).max(step);
    if (note.duration_secs - duration).abs() < 1e-6 {
        return false;
    }
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
    let step = grid_step_secs(project.tempo_bpm);
    let start = snap_time_round(start_time_secs, step).max(0.0);
    let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == id) else {
        return false;
    };
    if (seq.start_time_secs - start).abs() < 1e-6 {
        return false;
    }
    seq.start_time_secs = start;
    true
}

pub fn resize_seq_end(project: &mut Project, id: SeqId, end_time_secs: f32) -> bool {
    let step = grid_step_secs(project.tempo_bpm).max(1e-4);
    let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == id) else {
        return false;
    };
    let end = snap_time_round(end_time_secs, step).max(seq.start_time_secs + step);
    let duration = (end - seq.start_time_secs).max(step);
    if (seq.duration_secs - duration).abs() < 1e-6 {
        return false;
    }
    seq.duration_secs = duration;
    true
}

pub fn resize_seq_start(project: &mut Project, id: SeqId, start_time_secs: f32) -> bool {
    let step = grid_step_secs(project.tempo_bpm).max(1e-4);
    let Some(seq) = project.seq_clips.iter().find(|s| s.id == id).copied() else {
        return false;
    };
    let end = seq.end_time_secs();
    let start = snap_time_round(start_time_secs, step)
        .max(0.0)
        .min(end - step);
    let delta = start - seq.start_time_secs;
    if delta.abs() < 1e-6 {
        return false;
    }
    let Some(seq) = project.seq_clips.iter_mut().find(|s| s.id == id) else {
        return false;
    };
    seq.start_time_secs = start;
    seq.duration_secs = (end - start).max(step);
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
    fn nudge_tempo_is_project_source_of_truth() {
        let mut p = Project::empty();
        p.tempo_bpm = 120.0;
        assert!(nudge_tempo(&mut p, -10.0));
        assert!((p.tempo_bpm - 110.0).abs() < 1e-4);
    }
}
