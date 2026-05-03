//! STFT spectrogram for timeline preview (UI thread only).

use std::f32::consts::TAU;

use num_complex::Complex;
use rustfft::FftPlanner;

use crate::model::Spectrogram;

const N_FFT: usize = 512;

/// Time on X, frequency on Y (low frequencies at the bottom of the image).
pub fn compute_mono(samples: &[f32], _sample_rate: u32, out_w: usize, out_h: usize) -> Spectrogram {
    let cols = out_w.clamp(1, 512);
    let rows = out_h.clamp(1, 256);
    let n_bins = N_FFT / 2 + 1;
    let n = samples.len();

    let mut hann = [0f32; N_FFT];
    for i in 0..N_FFT {
        let denom = (N_FFT - 1).max(1) as f32;
        hann[i] = 0.5 * (1.0 - (TAU * i as f32 / denom).cos());
    }

    let mut planner = FftPlanner::new();
    let fft = planner.plan_fft_forward(N_FFT);

    let mut grid = vec![1e-12f32; cols * n_bins];
    let max_start = n.saturating_sub(N_FFT);

    for c in 0..cols {
        let start = if cols <= 1 || max_start == 0 {
            0
        } else {
            (c * max_start) / (cols - 1)
        };

        let mut buf: Vec<Complex<f32>> = (0..N_FFT)
            .map(|i| {
                let v = samples.get(start + i).copied().unwrap_or(0.0) * hann[i];
                Complex::new(v, 0.0)
            })
            .collect();

        fft.process(&mut buf);
        for b in 0..n_bins {
            let v = buf[b];
            grid[c * n_bins + b] = (v.re * v.re + v.im * v.im).sqrt() + 1e-12;
        }
    }

    let mut folded = vec![1e-12f32; cols * rows];
    let mut vmax = 1e-12f32;
    for r in 0..rows {
        let br = rows - 1 - r;
        let bin_lo = (br * n_bins) / rows;
        let bin_hi = (((br + 1) * n_bins) / rows).max(bin_lo + 1).min(n_bins);
        for c in 0..cols {
            let mut mx = 1e-12f32;
            for b in bin_lo..bin_hi {
                mx = mx.max(grid[c * n_bins + b]);
            }
            folded[r * cols + c] = mx;
            vmax = vmax.max(mx);
        }
    }

    let vmax_log = vmax.log10();
    let vmin_log = (vmax * 1e-4).max(1e-12).log10();

    let mut rgba = vec![0u8; cols * rows * 4];
    for r in 0..rows {
        for c in 0..cols {
            let v = folded[r * cols + c];
            let logv = v.log10().clamp(vmin_log, vmax_log);
            let t = if (vmax_log - vmin_log).abs() < 1e-6 {
                0.5
            } else {
                (logv - vmin_log) / (vmax_log - vmin_log)
            };

            let (cr, cg, cb) = magma(t);
            let i = (r * cols + c) * 4;
            rgba[i] = cr;
            rgba[i + 1] = cg;
            rgba[i + 2] = cb;
            rgba[i + 3] = 255;
        }
    }

    Spectrogram {
        width: cols,
        height: rows,
        rgba,
    }
}

/// Approximate matplotlib "magma" (dark → warm).
fn magma(t: f32) -> (u8, u8, u8) {
    let t = t.clamp(0.0, 1.0);
    let c0 = [12.0, 4.0, 32.0];
    let c1 = [80.0, 20.0, 110.0];
    let c2 = [180.0, 50.0, 120.0];
    let c3 = [252.0, 220.0, 95.0];
    let (a, b, u) = if t < 1.0 / 3.0 {
        let u = t * 3.0;
        (c0, c1, u)
    } else if t < 2.0 / 3.0 {
        let u = (t - 1.0 / 3.0) * 3.0;
        (c1, c2, u)
    } else {
        let u = (t - 2.0 / 3.0) * 3.0;
        (c2, c3, u)
    };
    let lerp = |i: usize| (a[i] + (b[i] - a[i]) * u).clamp(0.0, 255.0) as u8;
    (lerp(0), lerp(1), lerp(2))
}
