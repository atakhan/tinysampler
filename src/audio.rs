use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use arc_swap::ArcSwap;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::mix;
use crate::model::Project;

pub struct AudioEngine {
    #[allow(dead_code)]
    stream: cpal::Stream,
    #[allow(dead_code)]
    pub sample_rate: u32,
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

/// One default-output probe: builds [`Project`] at the device rate, starts the stream, returns handles.
pub fn open_output<F>(
    make_project: F,
    playhead_secs_bits: Arc<AtomicU32>,
    seek_pending: Arc<AtomicBool>,
    seek_target_secs_bits: Arc<AtomicU32>,
) -> Result<(AudioEngine, Arc<ArcSwap<Project>>), String>
where
    F: FnOnce(u32) -> Project,
{
    let host = cpal::default_host();
    let device = host
        .default_output_device()
        .ok_or_else(|| "no default output device".to_string())?;
    let config = device.default_output_config().map_err(|e| e.to_string())?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;

    let project = Arc::new(ArcSwap::from_pointee(make_project(sample_rate)));

    let mut playhead_secs: f32 = 0.0;
    let mut last_stop_generation: u64 = 0;

    let project_for_cb = Arc::clone(&project);
    let stream = device
        .build_output_stream(
            &config.into(),
            move |data: &mut [f32], _| {
                let proj = project_for_cb.load();
                let proj = &*proj;
                if proj.transport.stop_generation != last_stop_generation {
                    playhead_secs = 0.0;
                    last_stop_generation = proj.transport.stop_generation;
                }

                if seek_pending.swap(false, Ordering::AcqRel) {
                    playhead_secs = f32::from_bits(seek_target_secs_bits.load(Ordering::Relaxed));
                    playhead_secs_bits.store(playhead_secs.to_bits(), Ordering::Relaxed);
                }

                let rate = sample_rate as f32;
                let n = data.len() / channels;

                if !proj.transport.is_playing {
                    for o in data.iter_mut() {
                        *o = 0.0;
                    }
                    playhead_secs_bits.store(playhead_secs.to_bits(), Ordering::Relaxed);
                    return;
                }

                let base = playhead_secs;
                for i in 0..n {
                    let t = base + i as f32 / rate;
                    let v = mix::mix_mono_sample_at(proj, t, sample_rate);
                    let frame = i * channels;
                    for c in 0..channels {
                        data[frame + c] = v;
                    }
                }

                playhead_secs += n as f32 / rate;
                playhead_secs_bits.store(playhead_secs.to_bits(), Ordering::Relaxed);
            },
            |e| eprintln!("stream error: {e}"),
            None,
        )
        .map_err(|e| e.to_string())?;

    stream.play().map_err(|e| e.to_string())?;
    let engine = AudioEngine {
        stream,
        sample_rate,
    };
    Ok((engine, project))
}
