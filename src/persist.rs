//! On-disk projects: JSON metadata + unique mono WAV samples.
//!
//! Saves go through `p{id}.next` then a directory publish so a crash cannot
//! leave `project.json` pointing at deleted WAVs. Load prefers `p{id}`, then
//! `.next`, then `.bak`. A missing WAV becomes an empty track buffer, not a
//! failed project.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::model::{
    CueMarker, NoteId, PadMarker, PadNote, Project, Sample, SampleSlice, SeqClip, SeqId, Track,
    TrackId,
};
use crate::project_actions::{pad_default_len, pad_range_from_start};
use crate::wav_loader;
use crate::waveform::PeakPyramid;

const FORMAT: u32 = 1;

/// Library card: no PCM. Full documents load on Open.
#[derive(Clone, Debug)]
pub struct LibraryMeta {
    pub id: u64,
    pub name: String,
    pub track_count: usize,
}

/// Pointer identity of a live `Arc<Vec<f32>>` → filename in `samples/`.
pub type SampleFileMap = HashMap<usize, String>;

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
    tempo_bpm: f32,
    tracks: Vec<TrackFile>,
    #[serde(default)]
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
    #[serde(default)]
    next_track_id: u64,
    /// Legacy: PCM used to be stored at the output device rate.
    #[serde(default)]
    pcm_sample_rate: u32,
}

#[derive(Serialize, Deserialize)]
struct TrackFile {
    #[serde(default)]
    id: u64,
    name: String,
    pitch_semitones: i32,
    #[serde(default)]
    source_tempo_bpm: f32,
    #[serde(default)]
    pad_markers: Vec<PadMarkerFile>,
    #[serde(default)]
    sample: Option<String>,
    #[serde(default)]
    sample_label: String,
    #[serde(default)]
    sample_slices: Vec<SampleSliceFile>,
}

#[derive(Serialize, Deserialize)]
struct SampleSliceFile {
    #[serde(default)]
    label: String,
    #[serde(default)]
    start_index: usize,
    #[serde(default)]
    end_index: usize,
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
    #[serde(default)]
    id: u64,
    #[serde(default)]
    start_time_secs: f32,
    #[serde(default)]
    label: String,
    #[serde(default)]
    trim_start: usize,
    #[serde(default)]
    trim_end: usize,
    track_index: usize,
    #[serde(default)]
    sample: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct MarkerFile {
    slot: u8,
    time_secs: f32,
    #[serde(default)]
    track_id: Option<u64>,
    #[serde(default)]
    track_index: Option<usize>,
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
    #[serde(default)]
    track_id: Option<u64>,
    #[serde(default)]
    track_index: Option<usize>,
    start_time_secs: f32,
    duration_secs: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub sound_library_dir: Option<PathBuf>,
}

fn settings_path(root: &Path) -> PathBuf {
    root.join("settings.json")
}

pub fn load_settings(root: &Path) -> AppSettings {
    let Ok(text) = fs::read_to_string(settings_path(root)) else {
        return AppSettings::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_settings(root: &Path, settings: &AppSettings) -> Result<(), String> {
    let body = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    write_bytes_replace(&settings_path(root), &body)
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

fn project_next_dir(root: &Path, id: u64) -> PathBuf {
    projects_dir(root).join(format!("p{id}.next"))
}

fn project_bak_dir(root: &Path, id: u64) -> PathBuf {
    projects_dir(root).join(format!("p{id}.bak"))
}

fn index_path(root: &Path) -> PathBuf {
    root.join("library.json")
}

fn write_bytes_replace(path: &Path, body: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let new_path = sidecar(path, "new");
    let bak_path = sidecar(path, "bak");
    fs::write(&new_path, body).map_err(|e| e.to_string())?;
    if path.exists() {
        let _ = fs::remove_file(&bak_path);
        fs::rename(path, &bak_path).map_err(|e| e.to_string())?;
    }
    match fs::rename(&new_path, path) {
        Ok(()) => {
            let _ = fs::remove_file(&bak_path);
            Ok(())
        }
        Err(e) => {
            if bak_path.exists() && !path.exists() {
                let _ = fs::rename(&bak_path, path);
            }
            Err(e.to_string())
        }
    }
}

fn sidecar(path: &Path, kind: &str) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("file");
    path.with_file_name(format!("{name}.{kind}"))
}

pub fn save_index(root: &Path, next_project_id: u64) -> Result<(), String> {
    ensure_root(root)?;
    let file = LibraryFile {
        format: FORMAT,
        next_project_id,
    };
    let body = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    write_bytes_replace(&index_path(root), &body)
}

fn load_index_file(root: &Path) -> Option<LibraryFile> {
    let primary = index_path(root);
    let bak = sidecar(&primary, "bak");
    for path in [&primary, &bak] {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(idx) = serde_json::from_str::<LibraryFile>(&raw) else {
            continue;
        };
        if idx.format == FORMAT {
            return Some(idx);
        }
    }
    None
}

/// Directory that actually contains `project.json` for `id` (published, next, or bak).
pub fn resolve_project_dir(root: &Path, id: u64) -> Option<PathBuf> {
    for dir in [
        project_dir(root, id),
        project_next_dir(root, id),
        project_bak_dir(root, id),
    ] {
        if dir.join("project.json").is_file() {
            return Some(dir);
        }
    }
    None
}

pub fn save_project(
    root: &Path,
    id: u64,
    project: &Project,
    sample_files: &mut SampleFileMap,
) -> Result<(), String> {
    ensure_root(root)?;
    let dest = project_dir(root, id);
    let next = project_next_dir(root, id);
    let bak = project_bak_dir(root, id);
    let _ = fs::remove_dir_all(&next);
    let samples_next = next.join("samples");
    fs::create_dir_all(&samples_next).map_err(|e| e.to_string())?;

    let dest_samples = dest.join("samples");
    let mut used_names: HashSet<String> = HashSet::new();
    let mut ptr_to_file: SampleFileMap = HashMap::new();
    let mut track_sample_names: Vec<Option<String>> = Vec::with_capacity(project.tracks.len());

    for track in &project.tracks {
        let Some(sample) = track.sample.as_ref() else {
            track_sample_names.push(None);
            continue;
        };
        if sample.data.is_empty() {
            track_sample_names.push(None);
            continue;
        }
        let ptr = std::sync::Arc::as_ptr(&sample.data) as usize;
        if let Some(name) = ptr_to_file.get(&ptr) {
            used_names.insert(name.clone());
            track_sample_names.push(Some(name.clone()));
            continue;
        }
        if let Some(existing) = sample_files.get(&ptr) {
            let src = dest_samples.join(existing);
            if src.is_file() {
                fs::copy(&src, samples_next.join(existing)).map_err(|e| e.to_string())?;
                used_names.insert(existing.clone());
                ptr_to_file.insert(ptr, existing.clone());
                track_sample_names.push(Some(existing.clone()));
                continue;
            }
        }
        let name = next_sample_name(&samples_next, &used_names);
        let path = samples_next.join(&name);
        wav_loader::write_wav_f32_mono(&path, &sample.data, sample.rate())?;
        used_names.insert(name.clone());
        ptr_to_file.insert(ptr, name.clone());
        track_sample_names.push(Some(name));
    }

    let file = ProjectFile {
        format: FORMAT,
        id,
        name: project.name.clone(),
        tempo_bpm: project.tempo_bpm,
        pcm_sample_rate: 0,
        next_note_id: project.next_note_id,
        next_seq_id: project.next_seq_id,
        next_track_id: project.next_track_id,
        tracks: project
            .tracks
            .iter()
            .zip(track_sample_names)
            .map(|(t, sample)| TrackFile {
                id: t.id.0,
                name: t.name.clone(),
                pitch_semitones: t.pitch_semitones,
                source_tempo_bpm: project.tempo_bpm,
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
                sample,
                sample_label: t.sample_label.clone(),
                sample_slices: t
                    .sample_slices
                    .iter()
                    .map(|s| SampleSliceFile {
                        label: s.label.clone(),
                        start_index: s.start_index,
                        end_index: s.end_index,
                    })
                    .collect(),
            })
            .collect(),
        clips: Vec::new(),
        markers: project
            .markers
            .iter()
            .map(|m| MarkerFile {
                slot: m.slot,
                time_secs: m.time_secs,
                track_id: Some(m.track_id.0),
                track_index: project.track_index(m.track_id),
            })
            .collect(),
        notes: project
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
        seq_clips: project
            .seq_clips
            .iter()
            .map(|s| SeqClipFile {
                id: s.id.0,
                track_id: Some(s.track_id.0),
                track_index: project.track_index(s.track_id),
                start_time_secs: s.start_time_secs,
                duration_secs: s.duration_secs,
            })
            .collect(),
    };
    let body = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
    fs::write(next.join("project.json"), body).map_err(|e| e.to_string())?;

    publish_dir(&dest, &next, &bak)?;
    *sample_files = ptr_to_file;
    Ok(())
}

fn publish_dir(dest: &Path, next: &Path, bak: &Path) -> Result<(), String> {
    let _ = fs::remove_dir_all(bak);
    if dest.exists() {
        fs::rename(dest, bak).map_err(|e| e.to_string())?;
    }
    match fs::rename(next, dest) {
        Ok(()) => {
            let _ = fs::remove_dir_all(bak);
            Ok(())
        }
        Err(e) => {
            if bak.exists() && !dest.exists() {
                let _ = fs::rename(bak, dest);
            }
            Err(e.to_string())
        }
    }
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

pub struct LoadedProject {
    pub project: Project,
    pub sample_files: SampleFileMap,
    pub warnings: Vec<String>,
}

pub fn load_project(root: &Path, id: u64) -> Result<LoadedProject, String> {
    let dir = resolve_project_dir(root, id)
        .ok_or_else(|| format!("проект {id} не найден"))?;
    let path = dir.join("project.json");
    let raw = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let file: ProjectFile = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if file.format != FORMAT {
        return Err(format!("неизвестный формат проекта {}", file.format));
    }
    let samples_dir = dir.join("samples");
    let mut warnings = Vec::new();

    let mut cache: HashMap<String, Sample> = HashMap::new();
    let mut sample_files: SampleFileMap = HashMap::new();

    let load_named = |name: &str,
                      cache: &mut HashMap<String, Sample>,
                      sample_files: &mut SampleFileMap,
                      warnings: &mut Vec<String>|
     -> Option<Sample> {
        if let Some(existing) = cache.get(name) {
            return Some(existing.clone());
        }
        let wav_path = samples_dir.join(name);
        match wav_loader::load_wav_mono_f32(&wav_path) {
            Ok(loaded) => {
                let ptr = std::sync::Arc::as_ptr(&loaded.data) as usize;
                sample_files.insert(ptr, name.to_string());
                cache.insert(name.to_string(), loaded.clone());
                Some(loaded)
            }
            Err(e) => {
                warnings.push(format!("{name}: {e}"));
                None
            }
        }
    };

    let mut clip_by_track: HashMap<usize, (Option<Sample>, String)> = HashMap::new();
    for c in &file.clips {
        if clip_by_track.contains_key(&c.track_index) {
            continue;
        }
        let sample = c
            .sample
            .as_ref()
            .and_then(|name| load_named(name, &mut cache, &mut sample_files, &mut warnings));
        clip_by_track.insert(c.track_index, (sample, c.label.clone()));
    }

    let mut next_track_id = file.next_track_id.max(1);
    let mut seen_track = HashSet::new();
    let mut tracks: Vec<Track> = Vec::new();
    for (ti, t) in file.tracks.into_iter().enumerate() {
        let id_raw = if t.id == 0 || !seen_track.insert(t.id) {
            let id = next_track_id;
            next_track_id = next_track_id.saturating_add(1).max(1);
            id
        } else {
            next_track_id = next_track_id.max(t.id.saturating_add(1));
            t.id
        };
        let (legacy_sample, legacy_label) = clip_by_track.remove(&ti).unwrap_or((None, String::new()));
        let sample = t
            .sample
            .as_ref()
            .and_then(|name| load_named(name, &mut cache, &mut sample_files, &mut warnings))
            .or(legacy_sample);
        let sample_label = if t.sample_label.is_empty() {
            legacy_label
        } else {
            t.sample_label
        };
        let n = sample.as_ref().map(|s| s.data.len()).unwrap_or(0);
        let src_rate = file.pcm_sample_rate;
        let dst_rate = sample.as_ref().map(|s| s.rate()).unwrap_or(src_rate.max(1));
        let max_i = n.saturating_sub(1);
        let mut seen = [false; 16];
        let default_len = pad_default_len(dst_rate, file.tempo_bpm);
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
        let sample_slices = {
            let scaled: Vec<SampleSlice> = t
                .sample_slices
                .into_iter()
                .filter_map(|s| {
                    if n == 0 || s.end_index <= s.start_index {
                        return None;
                    }
                    let start = scale_index(s.start_index, src_rate, dst_rate, max_i);
                    let end = scale_index(s.end_index, src_rate, dst_rate, n).max(start + 1);
                    Some(SampleSlice {
                        label: s.label,
                        start_index: start,
                        end_index: end.min(n),
                    })
                })
                .collect();
            if scaled.is_empty() && n > 0 {
                vec![SampleSlice {
                    label: sample_label.clone(),
                    start_index: 0,
                    end_index: n,
                }]
            } else {
                scaled
            }
        };
        tracks.push(Track {
            id: TrackId(id_raw),
            name: t.name,
            pitch_semitones: t.pitch_semitones.clamp(-24, 24),
            sample,
            sample_label,
            sample_slices,
            pad_markers,
        });
    }

    let id_at = |ti: usize| tracks.get(ti).map(|t| t.id);

    let mut next_seq_id = file.next_seq_id.max(1);
    let mut seen_seq = HashSet::new();
    let mut seq_clips: Vec<SeqClip> = Vec::new();
    for s in file.seq_clips {
        let track_id = s
            .track_id
            .map(TrackId)
            .or_else(|| s.track_index.and_then(id_at));
        let Some(track_id) = track_id else {
            continue;
        };
        if project_track_missing(&tracks, track_id) || s.duration_secs <= 0.0 {
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
            track_id,
            start_time_secs: s.start_time_secs.max(0.0),
            duration_secs: s.duration_secs,
        });
    }

    let mut seen_note_ids = HashSet::new();
    let mut notes: Vec<PadNote> = Vec::new();
    let mut orphan_by_track: Vec<(TrackId, NoteFile)> = Vec::new();
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
        if let Some(track_id) = n.track_index.and_then(id_at) {
            orphan_by_track.push((track_id, n));
        }
    }

    if seq_clips.is_empty() {
        let default_dur = crate::time::default_seq_duration_secs(file.tempo_bpm);
        for track in &tracks {
            let related: Vec<&NoteFile> = orphan_by_track
                .iter()
                .filter(|(t, _)| *t == track.id)
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
                track_id: track.id,
                start_time_secs: start,
                duration_secs: (end - start).max(default_dur),
            });
        }
    } else {
        for track in &tracks {
            if seq_clips.iter().any(|s| s.track_id == track.id) {
                continue;
            }
            let id = next_seq_id;
            next_seq_id = next_seq_id.saturating_add(1).max(1);
            seq_clips.push(SeqClip {
                id: SeqId(id),
                track_id: track.id,
                start_time_secs: 0.0,
                duration_secs: crate::time::default_seq_duration_secs(file.tempo_bpm),
            });
        }
    }

    for (track_id, n) in orphan_by_track {
        let Some(seq) = seq_clips.iter().find(|s| s.track_id == track_id) else {
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

    let markers = file
        .markers
        .into_iter()
        .filter_map(|m| {
            let track_id = m.track_id.map(TrackId).or_else(|| m.track_index.and_then(id_at))?;
            if project_track_missing(&tracks, track_id) {
                return None;
            }
            Some(CueMarker {
                slot: m.slot,
                time_secs: m.time_secs.max(0.0),
                track_id,
            })
        })
        .collect();

    let project = Project {
        name: file.name,
        transport: crate::model::Transport::default(),
        markers,
        notes,
        seq_clips,
        tracks,
        next_note_id,
        next_seq_id,
        next_track_id,
        tempo_bpm: file.tempo_bpm.clamp(20.0, 400.0),
        sampler_preview: crate::model::SamplerPreview::default(),
        audition: crate::model::FileAudition::default(),
    };

    Ok(LoadedProject {
        project,
        sample_files,
        warnings,
    })
}

fn project_track_missing(tracks: &[Track], id: TrackId) -> bool {
    !tracks.iter().any(|t| t.id == id)
}

fn parse_project_id(name: &str) -> Option<u64> {
    let rest = name.strip_prefix('p')?;
    let num = rest.split('.').next()?;
    num.parse().ok()
}

/// Index only: names and track counts, no PCM.
pub fn load_library_index(root: &Path) -> (Vec<LibraryMeta>, u64, Option<String>) {
    if let Err(e) = ensure_root(root) {
        return (Vec::new(), 1, Some(e));
    }

    let mut next_id = 1u64;
    if let Some(idx) = load_index_file(root) {
        next_id = idx.next_project_id.max(1);
    }

    let mut ids = Vec::new();
    let dir = projects_dir(root);
    if let Ok(rd) = fs::read_dir(&dir) {
        for ent in rd.flatten() {
            let name = ent.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if let Some(id) = parse_project_id(name) {
                if resolve_project_dir(root, id).is_some() {
                    ids.push(id);
                }
            }
        }
    }
    ids.sort_unstable();
    ids.dedup();

    let mut loaded = Vec::new();
    let mut errors = Vec::new();
    for id in ids {
        match load_meta(root, id) {
            Ok(meta) => {
                next_id = next_id.max(meta.id.saturating_add(1));
                loaded.push(meta);
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

fn load_meta(root: &Path, id: u64) -> Result<LibraryMeta, String> {
    let dir = resolve_project_dir(root, id).ok_or_else(|| "нет project.json".to_string())?;
    let raw = fs::read_to_string(dir.join("project.json")).map_err(|e| e.to_string())?;
    let file: ProjectFile = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    Ok(LibraryMeta {
        id: file.id.max(id),
        name: file.name,
        track_count: file.tracks.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::model::Sample;
    use crate::project_actions;

    fn dummy_sample(n: usize, v: f32, rate: u32) -> Sample {
        let data = vec![v; n];
        let peaks = PeakPyramid::build(&data);
        Sample::new_mono(Arc::new(data), Arc::new(peaks), rate)
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
        let mut project = Project::empty();
        project.name = "Test".into();
        project.tempo_bpm = 96.0;
        let sample = dummy_sample(32, 0.25, 48_000);
        let t0 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t0, sample.clone(), "kick.wav".into());
        let t1 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t1, sample, "kick.wav".into());
        project_actions::try_place_marker(&mut project, 0.5, t0);
        assert_eq!(
            project_actions::try_place_pad_marker(&mut project, t0, 7, 32, 8),
            Some(0)
        );
        let seq0 = project_actions::seq_clip_on_track(&project, t0).unwrap();
        assert!(project_actions::place_pad_note(&mut project, seq0, 0, 0.5).is_some());

        let mut files = SampleFileMap::new();
        save_project(&root, 7, &project, &mut files).unwrap();
        save_index(&root, 8).unwrap();

        let samples = fs::read_dir(project_dir(&root, 7).join("samples"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("wav"))
            .count();
        assert_eq!(samples, 1, "shared Arc should write one wav");

        let (lib, next, err) = load_library_index(&root);
        assert!(err.is_none(), "{err:?}");
        assert_eq!(next, 8);
        assert_eq!(lib.len(), 1);
        assert_eq!(lib[0].name, "Test");
        assert_eq!(lib[0].track_count, 2);

        let loaded = load_project(&root, 7).unwrap();
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        let loaded = loaded.project;
        assert_eq!(loaded.name, "Test");
        assert_eq!(loaded.tempo_bpm, 96.0);
        assert_eq!(loaded.tracks.len(), 2);
        assert_eq!(loaded.tracks[0].sample.as_ref().unwrap().data.len(), 32);
        assert!((loaded.tracks[0].sample.as_ref().unwrap().data[0] - 0.25).abs() < 1e-5);
        assert_eq!(loaded.tracks[0].sample.as_ref().unwrap().sample_rate, 48_000);
        assert_eq!(loaded.markers.len(), 1);
        assert_eq!(loaded.tracks[0].pad_markers.len(), 1);
        assert_eq!(loaded.tracks[0].pad_markers[0].start_index, 7);
        assert_eq!(loaded.tracks[0].pad_markers[0].end_index, 15);
        assert_eq!(loaded.notes.len(), 1);
        assert_eq!(loaded.seq_clips.len(), 2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_wav_does_not_fail_the_project() {
        let root = temp_root("missing");
        let mut project = Project::empty();
        let t0 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t0, dummy_sample(8, 0.1, 48_000), "a".into());
        let mut files = SampleFileMap::new();
        save_project(&root, 1, &project, &mut files).unwrap();
        let samples = project_dir(&root, 1).join("samples");
        for ent in fs::read_dir(&samples).unwrap().flatten() {
            let _ = fs::remove_file(ent.path());
        }
        let loaded = load_project(&root, 1).unwrap();
        assert!(!loaded.warnings.is_empty());
        assert!(loaded.project.tracks[0].sample.is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn interrupted_publish_recovers_from_bak() {
        let root = temp_root("bak");
        let mut project = Project::empty();
        project.name = "Alive".into();
        let t0 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t0, dummy_sample(8, 0.2, 48_000), "a".into());
        let mut files = SampleFileMap::new();
        save_project(&root, 3, &project, &mut files).unwrap();
        let dest = project_dir(&root, 3);
        let bak = project_bak_dir(&root, 3);
        fs::rename(&dest, &bak).unwrap();
        let loaded = load_project(&root, 3).unwrap();
        assert_eq!(loaded.project.name, "Alive");
        assert!(loaded.project.tracks[0].sample.is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn replacing_sample_drops_old_wav() {
        let root = temp_root("replace");
        let mut project = Project::empty();
        let t0 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t0, dummy_sample(8, 0.1, 48_000), "a".into());
        let mut files = SampleFileMap::new();
        save_project(&root, 1, &project, &mut files).unwrap();
        project_actions::set_track_sample(
            &mut project,
            t0,
            dummy_sample(16, 0.9, 48_000),
            "b".into(),
        );
        save_project(&root, 1, &project, &mut files).unwrap();
        let n = fs::read_dir(project_dir(&root, 1).join("samples"))
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("wav"))
            .count();
        assert_eq!(n, 1);
        let loaded = load_project(&root, 1).unwrap();
        assert_eq!(loaded.project.tracks[0].sample.as_ref().unwrap().data.len(), 16);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn pad_marker_file_accepts_legacy_sample_index() {
        let m: PadMarkerFile = serde_json::from_str(r#"{"slot":2,"sample_index":9}"#).unwrap();
        assert_eq!(m.slot, 2);
        assert_eq!(m.sample_index, Some(9));
        assert_eq!(m.start_index, None);
        assert_eq!(m.end_index, None);
    }

    #[test]
    fn load_meta_does_not_need_wavs() {
        let root = temp_root("meta");
        let mut project = Project::empty();
        project.name = "Card".into();
        let t0 = project_actions::add_track(&mut project);
        project_actions::set_track_sample(&mut project, t0, dummy_sample(8, 0.1, 22_050), "a".into());
        let mut files = SampleFileMap::new();
        save_project(&root, 4, &project, &mut files).unwrap();
        let meta = load_meta(&root, 4).unwrap();
        assert_eq!(meta.name, "Card");
        assert_eq!(meta.track_count, 1);
        let _ = fs::remove_dir_all(&root);
    }
}
