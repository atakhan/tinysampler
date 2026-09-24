//! In-place radix-2 complex FFT. No extra DSP crate: the project does not already have one.

use std::f32::consts::PI;

/// Forward FFT. `re` and `im` must have the same power-of-two length.
pub fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    debug_assert!(n.is_power_of_two() && n == im.len() && n > 0);
    bit_reverse(re, im);
    let mut len = 2;
    while len <= n {
        let ang = -2.0 * PI / len as f32;
        let wlen_re = ang.cos();
        let wlen_im = ang.sin();
        let half = len / 2;
        let mut start = 0;
        while start < n {
            let mut w_re = 1.0;
            let mut w_im = 0.0;
            for k in 0..half {
                let i0 = start + k;
                let i1 = i0 + half;
                let v_re = re[i1] * w_re - im[i1] * w_im;
                let v_im = re[i1] * w_im + im[i1] * w_re;
                re[i1] = re[i0] - v_re;
                im[i1] = im[i0] - v_im;
                re[i0] += v_re;
                im[i0] += v_im;
                let next_re = w_re * wlen_re - w_im * wlen_im;
                w_im = w_re * wlen_im + w_im * wlen_re;
                w_re = next_re;
            }
            start += len;
        }
        len <<= 1;
    }
}

fn bit_reverse(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
}

pub fn next_pow2(n: usize) -> usize {
    n.max(1).next_power_of_two()
}

pub fn hann(n: usize) -> Vec<f32> {
    if n <= 1 {
        return vec![1.0; n.max(1)];
    }
    (0..n)
        .map(|i| {
            let x = i as f32 / (n as f32 - 1.0);
            0.5 - 0.5 * (2.0 * PI * x).cos()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn impulse_is_flat() {
        let mut re = [1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0];
        let mut im = [0.0; 8];
        fft(&mut re, &mut im);
        for (r, i) in re.iter().zip(im.iter()) {
            assert!((r - 1.0).abs() < 1e-5, "re {r}");
            assert!(i.abs() < 1e-5, "im {i}");
        }
    }

    #[test]
    fn sine_lands_in_its_bin() {
        let n = 64;
        let k = 5;
        let mut re = vec![0.0; n];
        let mut im = vec![0.0; n];
        for i in 0..n {
            re[i] = (2.0 * PI * k as f32 * i as f32 / n as f32).cos();
        }
        fft(&mut re, &mut im);
        let mag = |bin: usize| (re[bin] * re[bin] + im[bin] * im[bin]).sqrt();
        let peak = mag(k);
        assert!(
            peak > mag(k + 1) * 20.0,
            "peak {} neighbor {}",
            peak,
            mag(k + 1)
        );
        assert!(peak > 20.0, "peak {peak}");
    }
}
