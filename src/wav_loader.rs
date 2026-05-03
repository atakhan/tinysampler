use hound::{SampleFormat, WavSpec};
use std::sync::Arc;

use crate::model::Sample;
use crate::spectrogram;

/// Load WAV as mono f32 in [-1, 1]. Stereo → left channel only.
/// Then resample to `target_rate` if needed (UI thread only).
pub fn load_wav_mono_f32(path: &std::path::Path, target_rate: u32) -> Result<Sample, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err("WAV has zero channels".into());
    }

    let mono = read_mono_left(&mut reader, spec)?;
    if mono.is_empty() {
        return Err("WAV has zero samples".into());
    }
    for &s in &mono {
        if !(-1.1..=1.1).contains(&s) {
            return Err(format!("sample out of expected [-1,1] range: {s}"));
        }
    }

    let data = if spec.sample_rate == target_rate {
        mono
    } else {
        resample_linear(&mono, spec.sample_rate, target_rate)
    };

    let spec_img = Arc::new(spectrogram::compute_mono(&data, target_rate, 320, 160));
    Ok(Sample::new_mono(Arc::new(data), Some(spec_img)))
}

fn read_mono_left<R: std::io::Read>(
    reader: &mut hound::WavReader<R>,
    spec: WavSpec,
) -> Result<Vec<f32>, String> {
    let ch = spec.channels as usize;
    match spec.sample_format {
        SampleFormat::Float => read_mono_float(reader, ch),
        SampleFormat::Int => read_mono_int(reader, ch, spec.bits_per_sample),
    }
}

fn read_mono_float<R: std::io::Read>(
    reader: &mut hound::WavReader<R>,
    ch: usize,
) -> Result<Vec<f32>, String> {
    let mut mono = Vec::new();
    let mut it = reader.samples::<f32>();
    loop {
        let mut frame = Vec::with_capacity(ch);
        for _ in 0..ch {
            match it.next() {
                None => {
                    if frame.is_empty() {
                        return Ok(mono);
                    }
                    return Err("truncated WAV frame (float)".into());
                }
                Some(s) => frame.push(s.map_err(|e| e.to_string())?),
            }
        }
        mono.push(frame[0]);
    }
}

fn read_mono_int<R: std::io::Read>(
    reader: &mut hound::WavReader<R>,
    ch: usize,
    bits: u16,
) -> Result<Vec<f32>, String> {
    let mut mono = Vec::new();
    let mut it = reader.samples::<i32>();
    loop {
        let mut frame = Vec::with_capacity(ch);
        for _ in 0..ch {
            match it.next() {
                None => {
                    if frame.is_empty() {
                        return Ok(mono);
                    }
                    return Err("truncated WAV frame (int)".into());
                }
                Some(s) => {
                    let v = s.map_err(|e| e.to_string())?;
                    frame.push(int_to_f32(v, bits));
                }
            }
        }
        mono.push(frame[0]);
    }
}

fn int_to_f32(v: i32, bits: u16) -> f32 {
    if bits == 0 || bits > 32 {
        return 0.0;
    }
    let max = ((1i64 << (bits as i64 - 1)) - 1) as f32;
    (v as f32 / max).clamp(-1.0, 1.0)
}

fn resample_linear(src: &[f32], src_rate: u32, dst_rate: u32) -> Vec<f32> {
    if src_rate == 0 || dst_rate == 0 {
        return src.to_vec();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((src.len() as f64) / ratio).max(0.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let j = src_pos.floor() as usize;
        let frac = (src_pos - j as f64) as f32;
        let a = src.get(j).copied().unwrap_or(0.0);
        let b = src.get(j.saturating_add(1)).copied().unwrap_or(a);
        out.push(a * (1.0 - frac) + b * frac);
    }
    out
}
