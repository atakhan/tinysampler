//! Multi-resolution, multi-band onset evidence from an STFT.
//!
//! Raw fused novelty is kept beside the whitened envelope. Later stages
//! pick peaks from the envelope and read periodicity from the same curve.

use super::config::Config;
use super::fft::{self, fft as fft_inplace};
use super::util::{self, local_max};

const LOW: usize = 0;
const MID: usize = 1;
const HIGH: usize = 2;
const FULL: usize = 3;
const BANDS: usize = 4;

#[derive(Clone, Debug)]
pub struct OnsetExtraction {
    pub frame_rate: f32,
    pub hop_secs: f32,
    pub time_offset: f32,
    /// Whitened, compressed, peak-emphasised curve in `0..1`.
    pub envelope: Vec<f32>,
    /// Fused novelty before peak picking, same length, in `0..1`.
    pub raw: Vec<f32>,
    pub low: Vec<f32>,
    pub mid: Vec<f32>,
    pub high: Vec<f32>,
    pub full: Vec<f32>,
}

impl OnsetExtraction {
    pub fn time_of(&self, frame: f32) -> f32 {
        self.time_offset + frame * self.hop_secs
    }

    pub fn is_empty(&self) -> bool {
        self.envelope.is_empty()
    }
}

pub fn extract(samples: &[f32], sample_rate: u32, cfg: &Config) -> OnsetExtraction {
    let sample_rate = sample_rate.max(1);
    let hop = adapted_hop(samples.len(), sample_rate, cfg.hop);
    let windows: Vec<usize> = cfg
        .windows
        .iter()
        .copied()
        .filter(|w| *w >= 32 && w.is_power_of_two())
        .collect();
    let windows = if windows.is_empty() {
        vec![1024]
    } else {
        windows
    };
    let ref_window = *windows.iter().min().unwrap_or(&1024);

    let mut acc = [(); BANDS].map(|_| Vec::<f32>::new());
    let mut counts = [(); BANDS].map(|_| Vec::<f32>::new());
    let mut used = 0;

    for window in windows {
        let bands = novelties_for_window(samples, sample_rate, window, hop, cfg);
        if bands[0].is_empty() {
            continue;
        }
        let shift = ((window / 2).saturating_sub(ref_window / 2) / hop) as isize;
        for band in 0..BANDS {
            add_shifted(&mut acc[band], &mut counts[band], &bands[band], shift);
        }
        used += 1;
    }

    if used == 0 {
        return empty(sample_rate, hop, ref_window);
    }

    for band in 0..BANDS {
        for (sample, count) in acc[band].iter_mut().zip(counts[band].iter()) {
            if *count > 0.0 {
                *sample /= *count;
            }
        }
        normalize_curve(&mut acc[band]);
    }

    let weights = band_weights(&acc, sample_rate, hop, cfg);
    let n = acc.iter().map(|c| c.len()).max().unwrap_or(0);
    let mut raw = vec![0.0; n];
    for (band, curve) in acc.iter().enumerate() {
        for (dst, src) in raw.iter_mut().zip(curve.iter()) {
            *dst += *src * weights[band];
        }
    }
    normalize_curve(&mut raw);
    let envelope = whiten_envelope(&raw, sample_rate, hop);

    OnsetExtraction {
        frame_rate: sample_rate as f32 / hop as f32,
        hop_secs: hop as f32 / sample_rate as f32,
        time_offset: (ref_window / 2) as f32 / sample_rate as f32,
        low: acc[LOW].clone(),
        mid: acc[MID].clone(),
        high: acc[HIGH].clone(),
        full: acc[FULL].clone(),
        raw,
        envelope,
    }
}

fn empty(sample_rate: u32, hop: usize, window: usize) -> OnsetExtraction {
    OnsetExtraction {
        frame_rate: sample_rate as f32 / hop.max(1) as f32,
        hop_secs: hop.max(1) as f32 / sample_rate as f32,
        time_offset: (window / 2) as f32 / sample_rate.max(1) as f32,
        envelope: Vec::new(),
        raw: Vec::new(),
        low: Vec::new(),
        mid: Vec::new(),
        high: Vec::new(),
        full: Vec::new(),
    }
}

fn adapted_hop(len: usize, sample_rate: u32, hop: usize) -> usize {
    let dur = len as f32 / sample_rate.max(1) as f32;
    let mut hop = hop.max(32);
    if dur > 120.0 {
        hop *= 2;
    }
    if dur > 300.0 {
        hop *= 2;
    }
    hop
}

fn add_shifted(dst: &mut Vec<f32>, counts: &mut Vec<f32>, src: &[f32], shift: isize) {
    for (i, value) in src.iter().enumerate() {
        let j = i as isize + shift;
        if j < 0 {
            continue;
        }
        let j = j as usize;
        if j >= dst.len() {
            dst.resize(j + 1, 0.0);
            counts.resize(j + 1, 0.0);
        }
        dst[j] += *value;
        counts[j] += 1.0;
    }
}

fn novelties_for_window(
    samples: &[f32],
    sample_rate: u32,
    window: usize,
    hop: usize,
    cfg: &Config,
) -> [Vec<f32>; BANDS] {
    let n_frames = frame_count(samples.len(), window, hop);
    if n_frames == 0 {
        return [(); BANDS].map(|_| Vec::new());
    }
    let fft_size = fft::next_pow2(window);
    let hann = fft::hann(window);
    let ranges = band_ranges(fft_size, sample_rate, cfg);
    let weights = feature_weights(cfg);

    let mut flux = [(); BANDS].map(|_| vec![0.0; n_frames]);
    let mut log_flux = [(); BANDS].map(|_| vec![0.0; n_frames]);
    let mut energy_d = [(); BANDS].map(|_| vec![0.0; n_frames]);
    let mut phase_d = [(); BANDS].map(|_| vec![0.0; n_frames]);
    let mut complex_d = [(); BANDS].map(|_| vec![0.0; n_frames]);

    let bins = fft_size / 2;
    let mut re = vec![0.0; fft_size];
    let mut im = vec![0.0; fft_size];
    let mut mag = vec![0.0; bins];
    let mut prev_mag = vec![0.0; bins];
    let mut prev_energy = [0.0; BANDS];
    let mut phase = vec![0.0; bins];
    let mut prev_phase = vec![0.0; bins];
    let mut prev2_phase = vec![0.0; bins];
    let mut have_prev = false;
    let mut have_prev2 = false;

    for frame in 0..n_frames {
        let start = frame * hop;
        re.fill(0.0);
        im.fill(0.0);
        for n in 0..window {
            let idx = start + n;
            let s = if idx < samples.len() {
                samples[idx]
            } else {
                0.0
            };
            re[n] = s * hann[n];
        }
        fft_inplace(&mut re, &mut im);
        for bin in 1..bins {
            mag[bin] = (re[bin] * re[bin] + im[bin] * im[bin]).sqrt();
            phase[bin] = im[bin].atan2(re[bin]);
        }

        if have_prev {
            for band in 0..BANDS {
                let (lo, hi) = ranges[band];
                let mut flux_s = 0.0;
                let mut log_s = 0.0;
                let mut energy = 0.0;
                let mut phase_s = 0.0;
                let mut complex_s = 0.0;
                for bin in lo..hi {
                    let m = mag[bin];
                    let prev = prev_mag[bin];
                    let dm = m - prev;
                    let rel = if dm > 0.0 {
                        dm / prev.max(m).max(1e-4)
                    } else {
                        0.0
                    };
                    // Ignore bin-to-bin jitter of a steady tone. Normalizing that
                    // jitter later turns a pad into a fake tempo.
                    if rel > 0.08 {
                        flux_s += rel;
                        let l0 = (1.0 + prev).ln();
                        let l1 = (1.0 + m).ln();
                        log_s += (l1 - l0).max(0.0);
                    }
                    energy += m * m;
                    if have_prev2 && m > 1e-3 && rel > 0.08 {
                        let target = 2.0 * prev_phase[bin] - prev2_phase[bin];
                        let dphi = princarg(phase[bin] - target);
                        let w = m.sqrt();
                        phase_s += dphi.abs() * w;
                        let pred_re = prev * target.cos();
                        let pred_im = prev * target.sin();
                        let dre = m * phase[bin].cos() - pred_re;
                        let dim = m * phase[bin].sin() - pred_im;
                        complex_s += (dre * dre + dim * dim).sqrt();
                    }
                }
                let energy_rel = if energy > prev_energy[band] {
                    (energy - prev_energy[band]) / energy.max(prev_energy[band]).max(1e-6)
                } else {
                    0.0
                };
                flux[band][frame] = flux_s;
                log_flux[band][frame] = log_s;
                energy_d[band][frame] = if energy_rel > 0.04 { energy_rel } else { 0.0 };
                phase_d[band][frame] = phase_s;
                complex_d[band][frame] = complex_s;
                prev_energy[band] = energy;
            }
        }

        prev2_phase.copy_from_slice(&prev_phase);
        prev_phase.copy_from_slice(&phase);
        prev_mag.copy_from_slice(&mag);
        have_prev2 = have_prev;
        have_prev = true;
    }

    let mut fused = [(); BANDS].map(|_| vec![0.0; n_frames]);
    for band in 0..BANDS {
        scale_inplace(&mut flux[band]);
        scale_inplace(&mut log_flux[band]);
        scale_inplace(&mut energy_d[band]);
        scale_inplace(&mut phase_d[band]);
        scale_inplace(&mut complex_d[band]);
        for i in 0..n_frames {
            fused[band][i] = weights.flux * flux[band][i]
                + weights.log_flux * log_flux[band][i]
                + weights.energy * energy_d[band][i]
                + weights.phase * phase_d[band][i]
                + weights.complex * complex_d[band][i];
        }
        normalize_curve(&mut fused[band]);
    }
    fused
}

fn frame_count(len: usize, window: usize, hop: usize) -> usize {
    if len == 0 || hop == 0 || window == 0 {
        return 0;
    }
    if len <= window {
        1
    } else {
        1 + (len - window) / hop
    }
}

fn band_ranges(fft_size: usize, sample_rate: u32, cfg: &Config) -> [(usize, usize); BANDS] {
    let nyquist = sample_rate as f32 * 0.5;
    let bins = fft_size / 2;
    let range = |lo: f32, hi: f32| {
        let a = hz_to_bin(lo, fft_size, sample_rate).clamp(1, bins.saturating_sub(1));
        let b = hz_to_bin(hi.min(nyquist - 1.0), fft_size, sample_rate).clamp(a + 1, bins);
        (a, b)
    };
    [
        range(cfg.bands.low.0, cfg.bands.low.1),
        range(cfg.bands.mid.0, cfg.bands.mid.1),
        range(cfg.bands.high.0, cfg.bands.high.1),
        range(20.0, nyquist),
    ]
}

fn hz_to_bin(hz: f32, fft_size: usize, sample_rate: u32) -> usize {
    (hz / sample_rate as f32 * fft_size as f32).round() as usize
}

struct Weights {
    flux: f32,
    log_flux: f32,
    energy: f32,
    phase: f32,
    complex: f32,
}

fn feature_weights(cfg: &Config) -> Weights {
    let f = &cfg.features;
    let sum = (f.flux + f.log_flux + f.energy + f.phase + f.complex).max(1e-6);
    Weights {
        flux: f.flux / sum,
        log_flux: f.log_flux / sum,
        energy: f.energy / sum,
        phase: f.phase / sum,
        complex: f.complex / sum,
    }
}

fn scale_inplace(x: &mut [f32]) {
    let p = util::percentile(x, 0.95);
    if !p.is_finite() || p < 1e-3 {
        x.fill(0.0);
        return;
    }
    for s in x {
        *s /= p;
    }
}

fn normalize_curve(x: &mut [f32]) {
    util::unit_max(x);
}

fn band_weights(
    bands: &[Vec<f32>; BANDS],
    sample_rate: u32,
    hop: usize,
    cfg: &Config,
) -> [f32; BANDS] {
    let frame_rate = sample_rate as f32 / hop as f32;
    let mut w = [0.0; BANDS];
    for band in 0..BANDS {
        w[band] = coarse_periodicity(&bands[band], frame_rate, cfg.min_bpm, cfg.max_bpm);
    }
    let mut sum = w.iter().sum::<f32>();
    if sum < 1e-4 {
        return [0.2, 0.25, 0.2, 0.35];
    }
    // The full mix always keeps a vote, even when one band looks periodic by chance.
    w[FULL] = w[FULL].max(0.2 * sum);
    sum = w.iter().sum::<f32>();
    for weight in &mut w {
        *weight /= sum;
    }
    w
}

fn coarse_periodicity(x: &[f32], frame_rate: f32, min_bpm: f32, max_bpm: f32) -> f32 {
    if x.len() < 16 {
        return 0.0;
    }
    let mean = util::mean(x);
    let y: Vec<f32> = x.iter().map(|v| v - mean).collect();
    let lag_min = (frame_rate * 60.0 / max_bpm).round().max(2.0) as usize;
    let lag_max = (frame_rate * 60.0 / min_bpm).round() as usize;
    let lag_max = lag_max.min(y.len() / 2).max(lag_min + 1);
    let mut peak = 0.0_f32;
    let mut lag = lag_min;
    while lag < lag_max {
        peak = peak.max(normalized_acf(&y, lag));
        lag += 2;
    }
    peak.max(0.0)
}

pub fn normalized_acf(x: &[f32], lag: usize) -> f32 {
    if lag == 0 || lag >= x.len() || x.len() - lag < 8 {
        return 0.0;
    }
    let n = x.len() - lag;
    let mut sum = 0.0;
    let mut e0 = 0.0;
    let mut e1 = 0.0;
    for i in 0..n {
        let a = x[i];
        let b = x[i + lag];
        sum += a * b;
        e0 += a * a;
        e1 += b * b;
    }
    let denom = (e0 * e1).sqrt();
    if denom < 1e-12 {
        0.0
    } else {
        (sum / denom).max(0.0)
    }
}

fn whiten_envelope(raw: &[f32], sample_rate: u32, hop: usize) -> Vec<f32> {
    if raw.is_empty() {
        return Vec::new();
    }
    let mut compressed: Vec<f32> = raw.iter().map(|s| (1.0 + 6.0 * s.max(0.0)).ln()).collect();
    let win = ((0.40 * sample_rate as f32) / hop as f32).round().max(3.0) as usize;
    let mean = util::moving_mean(&compressed, win);
    for (sample, m) in compressed.iter_mut().zip(mean.iter()) {
        *sample = (*sample - m).max(0.0);
    }
    // A short smooth so a beat may sit next to a transient, not only on its peak sample.
    let smoothed = smooth3(&compressed);
    let mut out = smoothed;
    util::unit_max(&mut out);
    out
}

fn smooth3(x: &[f32]) -> Vec<f32> {
    if x.len() < 3 {
        return x.to_vec();
    }
    let mut out = vec![0.0; x.len()];
    out[0] = x[0];
    out[x.len() - 1] = x[x.len() - 1];
    for i in 1..x.len() - 1 {
        out[i] = 0.25 * x[i - 1] + 0.5 * x[i] + 0.25 * x[i + 1];
    }
    out
}

fn princarg(mut x: f32) -> f32 {
    let pi = std::f32::consts::PI;
    let twopi = 2.0 * pi;
    x = (x + pi) % twopi;
    if x < 0.0 {
        x += twopi;
    }
    x - pi
}

/// Peak frames of `envelope`, at least `min_dist` apart, at or above `thresh`.
pub fn strong_peaks(x: &[f32], min_dist: usize, thresh: f32) -> Vec<usize> {
    if x.len() < 3 {
        return Vec::new();
    }
    let mut idx: Vec<usize> = (1..x.len() - 1)
        .filter(|&i| x[i] >= x[i - 1] && x[i] > x[i + 1] && x[i] >= thresh)
        .collect();
    idx.sort_by(|&a, &b| x[b].partial_cmp(&x[a]).unwrap_or(std::cmp::Ordering::Equal));
    let min_dist = min_dist.max(1);
    let mut kept = Vec::new();
    for i in idx {
        if kept
            .iter()
            .all(|&k| (k as isize - i as isize).unsigned_abs() >= min_dist)
        {
            kept.push(i);
        }
    }
    kept.sort_unstable();
    kept
}

pub fn onset_near(envelope: &[f32], frame: f32, radius: f32) -> f32 {
    local_max(envelope, frame, radius)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::musical_time::config::{AnalysisMode, Config};
    use crate::musical_time::preprocess;

    #[test]
    fn impulses_land_on_the_envelope() {
        let sr = 22_050_u32;
        let sr_usize = sr as usize;
        let period = sr_usize / 2;
        let mut audio = vec![0.0; sr_usize * 4];
        let mut at = sr_usize / 10;
        while at + 40 < audio.len() {
            for k in 0..40 {
                let t = k as f32 / sr as f32;
                audio[at + k] += (2.0 * std::f32::consts::PI * 80.0 * t).sin() * (-t * 50.0).exp();
            }
            at += period;
        }
        let cfg = Config::for_mode(AnalysisMode::Fast);
        let signal = preprocess::prepare(&audio, sr, &cfg);
        let onset = extract(&signal.samples, signal.sample_rate, &cfg);
        assert!(onset.envelope.len() > 20, "frames {}", onset.envelope.len());
        let mut hits = 0;
        let mut expected = sr_usize / 10;
        while expected + period < audio.len() {
            let time = expected as f32 / sr as f32;
            let frame = (time - onset.time_offset) / onset.hop_secs;
            if onset_near(&onset.envelope, frame, 3.0) > 0.35 {
                hits += 1;
            }
            expected += period;
        }
        assert!(hits >= 5, "hits {hits} of the impulse train");
    }
}
