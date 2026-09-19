//! Sequence “колбаска” on the timeline and the piano-roll editor modal.

use egui::{
    Align2, Color32, CursorIcon, FontId, PointerButton, Pos2, Rect, Sense, Stroke, Vec2,
};

use crate::model::{NoteId, PadMarker, PadNote, Project, SeqClip, SeqId};
use crate::sampler;
use crate::theme;
use crate::timeline::{self, TrackLayout};

pub const KEY_COUNT: usize = theme::PIANO_KEY_COUNT;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SeqHit {
    Body,
    ResizeStart,
    ResizeEnd,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum NoteHit {
    Body,
    ResizeEnd,
}

#[derive(Clone, Copy)]
pub enum NoteDrag {
    Move { id: NoteId, grab_offset_secs: f32 },
    Resize { id: NoteId },
}

pub struct PianoRollModel<'a> {
    pub track_name: &'a str,
    pub tempo_bpm: f32,
    pub duration_secs: f32,
    pub notes: &'a [PadNote],
    pub pad_markers: &'a [PadMarker],
    pub selected_note: Option<NoteId>,
    pub playhead_local: Option<f32>,
    pub active_pad: Option<u8>,
    pub status: &'a str,
}

pub enum PianoRollAction {
    Close,
    Place { slot: u8, time: f32 },
    MoveNote { id: NoteId, slot: u8, time: f32 },
    ResizeNote { id: NoteId, end: f32 },
    DeleteNote { id: NoteId },
    SelectNote { id: Option<NoteId> },
}

pub fn seq_rect(
    seq: &SeqClip,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> Rect {
    let x0 = view_left + seq.start_time_secs * pps - scroll;
    let w = (seq.duration_secs * pps).max(8.0);
    let pad = 8.0_f32;
    let clips = layout.clips_rect(seq.track_index);
    Rect::from_min_size(
        Pos2::new(x0, clips.top() + pad),
        Vec2::new(w, (clips.height() - pad * 2.0).max(4.0)),
    )
}

pub fn hit_seq(
    project: &Project,
    pos: Pos2,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> Option<(SeqId, SeqHit)> {
    let mut best: Option<(SeqId, SeqHit, f32)> = None;
    for seq in &project.seq_clips {
        let clips = layout.clips_rect(seq.track_index);
        if !clips.contains(pos) {
            continue;
        }
        let r = seq_rect(seq, layout, view_left, pps, scroll);
        if !r.contains(pos) {
            continue;
        }
        let hw = theme::TRIM_HANDLE_WIDTH_PX.min(r.width() * 0.4);
        let hit = if pos.x <= r.left() + hw {
            SeqHit::ResizeStart
        } else if pos.x >= r.right() - hw {
            SeqHit::ResizeEnd
        } else {
            SeqHit::Body
        };
        let area = r.width();
        if best.map_or(true, |(_, _, a)| area <= a) {
            best = Some((seq.id, hit, area));
        }
    }
    best.map(|(id, hit, _)| (id, hit))
}

pub fn paint_sausages(
    painter: &egui::Painter,
    project: &Project,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
    selected: Option<SeqId>,
) {
    for seq in &project.seq_clips {
        let r = seq_rect(seq, layout, view_left, pps, scroll);
        let clips = layout.clips_rect(seq.track_index);
        if r.right() < clips.left() || r.left() > clips.right() {
            continue;
        }
        let clip_p = painter.with_clip_rect(clips);
        let sel = selected == Some(seq.id);
        let fill = if sel {
            theme::color_piano_note_selected()
        } else {
            theme::color_clip_bg()
        };
        clip_p.rect_filled(r, 6.0, fill);
        if sel {
            clip_p.rect_stroke(r, 6.0, Stroke::new(1.5_f32, Color32::WHITE));
            let s = (theme::TRIM_HANDLE_WIDTH_PX * 0.35).min(r.width() * 0.25);
            clip_p.rect_filled(
                Rect::from_min_size(r.left_top(), Vec2::new(s, r.height())),
                0.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, 90),
            );
            clip_p.rect_filled(
                Rect::from_min_max(Pos2::new(r.right() - s, r.top()), r.max),
                0.0,
                Color32::from_rgba_unmultiplied(255, 255, 255, 90),
            );
        }
        paint_sausage_notes(&clip_p, project, seq, r);
    }
}

fn paint_sausage_notes(painter: &egui::Painter, project: &Project, seq: &SeqClip, r: Rect) {
    if seq.duration_secs <= 1e-6 {
        return;
    }
    let clip_p = painter.with_clip_rect(r);
    let h = r.height() / KEY_COUNT as f32;
    for note in project.notes.iter().filter(|n| n.seq_id == seq.id) {
        let x0 = r.left() + (note.start_time_secs / seq.duration_secs) * r.width();
        let w = (note.duration_secs / seq.duration_secs * r.width()).max(2.0);
        let y = r.top() + note.slot as f32 * h;
        let nr = Rect::from_min_size(
            Pos2::new(x0, y + 1.0),
            Vec2::new(w, (h - 2.0).max(1.0)),
        );
        clip_p.rect_filled(nr, 1.0, Color32::from_rgb(72, 78, 140));
    }
}

pub fn show(
    ctx: &egui::Context,
    model: PianoRollModel<'_>,
    view_start: &mut f32,
    view_len: &mut f32,
    note_drag: &mut Option<NoteDrag>,
) -> Option<PianoRollAction> {
    let mut action = None;
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("pianoroll_dim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(170));
        });

    let mut open = true;
    egui::Window::new("Пианоролл")
        .id(egui::Id::new("piano_roll_editor"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([920.0, 520.0])
        .min_width(640.0)
        .min_height(360.0)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(model.track_name).strong().size(16.0));
                ui.separator();
                ui.label(
                    egui::RichText::new(format!("{:.0} BPM", model.tempo_bpm))
                        .monospace()
                        .size(14.0),
                );
            });
            ui.add_space(8.0);

            clamp_view(view_start, view_len, model.duration_secs);
            let key_w = theme::PIANO_KEY_W;
            const FOOTER_H: f32 = 22.0;
            let body = ui.available_rect_before_wrap();
            ui.allocate_rect(body, Sense::hover());
            let footer = Rect::from_min_max(
                Pos2::new(body.left(), (body.bottom() - FOOTER_H).max(body.top())),
                body.max,
            );
            let full = Rect::from_min_max(body.min, Pos2::new(body.right(), footer.top()));
            let resp = ui.interact(
                full,
                ui.id().with("piano_roll_grid"),
                Sense::click_and_drag(),
            );
            let keys = Rect::from_min_size(
                full.min,
                Vec2::new(key_w.min(full.width() * 0.2), full.height()),
            );
            let roll = Rect::from_min_max(Pos2::new(keys.right(), full.top()), full.max);
            paint_editor(ui, keys, roll, &model, *view_start, *view_len);
            if let Some(a) = handle_editor(
                &resp,
                keys,
                roll,
                &model,
                *view_start,
                *view_len,
                note_drag,
            ) {
                action = Some(a);
            }
            handle_roll_scroll(ctx, &resp, roll, view_start, view_len, model.duration_secs);

            let footer_text = if model.status.is_empty() {
                "Клик — нота, ПКМ — удалить. Q–I / A–K — слушать пэд, пока клавиша зажата. Ctrl+колёсико — зум, Alt — скролл."
            } else {
                model.status
            };
            ui.painter_at(footer).text(
                Pos2::new(footer.left(), footer.center().y),
                Align2::LEFT_CENTER,
                footer_text,
                FontId::proportional(12.0),
                Color32::from_gray(140),
            );
        });

    if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some(PianoRollAction::Close);
    } else if ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
        if let Some(id) = model.selected_note {
            action = Some(PianoRollAction::DeleteNote { id });
        }
    }
    action
}

fn clamp_view(start: &mut f32, len: &mut f32, duration: f32) {
    let n = duration.max(0.25);
    *len = len.clamp(0.25, n);
    *start = start.clamp(0.0, (n - *len).max(0.0));
}

fn handle_roll_scroll(
    ctx: &egui::Context,
    resp: &egui::Response,
    roll: Rect,
    start: &mut f32,
    len: &mut f32,
    duration: f32,
) {
    if !resp.hovered() {
        return;
    }
    let (ctrl, alt, dy) = ctx.input(|i| {
        (
            i.modifiers.ctrl || i.modifiers.command,
            i.modifiers.alt,
            i.smooth_scroll_delta.y + i.raw_scroll_delta.y,
        )
    });
    if dy.abs() < 0.01 {
        return;
    }
    let n = duration.max(0.25);
    if ctrl && !alt {
        let pos = ctx.pointer_hover_pos().unwrap_or(roll.center());
        let u = ((pos.x - roll.left()) / roll.width().max(1.0)).clamp(0.0, 1.0);
        let anchor = *start + u * *len;
        let new_len = (*len * (1.0 - dy * 0.004)).clamp(0.25, n);
        *len = new_len;
        *start = (anchor - u * new_len).clamp(0.0, (n - new_len).max(0.0));
    } else if alt && !ctrl {
        *start = (*start - dy / roll.width().max(1.0) * *len).clamp(0.0, (n - *len).max(0.0));
    }
}

fn time_at_x(x: f32, roll: Rect, view_start: f32, view_len: f32) -> f32 {
    let u = ((x - roll.left()) / roll.width().max(1.0)).clamp(0.0, 1.0);
    view_start + u * view_len.max(1e-4)
}

fn x_at_time(t: f32, roll: Rect, view_start: f32, view_len: f32) -> f32 {
    roll.left() + (t - view_start) / view_len.max(1e-4) * roll.width()
}

fn key_rect(keys: Rect, slot: u8) -> Rect {
    let h = keys.height() / KEY_COUNT as f32;
    let i = (slot as usize).min(KEY_COUNT - 1) as f32;
    Rect::from_min_size(
        Pos2::new(keys.left(), keys.top() + i * h),
        Vec2::new(keys.width(), h),
    )
}

fn slot_at_y(area: Rect, y: f32) -> u8 {
    if area.height() <= 1.0 {
        return 0;
    }
    let u = ((y - area.top()) / area.height()).clamp(0.0, 0.999);
    (u * KEY_COUNT as f32).floor() as u8
}

fn note_rect_local(note: &PadNote, roll: Rect, view_start: f32, view_len: f32) -> Rect {
    let x0 = x_at_time(note.start_time_secs, roll, view_start, view_len);
    let x1 = x_at_time(note.end_time_secs(), roll, view_start, view_len);
    let row = key_rect(roll, note.slot);
    Rect::from_min_max(
        Pos2::new(x0, row.top() + 1.0),
        Pos2::new(x1.max(x0 + 4.0), row.bottom() - 1.0),
    )
}

fn hit_editor_note(
    notes: &[PadNote],
    pos: Pos2,
    roll: Rect,
    view_start: f32,
    view_len: f32,
) -> Option<(NoteId, NoteHit)> {
    let mut best: Option<(NoteId, NoteHit, f32)> = None;
    for note in notes {
        let r = note_rect_local(note, roll, view_start, view_len);
        if !r.contains(pos) {
            continue;
        }
        let hw = theme::TRIM_HANDLE_WIDTH_PX.min(r.width() * 0.4);
        let hit = if pos.x >= r.right() - hw {
            NoteHit::ResizeEnd
        } else {
            NoteHit::Body
        };
        let area = r.width();
        if best.map_or(true, |(_, _, a)| area <= a) {
            best = Some((note.id, hit, area));
        }
    }
    best.map(|(id, hit, _)| (id, hit))
}

fn paint_editor(
    ui: &egui::Ui,
    keys: Rect,
    roll: Rect,
    model: &PianoRollModel<'_>,
    view_start: f32,
    view_len: f32,
) {
    let painter = ui.painter_at(Rect::from_min_max(keys.min, roll.max));
    painter.rect_filled(keys, 0.0, theme::color_ruler_bg());
    painter.rect_filled(roll, 0.0, theme::color_timeline_bg());
    let bound: Vec<bool> = (0..KEY_COUNT)
        .map(|s| model.pad_markers.iter().any(|m| m.slot == s as u8))
        .collect();
    let font = FontId::proportional(11.0);
    for slot in 0..KEY_COUNT {
        let r = key_rect(keys, slot as u8);
        let fill = if model.active_pad == Some(slot as u8) {
            theme::color_pad_held()
        } else if bound[slot] {
            theme::color_piano_key_bound()
        } else if slot % 2 == 0 {
            theme::color_piano_key()
        } else {
            theme::color_piano_key_alt()
        };
        painter.rect_filled(r.shrink(0.5), 0.0, fill);
        painter.line_segment(
            [Pos2::new(r.left(), r.bottom()), Pos2::new(roll.right(), r.bottom())],
            Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 16)),
        );
        let label = sampler::PAD_LABELS.get(slot).copied().unwrap_or("?");
        painter.text(
            r.center(),
            Align2::CENTER_CENTER,
            label,
            font.clone(),
            if model.active_pad == Some(slot as u8) || bound[slot] {
                Color32::WHITE
            } else {
                Color32::from_gray(140)
            },
        );
    }
    painter.line_segment(
        [Pos2::new(keys.right(), keys.top()), Pos2::new(keys.right(), keys.bottom())],
        Stroke::new(1.0_f32, theme::color_timeline_border()),
    );

    let beat = timeline::beat_secs(model.tempo_bpm);
    let step = beat / 4.0;
    let t0 = view_start;
    let t1 = view_start + view_len;
    let mut t = (t0 / step).floor() * step;
    if t < 0.0 {
        t = 0.0;
    }
    while t <= t1 + step * 0.5 {
        let x = x_at_time(t, roll, view_start, view_len);
        if x >= roll.left() - 1.0 && x <= roll.right() + 1.0 {
            let on_beat = (t / beat - (t / beat).round()).abs() < 1e-3;
            let on_bar = (t / (beat * timeline::BEATS_PER_BAR)
                - (t / (beat * timeline::BEATS_PER_BAR)).round())
            .abs()
                < 1e-3;
            let stroke = if on_bar {
                Stroke::new(1.2_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 48))
            } else if on_beat {
                Stroke::new(1.0_f32, theme::color_piano_grid_beat())
            } else {
                Stroke::new(1.0_f32, theme::color_piano_grid_step())
            };
            painter.line_segment(
                [Pos2::new(x, roll.top()), Pos2::new(x, roll.bottom())],
                stroke,
            );
        }
        t += step;
        if t > t0 + 600.0 {
            break;
        }
    }

    let clip_p = painter.with_clip_rect(roll);
    for note in model.notes {
        let r = note_rect_local(note, roll, view_start, view_len);
        let sel = model.selected_note == Some(note.id);
        clip_p.rect_filled(
            r,
            2.0,
            if sel {
                theme::color_piano_note_selected()
            } else {
                theme::color_piano_note()
            },
        );
        if sel {
            clip_p.rect_stroke(r, 2.0, Stroke::new(1.5_f32, Color32::WHITE));
        }
        let slot = note.slot as usize;
        if slot < sampler::PAD_COUNT && r.width() > 14.0 {
            clip_p.text(
                Pos2::new(r.left() + 6.0, r.center().y),
                Align2::LEFT_CENTER,
                sampler::PAD_LABELS[slot],
                FontId::proportional(11.0),
                Color32::from_rgb(28, 28, 40),
            );
        }
    }

    if let Some(ph) = model.playhead_local {
        if ph >= view_start && ph <= view_start + view_len {
            let x = x_at_time(ph, roll, view_start, view_len);
            painter.line_segment(
                [Pos2::new(x, roll.top()), Pos2::new(x, roll.bottom())],
                Stroke::new(2.0_f32, theme::color_playhead()),
            );
        }
    }
}

fn handle_editor(
    resp: &egui::Response,
    keys: Rect,
    roll: Rect,
    model: &PianoRollModel<'_>,
    view_start: f32,
    view_len: f32,
    note_drag: &mut Option<NoteDrag>,
) -> Option<PianoRollAction> {
    let pos = resp.interact_pointer_pos();
    if resp.hovered() {
        if let Some(hover) = resp.hover_pos() {
            if let Some((_, hit)) = hit_editor_note(model.notes, hover, roll, view_start, view_len) {
                resp.ctx.set_cursor_icon(match hit {
                    NoteHit::ResizeEnd => CursorIcon::ResizeHorizontal,
                    NoteHit::Body => CursorIcon::Move,
                });
            }
        }
    }

    if resp.ctx.input(|i| i.pointer.button_clicked(PointerButton::Secondary)) {
        if let Some(pos) = resp.hover_pos().or(resp.interact_pointer_pos()) {
            if let Some((id, _)) = hit_editor_note(model.notes, pos, roll, view_start, view_len) {
                return Some(PianoRollAction::DeleteNote { id });
            }
        }
    }

    if resp.drag_started_by(PointerButton::Primary) {
        if let Some(pos) = pos {
            if let Some((id, hit)) = hit_editor_note(model.notes, pos, roll, view_start, view_len) {
                match hit {
                    NoteHit::ResizeEnd => {
                        *note_drag = Some(NoteDrag::Resize { id });
                        return Some(PianoRollAction::SelectNote { id: Some(id) });
                    }
                    NoteHit::Body => {
                        let start = model
                            .notes
                            .iter()
                            .find(|n| n.id == id)
                            .map(|n| n.start_time_secs)
                            .unwrap_or(0.0);
                        *note_drag = Some(NoteDrag::Move {
                            id,
                            grab_offset_secs: time_at_x(pos.x, roll, view_start, view_len) - start,
                        });
                        return Some(PianoRollAction::SelectNote { id: Some(id) });
                    }
                }
            }
        }
    }

    if let Some(drag) = *note_drag {
        if resp.dragged_by(PointerButton::Primary) {
            if let Some(pos) = pos {
                match drag {
                    NoteDrag::Move { id, grab_offset_secs } => {
                        let t = time_at_x(pos.x, roll, view_start, view_len) - grab_offset_secs;
                        let slot = slot_at_y(roll, pos.y);
                        return Some(PianoRollAction::MoveNote { id, slot, time: t });
                    }
                    NoteDrag::Resize { id } => {
                        let end = time_at_x(pos.x, roll, view_start, view_len);
                        return Some(PianoRollAction::ResizeNote { id, end });
                    }
                }
            }
        }
        if resp.drag_stopped() {
            *note_drag = None;
        }
    }

    if resp.clicked_by(PointerButton::Primary) {
        let pos = pos?;
        if keys.contains(pos) {
            return Some(PianoRollAction::SelectNote { id: None });
        }
        if let Some((id, hit)) = hit_editor_note(model.notes, pos, roll, view_start, view_len) {
            if hit == NoteHit::Body || hit == NoteHit::ResizeEnd {
                return Some(PianoRollAction::SelectNote { id: Some(id) });
            }
        }
        if roll.contains(pos) {
            let slot = slot_at_y(roll, pos.y);
            let time = time_at_x(pos.x, roll, view_start, view_len);
            return Some(PianoRollAction::Place { slot, time });
        }
    }
    let _ = keys;
    None
}
