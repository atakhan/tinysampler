use hound::{SampleFormat, WavSpec};
use std::io::{Cursor, Read, Seek};
use std::path::Path;
use std::sync::Arc;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSource, MediaSourceStream};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// PCM frames (~1 hour at 48 kHz). Also stops a stuck MP3 demuxer from growing forever.
const MAX_MONO_SAMPLES: usize = 48_000 * 60 * 60;

struct InMemoryMedia {
    inner: Cursor<Vec<u8>>,
}

impl Read for InMemoryMedia {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for InMemoryMedia {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        self.inner.seek(pos)
    }
}

impl MediaSource for InMemoryMedia {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        Some(self.inner.get_ref().len() as u64)
    }
}

use crate::model::Sample;
use crate::waveform;

/// Load WAV or MP3 as mono f32 in [-1, 1]. Stereo → left channel only.
/// Then resample to `target_rate` if needed (UI thread only).
pub fn load_audio_mono_f32(path: &Path, target_rate: u32) -> Result<Sample, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "wav" => load_wav_mono_f32(path, target_rate),
        "mp3" => load_mp3_mono_f32(path, target_rate),
        other => Err(format!(
            "unsupported audio format: .{other} (use WAV or MP3)"
        )),
    }
}

/// Load WAV as mono f32 in [-1, 1]. Stereo → left channel only.
/// Then resample to `target_rate` if needed (UI thread only).
pub fn load_wav_mono_f32(path: &Path, target_rate: u32) -> Result<Sample, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let channels = spec.channels as usize;
    if channels == 0 {
        return Err("WAV has zero channels".into());
    }

    let mono = read_mono_left(&mut reader, spec)?;
    finalize_mono_sample(mono, spec.sample_rate, target_rate, "WAV")
}

fn load_mp3_mono_f32(path: &Path, target_rate: u32) -> Result<Sample, String> {
    let (mono, sample_rate) = decode_mp3_mono_f32(path)?;
    finalize_mono_sample(mono, sample_rate, target_rate, "MP3")
}

fn finalize_mono_sample(
    mono: Vec<f32>,
    src_rate: u32,
    target_rate: u32,
    kind: &str,
) -> Result<Sample, String> {
    if mono.is_empty() {
        return Err(format!("{kind} has zero samples"));
    }

    let data = if src_rate == target_rate {
        mono
    } else {
        resample_linear(&mono, src_rate, target_rate)
    };

    let peaks = waveform::PeakPyramid::build(&data);
    Ok(Sample::new_mono(Arc::new(data), Arc::new(peaks)))
}

fn decode_mp3_mono_f32(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    if bytes.is_empty() {
        return Err("MP3 file is empty".into());
    }
    let file_len = bytes.len() as u64;
    let mss = MediaSourceStream::new(
        Box::new(InMemoryMedia {
            inner: Cursor::new(bytes),
        }),
        Default::default(),
    );
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions {
                enable_gapless: false,
                prebuild_seek_index: false,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| format!("MP3 probe failed: {e}"))?;

    let mut format = probed.format;
    let (track_id, n_frames, codec_params) = {
        let track = format
            .tracks()
            .iter()
            .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| "MP3 has no supported audio track".to_string())?;
        (track.id, track.codec_params.n_frames, track.codec_params.clone())
    };
    if n_frames == Some(0) {
        return Err("MP3 has zero frames".into());
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&codec_params, &DecoderOptions::default())
        .map_err(|e| format!("MP3 decoder init failed: {e}"))?;

    let mut mono = Vec::new();
    if let Some(n) = n_frames {
        mono.reserve((n as usize).min(MAX_MONO_SAMPLES));
    } else {
        // ~1 PCM sample per compressed byte is a loose upper bound for typical MP3.
        mono.reserve((file_len as usize).min(MAX_MONO_SAMPLES));
    }
    let mut sample_rate = 0u32;
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    let mut buf_cap = 0u64;
    let mut consecutive_decode_errs = 0u32;
    // MPEG frames are tens of bytes; file_len as a packet cap was ~2e6 iterations on a 2 MB file.
    let max_packets = (file_len / 24).saturating_add(2048);

    for _ in 0..max_packets {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(SymphoniaError::ResetRequired) => break,
            Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::DecodeError(_)) => break,
            Err(_) => break,
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => {
                consecutive_decode_errs = 0;
                decoded
            }
            Err(SymphoniaError::DecodeError(_)) => {
                consecutive_decode_errs += 1;
                if consecutive_decode_errs >= 8 {
                    break;
                }
                continue;
            }
            Err(_) => break,
        };

        let spec = *decoded.spec();
        sample_rate = spec.rate;
        let channels = spec.channels.count();
        if channels == 0 {
            return Err("MP3 has zero channels".into());
        }

        let cap = (decoded.capacity() as u64).max(1);
        if sample_buf.is_none() || cap > buf_cap {
            sample_buf = Some(SampleBuffer::<f32>::new(cap, spec));
            buf_cap = cap;
        }
        let buf = sample_buf.as_mut().unwrap();
        buf.copy_planar_ref(decoded);
        let samples = buf.samples();
        let frames = samples.len() / channels.max(1);
        if frames > 0 {
            mono.extend_from_slice(&samples[..frames]);
        }
        if mono.len() > MAX_MONO_SAMPLES {
            return Err("MP3 is too long (limit is 1 hour)".into());
        }
    }

    if sample_rate == 0 {
        return Err("MP3 has unknown sample rate".into());
    }
    Ok((mono, sample_rate))
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
    if src.is_empty() || src_rate == 0 || dst_rate == 0 || src_rate == dst_rate {
        return src.to_vec();
    }
    let ratio = src_rate as f64 / dst_rate as f64;
    let out_len = ((src.len() as f64) / ratio).max(0.0) as usize;
    let last = src.len() - 1;
    let mut out = vec![0.0f32; out_len];
    for (i, dst) in out.iter_mut().enumerate() {
        let src_pos = i as f64 * ratio;
        let j = src_pos as usize;
        if j >= last {
            *dst = src[last];
            continue;
        }
        let frac = (src_pos - j as f64) as f32;
        let a = src[j];
        *dst = a + (src[j + 1] - a) * frac;
    }
    out
}
