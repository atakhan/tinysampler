use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;

use arc_swap::ArcSwap;
use egui::{Color32, CursorIcon, Key, Pos2, Rect, Stroke, Vec2};

use crate::model::{ClipId, Project, Sample};
use crate::project_actions;
use crate::theme;
use crate::timeline::{self, TrimDrag};
use crate::waveform::PeakPyramid;

use crate::audio;

#[derive(Clone)]
struct ClipSettleAnim {
    clip_id: ClipId,
    from_secs: f32,
    to_secs: f32,
    t0: f64,
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
    ruler_kind: timeline::RulerKind,
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
            status: String::new(),
            timeline_scroll_px: 0.0,
            selected_clip_id: None,
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
            ruler_kind: timeline::RulerKind::Time,
        })
    }

    fn publish(&mut self, project: Project) {
        self.project_swap.store(Arc::new(project));
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
                let track = project_actions::append_audio_clip(&mut p, sample, label.clone());
                self.status = format!("{label} → дорожка {}", track + 1);
                self.publish(p);
            }
            Some(Err(e)) => self.status = e,
            None => self.status = "Загрузка прервалась.".into(),
        }
        self.start_next_load_if_idle();
    }

    fn try_split_clip_at_playhead(&mut self) {
        let mut p = (*self.current_project()).clone();
        self.clip_settle_anim = None;
        project_actions::resolve_all_placement_previews(&mut p);
        match project_actions::split_clip_at_playhead(
            &mut p,
            self.playhead_secs(),
            self.selected_clip_id,
        ) {
            Ok(id) => {
                self.selected_clip_id = Some(id);
                self.trim_drag = None;
                self.clip_move_drag = None;
                self.clip_move_from_alt_duplicate = false;
                self.clip_settle_anim = None;
                self.status.clear();
                self.publish(p);
            }
            Err(e) => {
                self.status = e;
                self.publish(p);
            }
        }
    }

    fn delete_selected_clip(&mut self) {
        let Some(id) = self.selected_clip_id else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if !project_actions::delete_clip(&mut p, id) {
            self.selected_clip_id = None;
            return;
        }
        self.selected_clip_id = None;
        self.trim_drag = None;
        self.clip_move_drag = None;
        self.clip_move_from_alt_duplicate = false;
        self.clip_settle_anim = None;
        self.status.clear();
        self.publish(p);
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (ctrl_space, space, open_wav, split_playhead, delete_clip, place_marker, play_marker_slot) =
            ctx.input(|i| {
                let mods = i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt;
                let space = i.key_pressed(Key::Space);
                let open_wav = i.key_pressed(Key::O) && (i.modifiers.ctrl || i.modifiers.command);
                let split_playhead =
                    i.key_pressed(Key::K) && (i.modifiers.ctrl || i.modifiers.command);
                let delete_clip = i.key_pressed(Key::Delete);
                let place_marker = i.key_pressed(Key::M) && !mods;
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
                )
            });
        if ctrl_space {
            self.transport_stop();
        } else if space {
            self.transport_toggle_play_pause();
        } else if open_wav {
            self.try_pick_and_load_audio();
        } else if split_playhead {
            self.try_split_clip_at_playhead();
        } else if delete_clip {
            if self.selected_marker.is_some() {
                self.delete_selected_marker();
            } else {
                self.delete_selected_clip();
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
        self.poll_audio_load(ctx);
        self.poll_dropped_files(ctx);
        if let Some(msg) = self.engine.recover_if_needed(&self.project_swap) {
            self.status = msg;
        }
        self.handle_global_shortcuts(ctx);

        let btn = theme::TRANSPORT_BTN_DIAMETER;
        let btn_gap = theme::TRANSPORT_BTN_GAP;

        egui::TopBottomPanel::top("tempo_bar")
            .exact_height(theme::TEMPO_BAR_HEIGHT)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("Темп").weak());
                    let mut bpm = self.current_project().tempo_bpm;
                    let tempo_edit = ui.add(
                        egui::DragValue::new(&mut bpm)
                            .suffix(" BPM")
                            .range(20.0..=400.0)
                            .speed(0.25)
                            .min_decimals(0)
                            .max_decimals(1),
                    );
                    if tempo_edit.changed() {
                        let mut p = (*self.current_project()).clone();
                        p.tempo_bpm = bpm.clamp(20.0, 400.0);
                        self.publish(p);
                    }
                    ui.add_space(16.0);
                    ui.label(egui::RichText::new("Линейка").weak());
                    if ui
                        .selectable_label(
                            self.ruler_kind == timeline::RulerKind::Time,
                            "Время",
                        )
                        .clicked()
                    {
                        self.ruler_kind = timeline::RulerKind::Time;
                    }
                    if ui
                        .selectable_label(
                            self.ruler_kind == timeline::RulerKind::Tempo,
                            "Темп",
                        )
                        .clicked()
                    {
                        self.ruler_kind = timeline::RulerKind::Tempo;
                    }
                });
            });

        egui::TopBottomPanel::bottom("transport")
            .exact_height(btn + theme::TRANSPORT_RESERVE_H)
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        let total_w = btn * 3.0 + btn_gap * 2.0;
                        ui.add_space(((ui.available_width() - total_w) * 0.5).max(0.0));

                        if timeline::round_transport_btn(
                            ui,
                            "+",
                            "Load audio (Ctrl+O)",
                            theme::color_transport_load(),
                            btn,
                        )
                        .clicked()
                        {
                            self.try_pick_and_load_audio();
                        }
                        ui.add_space(btn_gap);
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
                        ui.add_space(btn_gap);
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
                    });
                    if !self.status.is_empty() {
                        ui.add_space(2.0);
                        ui.label(egui::RichText::new(&self.status).weak().size(12.0));
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
                // Spare empty lane under the last clip so the next drop has a visible target.
                let n_lanes = proj.track_count().saturating_add(1).max(2);
                self.selected_track = self.selected_track.min(n_lanes.saturating_sub(1));
                let gutter_w = theme::TRACK_GUTTER_WIDTH;
                let avail_for_lanes = (ui.available_height() - theme::TIME_RULER_HEIGHT).max(80.0);
                let block_h = (avail_for_lanes / n_lanes as f32).clamp(
                    theme::MARKER_LANE_HEIGHT + 72.0,
                    theme::TIMELINE_TRACK_HEIGHT + theme::MARKER_LANE_HEIGHT,
                );
                let timeline_height = block_h * n_lanes as f32;
                let marker_end = proj
                    .markers
                    .iter()
                    .map(|m| m.time_secs)
                    .fold(0.0f32, f32::max);
                let end_secs = proj
                    .clips
                    .iter()
                    .map(|c| c.start_time_secs + c.timeline_duration_secs(proj.device_sample_rate))
                    .fold(4.0f32, f32::max)
                    .max(self.playhead_secs() + 0.5)
                    .max(marker_end + 0.5);

                let stack_origin = ui.cursor().min;
                let combined_rect = Rect::from_min_size(
                    stack_origin,
                    Vec2::new(viewport_w, theme::TIME_RULER_HEIGHT + timeline_height),
                );
                if let Some(hp) = ctx.pointer_hover_pos() {
                    if combined_rect.contains(hp) {
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
                let ruler_gutter = Rect::from_min_size(
                    ruler_row.min,
                    Vec2::new(gutter_w, ruler_row.height()),
                );
                let ruler_rect = Rect::from_min_max(
                    Pos2::new(ruler_row.left() + gutter_w, ruler_row.top()),
                    ruler_row.max,
                );
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
                    egui::Sense::click_and_drag(),
                );

                self.tick_clip_settle_anim(ctx);

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
                }
                if ctx.input(|i| i.pointer.primary_pressed())
                    && self.trim_drag.is_none()
                    && self.clip_move_drag.is_none()
                    && self.marker_drag.is_none()
                {
                    let proj_now = self.current_project();
                    if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        if layout.gutter_contains(pos) {
                            self.selected_track = layout.lane_at_y(pos.y);
                            self.selected_clip_id = None;
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
                        } else if combined_rect.contains(pos)
                            && layout.content_rect().contains(pos)
                            && layout
                                .clips_rect(layout.lane_at_y(pos.y))
                                .contains(pos)
                        {
                            let sel = self.selected_clip_id.filter(|id| proj_now.clip_index(*id).is_some());
                            if let Some(sel) = sel {
                                if let Some(d) = timeline::trim_hit_test(
                                    &proj_now,
                                    sel,
                                    pos,
                                    &layout,
                                    view_left,
                                    pps,
                                    self.timeline_scroll_px,
                                    proj_now.device_sample_rate,
                                ) {
                                    self.trim_drag = Some(d);
                                }
                            }
                            if self.trim_drag.is_none() {
                                if let Some(id) = timeline::clip_id_at_pointer(
                                    &proj_now,
                                    pos,
                                    &layout,
                                    view_left,
                                    pps,
                                    self.timeline_scroll_px,
                                    proj_now.device_sample_rate,
                                ) {
                                    self.selected_clip_id = Some(id);
                                    self.selected_marker = None;
                                    if let Some(i) = proj_now.clip_index(id) {
                                        self.selected_track = proj_now.clips[i].track_index;
                                    }
                                    let alt = ctx.input(|i| i.modifiers.alt);
                                    if alt {
                                        let mut p = (*proj_now).clone();
                                        if let Some(new_id) =
                                            project_actions::duplicate_clip(&mut p, id)
                                        {
                                            self.selected_clip_id = Some(new_id);
                                            self.clip_move_from_alt_duplicate = true;
                                            self.clip_move_drag = Some(new_id);
                                            self.publish(p);
                                        } else {
                                            self.begin_clip_body_drag(&proj_now, id, false);
                                        }
                                    } else {
                                        self.begin_clip_body_drag(&proj_now, id, false);
                                    }
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
                    } else if timeline::pointer_near_trim_handle(
                        &proj,
                        self.selected_clip_id,
                        hp,
                        &layout,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                        proj.device_sample_rate,
                    ) {
                        ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                    } else if timeline::pointer_on_selected_clip_move_body(
                        &proj,
                        self.selected_clip_id,
                        hp,
                        &layout,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                        proj.device_sample_rate,
                    ) {
                        if ctx.input(|i| i.modifiers.alt) {
                            ctx.set_cursor_icon(CursorIcon::Alias);
                        } else {
                            ctx.set_cursor_icon(CursorIcon::Move);
                        }
                    } else if layout.gutter_contains(hp) {
                        ctx.set_cursor_icon(CursorIcon::PointingHand);
                    } else if pan_resp.hovered() {
                        ctx.set_cursor_icon(CursorIcon::Grab);
                    }
                }

                if pan_resp.clicked() {
                    if let Some(p) = pan_resp.interact_pointer_pos() {
                        let sc = self.timeline_scroll_px;
                        if let Some(id) = timeline::clip_id_at_pointer(
                            &proj,
                            p,
                            &layout,
                            view_left,
                            pps,
                            sc,
                            proj.device_sample_rate,
                        ) {
                            self.selected_clip_id = Some(id);
                            self.selected_marker = None;
                            if let Some(i) = proj.clip_index(id) {
                                self.selected_track = proj.clips[i].track_index;
                            }
                        } else if layout.gutter_contains(p) {
                            self.selected_track = layout.lane_at_y(p.y);
                            self.selected_clip_id = None;
                            self.selected_marker = None;
                        } else {
                            self.selected_clip_id = None;
                            let time_at =
                                |x: f32| -> f32 { (((x - view_left) + sc) / pps).max(0.0) };
                            if (ruler_rect.contains(p)
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

                let ruler_row_painter = ui.painter_at(ruler_row);
                ruler_row_painter.rect_filled(ruler_gutter, 0.0, theme::color_track_gutter());
                let ruler_painter = ui.painter_at(ruler_rect);
                timeline::paint_ruler(
                    &ruler_painter,
                    ruler_rect,
                    pps,
                    scroll,
                    ctx,
                    self.ruler_kind,
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
                    timeline::paint_track_gutter(
                        &painter,
                        layout.gutter_rect(lane),
                        lane,
                        lane == self.selected_track,
                    );
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

                let mut clip_draw_order: Vec<usize> = (0..proj.clips.len()).collect();
                clip_draw_order.sort_by_key(|&i| proj.clips[i].placement_preview);

                for &i in &clip_draw_order {
                    let clip = &proj.clips[i];
                    let ghost = clip.placement_preview;
                    let clip_rect = timeline::clip_rect_on_timeline(
                        clip,
                        &layout,
                        view_left,
                        pps,
                        scroll,
                        proj.device_sample_rate,
                    );

                    let fill = if ghost {
                        theme::color_clip_bg().gamma_multiply(0.55)
                    } else {
                        theme::color_clip_bg()
                    };
                    painter.rect_filled(clip_rect, 3.0, fill);

                    paint_waveform_overlay(
                        &painter,
                        clip_rect,
                        layout.clips_rect(clip.track_index),
                        &clip.sample.data,
                        &clip.sample.peaks,
                        clip.trim_start,
                        clip.trim_end,
                        ghost,
                    );

                    if self.selected_clip_id == Some(clip.id) && !proj.transport.is_playing {
                        let s = (theme::TRIM_HANDLE_WIDTH_PX * 0.35).min(clip_rect.width() * 0.25);
                        let h_alpha = if ghost { 45 } else { 90 };
                        painter.rect_filled(
                            Rect::from_min_size(
                                clip_rect.left_top(),
                                Vec2::new(s, clip_rect.height()),
                            ),
                            0.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, h_alpha),
                        );
                        painter.rect_filled(
                            Rect::from_min_max(
                                Pos2::new(clip_rect.right() - s, clip_rect.top()),
                                clip_rect.max,
                            ),
                            0.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, h_alpha),
                        );
                    }

                    if !clip.label.is_empty() {
                        let inset = 6.0_f32;
                        let pad = 3.0_f32;
                        let font = egui::FontId::proportional(11.0);
                        let text_col = if ghost {
                            Color32::from_rgba_unmultiplied(255, 255, 255, 200)
                        } else {
                            Color32::WHITE
                        };
                        let galley = ctx
                            .fonts(|f| f.layout_no_wrap(clip.label.clone(), font, text_col));
                        let tw = galley.rect.width();
                        let th = galley.rect.height();
                        let tl = Pos2::new(clip_rect.left() + inset, clip_rect.top() + inset);
                        let bg_min = tl - Vec2::splat(pad);
                        let bg_max = tl + Vec2::new(tw + pad, th + pad);
                        let bg_rect = Rect::from_min_max(bg_min, bg_max);
                        let clip_painter = painter.with_clip_rect(clip_rect);
                        let bg_alpha = if ghost { 110 } else { 175 };
                        clip_painter.rect_filled(
                            bg_rect,
                            2.0,
                            Color32::from_rgba_unmultiplied(0, 0, 0, bg_alpha),
                        );
                        clip_painter.galley(bg_min + Vec2::new(pad, pad), galley, text_col);
                    }

                    if self.selected_clip_id == Some(clip.id) {
                        let stroke = if ghost {
                            Color32::from_rgba_unmultiplied(180, 220, 255, 220)
                        } else {
                            Color32::WHITE
                        };
                        painter.rect_stroke(clip_rect, 3.0, Stroke::new(1.0, stroke));
                    }
                }

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

                if ctx.input(|i| i.pointer.primary_clicked()) {
                    if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        if !combined_rect.contains(pos) {
                            self.selected_clip_id = None;
                            self.selected_marker = None;
                        }
                    }
                }
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}

/// Audacity-style filled min/max envelope (mono). Only paints columns inside `view_rect`.
fn paint_waveform_overlay(
    painter: &egui::Painter,
    clip_rect: Rect,
    view_rect: Rect,
    data: &[f32],
    peaks: &PeakPyramid,
    trim_start: usize,
    trim_end: usize,
    ghost: bool,
) {
    let vis = trim_end.saturating_sub(trim_start);
    if vis == 0 {
        return;
    }
    let vis_rect = clip_rect.intersect(view_rect);
    if vis_rect.width() < 0.5 || vis_rect.height() < 0.5 {
        return;
    }

    let pixel_w = clip_rect.width().max(1.0);
    let vis_f = vis as f32;
    let center_y = clip_rect.center().y;
    let half_h = ((clip_rect.height() - 4.0).max(4.0)) * 0.5;
    let y_of = |s: f32| center_y - s.clamp(-1.0, 1.0) * half_h;

    let mut wave_col = theme::color_clip_waveform();
    let mut zero_col = theme::color_clip_zero_line();
    if ghost {
        wave_col = Color32::from_rgba_unmultiplied(wave_col.r(), wave_col.g(), wave_col.b(), 140);
        zero_col = Color32::from_rgba_unmultiplied(zero_col.r(), zero_col.g(), zero_col.b(), 90);
    }

    let clip_painter = painter.with_clip_rect(vis_rect);
    clip_painter.line_segment(
        [
            Pos2::new(vis_rect.left(), center_y),
            Pos2::new(vis_rect.right(), center_y),
        ],
        Stroke::new(1.0_f32, zero_col),
    );

    let x0 = vis_rect.left().floor() as i32;
    let x1 = vis_rect.right().ceil() as i32;
    for x in x0..x1 {
        let xf = x as f32;
        let u0 = ((xf - clip_rect.left()) / pixel_w).clamp(0.0, 1.0);
        let u1 = ((xf + 1.0 - clip_rect.left()) / pixel_w).clamp(0.0, 1.0);
        if u1 <= u0 {
            continue;
        }
        let s0 = trim_start as f32 + u0 * vis_f;
        let s1 = trim_start as f32 + u1 * vis_f;
        let (mn, mx) = if (s1 - s0) <= 1.0 {
            let a = crate::waveform::sample_at(data, s0);
            let b = crate::waveform::sample_at(data, s1.max(s0 + 1e-4));
            (a.min(b), a.max(b))
        } else {
            crate::waveform::min_max_range(data, peaks, s0.floor() as usize, s1.ceil() as usize)
        };
        let mut yt = y_of(mx);
        let mut yb = y_of(mn);
        if yb < yt {
            std::mem::swap(&mut yt, &mut yb);
        }
        if yb - yt < 1.0 {
            let mid = (yt + yb) * 0.5;
            yt = mid - 0.5;
            yb = mid + 0.5;
        }
        clip_painter.rect_filled(
            Rect::from_min_max(Pos2::new(xf, yt), Pos2::new(xf + 1.0, yb)),
            0.0,
            wave_col,
        );
    }
}
