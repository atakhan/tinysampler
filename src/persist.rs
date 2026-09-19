//! On-disk projects: JSON metadata + unique mono WAV samples.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{Clip, ClipId, CueMarker, NoteId, PadMarker, PadNote, Project, SeqClip, SeqId, Track};
use crate::project_actions::{pad_default_len, pad_range_from_start};
use crate::wav_loader;
use crate::waveform::PeakPyramid;

const FORMAT: u32 = 1;

#[derive(Clone)]
pub struct DiskProject {
    pub id: u64,
    pub project: Project,
    /// `Arc` pointer of PCM → filename inside `samples/`, so we do not rewrite unchanged buffers.
    sample_files: HashMap<usize, String>,
}

impl DiskProject {
    pub fn new(id: u64, project: Project) -> Self {
        Self {
            id,
            project,
            sample_files: HashMap::new(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct LibraryFile {
    format: u32,
    next_project_id: u64,
}

#[derive(Serialize, Deserialize)]
struct ProjectFile {
    format: u32,
    id: u64,
    name: String,
    pcm_sample_rate: u32,
    tempo_bpm: f32,
    next_clip_id: u64,
    tracks: Vec<TrackFile>,
    clips: Vec<ClipFile>,
    markers: Vec<MarkerFile>,
    #[serde(default)]
    notes: Vec<NoteFile>,
    #[serde(default)]
    seq_clips: Vec<SeqClipFile>,
    #[serde(default)]
    next_note_id: u64,
    #[serde(default)]
    next_seq_id: u64,
}

#[derive(Serialize, Deserialize)]
struct TrackFile {
    name: String,
    pitch_semitones: i32,
    source_tempo_bpm: f32,
    #[serde(default)]
    pad_markers: Vec<PadMarkerFile>,
}

#[derive(Serialize, Deserialize)]
struct PadMarkerFile {
    slot: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sample_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end_index: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct ClipFile {
    id: u64,
    start_time_secs: f32,
    label: String,
    trim_start: usize,
    trim_end: usize,
    track_index: usize,
    #[serde(default)]
    sample: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MarkerFile {
    slot: u8,
    time_secs: f32,
    track_index: usize,
}

#[derive(Serialize, Deserialize)]
struct NoteFile {
    id: u64,
    #[serde(default)]
    seq_id: Option<u64>,
    #[serde(default)]
    track_index: Option<usize>,
    slot: u8,
    start_time_secs: f32,
    duration_secs: f32,
}

#[derive(Serialize, Deserialize)]
struct SeqClipFile {
    id: u64,
    track_index: usize,
    start_time_secs: f32,
    duration_secs: f32,
}

pub fn default_root() -> PathBuf {
    if let Ok(p) = std::env::var("TINY_SAMPLER_DATA_DIR") {
        return PathBuf::from(p);
    }
    if let Ok(p) = std::env::var("APPDATA") {
        return PathBuf::from(p).join("tinysampler");
    }
    if let Ok(p) = std::env::var("HOME") {
        return PathBuf::from(p)
            .join(".local")
            .join("share")
            .join("tinysampler");
    }
    PathBuf::from("tinysampler-data")
}

pub fn ensure_root(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root.join("projects")).map_err(|e| e.to_string())
}

fn projects_dir(root: &Path) -> PathBuf {
    root.join("projects")
}

fn project_dir(root: &Path, id: u64) -> PathBuf {
    projects_dir(root).join(format!("p{id}"))
}

fn index_path(root: &Path) -> PathBuf {
    root.join("library.json")
}

fn write_json_atomic(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, body).map_err(|e| e.to_string())?;
    if path.exists() {
        fs::remove_file(path).map_err(|e| e.to_string())?;
    }
    fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn save_index(root: &Path, next_project_id: u64) -> Result<(), String> {
    ensure_root(root)?;
    let file = LibraryFile {
        format: FORMAT,
        next_project_id,
    };
    let body = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    write_json_atomic(&index_path(root), &body)
}

pub fn save_project(root: &Path, entry: &mut DiskProject) -> Result<(), String> {
    ensure_root(root)?;
    let dir = project_dir(root, entry.id);
    let samples_dir = dir.join("samples");
    fs::create_dir_all(&samples_dir).map_err(|e| e.to_string())?;

    let pcm_rate = entry.project.device_sample_rate.max(1);
    let mut used_names: HashSet<String> = HashSet::new();
    let mut ptr_to_file: HashMap<usize, String> = HashMap::new();
    let mut clip_files: Vec<Option<String>> = Vec::with_capacity(entry.project.clips.len());

    for clip in &entry.project.clips {
        if clip.sample.data.is_empty() {
            clip_files.push(None);
            continue;
        }
        let ptr = std::sync::Arc::as_ptr(&clip.sample.data) as usize;
        if let Some(name) = ptr_to_file.get(&ptr) {
            used_names.insert(name.clone());
            clip_files.push(Some(name.clone()));
            continue;
        }
        if let Some(existing) = entry.sample_files.get(&ptr) {
            let path = samples_dir.join(existing);
            if path.is_file() {
                used_names.insert(existing.clone());
                ptr_to_file.insert(ptr, existing.clone());
                clip_files.push(Some(existing.clone()));
                continue;
            }
        }
        let name = next_sample_name(&samples_dir, &used_names);
        let path = samples_dir.join(&name);
        wav_loader::write_wav_f32_mono(&path, &clip.sample.data, pcm_rate)?;
        used_names.insert(name.clone());
        ptr_to_file.insert(ptr, name.clone());
        clip_files.push(Some(name));
    }

    entry.sample_files = ptr_to_file;

    if samples_dir.is_dir() {
        for ent in fs::read_dir(&samples_dir).map_err(|e| e.to_string())? {
            let ent = ent.map_err(|e| e.to_string())?;
            let path = ent.path();
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if name.ends_with(".wav") && !used_names.contains(name) {
                let _ = fs::remove_file(&path);
            }
        }
    }

    let file = ProjectFile {
        format: FORMAT,
        id: entry.id,
        name: entry.project.name.clone(),
        pcm_sample_rate: pcm_rate,
        tempo_bpm: entry.project.tempo_bpm,
        next_clip_id: entry.project.next_clip_id,
        next_note_id: entry.project.next_note_id,
        next_seq_id: entry.project.next_seq_id,
        tracks: entry
            .project
            .tracks
            .iter()
            .map(|t| TrackFile {
                name: t.name.clone(),
                pitch_semitones: t.pitch_semitones,
                source_tempo_bpm: t.source_tempo_bpm,
                pad_markers: t
                    .pad_markers
                    .iter()
                    .map(|m| PadMarkerFile {
                        slot: m.slot,
                        sample_index: None,
                        start_index: Some(m.start_index),
                        end_index: Some(m.end_index),
                    })
                    .collect(),
            })
            .collect(),
        clips: entry
            .project
            .clips
            .iter()
            .zip(clip_files)
            .map(|(c, sample)| ClipFile {
                id: c.id.0,
                start_time_secs: c.start_time_secs,
                label: c.label.clone(),
                trim_start: c.trim_start,
                trim_end: c.trim_end,
                track_index: c.track_index,
                sample,
            })
            .collect(),
        markers: entry
            .project
            .markers
            .iter()
            .map(|m| MarkerFile {
                slot: m.slot,
                time_secs: m.time_secs,
                track_index: m.track_index,
            })
            .collect(),
        notes: entry
            .project
            .notes
            .iter()
            .map(|n| NoteFile {
                id: n.id.0,
                seq_id: Some(n.seq_id.0),
                track_index: None,
                slot: n.slot,
                start_time_secs: n.start_time_secs,
                duration_secs: n.duration_secs,
            })
            .collect(),
        seq_clips: entry
            .project
            .seq_clips
            .iter()
            .map(|s| SeqClipFile {
                id: s.id.0,
                track_index: s.track_index,
                start_time_secs: s.start_time_secs,
                duration_secs: s.duration_secs,
            })
            .collect(),
    };
    let body = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    write_json_atomic(&dir.join("project.json"), &body)
}

fn next_sample_name(samples_dir: &Path, used: &HashSet<String>) -> String {
    let mut n = 1u32;
    loop {
        let name = format!("s{n}.wav");
        if !used.contains(&name) && !samples_dir.join(&name).exists() {
            return name;
        }
        n = n.saturating_add(1);
        if n == 0 {
            return format!("s{}.wav", used.len() + 1);
        }
    }
}

fn scale_index(idx: usize, src_rate: u32, dst_rate: u32, max: usize) -> usize {
    if src_rate == 0 || dst_rate == 0 || src_rate == dst_rate {
        return idx.min(max);
    }
    let scaled = (idx as f64 * dst_rate as f64 / src_rate as f64).round() as usize;
    scaled.min(max)
}

pub fn load_project(root: &Path, id: u64, device_sample_rate: u32) -> Result<DiskProject, String> {
    let dir = project_dir(root, id);
    let path = dir.join("project.json");
    let raw = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: ProjectFile = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if file.format != FORMAT {
        return Err(format!("неизвестный формат проекта {}", file.format));
    }
    let src_rate = file.pcm_sample_rate.max(1);
    let dst_rate = device_sample_rate.max(1);
    let samples_dir = dir.join("samples");

    let mut cache: HashMap<String, crate::model::Sample> = HashMap::new();
    let mut sample_files: HashMap<usize, String> = HashMap::new();
    let mut clips = Vec::with_capacity(file.clips.len());

    for c in file.clips {
        let sample = if let Some(name) = c.sample.as_ref() {
            if let Some(existing) = cache.get(name) {
                existing.clone()
            } else {
                let wav_path = samples_dir.join(name);
                let loaded = wav_loader::load_wav_mono_f32(&wav_path, dst_rate)?;
                cache.insert(name.clone(), loaded.clone());
                loaded
            }
        } else {
            let data = std::sync::Arc::new(Vec::new());
            crate::model::Sample::new_mono(data, std::sync::Arc::new(PeakPyramid::build(&[])))
        };
        let n = sample.data.len();
        let ptr = std::sync::Arc::as_ptr(&sample.data) as usize;
        if let Some(name) = c.sample.as_ref() {
            sample_files.insert(ptr, name.clone());
        }
        let trim_start = scale_index(c.trim_start, src_rate, dst_rate, n);
        let mut trim_end = scale_index(c.trim_end, src_rate, dst_rate, n);
        if trim_end < trim_start {
            trim_end = trim_start;
        }
        clips.push(Clip {
            id: ClipId(c.id),
            start_time_secs: c.start_time_secs.max(0.0),
            label: c.label,
            sample,
            trim_start,
            trim_end,
            track_index: c.track_index,
            placement_preview: false,
        });
    }

    let tracks: Vec<Track> = file
        .tracks
        .into_iter()
        .enumerate()
        .map(|(ti, t)| {
            let n = clips
                .iter()
                .find(|c| c.track_index == ti)
                .map(|c| c.sample.data.len())
                .unwrap_or(0);
            let max_i = n.saturating_sub(1);
            let mut seen = [false; 16];
            let default_len = pad_default_len(dst_rate, t.source_tempo_bpm);
            let pad_markers = if n == 0 {
                Vec::new()
            } else {
                t.pad_markers
                    .into_iter()
                    .filter_map(|m| {
                        let i = m.slot as usize;
                        if i >= 16 || seen[i] {
                            return None;
                        }
                        let start_raw = m.start_index.or(m.sample_index)?;
                        let start = scale_index(start_raw, src_rate, dst_rate, max_i);
                        let end = if let Some(end_raw) = m.end_index {
                            scale_index(end_raw, src_rate, dst_rate, n)
                        } else {
                            pad_range_from_start(start, n, default_len)?.1
                        };
                        let end = if end <= start {
                            pad_range_from_start(start, n, default_len)?.1
                        } else {
                            end
                        };
                        seen[i] = true;
                        Some(PadMarker {
                            slot: m.slot,
                            start_index: start,
                            end_index: end,
                        })
                    })
                    .collect()
            };
            Track {
                name: t.name,
                pitch_semitones: t.pitch_semitones.clamp(-24, 24),
                source_tempo_bpm: t.source_tempo_bpm.clamp(20.0, 400.0),
                pad_markers,
            }
        })
        .collect();

    let n_tracks = tracks.len();
    let mut next_seq_id = file.next_seq_id.max(1);
    let mut seen_seq = HashSet::new();
    let mut seq_clips: Vec<SeqClip> = Vec::new();
    for s in file.seq_clips {
        if s.track_index >= n_tracks || s.duration_secs <= 0.0 {
            continue;
        }
        let id = if s.id == 0 {
            let id = next_seq_id;
            next_seq_id = next_seq_id.saturating_add(1).max(1);
            id
        } else {
            if !seen_seq.insert(s.id) {
                continue;
            }
            next_seq_id = next_seq_id.max(s.id.saturating_add(1));
            s.id
        };
        seq_clips.push(SeqClip {
            id: SeqId(id),
            track_index: s.track_index,
            start_time_secs: s.start_time_secs.max(0.0),
            duration_secs: s.duration_secs,
        });
    }

    let mut seen_note_ids = HashSet::new();
    let mut notes: Vec<PadNote> = Vec::new();
    let mut orphan_by_track: Vec<(usize, NoteFile)> = Vec::new();
    for n in file.notes {
        if n.slot >= 16 || n.duration_secs <= 0.0 {
            continue;
        }
        if n.id != 0 && !seen_note_ids.insert(n.id) {
            continue;
        }
        if let Some(seq_id) = n.seq_id {
            if seq_clips.iter().any(|s| s.id.0 == seq_id) {
                notes.push(PadNote {
                    id: NoteId(n.id),
                    seq_id: SeqId(seq_id),
                    slot: n.slot,
                    start_time_secs: n.start_time_secs.max(0.0),
                    duration_secs: n.duration_secs,
                });
                continue;
            }
        }
        if let Some(track_index) = n.track_index {
            if track_index < n_tracks {
                orphan_by_track.push((track_index, n));
            }
        }
    }

    if seq_clips.is_empty() {
        let default_dur = crate::project_actions::default_seq_duration_secs(file.tempo_bpm);
        for ti in 0..n_tracks {
            let related: Vec<&NoteFile> = orphan_by_track
                .iter()
                .filter(|(t, _)| *t == ti)
                .map(|(_, n)| n)
                .collect();
            let start = related
                .iter()
                .map(|n| n.start_time_secs.max(0.0))
                .fold(f32::INFINITY, f32::min);
            let start = if start.is_finite() { start } else { 0.0 };
            let end = related
                .iter()
                .map(|n| n.start_time_secs.max(0.0) + n.duration_secs)
                .fold(start + default_dur, f32::max);
            let id = next_seq_id;
            next_seq_id = next_seq_id.saturating_add(1).max(1);
            seq_clips.push(SeqClip {
                id: SeqId(id),
                track_index: ti,
                start_time_secs: start,
                duration_secs: (end - start).max(default_dur),
            });
        }
    } else {
        for ti in 0..n_tracks {
            if seq_clips.iter().any(|s| s.track_index == ti) {
                continue;
            }
            let id = next_seq_id;
            next_seq_id = next_seq_id.saturating_add(1).max(1);
            seq_clips.push(SeqClip {
                id: SeqId(id),
                track_index: ti,
                start_time_secs: 0.0,
                duration_secs: crate::project_actions::default_seq_duration_secs(file.tempo_bpm),
            });
        }
    }

    for (track_index, n) in orphan_by_track {
        let Some(seq) = seq_clips.iter().find(|s| s.track_index == track_index) else {
            continue;
        };
        let local = (n.start_time_secs - seq.start_time_secs).max(0.0);
        notes.push(PadNote {
            id: NoteId(n.id),
            seq_id: seq.id,
            slot: n.slot,
            start_time_secs: local,
            duration_secs: n.duration_secs,
        });
    }

    let mut next_note_id = file.next_note_id.max(1);
    for note in &notes {
        next_note_id = next_note_id.max(note.id.0.saturating_add(1));
    }
    for note in &mut notes {
        if note.id.0 == 0 {
            let id = next_note_id;
            next_note_id = next_note_id.saturating_add(1).max(1);
            note.id = NoteId(id);
        }
    }

    let project = Project {
        name: file.name,
        clips,
        transport: crate::model::Transport::default(),
        device_sample_rate: dst_rate,
        next_clip_id: file.next_clip_id.max(1),
        markers: file
            .markers
            .into_iter()
            .map(|m| CueMarker {
                slot: m.slot,
                time_secs: m.time_secs.max(0.0),
                track_index: m.track_index,
            })
            .collect(),
        tracks,
        notes,
        seq_clips,
        next_note_id,
        next_seq_id,
        tempo_bpm: file.tempo_bpm.clamp(20.0, 400.0),
        sampler_preview: crate::model::SamplerPreview::default(),
    };

    Ok(DiskProject {
        id: file.id.max(id),
        project,
        sample_files,
    })
}

pub fn load_library(root: &Path, device_sample_rate: u32) -> (Vec<DiskProject>, u64, Option<String>) {
    if let Err(e) = ensure_root(root) {
        return (Vec::new(), 1, Some(e));
    }

    let mut next_id = 1u64;
    if let Ok(raw) = fs::read_to_string(index_path(root)) {
        if let Ok(idx) = serde_json::from_str::<LibraryFile>(&raw) {
            if idx.format == FORMAT {
                next_id = idx.next_project_id.max(1);
            }
        }
    }

    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    let dir = projects_dir(root);
    let rd = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(_) => return (loaded, next_id, None),
    };
    let mut ids = Vec::new();
    for ent in rd.flatten() {
        let name = ent.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(rest) = name.strip_prefix('p') else {
            continue;
        };
        if let Ok(id) = rest.parse::<u64>() {
            if ent.path().join("project.json").is_file() {
                ids.push(id);
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        match load_project(root, id, device_sample_rate) {
            Ok(p) => {
                next_id = next_id.max(p.id.saturating_add(1));
                loaded.push(p);
            }
            Err(e) => errors.push(format!("проект {id}: {e}")),
        }
    }

    let err = if errors.is_empty() {
        None
    } else {
        Some(errors.join("; "))
    };
    (loaded, next_id, err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::model::Sample;
    use crate::project_actions;

    fn dummy_sample(n: usize, v: f32) -> Sample {
        let data = vec![v; n];
        let peaks = PeakPyramid::build(&data);
        Sample::new_mono(Arc::new(data), Arc::new(peaks))
    }

    fn temp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "tinysampler-persist-{}-{}",
            tag,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn roundtrip_project_with_shared_sample() {
        let root = temp_root("roundtrip");
        let mut project = Project::empty(48_000);
        project.name = "Test".into();
        project.tempo_bpm = 96.0;
        let sample = dummy_sample(32, 0.25);
        let t0 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t0, sample.clone(), "kick.wav".into());
        let t1 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t1, sample, "kick.wav".into());
        project.clips[1].start_time_secs = 1.5;
        project_actions::try_place_marker(&mut project, 0.5, 0);
        assert_eq!(
            project_actions::try_place_pad_marker(&mut project, t0, 7, 32, 8),
            Some(0)
        );
        let seq0 = project_actions::seq_clip_on_track(&project, t0).unwrap();
        assert!(project_actions::place_pad_note(&mut project, seq0, 0, 0.5).is_some());

        let mut entry = DiskProject::new(7, project);
        save_project(&root, &mut entry).unwrap();
        save_index(&root, 8).unwrap();

        let samples = fs::read_dir(project_dir(&root, 7).join("samples"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("wav"))
            .count();
        assert_eq!(samples, 1, "shared Arc should write one wav");

        let (lib, next, err) = load_library(&root, 48_000);
        assert!(err.is_none(), "{err:?}");
        assert_eq!(next, 8);
        assert_eq!(lib.len(), 1);
        let loaded = &lib[0].project;
        assert_eq!(loaded.name, "Test");
        assert_eq!(loaded.tempo_bpm, 96.0);
        assert_eq!(loaded.tracks.len(), 2);
        assert_eq!(loaded.clips.len(), 2);
        assert_eq!(loaded.clips[0].sample.data.len(), 32);
        assert!((loaded.clips[0].sample.data[0] - 0.25).abs() < 1e-5);
        assert_eq!(loaded.clips[1].start_time_secs, 1.5);
        assert_eq!(loaded.markers.len(), 1);
        assert_eq!(loaded.tracks[0].pad_markers.len(), 1);
        assert_eq!(loaded.tracks[0].pad_markers[0].start_index, 7);
        assert_eq!(loaded.tracks[0].pad_markers[0].end_index, 15);
        assert_eq!(loaded.notes.len(), 1);
        assert_eq!(loaded.notes[0].slot, 0);
        assert_eq!(loaded.seq_clips.len(), 2);
        assert_eq!(
            loaded.notes[0].seq_id,
            loaded.seq_clips.iter().find(|s| s.track_index == 0).unwrap().id
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pad_marker_file_accepts_legacy_sample_index() {
        let m: PadMarkerFile = serde_json::from_str(r#"{"slot":2,"sample_index":9}"#).unwrap();
        assert_eq!(m.slot, 2);
        assert_eq!(m.sample_index, Some(9));
        assert_eq!(m.start_index, None);
        assert_eq!(m.end_index, None);
        let m: PadMarkerFile =
            serde_json::from_str(r#"{"slot":1,"start_index":4,"end_index":20}"#).unwrap();
        assert_eq!(m.start_index, Some(4));
        assert_eq!(m.end_index, Some(20));
        assert_eq!(m.sample_index, None);
    }

    #[test]
    fn replacing_sample_drops_old_wav() {
        let root = temp_root("replace");
        let mut project = Project::empty(48_000);
        let t0 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t0, dummy_sample(8, 0.1), "a".into());
        let mut entry = DiskProject::new(1, project);
        save_project(&root, &mut entry).unwrap();
        project_actions::set_track_sample(
            &mut entry.project,
            t0,
            dummy_sample(16, 0.9),
            "b".into(),
        );
        save_project(&root, &mut entry).unwrap();
        let n = fs::read_dir(project_dir(&root, 1).join("samples"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("wav"))
            .count();
        assert_eq!(n, 1);
        let loaded = load_project(&root, 1, 48_000).unwrap();
        assert_eq!(loaded.project.clips[0].sample.data.len(), 16);
        let _ = fs::remove_dir_all(&root);
    }
}
