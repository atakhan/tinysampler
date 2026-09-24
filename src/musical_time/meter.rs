//! Bar length from accent patterns on an existing beat grid.
//! Meter is optional: a tempo can stand without it.

use super::onset::onset_near;
use super::types::{Bar, Beat, MeterEstimate, MeterOption};

pub fn estimate(
    beats: &[Beat],
    onset: &[f32],
    low: &[f32],
    frame_of: impl Fn(f32) -> f32,
) -> (MeterEstimate, Vec<Beat>, Vec<Bar>) {
    if beats.len() < 8 {
        return (MeterEstimate::default(), Vec::new(), Vec::new());
    }
    let low_ok = low.len() == onset.len() && !low.is_empty();
    let mut options = Vec::new();
    for meter in [2_u8, 3, 4, 6] {
        if beats.len() < meter as usize * 2 {
            continue;
        }
        let mut best_score = f32::MIN;
        let mut best_phase = 0_usize;
        for phase in 0..meter as usize {
            let mut down = 0.0;
            let mut other = 0.0;
            let mut n_down = 0.0;
            let mut n_other = 0.0;
            for (i, beat) in beats.iter().enumerate() {
                let frame = frame_of(beat.time_secs);
                let accent = accent_at(onset, low, frame, low_ok);
                if i % meter as usize == phase {
                    down += accent;
                    n_down += 1.0;
                } else {
                    other += accent;
                    n_other += 1.0;
                }
            }
            if n_down < 2.0 || n_other < 1.0 {
                continue;
            }
            let ds = down / n_down;
            let os = other / n_other;
            let mut score = (ds - os) / (ds + 0.08);
            score *= match meter {
                4 => 1.0,
                3 => 0.98,
                2 => 0.96,
                _ => 0.94,
            };
            if score > best_score {
                best_score = score;
                best_phase = phase;
            }
        }
        if best_score.is_finite() {
            options.push((meter, best_phase, best_score.max(0.0)));
        }
    }
    if options.is_empty() {
        return (MeterEstimate::default(), Vec::new(), Vec::new());
    }
    options.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    let best = options[0];
    let second = options.get(1).map(|o| o.2).unwrap_or(0.0);
    let confidence = ((best.2 - second) / (best.2 + 0.15)).clamp(0.0, 1.0);
    let alternatives = options
        .iter()
        .map(|(meter, _, score)| MeterOption {
            beats_per_bar: *meter,
            confidence: (score / (best.2 + 1e-3)).clamp(0.0, 1.0),
        })
        .collect();
    let clear = best.2 > 0.12 && confidence > 0.18;
    let estimate = MeterEstimate {
        beats_per_bar: if clear { Some(best.0) } else { None },
        confidence: if clear { confidence } else { confidence * 0.5 },
        alternatives,
    };
    if !clear {
        return (estimate, Vec::new(), Vec::new());
    }
    let (downbeats, bars) = downbeats_for(beats, best.0, best.1);
    (estimate, downbeats, bars)
}

fn accent_at(onset: &[f32], low: &[f32], frame: f32, low_ok: bool) -> f32 {
    let full = onset_near(onset, frame, 1.5);
    if low_ok {
        0.55 * full + 0.45 * onset_near(low, frame, 1.5)
    } else {
        full
    }
}

fn downbeats_for(beats: &[Beat], meter: u8, phase: usize) -> (Vec<Beat>, Vec<Bar>) {
    let mut downbeats = Vec::new();
    for (i, beat) in beats.iter().enumerate() {
        if i % meter as usize == phase {
            downbeats.push(Beat {
                time_secs: beat.time_secs,
                position: downbeats.len() as f32,
                strength: beat.strength,
            });
        }
    }
    let mut bars = Vec::new();
    for (i, down) in downbeats.iter().enumerate() {
        let end = downbeats
            .get(i + 1)
            .map(|n| n.time_secs)
            .unwrap_or(beats.last().map(|b| b.time_secs).unwrap_or(down.time_secs));
        if end > down.time_secs {
            bars.push(Bar {
                start_secs: down.time_secs,
                end_secs: end,
                downbeat_index: i,
            });
        }
    }
    (downbeats, bars)
}
