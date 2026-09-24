use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use arc_swap::ArcSwap;
use egui::Key;

use crate::audio;
use crate::browser::{self, BrowserAction, LoadBrowser};
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum AudioLoadKind {
    Append,
    Replace,
    Audition,
}

pub(crate) struct PendingAudio {
    path: PathBuf,
    kind: AudioLoadKind,
    audition_ticket: u64,
}

#[derive(Clone, Copy)]
pub(crate) enum SeqDrag {
    Move { id: SeqId, grab_offset_secs: f32 },
    Copy { source: SeqId, grab_offset_secs: f32 },
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
    /// Horizontal zoom of the piano roll. `0` means “fit the sequence on the next frame”.
    pub(crate) seq_pps: f32,
    pub(crate) seq_note_drag: Option<pianoroll::NoteDrag>,
    pub(crate) seq_drag: Option<SeqDrag>,
    pub(crate) load_rx: Option<Receiver<Result<(crate::model::Sample, String), String>>>,
    pub(crate) load_kind: AudioLoadKind,
    pub(crate) load_ticket: u64,
    pub(crate) audition_ticket: u64,
    pub(crate) load_browser: Option<LoadBrowser>,
    pub(crate) follow_playhead_suspended: bool,
    pub(crate) pending_loads: VecDeque<PendingAudio>,
    pub(crate) selected_marker: Option<(u8, TrackId)>,
    pub(crate) marker_drag: Option<(u8, TrackId)>,
    pub(crate) selected_track: Option<TrackId>,
    pub(crate) screen: AppScreen,
    pub(crate) persist: PersistSession,
    pub(crate) sound_library_dir: Option<PathBuf>,
    pub(crate) settings_open: bool,
    pub(crate) settings_browse: PathBuf,
    pub(crate) sampler_track: Option<TrackId>,
    pub(crate) sampler_view_start: f32,
    pub(crate) sampler_view_len: f32,
    pub(crate) sampler_selected_pad: Option<u8>,
    pub(crate) sampler_pad_drag: Option<(u8, PadEdge)>,
    pub(crate) sampler_active_pad: Option<u8>,
    pub(crate) piano_pad_held: Option<u8>,
    pub(crate) sampler_base_secs: f32,
    /// Studio cursor at the moment Play was pressed. Stop returns here.
    pub(crate) studio_base_secs: f32,
    pub(crate) studio_base_scroll_px: f32,
    /// Pause froze the playhead away from [`Self::studio_base_secs`].
    pub(crate) studio_transport_paused: bool,
    pub(crate) startup_maximize_after: u8,
    pub(crate) sampler_tempo_rx: Option<Receiver<(u64, crate::musical_time::MusicalTimeAnalysis)>>,
    pub(crate) sampler_tempo_gen: u64,
    pub(crate) sampler_tempo: sampler::TempoDetectState,
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
        let settings = persist::load_settings(&persist.root);
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
            seq_pps: 0.0,
            seq_note_drag: None,
            seq_drag: None,
            load_rx: None,
            load_kind: AudioLoadKind::Replace,
            load_ticket: 0,
            audition_ticket: 0,
            load_browser: None,
            follow_playhead_suspended: false,
            pending_loads: VecDeque::new(),
            selected_marker: None,
            marker_drag: None,
            selected_track: None,
            screen: AppScreen::Library,
            persist,
            sound_library_dir: settings.sound_library_dir,
            settings_open: false,
            settings_browse: crate::browser::default_audio_dir(),
            sampler_track: None,
            sampler_view_start: 0.0,
            sampler_view_len: 1.0,
            sampler_selected_pad: None,
            sampler_pad_drag: None,
            sampler_active_pad: None,
            piano_pad_held: None,
            sampler_base_secs: 0.0,
            studio_base_secs: 0.0,
            studio_base_scroll_px: 0.0,
            studio_transport_paused: false,
            startup_maximize_after: 2,
            sampler_tempo_rx: None,
            sampler_tempo_gen: 0,
            sampler_tempo: sampler::TempoDetectState::default(),
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

    /// Space: start or resume, or stop if already playing.
    pub(crate) fn transport_on_space(&mut self) {
        let playing = self.current_project().transport.is_playing;
        if playing {
            self.transport_stop();
        } else {
            self.transport_toggle_play_pause();
        }
    }

    /// Ctrl+Space: freeze the playhead. Does nothing when playback is already stopped.
    pub(crate) fn transport_pause(&mut self) {
        let mut p = (*self.current_project()).clone();
        if !p.transport.is_playing {
            return;
        }
        p.transport.is_playing = false;
        self.studio_transport_paused = true;
        self.publish(p);
    }

    pub(crate) fn transport_toggle_play_pause(&mut self) {
        let mut p = (*self.current_project()).clone();
        if p.transport.is_playing {
            p.transport.is_playing = false;
            self.studio_transport_paused = true;
            self.publish(p);
            return;
        }
        if !self.studio_transport_paused {
            self.capture_studio_base();
        }
        self.studio_transport_paused = false;
        p.transport.is_playing = true;
        self.publish(p);
    }

    pub(crate) fn transport_stop(&mut self) {
        let mut p = (*self.current_project()).clone();
        let restore_view = p.transport.is_playing || self.studio_transport_paused;
        let return_to = self.studio_return_secs();
        p.transport.is_playing = false;
        p.transport.stop_generation = p.transport.stop_generation.wrapping_add(1);
        p.transport.stop_return_secs = return_to;
        self.seek_pending.store(false, Ordering::Release);
        self.publish(p);
        self.studio_transport_paused = false;
        self.playhead_bits
            .store(return_to.to_bits(), Ordering::Relaxed);
        if restore_view {
            self.timeline_scroll_px = self.studio_base_scroll_px.max(0.0);
        }
        self.follow_playhead_suspended = false;
        self.marker_drag = None;
    }

    fn capture_studio_base(&mut self) {
        let t = self.playhead_secs();
        self.studio_base_secs = if t.is_finite() { t.max(0.0) } else { 0.0 };
        self.studio_base_scroll_px = self.timeline_scroll_px.max(0.0);
    }

    fn studio_return_secs(&self) -> f32 {
        let t = self.studio_base_secs;
        if t.is_finite() {
            t.max(0.0)
        } else {
            0.0
        }
    }

    /// Move the studio playhead. While fully stopped, this also moves the return cursor.
    pub(crate) fn seek_studio(&mut self, secs: f32) {
        let t = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        let playing = self.current_project().transport.is_playing;
        if !playing && !self.studio_transport_paused {
            self.studio_base_secs = t;
        }
        self.request_seek(t);
        self.playhead_bits.store(t.to_bits(), Ordering::Relaxed);
    }

    fn open_load_browser(&mut self, append: bool) {
        let dir = self.sound_library_dir.clone();
        self.load_browser = Some(LoadBrowser::open(append, dir.as_deref()));
    }

    fn close_load_browser(&mut self) {
        self.load_browser = None;
        self.stop_browser_playback();
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

    fn enqueue_audio_path(&mut self, path: PathBuf, kind: AudioLoadKind) {
        if !Self::is_supported_audio_path(&path) {
            let msg = format!(
                "Нужен WAV или MP3: {}",
                path.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("файл")
            );
            self.status = msg;
            return;
        }
        self.pending_loads.push_back(PendingAudio {
            path,
            kind,
            audition_ticket: if kind == AudioLoadKind::Audition {
                self.audition_ticket
            } else {
                0
            },
        });
        self.start_next_load_if_idle();
    }

    fn start_next_load_if_idle(&mut self) {
        if self.load_rx.is_some() {
            return;
        }
        let Some(pending) = self.pending_loads.pop_front() else {
            return;
        };
        let (tx, rx) = mpsc::channel();
        self.load_rx = Some(rx);
        self.load_kind = pending.kind;
        self.load_ticket = pending.audition_ticket;
        let path = pending.path;
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
            self.enqueue_audio_path(path, AudioLoadKind::Append);
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
                if self.load_kind == AudioLoadKind::Audition {
                    if self.load_browser.is_some() && self.load_ticket == self.audition_ticket {
                        self.start_file_audition(sample, label);
                    }
                    self.start_next_load_if_idle();
                    return;
                }
                let append = self.load_kind == AudioLoadKind::Append;
                let mut p = (*self.current_project()).clone();
                let track = self.sampler_track.or(self.selected_track);
                if let Some(track) = track {
                    let had_sample = p.track(track).and_then(|t| t.sample.as_ref()).is_some();
                    if append && had_sample {
                        match project_actions::append_track_sample(&mut p, track, sample, label.clone())
                        {
                            Ok(added) => {
                                let slot = project_actions::bind_next_pad_range(
                                    &mut p,
                                    track,
                                    added.start_index,
                                    added.end_index,
                                );
                                self.sampler_view_start = added.start_index as f32;
                                self.sampler_view_len =
                                    (added.end_index - added.start_index).max(1) as f32;
                                self.status = append_status(&label, slot, added.resampled);
                                self.publish(p);
                            }
                            Err(e) => self.status = e,
                        }
                    } else {
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
                        if append {
                            let end = n as usize;
                            let slot =
                                project_actions::bind_next_pad_range(&mut p, track, 0, end);
                            let name = p
                                .track(track)
                                .map(|t| t.name.clone())
                                .unwrap_or_else(|| "трек".into());
                            self.status = match slot {
                                Some(slot) => format!(
                                    "{label} → {name} · пэд {}",
                                    sampler::PAD_LABELS[slot as usize]
                                ),
                                None => format!("{label} → {name}"),
                            };
                        } else {
                            let name = p
                                .track(track)
                                .map(|t| t.name.clone())
                                .unwrap_or_else(|| "трек".into());
                            self.status = format!("{label} → {name}");
                        }
                        self.publish(p);
                    }
                } else {
                    self.status = "Сначала добавьте трек в студии.".into();
                }
            }
            Some(Err(e)) => {
                if self.load_kind == AudioLoadKind::Audition
                    && self.load_ticket == self.audition_ticket
                {
                    if let Some(browser) = &mut self.load_browser {
                        browser.playing = None;
                    }
                }
                self.status = e;
            }
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
        if self.load_browser.is_some() {
            self.handle_browser_shortcuts(ctx);
            return;
        }
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
                self.open_load_browser(true);
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
                self.transport_pause();
            } else if space {
                self.transport_on_space();
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
            self.transport_pause();
        } else if space {
            self.transport_on_space();
        } else if open_wav {
            self.open_load_browser(false);
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
        self.studio_base_secs = t;
        self.studio_base_scroll_px = self.timeline_scroll_px.max(0.0);
        self.studio_transport_paused = false;
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
        self.studio_base_secs = 0.0;
        self.studio_base_scroll_px = 0.0;
        self.studio_transport_paused = false;
        self.status.clear();
        self.request_seek(0.0);
        self.playhead_bits.store(0.0f32.to_bits(), Ordering::Relaxed);
    }

    pub(crate) fn go_home(&mut self) {
        self.transport_stop();
        self.close_seq_editor();
        self.close_sampler();
        if self.load_browser.is_some() {
            self.close_load_browser();
        }
        self.settings_open = false;
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
        self.seq_pps = 0.0;
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
        self.delete_seq(id);
    }

    pub(crate) fn delete_seq(&mut self, id: crate::model::SeqId) {
        if self.seq_editor == Some(id) {
            self.close_seq_editor();
        }
        let mut p = (*self.current_project()).clone();
        if project_actions::delete_seq_clip(&mut p, id) {
            if self.selected_seq == Some(id) {
                self.selected_seq = None;
            }
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
            pianoroll::PianoRollAction::DuplicateNote {
                source,
                slot,
                time,
                grab_offset_secs,
            } => {
                let mut p = (*self.current_project()).clone();
                match project_actions::duplicate_pad_note(&mut p, source, time, slot) {
                    Some(id) => {
                        self.selected_note = Some(id);
                        self.seq_note_drag = Some(pianoroll::NoteDrag::Move {
                            id,
                            grab_offset_secs,
                        });
                        self.status.clear();
                        self.publish(p);
                    }
                    None => {
                        let label = sampler::PAD_LABELS.get(slot as usize).copied().unwrap_or("?");
                        self.status = format!("Сначала нарежьте пэд {label} в инструменте сэмплинга");
                    }
                }
            }
            pianoroll::PianoRollAction::ResizeNoteStart { id, start } => {
                let mut p = (*self.current_project()).clone();
                let changed = project_actions::resize_pad_note_start(&mut p, id, start);
                self.selected_note = Some(id);
                if changed {
                    self.publish(p);
                }
            }
            pianoroll::PianoRollAction::ResizeNote { id, end } => {
                let mut p = (*self.current_project()).clone();
                let changed = project_actions::resize_pad_note(&mut p, id, end);
                self.selected_note = Some(id);
                if changed {
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
        let speed = track.playback_speed();
        let sample_hold = track.sample.as_ref().map(|sample| {
            (
                Arc::clone(&sample.data),
                Arc::clone(&sample.peaks),
                sample.rate(),
                speed,
            )
        });
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
            Some(self.playhead_secs() - seq.start_time_secs)
        } else {
            None
        };
        let status = self.status.clone();
        let sample = sample_hold
            .as_ref()
            .map(|(data, peaks, rate, speed)| (data.as_slice(), peaks.as_ref(), *rate, *speed));
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
            sample,
        };
        let action = pianoroll::show(
            ctx,
            model,
            &mut self.seq_view_start,
            &mut self.seq_pps,
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
        self.clear_tempo_detect();
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
        self.clear_tempo_detect();
    }

    fn clear_tempo_detect(&mut self) {
        self.sampler_tempo_gen = self.sampler_tempo_gen.wrapping_add(1);
        self.sampler_tempo_rx = None;
        self.sampler_tempo = sampler::TempoDetectState::default();
    }

    fn start_tempo_detect(&mut self) {
        let Some(track_id) = self.sampler_track else {
            return;
        };
        let proj = self.current_project();
        let Some(sample) = proj.track(track_id).and_then(|t| t.sample.as_ref()) else {
            return;
        };
        let data = Arc::clone(&sample.data);
        let rate = sample.rate();
        let len = data.len();
        drop(proj);
        self.sampler_tempo_gen = self.sampler_tempo_gen.wrapping_add(1);
        let generation = self.sampler_tempo_gen;
        let (tx, rx) = mpsc::channel();
        self.sampler_tempo_rx = Some(rx);
        self.sampler_tempo.running = true;
        self.sampler_tempo.message = "Считаю темп…".into();
        self.sampler_tempo.detail.clear();
        self.sampler_tempo.alternatives.clear();
        self.sampler_tempo.beats.clear();
        self.sampler_tempo.downbeats.clear();
        self.sampler_tempo.bpm = None;
        self.sampler_tempo.sample_len = len;
        std::thread::spawn(move || {
            let analysis = crate::musical_time::analyze(
                &data,
                rate,
                crate::musical_time::AnalysisMode::Deep,
            );
            let _ = tx.send((generation, analysis));
        });
    }

    fn poll_tempo_detect(&mut self) {
        let received = self.sampler_tempo_rx.as_ref().and_then(|rx| match rx.try_recv() {
            Ok(message) => Some(Ok(message)),
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => Some(Err(())),
        });
        match received {
            Some(Ok((generation, analysis))) => {
                self.sampler_tempo_rx = None;
                if generation == self.sampler_tempo_gen && self.sampler_track.is_some() {
                    self.finish_tempo_detect(analysis);
                } else {
                    self.sampler_tempo.running = false;
                }
            }
            Some(Err(())) => {
                self.sampler_tempo_rx = None;
                self.sampler_tempo.running = false;
                if self.sampler_tempo.message == "Считаю темп…" {
                    self.sampler_tempo.message = "Анализ прервался".into();
                }
            }
            None => {}
        }
    }

    fn finish_tempo_detect(&mut self, analysis: crate::musical_time::MusicalTimeAnalysis) {
        let current_len = self
            .sampler_track
            .and_then(|id| self.current_project().track(id).and_then(|t| t.sample.as_ref()).map(|s| s.data.len()));
        self.sampler_tempo.running = false;
        if current_len != Some(self.sampler_tempo.sample_len) {
            self.sampler_tempo.message = "Сэмпл изменился во время анализа".into();
            return;
        }
        self.sampler_tempo.bpm = analysis.tempo.bpm;
        self.sampler_tempo.confidence = analysis.tempo.confidence;
        self.sampler_tempo.beats = analysis.beats.iter().map(|beat| beat.time_secs).collect();
        self.sampler_tempo.downbeats = analysis.downbeats.iter().map(|beat| beat.time_secs).collect();
        let chosen = analysis.tempo.bpm;
        self.sampler_tempo.alternatives = analysis
            .tempo
            .alternatives
            .iter()
            .filter(|candidate| {
                chosen
                    .map(|bpm| (candidate.bpm - bpm).abs() > 0.4)
                    .unwrap_or(true)
            })
            .take(4)
            .map(|candidate| sampler::DetectedTempo {
                bpm: candidate.bpm,
                confidence: candidate.confidence,
            })
            .collect();
        self.sampler_tempo.message = tempo_detect_message(&analysis);
        self.sampler_tempo.detail = analysis.diagnostics.notes.join("\n");
        if let Some(bpm) = analysis.tempo.bpm {
            self.apply_detected_tempo(bpm);
        }
    }

    fn apply_detected_tempo(&mut self, bpm: f32) {
        let mut project = (*self.current_project()).clone();
        if project_actions::set_tempo(&mut project, bpm) {
            self.publish(project);
        }
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
        p.audition.playing = false;
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

    fn reorder_sampler_slice(&mut self, from: usize, to: usize) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let cursor = self.sampler_cursor_sample_index(track);
        let mut p = (*self.current_project()).clone();
        let old = p
            .track(track)
            .map(|t| t.sample_slices.clone())
            .unwrap_or_default();
        if !project_actions::reorder_track_slice(&mut p, track, from, to) {
            return;
        }
        let mapped = cursor.map(|idx| {
            project_actions::sample_index_after_slice_reorder(&old, from, to, idx)
        });
        self.finish_slice_edit(p, track, mapped);
    }

    fn delete_sampler_slice(&mut self, index: usize) {
        let Some(track) = self.sampler_track else {
            return;
        };
        let cursor = self.sampler_cursor_sample_index(track);
        let mut p = (*self.current_project()).clone();
        let old = p
            .track(track)
            .map(|t| t.sample_slices.clone())
            .unwrap_or_default();
        if !project_actions::delete_track_slice(&mut p, track, index) {
            return;
        }
        let mapped = cursor.and_then(|idx| {
            project_actions::sample_index_after_slice_delete(&old, index, idx)
        });
        self.finish_slice_edit(p, track, mapped);
    }

    fn finish_slice_edit(&mut self, mut project: Project, track: TrackId, cursor: Option<usize>) {
        if let Some(slot) = self.sampler_selected_pad {
            let kept = project
                .track(track)
                .is_some_and(|t| t.pad_markers.iter().any(|m| m.slot == slot));
            if !kept {
                self.sampler_selected_pad = None;
            }
        }
        self.sampler_active_pad = None;
        project.sampler_preview.playing = false;
        project.sampler_preview.end_secs = None;
        let secs = match cursor {
            Some(idx) => {
                let rate = project
                    .track(track)
                    .and_then(|t| t.sample.as_ref())
                    .map(|s| s.rate())
                    .unwrap_or(1);
                let speed = project.track_speed(track);
                project_actions::sample_index_to_preview_secs(idx, rate, speed)
            }
            None => 0.0,
        };
        self.sampler_base_secs = secs;
        if project.sample_len(track) == 0 {
            self.sampler_view_start = 0.0;
            self.sampler_view_len = 1.0;
        }
        self.engine.seek_preview_secs(secs);
        self.publish(project);
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
            sampler::SamplerAction::LoadFile => self.open_load_browser(false),
            sampler::SamplerAction::AddSounds => self.open_load_browser(true),
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
            sampler::SamplerAction::ReorderSlice { from, to } => self.reorder_sampler_slice(from, to),
            sampler::SamplerAction::DeleteSlice { index } => self.delete_sampler_slice(index),
            sampler::SamplerAction::SelectPad { slot } => {
                self.sampler_selected_pad = slot;
            }
            sampler::SamplerAction::TriggerPad { slot } => self.trigger_sampler_pad(slot),
            sampler::SamplerAction::DetectTempo => self.start_tempo_detect(),
            sampler::SamplerAction::ApplyTempo(bpm) => self.apply_detected_tempo(bpm),
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
        self.poll_tempo_detect();
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
        let audition_playing = proj.audition.playing;
        let preview_end = proj.sampler_preview.end_secs;
        let sample_buf = track.sample.as_ref().map(|s| {
            (
                Arc::clone(&s.data),
                Arc::clone(&s.peaks),
                s.rate(),
            )
        });
        let pad_markers = track.pad_markers.clone();
        let sample_slices = track.sample_slices.clone();
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
        if !preview_playing && !audition_playing {
            self.engine
                .seek_preview_secs(self.sampler_base_secs_clamped());
        }
        let status = self.status.clone();
        let sample_rate = sample_buf.as_ref().map(|(_, _, r)| *r).unwrap_or(1);
        if let Some((data, _, _)) = &sample_buf {
            if !self.sampler_tempo.running
                && self.sampler_tempo.sample_len != 0
                && self.sampler_tempo.sample_len != data.len()
            {
                self.sampler_tempo = sampler::TempoDetectState::default();
            }
        }
        if self.sampler_tempo.running {
            ctx.request_repaint();
        }
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
            slices: &sample_slices,
            selected_pad: self.sampler_selected_pad,
            active_pad: self.sampler_active_pad,
            tempo_detect: &self.sampler_tempo,
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

    fn publish_live(&mut self, project: Project) {
        self.project_swap.store(Arc::new(project));
    }

    fn start_file_audition(&mut self, sample: crate::model::Sample, label: String) {
        let end = sample.data.len() as f32 / sample.rate() as f32;
        let mut p = (*self.current_project()).clone();
        p.sampler_preview.playing = false;
        p.sampler_preview.end_secs = None;
        p.audition.sample = Some(sample);
        p.audition.label = label;
        p.audition.end_secs = end.max(0.0);
        p.audition.playing = true;
        p.audition.generation = p.audition.generation.wrapping_add(1);
        self.sampler_active_pad = None;
        self.engine.seek_preview_secs(0.0);
        self.publish_live(p);
    }

    fn cancel_pending_audition(&mut self) {
        self.audition_ticket = self.audition_ticket.wrapping_add(1);
    }

    fn audition_load_pending(&self) -> bool {
        let inflight = self.load_rx.is_some()
            && self.load_kind == AudioLoadKind::Audition
            && self.load_ticket == self.audition_ticket;
        let queued = self
            .pending_loads
            .iter()
            .any(|job| job.kind == AudioLoadKind::Audition && job.audition_ticket == self.audition_ticket);
        inflight || queued
    }

    fn stop_browser_playback(&mut self) {
        self.cancel_pending_audition();
        if let Some(browser) = &mut self.load_browser {
            browser.playing = None;
        }
        let mut p = (*self.current_project()).clone();
        let audition = p.audition.playing;
        let preview = p.sampler_preview.playing;
        if !audition && !preview {
            return;
        }
        p.audition.playing = false;
        p.sampler_preview.playing = false;
        p.sampler_preview.end_secs = None;
        self.sampler_active_pad = None;
        if preview {
            self.publish(p);
        } else {
            self.publish_live(p);
        }
        if self.sampler_track.is_some() {
            self.engine
                .seek_preview_secs(self.sampler_base_secs_clamped());
        }
    }

    fn handle_browser_shortcuts(&mut self, ctx: &egui::Context) {
        let (escape, space) = ctx.input(|i| {
            (
                i.key_pressed(Key::Escape),
                i.key_pressed(Key::Space) && !i.modifiers.ctrl,
            )
        });
        if escape {
            self.close_load_browser();
        } else if space {
            self.browser_space();
        }
    }

    fn browser_space(&mut self) {
        let Some(browser) = &self.load_browser else {
            return;
        };
        if browser.playing.is_some() {
            self.stop_browser_playback();
            return;
        }
        let Some(index) = browser.selected else {
            return;
        };
        let path = match browser.rows.get(index) {
            Some(browser::BrowserRow::Sound { path, .. }) => path.clone(),
            _ => return,
        };
        self.play_browser_file(path);
    }

    fn play_browser_file(&mut self, path: PathBuf) {
        self.cancel_pending_audition();
        if let Some(browser) = &mut self.load_browser {
            browser.playing = Some(path.clone());
        }
        self.enqueue_audio_path(path, AudioLoadKind::Audition);
    }

    fn sync_browser_playback(&mut self) {
        if self
            .load_browser
            .as_ref()
            .and_then(|b| b.playing.as_ref())
            .is_none()
        {
            return;
        }
        let proj = self.current_project();
        let audition_on = proj.audition.playing;
        let audition_end = proj.audition.end_secs;
        drop(proj);
        if !audition_on || !audition_end.is_finite() || self.engine.preview_secs() < audition_end {
            return;
        }
        if self.audition_load_pending() {
            let mut p = (*self.current_project()).clone();
            p.audition.playing = false;
            self.publish_live(p);
        } else {
            self.stop_file_audition_ui();
        }
    }

    fn stop_file_audition_ui(&mut self) {
        if let Some(browser) = &mut self.load_browser {
            browser.playing = None;
        }
        let mut p = (*self.current_project()).clone();
        if p.audition.playing {
            p.audition.playing = false;
            self.publish_live(p);
        }
    }

    fn show_load_browser(&mut self, ctx: &egui::Context) {
        self.sync_browser_playback();
        let Some(browser) = &mut self.load_browser else {
            return;
        };
        let action = browser::show(ctx, browser);
        let Some(action) = action else {
            return;
        };
        match action {
            BrowserAction::Close => self.close_load_browser(),
            BrowserAction::Stop => self.stop_browser_playback(),
            BrowserAction::PlayFile(path) => self.play_browser_file(path),
            BrowserAction::AddFile(path) => {
                let append = self
                    .load_browser
                    .as_ref()
                    .map(|b| b.append)
                    .unwrap_or(true);
                self.enqueue_audio_path(
                    path,
                    if append {
                        AudioLoadKind::Append
                    } else {
                        AudioLoadKind::Replace
                    },
                );
            }
        }
    }
}

fn append_status(label: &str, slot: Option<u8>, resampled: bool) -> String {
    let mut status = match slot {
        Some(slot) => format!(
            "{label} добавлен после текущих · пэд {}",
            sampler::PAD_LABELS[slot as usize]
        ),
        None => format!("{label} добавлен после текущих · свободных пэдов нет"),
    };
    if resampled {
        status.push_str(" · частота приведена к треку");
    }
    status
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
        if self.load_browser.is_some() {
            self.show_load_browser(ctx);
        }
        if self.settings_open && self.screen == AppScreen::Studio {
            self.show_settings_window(ctx);
        }

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.persist_open_on_exit();
    }
}

fn tempo_detect_message(analysis: &crate::musical_time::MusicalTimeAnalysis) -> String {
    if let Some(bpm) = analysis.tempo.bpm {
        format!(
            "{} · уверенность {:.0}%",
            crate::time::format_bpm(bpm),
            analysis.tempo.confidence * 100.0
        )
    } else if let Some(reason) = analysis.tempo.unknown_reason {
        format!("Темп не определён: {}", unknown_reason_ru(reason))
    } else {
        "Темп не определён".into()
    }
}

fn unknown_reason_ru(reason: crate::musical_time::UnknownReason) -> &'static str {
    use crate::musical_time::UnknownReason;
    match reason {
        UnknownReason::InsufficientPeriodicity => "недостаточно периодичности",
        UnknownReason::LowRhythmicity => "слабый ритм",
        UnknownReason::InsufficientDuration => "слишком короткий фрагмент",
        UnknownReason::AmbiguousTempo => "неоднозначный темп",
        UnknownReason::Silent => "тишина",
    }
}
