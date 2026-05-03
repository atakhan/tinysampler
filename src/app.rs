use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use egui::{Color32, CursorIcon, Key, Pos2, Rect, Stroke, Vec2};

use crate::model::{ClipId, Project};
use crate::project_actions;
use crate::spec_textures::SpecTextureCache;
use crate::theme;
use crate::timeline::{self, TrimDrag};

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
    spec_cache: SpecTextureCache,
    timeline_scroll_px: f32,
    selected_clip_id: Option<ClipId>,
    trim_drag: Option<TrimDrag>,
    clip_move_drag: Option<ClipId>,
    /// `true` only for Alt+duplicate drag; `false` for normal move (both use ghost preview).
    clip_move_from_alt_duplicate: bool,
    clip_settle_anim: Option<ClipSettleAnim>,
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
            spec_cache: SpecTextureCache::default(),
            timeline_scroll_px: 0.0,
            selected_clip_id: None,
            trim_drag: None,
            clip_move_drag: None,
            clip_move_from_alt_duplicate: false,
            clip_settle_anim: None,
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
    }

    fn try_pick_and_load_wav(&mut self) {
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("WAV", &["wav"])
            .pick_file()
        {
            let mut p = (*self.current_project()).clone();
            match project_actions::try_load_wav_clip(&mut p, &path) {
                Ok(()) => {
                    self.status.clear();
                    self.publish(p);
                }
                Err(e) => self.status = e,
            }
        }
    }

    fn try_split_clip_at_playhead(&mut self) {
        let mut p = (*self.current_project()).clone();
        self.clip_settle_anim = None;
        project_actions::resolve_all_placement_previews(&mut p);
        match project_actions::split_clip_at_playhead(&mut p, self.playhead_secs()) {
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
}

impl eframe::App for TinySamplerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_global_shortcuts(ctx);

        let btn = theme::TRANSPORT_BTN_DIAMETER;
        let btn_gap = theme::TRANSPORT_BTN_GAP;

        egui::CentralPanel::default().show(ctx, |ui| {
            let proj = self.current_project();
            self.spec_cache.sync(ctx, &proj.clips);
            if let Some(id) = self.selected_clip_id {
                if proj.clip_index(id).is_none() {
                    self.selected_clip_id = None;
                }
            }

            let timeline_height = theme::TIMELINE_TRACK_HEIGHT;
            let transport_block_h = btn + theme::TRANSPORT_RESERVE_H;

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
                    Vec2::new(viewport_w, theme::TIME_RULER_HEIGHT + timeline_height),
                );
                if let Some(hp) = ctx.pointer_hover_pos() {
                    if combined_rect.contains(hp) && ctx.input(|i| i.modifiers.ctrl) {
                        let dy = ctx.input(|i| i.smooth_scroll_delta.y + i.raw_scroll_delta.y);
                        if dy.abs() > 0.01 {
                            let old_pps = self.pixels_per_second;
                            let new_pps = (old_pps * (1.0 + dy * 0.0025))
                                .clamp(theme::TIMELINE_PPS_MIN, theme::TIMELINE_PPS_MAX);
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

                let (ruler_rect, _) = ui.allocate_exact_size(
                    Vec2::new(viewport_w, theme::TIME_RULER_HEIGHT),
                    egui::Sense::hover(),
                );
                let (rect, _) =
                    ui.allocate_exact_size(Vec2::new(viewport_w, timeline_height), egui::Sense::hover());
                let view_left = ruler_rect.left();

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
                        if dx != 0.0 {
                            let mut p = (*self.current_project()).clone();
                            let allow_overlap = p
                                .clip_index(clip_id)
                                .is_some_and(|i| p.clips[i].placement_preview);
                            if project_actions::nudge_clip_time_by_drag(
                                &mut p,
                                clip_id,
                                dx,
                                pps,
                                allow_overlap,
                            ) {
                                self.publish(p);
                            }
                        }
                    } else {
                        self.clip_move_drag = None;
                        self.clip_move_from_alt_duplicate = false;
                        self.finish_clip_preview_drop(ctx, clip_id);
                    }
                }
                if ctx.input(|i| i.pointer.primary_pressed()) {
                    let proj_now = self.current_project();
                    if !proj_now.transport.is_playing {
                        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                            if combined_rect.contains(pos) && rect.contains(pos) {
                                if let Some(sel) = self.selected_clip_id {
                                    if proj_now.clip_index(sel).is_some() {
                                        if let Some(d) = timeline::trim_hit_test(
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
                                        } else if let Some(clip) =
                                            proj_now.clips.iter().find(|c| c.id == sel)
                                        {
                                            let cr = timeline::clip_rect_on_timeline(
                                                clip,
                                                rect,
                                                view_left,
                                                pps,
                                                self.timeline_scroll_px,
                                                proj_now.device_sample_rate,
                                            );
                                            if cr.contains(pos) {
                                                let alt = ctx.input(|i| i.modifiers.alt);
                                                if alt {
                                                    let mut p = (*proj_now).clone();
                                                    if let Some(new_id) =
                                                        project_actions::duplicate_clip(&mut p, sel)
                                                    {
                                                        self.selected_clip_id = Some(new_id);
                                                        self.clip_move_from_alt_duplicate = true;
                                                        self.clip_move_drag = Some(new_id);
                                                        self.publish(p);
                                                    } else {
                                                        self.begin_clip_body_drag(&proj_now, sel, false);
                                                    }
                                                } else {
                                                    self.begin_clip_body_drag(&proj_now, sel, false);
                                                }
                                            }
                                        }
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
                    && self.clip_settle_anim.is_none()
                {
                    self.timeline_scroll_px =
                        (self.timeline_scroll_px - pan_resp.drag_delta().x).clamp(0.0, max_scroll);
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
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
                    if timeline::pointer_near_trim_handle(
                        &proj,
                        self.selected_clip_id,
                        hp,
                        rect,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                        proj.device_sample_rate,
                    ) && !proj.transport.is_playing
                    {
                        ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                    } else if timeline::pointer_on_selected_clip_move_body(
                        &proj,
                        self.selected_clip_id,
                        hp,
                        rect,
                        view_left,
                        pps,
                        self.timeline_scroll_px,
                        proj.device_sample_rate,
                    ) && !proj.transport.is_playing
                    {
                        if ctx.input(|i| i.modifiers.alt) {
                            ctx.set_cursor_icon(CursorIcon::Alias);
                        } else {
                            ctx.set_cursor_icon(CursorIcon::Move);
                        }
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
                            rect,
                            view_left,
                            pps,
                            sc,
                            proj.device_sample_rate,
                        ) {
                            self.selected_clip_id = Some(id);
                        } else {
                            self.selected_clip_id = None;
                            let time_at =
                                |x: f32| -> f32 { (((x - view_left) + sc) / pps).max(0.0) };
                            if ruler_rect.contains(p) || rect.contains(p) {
                                let t = time_at(p.x);
                                self.request_seek(t);
                                self.timeline_scroll_px =
                                    (view_left + t * pps - p.x).clamp(0.0, max_scroll);
                            }
                        }
                    }
                }

                if proj.transport.is_playing {
                    self.timeline_scroll_px = timeline::scroll_keep_playhead_in_view(
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
                timeline::paint_time_ruler(&ruler_painter, ruler_rect, pps, scroll, ctx);
                let play_x_head = view_left + self.playhead_secs() * pps - scroll;
                if play_x_head >= ruler_rect.left() && play_x_head <= ruler_rect.right() {
                    ruler_painter.line_segment(
                        [
                            Pos2::new(play_x_head, ruler_rect.top()),
                            Pos2::new(play_x_head, ruler_rect.bottom()),
                        ],
                        Stroke::new(2.0, theme::color_playhead()),
                    );
                }

                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 4.0, theme::color_timeline_bg());
                painter.rect_stroke(rect, 4.0, Stroke::new(1.0, theme::color_timeline_border()));

                let mut clip_draw_order: Vec<usize> = (0..proj.clips.len()).collect();
                clip_draw_order.sort_by_key(|&i| proj.clips[i].placement_preview);

                for &i in &clip_draw_order {
                    let clip = &proj.clips[i];
                    let ghost = clip.placement_preview;
                    let dur = clip.timeline_duration_secs(proj.device_sample_rate);
                    let x0 = to_screen(clip.start_time_secs);
                    let w = dur * pps;
                    let clip_rect = Rect::from_min_size(
                        Pos2::new(x0, rect.top() + 20.0),
                        Vec2::new(w.max(8.0), rect.height() - 40.0),
                    );

                    if let Some(tex) = self.spec_cache.texture_at(i) {
                        let n = clip.sample.data.len().max(1);
                        let u0 = clip.trim_start as f32 / n as f32;
                        let u1 = clip.trim_end as f32 / n as f32;
                        let uv = Rect::from_min_max(Pos2::new(u0, 0.0), Pos2::new(u1, 1.0));
                        let tint = if ghost {
                            Color32::from_rgba_unmultiplied(200, 230, 255, 150)
                        } else {
                            Color32::WHITE
                        };
                        painter.image(tex.id(), clip_rect, uv, tint);
                    } else {
                        let fill = if ghost {
                            theme::color_clip_fallback().gamma_multiply(0.5)
                        } else {
                            theme::color_clip_fallback()
                        };
                        painter.rect_filled(clip_rect, 3.0, fill);
                    }

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
                    if combined_rect.contains(hp) {
                        let gx = hp.x.clamp(combined_rect.left(), combined_rect.right());
                        let cross_painter = ui.painter_at(combined_rect);
                        cross_painter.line_segment(
                            [
                                Pos2::new(gx, combined_rect.top()),
                                Pos2::new(gx, rect.bottom()),
                            ],
                            Stroke::new(1.5, theme::color_playhead_cross()),
                        );
                    }
                }

                let play_x = to_screen(self.playhead_secs());
                painter.line_segment(
                    [
                        Pos2::new(play_x, rect.top()),
                        Pos2::new(play_x, rect.bottom()),
                    ],
                    Stroke::new(2.0, theme::color_playhead()),
                );

                let gap_before_transport = (ui.available_height() - transport_block_h).max(0.0);
                ui.add_space(gap_before_transport);

                ui.vertical_centered(|ui| {
                    ui.horizontal(|ui| {
                        let total_w = btn * 3.0 + btn_gap * 2.0;
                        ui.add_space(((ui.available_width() - total_w) * 0.5).max(0.0));

                        if timeline::round_transport_btn(
                            ui,
                            "+",
                            "Load WAV (Ctrl+O)",
                            theme::color_transport_load(),
                            btn,
                        )
                        .clicked()
                        {
                            self.try_pick_and_load_wav();
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
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(&self.status).weak().size(12.0));
                    }
                });

                if ctx.input(|i| i.pointer.primary_clicked()) {
                    if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        if !combined_rect.contains(pos) {
                            self.selected_clip_id = None;
                        }
                    }
                }
            });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }
}
