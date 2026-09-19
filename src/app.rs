use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use arc_swap::ArcSwap;
use egui::Key;

use crate::audio;
use crate::library;
use crate::model::{NoteId, PadEdge, Project, SeqId, TrackId};
use crate::persist;
use crate::pianoroll;
use crate::project_actions;
use crate::sampler;
use crate::session::PersistSession;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppScreen {
    Library,
    Studio,
}

#[derive(Clone, Copy)]
pub(crate) enum SeqDrag {
    Move { id: SeqId, grab_offset_secs: f32 },
    ResizeStart { id: SeqId },
    ResizeEnd { id: SeqId },
}

pub struct TinySamplerApp {
    pub(crate) project_swap: Arc<ArcSwap<Project>>,
    pub(crate) playhead_bits: Arc<AtomicU32>,
    pub(crate) seek_pending: Arc<AtomicBool>,
    pub(crate) seek_target_secs_bits: Arc<AtomicU32>,
    #[allow(dead_code)]
    pub(crate) engine: audio::AudioEngine,
    pub(crate) pixels_per_second: f32,
    pub(crate) status: String,
    pub(crate) timeline_scroll_px: f32,
    pub(crate) selected_note: Option<NoteId>,
    pub(crate) selected_seq: Option<SeqId>,
    pub(crate) seq_editor: Option<SeqId>,
    pub(crate) seq_view_start: f32,
    pub(crate) seq_view_len: f32,
    pub(crate) seq_note_drag: Option<pianoroll::NoteDrag>,
    pub(crate) seq_drag: Option<SeqDrag>,
    pub(crate) load_rx: Option<Receiver<Result<(crate::model::Sample, String), String>>>,
    pub(crate) follow_playhead_suspended: bool,
    pub(crate) pending_loads: VecDeque<PathBuf>,
    pub(crate) selected_marker: Option<(u8, TrackId)>,
    pub(crate) marker_drag: Option<(u8, TrackId)>,
    pub(crate) selected_track: Option<TrackId>,
    pub(crate) screen: AppScreen,
    pub(crate) persist: PersistSession,
    pub(crate) sampler_track: Option<TrackId>,
    pub(crate) sampler_view_start: f32,
    pub(crate) sampler_view_len: f32,
    pub(crate) sampler_selected_pad: Option<u8>,
    pub(crate) sampler_pad_drag: Option<(u8, PadEdge)>,
    pub(crate) sampler_active_pad: Option<u8>,
    pub(crate) piano_pad_held: Option<u8>,
    pub(crate) sampler_base_secs: f32,
    pub(crate) startup_maximize_after: u8,
}

impl TinySamplerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let playhead_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let seek_pending = Arc::new(AtomicBool::new(false));
        let seek_target_secs_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));

        let (engine, project_swap) = audio::open_output(
            Arc::clone(&playhead_bits),
            Arc::clone(&seek_pending),
            Arc::clone(&seek_target_secs_bits),
        )?;

        let persist_root = persist::default_root();
        if let Err(e) = persist::ensure_root(&persist_root) {
            eprintln!("persist dir: {e}");
        }
        let (persist, load_err) = PersistSession::load(persist_root);
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
            selected_note: None,
            selected_seq: None,
            seq_editor: None,
            seq_view_start: 0.0,
            seq_view_len: 1.0,
            seq_note_drag: None,
            seq_drag: None,
            load_rx: None,
            follow_playhead_suspended: false,
            pending_loads: VecDeque::new(),
            selected_marker: None,
            marker_drag: None,
            selected_track: None,
            screen: AppScreen::Library,
            persist,
            sampler_track: None,
            sampler_view_start: 0.0,
            sampler_view_len: 1.0,
            sampler_selected_pad: None,
            sampler_pad_drag: None,
            sampler_active_pad: None,
            piano_pad_held: None,
            sampler_base_secs: 0.0,
            startup_maximize_after: 2,
        })
    }

    pub(crate) fn publish(&mut self, project: Project) {
        self.project_swap.store(Arc::new(project));
        if self.screen == AppScreen::Studio {
            self.persist.mark_dirty();
        }
    }

    pub(crate) fn current_project(&self) -> Arc<Project> {
        self.project_swap.load_full()
    }

    pub(crate) fn playhead_secs(&self) -> f32 {
        f32::from_bits(self.playhead_bits.load(Ordering::Relaxed))
    }

    pub(crate) fn request_seek(&self, secs: f32) {
        let t = secs.max(0.0);
        self.seek_target_secs_bits
            .store(t.to_bits(), Ordering::Relaxed);
        self.seek_pending.store(true, Ordering::Release);
    }

    pub(crate) fn transport_toggle_play_pause(&mut self) {
        let mut p = (*self.current_project()).clone();
        p.transport.is_playing = !p.transport.is_playing;
        self.publish(p);
    }

    pub(crate) fn transport_stop(&mut self) {
        let mut p = (*self.current_project()).clone();
        p.transport.is_playing = false;
        p.transport.stop_generation = p.transport.stop_generation.wrapping_add(1);
        self.publish(p);
        self.timeline_scroll_px = 0.0;
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
        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file");
        self.status = format!("Загрузка {name}…");
        std::thread::spawn(move || {
            let _ = tx.send(project_actions::load_audio_file(&path));
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
                let track = self.sampler_track.or(self.selected_track);
                if let Some(track) = track {
                    let n = sample.data.len() as f32;
                    project_actions::set_track_sample(&mut p, track, sample, label.clone());
                    self.sampler_view_start = 0.0;
                    self.sampler_view_len = n.max(1.0);
                    self.sampler_selected_pad = None;
                    self.sampler_pad_drag = None;
                    self.sampler_active_pad = None;
                    self.sampler_base_secs = 0.0;
                    p.sampler_preview.playing = false;
                    p.sampler_preview.end_secs = None;
                    self.engine.reset_preview_secs();
                    let name = p
                        .track(track)
                        .map(|t| t.name.clone())
                        .unwrap_or_else(|| "трек".into());
                    self.status = format!("{label} → {name}");
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
        let (ctrl_space, space, open_wav, delete_clip, place_marker, play_marker_slot, save) =
            ctx.input(|i| {
                let mods = i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt;
                let space = i.key_pressed(Key::Space);
                let open_wav = i.key_pressed(Key::O) && (i.modifiers.ctrl || i.modifiers.command);
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
        let Some(track) = self.selected_track else {
            return;
        };
        let t = self.playhead_secs();
        let mut p = (*self.current_project()).clone();
        match project_actions::try_place_marker(&mut p, t, track) {
            Some(slot) => {
                let lane = p.track_index(track).unwrap_or(0);
                self.status = format!("Метка {slot} · дорожка {}", lane + 1);
                self.selected_marker = Some((slot, track));
                self.publish(p);
            }
            None => {
                self.status = "Все метки 1–9 на дорожке заняты".into();
            }
        }
    }

    fn play_from_marker(&mut self, slot: u8) {
        let Some(track) = self.selected_track else {
            return;
        };
        let Some(t) = project_actions::marker_time(&self.current_project(), slot, track) else {
            return;
        };
        self.request_seek(t);
        self.playhead_bits.store(t.to_bits(), Ordering::Relaxed);
        self.follow_playhead_suspended = false;
        self.selected_marker = Some((slot, track));
        let mut p = (*self.current_project()).clone();
        if !p.transport.is_playing {
            p.transport.is_playing = true;
        }
        self.publish(p);
    }

    pub(crate) fn delete_selected_marker(&mut self) {
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

    fn persist_now(&mut self, announce: bool) {
        let live = (*self.current_project()).clone();
        match self.persist.save_open(&live) {
            Ok(()) => {
                if announce {
                    self.status = "Сохранено".into();
                }
            }
            Err(e) => self.status = format!("Не удалось сохранить: {e}"),
        }
    }

    fn persist_if_due(&mut self) {
        let dragging = self.marker_drag.is_some()
            || self.sampler_pad_drag.is_some()
            || self.seq_drag.is_some()
            || self.seq_note_drag.is_some();
        let live = (*self.current_project()).clone();
        match self.persist.persist_if_due(&live, dragging) {
            Ok(_) => {}
            Err(e) => self.status = format!("Не удалось сохранить: {e}"),
        }
    }

    fn persist_open_on_exit(&mut self) {
        let live = (*self.current_project()).clone();
        if let Err(e) = self.persist.save_open(&live) {
            eprintln!("save project: {e}");
        }
    }

    fn reset_studio_view(&mut self) {
        self.timeline_scroll_px = 0.0;
        self.selected_note = None;
        self.selected_seq = None;
        self.seq_editor = None;
        self.seq_note_drag = None;
        self.seq_drag = None;
        self.follow_playhead_suspended = false;
        self.selected_marker = None;
        self.marker_drag = None;
        let p = self.current_project();
        self.selected_track = p.tracks.first().map(|t| t.id);
        drop(p);
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

    pub(crate) fn go_home(&mut self) {
        self.transport_stop();
        self.close_seq_editor();
        self.close_sampler();
        self.persist_now(false);
        self.persist.close_without_save();
        self.publish(Project::empty());
        self.screen = AppScreen::Library;
    }

    fn create_project(&mut self) {
        let _ = self.persist.save_open(&self.current_project());
        let mut project = Project::empty();
        let tentative_id = self.persist.next_project_id;
        project.name = format!("Проект {tentative_id}");
        match self.persist.create_and_open(&project) {
            Ok(_) => {
                self.publish(project);
                self.screen = AppScreen::Studio;
                self.reset_studio_view();
            }
            Err(e) => self.status = format!("Не удалось создать: {e}"),
        }
    }

    fn open_project(&mut self, id: u64) {
        let _ = self.persist.save_open(&self.current_project());
        match self.persist.open(id) {
            Ok((project, warnings)) => {
                self.publish(project);
                self.screen = AppScreen::Studio;
                self.reset_studio_view();
                if !warnings.is_empty() {
                    self.status = warnings.join("; ");
                }
            }
            Err(e) => self.status = format!("Не удалось открыть: {e}"),
        }
    }

    pub(crate) fn add_studio_track(&mut self) {
        let mut p = (*self.current_project()).clone();
        let id = project_actions::add_track(&mut p);
        self.selected_track = Some(id);
        self.publish(p);
        self.open_sampler(id);
    }

    pub(crate) fn open_seq_editor(&mut self, id: SeqId) {
        self.close_sampler();
        let proj = self.current_project();
        let Some(seq) = proj.seq_clips.iter().find(|s| s.id == id).copied() else {
            return;
        };
        drop(proj);
        self.seq_editor = Some(id);
        self.selected_seq = Some(id);
        self.selected_track = Some(seq.track_id);
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
        let Some(track) = proj.track(seq.track_id) else {
            drop(proj);
            self.close_seq_editor();
            return;
        };
        let track_name = track.name.clone();
        let tempo_bpm = proj.tempo_bpm;
        let pad_markers = track.pad_markers.clone();
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

    pub(crate) fn open_sampler(&mut self, track_id: TrackId) {
        self.close_seq_editor();
        let proj = self.current_project();
        let Some(track) = proj.track(track_id) else {
            return;
        };
        let n = track
            .sample
            .as_ref()
            .map(|s| s.data.len() as f32)
            .unwrap_or(1.0);
        drop(proj);
        self.sampler_track = Some(track_id);
        self.selected_track = Some(track_id);
        self.sampler_view_start = 0.0;
        self.sampler_view_len = n.max(1.0);
        self.sampler_selected_pad = None;
        self.sampler_pad_drag = None;
        self.sampler_active_pad = None;
        self.sampler_base_secs = 0.0;
        let mut p = (*self.current_project()).clone();
        p.sampler_preview.playing = false;
        p.sampler_preview.track_id = track_id;
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
        if s.is_finite() {
            s.max(0.0)
        } else {
            0.0
        }
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
            self.engine
                .seek_preview_secs(self.sampler_base_secs_clamped());
        } else {
            let start = self.sampler_base_secs_clamped();
            p.transport.is_playing = false;
            p.sampler_preview.playing = true;
            p.sampler_preview.start_secs = start;
            p.sampler_preview.end_secs = None;
            p.sampler_preview.generation = p.sampler_preview.generation.wrapping_add(1);
            p.sampler_preview.track_id = track;
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
        track: TrackId,
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
        p.sampler_preview.track_id = track;
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
            self.engine
                .seek_preview_secs(self.sampler_base_secs_clamped());
        }
    }

    fn sampler_sample_len(&self, track: TrackId) -> usize {
        self.current_project().sample_len(track)
    }

    fn sampler_cursor_sample_index(&self, track: TrackId) -> Option<usize> {
        let n = self.sampler_sample_len(track);
        if n == 0 {
            return None;
        }
        let p = self.current_project();
        let speed = p.track_speed(track).max(0.05);
        let rate = p
            .track(track)
            .and_then(|t| t.sample.as_ref())
            .map(|s| s.rate() as f32)
            .unwrap_or(1.0);
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
        let rate = p
            .track(track)
            .and_then(|t| t.sample.as_ref())
            .map(|s| s.rate())
            .unwrap_or(1);
        let secs = project_actions::sample_index_to_preview_secs(idx, rate, p.track_speed(track));
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
        let n = p.sample_len(track);
        let rate = p
            .track(track)
            .and_then(|t| t.sample.as_ref())
            .map(|s| s.rate())
            .unwrap_or(1);
        let default_len = project_actions::pad_default_len(rate, p.tempo_bpm);
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
        let rate = p
            .track(track)
            .and_then(|t| t.sample.as_ref())
            .map(|s| s.rate())
            .unwrap_or(1);
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
        if let Some(t) = p.track_mut(track) {
            t.pitch_semitones = (t.pitch_semitones + delta).clamp(-24, 24);
        }
        self.publish(p);
    }

    fn sampler_nudge_tempo(&mut self, delta: f32) {
        let mut p = (*self.current_project()).clone();
        if project_actions::nudge_tempo(&mut p, delta) {
            self.publish(p);
        }
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
                let n = p.sample_len(track);
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
        let track = seq.track_id;
        let Some((start_i, end_i)) = project_actions::pad_marker_range(&p, track, slot) else {
            drop(p);
            let label = sampler::PAD_LABELS.get(slot as usize).copied().unwrap_or("?");
            self.status = format!("Сначала нарежьте пэд {label} в инструменте сэмплинга");
            return;
        };
        let rate = p
            .track(track)
            .and_then(|t| t.sample.as_ref())
            .map(|s| s.rate())
            .unwrap_or(1);
        let speed = p.track_speed(track);
        drop(p);
        let start = project_actions::sample_index_to_preview_secs(start_i, rate, speed);
        let end = project_actions::sample_index_to_preview_secs(end_i, rate, speed);
        self.sampler_active_pad = Some(slot);
        self.status.clear();
        self.start_preview_on_track(track, start, Some(end), false);
    }

    fn show_sampler_modal(&mut self, ctx: &egui::Context) {
        let Some(track_id) = self.sampler_track else {
            return;
        };
        let proj = self.current_project();
        let Some(track) = proj.track(track_id) else {
            drop(proj);
            self.close_sampler();
            return;
        };
        let track_name = track.name.clone();
        let pitch_semitones = track.pitch_semitones;
        let tempo_bpm = proj.tempo_bpm;
        let speed = track.playback_speed();
        let mut preview_playing = proj.sampler_preview.playing;
        let preview_end = proj.sampler_preview.end_secs;
        let sample_buf = track.sample.as_ref().map(|s| {
            (
                Arc::clone(&s.data),
                Arc::clone(&s.peaks),
                s.rate(),
            )
        });
        let pad_markers = track.pad_markers.clone();
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
            self.engine
                .seek_preview_secs(self.sampler_base_secs_clamped());
        }
        let status = self.status.clone();
        let sample_rate = sample_buf.as_ref().map(|(_, _, r)| *r).unwrap_or(1);
        let sample = sample_buf
            .as_ref()
            .map(|(data, peaks, _)| (data.as_slice(), peaks.as_ref()));
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
            self.persist_open_on_exit();
        }

        match self.screen {
            AppScreen::Library => {
                let data_dir = self.persist.root.display().to_string();
                if let Some(action) = library::show(
                    ctx,
                    self.persist.library_items(),
                    &data_dir,
                    &self.status,
                ) {
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
        self.persist_open_on_exit();
    }
}
