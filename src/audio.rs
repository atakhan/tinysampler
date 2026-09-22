use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mix;
use crate::model::Project;

pub struct AudioEngine {
    stream: Option<cpal::Stream>,
    #[allow(dead_code)]
    pub sample_rate: u32,
    device_name: String,
    stream_failed: Arc<AtomicBool>,
    playhead_secs_bits: Arc<AtomicU32>,
    seek_pending: Arc<AtomicBool>,
    seek_target_secs_bits: Arc<AtomicU32>,
    last_rebuild_attempt: Option<Instant>,
    preview_secs_bits: Arc<AtomicU32>,
}

/// Runs a short 440 Hz sine on the default output to validate the pipeline (step 0).
pub fn play_test_tone_blocking(duration_secs: f32) -> Result<(), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let sr = config.sample_rate().0 as f32;
    let channels = config.channels() as usize;

    let mut phase: f32 = 0.0;

    let stream = device
        .build_output_stream(
            &config.into(),
            move |data: &mut [f32], _| {
                let two_pi = std::f32::consts::TAU;
                for frame in data.chunks_mut(channels) {
                    phase += two_pi * 440.0 / sr;
                    if phase > two_pi {
                        phase -= two_pi;
                    }
                    let s = phase.sin() * 0.2;
                    for o in frame.iter_mut() {
                        *o = s;
                    }
                }
            },
            |e| eprintln!("stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    std::thread::sleep(std::time::Duration::from_secs_f32(duration_secs));
    drop(stream);
    Ok(())
}

fn default_output_device_name() -> Option<String> {
    cpal::default_host()
        .default_output_device()
        .and_then(|d| d.name().ok())
}

fn start_stream(
    project: Arc<ArcSwap<Project>>,
    playhead_secs_bits: Arc<AtomicU32>,
    seek_pending: Arc<AtomicBool>,
    seek_target_secs_bits: Arc<AtomicU32>,
    preview_secs_bits: Arc<AtomicU32>,
    stream_failed: Arc<AtomicBool>,
) -> Result<(cpal::Stream, u32, String), String> {
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "нет устройства вывода".to_string())?;
    let device_name = device.name().unwrap_or_else(|_| "output".into());
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let mut playhead_secs = f32::from_bits(playhead_secs_bits.load(Ordering::Relaxed));
    if !playhead_secs.is_finite() {
        playhead_secs = 0.0;
    }
    let mut last_stop_generation = project.load().transport.stop_generation;
    let mut preview_secs = f32::from_bits(preview_secs_bits.load(Ordering::Relaxed));
    if !preview_secs.is_finite() {
        preview_secs = 0.0;
    }
    let mut last_preview_generation = project.load().sampler_preview.generation;
    let mut last_audition_generation = project.load().audition.generation;

    let err_flag = Arc::clone(&stream_failed);
    let stream = device
        .build_output_stream(
            &config.into(),
            move |data: &mut [f32], _| {
                let proj = project.load();
                let proj = &*proj;
                if proj.transport.stop_generation != last_stop_generation {
                    playhead_secs = proj.transport.stop_return_secs;
                    if !playhead_secs.is_finite() {
                        playhead_secs = 0.0;
                    }
                    playhead_secs = playhead_secs.max(0.0);
                    last_stop_generation = proj.transport.stop_generation;
                }
                if proj.audition.generation != last_audition_generation {
                    preview_secs = 0.0;
                    last_audition_generation = proj.audition.generation;
                }
                if proj.sampler_preview.generation != last_preview_generation {
                    preview_secs = proj.sampler_preview.start_secs.max(0.0);
                    if !preview_secs.is_finite() {
                        preview_secs = 0.0;
                    }
                    last_preview_generation = proj.sampler_preview.generation;
                }

                if seek_pending.swap(false, Ordering::AcqRel) {
                    playhead_secs = f32::from_bits(seek_target_secs_bits.load(Ordering::Relaxed));
                    if !playhead_secs.is_finite() {
                        playhead_secs = 0.0;
                    }
                    playhead_secs_bits.store(playhead_secs.to_bits(), Ordering::Relaxed);
                }

                let rate = sample_rate as f32;
                let n = data.len() / channels;
                let mix_audition = proj.audition.playing && proj.audition.sample.is_some();
                let mix_preview = proj.sampler_preview.playing && !mix_audition;
                let mix_song = proj.transport.is_playing && !mix_audition;

                if !mix_preview && !mix_song && !mix_audition {
                    for o in data.iter_mut() {
                        *o = 0.0;
                    }
                    playhead_secs_bits.store(playhead_secs.to_bits(), Ordering::Relaxed);
                    // Sampler cursor is owned by the UI while preview is stopped.
                    preview_secs = f32::from_bits(preview_secs_bits.load(Ordering::Relaxed));
                    if !preview_secs.is_finite() {
                        preview_secs = 0.0;
                    }
                    return;
                }

                let preview_base = preview_secs;
                let song_base = playhead_secs;
                for i in 0..n {
                    let mut v = 0.0f32;
                    if mix_audition {
                        if let Some(sample) = proj.audition.sample.as_ref() {
                            v += mix::mix_file_audition_at(
                                sample,
                                preview_base + i as f32 / rate,
                                proj.audition.end_secs,
                            );
                        }
                    }
                    if mix_preview {
                        v += mix::mix_sampler_preview_at(proj, preview_base + i as f32 / rate);
                    }
                    if mix_song {
                        v += mix::mix_mono_sample_at(proj, song_base + i as f32 / rate);
                    }
                    v = v.clamp(-1.0, 1.0);
                    let frame = i * channels;
                    for c in 0..channels {
                        data[frame + c] = v;
                    }
                }

                if mix_preview || mix_audition {
                    preview_secs += n as f32 / rate;
                    preview_secs_bits.store(preview_secs.to_bits(), Ordering::Relaxed);
                } else {
                    preview_secs = f32::from_bits(preview_secs_bits.load(Ordering::Relaxed));
                    if !preview_secs.is_finite() {
                        preview_secs = 0.0;
                    }
                }
                if mix_song {
                    playhead_secs += n as f32 / rate;
                }
                playhead_secs_bits.store(playhead_secs.to_bits(), Ordering::Relaxed);
            },
            move |e| {
                eprintln!("stream error: {e}");
                err_flag.store(true, Ordering::Release);
            },
            None,
        )
        .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, sample_rate, device_name))
}

/// Starts the default-output stream. Document PCM keeps its own sample rate;
/// the stream rate only advances wall-clock time in the callback.
pub fn open_output(
    playhead_secs_bits: Arc<AtomicU32>,
    seek_pending: Arc<AtomicBool>,
    seek_target_secs_bits: Arc<AtomicU32>,
) -> Result<(AudioEngine, Arc<ArcSwap<Project>>), String> {
    let project = Arc::new(ArcSwap::from_pointee(Project::empty()));
    let stream_failed = Arc::new(AtomicBool::new(false));
    let preview_secs_bits = Arc::new(AtomicU32::new(0.0f32.to_bits()));
    let (stream, out_rate, device_name) = start_stream(
        Arc::clone(&project),
        Arc::clone(&playhead_secs_bits),
        Arc::clone(&seek_pending),
        Arc::clone(&seek_target_secs_bits),
        Arc::clone(&preview_secs_bits),
        Arc::clone(&stream_failed),
    )?;

    let engine = AudioEngine {
        stream: Some(stream),
        sample_rate: out_rate,
        device_name,
        stream_failed,
        playhead_secs_bits,
        seek_pending,
        seek_target_secs_bits,
        last_rebuild_attempt: None,
        preview_secs_bits,
    };
    Ok((engine, project))
}

impl AudioEngine {
    pub fn preview_secs(&self) -> f32 {
        f32::from_bits(self.preview_secs_bits.load(Ordering::Relaxed))
    }

    pub fn reset_preview_secs(&self) {
        self.seek_preview_secs(0.0);
    }

    pub fn seek_preview_secs(&self, secs: f32) {
        let secs = if secs.is_finite() { secs.max(0.0) } else { 0.0 };
        self.preview_secs_bits.store(secs.to_bits(), Ordering::Relaxed);
    }

    /// Recreate the cpal stream when the default device changes or the old stream dies
    /// (Bluetooth unplug, speakers selected, etc.).
    pub fn recover_if_needed(&mut self, project: &Arc<ArcSwap<Project>>) -> Option<String> {
        let current_name = default_output_device_name();
        let device_changed = current_name
            .as_ref()
            .is_some_and(|n| n != &self.device_name)
            || (current_name.is_none() && self.stream.is_some());
        let failed = self.stream_failed.swap(false, Ordering::AcqRel);
        let dead = self.stream.is_none();
        if !failed && !device_changed && !dead {
            return None;
        }

        if let Some(t0) = self.last_rebuild_attempt {
            if t0.elapsed() < Duration::from_millis(300) {
                if failed {
                    self.stream_failed.store(true, Ordering::Release);
                }
                return None;
            }
        }

        self.last_rebuild_attempt = Some(Instant::now());
        self.stream = None;

        match start_stream(
            Arc::clone(project),
            Arc::clone(&self.playhead_secs_bits),
            Arc::clone(&self.seek_pending),
            Arc::clone(&self.seek_target_secs_bits),
            Arc::clone(&self.preview_secs_bits),
            Arc::clone(&self.stream_failed),
        ) {
            Ok((stream, sample_rate, device_name)) => {
                let switched = device_name != self.device_name;
                self.stream = Some(stream);
                self.sample_rate = sample_rate;
                self.device_name = device_name.clone();
                if switched || failed {
                    Some(format!("Вывод: {device_name}"))
                } else {
                    None
                }
            }
            Err(e) => Some(e),
        }
    }
}
