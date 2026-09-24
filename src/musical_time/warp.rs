//! Audio time ↔ beat position. Later time-stretch and warp editing read this map.

use super::types::{Beat, WarpMap, WarpPoint};

pub fn from_beats(beats: &[Beat]) -> WarpMap {
    let points = beats
        .iter()
        .enumerate()
        .map(|(i, beat)| WarpPoint {
            audio_secs: beat.time_secs,
            beat: i as f32,
        })
        .collect();
    WarpMap { points }
}

impl WarpMap {
    pub fn beat_at(&self, audio_secs: f32) -> Option<f32> {
        map_lerp(&self.points, audio_secs, |p| p.audio_secs, |p| p.beat)
    }

    pub fn audio_at(&self, beat: f32) -> Option<f32> {
        map_lerp(&self.points, beat, |p| p.beat, |p| p.audio_secs)
    }
}

fn map_lerp(
    points: &[WarpPoint],
    x: f32,
    get_x: impl Fn(&WarpPoint) -> f32,
    get_y: impl Fn(&WarpPoint) -> f32,
) -> Option<f32> {
    if points.is_empty() || !x.is_finite() {
        return None;
    }
    if points.len() == 1 {
        return Some(get_y(&points[0]));
    }
    if x <= get_x(&points[0]) {
        return Some(extrapolate(&points[0], &points[1], x, &get_x, &get_y));
    }
    let last = points.len() - 1;
    if x >= get_x(&points[last]) {
        return Some(extrapolate(
            &points[last - 1],
            &points[last],
            x,
            &get_x,
            &get_y,
        ));
    }
    for w in points.windows(2) {
        let x0 = get_x(&w[0]);
        let x1 = get_x(&w[1]);
        if x >= x0 && x <= x1 {
            let span = x1 - x0;
            if span.abs() < 1e-6 {
                return Some(get_y(&w[0]));
            }
            let t = (x - x0) / span;
            return Some(get_y(&w[0]) + t * (get_y(&w[1]) - get_y(&w[0])));
        }
    }
    None
}

fn extrapolate(
    a: &WarpPoint,
    b: &WarpPoint,
    x: f32,
    get_x: &impl Fn(&WarpPoint) -> f32,
    get_y: &impl Fn(&WarpPoint) -> f32,
) -> f32 {
    let span = get_x(b) - get_x(a);
    if span.abs() < 1e-6 {
        return get_y(a);
    }
    let slope = (get_y(b) - get_y(a)) / span;
    get_y(a) + (x - get_x(a)) * slope
}
