//! Small numeric helpers shared by the pipeline stages.

pub fn median(xs: &mut [f32]) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = xs.len() / 2;
    if xs.len() % 2 == 0 {
        0.5 * (xs[mid - 1] + xs[mid])
    } else {
        xs[mid]
    }
}

pub fn percentile(xs: &[f32], q: f32) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    let mut v = xs.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = q.clamp(0.0, 1.0);
    let idx = ((v.len() - 1) as f32 * q).round() as usize;
    v[idx.min(v.len() - 1)]
}

pub fn moving_mean(x: &[f32], win: usize) -> Vec<f32> {
    if x.is_empty() {
        return Vec::new();
    }
    let w = win.max(1);
    let mut c = vec![0.0; x.len() + 1];
    for (i, s) in x.iter().enumerate() {
        c[i + 1] = c[i] + *s;
    }
    let half = w / 2;
    let mut out = vec![0.0; x.len()];
    for i in 0..x.len() {
        let a = i.saturating_sub(half);
        let b = (i + half + 1).min(x.len());
        out[i] = (c[b] - c[a]) / (b - a) as f32;
    }
    out
}

pub fn unit_max(x: &mut [f32]) {
    let m = x.iter().copied().fold(0.0_f32, f32::max);
    if m <= 1e-8 {
        return;
    }
    for s in x {
        *s /= m;
    }
}

pub fn parabolic(y0: f32, y1: f32, y2: f32) -> f32 {
    let denom = y0 - 2.0 * y1 + y2;
    if denom.abs() < 1e-9 {
        return 0.0;
    }
    (0.5 * (y0 - y2) / denom).clamp(-0.5, 0.5)
}

pub fn rel_close(a: f32, b: f32, tol: f32) -> bool {
    let scale = a.abs().max(b.abs()).max(1e-6);
    (a - b).abs() / scale < tol
}

pub fn octave_related(a: f32, b: f32) -> bool {
    if a <= 1.0 || b <= 1.0 {
        return false;
    }
    let log2 = (a.max(b) / a.min(b)).log2();
    let nearest = log2.round();
    nearest >= 1.0 && (log2 - nearest).abs() < 0.07
}

/// Soft preference for a tactus near 124 BPM. Used only to break near-ties.
pub fn tempo_prior(bpm: f32) -> f32 {
    let z = bpm.max(1.0).log2() - 124.0_f32.log2();
    (-0.5 * (z / 1.15).powi(2)).exp()
}

pub fn decimate(times: &[f32], values: &[f32], max_points: usize) -> (Vec<f32>, Vec<f32>) {
    let n = times.len().min(values.len());
    if n == 0 {
        return (Vec::new(), Vec::new());
    }
    if n <= max_points || max_points == 0 {
        return (times[..n].to_vec(), values[..n].to_vec());
    }
    let step = n as f32 / max_points as f32;
    let mut ts = Vec::with_capacity(max_points);
    let mut vs = Vec::with_capacity(max_points);
    let mut i = 0.0;
    while (i as usize) < n && ts.len() < max_points {
        let k = i as usize;
        ts.push(times[k]);
        vs.push(values[k]);
        i += step;
    }
    (ts, vs)
}

pub fn mean(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f32>() / xs.len() as f32
    }
}

pub fn sample_at(x: &[f32], index: f32) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    if index <= 0.0 {
        return x[0];
    }
    let last = (x.len() - 1) as f32;
    if index >= last {
        return x[x.len() - 1];
    }
    let i0 = index.floor() as usize;
    let frac = index - i0 as f32;
    x[i0] * (1.0 - frac) + x[i0 + 1] * frac
}

pub fn local_max(x: &[f32], center: f32, radius: f32) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let last = x.len() - 1;
    let a = ((center - radius).floor().max(0.0) as usize).min(last);
    let b = ((center + radius).ceil().max(0.0) as usize).min(last);
    let (a, b) = if a <= b { (a, b) } else { (b, a) };
    let mut m = 0.0_f32;
    for v in &x[a..=b] {
        m = m.max(*v);
    }
    m
}
