//! Beat grids: phase search on a comb, then a dynamic-programming tracker.
//!
//! A beat does not have to sit on the single loudest onset sample. Scoring
//! looks in a short neighborhood, and the tracker may drift when the pulse does.

use super::onset::{onset_near, strong_peaks};
use super::util::{self, tempo_prior};

#[derive(Clone, Debug)]
pub struct GridScore {
    pub bpm: f32,
    pub phase_frames: f32,
    pub align: f32,
    pub offbeat: f32,
    pub hit_rate: f32,
    pub coverage: f32,
    pub low_align: f32,
    pub score: f32,
    pub periodicity: f32,
    pub phase_confidence: f32,
}

/// Mix a neural `P(beat)` curve into onset evidence when the lengths match.
/// A mismatched curve is ignored so a bad model cannot shift the frame grid.
pub fn mix_evidence(onset: &[f32], neural_beat: Option<&[f32]>) -> Vec<f32> {
    let Some(neural) = neural_beat else {
        return onset.to_vec();
    };
    if neural.len() != onset.len() {
        return onset.to_vec();
    }
    onset
        .iter()
        .zip(neural.iter())
        .map(|(o, p)| 0.55 * *o + 0.45 * p.clamp(0.0, 1.0))
        .collect()
}

pub fn score_period(
    onset: &[f32],
    low: &[f32],
    bpm: f32,
    frame_rate: f32,
    periodicity: f32,
) -> Option<GridScore> {
    if onset.len() < 8 || bpm < 1.0 || frame_rate <= 1.0 {
        return None;
    }
    let period = frame_rate * 60.0 / bpm;
    if period < 2.0 || period * 2.2 > onset.len() as f32 {
        return None;
    }
    let radius = (period * 0.08).clamp(1.0, 6.0);
    let low_useful = util::mean(low) > 0.02 && low.len() == onset.len();
    let n_phase = period.floor().max(1.0) as usize;
    let mut best: Option<PhasePick> = None;
    let mut phase_scores = Vec::with_capacity(n_phase);
    for phase in 0..n_phase {
        let (align, off, low_a, count) =
            comb_means(onset, low, phase as f32, period, radius, low_useful);
        if count < 3.0 {
            continue;
        }
        let pick_score = align - 0.45 * off;
        phase_scores.push(pick_score);
        let replace = best.as_ref().map(|b| pick_score > b.pick).unwrap_or(true);
        if replace {
            best = Some(PhasePick {
                phase: phase as f32,
                align,
                off,
                low_a,
                pick: pick_score,
            });
        }
    }
    let best = best?;
    let beats = comb_beats(best.phase, period, onset.len());
    if beats.len() < 3 {
        return None;
    }
    let hit_thresh = 0.42;
    let hits = beats
        .iter()
        .filter(|b| onset_near(onset, **b, radius) >= hit_thresh)
        .count();
    let hit_rate = hits as f32 / beats.len() as f32;
    let min_dist = (frame_rate * 0.12).round().max(2.0) as usize;
    let peaks = strong_peaks(onset, min_dist, 0.40);
    let tol = period * 0.12;
    let coverage = if peaks.len() < 4 {
        hit_rate
    } else {
        let matched = peaks
            .iter()
            .filter(|p| beats.iter().any(|b| (*b - **p as f32).abs() <= tol))
            .count();
        matched as f32 / peaks.len() as f32
    };
    let contrast = ((best.align - best.off) / (best.align + 0.05)).clamp(0.0, 1.0);
    let prior = tempo_prior(bpm);
    let raw = if low_useful {
        0.30 * best.align + 0.18 * hit_rate + 0.16 * coverage + 0.18 * contrast + 0.18 * best.low_a
    } else {
        0.36 * best.align + 0.20 * hit_rate + 0.20 * coverage + 0.24 * contrast
    };
    let score = (raw * (0.86 + 0.14 * prior)).clamp(0.0, 1.0);
    let phase_confidence = phase_separation(&phase_scores, best.pick);
    Some(GridScore {
        bpm,
        phase_frames: best.phase,
        align: best.align,
        offbeat: best.off,
        hit_rate,
        coverage,
        low_align: best.low_a,
        score,
        periodicity,
        phase_confidence,
    })
}

struct PhasePick {
    phase: f32,
    align: f32,
    off: f32,
    low_a: f32,
    pick: f32,
}

fn comb_means(
    onset: &[f32],
    low: &[f32],
    phase: f32,
    period: f32,
    radius: f32,
    low_useful: bool,
) -> (f32, f32, f32, f32) {
    let mut align = 0.0;
    let mut off = 0.0;
    let mut low_a = 0.0;
    let mut count = 0.0;
    let mut t = phase;
    while (t as usize) < onset.len() {
        align += onset_near(onset, t, radius);
        off += onset_near(onset, t + period * 0.5, radius);
        low_a += if low_useful {
            onset_near(low, t, radius)
        } else {
            onset_near(onset, t, radius)
        };
        count += 1.0;
        t += period;
    }
    if count == 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    (align / count, off / count, low_a / count, count)
}

fn comb_beats(phase: f32, period: f32, len: usize) -> Vec<f32> {
    let mut beats = Vec::new();
    let mut t = phase;
    while (t as usize) < len {
        beats.push(t);
        t += period;
    }
    beats
}

fn phase_separation(scores: &[f32], best: f32) -> f32 {
    if scores.is_empty() {
        return 0.0;
    }
    let mut sorted = scores.to_vec();
    let med = util::median(&mut sorted);
    ((best - med) / (best.abs() + 0.15)).clamp(0.0, 1.0)
}

/// Ellis-style dynamic programming. Returns frame positions, strictly increasing.
pub fn track_beats(onset: &[f32], period_frames: f32, tightness: f32) -> Vec<f32> {
    let n = onset.len();
    let period_i = period_frames.round().max(1.0) as i32;
    if n < 4 || period_i < 1 {
        return Vec::new();
    }
    let smoothed = triangular(onset, 2);
    let start = -2 * period_i;
    let end = (-period_i / 2).min(-1);
    if start > end {
        return Vec::new();
    }
    let window: Vec<i32> = (start..=end).collect();
    let txwt: Vec<f32> = window
        .iter()
        .map(|w| {
            let ratio = (-*w) as f32 / period_i as f32;
            -tightness * ratio.max(1e-3).ln().powi(2)
        })
        .collect();

    let mut backlink = vec![-1_isize; n];
    let mut cumulative = vec![0.0; n];
    for i in 0..n {
        let mut best = f32::MIN;
        let mut best_prev = -1_isize;
        for (k, w) in window.iter().enumerate() {
            let prev = i as i32 + *w;
            if prev < 0 || prev >= i as i32 {
                continue;
            }
            let value = cumulative[prev as usize] + txwt[k];
            if value > best {
                best = value;
                best_prev = prev as isize;
            }
        }
        if best_prev < 0 {
            cumulative[i] = smoothed[i];
        } else {
            cumulative[i] = best + smoothed[i];
            backlink[i] = best_prev;
        }
    }

    let mut end_i = cumulative
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let mut rev = Vec::new();
    let mut guard = 0;
    while guard <= n {
        rev.push(end_i);
        let prev = backlink[end_i];
        if prev < 0 || prev as usize >= end_i {
            break;
        }
        end_i = prev as usize;
        guard += 1;
    }
    rev.reverse();
    let frames: Vec<f32> = rev.into_iter().map(|i| i as f32).collect();
    snap_to_neighborhood(onset, &frames, (period_frames * 0.1).clamp(1.0, 8.0))
}

fn triangular(x: &[f32], radius: usize) -> Vec<f32> {
    if radius == 0 || x.is_empty() {
        return x.to_vec();
    }
    let mut out = vec![0.0; x.len()];
    for (i, dst) in out.iter_mut().enumerate() {
        let a = i.saturating_sub(radius);
        let b = (i + radius).min(x.len() - 1);
        let mut acc = 0.0;
        let mut w = 0.0;
        for j in a..=b {
            let weight = (radius as isize - (j as isize - i as isize).abs()) as f32 + 1.0;
            acc += x[j] * weight;
            w += weight;
        }
        *dst = if w > 0.0 { acc / w } else { x[i] };
    }
    out
}

fn snap_to_neighborhood(onset: &[f32], beats: &[f32], radius: f32) -> Vec<f32> {
    let mut out = Vec::with_capacity(beats.len());
    let mut prev = -1.0_f32;
    let last = onset.len().saturating_sub(1) as f32;
    for beat in beats {
        let a = (beat - radius).floor().max(0.0) as usize;
        let b = (beat + radius).ceil().min(last) as usize;
        let mut best = *beat;
        let mut best_v = f32::MIN;
        for i in a..=b {
            if onset[i] > best_v {
                best_v = onset[i];
                best = i as f32;
            }
        }
        if best <= prev {
            best = (prev + 1.0).min(last);
        }
        if best > prev {
            out.push(best);
            prev = best;
        }
    }
    out
}

pub fn steady_grid(phase_frames: f32, period_frames: f32, len: usize) -> Vec<f32> {
    comb_beats(phase_frames, period_frames, len)
}

pub fn interval_bpms(beat_secs: &[f32]) -> Vec<f32> {
    beat_secs
        .windows(2)
        .filter_map(|w| {
            let dt = w[1] - w[0];
            (dt > 1e-3).then_some(60.0 / dt)
        })
        .collect()
}

pub fn stability_of(bpms: &[f32], beat_count: usize) -> f32 {
    if bpms.len() < 3 {
        return (beat_count as f32 / 8.0).clamp(0.0, 0.35);
    }
    let mut copy = bpms.to_vec();
    let med = util::median(&mut copy);
    if med < 1.0 {
        return 0.0;
    }
    let mut dev: Vec<f32> = bpms.iter().map(|b| (b - med).abs()).collect();
    let mad = util::median(&mut dev);
    let cv = mad / med;
    let mut stability = (-cv * 14.0).exp();
    if beat_count < 8 {
        stability *= beat_count as f32 / 8.0;
    }
    stability.clamp(0.0, 1.0)
}

pub fn smooth_tempo(bpms: &[f32]) -> Vec<f32> {
    if bpms.len() < 5 {
        return bpms.to_vec();
    }
    let radius = if bpms.len() >= 12 { 2 } else { 1 };
    bpms.iter()
        .enumerate()
        .map(|(i, _)| {
            let a = i.saturating_sub(radius);
            let b = (i + radius + 1).min(bpms.len());
            let mut window = bpms[a..b].to_vec();
            util::median(&mut window)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_follows_a_regular_pulse() {
        let period = 50.0;
        let mut onset = vec![0.0; 600];
        let mut i = 7.0;
        while (i as usize) < onset.len() {
            let k = i as usize;
            onset[k] = 1.0;
            if k + 1 < onset.len() {
                onset[k + 1] = 0.4;
            }
            i += period;
        }
        let beats = track_beats(&onset, period, 30.0);
        assert!(beats.len() >= 8, "beats {}", beats.len());
        let mut close = 0;
        let mut expected = 7.0;
        while (expected as usize) < onset.len() {
            if beats.iter().any(|b| (b - expected).abs() <= 1.5) {
                close += 1;
            }
            expected += period;
        }
        assert!(close >= 8, "close {close}");
    }

    #[test]
    fn neural_curve_is_mixed_only_when_aligned() {
        let onset = [0.0, 1.0, 0.0];
        let mixed = mix_evidence(&onset, Some(&[0.0, 0.0, 1.0]));
        assert!((mixed[1] - 0.55).abs() < 1e-5);
        assert!((mixed[2] - 0.45).abs() < 1e-5);
        let ignored = mix_evidence(&onset, Some(&[1.0, 1.0]));
        assert_eq!(ignored, onset);
    }
}
