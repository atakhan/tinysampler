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
                ui.horizontal_centered(|ui| {
                    ui.add_enabled_ui(!studio_locked, |ui| {
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
                                "Pause (Ctrl+Space)",
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
                            "Stop (Space)",
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
                            egui::RichText::new(crate::time::format_bpm(bpm))
                                .weak()
                                .size(14.0),
                        );
                    });
                    let name = self.current_project().name.clone();
                    let status = self.status.clone();
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.add_space(12.0);
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Настройки").color(Color32::WHITE),
                                )
                                .fill(theme::color_track_gutter_selected())
                                .min_size(Vec2::new(108.0, 32.0)),
                            )
                            .clicked()
                        {
                            self.open_settings();
                        }
                        ui.add_enabled_ui(!studio_locked, |ui| {
                            if !status.is_empty() {
                                ui.label(egui::RichText::new(&status).weak().size(12.0));
                            }
                            ui.label(egui::RichText::new(name).weak().size(14.0));
                        });
                    });
                });
            });

        if self.screen != AppScreen::Studio {
            return;
        }

        let panel_fill = ctx.style().visuals.panel_fill;
        // Same top/bottom inset as the timeline panel so each sidebar row
        // starts on the same Y as its lane.
        let sidebar_frame = egui::Frame::none().inner_margin(egui::Margin::symmetric(0.0, 8.0)).fill(panel_fill);
        egui::SidePanel::left("studio_tracks")
            .exact_width(theme::STUDIO_SIDEBAR_W)
            .resizable(false)
            .frame(sidebar_frame)
            .show(ctx, |ui| {
                ui.add_enabled_ui(!studio_locked, |ui| {
                    let proj = self.current_project();
                    let names: Vec<(TrackId, String)> =
                        proj.tracks.iter().map(|t| (t.id, t.name.clone())).collect();
                    drop(proj);
                    if names.is_empty() {
                        ui.add_space(8.0);
                        ui.label(egui::RichText::new("Треки").strong().size(15.0));
                        ui.add_space(6.0);
                        ui.label(
                            egui::RichText::new("Пока нет дорожек")
                                .weak()
                                .size(12.0),
                        );
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
                        return;
                    }

                    // Flush with the ruler: default item spacing would open a gap
                    // the lanes do not have.
                    ui.spacing_mut().item_spacing.y = 0.0;
                    let block_h = studio_lane_block_height(ui.available_height(), names.len());
                    if show_track_sidebar_header(ui) {
                        self.add_studio_track();
                    }
                    for (lane, (id, name)) in names.iter().enumerate() {
                        match show_track_sidebar_row(
                            ui,
                            block_h,
                            lane,
                            *id,
                            name,
                            self.selected_track == Some(*id),
                        ) {
                            TrackSidebarAction::Select(id) => self.selected_track = Some(id),
                            TrackSidebarAction::Sampler(id) => self.open_sampler(id),
                            TrackSidebarAction::None => {}
                        }
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
                // Match the sidebar: no gap between the ruler and the first lane.
                ui.spacing_mut().item_spacing.y = 0.0;
                let block_h = studio_lane_block_height(ui.available_height(), n_lanes);
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
                if self.timeline_scroll_px > max_scroll {
                    self.timeline_scroll_px = max_scroll;
                }

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
                                    self.seek_studio(t);
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
                let show_base = (proj.transport.is_playing || self.studio_transport_paused)
                    && (self.studio_base_secs - self.playhead_secs()).abs() > 0.001;
                if show_base {
                    paint_studio_base_cursor(&ruler_painter, ruler_rect, to_screen(self.studio_base_secs), true);
                }
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

                if show_base {
                    paint_studio_base_cursor(
                        &painter,
                        tracks_rect,
                        to_screen(self.studio_base_secs),
                        false,
                    );
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

    pub(crate) fn open_settings(&mut self) {
        self.settings_browse = self
            .sound_library_dir
            .clone()
            .filter(|p| p.is_dir())
            .unwrap_or_else(crate::browser::default_audio_dir);
        self.settings_open = true;
    }

    pub(crate) fn show_settings_window(&mut self, ctx: &egui::Context) {
        let mut open = true;
        let saved = self.sound_library_dir.clone();
        let browse = self.settings_browse.clone();
        let mut enter = None;
        let mut go_up = false;
        let mut choose = false;
        egui::Window::new("Настройки")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(480.0)
            .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-16.0, 56.0))
            .order(egui::Order::Foreground)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Папка библиотеки").strong());
                ui.label(
                    egui::RichText::new(
                        "Инструмент сэмплинга открывает загрузку звуков в этой папке.",
                    )
                    .weak()
                    .size(12.0),
                );
                ui.add_space(4.0);
                let saved_text = match &saved {
                    Some(path) if path.is_dir() => path.display().to_string(),
                    Some(path) => format!("{} · папка недоступна", path.display()),
                    None => "Не выбрана — откроется папка «Музыка»".into(),
                };
                ui.label(egui::RichText::new(saved_text).monospace().size(12.0));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button("Вверх").clicked() {
                        go_up = true;
                    }
                    if ui.button("Использовать эту папку").clicked() {
                        choose = true;
                    }
                });
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(browse.display().to_string())
                        .monospace()
                        .size(12.0),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("settings_library_dirs")
                    .max_height(280.0)
                    .show(ui, |ui| {
                        let dirs = list_child_dirs(&browse);
                        if dirs.is_empty() {
                            ui.label(egui::RichText::new("Нет вложенных папок").weak());
                        }
                        for (name, path) in dirs {
                            if ui.selectable_label(false, format!("📁 {name}")).clicked() {
                                enter = Some(path);
                            }
                        }
                    });
            });
        if go_up {
            if let Some(parent) = self.settings_browse.parent() {
                if parent != self.settings_browse {
                    self.settings_browse = parent.to_path_buf();
                }
            }
        }
        if let Some(path) = enter {
            if path.is_dir() {
                self.settings_browse = path;
            }
        }
        if choose {
            self.choose_sound_library();
        }
        let escape_closes = self.load_browser.is_none()
            && self.sampler_track.is_none()
            && self.seq_editor.is_none()
            && ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if !open || escape_closes {
            self.settings_open = false;
        }
    }

    fn choose_sound_library(&mut self) {
        if !self.settings_browse.is_dir() {
            self.status = "Эта папка недоступна".into();
            return;
        }
        self.sound_library_dir = Some(self.settings_browse.clone());
        let settings = crate::persist::AppSettings {
            sound_library_dir: self.sound_library_dir.clone(),
        };
        match crate::persist::save_settings(&self.persist.root, &settings) {
            Ok(()) => self.status = format!("Библиотека: {}", self.settings_browse.display()),
            Err(e) => self.status = format!("Не удалось сохранить настройки: {e}"),
        }
    }
}

fn list_child_dirs(dir: &std::path::Path) -> Vec<(String, std::path::PathBuf)> {
    let mut dirs = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return dirs;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        dirs.push((name, path));
    }
    dirs.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    dirs
}

/// Lane height shared by the timeline and the track sidebar.
/// `available_height` is the panel's inner height, including the ruler row.
fn studio_lane_block_height(available_height: f32, n_lanes: usize) -> f32 {
    let avail_for_lanes = (available_height - theme::TIME_RULER_HEIGHT).max(80.0);
    let min_h = theme::MARKER_LANE_HEIGHT + 72.0;
    let max_h = theme::TIMELINE_TRACK_HEIGHT + theme::MARKER_LANE_HEIGHT;
    (avail_for_lanes / n_lanes.max(1) as f32).clamp(min_h, max_h)
}

enum TrackSidebarAction {
    None,
    Select(TrackId),
    Sampler(TrackId),
}

/// Header strip the same height as the time ruler. Returns true when "+" was clicked.
fn show_track_sidebar_header(ui: &mut egui::Ui) -> bool {
    let width = ui.available_width();
    let h = theme::TIME_RULER_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), egui::Sense::hover());
    ui.painter().rect_filled(rect, 0.0, theme::color_ruler_bg());
    ui.painter().line_segment(
        [
            Pos2::new(rect.left(), rect.bottom()),
            Pos2::new(rect.right(), rect.bottom()),
        ],
        Stroke::new(1.0_f32, theme::color_ruler_bottom_line()),
    );

    let mut add_clicked = false;
    let mut header = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    header.add_space(8.0);
    header.label(
        egui::RichText::new("Треки")
            .strong()
            .size(14.0)
            .color(theme::color_ruler_text()),
    );
    header.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
        ui.add_space(6.0);
        add_clicked = ui
            .add(
                egui::Button::new(
                    egui::RichText::new("+")
                        .color(Color32::WHITE)
                        .size(16.0),
                )
                .fill(theme::color_transport_play())
                .min_size(Vec2::new(28.0, 22.0)),
            )
            .on_hover_text("Добавить трек")
            .clicked();
    });
    add_clicked
}

/// One sidebar row, exactly `block_h` tall, with the same marker strip as the lane.
fn show_track_sidebar_row(
    ui: &mut egui::Ui,
    block_h: f32,
    lane: usize,
    id: TrackId,
    name: &str,
    selected: bool,
) -> TrackSidebarAction {
    let width = ui.available_width();
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, block_h), egui::Sense::hover());
    let marker_h = theme::MARKER_LANE_HEIGHT.min(row.height());
    let marker = Rect::from_min_size(row.min, Vec2::new(row.width(), marker_h));
    let clips = Rect::from_min_max(Pos2::new(row.left(), marker.bottom()), row.max);
    let bg = if lane % 2 == 0 {
        theme::color_timeline_bg()
    } else {
        theme::color_timeline_bg_alt()
    };
    ui.painter().rect_filled(clips, 0.0, bg);
    ui.painter()
        .rect_filled(marker, 0.0, theme::color_marker_lane_bg());
    ui.painter().line_segment(
        [
            Pos2::new(marker.left(), marker.bottom()),
            Pos2::new(marker.right(), marker.bottom()),
        ],
        Stroke::new(1.0_f32, theme::color_ruler_bottom_line()),
    );
    if selected {
        ui.painter().rect_stroke(
            row,
            0.0,
            Stroke::new(1.5_f32, theme::color_track_gutter_selected()),
        );
    }

    let click = ui.interact(
        row,
        ui.id().with(("studio_track_row", id)),
        egui::Sense::click(),
    );
    if click.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
    }
    let mut action = if click.clicked() {
        TrackSidebarAction::Select(id)
    } else {
        TrackSidebarAction::None
    };

    let controls = Rect::from_min_size(
        Pos2::new(row.left() + 8.0, marker.bottom() + 6.0),
        Vec2::new((row.width() - 16.0).max(0.0), 24.0),
    );
    let mut controls_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(controls)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let button_w = 72.0;
    let name_w = (controls_ui.available_width() - button_w).max(16.0);
    let name_color = if selected {
        Color32::WHITE
    } else {
        Color32::from_gray(220)
    };
    controls_ui.add_sized(
        Vec2::new(name_w, 22.0),
        egui::Label::new(egui::RichText::new(name).size(14.0).color(name_color))
            .selectable(false)
            .truncate(),
    );
    if controls_ui
        .small_button("сэмпл")
        .on_hover_text("Инструмент сэмплинга")
        .clicked()
    {
        action = TrackSidebarAction::Sampler(id);
    }
    action
}

fn paint_studio_base_cursor(painter: &egui::Painter, rect: Rect, x: f32, ruler: bool) {
    if x < rect.left() || x > rect.right() {
        return;
    }
    let col = theme::color_sampler_base();
    painter.line_segment(
        [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
        Stroke::new(1.5_f32, col),
    );
    if ruler {
        let top = rect.top();
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(x - 5.0, top),
                Pos2::new(x + 5.0, top),
                Pos2::new(x, top + 8.0),
            ],
            col,
            Stroke::NONE,
        ));
    }
}
