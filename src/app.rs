use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use egui::{Color32, CursorIcon, FontId, Key, Pos2, Rect, RichText, Rounding, Sense, Stroke, Vec2};

use cpal::traits::{DeviceTrait, HostTrait};

use crate::model::{Clip, Project};
use crate::{audio, wav_loader};

/// Same range as the "px / sec" slider; Ctrl+wheel on the timeline uses these bounds.
const TIMELINE_PPS_MIN: f32 = 40.0;
const TIMELINE_PPS_MAX: f32 = 300.0;

/// While playing, nudge horizontal scroll if the playhead gets closer than this to a viewport edge.
const PLAYHEAD_EDGE_MARGIN_PX: f32 = 48.0;

/// Time scale bar height at the top of the timeline stack.
const TIME_RULER_HEIGHT: f32 = 30.0;

/// Horizontal hit width for trim handles on a selected clip.
const TRIM_HANDLE_WIDTH_PX: f32 = 10.0;

/// Minimum visible clip length after trim (seconds).
const MIN_TRIM_DURATION_SECS: f32 = 0.08;

#[derive(Clone, Copy)]
enum TrimSide {
    Left,
    Right,
}

#[derive(Clone, Copy)]
struct TrimDrag {
    clip_index: usize,
    side: TrimSide,
}

struct SpecTex {
    key: u64,
    texture: egui::TextureHandle,
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
    /// egui textures for clip spectrograms (rebuilt when sample / spec Arc changes).
    spec_textures: Vec<Option<SpecTex>>,
    /// Horizontal scroll in world px (time * pps).
    /// - Seek click: anchor so the chosen time stays under the pointer.
    /// - While playing: only edge-nudge so the playhead stays inside margins (no snap-to-center).
    timeline_scroll_px: f32,
    /// Selected clip in the timeline (`None` if nothing selected).
    selected_clip_index: Option<usize>,
    /// Active trim drag (handle grabbed); blocks timeline pan scroll.
    trim_drag: Option<TrimDrag>,
    /// Moving a clip along the timeline (body drag while selected); blocks pan.
    clip_move_drag: Option<usize>,
}

impl TinySamplerApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = host
            .default_output_device()
            .ok_or_else(|| "no default output device".to_string())?;
        let config = device.default_output_config().map_err(|e| e.to_string())?;
        let sample_rate = config.sample_rate().0;

        let project_swap = Arc::new(ArcSwap::from_pointee(Project::empty(sample_rate)));
        let playhead_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let seek_pending = Arc::new(AtomicBool::new(false));
        let seek_target_secs_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));

        let engine = audio::spawn_output_stream(
            Arc::clone(&project_swap),
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
            spec_textures: Vec::new(),
            timeline_scroll_px: 0.0,
            selected_clip_index: None,
            trim_drag: None,
            clip_move_drag: None,
        })
    }

    /// During playback, shift scroll minimally so the playhead stays between edge margins.
    fn scroll_keep_playhead_in_view(
        rect: Rect,
        playhead_secs: f32,
        pps: f32,
        max_scroll: f32,
        scroll: f32,
    ) -> f32 {
        let m = PLAYHEAD_EDGE_MARGIN_PX.min(rect.width() * 0.15).max(20.0);
        let ph_px = playhead_secs * pps;
        let play_x = rect.left() + ph_px - scroll;
        let mut s = scroll;
        let left_bound = rect.left() + m;
        let right_bound = rect.right() - m;
        if play_x > right_bound {
            s += play_x - right_bound;
        } else if play_x < left_bound {
            s -= left_bound - play_x;
        }
        s.clamp(0.0, max_scroll)
    }

    fn spec_cache_key(clip: &Clip) -> Option<u64> {
        let sp = clip.sample.spectrogram.as_ref()?;
        let d = &clip.sample.data;
        Some(
            (Arc::as_ptr(d) as usize as u64).wrapping_mul(0x9E37_79B1_97F4_A7C7)
                ^ (Arc::as_ptr(sp) as usize as u64)
                ^ (d.len() as u64).wrapping_shl(17),
        )
    }

    fn sync_spec_textures(&mut self, ctx: &egui::Context, clips: &[Clip]) {
        if self.spec_textures.len() > clips.len() {
            self.spec_textures.truncate(clips.len());
        }
        while self.spec_textures.len() < clips.len() {
            self.spec_textures.push(None);
        }
        for i in 0..clips.len() {
            let clip = &clips[i];
            let Some(spec) = clip.sample.spectrogram.as_ref() else {
                self.spec_textures[i] = None;
                continue;
            };
            let Some(key) = Self::spec_cache_key(clip) else {
                self.spec_textures[i] = None;
                continue;
            };
            let rebuild = match &self.spec_textures[i] {
                None => true,
                Some(e) => e.key != key,
            };
            if rebuild {
                let img =
                    egui::ColorImage::from_rgba_unmultiplied([spec.width, spec.height], &spec.rgba);
                let tex = ctx.load_texture(
                    format!("tinysampler_spec_{i}_{key}"),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.spec_textures[i] = Some(SpecTex { key, texture: tex });
            }
        }
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

    fn transport_toggle_play_pause(&mut self) {
        let mut p = (*self.current_project()).clone();
        p.transport.is_playing = !p.transport.is_playing;
        if p.transport.is_playing {
            self.trim_drag = None;
            self.clip_move_drag = None;
        }
        self.publish(p);
    }

    fn transport_stop(&mut self) {
        let mut p = (*self.current_project()).clone();
        p.transport.is_playing = false;
        p.transport.stop_generation = p.transport.stop_generation.wrapping_add(1);
        self.publish(p);
        self.timeline_scroll_px = 0.0;
        self.trim_drag = None;
        self.clip_move_drag = None;
        self.seek_pending.store(false, Ordering::Release);
    }

    fn try_pick_and_load_wav(&mut self) {
        let sr = self.current_project().device_sample_rate;
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV", &["wav"])
            .pick_file()
        {
            match wav_loader::load_wav_mono_f32(&path, sr) {
                Ok(sample) => {
                    let mut p = (*self.current_project()).clone();
                    let label = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("clip")
                        .to_string();
                    let n = sample.data.len();
                    let clip = Clip {
                        start_time_secs: 0.0,
                        label,
                        sample,
                        trim_start: 0,
                        trim_end: n,
                    };
                    p.clips.push(clip);
                    self.status.clear();
                    self.publish(p);
                }
                Err(e) => self.status = e,
            }
        }
    }

    /// Split the clip under the playhead into two clips at the nearest sample boundary (Ctrl+K).
    fn try_split_clip_at_playhead(&mut self) {
        let t = self.playhead_secs();
        let mut p = (*self.current_project()).clone();
        let sr = p.device_sample_rate;
        let sr_f = sr as f32;
        let min_samples = ((MIN_TRIM_DURATION_SECS * sr_f).ceil() as usize).max(1);

        let idx = p.clips.iter().position(|clip| {
            let t0 = clip.start_time_secs;
            let t1 = t0 + clip.timeline_duration_secs(sr);
            t > t0 && t < t1
        });
        let Some(i) = idx else {
            self.status = "Разрез: поставьте плейхед внутри клипа.".into();
            return;
        };

        let clip = p.clips.remove(i);
        let mut split_at = clip.trim_start + ((t - clip.start_time_secs) * sr_f).round() as usize;
        split_at = split_at
            .max(clip.trim_start + min_samples)
            .min(clip.trim_end.saturating_sub(min_samples));
        if split_at <= clip.trim_start || split_at >= clip.trim_end {
            p.clips.insert(i, clip);
            self.status = "Нельзя разрезать: слишком короткий фрагмент.".into();
            return;
        }

        let split_time = clip.start_time_secs + (split_at - clip.trim_start) as f32 / sr_f;

        let left = Clip {
            start_time_secs: clip.start_time_secs,
            label: clip.label.clone(),
            sample: clip.sample.clone(),
            trim_start: clip.trim_start,
            trim_end: split_at,
        };
        let right = Clip {
            start_time_secs: split_time,
            label: clip.label.clone(),
            sample: clip.sample.clone(),
            trim_start: split_at,
            trim_end: clip.trim_end,
        };

        p.clips.insert(i, left);
        p.clips.insert(i + 1, right);

        match self.selected_clip_index {
            None => {}
            Some(sel) if sel < i => {}
            Some(sel) if sel == i => {
                self.selected_clip_index = Some(i);
            }
            Some(sel) => {
                self.selected_clip_index = Some(sel + 1);
            }
        }

        self.trim_drag = None;
        self.clip_move_drag = None;
        self.status.clear();
        self.publish(p);
    }

    /// Remove the selected clip from the project (Delete).
    fn delete_selected_clip(&mut self) {
        let Some(i) = self.selected_clip_index else {
            return;
        };
        let mut p = (*self.current_project()).clone();
        if i >= p.clips.len() {
            self.selected_clip_index = None;
            return;
        }
        p.clips.remove(i);
        self.selected_clip_index = None;
        self.trim_drag = None;
        self.clip_move_drag = None;
        self.status.clear();
        self.publish(p);
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (ctrl_space, space, open_wav, split_playhead, delete_clip) = ctx.input(|i| {
            let space = i.key_pressed(Key::Space);
            let open_wav = i.key_pressed(Key::O) && (i.modifiers.ctrl || i.modifiers.command);
            let split_playhead = i.key_pressed(Key::K) && (i.modifiers.ctrl || i.modifiers.command);
            let delete_clip = i.key_pressed(Key::Delete);
            (
                space && i.modifiers.ctrl,
                space && !i.modifiers.ctrl,
                open_wav,
                split_playhead,
                delete_clip,
            )
        });
        if ctrl_space {
            self.transport_stop();
        } else if space {
            self.transport_toggle_play_pause();
        } else if open_wav {
            self.try_pick_and_load_wav();
        } else if split_playhead {
            self.try_split_clip_at_playhead();
        } else if delete_clip {
            self.delete_selected_clip();
        }
    }

    /// Circular transport control; icon is centered.
    fn round_transport_btn(
        ui: &mut egui::Ui,
        icon: &str,
        tooltip: &str,
        fill: Color32,
        diameter: f32,
    ) -> egui::Response {
        let r = diameter * 0.5;
        let text = RichText::new(icon)
            .size(diameter * 0.38)
            .color(Color32::WHITE);
        ui.add(
            egui::Button::new(text)
                .min_size(Vec2::splat(diameter))
                .fill(fill)
                .stroke(Stroke::new(1.0, Color32::from_gray(55)))
                .rounding(Rounding::same(r)),
        )
        .on_hover_text(tooltip)
    }

    /// Major tick spacing in seconds for the time ruler (~constant label width on screen).
    fn time_ruler_step_secs(pps: f32) -> f32 {
        const TARGET_MAJOR_PX: f32 = 72.0;
        let approx = (TARGET_MAJOR_PX / pps.max(1.0)).max(1e-5);
        let exp = approx.log10().floor();
        let frac = approx / 10f32.powf(exp);
        let n = if frac <= 1.0 {
            1.0
        } else if frac <= 2.0 {
            2.0
        } else if frac <= 5.0 {
            5.0
        } else {
            10.0
        };
        n * 10f32.powf(exp)
    }

    fn clip_rect_on_timeline(
        clip: &Clip,
        rect: Rect,
        view_left: f32,
        pps: f32,
        scroll: f32,
        sample_rate: u32,
    ) -> Rect {
        let dur = clip.timeline_duration_secs(sample_rate);
        let x0 = view_left + clip.start_time_secs * pps - scroll;
        let w = dur * pps;
        Rect::from_min_size(
            Pos2::new(x0, rect.top() + 20.0),
            Vec2::new(w.max(8.0), rect.height() - 40.0),
        )
    }

    fn trim_hit_test(
        proj: &Project,
        selected_index: usize,
        pos: Pos2,
        rect: Rect,
        view_left: f32,
        pps: f32,
        scroll: f32,
        sample_rate: u32,
    ) -> Option<TrimDrag> {
        let clip = proj.clips.get(selected_index)?;
        let cr = Self::clip_rect_on_timeline(clip, rect, view_left, pps, scroll, sample_rate);
        let hw = TRIM_HANDLE_WIDTH_PX.min(cr.width() * 0.5);
        let left_h = Rect::from_min_size(cr.min, Vec2::new(hw, cr.height()));
        let right_h = Rect::from_min_max(Pos2::new(cr.right() - hw, cr.top()), cr.max);
        if left_h.contains(pos) {
            return Some(TrimDrag {
                clip_index: selected_index,
                side: TrimSide::Left,
            });
        }
        if right_h.contains(pos) {
            return Some(TrimDrag {
                clip_index: selected_index,
                side: TrimSide::Right,
            });
        }
        None
    }

    fn pointer_near_trim_handle(
        proj: &Project,
        selected: Option<usize>,
        pos: Pos2,
        rect: Rect,
        view_left: f32,
        pps: f32,
        scroll: f32,
        sample_rate: u32,
    ) -> bool {
        let Some(si) = selected else {
            return false;
        };
        Self::trim_hit_test(proj, si, pos, rect, view_left, pps, scroll, sample_rate).is_some()
    }

    /// Selected clip body (full rect minus trim handles) — for move hover / drag start.
    fn pointer_on_selected_clip_move_body(
        proj: &Project,
        selected: Option<usize>,
        pos: Pos2,
        rect: Rect,
        view_left: f32,
        pps: f32,
        scroll: f32,
        sample_rate: u32,
    ) -> bool {
        let Some(si) = selected else {
            return false;
        };
        if Self::trim_hit_test(proj, si, pos, rect, view_left, pps, scroll, sample_rate).is_some() {
            return false;
        }
        let clip = match proj.clips.get(si) {
            Some(c) => c,
            None => return false,
        };
        let cr = Self::clip_rect_on_timeline(clip, rect, view_left, pps, scroll, sample_rate);
        cr.contains(pos)
    }

    fn apply_trim_delta(project: &mut Project, drag: TrimDrag, dx_px: f32, pps: f32) -> bool {
        let sr = project.device_sample_rate as f32;
        let min_samples = ((MIN_TRIM_DURATION_SECS * sr).ceil() as usize).max(1);
        let ds_samples = ((dx_px / pps) * sr).round() as i64;
        if ds_samples == 0 {
            return false;
        }
        let clip = match project.clips.get_mut(drag.clip_index) {
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

    fn clip_index_at_pointer(
        proj: &Project,
        p: Pos2,
        rect: Rect,
        view_left: f32,
        pps: f32,
        scroll: f32,
        sample_rate: u32,
    ) -> Option<usize> {
        proj.clips.iter().enumerate().find_map(|(i, clip)| {
            let r = Self::clip_rect_on_timeline(clip, rect, view_left, pps, scroll, sample_rate);
            r.contains(p).then_some(i)
        })
    }

    fn format_ruler_time(secs: f32, step: f32) -> String {
        let secs = secs.max(0.0);
        if step >= 60.0 {
            let s = secs.floor() as i64;
            format!("{}:{:02}", s / 60, s % 60)
        } else if step >= 1.0 {
            format!("{:.0}", secs)
        } else if step >= 0.1 {
            format!("{:.1}", secs)
        } else {
            format!("{:.2}", secs)
        }
    }

    /// Subdivision of one major ruler step: 5 ticks if there is room, else 2, else none.
    fn time_ruler_minor_step(major_step: f32, pps: f32) -> Option<f32> {
        const MIN_MINOR_PX: f32 = 22.0;
        let fifth = major_step / 5.0;
        if fifth * pps >= MIN_MINOR_PX {
            return Some(fifth);
        }
        let half = major_step / 2.0;
        if half * pps >= MIN_MINOR_PX * 0.75 {
            return Some(half);
        }
        None
    }

    fn is_time_ruler_major_tick(t: f32, major_step: f32) -> bool {
        let r = (t / major_step).round();
        (r * major_step - t).abs() < (major_step * 1e-5).max(1e-4)
    }

    fn paint_time_ruler(
        painter: &egui::Painter,
        ruler_rect: Rect,
        pps: f32,
        scroll: f32,
        ctx: &egui::Context,
    ) {
        let bg = Color32::from_rgb(24, 24, 30);
        let line_major = Color32::from_gray(95);
        let line_minor = Color32::from_gray(62);
        let text_col = Color32::from_gray(200);
        painter.rect_filled(ruler_rect, 0.0, bg);
        painter.line_segment(
            [
                Pos2::new(ruler_rect.left(), ruler_rect.bottom()),
                Pos2::new(ruler_rect.right(), ruler_rect.bottom()),
            ],
            Stroke::new(1.0, Color32::from_gray(55)),
        );

        let step = Self::time_ruler_step_secs(pps);
        let span_secs = ruler_rect.width() / pps;
        let t_max = scroll / pps + span_secs + step * 2.0;

        if let Some(minor_step) = Self::time_ruler_minor_step(step, pps) {
            let t_minor0 = (scroll / pps / minor_step).floor() * minor_step;
            let mut tm = t_minor0;
            while tm <= t_max {
                if !Self::is_time_ruler_major_tick(tm, step) {
                    let x = ruler_rect.left() + tm * pps - scroll;
                    if x >= ruler_rect.left() - 1.0 && x <= ruler_rect.right() + 1.0 {
                        painter.line_segment(
                            [
                                Pos2::new(x, ruler_rect.bottom() - 5.0),
                                Pos2::new(x, ruler_rect.bottom()),
                            ],
                            Stroke::new(1.0, line_minor),
                        );
                    }
                }
                tm += minor_step;
            }
        }

        let t_min = (scroll / pps / step).floor() * step;
        let font = FontId::proportional(11.0);

        let mut t = t_min;
        while t <= t_max {
            let x = ruler_rect.left() + t * pps - scroll;
            if x >= ruler_rect.left() - 1.0 && x <= ruler_rect.right() + 1.0 {
                painter.line_segment(
                    [
                        Pos2::new(x, ruler_rect.bottom() - 10.0),
                        Pos2::new(x, ruler_rect.bottom()),
                    ],
                    Stroke::new(1.0, line_major),
                );
                let label = Self::format_ruler_time(t, step);
                let galley = ctx.fonts(|f| f.layout_no_wrap(label, font.clone(), text_col));
                let tw = galley.rect.width();
                let tx =
                    (x - tw * 0.5).clamp(ruler_rect.left() + 2.0, ruler_rect.right() - tw - 2.0);
                painter.galley(Pos2::new(tx, ruler_rect.top() + 3.0), galley, text_col);
            }
            t += step;
        }
    }
}

impl eframe::App for TinySamplerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_global_shortcuts(ctx);

        const BTN: f32 = 56.0;
        const BTN_GAP: f32 = 24.0;
        // Reserve under the buttons for a possible error line.
        const TRANSPORT_RESERVE_H: f32 = 40.0;

        egui::CentralPanel::default().show(ctx, |ui| {
            let proj = self.current_project();
            self.sync_spec_textures(ctx, &proj.clips);
            if let Some(i) = self.selected_clip_index {
                if i >= proj.clips.len() {
                    self.selected_clip_index = None;
                }
            }

            let timeline_height = 160.0;
            let transport_block_h = BTN + TRANSPORT_RESERVE_H;

            ui.vertical(|ui| {
                let viewport_w = ui.available_width();
                let end_secs = proj
                    .clips
                    .iter()
                    .map(|c| c.start_time_secs + c.timeline_duration_secs(proj.device_sample_rate))
                    .fold(4.0f32, f32::max)
                    .max(self.playhead_secs() + 0.5);

                let stack_origin = ui.cursor().min;
                let combined_rect = Rect::from_min_size(
                    stack_origin,
                    Vec2::new(viewport_w, TIME_RULER_HEIGHT + timeline_height),
                );
                if let Some(hp) = ctx.pointer_hover_pos() {
                    if combined_rect.contains(hp) && ctx.input(|i| i.modifiers.ctrl) {
                        let dy = ctx.input(|i| i.smooth_scroll_delta.y + i.raw_scroll_delta.y);
                        if dy.abs() > 0.01 {
                            let old_pps = self.pixels_per_second;
                            let new_pps = (old_pps * (1.0 + dy * 0.0025))
                                .clamp(TIMELINE_PPS_MIN, TIMELINE_PPS_MAX);
                            if (new_pps - old_pps).abs() > f32::EPSILON {
                                let scroll = self.timeline_scroll_px;
                                let t_here =
                                    (((hp.x - combined_rect.left()) + scroll) / old_pps).max(0.0);
                                self.pixels_per_second = new_pps;
                                let new_content_w = (end_secs * new_pps).max(viewport_w);
                                let max_scroll = (new_content_w - viewport_w).max(0.0);
                                let new_scroll = combined_rect.left() + t_here * new_pps - hp.x;
                                self.timeline_scroll_px = new_scroll.clamp(0.0, max_scroll);
                            }
                        }
                    }
                }

                let pps = self.pixels_per_second;
                let content_w = (end_secs * pps).max(viewport_w);
                let max_scroll = (content_w - viewport_w).max(0.0);

                let (ruler_rect, _) = ui
                    .allocate_exact_size(Vec2::new(viewport_w, TIME_RULER_HEIGHT), Sense::hover());
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(viewport_w, timeline_height), Sense::hover());
                let view_left = ruler_rect.left();

                let pan_resp = ui.interact(
                    combined_rect,
                    egui::Id::new("timeline_scroll_pan"),
                    Sense::click_and_drag(),
                );

                if let Some(drag) = self.trim_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        let dx = ctx.input(|i| i.pointer.delta().x);
                        if dx != 0.0 {
                            let mut p = (*self.current_project()).clone();
                            if Self::apply_trim_delta(&mut p, drag, dx, pps) {
                                self.publish(p);
                            }
                        }
                    }
                } else if let Some(idx) = self.clip_move_drag {
                    if ctx.input(|i| i.pointer.primary_down()) {
                        let dx = ctx.input(|i| i.pointer.delta().x);
                        if dx != 0.0 {
                            let mut p = (*self.current_project()).clone();
                            if let Some(clip) = p.clips.get_mut(idx) {
                                clip.start_time_secs = (clip.start_time_secs + dx / pps).max(0.0);
                                self.publish(p);
                            }
                        }
                    }
                }
                if !ctx.input(|i| i.pointer.primary_down()) {
                    self.trim_drag = None;
                    self.clip_move_drag = None;
                }
                if ctx.input(|i| i.pointer.primary_pressed()) {
                    let proj_now = self.current_project();
                    if !proj_now.transport.is_playing {
                        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                            if combined_rect.contains(pos) && rect.contains(pos) {
                                if let Some(sel) = self.selected_clip_index {
                                    if proj_now.clips.get(sel).is_some() {
                                        if let Some(d) = Self::trim_hit_test(
                                            &proj_now,
                                            sel,
                                            pos,
                                            rect,
                                            view_left,
                                            pps,
                                            self.timeline_scroll_px,
                                            proj_now.device_sample_rate,
                                        ) {
                                            self.trim_drag = Some(d);
                                        } else {
                                            let cr = Self::clip_rect_on_timeline(
                                                &proj_now.clips[sel],
                                                rect,
                                                view_left,
                                                pps,
                                                self.timeline_scroll_px,
                                                proj_now.device_sample_rate,
                                            );
                                            if cr.contains(pos) {
                                                self.clip_move_drag = Some(sel);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                let proj = self.current_project();

                if pan_resp.dragged() && self.trim_drag.is_none() && self.clip_move_drag.is_none() {
                    self.timeline_scroll_px =
                        (self.timeline_scroll_px - pan_resp.drag_delta().x).clamp(0.0, max_scroll);
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                } else if self.trim_drag.is_some() || self.clip_move_drag.is_some() {
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                } else if let Some(hp) = ctx.pointer_hover_pos() {
                    if Self::pointer_near_trim_handle(
                        &proj,
                        self.selected_clip_index,
                        hp,
                        rect,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                        proj.device_sample_rate,
                    ) && !proj.transport.is_playing
                    {
                        ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                    } else if Self::pointer_on_selected_clip_move_body(
                        &proj,
                        self.selected_clip_index,
                        hp,
                        rect,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                        proj.device_sample_rate,
                    ) && !proj.transport.is_playing
                    {
                        ctx.set_cursor_icon(CursorIcon::Move);
                    } else if pan_resp.hovered() {
                        ctx.set_cursor_icon(CursorIcon::Grab);
                    }
                }

                if pan_resp.clicked() {
                    if let Some(p) = pan_resp.interact_pointer_pos() {
                        let sc = self.timeline_scroll_px;
                        if let Some(idx) = Self::clip_index_at_pointer(
                            &proj,
                            p,
                            rect,
                            view_left,
                            pps,
                            sc,
                            proj.device_sample_rate,
                        ) {
                            self.selected_clip_index = Some(idx);
                        } else {
                            self.selected_clip_index = None;
                            let time_at =
                                |x: f32| -> f32 { (((x - view_left) + sc) / pps).max(0.0) };
                            if ruler_rect.contains(p) {
                                let t = time_at(p.x);
                                self.request_seek(t);
                                self.timeline_scroll_px =
                                    (view_left + t * pps - p.x).clamp(0.0, max_scroll);
                            } else if rect.contains(p) {
                                let t = time_at(p.x);
                                self.request_seek(t);
                                self.timeline_scroll_px =
                                    (view_left + t * pps - p.x).clamp(0.0, max_scroll);
                            }
                        }
                    }
                }

                if proj.transport.is_playing {
                    self.timeline_scroll_px = Self::scroll_keep_playhead_in_view(
                        rect,
                        self.playhead_secs(),
                        pps,
                        max_scroll,
                        self.timeline_scroll_px,
                    );
                }

                let scroll = self.timeline_scroll_px;
                let to_screen = |t: f32| -> f32 { view_left + t * pps - scroll };

                let ruler_painter = ui.painter_at(ruler_rect);
                Self::paint_time_ruler(&ruler_painter, ruler_rect, pps, scroll, ctx);
                let play_x_head = view_left + self.playhead_secs() * pps - scroll;
                if play_x_head >= ruler_rect.left() && play_x_head <= ruler_rect.right() {
                    ruler_painter.line_segment(
                        [
                            Pos2::new(play_x_head, ruler_rect.top()),
                            Pos2::new(play_x_head, ruler_rect.bottom()),
                        ],
                        Stroke::new(2.0, Color32::from_rgb(200, 80, 80)),
                    );
                }

                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 4.0, Color32::from_rgb(30, 30, 36));
                painter.rect_stroke(rect, 4.0, Stroke::new(1.0, Color32::from_gray(80)));

                for (i, clip) in proj.clips.iter().enumerate() {
                    let dur = clip.timeline_duration_secs(proj.device_sample_rate);
                    let x0 = to_screen(clip.start_time_secs);
                    let w = dur * pps;
                    let clip_rect = Rect::from_min_size(
                        Pos2::new(x0, rect.top() + 20.0),
                        Vec2::new(w.max(8.0), rect.height() - 40.0),
                    );

                    if let Some(entry) = self.spec_textures.get(i).and_then(|e| e.as_ref()) {
                        let n = clip.sample.data.len().max(1);
                        let u0 = clip.trim_start as f32 / n as f32;
                        let u1 = clip.trim_end as f32 / n as f32;
                        let uv = Rect::from_min_max(Pos2::new(u0, 0.0), Pos2::new(u1, 1.0));
                        painter.image(entry.texture.id(), clip_rect, uv, Color32::WHITE);
                    } else {
                        painter.rect_filled(clip_rect, 3.0, Color32::from_rgb(50, 90, 160));
                    }

                    if self.selected_clip_index == Some(i) && !proj.transport.is_playing {
                        let s = (TRIM_HANDLE_WIDTH_PX * 0.35).min(clip_rect.width() * 0.25);
                        painter.rect_filled(
                            Rect::from_min_size(
                                clip_rect.left_top(),
                                Vec2::new(s, clip_rect.height()),
                            ),
                            0.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 90),
                        );
                        painter.rect_filled(
                            Rect::from_min_max(
                                Pos2::new(clip_rect.right() - s, clip_rect.top()),
                                clip_rect.max,
                            ),
                            0.0,
                            Color32::from_rgba_unmultiplied(255, 255, 255, 90),
                        );
                    }

                    if !clip.label.is_empty() {
                        let inset = 6.0_f32;
                        let pad = 3.0_f32;
                        let font = FontId::proportional(11.0);
                        let galley = ctx
                            .fonts(|f| f.layout_no_wrap(clip.label.clone(), font, Color32::WHITE));
                        let tw = galley.rect.width();
                        let th = galley.rect.height();
                        let tl = Pos2::new(clip_rect.left() + inset, clip_rect.top() + inset);
                        let bg_min = tl - Vec2::splat(pad);
                        let bg_max = tl + Vec2::new(tw + pad, th + pad);
                        let bg_rect = Rect::from_min_max(bg_min, bg_max);
                        let clip_painter = painter.with_clip_rect(clip_rect);
                        clip_painter.rect_filled(
                            bg_rect,
                            2.0,
                            Color32::from_rgba_unmultiplied(0, 0, 0, 175),
                        );
                        clip_painter.galley(bg_min + Vec2::new(pad, pad), galley, Color32::WHITE);
                    }

                    if self.selected_clip_index == Some(i) {
                        painter.rect_stroke(clip_rect, 3.0, Stroke::new(1.0, Color32::WHITE));
                    }
                }

                if let Some(hp) = ui.ctx().pointer_hover_pos() {
                    if combined_rect.contains(hp) {
                        let gx = hp.x.clamp(combined_rect.left(), combined_rect.right());
                        let cross_painter = ui.painter_at(combined_rect);
                        cross_painter.line_segment(
                            [
                                Pos2::new(gx, combined_rect.top()),
                                Pos2::new(gx, rect.bottom()),
                            ],
                            Stroke::new(1.5, Color32::from_rgba_unmultiplied(240, 120, 120, 110)),
                        );
                    }
                }

                let play_x = to_screen(self.playhead_secs());
                painter.line_segment(
                    [
                        Pos2::new(play_x, rect.top()),
                        Pos2::new(play_x, rect.bottom()),
                    ],
                    Stroke::new(2.0, Color32::from_rgb(200, 80, 80)),
                );

                let gap_before_transport = (ui.available_height() - transport_block_h).max(0.0);
                ui.add_space(gap_before_transport);

                let load_fill = Color32::from_rgb(64, 108, 168);
                let play_fill = Color32::from_rgb(52, 140, 92);
                let pause_fill = Color32::from_rgb(118, 98, 52);
                let stop_fill = Color32::from_rgb(138, 56, 56);

                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        let total_w = BTN * 3.0 + BTN_GAP * 2.0;
                        ui.add_space(((ui.available_width() - total_w) * 0.5).max(0.0));

                        if Self::round_transport_btn(ui, "+", "Load WAV (Ctrl+O)", load_fill, BTN)
                            .clicked()
                        {
                            self.try_pick_and_load_wav();
                        }
                        ui.add_space(BTN_GAP);
                        let playing = self.current_project().transport.is_playing;
                        if playing {
                            if Self::round_transport_btn(ui, "⏸", "Pause (Space)", pause_fill, BTN)
                                .clicked()
                            {
                                self.transport_toggle_play_pause();
                            }
                        } else if Self::round_transport_btn(ui, "▶", "Play (Space)", play_fill, BTN)
                            .clicked()
                        {
                            self.transport_toggle_play_pause();
                        }
                        ui.add_space(BTN_GAP);
                        if Self::round_transport_btn(ui, "⏹", "Stop (Ctrl+Space)", stop_fill, BTN)
                            .clicked()
                        {
                            self.transport_stop();
                        }
                    });
                    if !self.status.is_empty() {
                        ui.add_space(4.0);
                        ui.label(RichText::new(&self.status).weak().size(12.0));
                    }
                });

                if ctx.input(|i| i.pointer.primary_clicked()) {
                    if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        if !combined_rect.contains(pos) {
                            self.selected_clip_index = None;
                        }
                    }
                }
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}
