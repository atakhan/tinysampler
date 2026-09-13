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
