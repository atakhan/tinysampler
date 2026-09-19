//! Peak pyramid for Audacity-style min/max waveform (UI thread / load thread only).

const BLOCK: usize = 256;

/// One (min, max) bin per [`BLOCK`] source samples.
#[derive(Clone)]
pub struct PeakPyramid {
    pub block: usize,
    pub bins: Vec<(f32, f32)>,
}

impl PeakPyramid {
    pub fn build(data: &[f32]) -> Self {
        if data.is_empty() {
            return Self {
                block: BLOCK,
                bins: Vec::new(),
            };
        }
        let n = (data.len() + BLOCK - 1) / BLOCK;
        let mut bins = vec![(f32::INFINITY, f32::NEG_INFINITY); n];
        for (i, &s) in data.iter().enumerate() {
            let bin = &mut bins[i / BLOCK];
            if s < bin.0 {
                bin.0 = s;
            }
            if s > bin.1 {
                bin.1 = s;
            }
        }
        Self {
            block: BLOCK,
            bins,
        }
    }
}

pub fn sample_at(data: &[f32], pos: f32) -> f32 {
    if data.is_empty() {
        return 0.0;
    }
    let last = data.len() - 1;
    if pos <= 0.0 {
        return data[0];
    }
    let i = pos.floor() as usize;
    if i >= last {
        return data[last];
    }
    let frac = pos - i as f32;
    let a = data[i];
    a + (data[i + 1] - a) * frac
}

/// Inclusive-exclusive sample range `[start, end)`.
pub fn min_max_range(data: &[f32], peaks: &PeakPyramid, mut start: usize, mut end: usize) -> (f32, f32) {
    end = end.min(data.len());
    start = start.min(end);
    if start == end {
        return (0.0, 0.0);
    }

    let mut mn = f32::INFINITY;
    let mut mx = f32::NEG_INFINITY;
    let b = peaks.block;

    while start < end && start % b != 0 {
        let s = data[start];
        mn = mn.min(s);
        mx = mx.max(s);
        start += 1;
    }
    while start + b <= end {
        if let Some(&(a, c)) = peaks.bins.get(start / b) {
            mn = mn.min(a);
            mx = mx.max(c);
        }
        start += b;
    }
    while start < end {
        let s = data[start];
        mn = mn.min(s);
        mx = mx.max(s);
        start += 1;
    }

    if mn.is_finite() && mx.is_finite() {
        (mn, mx)
    } else {
        (0.0, 0.0)
    }
}

/// Audacity-style filled min/max envelope (mono). Only paints columns inside `view_rect`.
pub fn paint_waveform_overlay(
    painter: &egui::Painter,
    clip_rect: egui::Rect,
    view_rect: egui::Rect,
    data: &[f32],
    peaks: &PeakPyramid,
    trim_start: usize,
    trim_end: usize,
    wave_col: egui::Color32,
    zero_col: egui::Color32,
) {
    use egui::{Pos2, Rect, Stroke};

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
            let a = sample_at(data, s0);
            let b = sample_at(data, s1.max(s0 + 1e-4));
            (a.min(b), a.max(b))
        } else {
            min_max_range(data, peaks, s0.floor() as usize, s1.ceil() as usize)
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
