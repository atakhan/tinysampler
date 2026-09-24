//! Analysis copy of the audio. The caller's buffer is never written.

use super::config::Config;

#[derive(Clone, Debug)]
pub struct AnalysisSignal {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub duration_secs: f32,
    pub rms: f32,
    pub silent: bool,
}

pub fn prepare(input: &[f32], sample_rate: u32, cfg: &Config) -> AnalysisSignal {
    let sample_rate = sample_rate.max(1);
    let duration_secs = input.len() as f32 / sample_rate as f32;
    if input.is_empty() {
        return AnalysisSignal {
            samples: Vec::new(),
            sample_rate,
            duration_secs: 0.0,
            rms: 0.0,
            silent: true,
        };
    }

    let mut samples: Vec<f32> = input
        .iter()
        .map(|s| if s.is_finite() { *s } else { 0.0 })
        .collect();
    let target = cfg.target_sample_rate.min(sample_rate).max(1_000);
    if target < sample_rate {
        let cutoff = 0.45 * target as f32;
        samples = one_pole_lowpass(&samples, cutoff, sample_rate as f32);
        samples = one_pole_lowpass(&samples, cutoff, sample_rate as f32);
        samples = resample_linear(&samples, sample_rate, target);
    }

    let mean = super::util::mean(&samples);
    for s in &mut samples {
        *s -= mean;
    }
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    let silent = !rms.is_finite() || rms < 1e-4;
    if !silent {
        let gain = 0.2 / rms;
        for s in &mut samples {
            *s *= gain;
        }
    }

    AnalysisSignal {
        samples,
        sample_rate: if target < sample_rate {
            target
        } else {
            sample_rate
        },
        duration_secs,
        rms,
        silent,
    }
}

fn one_pole_lowpass(x: &[f32], cutoff_hz: f32, sample_rate: f32) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let dt = 1.0 / sample_rate.max(1.0);
    let rc = 1.0 / (2.0 * std::f32::consts::PI * cutoff_hz.max(1.0));
    let a = dt / (rc + dt);
    let mut out = vec![0.0; x.len()];
    let mut acc = x[0];
    out[0] = acc;
    for i in 1..x.len() {
        acc += a * (x[i] - acc);
        out[i] = acc;
    }
    out
}

fn resample_linear(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if input.is_empty() || from == 0 || to == 0 || from == to {
        return input.to_vec();
    }
    let out_len = ((input.len() as f64) * f64::from(to) / f64::from(from))
        .round()
        .max(1.0) as usize;
    let scale = from as f32 / to as f32;
    let mut out = vec![0.0; out_len];
    for (i, sample) in out.iter_mut().enumerate() {
        let x = i as f32 * scale;
        let i0 = x.floor() as usize;
        let frac = x - i0 as f32;
        let a = input.get(i0).copied().unwrap_or(0.0);
        let b = input.get(i0 + 1).copied().unwrap_or(a);
        *sample = a + (b - a) * frac;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::musical_time::config::{AnalysisMode, Config};

    #[test]
    fn does_not_mutate_input_and_removes_dc() {
        let cfg = Config::for_mode(AnalysisMode::Fast);
        let input = vec![0.4_f32; 4000];
        let original = input.clone();
        let signal = prepare(&input, 22_050, &cfg);
        assert_eq!(input, original);
        let mean = super::super::util::mean(&signal.samples).abs();
        assert!(mean < 1e-3, "mean {mean}");
        assert!(signal.silent || signal.rms > 0.0);
    }

    #[test]
    fn silence_is_flagged() {
        let cfg = Config::for_mode(AnalysisMode::Fast);
        let signal = prepare(&[0.0; 2000], 44_100, &cfg);
        assert!(signal.silent);
    }

    #[test]
    fn resample_keeps_duration() {
        let cfg = Config::for_mode(AnalysisMode::Deep);
        let n = 44_100;
        let input = vec![0.0_f32; n];
        let signal = prepare(&input, 44_100, &cfg);
        let dur = signal.samples.len() as f32 / signal.sample_rate as f32;
        assert!(
            (dur - 1.0).abs() < 0.01,
            "dur {dur} rate {}",
            signal.sample_rate
        );
        assert_eq!(signal.sample_rate, 22_050);
    }
}
