use egui::{Color32, CursorIcon, Pos2, Rect, Stroke, Vec2};

use crate::app::{AppScreen, SeqDrag, TinySamplerApp};
use crate::model::TrackId;
use crate::pianoroll;
use crate::project_actions;
use crate::theme;
use crate::timeline;

impl TinySamplerApp {
    pub(crate) fn show_studio(&mut self, ctx: &egui::Context) {
        let btn = theme::STUDIO_TRANSPORT_BTN;
        let studio_locked = self.sampler_track.is_some() || self.seq_editor.is_some();
        if studio_locked {
            self.marker_drag = None;
            self.seq_drag = None;
        }

        egui::TopBottomPanel::top("studio_top")
            .exact_height(theme::STUDIO_TOP_BAR_H)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!studio_locked, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.add_space(8.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("⌂ Домой").color(Color32::WHITE),
                                )
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
                    let names: Vec<(TrackId, String)> =
                        proj.tracks.iter().map(|t| (t.id, t.name.clone())).collect();
                    drop(proj);
                    if names.is_empty() {
                        ui.label(
                            egui::RichText::new("Пока нет дорожек")
                                .weak()
                                .size(12.0),
                        );
                    }
                    egui::ScrollArea::vertical()
                        .max_height(ui.available_height() - 52.0)
                        .show(ui, |ui| {
                            for (id, name) in &names {
                                let selected = self.selected_track == Some(*id);
                                ui.horizontal(|ui| {
                                    if ui
                                        .selectable_label(selected, name)
                                        .on_hover_text("Выбрать дорожку")
                                        .clicked()
                                    {
                                        self.selected_track = Some(*id);
                                    }
                                    if ui
                                        .small_button("сэмпл")
                                        .on_hover_text("Инструмент сэмплинга")
                                        .clicked()
                                    {
                                        self.open_sampler(*id);
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
            ui.vertical(|ui| {
                let viewport_w = ui.available_width();
                let proj = self.current_project();
                let n_lanes = proj.tracks.len();
                if n_lanes == 0 {
                    ui.add_space(48.0);
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(
                                "Добавьте трек слева, чтобы открыть инструмент сэмплинга",
                            )
                            .weak()
                            .size(15.0),
                        );
                    });
                    return;
                }
                if self.selected_track.and_then(|id| proj.track_index(id)).is_none() {
                    self.selected_track = proj.tracks.first().map(|t| t.id);
                }
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
                let (tracks_rect, _) = ui.allocate_exact_size(
                    Vec2::new(viewport_w, timeline_height),
                    egui::Sense::hover(),
                );
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

                if !studio_locked {
                    if let Some((slot, track)) = self.marker_drag {
                        if ctx.input(|i| i.pointer.primary_down()) {
                            if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                                let t =
                                    (((pos.x - view_left) + self.timeline_scroll_px) / pps).max(0.0);
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
                                let t =
                                    (((pos.x - view_left) + self.timeline_scroll_px) / pps).max(0.0);
                                let mut p = (*self.current_project()).clone();
                                let changed = match drag {
                                    SeqDrag::Move {
                                        id,
                                        grab_offset_secs,
                                    } => project_actions::move_seq_clip(&mut p, id, t - grab_offset_secs),
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
                        && self.marker_drag.is_none()
                        && self.seq_drag.is_none()
                    {
                        let proj_now = self.current_project();
                        if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                            if layout.gutter_contains(pos) {
                                self.selected_track = proj_now.track_id_at_lane(layout.lane_at_y(pos.y));
                                self.selected_seq = None;
                                self.selected_marker = None;
                            } else if let Some((slot, track, on_delete)) =
                                timeline::marker_hit_at_pointer(
                                    &proj_now,
                                    pos,
                                    &layout,
                                    view_left,
                                    pps,
                                    self.timeline_scroll_px,
                                )
                            {
                                self.selected_marker = Some((slot, track));
                                self.selected_track = Some(track);
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
                                self.selected_track =
                                    proj_now.track_id_at_lane(layout.lane_at_y(pos.y));
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
                                if let Some(i) = proj_now.seq_index(id) {
                                    self.selected_track = Some(proj_now.seq_clips[i].track_id);
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
                                        let t = (((pos.x - view_left) + self.timeline_scroll_px)
                                            / pps)
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
                        && self.marker_drag.is_none()
                        && self.seq_drag.is_none()
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
                    } else if let Some(hp) = ctx.pointer_hover_pos() {
                        if let Some((_, _, on_delete)) = timeline::marker_hit_at_pointer(
                            &proj,
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
                                let lane = layout.lane_at_y(p.y);
                                let t = (((p.x - view_left) + sc) / pps).max(0.0);
                                drop(proj);
                                let mut proj = (*self.current_project()).clone();
                                if let Some(track_id) = proj.track_id_at_lane(lane) {
                                    if let Some(id) =
                                        project_actions::add_seq_clip(&mut proj, track_id, t)
                                    {
                                        self.selected_seq = Some(id);
                                        self.selected_track = Some(track_id);
                                        self.publish(proj);
                                    }
                                }
                            }
                        }
                    } else if pan_resp.clicked() {
                        if let Some(p) = pan_resp.interact_pointer_pos() {
                            let sc = self.timeline_scroll_px;
                            if pianoroll::hit_seq(&proj, p, &layout, view_left, pps, sc).is_some() {
                                // selected on press
                            } else if layout.gutter_contains(p) {
                                self.selected_track =
                                    proj.track_id_at_lane(layout.lane_at_y(p.y));
                                self.selected_seq = None;
                                self.selected_marker = None;
                            } else {
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
                        Stroke::new(2.0_f32, theme::color_playhead()),
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
                    let lane_id = proj.track_id_at_lane(lane);
                    if lane_id == self.selected_track {
                        painter.rect_stroke(
                            layout.block_rect(lane),
                            0.0,
                            Stroke::new(1.5_f32, theme::color_track_gutter_selected()),
                        );
                    }
                    if let Some(track_id) = lane_id {
                        timeline::paint_marker_lane(
                            &painter,
                            &proj.markers,
                            layout.marker_bar(lane),
                            track_id,
                            view_left,
                            pps,
                            scroll,
                            self.selected_marker,
                            lane_id == self.selected_track,
                        );
                    }
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
                    let Some(track_id) = proj.track_id_at_lane(lane) else {
                        continue;
                    };
                    if proj.seq_clips.iter().any(|s| s.track_id == track_id) {
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
                                Stroke::new(1.5_f32, theme::color_playhead_cross()),
                            );
                        }
                    }
                }

                timeline::paint_markers(&painter, &proj, &layout, view_left, pps, scroll);
                for lane in 0..n_lanes {
                    if let Some(track_id) = proj.track_id_at_lane(lane) {
                        timeline::paint_marker_lane(
                            &painter,
                            &proj.markers,
                            layout.marker_bar(lane),
                            track_id,
                            view_left,
                            pps,
                            scroll,
                            self.selected_marker,
                            Some(track_id) == self.selected_track,
                        );
                    }
                }

                let play_x = to_screen(self.playhead_secs());
                if play_x >= view_left && play_x <= tracks_rect.right() {
                    painter.line_segment(
                        [
                            Pos2::new(play_x, tracks_rect.top()),
                            Pos2::new(play_x, tracks_rect.bottom()),
                        ],
                        Stroke::new(2.0_f32, theme::color_playhead()),
                    );
                }

                if !studio_locked && ctx.input(|i| i.pointer.primary_clicked()) {
                    if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                        if !combined_rect.contains(pos) {
                            self.selected_seq = None;
                            self.selected_marker = None;
                        }
                    }
                }
            });
        });
    }
}
