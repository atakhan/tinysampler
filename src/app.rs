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
        self.publish(p);
    }

    fn transport_stop(&mut self) {
        let mut p = (*self.current_project()).clone();
        p.transport.is_playing = false;
        p.transport.stop_generation = p.transport.stop_generation.wrapping_add(1);
        self.publish(p);
        self.timeline_scroll_px = 0.0;
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
                    let clip = Clip {
                        start_time_secs: 0.0,
                        label,
                        sample,
                    };
                    p.clips.push(clip);
                    self.status.clear();
                    self.publish(p);
                }
                Err(e) => self.status = e,
            }
        }
    }

    fn handle_global_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() {
            return;
        }
        let (ctrl_space, space, open_wav) = ctx.input(|i| {
            let space = i.key_pressed(Key::Space);
            let open_wav = i.key_pressed(Key::O) && (i.modifiers.ctrl || i.modifiers.command);
            (
                space && i.modifiers.ctrl,
                space && !i.modifiers.ctrl,
                open_wav,
            )
        });
        if ctrl_space {
            self.transport_stop();
        } else if space {
            self.transport_toggle_play_pause();
        } else if open_wav {
            self.try_pick_and_load_wav();
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

    fn pointer_hits_timeline_clip(
        proj: &Project,
        p: Pos2,
        rect: Rect,
        view_left: f32,
        pps: f32,
        scroll: f32,
        sample_rate: u32,
    ) -> bool {
        proj.clips.iter().any(|clip| {
            let dur = clip.sample.duration_secs(sample_rate);
            let x0 = view_left + clip.start_time_secs * pps - scroll;
            let w = dur * pps;
            let clip_rect = Rect::from_min_size(
                Pos2::new(x0, rect.top() + 20.0),
                Vec2::new(w.max(8.0), rect.height() - 40.0),
            );
            clip_rect.contains(p)
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

            let timeline_height = 160.0;
            let transport_block_h = BTN + TRANSPORT_RESERVE_H;

            ui.vertical(|ui| {
                let viewport_w = ui.available_width();
                let end_secs = proj
                    .clips
                    .iter()
                    .map(|c| c.start_time_secs + c.sample.duration_secs(proj.device_sample_rate))
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
                if pan_resp.dragged() {
                    self.timeline_scroll_px =
                        (self.timeline_scroll_px - pan_resp.drag_delta().x).clamp(0.0, max_scroll);
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                } else if pan_resp.hovered() {
                    ctx.set_cursor_icon(CursorIcon::Grab);
                }

                if pan_resp.clicked() {
                    if let Some(p) = pan_resp.interact_pointer_pos() {
                        let sc = self.timeline_scroll_px;
                        let time_at = |x: f32| -> f32 { (((x - view_left) + sc) / pps).max(0.0) };
                        if ruler_rect.contains(p) {
                            let t = time_at(p.x);
                            self.request_seek(t);
                            self.timeline_scroll_px =
                                (view_left + t * pps - p.x).clamp(0.0, max_scroll);
                        } else if rect.contains(p)
                            && !Self::pointer_hits_timeline_clip(
                                &proj,
                                p,
                                rect,
                                view_left,
                                pps,
                                sc,
                                proj.device_sample_rate,
                            )
                        {
                            let t = time_at(p.x);
                            self.request_seek(t);
                            self.timeline_scroll_px =
                                (view_left + t * pps - p.x).clamp(0.0, max_scroll);
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
                    let dur = clip.sample.duration_secs(proj.device_sample_rate);
                    let x0 = to_screen(clip.start_time_secs);
                    let w = dur * pps;
                    let clip_rect = Rect::from_min_size(
                        Pos2::new(x0, rect.top() + 20.0),
                        Vec2::new(w.max(8.0), rect.height() - 40.0),
                    );

                    if let Some(entry) = self.spec_textures.get(i).and_then(|e| e.as_ref()) {
                        painter.image(
                            entry.texture.id(),
                            clip_rect,
                            Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    } else {
                        painter.rect_filled(clip_rect, 3.0, Color32::from_rgb(50, 90, 160));
                    }

                    painter.rect_stroke(clip_rect, 3.0, Stroke::new(1.0, Color32::WHITE));

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
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}
