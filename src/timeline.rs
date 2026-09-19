//! Timeline geometry, hit-testing, and ruler painting.

#![allow(clippy::too_many_arguments)]

use egui::{Color32, Context, FontId, Painter, Pos2, Rect, RichText, Stroke, Vec2};

use crate::model::{Clip, ClipId, CueMarker, Project};
use crate::theme::{
    self, TRIM_HANDLE_WIDTH_PX,
};

#[derive(Clone, Copy)]
pub enum TrimSide {
    Left,
    Right,
}

#[derive(Clone, Copy)]
pub struct TrimDrag {
    pub clip_id: ClipId,
    pub side: TrimSide,
}

/// Major tick spacing in seconds for the time ruler (~constant label width on screen).
pub fn time_ruler_step_secs(pps: f32) -> f32 {
    const TARGET_MAJOR_PX: f32 = 72.0;
    let approx = (TARGET_MAJOR_PX / pps.max(1e-6)).max(1e-6);
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

#[derive(Clone, Copy)]
pub struct TrackLayout {
    pub area: Rect,
    pub n_lanes: usize,
    pub gutter_w: f32,
    pub marker_h: f32,
}

impl TrackLayout {
    pub fn block_h(&self) -> f32 {
        (self.area.height() / self.n_lanes.max(1) as f32).max(1.0)
    }

    pub fn content_left(&self) -> f32 {
        self.area.left() + self.gutter_w
    }

    pub fn block_rect(&self, lane: usize) -> Rect {
        let h = self.block_h();
        let lane = lane.min(self.n_lanes.saturating_sub(1));
        Rect::from_min_size(
            Pos2::new(self.area.left(), self.area.top() + lane as f32 * h),
            Vec2::new(self.area.width(), h),
        )
    }

    pub fn gutter_rect(&self, lane: usize) -> Rect {
        let b = self.block_rect(lane);
        Rect::from_min_size(b.min, Vec2::new(self.gutter_w, b.height()))
    }

    pub fn marker_bar(&self, lane: usize) -> Rect {
        let b = self.block_rect(lane);
        let h = self.marker_h.min(b.height());
        Rect::from_min_size(
            Pos2::new(self.content_left(), b.top()),
            Vec2::new((b.width() - self.gutter_w).max(0.0), h),
        )
    }

    pub fn clips_rect(&self, lane: usize) -> Rect {
        let b = self.block_rect(lane);
        let top = b.top() + self.marker_h.min(b.height());
        Rect::from_min_max(Pos2::new(self.content_left(), top), b.max)
    }

    pub fn content_rect(&self) -> Rect {
        Rect::from_min_max(
            Pos2::new(self.content_left(), self.area.top()),
            self.area.max,
        )
    }

    pub fn lane_at_y(&self, y: f32) -> usize {
        let i = ((y - self.area.top()) / self.block_h()).floor() as i32;
        i.clamp(0, self.n_lanes.saturating_sub(1) as i32) as usize
    }

    pub fn gutter_contains(&self, pos: Pos2) -> bool {
        pos.x >= self.area.left() && pos.x < self.content_left() && self.area.contains(pos)
    }
}

pub fn clip_rect_on_timeline(
    clip: &Clip,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
    duration_secs: f32,
) -> Rect {
    let x0 = view_left + clip.start_time_secs * pps - scroll;
    let w = duration_secs * pps;
    let pad = 8.0_f32;
    let clips = layout.clips_rect(clip.track_index);
    Rect::from_min_size(
        Pos2::new(x0, clips.top() + pad),
        Vec2::new(w.max(8.0), (clips.height() - pad * 2.0).max(4.0)),
    )
}

pub fn track_index_at_y(y: f32, layout: &TrackLayout) -> usize {
    layout.lane_at_y(y)
}

pub fn trim_hit_test(
    proj: &Project,
    selected_id: ClipId,
    pos: Pos2,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> Option<TrimDrag> {
    let i = proj.clip_index(selected_id)?;
    let dur = proj.clip_sounding_secs_at(i);
    let clip = &proj.clips[i];
    let cr = clip_rect_on_timeline(clip, layout, view_left, pps, scroll, dur);
    let hw = TRIM_HANDLE_WIDTH_PX.min(cr.width() * 0.5);
    let left_h = Rect::from_min_size(cr.min, Vec2::new(hw, cr.height()));
    let right_h = Rect::from_min_max(Pos2::new(cr.right() - hw, cr.top()), cr.max);
    if left_h.contains(pos) {
        return Some(TrimDrag {
            clip_id: clip.id,
            side: TrimSide::Left,
        });
    }
    if right_h.contains(pos) {
        return Some(TrimDrag {
            clip_id: clip.id,
            side: TrimSide::Right,
        });
    }
    None
}

pub fn pointer_near_trim_handle(
    proj: &Project,
    selected: Option<ClipId>,
    pos: Pos2,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> bool {
    let Some(sid) = selected else {
        return false;
    };
    trim_hit_test(proj, sid, pos, layout, view_left, pps, scroll).is_some()
}

/// Selected clip body (full rect minus trim handles) — for move hover / drag start.
pub fn pointer_on_selected_clip_move_body(
    proj: &Project,
    selected: Option<ClipId>,
    pos: Pos2,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> bool {
    let Some(sid) = selected else {
        return false;
    };
    if trim_hit_test(proj, sid, pos, layout, view_left, pps, scroll).is_some() {
        return false;
    }
    let Some(i) = proj.clip_index(sid) else {
        return false;
    };
    let dur = proj.clip_sounding_secs_at(i);
    let clip = &proj.clips[i];
    let cr = clip_rect_on_timeline(clip, layout, view_left, pps, scroll, dur);
    cr.contains(pos)
}

pub fn clip_index_at_pointer(
    proj: &Project,
    p: Pos2,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> Option<usize> {
    let mut order: Vec<usize> = (0..proj.clips.len()).collect();
    order.sort_by_key(|&i| !proj.clips[i].placement_preview);
    order.into_iter().find_map(|i| {
        let dur = proj.clip_sounding_secs_at(i);
        let clip = &proj.clips[i];
        let r = clip_rect_on_timeline(clip, layout, view_left, pps, scroll, dur);
        r.contains(p).then_some(i)
    })
}

pub fn clip_id_at_pointer(
    proj: &Project,
    p: Pos2,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> Option<ClipId> {
    let i = clip_index_at_pointer(proj, p, layout, view_left, pps, scroll)?;
    Some(proj.clips[i].id)
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
    } else if step >= 0.01 {
        format!("{:.2}", secs)
    } else if step >= 0.001 {
        format!("{:.3}", secs)
    } else {
        format!("{:.0} ms", secs * 1000.0)
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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum RulerKind {
    Time,
    Tempo,
}

pub const BEATS_PER_BAR: f32 = 4.0;

pub fn beat_secs(bpm: f32) -> f32 {
    60.0 / bpm.clamp(20.0, 400.0)
}

/// Major/minor spacing in seconds for a 4/4 tempo ruler.
fn tempo_ruler_steps(pps: f32, bpm: f32) -> (f32, Option<f32>) {
    let beat = beat_secs(bpm);
    let bar = beat * BEATS_PER_BAR;
    const TARGET_MAJOR_PX: f32 = 64.0;
    if beat * pps >= TARGET_MAJOR_PX {
        let sixteenth = beat / 4.0;
        let minor = if sixteenth * pps >= 14.0 {
            Some(sixteenth)
        } else {
            None
        };
        return (beat, minor);
    }
    let mut bars = 1.0_f32;
    while bars * bar * pps < TARGET_MAJOR_PX && bars < 128.0 {
        bars *= 2.0;
    }
    let major = bars * bar;
    let minor = if bars <= 1.0 + f32::EPSILON {
        if beat * pps >= 18.0 {
            Some(beat)
        } else {
            None
        }
    } else if bar * pps >= 18.0 {
        Some(bar)
    } else {
        None
    };
    (major, minor)
}

fn format_tempo_label(t: f32, major_secs: f32, bpm: f32) -> String {
    let beat = beat_secs(bpm);
    let bar = beat * BEATS_PER_BAR;
    let t = t.max(0.0);
    if major_secs + 1e-6 < bar {
        let beat_i = (t / beat).round() as i64;
        let bar_n = beat_i / BEATS_PER_BAR as i64 + 1;
        let beat_n = beat_i % BEATS_PER_BAR as i64 + 1;
        format!("{bar_n}.{beat_n}")
    } else {
        let bar_n = (t / bar).round() as i64 + 1;
        format!("{bar_n}")
    }
}

pub fn paint_ruler(
    painter: &Painter,
    ruler_rect: Rect,
    pps: f32,
    scroll: f32,
    ctx: &Context,
    kind: RulerKind,
    bpm: f32,
) {
    match kind {
        RulerKind::Time => paint_time_ruler(painter, ruler_rect, pps, scroll, ctx),
        RulerKind::Tempo => paint_tempo_ruler(painter, ruler_rect, pps, scroll, ctx, bpm),
    }
}

pub fn paint_time_ruler(painter: &Painter, ruler_rect: Rect, pps: f32, scroll: f32, ctx: &Context) {
    let bg = theme::color_ruler_bg();
    let line_major = theme::color_ruler_line_major();
    let line_minor = theme::color_ruler_line_minor();
    let text_col = theme::color_ruler_text();
    painter.rect_filled(ruler_rect, 0.0, bg);
    painter.line_segment(
        [
            Pos2::new(ruler_rect.left(), ruler_rect.bottom()),
            Pos2::new(ruler_rect.right(), ruler_rect.bottom()),
        ],
        Stroke::new(1.0, theme::color_ruler_bottom_line()),
    );

    let step = time_ruler_step_secs(pps);
    if !pps.is_finite() || pps <= 0.0 || !step.is_finite() || step <= 0.0 {
        return;
    }
    let span_secs = ruler_rect.width() / pps;
    let t_max = scroll / pps + span_secs + step * 2.0;
    const MAX_TICKS: u32 = 256;

    if let Some(minor_step) = time_ruler_minor_step(step, pps) {
        if minor_step.is_finite() && minor_step > 0.0 {
            let t_minor0 = (scroll / pps / minor_step).floor() * minor_step;
            let mut tm = t_minor0;
            let mut n = 0u32;
            while tm <= t_max && n < MAX_TICKS {
                if !is_time_ruler_major_tick(tm, step) {
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
                n += 1;
            }
        }
    }

    let t_min = (scroll / pps / step).floor() * step;
    let font = FontId::proportional(11.0);

    let mut t = t_min;
    let mut n = 0u32;
    while t <= t_max && n < MAX_TICKS {
        let x = ruler_rect.left() + t * pps - scroll;
        if x >= ruler_rect.left() - 1.0 && x <= ruler_rect.right() + 1.0 {
            painter.line_segment(
                [
                    Pos2::new(x, ruler_rect.bottom() - 10.0),
                    Pos2::new(x, ruler_rect.bottom()),
                ],
                Stroke::new(1.0, line_major),
            );
            let label = format_ruler_time(t, step);
            let galley = ctx.fonts(|f| f.layout_no_wrap(label, font.clone(), text_col));
            let tw = galley.rect.width();
            let tx = (x - tw * 0.5).clamp(ruler_rect.left() + 2.0, ruler_rect.right() - tw - 2.0);
            painter.galley(Pos2::new(tx, ruler_rect.top() + 3.0), galley, text_col);
        }
        t += step;
        n += 1;
    }
}

pub fn paint_tempo_ruler(
    painter: &Painter,
    ruler_rect: Rect,
    pps: f32,
    scroll: f32,
    ctx: &Context,
    bpm: f32,
) {
    let bg = theme::color_ruler_bg();
    let line_major = theme::color_ruler_line_major();
    let line_minor = theme::color_ruler_line_minor();
    let text_col = theme::color_ruler_text();
    painter.rect_filled(ruler_rect, 0.0, bg);
    painter.line_segment(
        [
            Pos2::new(ruler_rect.left(), ruler_rect.bottom()),
            Pos2::new(ruler_rect.right(), ruler_rect.bottom()),
        ],
        Stroke::new(1.0_f32, theme::color_ruler_bottom_line()),
    );

    let (step, minor_step) = tempo_ruler_steps(pps, bpm);
    if !pps.is_finite() || pps <= 0.0 || !step.is_finite() || step <= 0.0 {
        return;
    }
    let span_secs = ruler_rect.width() / pps;
    let t_max = scroll / pps + span_secs + step * 2.0;
    const MAX_TICKS: u32 = 256;

    if let Some(minor_step) = minor_step {
        if minor_step.is_finite() && minor_step > 0.0 {
            let t_minor0 = (scroll / pps / minor_step).floor() * minor_step;
            let mut tm = t_minor0.max(0.0);
            let mut n = 0u32;
            while tm <= t_max && n < MAX_TICKS {
                if !is_time_ruler_major_tick(tm, step) {
                    let x = ruler_rect.left() + tm * pps - scroll;
                    if x >= ruler_rect.left() - 1.0 && x <= ruler_rect.right() + 1.0 {
                        painter.line_segment(
                            [
                                Pos2::new(x, ruler_rect.bottom() - 5.0),
                                Pos2::new(x, ruler_rect.bottom()),
                            ],
                            Stroke::new(1.0_f32, line_minor),
                        );
                    }
                }
                tm += minor_step;
                n += 1;
            }
        }
    }

    let t_min = ((scroll / pps / step).floor() * step).max(0.0);
    let font = FontId::proportional(11.0);
    let mut t = t_min;
    let mut n = 0u32;
    while t <= t_max && n < MAX_TICKS {
        let x = ruler_rect.left() + t * pps - scroll;
        if x >= ruler_rect.left() - 1.0 && x <= ruler_rect.right() + 1.0 {
            painter.line_segment(
                [
                    Pos2::new(x, ruler_rect.bottom() - 10.0),
                    Pos2::new(x, ruler_rect.bottom()),
                ],
                Stroke::new(1.0_f32, line_major),
            );
            let label = format_tempo_label(t, step, bpm);
            let galley = ctx.fonts(|f| f.layout_no_wrap(label, font.clone(), text_col));
            let tw = galley.rect.width();
            let tx = (x - tw * 0.5).clamp(ruler_rect.left() + 2.0, ruler_rect.right() - tw - 2.0);
            painter.galley(Pos2::new(tx, ruler_rect.top() + 3.0), galley, text_col);
        }
        t += step;
        n += 1;
    }
}

pub fn paint_markers(
    painter: &Painter,
    markers: &[CueMarker],
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) {
    if !pps.is_finite() || pps <= 0.0 {
        return;
    }
    let col = theme::color_marker();
    for m in markers {
        if !(1..=9).contains(&m.slot) || !m.time_secs.is_finite() {
            continue;
        }
        let clips = layout.clips_rect(m.track_index);
        let x = view_left + m.time_secs * pps - scroll;
        if x < clips.left() - 8.0 || x > clips.right() + 8.0 {
            continue;
        }
        painter.line_segment(
            [Pos2::new(x, clips.top()), Pos2::new(x, clips.bottom())],
            Stroke::new(1.5_f32, col),
        );
    }
}

const MARKER_CHIP_W: f32 = 40.0;
const MARKER_CHIP_H: f32 = 18.0;
const MARKER_DELETE_W: f32 = 14.0;

fn marker_chip_rect(time_secs: f32, bar: Rect, view_left: f32, pps: f32, scroll: f32) -> Rect {
    let x = view_left + time_secs * pps - scroll;
    let y = bar.center().y - MARKER_CHIP_H * 0.5;
    Rect::from_min_size(
        Pos2::new(x - MARKER_CHIP_W * 0.5, y),
        Vec2::new(MARKER_CHIP_W, MARKER_CHIP_H),
    )
}

fn marker_delete_rect(chip: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(chip.right() - MARKER_DELETE_W, chip.top()),
        chip.max,
    )
}

/// `(slot, track, hit_delete)`.
pub fn marker_hit_at_pointer(
    markers: &[CueMarker],
    pos: Pos2,
    layout: &TrackLayout,
    view_left: f32,
    pps: f32,
    scroll: f32,
) -> Option<(u8, usize, bool)> {
    let lane = layout.lane_at_y(pos.y);
    let bar = layout.marker_bar(lane);
    if !bar.contains(pos) {
        return None;
    }
    let mut hits: Vec<(u8, usize, bool, f32)> = Vec::new();
    for m in markers {
        if m.track_index != lane || !(1..=9).contains(&m.slot) {
            continue;
        }
        let chip = marker_chip_rect(m.time_secs, bar, view_left, pps, scroll);
        if !chip.contains(pos) {
            continue;
        }
        let on_delete = marker_delete_rect(chip).contains(pos);
        hits.push((m.slot, lane, on_delete, (chip.center().x - pos.x).abs()));
    }
    hits.sort_by(|a, b| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal));
    hits.first().map(|&(slot, track, del, _)| (slot, track, del))
}

pub fn paint_marker_lane(
    painter: &Painter,
    markers: &[CueMarker],
    bar: Rect,
    track_index: usize,
    view_left: f32,
    pps: f32,
    scroll: f32,
    selected: Option<(u8, usize)>,
    track_selected: bool,
) {
    painter.rect_filled(bar, 0.0, theme::color_marker_lane_bg());
    painter.line_segment(
        [
            Pos2::new(bar.left(), bar.bottom()),
            Pos2::new(bar.right(), bar.bottom()),
        ],
        Stroke::new(1.0_f32, theme::color_ruler_bottom_line()),
    );
    let on_track: Vec<&CueMarker> = markers
        .iter()
        .filter(|m| m.track_index == track_index)
        .collect();
    if on_track.is_empty() {
        if track_selected {
            painter.text(
                Pos2::new(bar.left() + 8.0, bar.center().y),
                egui::Align2::LEFT_CENTER,
                "M — метка",
                FontId::proportional(11.0),
                Color32::from_gray(120),
            );
        }
        return;
    }
    let font = FontId::proportional(11.0);
    let col = theme::color_marker();
    let mut ordered = on_track;
    ordered.sort_by_key(|m| m.slot);
    for m in ordered {
        if !(1..=9).contains(&m.slot) || !m.time_secs.is_finite() {
            continue;
        }
        let chip = marker_chip_rect(m.time_secs, bar, view_left, pps, scroll);
        if chip.right() < bar.left() - 2.0 || chip.left() > bar.right() + 2.0 {
            continue;
        }
        let selected = selected == Some((m.slot, track_index));
        painter.rect_filled(chip, 3.0, col);
        if selected {
            painter.rect_stroke(chip, 3.0, Stroke::new(1.5_f32, Color32::WHITE));
        }
        let del = marker_delete_rect(chip);
        painter.rect_filled(del, 3.0, theme::color_marker_delete());
        painter.text(
            Pos2::new(
                chip.left() + (chip.width() - MARKER_DELETE_W) * 0.5,
                chip.center().y,
            ),
            egui::Align2::CENTER_CENTER,
            format!("{}", m.slot),
            font.clone(),
            Color32::BLACK,
        );
        painter.text(
            del.center(),
            egui::Align2::CENTER_CENTER,
            "×",
            font.clone(),
            Color32::from_gray(210),
        );
    }
}

pub fn paint_track_gutter(
    painter: &Painter,
    gutter: Rect,
    lane: usize,
    selected: bool,
) {
    let bg = if selected {
        theme::color_track_gutter_selected()
    } else {
        theme::color_track_gutter()
    };
    painter.rect_filled(gutter, 0.0, bg);
    painter.line_segment(
        [
            Pos2::new(gutter.right(), gutter.top()),
            Pos2::new(gutter.right(), gutter.bottom()),
        ],
        Stroke::new(1.0_f32, theme::color_ruler_bottom_line()),
    );
    let label_col = if selected {
        Color32::WHITE
    } else {
        Color32::from_gray(180)
    };
    painter.text(
        gutter.center(),
        egui::Align2::CENTER_CENTER,
        format!("{}", lane + 1),
        FontId::proportional(16.0),
        label_col,
    );
}

/// Circular transport control; icon is centered.
pub fn round_transport_btn(
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
            .rounding(egui::Rounding::same(r)),
    )
    .on_hover_text(tooltip)
}

pub fn scroll_keep_playhead_in_view(
    rect: Rect,
    playhead_secs: f32,
    pps: f32,
    max_scroll: f32,
    scroll: f32,
) -> f32 {
    let m = playhead_follow_margin(rect);
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

fn playhead_follow_margin(rect: Rect) -> f32 {
    theme::PLAYHEAD_EDGE_MARGIN_PX
        .min(rect.width() * 0.15)
        .max(20.0)
}

pub fn playhead_in_viewport(rect: Rect, playhead_secs: f32, pps: f32, scroll: f32) -> bool {
    let m = playhead_follow_margin(rect);
    let play_x = rect.left() + playhead_secs * pps - scroll;
    play_x >= rect.left() + m && play_x <= rect.right() - m
}

pub fn format_clock(secs: f32) -> String {
    let s = secs.max(0.0);
    let m = (s / 60.0).floor() as u32;
    let rem = s - m as f32 * 60.0;
    format!("{m:02}:{rem:06.3}")
}
