//! Timeline geometry, hit-testing, and ruler painting.

#![allow(clippy::too_many_arguments)]

use egui::{Color32, Context, FontId, Painter, Pos2, Rect, RichText, Stroke, Vec2};

use crate::model::{Clip, ClipId, Project};
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

pub fn clip_rect_on_timeline(
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

pub fn trim_hit_test(
    proj: &Project,
    selected_id: ClipId,
    pos: Pos2,
    rect: Rect,
    view_left: f32,
    pps: f32,
    scroll: f32,
    sample_rate: u32,
) -> Option<TrimDrag> {
    let clip = proj.clips.iter().find(|c| c.id == selected_id)?;
    let cr = clip_rect_on_timeline(clip, rect, view_left, pps, scroll, sample_rate);
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
    rect: Rect,
    view_left: f32,
    pps: f32,
    scroll: f32,
    sample_rate: u32,
) -> bool {
    let Some(sid) = selected else {
        return false;
    };
    trim_hit_test(proj, sid, pos, rect, view_left, pps, scroll, sample_rate).is_some()
}

/// Selected clip body (full rect minus trim handles) — for move hover / drag start.
pub fn pointer_on_selected_clip_move_body(
    proj: &Project,
    selected: Option<ClipId>,
    pos: Pos2,
    rect: Rect,
    view_left: f32,
    pps: f32,
    scroll: f32,
    sample_rate: u32,
) -> bool {
    let Some(sid) = selected else {
        return false;
    };
    if trim_hit_test(proj, sid, pos, rect, view_left, pps, scroll, sample_rate).is_some() {
        return false;
    }
    let Some(clip) = proj.clips.iter().find(|c| c.id == sid) else {
        return false;
    };
    let cr = clip_rect_on_timeline(clip, rect, view_left, pps, scroll, sample_rate);
    cr.contains(pos)
}

pub fn clip_index_at_pointer(
    proj: &Project,
    p: Pos2,
    rect: Rect,
    view_left: f32,
    pps: f32,
    scroll: f32,
    sample_rate: u32,
) -> Option<usize> {
    let mut order: Vec<usize> = (0..proj.clips.len()).collect();
    // Prefer preview (ghost) clips so stacked duplicates are picked on top.
    order.sort_by_key(|&i| !proj.clips[i].placement_preview);
    order.into_iter().find_map(|i| {
        let clip = &proj.clips[i];
        let r = clip_rect_on_timeline(clip, rect, view_left, pps, scroll, sample_rate);
        r.contains(p).then_some(i)
    })
}

pub fn clip_id_at_pointer(
    proj: &Project,
    p: Pos2,
    rect: Rect,
    view_left: f32,
    pps: f32,
    scroll: f32,
    sample_rate: u32,
) -> Option<ClipId> {
    let i = clip_index_at_pointer(proj, p, rect, view_left, pps, scroll, sample_rate)?;
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
    let span_secs = ruler_rect.width() / pps;
    let t_max = scroll / pps + span_secs + step * 2.0;

    if let Some(minor_step) = time_ruler_minor_step(step, pps) {
        let t_minor0 = (scroll / pps / minor_step).floor() * minor_step;
        let mut tm = t_minor0;
        while tm <= t_max {
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
            let label = format_ruler_time(t, step);
            let galley = ctx.fonts(|f| f.layout_no_wrap(label, font.clone(), text_col));
            let tw = galley.rect.width();
            let tx = (x - tw * 0.5).clamp(ruler_rect.left() + 2.0, ruler_rect.right() - tw - 2.0);
            painter.galley(Pos2::new(tx, ruler_rect.top() + 3.0), galley, text_col);
        }
        t += step;
    }
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
    let m = theme::PLAYHEAD_EDGE_MARGIN_PX
        .min(rect.width() * 0.15)
        .max(20.0);
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
