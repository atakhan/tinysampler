//! Sequence “колбаска” on the timeline and the piano-roll editor modal.

use egui::{
    Align2, Color32, CursorIcon, FontId, PointerButton, Pos2, Rect, Sense, Stroke, Vec2,
};

use crate::model::{NoteId, PadMarker, PadNote, Project, SeqClip, SeqId};
use crate::sampler;
use crate::theme;
use crate::timeline::{self, TrackLayout};
use crate::waveform::{self, PeakPyramid};

pub const KEY_COUNT: usize = theme::PIANO_KEY_COUNT;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SeqHit {
    Body,
    ResizeStart,
    ResizeEnd,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoteHit {
    ResizeStart,
    Body,
    ResizeEnd,
}

#[derive(Clone, Copy)]
pub enum NoteDrag {
    Move { id: NoteId, grab_offset_secs: f32 },
    /// Alt was held when the body was pressed. The original stays; the first move creates a copy.
    Copy { source: NoteId, grab_offset_secs: f32 },
    /// Left edge: the note's end stays put, the start (and therefore the length) follows the pointer.
    ResizeStart { id: NoteId },
    /// Right edge: the start stays put, the end follows the pointer.
    ResizeEnd { id: NoteId },
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
    /// Track sample used to draw each note: samples, peaks, rate, playback speed.
    pub sample: Option<(&'a [f32], &'a PeakPyramid, u32, f32)>,
}

pub enum PianoRollAction {
    Close,
    Place { slot: u8, time: f32 },
    MoveNote { id: NoteId, slot: u8, time: f32 },
    /// Place a copy of `source` and keep dragging that copy.
    DuplicateNote {
        source: NoteId,
        slot: u8,
        time: f32,
        grab_offset_secs: f32,
    },
    ResizeNoteStart { id: NoteId, start: f32 },
    ResizeNote { id: NoteId, end: f32 },
    DeleteNote { id: NoteId },
    SelectNote { id: Option<NoteId> },
}

pub fn seq_rect(
    seq: &SeqClip,
    lane: usize,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> Rect {
    let x0 = view_left + seq.start_time_secs * pps - scroll;
    let w = (seq.duration_secs * pps).max(2.0);
    let pad = 8.0_f32;
    let clips = layout.clips_rect(lane);
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
        let Some(lane) = project.track_index(seq.track_id) else {
            continue;
        };
        let clips = layout.clips_rect(lane);
        if !clips.contains(pos) {
            continue;
        }
        let r = seq_rect(seq, lane, layout, view_left, pps, scroll);
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
        let Some(lane) = project.track_index(seq.track_id) else {
            continue;
        };
        let r = seq_rect(seq, lane, layout, view_left, pps, scroll);
        let clips = layout.clips_rect(lane);
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
        paint_arranged_waveform(&clip_p, project, seq, r, clips);
        let edge = Color32::from_rgba_unmultiplied(255, 255, 255, if sel { 120 } else { 70 });
        let edge_w = 4.0_f32.min(r.width() * 0.22).max(1.0);
        clip_p.rect_filled(
            Rect::from_min_size(r.left_top(), Vec2::new(edge_w, r.height())),
            0.0,
            edge,
        );
        clip_p.rect_filled(
            Rect::from_min_max(Pos2::new(r.right() - edge_w, r.top()), r.max),
            0.0,
            edge,
        );
        if sel {
            clip_p.rect_stroke(r, 6.0, Stroke::new(1.5_f32, Color32::WHITE));
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
        clip_p.rect_filled(nr, 1.0, Color32::from_rgba_unmultiplied(72, 78, 140, 80));
    }
}

/// One column per pixel: the mix of every note in the sausage, the same way playback sums them.
fn paint_arranged_waveform(
    painter: &egui::Painter,
    project: &Project,
    seq: &SeqClip,
    rect: Rect,
    clips: Rect,
) {
    if seq.duration_secs <= 1e-4 {
        return;
    }
    let Some(track) = project.track(seq.track_id) else {
        return;
    };
    let Some(sample) = track.sample.as_ref() else {
        return;
    };
    let notes: Vec<_> = project
        .notes
        .iter()
        .filter(|n| n.seq_id == seq.id)
        .collect();
    if notes.is_empty() {
        return;
    }
    let vis = rect.intersect(clips);
    if vis.width() < 0.5 || vis.height() < 0.5 {
        return;
    }
    let speed = track.playback_speed().max(0.05);
    let rate = sample.rate() as f32 * speed;
    let width = rect.width().max(1.0);
    let center_y = rect.center().y;
    let half_h = ((rect.height() - 4.0).max(4.0)) * 0.5;
    let y_of = |s: f32| center_y - s.clamp(-1.0, 1.0) * half_h;
    let clip_p = painter.with_clip_rect(vis);
    clip_p.line_segment(
        [
            Pos2::new(vis.left(), center_y),
            Pos2::new(vis.right(), center_y),
        ],
        Stroke::new(1.0_f32, theme::color_clip_zero_line()),
    );
    let x0 = vis.left().floor() as i32;
    let x1 = vis.right().ceil() as i32;
    for x in x0..x1 {
        let xf = x as f32;
        let u0 = ((xf - rect.left()) / width).clamp(0.0, 1.0);
        let u1 = ((xf + 1.0 - rect.left()) / width).clamp(0.0, 1.0);
        if u1 <= u0 {
            continue;
        }
        let t0 = u0 * seq.duration_secs;
        let t1 = u1 * seq.duration_secs;
        let mut mn = 0.0_f32;
        let mut mx = 0.0_f32;
        let mut any = false;
        for note in &notes {
            if note.end_time_secs() <= t0 || note.start_time_secs >= t1 {
                continue;
            }
            let Some(marker) = track.pad_markers.iter().find(|m| m.slot == note.slot) else {
                continue;
            };
            let into0 = (t0 - note.start_time_secs).max(0.0);
            let into1 = (t1 - note.start_time_secs).max(into0);
            let i0 = marker.start_index as f32 + into0 * rate;
            let mut i1 = marker.start_index as f32 + into1 * rate;
            if i1 < i0 + 1.0 {
                i1 = i0 + 1.0;
            }
            let s0 = i0.floor() as usize;
            let s1 = i1.ceil() as usize;
            if s0 >= marker.end_index || s0 >= sample.data.len() {
                continue;
            }
            let s1 = s1.min(marker.end_index).min(sample.data.len());
            if s1 <= s0 {
                continue;
            }
            let (a, b) = waveform::min_max_range(sample.data.as_slice(), sample.peaks.as_ref(), s0, s1);
            mn += a;
            mx += b;
            any = true;
        }
        if !any {
            continue;
        }
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
        clip_p.rect_filled(
            Rect::from_min_max(Pos2::new(xf, yt), Pos2::new(xf + 1.0, yb)),
            0.0,
            theme::color_clip_waveform(),
        );
    }
}

pub fn show(
    ctx: &egui::Context,
    model: PianoRollModel<'_>,
    view_start: &mut f32,
    pixels_per_second: &mut f32,
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
                    egui::RichText::new(crate::time::format_bpm(model.tempo_bpm))
                        .monospace()
                        .size(14.0),
                );
            });
            ui.add_space(8.0);

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
            // Zoom is pixels per second. A wider window shows more time; it does not stretch the grid.
            settle_piano_view(pixels_per_second, view_start, roll.width(), model.duration_secs);
            let view_len = visible_seconds(roll.width(), *pixels_per_second);
            paint_editor(ui, keys, roll, &model, *view_start, view_len);
            if let Some(a) = handle_editor(
                &resp,
                keys,
                roll,
                &model,
                *view_start,
                view_len,
                note_drag,
            ) {
                action = Some(a);
            }
            handle_roll_scroll(ctx, &resp, roll, view_start, pixels_per_second);

            let footer_text = if model.status.is_empty() {
                "Клик — нота, ПКМ — удалить. Q–I / A–K — слушать пэд, пока клавиша зажата. Ctrl+колёсико — зум, Alt+колёсико — скролл, Alt+перетаскивание — копия ноты."
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

const PIANO_SCROLL_LIMIT: f32 = 100_000.0;

/// Seconds visible across `width_px` at a fixed zoom. Wider width shows more time.
pub(crate) fn visible_seconds(width_px: f32, pps: f32) -> f32 {
    width_px.max(1.0) / pps.clamp(theme::TIMELINE_PPS_MIN, theme::TIMELINE_PPS_MAX)
}

fn settle_piano_view(pps: &mut f32, start: &mut f32, width: f32, duration: f32) {
    if !pps.is_finite() || *pps < theme::TIMELINE_PPS_MIN {
        let fit = width.max(1.0) / duration.max(0.25);
        *pps = fit.clamp(theme::TIMELINE_PPS_MIN, theme::TIMELINE_PPS_MAX);
    } else {
        *pps = pps.clamp(theme::TIMELINE_PPS_MIN, theme::TIMELINE_PPS_MAX);
    }
    if !start.is_finite() {
        *start = 0.0;
    }
    *start = clamp_piano_start(*start);
}

fn clamp_piano_start(start: f32) -> f32 {
    if !start.is_finite() {
        0.0
    } else {
        start.clamp(0.0, PIANO_SCROLL_LIMIT)
    }
}

fn handle_roll_scroll(
    ctx: &egui::Context,
    resp: &egui::Response,
    roll: Rect,
    start: &mut f32,
    pps: &mut f32,
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
    let width = roll.width().max(1.0);
    if ctrl && !alt {
        let pos = ctx.pointer_hover_pos().unwrap_or(roll.center());
        let u = ((pos.x - roll.left()) / width).clamp(0.0, 1.0);
        let old = *pps;
        let new = (old * (1.0 + dy * 0.004)).clamp(theme::TIMELINE_PPS_MIN, theme::TIMELINE_PPS_MAX);
        let anchor = *start + u * (width / old);
        *pps = new;
        *start = clamp_piano_start(anchor - u * (width / new));
    } else if alt && !ctrl {
        *start = clamp_piano_start(*start - dy / (*pps).max(theme::TIMELINE_PPS_MIN));
    }
}

fn time_at_x(x: f32, roll: Rect, view_start: f32, view_len: f32) -> f32 {
    let u = ((x - roll.left()) / roll.width().max(1.0)).clamp(0.0, 1.0);
    view_start + u * view_len.max(1e-4)
}

fn x_at_time(t: f32, roll: Rect, view_start: f32, view_len: f32) -> f32 {
    roll.left() + (t - view_start) / view_len.max(1e-4) * roll.width()
}

/// Minor and major grid spacing, in seconds, so lines stay readable at any zoom.
fn piano_grid_steps(pps: f32, bpm: f32) -> (f32, f32) {
    let beat = timeline::beat_secs(bpm).max(1e-4);
    let bar = beat * timeline::BEATS_PER_BAR;
    let mut minor = beat / 64.0;
    while minor * pps < 12.0 && minor < bar * 128.0 {
        minor *= 2.0;
    }
    let mut major = minor.max(beat);
    while major * pps < 56.0 && major < bar * 128.0 {
        major *= 2.0;
    }
    (minor, major.max(minor))
}

fn near_multiple(t: f32, period: f32) -> bool {
    if period <= 1e-6 {
        return false;
    }
    let q = (t / period).round();
    (q * period - t).abs() < (period * 1e-3).max(1e-4)
}

fn shade_outside_sequence(
    painter: &egui::Painter,
    roll: Rect,
    duration: f32,
    view_start: f32,
    view_len: f32,
) {
    let shade = Color32::from_black_alpha(36);
    let x1 = x_at_time(duration.max(0.0), roll, view_start, view_len);
    if x1 < roll.right() {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(x1.max(roll.left()), roll.top()),
                roll.max,
            ),
            0.0,
            shade,
        );
    }
}

fn paint_note_waveform(painter: &egui::Painter, model: &PianoRollModel<'_>, note: &PadNote, rect: Rect) {
    let Some((data, peaks, rate, speed)) = model.sample else {
        return;
    };
    let Some(marker) = model.pad_markers.iter().find(|m| m.slot == note.slot) else {
        return;
    };
    if rect.width() < 4.0 || rect.height() < 6.0 || note.duration_secs <= 1e-4 {
        return;
    }
    let samples_per_sec = rate as f32 * speed.max(0.05);
    let slice_len = marker.end_index.saturating_sub(marker.start_index);
    if slice_len == 0 || samples_per_sec <= 1.0 {
        return;
    }
    let slice_secs = slice_len as f32 / samples_per_sec;
    let shown_secs = slice_secs.min(note.duration_secs);
    let wave_w = (shown_secs / note.duration_secs) * rect.width();
    let wave = Rect::from_min_size(rect.min, Vec2::new(wave_w.max(1.0), rect.height()));
    let shown_samples = ((shown_secs * samples_per_sec).round() as usize)
        .clamp(1, slice_len);
    let trim_end = marker.start_index.saturating_add(shown_samples).min(data.len());
    waveform::paint_waveform_overlay(
        painter,
        wave,
        rect,
        data,
        peaks,
        marker.start_index,
        trim_end,
        theme::color_clip_waveform(),
        theme::color_clip_zero_line(),
    );
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
        Pos2::new(x1.max(x0 + 1.0), row.bottom() - 1.0),
    )
}

/// Which part of a note `pos` hits. Edges win over the body, including a few pixels outside the rect,
/// so a press on the border still counts after the cursor has moved a pixel.
fn note_hit_at(rect: Rect, pos: Pos2) -> Option<NoteHit> {
    let slop = 8.0;
    let padded = rect.expand2(Vec2::new(slop, 3.0));
    if !padded.contains(pos) {
        return None;
    }
    let hw = theme::TRIM_HANDLE_WIDTH_PX.min(rect.width() * 0.45).max(6.0);
    if rect.width() <= hw * 2.0 {
        let mid = (rect.left() + rect.right()) * 0.5;
        return Some(if pos.x >= mid {
            NoteHit::ResizeEnd
        } else {
            NoteHit::ResizeStart
        });
    }
    if pos.x <= rect.left() + hw {
        Some(NoteHit::ResizeStart)
    } else if pos.x >= rect.right() - hw {
        Some(NoteHit::ResizeEnd)
    } else if rect.contains(pos) {
        Some(NoteHit::Body)
    } else if pos.x < rect.center().x {
        Some(NoteHit::ResizeStart)
    } else {
        Some(NoteHit::ResizeEnd)
    }
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
        let Some(hit) = note_hit_at(r, pos) else {
            continue;
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

    let pps = roll.width().max(1.0) / view_len.max(1e-6);
    let (minor, major) = piano_grid_steps(pps, model.tempo_bpm);
    let beat = timeline::beat_secs(model.tempo_bpm);
    let bar = beat * timeline::BEATS_PER_BAR;
    let t0 = view_start.max(0.0);
    let t1 = view_start + view_len;
    let mut t = (t0 / minor).floor() * minor;
    if t < 0.0 {
        t = 0.0;
    }
    let mut guard = 0;
    while t <= t1 + minor && guard < 4_000 {
        if t >= 0.0 {
            let x = x_at_time(t, roll, view_start, view_len);
            if x >= roll.left() - 1.0 && x <= roll.right() + 1.0 {
                let stroke = if near_multiple(t, bar) || near_multiple(t, major) {
                    Stroke::new(1.2_f32, Color32::from_rgba_unmultiplied(255, 255, 255, 48))
                } else if near_multiple(t, beat) {
                    Stroke::new(1.0_f32, theme::color_piano_grid_beat())
                } else {
                    Stroke::new(1.0_f32, theme::color_piano_grid_step())
                };
                painter.line_segment(
                    [Pos2::new(x, roll.top()), Pos2::new(x, roll.bottom())],
                    stroke,
                );
            }
        }
        t += minor;
        guard += 1;
    }

    shade_outside_sequence(&painter, roll, model.duration_secs, view_start, view_len);

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
        paint_note_waveform(&clip_p, model, note, r);
        if sel {
            clip_p.rect_stroke(r, 2.0, Stroke::new(1.5_f32, Color32::WHITE));
        } else {
            let edge = Color32::from_rgba_unmultiplied(40, 44, 70, 110);
            let edge_w = 3.0_f32.min(r.width() * 0.2).max(1.5);
            clip_p.rect_filled(
                Rect::from_min_size(r.left_top(), Vec2::new(edge_w, r.height())),
                0.0,
                edge,
            );
            clip_p.rect_filled(
                Rect::from_min_max(Pos2::new(r.right() - edge_w, r.top()), r.max),
                0.0,
                edge,
            );
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
        let x = x_at_time(ph, roll, view_start, view_len);
        if x >= roll.left() && x <= roll.right() {
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
    let ctx = &resp.ctx;
    let pointer = ctx.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.press_origin(),
            i.pointer.latest_pos().or(i.pointer.interact_pos()),
            i.pointer.hover_pos(),
        )
    });
    let (pressed, down, press_origin, latest, hover) = pointer;

    let alt = ctx.input(|i| i.modifiers.alt);
    if resp.hovered() {
        if let Some(hover) = hover {
            if let Some((_, hit)) = hit_editor_note(model.notes, hover, roll, view_start, view_len) {
                ctx.set_cursor_icon(match hit {
                    NoteHit::ResizeStart | NoteHit::ResizeEnd => CursorIcon::ResizeHorizontal,
                    NoteHit::Body if alt => CursorIcon::Alias,
                    NoteHit::Body => CursorIcon::Move,
                });
            }
        }
    }
    if note_drag.is_some() && down {
        ctx.set_cursor_icon(match *note_drag {
            Some(NoteDrag::Move { .. }) => CursorIcon::Grabbing,
            Some(NoteDrag::Copy { .. }) => CursorIcon::Alias,
            Some(NoteDrag::ResizeStart { .. } | NoteDrag::ResizeEnd { .. }) => {
                CursorIcon::ResizeHorizontal
            }
            None => CursorIcon::Default,
        });
    }

    if ctx.input(|i| i.pointer.button_clicked(PointerButton::Secondary)) {
        if let Some(pos) = hover.or(latest) {
            if let Some((id, _)) = hit_editor_note(model.notes, pos, roll, view_start, view_len) {
                return Some(PianoRollAction::DeleteNote { id });
            }
        }
    }

    // Decide the gesture from where the button went down, not from where the cursor is
    // after egui's drag threshold. Lengthening a note moves the pointer off the rect
    // before a drag would otherwise be recognized, so the resize never started.
    if pressed {
        if let Some(origin) = press_origin {
            if let Some((id, hit)) = hit_editor_note(model.notes, origin, roll, view_start, view_len)
            {
                *note_drag = Some(match hit {
                    NoteHit::ResizeStart => NoteDrag::ResizeStart { id },
                    NoteHit::ResizeEnd => NoteDrag::ResizeEnd { id },
                    NoteHit::Body => {
                        let start = model
                            .notes
                            .iter()
                            .find(|n| n.id == id)
                            .map(|n| n.start_time_secs)
                            .unwrap_or(0.0);
                        let grab_offset_secs =
                            time_at_pointer(origin.x, roll, view_start, view_len) - start;
                        if alt {
                            NoteDrag::Copy {
                                source: id,
                                grab_offset_secs,
                            }
                        } else {
                            NoteDrag::Move {
                                id,
                                grab_offset_secs,
                            }
                        }
                    }
                });
            }
        }
    }

    if let Some(drag) = *note_drag {
        if down && !pressed {
            if let Some(pos) = latest {
                let time = time_at_pointer(pos.x, roll, view_start, view_len);
                let step = piano_grid_steps(
                    roll.width().max(1.0) / view_len.max(1e-6),
                    model.tempo_bpm,
                )
                .0;
                return Some(match drag {
                    NoteDrag::Move { id, grab_offset_secs } => PianoRollAction::MoveNote {
                        id,
                        slot: slot_at_y(roll, pos.y),
                        time: snap_note_move(time - grab_offset_secs, step),
                    },
                    NoteDrag::Copy {
                        source,
                        grab_offset_secs,
                    } => PianoRollAction::DuplicateNote {
                        source,
                        slot: slot_at_y(roll, pos.y),
                        time: snap_note_move(time - grab_offset_secs, step),
                        grab_offset_secs,
                    },
                    NoteDrag::ResizeStart { id } => {
                        let end = model
                            .notes
                            .iter()
                            .find(|n| n.id == id)
                            .map(|n| n.end_time_secs())
                            .unwrap_or(time);
                        PianoRollAction::ResizeNoteStart {
                            id,
                            start: snap_note_start(end, time, step),
                        }
                    }
                    NoteDrag::ResizeEnd { id } => {
                        let start = model
                            .notes
                            .iter()
                            .find(|n| n.id == id)
                            .map(|n| n.start_time_secs)
                            .unwrap_or(0.0);
                        PianoRollAction::ResizeNote {
                            id,
                            end: snap_note_end(start, time, step),
                        }
                    }
                });
            }
        } else if !down {
            *note_drag = None;
        }
    }

    if pressed {
        if let Some(id) = note_drag.as_ref().map(|drag| match drag {
            NoteDrag::Move { id, .. }
            | NoteDrag::Copy { source: id, .. }
            | NoteDrag::ResizeStart { id }
            | NoteDrag::ResizeEnd { id } => *id,
        }) {
            return Some(PianoRollAction::SelectNote { id: Some(id) });
        }
    }

    if resp.clicked_by(PointerButton::Primary) {
        let pos = latest.or(hover)?;
        if keys.contains(pos) {
            return Some(PianoRollAction::SelectNote { id: None });
        }
        if let Some((id, _)) = hit_editor_note(model.notes, pos, roll, view_start, view_len) {
            return Some(PianoRollAction::SelectNote { id: Some(id) });
        }
        if roll.contains(pos) {
            let slot = slot_at_y(roll, pos.y);
            let time = time_at_x(pos.x, roll, view_start, view_len).max(0.0);
            return Some(PianoRollAction::Place { slot, time });
        }
    }
    None
}

/// Pointer x may sit outside the roll while a resize is in progress. Don't clamp it back
/// into the visible window, or the edge stops following the cursor.
fn time_at_pointer(x: f32, roll: Rect, view_start: f32, view_len: f32) -> f32 {
    let u = (x - roll.left()) / roll.width().max(1.0);
    (view_start + u * view_len.max(1e-4)).max(0.0)
}

/// Dragged edge lands on the grid line that is currently drawn. `step` is that line spacing.
fn snap_note_end(start: f32, pointer_time: f32, step: f32) -> f32 {
    let step = step.max(1e-4);
    let snapped = crate::time::snap_time_round(pointer_time, step);
    if snapped > start + 1e-4 {
        snapped
    } else {
        ((start / step).floor() + 1.0) * step
    }
}

fn snap_note_start(end: f32, pointer_time: f32, step: f32) -> f32 {
    let step = step.max(1e-4);
    let snapped = crate::time::snap_time_round(pointer_time, step).max(0.0);
    if snapped + 1e-4 < end {
        snapped
    } else {
        ((end / step).ceil() - 1.0).max(0.0) * step
    }
}

fn snap_note_move(time: f32, step: f32) -> f32 {
    crate::time::snap_time_round(time.max(0.0), step.max(1e-4))
}

#[cfg(test)]
mod tests {
    use super::{
        clamp_piano_start, note_hit_at, piano_grid_steps, snap_note_end, snap_note_move, snap_note_start,
        visible_seconds, NoteHit,
    };
    use egui::{Pos2, Rect};

    #[test]
    fn wider_window_shows_more_time_without_changing_zoom() {
        let pps = 80.0;
        let narrow = visible_seconds(400.0, pps);
        let wide = visible_seconds(800.0, pps);
        assert!((wide - narrow * 2.0).abs() < 1e-3);
        assert!((narrow - 5.0).abs() < 1e-3);
    }

    #[test]
    fn view_never_starts_before_zero() {
        assert_eq!(clamp_piano_start(-12.0), 0.0);
        assert_eq!(clamp_piano_start(f32::NAN), 0.0);
        assert_eq!(clamp_piano_start(f32::NEG_INFINITY), 0.0);
        assert!((clamp_piano_start(1.25) - 1.25).abs() < 1e-6);
    }

    #[test]
    fn both_edges_of_a_note_change_its_length() {
        let rect = Rect::from_min_max(Pos2::new(100.0, 0.0), Pos2::new(200.0, 24.0));
        assert_eq!(
            note_hit_at(rect, Pos2::new(104.0, 12.0)),
            Some(NoteHit::ResizeStart)
        );
        assert_eq!(
            note_hit_at(rect, Pos2::new(150.0, 12.0)),
            Some(NoteHit::Body)
        );
        assert_eq!(
            note_hit_at(rect, Pos2::new(196.0, 12.0)),
            Some(NoteHit::ResizeEnd)
        );
        assert_eq!(
            note_hit_at(rect, Pos2::new(206.0, 12.0)),
            Some(NoteHit::ResizeEnd)
        );
        assert_eq!(note_hit_at(rect, Pos2::new(40.0, 12.0)), None);
    }

    #[test]
    fn resize_snaps_to_the_grid_that_zoom_is_showing() {
        let bpm = 120.0;
        let (fine, _) = piano_grid_steps(800.0, bpm);
        let (coarse, _) = piano_grid_steps(30.0, bpm);
        assert!(fine < coarse, "zoomed-in grid {fine} should be finer than {coarse}");

        let end = snap_note_end(0.0, fine * 0.6, fine);
        assert!(
            (end - fine).abs() < 1e-3,
            "fine snap landed on {end}, step {fine}"
        );
        let coarse_end = snap_note_end(0.0, coarse * 0.6, coarse);
        assert!(
            (coarse_end - coarse).abs() < 1e-3,
            "coarse snap landed on {coarse_end}, step {coarse}"
        );
        assert!((end - coarse_end).abs() > 1e-3);

        let start = snap_note_start(coarse, coarse * 0.4, coarse);
        assert!((start - 0.0).abs() < 1e-3, "left edge snapped to {start}");
        let moved = snap_note_move(fine * 1.6, fine);
        assert!(
            (moved - fine * 2.0).abs() < 1e-3,
            "move snapped to {moved}, step {fine}"
        );
    }
}
