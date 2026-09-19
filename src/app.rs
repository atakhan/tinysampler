use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use egui::{Color32, CursorIcon, Key, Pos2, Rect, Stroke, Vec2};

use crate::model::{ClipId, NoteId, PadEdge, Project, Sample, SeqId};
use crate::project_actions;
use crate::theme;
use crate::timeline::{self, TrimDrag};

use crate::audio;
use crate::library;
use crate::persist::{self, DiskProject};
use crate::pianoroll;
use crate::sampler;

#[derive(Clone)]
struct ClipSettleAnim {
    clip_id: ClipId,
    from_secs: f32,
    to_secs: f32,
    t0: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AppScreen {
    Library,
    Studio,
}

#[derive(Clone, Copy)]
enum SeqDrag {
    Move { id: SeqId, grab_offset_secs: f32 },
    ResizeStart { id: SeqId },
    ResizeEnd { id: SeqId },
}

pub struct TinySamplerApp {
    project_swap: Arc<ArcSwap<Project>>,
    playhead_bits: Arc<AtomicU32>,
    seek_pending: Arc<AtomicBool>,
    seek_target_secs_bits: Arc<AtomicU32>,
    #[allow(dead_code)]
    engine: audio::AudioEngine,
    pixels_per_second: f32,
    status: String,
    timeline_scroll_px: f32,
    selected_clip_id: Option<ClipId>,
    selected_note: Option<NoteId>,
    selected_seq: Option<SeqId>,
    seq_editor: Option<SeqId>,
    seq_view_start: f32,
    seq_view_len: f32,
    seq_note_drag: Option<pianoroll::NoteDrag>,
    seq_drag: Option<SeqDrag>,
    trim_drag: Option<TrimDrag>,
    clip_move_drag: Option<ClipId>,
    /// `true` only for Alt+duplicate drag; `false` for normal move (both use ghost preview).
    clip_move_from_alt_duplicate: bool,
    clip_settle_anim: Option<ClipSettleAnim>,
    load_rx: Option<Receiver<Result<(Sample, String), String>>>,
    /// Play: user panned away; edge-follow waits until the playhead is on screen again.
    follow_playhead_suspended: bool,
    pending_loads: VecDeque<PathBuf>,
    selected_marker: Option<(u8, usize)>,
    marker_drag: Option<(u8, usize)>,
    selected_track: usize,
    screen: AppScreen,
    library: Vec<DiskProject>,
    open_project_id: Option<u64>,
    next_project_id: u64,
    sampler_track: Option<usize>,
    sampler_view_start: f32,
    sampler_view_len: f32,
    sampler_selected_pad: Option<u8>,
    sampler_pad_drag: Option<(u8, PadEdge)>,
    sampler_active_pad: Option<u8>,
    /// Pad key currently held in the piano-roll (until release, even after the chop ends).
    piano_pad_held: Option<u8>,
    /// Return-to point for the sampler playhead. Click on the waveform sets it.
    sampler_base_secs: f32,
    persist_root: PathBuf,
    dirty: bool,
    last_persist: Instant,
    /// Wait this many frames after show, then send a single Maximized(true).
    /// Must not maximize at HWND creation — see comment in `main`.
    startup_maximize_after: u8,
}

impl TinySamplerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let playhead_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let seek_pending = Arc::new(AtomicBool::new(false));
        let seek_target_secs_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));

        let (engine, project_swap) = audio::open_output(
            Project::empty,
            Arc::clone(&playhead_bits),
            Arc::clone(&seek_pending),
            Arc::clone(&seek_target_secs_bits),
        )?;

        let persist_root = persist::default_root();
        if let Err(e) = persist::ensure_root(&persist_root) {
            eprintln!("persist dir: {e}");
        }
        let device_rate = project_swap.load().device_sample_rate;
        let (library, next_project_id, load_err) =
            persist::load_library(&persist_root, device_rate);
        let mut status = String::new();
        if let Some(e) = load_err {
            status = format!("Загрузка проектов: {e}");
        }

        let mut style = (*cc.egui_ctx.style()).clone();
        style.visuals.dark_mode = true;
        cc.egui_ctx.set_style(style);

        Ok(Self {
            project_swap,
            playhead_bits,
            seek_pending,
            seek_target_secs_bits,
            engine,
            pixels_per_second: 120.0,
            status,
            timeline_scroll_px: 0.0,
            selected_clip_id: None,
            selected_note: None,
            selected_seq: None,
            seq_editor: None,
            seq_view_start: 0.0,
            seq_view_len: 1.0,
            seq_note_drag: None,
            seq_drag: None,
            trim_drag: None,
            clip_move_drag: None,
            clip_move_from_alt_duplicate: false,
            clip_settle_anim: None,
            load_rx: None,
            follow_playhead_suspended: false,
            pending_loads: VecDeque::new(),
            selected_marker: None,
            marker_drag: None,
            selected_track: 0,
            screen: AppScreen::Library,
            library,
            open_project_id: None,
            next_project_id,
            sampler_track: None,
            sampler_view_start: 0.0,
            sampler_view_len: 1.0,
            sampler_selected_pad: None,
            sampler_pad_drag: None,
            sampler_active_pad: None,
            piano_pad_held: None,
            sampler_base_secs: 0.0,
            persist_root,
            dirty: false,
            last_persist: Instant::now(),
            startup_maximize_after: 2,
        })
    }

    fn publish(&mut self, project: Project) {
        self.project_swap.store(Arc::new(project));
        if self.screen == AppScreen::Studio {
            self.dirty = true;
        }
    }

    fn current_project(&self) -> Arc<Project> {
        self.project_swap.load_full()
    }

    fn playhead_secs(&self) -> f32 {
        f32::from_bits(self.playhead_bits.load(Ordering::Relaxed))
    }

    fn request_seek(&self, secs: f32) {
        let t = secs.max(0.0);
        self.seek_target_secs_bits
            .store(t.to_bits(), Ordering::Relaxed);
        self.seek_pending.store(true, Ordering::Release);
    }

    fn begin_clip_body_drag(&mut self, proj: &Project, clip_id: ClipId, from_alt_duplicate: bool) {
        let mut p = proj.clone();
        if let Some(i) = p.clip_index(clip_id) {
            p.clips[i].placement_preview = true;
            self.publish(p);
        }
        self.clip_move_from_alt_duplicate = from_alt_duplicate;
        self.clip_move_drag = Some(clip_id);
    }

    fn tick_clip_settle_anim(&mut self, ctx: &egui::Context) {
        let Some(anim) = self.clip_settle_anim.clone() else {
            return;
        };
        let now = ctx.input(|i| i.time);
        let elapsed = (now - anim.t0) as f32;
        const DUR_SECS: f32 = 0.12;
        let u = (elapsed / DUR_SECS).clamp(0.0, 1.0);
        let u = u * u * (3.0 - 2.0 * u);
        let pos = anim.from_secs + (anim.to_secs - anim.from_secs) * u;

        let mut p = (*self.current_project()).clone();
        let Some(idx) = p.clip_index(anim.clip_id) else {
            self.clip_settle_anim = None;
            return;
        };
        p.clips[idx].start_time_secs = pos;
        let done = u >= 1.0 - 1e-4;
        if done {
            p.clips[idx].placement_preview = false;
            self.clip_settle_anim = None;
        } else {
            self.clip_settle_anim = Some(anim);
        }
        self.publish(p);
        ctx.request_repaint();
    }

    /// End of clip drag (mouse released). Preview clips settle with optional short animation.
    fn finish_clip_preview_drop(&mut self, ctx: &egui::Context, clip_id: ClipId) {
        let proj = self.current_project();
        let Some(idx) = proj.clip_index(clip_id) else {
            return;
        };
        if !proj.clips[idx].placement_preview {
            return;
        }
        let cur = proj.clips[idx].start_time_secs;
        drop(proj);

        let mut p = (*self.current_project()).clone();
        let idx = p.clip_index(clip_id).unwrap();
        if !project_actions::clip_overlaps_others(&p, idx) {
            p.clips[idx].placement_preview = false;
            self.publish(p);
            return;
        }
        let target = project_actions::clip_start_respecting_no_overlap(&p, idx, cur, cur);
        if (target - cur).abs() < 1e-4 {
            p.clips[idx].start_time_secs = target;
            p.clips[idx].placement_preview = false;
            self.publish(p);
            return;
        }
        self.clip_settle_anim = Some(ClipSettleAnim {
            clip_id,
            from_secs: cur,
            to_secs: target,
            t0: ctx.input(|i| i.time),
        });
    }

    fn transport_toggle_play_pause(&mut self) {
        let mut p = (*self.current_project()).clone();
        p.transport.is_playing = !p.transport.is_playing;
        if p.transport.is_playing {
            self.trim_drag = None;
            self.clip_move_drag = None;
            self.clip_move_from_alt_duplicate = false;
            self.clip_settle_anim = None;
            project_actions::resolve_all_placement_previews(&mut p);
        }
        self.publish(p);
    }

    fn transport_stop(&mut self) {
        let mut p = (*self.current_project()).clone();
        p.transport.is_playing = false;
        p.transport.stop_generation = p.transport.stop_generation.wrapping_add(1);
        project_actions::resolve_all_placement_previews(&mut p);
        self.publish(p);
        self.timeline_scroll_px = 0.0;
        self.trim_drag = None;
        self.clip_move_drag = None;
        self.clip_move_from_alt_duplicate = false;
        self.clip_settle_anim = None;
        self.seek_pending.store(false, Ordering::Release);
        self.follow_playhead_suspended = false;
        self.marker_drag = None;
    }

    fn try_pick_and_load_audio(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Audio", &["wav", "mp3"])
            .add_filter("WAV", &["wav"])
            .add_filter("MP3", &["mp3"])
            .pick_file()
        {
            self.enqueue_audio_path(path);
        }
    }

    fn is_supported_audio_path(path: &std::path::Path) -> bool {
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| {
                let e = e.to_ascii_lowercase();
                e == "wav" || e == "mp3"
            })
            .unwrap_or(false)
    }

    fn enqueue_audio_path(&mut self, path: PathBuf) {
        if !Self::is_supported_audio_path(&path) {
            self.status = format!(
                "unsupported audio format (use WAV or MP3): {}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("file")
            );
            return;
        }
        self.pending_loads.push_back(path);
        self.start_next_load_if_idle();
    }

    fn start_next_load_if_idle(&mut self) {
        if self.load_rx.is_some() {
            return;
        }
        let Some(path) = self.pending_loads.pop_front() else {
            return;
        };
        let sample_rate = self.current_project().device_sample_rate;
        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        self.status = format!("Загрузка {name}…");
        std::thread::spawn(move || {
            let _ = tx.send(project_actions::load_audio_file(&path, sample_rate));
        });
    }

    fn poll_dropped_files(&mut self, ctx: &egui::Context) {
        if self.screen != AppScreen::Studio || self.sampler_track.is_none() {
            ctx.input_mut(|i| i.raw.dropped_files.clear());
            return;
        }
        let hovered = ctx.input(|i| !i.raw.hovered_files.is_empty());
        const DROP_HINT: &str = "Отпустите WAV или MP3, чтобы загрузить";
        if hovered {
            if self.status.is_empty() || self.status == DROP_HINT {
                self.status = DROP_HINT.into();
            }
        } else if self.status == DROP_HINT {
            self.status.clear();
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        for path in dropped {
            self.enqueue_audio_path(path);
        }
    }

    fn poll_audio_load(&mut self, ctx: &egui::Context) {
        let outcome = {
            let Some(rx) = &self.load_rx else {
                self.start_next_load_if_idle();
                return;
            };
            match rx.try_recv() {
                Ok(v) => Some(v),
                Err(mpsc::TryRecvError::Empty) => {
                    ctx.request_repaint();
                    return;
                }
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        self.load_rx = None;
        match outcome {
            Some(Ok((sample, label))) => {
                let mut p = (*self.current_project()).clone();
                let track = self
                    .sampler_track
                    .or_else(|| {
                        if p.tracks.is_empty() {
                            None
                        } else {
                            Some(self.selected_track.min(p.tracks.len().saturating_sub(1)))
                        }
                    });
                if let Some(track) = track {
                    project_actions::set_track_sample(&mut p, track, sample, label.clone());
                    let n = p
                        .clips
                        .iter()
                        .find(|c| c.track_index == track)
                        .map(|c| c.sample.data.len() as f32)
                        .unwrap_or(1.0);
                    self.sampler_view_start = 0.0;
                    self.sampler_view_len = n.max(1.0);
                    self.sampler_selected_pad = None;
                    self.sampler_pad_drag = None;
                    self.sampler_active_pad = None;
                    self.sampler_base_secs = 0.0;
                    p.sampler_preview.playing = false;
                    p.sampler_preview.end_secs = None;
                    self.engine.reset_preview_secs();
                    self.status = format!("{label} → {}", p.tracks[track].name);
                    self.publish(p);
                } else {
                    self.status = "Сначала добавьте трек в студии.".into();
                }
            }
            Some(Err(e)) => self.status = e,
            None => self.status = "Загрузка прервалась.".into(),
        }
        self.start_next_load_if_idle();
    }

    fn try_split_clip_at_playhead(&mut self) {}

    fn delete_selected_note(&mut self) {
        let Some(id) = self.selected_note else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if project_actions::delete_pad_note(&mut p, id) {
            self.selected_note = None;
            self.seq_note_drag = None;
            self.publish(p);
        }
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        if self.screen != AppScreen::Studio {
            return;
        }
        if self.sampler_track.is_some() {
            let (space, open_wav, escape, save, delete_pad, pad_slot) = ctx.input(|i| {
                let mods = i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt;
                (
                    i.key_pressed(Key::Space) && !i.modifiers.ctrl,
                    i.key_pressed(Key::O) && (i.modifiers.ctrl || i.modifiers.command),
                    i.key_pressed(Key::Escape),
                    i.key_pressed(Key::S) && (i.modifiers.ctrl || i.modifiers.command),
                    i.key_pressed(Key::Delete) && !mods,
                    sampler::pad_slot_from_keys(i),
                )
            });
            if escape {
                self.close_sampler();
            } else if space {
                self.sampler_toggle_preview();
            } else if open_wav {
                self.try_pick_and_load_audio();
            } else if save {
                self.persist_now(true);
            } else if delete_pad {
                if let Some(slot) = self.sampler_selected_pad {
                    self.delete_sampler_pad(slot);
                }
            } else if let Some(slot) = pad_slot {
                self.trigger_sampler_pad(slot);
            }
            return;
        }
        if self.seq_editor.is_some() {
            let (escape, delete, space, ctrl_space, save) = ctx.input(|i| {
                let mods = i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt;
                (
                    i.key_pressed(Key::Escape),
                    i.key_pressed(Key::Delete) && !mods,
                    i.key_pressed(Key::Space) && !i.modifiers.ctrl,
                    i.key_pressed(Key::Space) && i.modifiers.ctrl,
                    i.key_pressed(Key::S) && (i.modifiers.ctrl || i.modifiers.command),
                )
            });
            if escape {
                self.close_seq_editor();
            } else if delete {
                self.delete_selected_note();
            } else if ctrl_space {
                self.transport_stop();
            } else if space {
                self.transport_toggle_play_pause();
            } else if save {
                self.persist_now(true);
            }
            return;
        }
        let (ctrl_space, space, open_wav, split_playhead, delete_clip, place_marker, play_marker_slot, save) =
            ctx.input(|i| {
                let mods = i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt;
                let space = i.key_pressed(Key::Space);
                let open_wav = i.key_pressed(Key::O) && (i.modifiers.ctrl || i.modifiers.command);
                let split_playhead =
                    i.key_pressed(Key::K) && (i.modifiers.ctrl || i.modifiers.command);
                let delete_clip = i.key_pressed(Key::Delete);
                let place_marker = i.key_pressed(Key::M) && !mods;
                let save = i.key_pressed(Key::S) && (i.modifiers.ctrl || i.modifiers.command);
                let play_marker_slot = if mods {
                    None
                } else {
                    marker_slot_from_keys(i)
                };
                (
                    space && i.modifiers.ctrl,
                    space && !i.modifiers.ctrl,
                    open_wav,
                    split_playhead,
                    delete_clip,
                    place_marker,
                    play_marker_slot,
                    save,
                )
            });
        if ctrl_space {
            self.transport_stop();
        } else if space {
            self.transport_toggle_play_pause();
        } else if open_wav {
            self.try_pick_and_load_audio();
        } else if save {
            self.persist_now(true);
        } else if split_playhead {
            self.try_split_clip_at_playhead();
        } else if delete_clip {
            if self.selected_marker.is_some() {
                self.delete_selected_marker();
            } else {
                self.delete_selected_seq();
            }
        } else if place_marker {
            self.try_place_marker_at_playhead();
        } else if let Some(slot) = play_marker_slot {
            self.play_from_marker(slot);
        }
    }

    fn try_place_marker_at_playhead(&mut self) {
        let t = self.playhead_secs();
        let mut p = (*self.current_project()).clone();
        match project_actions::try_place_marker(&mut p, t, self.selected_track) {
            Some(slot) => {
                self.status = format!("Метка {slot} · дорожка {}", self.selected_track + 1);
                self.selected_marker = Some((slot, self.selected_track));
                self.selected_clip_id = None;
                self.publish(p);
            }
            None => {
                self.status = format!(
                    "Все метки 1–9 на дорожке {} заняты",
                    self.selected_track + 1
                );
            }
        }
    }

    fn play_from_marker(&mut self, slot: u8) {
        let Some(t) = project_actions::marker_time(
            &self.current_project(),
            slot,
            self.selected_track,
        ) else {
            return;
        };
        self.request_seek(t);
        self.playhead_bits.store(t.to_bits(), Ordering::Relaxed);
        self.follow_playhead_suspended = false;
        self.selected_marker = Some((slot, self.selected_track));
        let mut p = (*self.current_project()).clone();
        if !p.transport.is_playing {
            p.transport.is_playing = true;
            self.trim_drag = None;
            self.clip_move_drag = None;
            self.clip_move_from_alt_duplicate = false;
            self.clip_settle_anim = None;
            project_actions::resolve_all_placement_previews(&mut p);
        }
        self.publish(p);
    }

    fn delete_selected_marker(&mut self) {
        let Some((slot, track)) = self.selected_marker else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if project_actions::delete_marker(&mut p, slot, track) {
            self.selected_marker = None;
            self.marker_drag = None;
            self.status = format!("Метка {slot} удалена");
            self.publish(p);
        }
    }

    fn library_items(&self) -> Vec<library::LibraryItem> {
        self.library
            .iter()
            .map(|e| library::LibraryItem {
                id: e.id,
                name: e.project.name.clone(),
                track_count: e.project.tracks.len(),
            })
            .collect()
    }

    fn flush_open_project(&mut self) {
        let Some(id) = self.open_project_id else {
            return;
        };
        let live = (*self.current_project()).clone();
        if let Some(entry) = self.library.iter_mut().find(|e| e.id == id) {
            entry.project = live;
        }
    }

    fn persist_now(&mut self, announce: bool) {
        self.flush_open_project();
        if let Some(id) = self.open_project_id {
            if let Some(idx) = self.library.iter().position(|e| e.id == id) {
                match persist::save_project(&self.persist_root, &mut self.library[idx]) {
                    Ok(()) => {
                        self.dirty = false;
                        self.last_persist = Instant::now();
                        if announce {
                            self.status = "Сохранено".into();
                        }
                    }
                    Err(e) => {
                        self.status = format!("Не удалось сохранить: {e}");
                        return;
                    }
                }
            }
        }
        if let Err(e) = persist::save_index(&self.persist_root, self.next_project_id) {
            self.status = format!("Не удалось сохранить список: {e}");
        }
    }

    fn persist_if_due(&mut self) {
        if self.screen != AppScreen::Studio || !self.dirty {
            return;
        }
        if self.trim_drag.is_some()
            || self.clip_move_drag.is_some()
            || self.marker_drag.is_some()
            || self.sampler_pad_drag.is_some()
            || self.seq_drag.is_some()
            || self.seq_note_drag.is_some()
        {
            return;
        }
        if self.last_persist.elapsed() < Duration::from_millis(2000) {
            return;
        }
        self.persist_now(false);
    }

    fn persist_everything(&mut self) {
        self.flush_open_project();
        for i in 0..self.library.len() {
            if let Err(e) = persist::save_project(&self.persist_root, &mut self.library[i]) {
                eprintln!("save project {}: {e}", self.library[i].id);
            }
        }
        if let Err(e) = persist::save_index(&self.persist_root, self.next_project_id) {
            eprintln!("save library: {e}");
        }
        self.dirty = false;
        self.last_persist = Instant::now();
    }

    fn reset_studio_view(&mut self) {
        self.timeline_scroll_px = 0.0;
        self.selected_clip_id = None;
        self.selected_note = None;
        self.selected_seq = None;
        self.seq_editor = None;
        self.seq_note_drag = None;
        self.seq_drag = None;
        self.trim_drag = None;
        self.clip_move_drag = None;
        self.clip_move_from_alt_duplicate = false;
        self.clip_settle_anim = None;
        self.follow_playhead_suspended = false;
        self.selected_marker = None;
        self.marker_drag = None;
        self.selected_track = 0;
        self.sampler_track = None;
        self.sampler_selected_pad = None;
        self.sampler_pad_drag = None;
        self.sampler_active_pad = None;
        self.piano_pad_held = None;
        self.sampler_base_secs = 0.0;
        self.status.clear();
        self.request_seek(0.0);
        self.playhead_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
    }

    fn go_home(&mut self) {
        self.transport_stop();
        self.close_seq_editor();
        self.close_sampler();
        self.flush_open_project();
        self.persist_now(false);
        self.open_project_id = None;
        self.screen = AppScreen::Library;
    }

    fn create_project(&mut self) {
        self.flush_open_project();
        let sr = self.current_project().device_sample_rate;
        let mut project = Project::empty(sr);
        let id = self.next_project_id;
        self.next_project_id = self.next_project_id.saturating_add(1);
        project.name = format!("Проект {id}");
        self.library.push(DiskProject::new(id, project.clone()));
        self.publish(project);
        self.open_project_id = Some(id);
        self.screen = AppScreen::Studio;
        self.reset_studio_view();
        self.dirty = true;
        self.persist_now(false);
    }

    fn open_project(&mut self, id: u64) {
        self.flush_open_project();
        self.persist_now(false);
        let Some(project) = self
            .library
            .iter()
            .find(|e| e.id == id)
            .map(|e| e.project.clone())
        else {
            return;
        };
        self.publish(project);
        self.open_project_id = Some(id);
        self.screen = AppScreen::Studio;
        self.reset_studio_view();
        self.dirty = false;
    }

    fn add_studio_track(&mut self) {
        let mut p = (*self.current_project()).clone();
        let i = project_actions::add_track(&mut p);
        self.selected_track = i;
        self.publish(p);
        self.open_sampler(i);
    }

    fn open_seq_editor(&mut self, id: SeqId) {
        self.close_sampler();
        let proj = self.current_project();
        let Some(seq) = proj.seq_clips.iter().find(|s| s.id == id).copied() else {
            return;
        };
        drop(proj);
        self.seq_editor = Some(id);
        self.selected_seq = Some(id);
        self.selected_track = seq.track_index;
        self.seq_view_start = 0.0;
        self.seq_view_len = seq.duration_secs.max(0.25);
        self.seq_note_drag = None;
        self.selected_note = None;
        self.seq_drag = None;
    }

    fn close_seq_editor(&mut self) {
        self.seq_editor = None;
        self.seq_note_drag = None;
        self.selected_note = None;
        self.piano_pad_held = None;
        if self.sampler_track.is_none() {
            self.stop_instrument_preview(false);
        }
    }

    fn delete_selected_seq(&mut self) {
        let Some(id) = self.selected_seq else {
            return;
        };
        if self.seq_editor == Some(id) {
            self.close_seq_editor();
        }
        let mut p = (*self.current_project()).clone();
        if project_actions::delete_seq_clip(&mut p, id) {
            self.selected_seq = None;
            self.seq_drag = None;
            self.publish(p);
        }
    }

    fn apply_piano_roll_action(&mut self, action: pianoroll::PianoRollAction) {
        let Some(seq_id) = self.seq_editor else {
            return;
        };
        match action {
            pianoroll::PianoRollAction::Close => self.close_seq_editor(),
            pianoroll::PianoRollAction::SelectNote { id } => self.selected_note = id,
            pianoroll::PianoRollAction::DeleteNote { id } => {
                let mut p = (*self.current_project()).clone();
                if project_actions::delete_pad_note(&mut p, id) {
                    if self.selected_note == Some(id) {
                        self.selected_note = None;
                    }
                    self.publish(p);
                }
            }
            pianoroll::PianoRollAction::Place { slot, time } => {
                let mut p = (*self.current_project()).clone();
                match project_actions::place_pad_note(&mut p, seq_id, slot, time) {
                    Some(id) => {
                        self.selected_note = Some(id);
                        self.status.clear();
                        self.publish(p);
                    }
                    None => {
                        let label = sampler::PAD_LABELS.get(slot as usize).copied().unwrap_or("?");
                        self.status = format!("Сначала нарежьте пэд {label} в инструменте сэмплинга");
                    }
                }
            }
            pianoroll::PianoRollAction::MoveNote { id, slot, time } => {
                let mut p = (*self.current_project()).clone();
                if project_actions::move_pad_note(&mut p, id, time, slot) {
                    self.selected_note = Some(id);
                    self.publish(p);
                }
            }
            pianoroll::PianoRollAction::ResizeNote { id, end } => {
                let mut p = (*self.current_project()).clone();
                if project_actions::resize_pad_note(&mut p, id, end) {
                    self.selected_note = Some(id);
                    self.publish(p);
                }
            }
        }
    }

    fn show_piano_roll_modal(&mut self, ctx: &egui::Context) {
        let Some(seq_id) = self.seq_editor else {
            return;
        };
        let proj = self.current_project();
        let Some(seq) = proj.seq_clips.iter().find(|s| s.id == seq_id).copied() else {
            drop(proj);
            self.close_seq_editor();
            return;
        };
        if seq.track_index >= proj.tracks.len() {
            drop(proj);
            self.close_seq_editor();
            return;
        }
        let track_name = proj.tracks[seq.track_index].name.clone();
        let tempo_bpm = proj.tempo_bpm;
        let pad_markers = proj.tracks[seq.track_index].pad_markers.clone();
        let notes: Vec<_> = proj
            .notes
            .iter()
            .copied()
            .filter(|n| n.seq_id == seq_id)
            .collect();
        let playing = proj.transport.is_playing;
        let preview_playing = proj.sampler_preview.playing;
        let preview_end = proj.sampler_preview.end_secs;
        drop(proj);
        if preview_playing {
            if let Some(end) = preview_end {
                if end.is_finite() && self.engine.preview_secs() >= end {
                    self.stop_instrument_preview(false);
                }
            }
        }
        self.poll_piano_pad_keys(ctx);
        if self.piano_pad_held.is_some() {
            ctx.request_repaint();
        }
        let playhead_local = if playing {
            let t = self.playhead_secs() - seq.start_time_secs;
            (t >= 0.0 && t <= seq.duration_secs).then_some(t)
        } else {
            None
        };
        let status = self.status.clone();
        let model = pianoroll::PianoRollModel {
            track_name: &track_name,
            tempo_bpm,
            duration_secs: seq.duration_secs,
            notes: &notes,
            pad_markers: &pad_markers,
            selected_note: self.selected_note,
            playhead_local,
            active_pad: self.sampler_active_pad,
            status: &status,
        };
        let action = pianoroll::show(
            ctx,
            model,
            &mut self.seq_view_start,
            &mut self.seq_view_len,
            &mut self.seq_note_drag,
        );
        if let Some(action) = action {
            self.apply_piano_roll_action(action);
        }
    }

    fn open_sampler(&mut self, track_index: usize) {
        self.close_seq_editor();
        let proj = self.current_project();
        if track_index >= proj.tracks.len() {
            return;
        }
        let n = proj
            .clips
            .iter()
            .find(|c| c.track_index == track_index)
            .map(|c| c.sample.data.len() as f32)
            .unwrap_or(1.0);
        drop(proj);
        self.sampler_track = Some(track_index);
        self.selected_track = track_index;
        self.sampler_view_start = 0.0;
        self.sampler_view_len = n.max(1.0);
        self.sampler_selected_pad = None;
        self.sampler_pad_drag = None;
        self.sampler_active_pad = None;
        self.sampler_base_secs = 0.0;
        let mut p = (*self.current_project()).clone();
        p.sampler_preview.playing = false;
        p.sampler_preview.track_index = track_index;
        p.sampler_preview.start_secs = 0.0;
        p.sampler_preview.end_secs = None;
        self.publish(p);
        self.engine.reset_preview_secs();
    }

    fn close_sampler(&mut self) {
        if self.sampler_track.is_none() {
            return;
        }
        self.sampler_track = None;
        self.sampler_selected_pad = None;
        self.sampler_pad_drag = None;
        self.sampler_active_pad = None;
        self.sampler_base_secs = 0.0;
        let mut p = (*self.current_project()).clone();
        p.sampler_preview.playing = false;
        p.sampler_preview.start_secs = 0.0;
        p.sampler_preview.end_secs = None;
        self.publish(p);
        self.engine.reset_preview_secs();
    }

    fn sampler_base_secs_clamped(&self) -> f32 {
        let s = self.sampler_base_secs;
        if s.is_finite() { s.max(0.0) } else { 0.0 }
    }

    fn sampler_toggle_preview(&mut self) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if p.sampler_preview.playing {
            p.sampler_preview.playing = false;
            p.sampler_preview.end_secs = None;
            self.sampler_active_pad = None;
            self.publish(p);
            self.engine.seek_preview_secs(self.sampler_base_secs_clamped());
        } else {
            let start = self.sampler_base_secs_clamped();
            p.transport.is_playing = false;
            p.sampler_preview.playing = true;
            p.sampler_preview.start_secs = start;
            p.sampler_preview.end_secs = None;
            p.sampler_preview.generation = p.sampler_preview.generation.wrapping_add(1);
            p.sampler_preview.track_index = track;
            self.sampler_active_pad = None;
            self.engine.seek_preview_secs(start);
            self.publish(p);
        }
    }

    fn start_sampler_preview_range(&mut self, start_secs: f32, end_secs: Option<f32>) {
        let Some(track) = self.sampler_track else {
            return;
        };
        self.start_preview_on_track(track, start_secs, end_secs, true);
    }

    fn start_preview_on_track(
        &mut self,
        track: usize,
        start_secs: f32,
        end_secs: Option<f32>,
        stop_transport: bool,
    ) {
        let start_secs = if start_secs.is_finite() {
            start_secs.max(0.0)
        } else {
            0.0
        };
        let end_secs = end_secs.filter(|t| t.is_finite() && *t > start_secs);
        let mut p = (*self.current_project()).clone();
        if stop_transport {
            p.transport.is_playing = false;
        }
        p.sampler_preview.playing = true;
        p.sampler_preview.start_secs = start_secs;
        p.sampler_preview.end_secs = end_secs;
        p.sampler_preview.generation = p.sampler_preview.generation.wrapping_add(1);
        p.sampler_preview.track_index = track;
        self.engine.seek_preview_secs(start_secs);
        self.publish(p);
    }

    fn stop_instrument_preview(&mut self, restore_sampler_cursor: bool) {
        let mut p = (*self.current_project()).clone();
        if !p.sampler_preview.playing && self.sampler_active_pad.is_none() {
            return;
        }
        p.sampler_preview.playing = false;
        p.sampler_preview.end_secs = None;
        self.sampler_active_pad = None;
        self.publish(p);
        if restore_sampler_cursor {
            self.engine.seek_preview_secs(self.sampler_base_secs_clamped());
        }
    }

    fn sampler_sample_len(&self, track: usize) -> usize {
        self.current_project()
            .clips
            .iter()
            .find(|c| c.track_index == track)
            .map(|c| c.sample.data.len())
            .unwrap_or(0)
    }

    fn sampler_cursor_sample_index(&self, track: usize) -> Option<usize> {
        let n = self.sampler_sample_len(track);
        if n == 0 {
            return None;
        }
        let p = self.current_project();
        let speed = p.track_speed(track).max(0.05);
        let rate = p.device_sample_rate.max(1) as f32;
        let idx = (self.sampler_base_secs_clamped() * rate * speed).round() as usize;
        Some(idx.min(n - 1))
    }

    fn seek_sampler_cursor(&mut self, sample_index: usize) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let n = self.sampler_sample_len(track);
        if n == 0 {
            return;
        }
        let idx = sample_index.min(n - 1);
        let p = self.current_project();
        let secs = project_actions::sample_index_to_preview_secs(
            idx,
            p.device_sample_rate,
            p.track_speed(track),
        );
        let playing = p.sampler_preview.playing;
        drop(p);
        self.sampler_base_secs = secs;
        if !playing {
            self.engine.seek_preview_secs(secs);
        }
    }

    fn bind_sampler_pad_at_cursor(&mut self, slot: u8) {
        let Some(track) = self.sampler_track else {
            return;
        };
        if (slot as usize) >= sampler::PAD_COUNT {
            return;
        }
        let Some(idx) = self.sampler_cursor_sample_index(track) else {
            self.status = "Сначала загрузите сэмпл".into();
            return;
        };
        let mut p = (*self.current_project()).clone();
        let n = p
            .clips
            .iter()
            .find(|c| c.track_index == track)
            .map(|c| c.sample.data.len())
            .unwrap_or(0);
        let tempo = p
            .tracks
            .get(track)
            .map(|t| t.source_tempo_bpm)
            .unwrap_or(120.0);
        let default_len = project_actions::pad_default_len(p.device_sample_rate, tempo);
        if project_actions::bind_pad_marker(&mut p, track, slot, idx, n, default_len) {
            self.sampler_selected_pad = Some(slot);
            self.status = format!("Сэмпл {}", sampler::PAD_LABELS[slot as usize]);
            self.publish(p);
        }
    }

    fn delete_sampler_pad(&mut self, slot: u8) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if project_actions::delete_pad_marker(&mut p, track, slot) {
            if self.sampler_selected_pad == Some(slot) {
                self.sampler_selected_pad = None;
            }
            if self.sampler_active_pad == Some(slot) {
                self.sampler_active_pad = None;
            }
            self.sampler_pad_drag = None;
            self.publish(p);
        }
    }

    fn trigger_sampler_pad(&mut self, slot: u8) {
        let Some(track) = self.sampler_track else {
            return;
        };
        if (slot as usize) >= sampler::PAD_COUNT {
            return;
        }
        let p = self.current_project();
        let Some((start_i, end_i)) = project_actions::pad_marker_range(&p, track, slot) else {
            drop(p);
            self.bind_sampler_pad_at_cursor(slot);
            return;
        };
        let rate = p.device_sample_rate;
        let speed = p.track_speed(track);
        drop(p);
        let start = project_actions::sample_index_to_preview_secs(start_i, rate, speed);
        let end = project_actions::sample_index_to_preview_secs(end_i, rate, speed);
        self.sampler_selected_pad = Some(slot);
        self.sampler_active_pad = Some(slot);
        self.status.clear();
        self.start_sampler_preview_range(start, Some(end));
    }

    fn sampler_nudge_pitch(&mut self, delta: i32) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if let Some(t) = p.tracks.get_mut(track) {
            t.pitch_semitones = (t.pitch_semitones + delta).clamp(-24, 24);
        }
        self.publish(p);
    }

    fn sampler_nudge_tempo(&mut self, delta: f32) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if let Some(t) = p.tracks.get_mut(track) {
            t.source_tempo_bpm = (t.source_tempo_bpm + delta).clamp(20.0, 400.0);
            p.tempo_bpm = t.source_tempo_bpm;
        }
        self.publish(p);
    }

    fn apply_sampler_action(&mut self, action: sampler::SamplerAction) {
        match action {
            sampler::SamplerAction::Close => self.close_sampler(),
            sampler::SamplerAction::TogglePreview => self.sampler_toggle_preview(),
            sampler::SamplerAction::PitchDelta(d) => self.sampler_nudge_pitch(d),
            sampler::SamplerAction::TempoDelta(d) => self.sampler_nudge_tempo(d),
            sampler::SamplerAction::LoadFile => self.try_pick_and_load_audio(),
            sampler::SamplerAction::SeekCursor { sample_index } => {
                self.seek_sampler_cursor(sample_index);
            }
            sampler::SamplerAction::MovePad {
                slot,
                edge,
                sample_index,
            } => {
                let Some(track) = self.sampler_track else {
                    return;
                };
                let mut p = (*self.current_project()).clone();
                let n = p
                    .clips
                    .iter()
                    .find(|c| c.track_index == track)
                    .map(|c| c.sample.data.len())
                    .unwrap_or(0);
                if project_actions::move_pad_edge(&mut p, track, slot, edge, sample_index, n) {
                    self.sampler_selected_pad = Some(slot);
                    self.publish(p);
                }
            }
            sampler::SamplerAction::DeletePad { slot } => self.delete_sampler_pad(slot),
            sampler::SamplerAction::SelectPad { slot } => {
                self.sampler_selected_pad = slot;
            }
            sampler::SamplerAction::TriggerPad { slot } => self.trigger_sampler_pad(slot),
        }
    }

    fn poll_piano_pad_keys(&mut self, ctx: &egui::Context) {
        if let Some(slot) = self.piano_pad_held {
            if !ctx.input(|i| sampler::pad_key_down(i, slot)) {
                self.piano_pad_held = None;
                self.stop_instrument_preview(false);
            }
        }
        let pressed: Vec<u8> = ctx.input(|i| sampler::pad_slots_pressed(i).collect());
        for slot in pressed {
            if self.piano_pad_held == Some(slot) {
                continue;
            }
            self.piano_pad_held = Some(slot);
            self.trigger_piano_pad(slot);
        }
    }

    fn trigger_piano_pad(&mut self, slot: u8) {
        let Some(seq_id) = self.seq_editor else {
            return;
        };
        if (slot as usize) >= sampler::PAD_COUNT {
            return;
        }
        let p = self.current_project();
        let Some(seq) = p.seq_clips.iter().find(|s| s.id == seq_id).copied() else {
            return;
        };
        let track = seq.track_index;
        let Some((start_i, end_i)) = project_actions::pad_marker_range(&p, track, slot) else {
            drop(p);
            let label = sampler::PAD_LABELS.get(slot as usize).copied().unwrap_or("?");
            self.status = format!("Сначала нарежьте пэд {label} в инструменте сэмплинга");
            return;
        };
        let rate = p.device_sample_rate;
        let speed = p.track_speed(track);
        drop(p);
        let start = project_actions::sample_index_to_preview_secs(start_i, rate, speed);
        let end = project_actions::sample_index_to_preview_secs(end_i, rate, speed);
        self.sampler_active_pad = Some(slot);
        self.status.clear();
        self.start_preview_on_track(track, start, Some(end), false);
    }
}

fn marker_slot_from_keys(i: &egui::InputState) -> Option<u8> {
    const KEYS: [(Key, u8); 9] = [
        (Key::Num1, 1),
        (Key::Num2, 2),
        (Key::Num3, 3),
        (Key::Num4, 4),
        (Key::Num5, 5),
        (Key::Num6, 6),
        (Key::Num7, 7),
        (Key::Num8, 8),
        (Key::Num9, 9),
    ];
    KEYS.into_iter()
        .find(|(k, _)| i.key_pressed(*k))
        .map(|(_, slot)| slot)
}

impl eframe::App for TinySamplerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.startup_maximize_after > 0 {
            self.startup_maximize_after -= 1;
            if self.startup_maximize_after == 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(true));
            }
            ctx.request_repaint();
        }
        self.poll_audio_load(ctx);
        self.poll_dropped_files(ctx);
        if let Some(msg) = self.engine.recover_if_needed(&self.project_swap) {
            self.status = msg;
        }
        self.handle_global_shortcuts(ctx);
        self.persist_if_due();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.persist_everything();
        }

        match self.screen {
            AppScreen::Library => {
                let items = self.library_items();
                let data_dir = self.persist_root.display().to_string();
                if let Some(action) = library::show(ctx, &items, &data_dir, &self.status) {
                    match action {
                        library::LibraryAction::Create => self.create_project(),
                        library::LibraryAction::Open(id) => self.open_project(id),
                    }
                }
            }
            AppScreen::Studio => self.show_studio(ctx),
        }

        if self.sampler_track.is_some() {
            self.show_sampler_modal(ctx);
        }
        if self.seq_editor.is_some() {
            self.show_piano_roll_modal(ctx);
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_everything();
    }
}

impl TinySamplerApp {
    fn show_sampler_modal(&mut self, ctx: &egui::Context) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let proj = self.current_project();
        if track >= proj.tracks.len() {
            drop(proj);
            self.close_sampler();
            return;
        }
        let track_name = proj.tracks[track].name.clone();
        let pitch_semitones = proj.tracks[track].pitch_semitones;
        let tempo_bpm = proj.tracks[track].source_tempo_bpm;
        let speed = proj.tracks[track].playback_speed();
        let mut preview_playing = proj.sampler_preview.playing;
        let preview_end = proj.sampler_preview.end_secs;
        let sample_rate = proj.device_sample_rate;
        let pad_markers = proj.tracks[track].pad_markers.clone();
        let sample_buf = proj.clips.iter().find(|c| c.track_index == track).map(|c| {
            (Arc::clone(&c.sample.data), Arc::clone(&c.sample.peaks))
        });
        drop(proj);
        if preview_playing {
            if let Some(end) = preview_end {
                if end.is_finite() && self.engine.preview_secs() >= end {
                    let mut p = (*self.current_project()).clone();
                    p.sampler_preview.playing = false;
                    self.sampler_active_pad = None;
                    self.publish(p);
                    preview_playing = false;
                }
            }
        }
        if !preview_playing {
            self.engine.seek_preview_secs(self.sampler_base_secs_clamped());
        }
        let status = self.status.clone();
        let sample = sample_buf
            .as_ref()
            .map(|(data, peaks)| (data.as_slice(), peaks.as_ref()));
        let model = sampler::SamplerModel {
            track_name: &track_name,
            pitch_semitones,
            tempo_bpm,
            sample,
            preview_playing,
            preview_secs: self.engine.preview_secs(),
            base_secs: self.sampler_base_secs_clamped(),
            sample_rate,
            speed,
            status: &status,
            pad_markers: &pad_markers,
            selected_pad: self.sampler_selected_pad,
            active_pad: self.sampler_active_pad,
        };
        let action = sampler::show(
            ctx,
            model,
            &mut self.sampler_view_start,
            &mut self.sampler_view_len,
            &mut self.sampler_pad_drag,
        );
        if let Some(action) = action {
            self.apply_sampler_action(action);
        }
    }

    fn show_studio(&mut self, ctx: &egui::Context) {
        let btn = theme::STUDIO_TRANSPORT_BTN;
        let studio_locked = self.sampler_track.is_some() || self.seq_editor.is_some();
        if studio_locked {
            self.trim_drag = None;
            self.marker_drag = None;
            self.seq_drag = None;
            if let Some(id) = self.clip_move_drag.take() {
                self.clip_move_from_alt_duplicate = false;
                self.finish_clip_preview_drop(ctx, id);
            }
        }

        egui::TopBottomPanel::top("studio_top")
            .exact_height(theme::STUDIO_TOP_BAR_H)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!studio_locked, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(egui::RichText::new("⌂ Домой").color(Color32::WHITE))
                                .fill(theme::color_track_gutter_selected())
                                .min_size(Vec2::new(88.0, 32.0)),
                        )
                        .clicked()
                    {
                        self.go_home();
                    }
                    ui.add_space(16.0);
                    let playing = self.current_project().transport.is_playing;
                    if playing {
                        if timeline::round_transport_btn(
                            ui,
                            "⏸",
                            "Pause (Space)",
                            theme::color_transport_pause(),
                            btn,
                        )
                        .clicked()
                        {
                            self.transport_toggle_play_pause();
                        }
                    } else if timeline::round_transport_btn(
                        ui,
                        "▶",
                        "Play (Space)",
                        theme::color_transport_play(),
                        btn,
                    )
                    .clicked()
                    {
                        self.transport_toggle_play_pause();
                    }
                    ui.add_space(8.0);
                    if timeline::round_transport_btn(
                        ui,
                        "⏹",
                        "Stop (Ctrl+Space)",
                        theme::color_transport_stop(),
                        btn,
                    )
                    .clicked()
                    {
                        self.transport_stop();
                    }
                    ui.add_space(16.0);
                    let clock = timeline::format_clock(self.playhead_secs());
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(clock)
                                .monospace()
                                .size(20.0)
                                .color(Color32::from_gray(230)),
                        )
                        .selectable(false),
                    );
                    ui.add_space(16.0);
                    let bpm = self.current_project().tempo_bpm;
                    ui.label(
                        egui::RichText::new(format!("{bpm:.0} BPM"))
                            .weak()
                            .size(14.0),
                    );
                    let name = self.current_project().name.clone();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        if !self.status.is_empty() {
                            ui.label(egui::RichText::new(&self.status).weak().size(12.0));
                        }
                        ui.label(egui::RichText::new(name).weak().size(14.0));
                    });
                });
                });
            });

        if self.screen != AppScreen::Studio {
            return;
        }

        egui::SidePanel::left("studio_tracks")
            .exact_width(theme::STUDIO_SIDEBAR_W)
            .resizable(false)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!studio_locked, |ui| {
                ui.add_space(8.0);
                ui.label(egui::RichText::new("Треки").strong().size(15.0));
                ui.add_space(6.0);
                let proj = self.current_project();
                let names: Vec<String> = proj.tracks.iter().map(|t| t.name.clone()).collect();
                let n = names.len();
                drop(proj);
                if n == 0 {
                    ui.label(
                        egui::RichText::new("Пока нет дорожек")
                            .weak()
                            .size(12.0),
                    );
                }
                egui::ScrollArea::vertical()
                    .max_height(ui.available_height() - 52.0)
                    .show(ui, |ui| {
                        for (i, name) in names.iter().enumerate() {
                            let selected = self.selected_track == i;
                            ui.horizontal(|ui| {
                                if ui
                                    .selectable_label(selected, name)
                                    .on_hover_text("Выбрать дорожку")
                                    .clicked()
                                {
                                    self.selected_track = i;
                                }
                                if ui
                                    .small_button("сэмпл")
                                    .on_hover_text("Инструмент сэмплинга")
                                    .clicked()
                                {
                                    self.open_sampler(i);
                                }
                            });
                        }
                    });
                ui.add_space(8.0);
                if ui
                    .add(
                        egui::Button::new(
                            egui::RichText::new("+ Добавить трек").color(Color32::WHITE),
                        )
                        .fill(theme::color_transport_play())
                        .min_size(Vec2::new(ui.available_width(), 32.0)),
                    )
                    .clicked()
                {
                    self.add_studio_track();
                }
                });
            });

        egui::CentralPanel::default().show(ctx, |ui| {
            let proj = self.current_project();
            if let Some(id) = self.selected_clip_id {
                if proj.clip_index(id).is_none() {
                    self.selected_clip_id = None;
                }
            }

            ui.vertical(|ui| {
                let viewport_w = ui.available_width();
                let n_lanes = proj.tracks.len();
                if n_lanes == 0 {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("Добавьте трек слева, чтобы открыть инструмент сэмплинга")
                                .weak()
                                .size(15.0),
                        );
                    });
                    return;
                }
                self.selected_track = self.selected_track.min(n_lanes.saturating_sub(1));
                let gutter_w = 0.0_f32;
                let avail_for_lanes = (ui.available_height() - theme::TIME_RULER_HEIGHT).max(80.0);
                let min_h = theme::MARKER_LANE_HEIGHT + 72.0;
                let block_h = (avail_for_lanes / n_lanes as f32).clamp(
                    min_h,
                    theme::TIMELINE_TRACK_HEIGHT + theme::MARKER_LANE_HEIGHT,
                );
                let timeline_height = block_h * n_lanes as f32;
                let marker_end = proj
                    .markers
                    .iter()
                    .map(|m| m.time_secs)
                    .fold(0.0f32, f32::max);
                let mut end_secs = 4.0f32;
                for s in &proj.seq_clips {
                    end_secs = end_secs.max(s.end_time_secs());
                }
                let end_secs = end_secs
                    .max(self.playhead_secs() + 0.5)
                    .max(marker_end + 0.5);

                let stack_origin = ui.cursor().min;
                let combined_rect = Rect::from_min_size(
                    stack_origin,
                    Vec2::new(viewport_w, theme::TIME_RULER_HEIGHT + timeline_height),
                );
                if let Some(hp) = ctx.pointer_hover_pos() {
                    if self.sampler_track.is_none()
                        && self.seq_editor.is_none()
                        && combined_rect.contains(hp)
                    {
                        let (ctrl, alt, dy) = ctx.input(|i| {
                            (
                                i.modifiers.ctrl,
                                i.modifiers.alt,
                                i.smooth_scroll_delta.y + i.raw_scroll_delta.y,
                            )
                        });
                        if dy.abs() > 0.01 && ctrl && !alt {
                            let old_pps = self.pixels_per_second;
                            let new_pps = (old_pps * (1.0 + dy * 0.004))
                                .clamp(theme::TIMELINE_PPS_MIN, theme::TIMELINE_PPS_MAX);
                            if (new_pps - old_pps).abs() > f32::EPSILON {
                                let scroll = self.timeline_scroll_px;
                                let t_here = (((hp.x - (stack_origin.x + gutter_w)) + scroll)
                                    / old_pps)
                                    .max(0.0);
                                self.pixels_per_second = new_pps;
                                let timeline_w = (viewport_w - gutter_w).max(1.0);
                                let new_content_w = (end_secs * new_pps).max(timeline_w);
                                let max_scroll = (new_content_w - timeline_w).max(0.0);
                                let view_left = stack_origin.x + gutter_w;
                                let new_scroll = view_left + t_here * new_pps - hp.x;
                                self.timeline_scroll_px = new_scroll.clamp(0.0, max_scroll);
                            }
                        } else if dy.abs() > 0.01 && alt && !ctrl {
                            let timeline_w = (viewport_w - gutter_w).max(1.0);
                            let max_scroll = {
                                let pps = self.pixels_per_second;
                                let content_w = (end_secs * pps).max(timeline_w);
                                (content_w - timeline_w).max(0.0)
                            };
                            self.timeline_scroll_px =
                                (self.timeline_scroll_px - dy).clamp(0.0, max_scroll);
                            if proj.transport.is_playing {
                                self.follow_playhead_suspended = true;
                            }
                        }
                    }
                }

                let pps = self.pixels_per_second;
                let timeline_w = (viewport_w - gutter_w).max(1.0);
                let content_w = (end_secs * pps).max(timeline_w);
                let max_scroll = (content_w - timeline_w).max(0.0);

                let (ruler_row, _) = ui.allocate_exact_size(
                    Vec2::new(viewport_w, theme::TIME_RULER_HEIGHT),
                    egui::Sense::hover(),
                );
                let ruler_rect = ruler_row;
                let (tracks_rect, _) =
                    ui.allocate_exact_size(Vec2::new(viewport_w, timeline_height), egui::Sense::hover());
                let layout = timeline::TrackLayout {
                    area: tracks_rect,
                    n_lanes,
                    gutter_w,
                    marker_h: theme::MARKER_LANE_HEIGHT,
                };
                let view_left = layout.content_left();

                let pan_resp = ui.interact(
                    combined_rect,
                    egui::Id::new("timeline_scroll_pan"),
                    if studio_locked {
                        egui::Sense::hover()
                    } else {
                        egui::Sense::click_and_drag()
                    },
                );

                self.tick_clip_settle_anim(ctx);

                if !studio_locked {
                if let Some(drag) = self.trim_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        let dx = ctx.input(|i| i.pointer.delta().x);
                        if dx != 0.0 {
                            let mut p = (*self.current_project()).clone();
                            if project_actions::apply_trim_delta(&mut p, drag, dx, pps) {
                                self.publish(p);
                            }
                        }
                    } else {
                        self.trim_drag = None;
                    }
                } else if let Some(clip_id) = self.clip_move_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        let dx = ctx.input(|i| i.pointer.delta().x);
                        let mut p = (*self.current_project()).clone();
                        let allow_overlap = p
                            .clip_index(clip_id)
                            .is_some_and(|i| p.clips[i].placement_preview);
                        let mut changed = false;
                        if dx != 0.0 {
                            changed |= project_actions::nudge_clip_time_by_drag(
                                &mut p,
                                clip_id,
                                dx,
                                pps,
                                allow_overlap,
                            );
                        }
                        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                            let t = timeline::track_index_at_y(pos.y, &layout);
                            changed |= project_actions::set_clip_track(&mut p, clip_id, t);
                        }
                        if changed {
                            self.publish(p);
                        }
                    } else {
                        self.clip_move_drag = None;
                        self.clip_move_from_alt_duplicate = false;
                        self.finish_clip_preview_drop(ctx, clip_id);
                    }
                } else if let Some((slot, track)) = self.marker_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                            let t = (((pos.x - view_left) + self.timeline_scroll_px) / pps).max(0.0);
                            let mut p = (*self.current_project()).clone();
                            if project_actions::move_marker(&mut p, slot, track, t) {
                                self.publish(p);
                            }
                        }
                    } else {
                        self.marker_drag = None;
                    }
                } else if let Some(drag) = self.seq_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                            let t = (((pos.x - view_left) + self.timeline_scroll_px) / pps).max(0.0);
                            let mut p = (*self.current_project()).clone();
                            let changed = match drag {
                                SeqDrag::Move { id, grab_offset_secs } => {
                                    project_actions::move_seq_clip(&mut p, id, t - grab_offset_secs)
                                }
                                SeqDrag::ResizeStart { id } => {
                                    project_actions::resize_seq_start(&mut p, id, t)
                                }
                                SeqDrag::ResizeEnd { id } => {
                                    project_actions::resize_seq_end(&mut p, id, t)
                                }
                            };
                            if changed {
                                self.publish(p);
                            }
                        }
                    } else {
                        self.seq_drag = None;
                    }
                }
                if ctx.input(|i| i.pointer.primary_pressed())
                    && self.trim_drag.is_none()
                    && self.clip_move_drag.is_none()
                    && self.marker_drag.is_none()
                    && self.seq_drag.is_none()
                {
                    let proj_now = self.current_project();
                    if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        if layout.gutter_contains(pos) {
                            self.selected_track = layout.lane_at_y(pos.y);
                            self.selected_clip_id = None;
                            self.selected_seq = None;
                            self.selected_marker = None;
                        } else if let Some((slot, track, on_delete)) = timeline::marker_hit_at_pointer(
                            &proj_now.markers,
                            pos,
                            &layout,
                            view_left,
                            pps,
                            self.timeline_scroll_px,
                        ) {
                            self.selected_marker = Some((slot, track));
                            self.selected_track = track;
                            self.selected_clip_id = None;
                            self.selected_seq = None;
                            if on_delete {
                                drop(proj_now);
                                self.delete_selected_marker();
                            } else {
                                self.marker_drag = Some((slot, track));
                            }
                        } else if layout.marker_bar(layout.lane_at_y(pos.y)).contains(pos)
                            && !layout.gutter_contains(pos)
                        {
                            self.selected_track = layout.lane_at_y(pos.y);
                            self.selected_marker = None;
                        } else if let Some((id, hit)) = pianoroll::hit_seq(
                            &proj_now,
                            pos,
                            &layout,
                            view_left,
                            pps,
                            self.timeline_scroll_px,
                        ) {
                            self.selected_seq = Some(id);
                            self.selected_marker = None;
                            self.selected_clip_id = None;
                            if let Some(i) = proj_now.seq_index(id) {
                                self.selected_track = proj_now.seq_clips[i].track_index;
                            }
                            match hit {
                                pianoroll::SeqHit::ResizeStart => {
                                    self.seq_drag = Some(SeqDrag::ResizeStart { id });
                                }
                                pianoroll::SeqHit::ResizeEnd => {
                                    self.seq_drag = Some(SeqDrag::ResizeEnd { id });
                                }
                                pianoroll::SeqHit::Body => {
                                    let start = proj_now
                                        .seq_clips
                                        .iter()
                                        .find(|s| s.id == id)
                                        .map(|s| s.start_time_secs)
                                        .unwrap_or(0.0);
                                    let t = (((pos.x - view_left) + self.timeline_scroll_px) / pps)
                                        .max(0.0);
                                    self.seq_drag = Some(SeqDrag::Move {
                                        id,
                                        grab_offset_secs: t - start,
                                    });
                                }
                            }
                        }
                    }
                }

                let proj = self.current_project();

                if pan_resp.dragged()
                    && self.trim_drag.is_none()
                    && self.clip_move_drag.is_none()
                    && self.marker_drag.is_none()
                    && self.seq_drag.is_none()
                    && self.clip_settle_anim.is_none()
                    && pan_resp
                        .interact_pointer_pos()
                        .is_none_or(|p| !layout.gutter_contains(p))
                {
                    self.timeline_scroll_px =
                        (self.timeline_scroll_px - pan_resp.drag_delta().x).clamp(0.0, max_scroll);
                    if proj.transport.is_playing {
                        self.follow_playhead_suspended = true;
                    }
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                } else if self.marker_drag.is_some() {
                    ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                } else if self.seq_drag.is_some() {
                    ctx.set_cursor_icon(match self.seq_drag {
                        Some(SeqDrag::Move { .. }) => CursorIcon::Grabbing,
                        _ => CursorIcon::ResizeHorizontal,
                    });
                } else if self.trim_drag.is_some() || self.clip_move_drag.is_some() {
                    let ghost_drag = self.clip_move_drag.is_some_and(|cid| {
                        proj.clip_index(cid)
                            .is_some_and(|i| proj.clips[i].placement_preview)
                    });
                    ctx.set_cursor_icon(if ghost_drag {
                        if self.clip_move_from_alt_duplicate {
                            CursorIcon::Alias
                        } else {
                            CursorIcon::Move
                        }
                    } else {
                        CursorIcon::Grabbing
                    });
                } else if let Some(hp) = ctx.pointer_hover_pos() {
                    if let Some((_, _, on_delete)) = timeline::marker_hit_at_pointer(
                        &proj.markers,
                        hp,
                        &layout,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                    ) {
                        ctx.set_cursor_icon(if on_delete {
                            CursorIcon::PointingHand
                        } else {
                            CursorIcon::ResizeHorizontal
                        });
                    } else if let Some((_, hit)) = pianoroll::hit_seq(
                        &proj,
                        hp,
                        &layout,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                    ) {
                        ctx.set_cursor_icon(match hit {
                            pianoroll::SeqHit::Body => CursorIcon::Move,
                            _ => CursorIcon::ResizeHorizontal,
                        });
                    } else if layout.gutter_contains(hp) {
                        ctx.set_cursor_icon(CursorIcon::PointingHand);
                    } else if pan_resp.hovered() {
                        ctx.set_cursor_icon(CursorIcon::Grab);
                    }
                }

                if pan_resp.double_clicked() {
                    if let Some(p) = pan_resp.interact_pointer_pos() {
                        let sc = self.timeline_scroll_px;
                        if let Some((id, _)) =
                            pianoroll::hit_seq(&proj, p, &layout, view_left, pps, sc)
                        {
                            drop(proj);
                            self.open_seq_editor(id);
                        } else if layout.clips_rect(layout.lane_at_y(p.y)).contains(p)
                            && !layout.gutter_contains(p)
                        {
                            let track = layout.lane_at_y(p.y);
                            let t = (((p.x - view_left) + sc) / pps).max(0.0);
                            drop(proj);
                            let mut proj = (*self.current_project()).clone();
                            if let Some(id) = project_actions::add_seq_clip(&mut proj, track, t) {
                                self.selected_seq = Some(id);
                                self.selected_track = track;
                                self.publish(proj);
                            }
                        }
                    }
                } else if pan_resp.clicked() {
                    if let Some(p) = pan_resp.interact_pointer_pos() {
                        let sc = self.timeline_scroll_px;
                        if pianoroll::hit_seq(&proj, p, &layout, view_left, pps, sc).is_some() {
                            // selected on press
                        } else if layout.gutter_contains(p) {
                            self.selected_track = layout.lane_at_y(p.y);
                            self.selected_clip_id = None;
                            self.selected_seq = None;
                            self.selected_marker = None;
                        } else {
                            self.selected_clip_id = None;
                            self.selected_seq = None;
                            let time_at =
                                |x: f32| -> f32 { (((x - view_left) + sc) / pps).max(0.0) };
                            if (ruler_rect.contains(p)
                                || layout.marker_bar(layout.lane_at_y(p.y)).contains(p)
                                || layout.content_rect().contains(p))
                                && p.x >= view_left
                            {
                                let t = time_at(p.x);
                                self.request_seek(t);
                                self.timeline_scroll_px =
                                    (view_left + t * pps - p.x).clamp(0.0, max_scroll);
                                self.follow_playhead_suspended = false;
                            }
                        }
                    }
                }
                }

                let proj = self.current_project();

                if proj.transport.is_playing {
                    if self.follow_playhead_suspended {
                        if timeline::playhead_in_viewport(
                            layout.content_rect(),
                            self.playhead_secs(),
                            pps,
                            self.timeline_scroll_px,
                        ) {
                            self.follow_playhead_suspended = false;
                            self.timeline_scroll_px = timeline::scroll_keep_playhead_in_view(
                                layout.content_rect(),
                                self.playhead_secs(),
                                pps,
                                max_scroll,
                                self.timeline_scroll_px,
                            );
                        }
                    } else {
                        self.timeline_scroll_px = timeline::scroll_keep_playhead_in_view(
                            layout.content_rect(),
                            self.playhead_secs(),
                            pps,
                            max_scroll,
                            self.timeline_scroll_px,
                        );
                    }
                }

                let scroll = self.timeline_scroll_px;
                let to_screen = |t: f32| -> f32 { view_left + t * pps - scroll };

                let ruler_painter = ui.painter_at(ruler_rect);
                timeline::paint_ruler(
                    &ruler_painter,
                    ruler_rect,
                    pps,
                    scroll,
                    ctx,
                    timeline::RulerKind::Tempo,
                    self.current_project().tempo_bpm,
                );
                let play_x_head = to_screen(self.playhead_secs());
                if play_x_head >= ruler_rect.left() && play_x_head <= ruler_rect.right() {
                    ruler_painter.line_segment(
                        [
                            Pos2::new(play_x_head, ruler_rect.top()),
                            Pos2::new(play_x_head, ruler_rect.bottom()),
                        ],
                        Stroke::new(2.0, theme::color_playhead()),
                    );
                }

                let painter = ui.painter_at(tracks_rect);
                for lane in 0..n_lanes {
                    let clips = layout.clips_rect(lane);
                    let bg = if lane % 2 == 0 {
                        theme::color_timeline_bg()
                    } else {
                        theme::color_timeline_bg_alt()
                    };
                    painter.rect_filled(clips, 0.0, bg);
                    if lane == self.selected_track {
                        painter.rect_stroke(
                            layout.block_rect(lane),
                            0.0,
                            Stroke::new(1.5_f32, theme::color_track_gutter_selected()),
                        );
                    }
                    timeline::paint_marker_lane(
                        &painter,
                        &proj.markers,
                        layout.marker_bar(lane),
                        lane,
                        view_left,
                        pps,
                        scroll,
                        self.selected_marker,
                        lane == self.selected_track,
                    );
                }
                painter.rect_stroke(
                    tracks_rect,
                    4.0,
                    Stroke::new(1.0_f32, theme::color_timeline_border()),
                );

                pianoroll::paint_sausages(
                    &painter,
                    &proj,
                    &layout,
                    view_left,
                    pps,
                    scroll,
                    self.selected_seq,
                );
                for lane in 0..n_lanes {
                    if proj.seq_clips.iter().any(|s| s.track_index == lane) {
                        continue;
                    }
                    let clips = layout.clips_rect(lane);
                    painter.text(
                        clips.center(),
                        egui::Align2::CENTER_CENTER,
                        "Двойной клик — новая последовательность",
                        egui::FontId::proportional(13.0),
                        Color32::from_gray(110),
                    );
                }

                if !studio_locked {
                    if let Some(hp) = ui.ctx().pointer_hover_pos() {
                    if combined_rect.contains(hp) && hp.x >= view_left {
                        let gx = hp.x.clamp(view_left, combined_rect.right());
                        let cross_painter = ui.painter_at(combined_rect);
                        cross_painter.line_segment(
                            [
                                Pos2::new(gx, combined_rect.top()),
                                Pos2::new(gx, tracks_rect.bottom()),
                            ],
                            Stroke::new(1.5, theme::color_playhead_cross()),
                        );
                    }
                    }
                }

                timeline::paint_markers(
                    &painter,
                    &proj.markers,
                    &layout,
                    view_left,
                    pps,
                    scroll,
                );
                for lane in 0..n_lanes {
                    timeline::paint_marker_lane(
                        &painter,
                        &proj.markers,
                        layout.marker_bar(lane),
                        lane,
                        view_left,
                        pps,
                        scroll,
                        self.selected_marker,
                        lane == self.selected_track,
                    );
                }

                let play_x = to_screen(self.playhead_secs());
                if play_x >= view_left && play_x <= tracks_rect.right() {
                    painter.line_segment(
                        [
                            Pos2::new(play_x, tracks_rect.top()),
                            Pos2::new(play_x, tracks_rect.bottom()),
                        ],
                        Stroke::new(2.0, theme::color_playhead()),
                    );
                }

                if !studio_locked && ctx.input(|i| i.pointer.primary_clicked()) {
                    if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        if !combined_rect.contains(pos) {
                            self.selected_clip_id = None;
                            self.selected_seq = None;
                            self.selected_marker = None;
                        }
                    }
                }
            });
        });
    }
}
