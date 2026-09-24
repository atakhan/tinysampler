//! Autocorrelation and a Fourier tempogram, combined on a BPM grid.

use super::config::Config;
use super::fft::{self, fft as fft_inplace};
use super::onset::normalized_acf;
use super::util::{self, parabolic};

#[derive(Clone, Debug)]
pub struct Periodicity {
    pub bpm_min: f32,
    pub bpm_step: f32,
    pub scores: Vec<f32>,
    pub rhythmicity: f32,
    pub peaks: Vec<RawPeak>,
}

#[derive(Clone, Debug)]
pub struct RawPeak {
    pub bpm: f32,
    pub score: f32,
}

#[derive(Clone, Debug)]
pub struct WindowPeriod {
    pub start_frame: usize,
    pub end_frame: usize,
    pub bpm: Option<f32>,
    pub rhythmicity: f32,
    pub confidence: f32,
}

pub fn analyze_envelope(
    envelope: &[f32],
    frame_rate: f32,
    cfg: &Config,
) -> (Periodicity, Vec<WindowPeriod>) {
    let windows = local_windows(envelope.len(), frame_rate, cfg);
    let n_bins = bpm_bins(cfg);
    let mut acc = vec![0.0; n_bins];
    let mut weight_sum = 0.0;
    let mut rhy_sum = 0.0;
    let mut locals = Vec::with_capacity(windows.len());

    for (start, end) in windows {
        let slice = &envelope[start..end.min(envelope.len())];
        let (curve, rhy) = periodicity_curve(slice, frame_rate, cfg);
        let frames = end.saturating_sub(start).max(1) as f32;
        let weight = rhy * rhy * frames.sqrt();
        let bpm = best_bpm(&curve, cfg).filter(|_| rhy >= cfg.rhythmicity_min * 0.75);
        locals.push(WindowPeriod {
            start_frame: start,
            end_frame: end,
            bpm,
            rhythmicity: rhy,
            confidence: (rhy * curve.iter().copied().fold(0.0_f32, f32::max)).clamp(0.0, 1.0),
        });
        if weight <= 1e-6 {
            continue;
        }
        for (dst, src) in acc.iter_mut().zip(curve.iter()) {
            *dst += *src * weight;
        }
        weight_sum += weight;
        rhy_sum += rhy * weight;
    }

    let (scores, rhythmicity) = if weight_sum > 0.0 {
        for s in &mut acc {
            *s /= weight_sum;
        }
        util::unit_max(&mut acc);
        (acc, (rhy_sum / weight_sum).clamp(0.0, 1.0))
    } else {
        let (curve, rhy) = periodicity_curve(envelope, frame_rate, cfg);
        (curve, rhy)
    };

    let peaks = pick_peaks(&scores, cfg);
    (
        Periodicity {
            bpm_min: cfg.min_bpm,
            bpm_step: cfg.bpm_step,
            scores,
            rhythmicity,
            peaks,
        },
        locals,
    )
}

pub fn score_at(periodicity: &Periodicity, bpm: f32) -> f32 {
    if periodicity.scores.is_empty() || periodicity.bpm_step <= 0.0 {
        return 0.0;
    }
    let pos = (bpm - periodicity.bpm_min) / periodicity.bpm_step;
    util::sample_at(&periodicity.scores, pos).clamp(0.0, 1.0)
}

fn bpm_bins(cfg: &Config) -> usize {
    let step = cfg.bpm_step.max(0.05);
    (((cfg.max_bpm - cfg.min_bpm) / step).floor() as usize).max(1) + 1
}

fn bpm_of(index: usize, cfg: &Config) -> f32 {
    cfg.min_bpm + index as f32 * cfg.bpm_step
}

pub fn periodicity_curve(envelope: &[f32], frame_rate: f32, cfg: &Config) -> (Vec<f32>, f32) {
    let n_bins = bpm_bins(cfg);
    if envelope.len() < 16 || frame_rate <= 1.0 {
        return (vec![0.0; n_bins], 0.0);
    }
    let mean = util::mean(envelope);
    let y: Vec<f32> = envelope.iter().map(|v| v - mean).collect();
    let mut acf = vec![0.0; n_bins];
    let mut values = Vec::with_capacity(n_bins);
    for i in 0..n_bins {
        let bpm = bpm_of(i, cfg).max(1.0);
        let lag = (frame_rate * 60.0 / bpm).round().max(1.0) as usize;
        let s = normalized_acf(&y, lag);
        acf[i] = s;
        values.push(s);
    }
    let mut tempo = fourier_tempogram(&y, frame_rate, cfg);
    util::unit_max(&mut acf);
    util::unit_max(&mut tempo);
    let mut scores = vec![0.0; n_bins];
    for i in 0..n_bins {
        scores[i] = 0.75 * acf[i] + 0.25 * tempo[i];
    }
    smooth5(&mut scores);
    util::unit_max(&mut scores);

    values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let peak = values.last().copied().unwrap_or(0.0).max(0.0);
    let median = if values.is_empty() {
        0.0
    } else {
        values[values.len() / 2].max(0.0)
    };
    let contrast = ((peak - median) / (peak + 0.18)).clamp(0.0, 1.0);
    let rhythmicity = (peak * contrast).sqrt().clamp(0.0, 1.0);
    (scores, rhythmicity)
}

fn fourier_tempogram(envelope: &[f32], frame_rate: f32, cfg: &Config) -> Vec<f32> {
    let n_bins = bpm_bins(cfg);
    let n = fft::next_pow2(envelope.len().max(8) * 4);
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    let hann = fft::hann(envelope.len());
    for (i, s) in envelope.iter().enumerate() {
        re[i] = *s * hann[i];
    }
    fft_inplace(&mut re, &mut im);
    let mut mag = vec![0.0; n / 2];
    for (bin, m) in mag.iter_mut().enumerate() {
        *m = (re[bin] * re[bin] + im[bin] * im[bin]).sqrt();
    }
    let mut out = vec![0.0; n_bins];
    for i in 0..n_bins {
        let freq = bpm_of(i, cfg) / 60.0;
        let bin = freq / frame_rate * n as f32;
        out[i] = util::sample_at(&mag, bin);
    }
    out
}

fn smooth5(x: &mut [f32]) {
    if x.len() < 5 {
        return;
    }
    let src = x.to_vec();
    for i in 2..x.len() - 2 {
        x[i] = (src[i - 2] + 2.0 * src[i - 1] + 3.0 * src[i] + 2.0 * src[i + 1] + src[i + 2]) / 9.0;
    }
}

fn best_bpm(scores: &[f32], cfg: &Config) -> Option<f32> {
    let (i, score) = scores
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))?;
    if *score < 1e-4 {
        None
    } else {
        Some(bpm_of(i, cfg))
    }
}

fn pick_peaks(scores: &[f32], cfg: &Config) -> Vec<RawPeak> {
    if scores.len() < 3 {
        return Vec::new();
    }
    let best = scores.iter().copied().fold(0.0_f32, f32::max);
    if best < 1e-5 {
        return Vec::new();
    }
    let thresh = best * 0.34;
    let mut peaks = Vec::new();
    for i in 1..scores.len() - 1 {
        if scores[i] >= scores[i - 1] && scores[i] > scores[i + 1] && scores[i] >= thresh {
            let delta = parabolic(scores[i - 1], scores[i], scores[i + 1]);
            peaks.push(RawPeak {
                bpm: bpm_of(i, cfg) + delta * cfg.bpm_step,
                score: scores[i],
            });
        }
    }
    peaks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    cluster(&mut peaks);
    peaks.truncate(8);
    peaks
}

fn cluster(peaks: &mut Vec<RawPeak>) {
    let sorted = std::mem::take(peaks);
    for peak in sorted {
        if let Some(existing) = peaks
            .iter_mut()
            .find(|p| util::rel_close(p.bpm, peak.bpm, 0.035))
        {
            if peak.score > existing.score {
                *existing = peak;
            }
        } else {
            peaks.push(peak);
        }
    }
    peaks.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

/// Half- and double-time readings of the same peaks, looked up on the curve.
pub fn with_octaves(peaks: &[RawPeak], periodicity: &Periodicity, cfg: &Config) -> Vec<RawPeak> {
    let mut out = peaks.to_vec();
    let seeds: Vec<f32> = out.iter().map(|p| p.bpm).collect();
    for bpm in seeds {
        for factor in [0.5_f32, 2.0, 0.25, 4.0] {
            let candidate = bpm * factor;
            if !(cfg.min_bpm..=cfg.max_bpm).contains(&candidate) {
                continue;
            }
            if out.iter().any(|p| util::rel_close(p.bpm, candidate, 0.035)) {
                continue;
            }
            out.push(RawPeak {
                bpm: candidate,
                score: score_at(periodicity, candidate),
            });
        }
    }
    out.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out.truncate(10);
    out
}

fn local_windows(n_frames: usize, frame_rate: f32, cfg: &Config) -> Vec<(usize, usize)> {
    let dur = n_frames as f32 / frame_rate.max(1.0);
    if n_frames < 16 || dur < 12.0 {
        return vec![(0, n_frames)];
    }
    let lengths: &[f32] = match cfg.mode {
        super::config::AnalysisMode::Fast => &[8.0],
        super::config::AnalysisMode::Deep => &[8.0, 16.0],
    };
    let mut out = Vec::new();
    for win_secs in lengths {
        let win = (*win_secs * frame_rate) as usize;
        if win < 16 || win > n_frames {
            continue;
        }
        let hop = (win / 2).max(1);
        let mut start = 0;
        while start + win <= n_frames {
            out.push((start, start + win));
            start += hop;
        }
    }
    if out.is_empty() {
        vec![(0, n_frames)]
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::musical_time::config::{AnalysisMode, Config};

    #[test]
    fn pulse_train_peaks_near_its_period() {
        let frame_rate = 100.0;
        let bpm = 120.0_f32;
        let period = (frame_rate * 60.0 / bpm).round() as usize;
        let mut env = vec![0.0; 800];
        let mut i = 10;
        while i < env.len() {
            env[i] = 1.0;
            if i + 1 < env.len() {
                env[i + 1] = 0.5;
            }
            i += period;
        }
        let cfg = Config::for_mode(AnalysisMode::Fast);
        let (scores, rhy) = periodicity_curve(&env, frame_rate, &cfg);
        assert!(rhy > 0.45, "rhythmicity {rhy}");
        let (best_i, best_score) = scores
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|(i, s)| (i, *s))
            .unwrap();
        let best = cfg.min_bpm + best_i as f32 * cfg.bpm_step;
        let pos = (bpm - cfg.min_bpm) / cfg.bpm_step;
        let at_truth = crate::musical_time::util::sample_at(&scores, pos);
        assert!(
            at_truth > 0.65 * best_score,
            "truth {at_truth} best {best_score} at {best}"
        );
    }
}
