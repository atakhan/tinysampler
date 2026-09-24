//! Glue: features → hypotheses → beat grid → musical-time description.

use super::beat::{self, GridScore};
use super::config::{AnalysisMode, Config};
use super::meter;
use super::neural::NeuralBeatEstimator;
use super::onset::{self, OnsetExtraction};
use super::periodicity::{self, Periodicity};
use super::preprocess;
use super::types::*;
use super::util;
use super::warp;

pub fn analyze(samples: &[f32], sample_rate: u32, mode: AnalysisMode) -> MusicalTimeAnalysis {
    analyze_with_estimator(samples, sample_rate, mode, None)
}

pub fn analyze_with_estimator(
    samples: &[f32],
    sample_rate: u32,
    mode: AnalysisMode,
    estimator: Option<&dyn NeuralBeatEstimator>,
) -> MusicalTimeAnalysis {
    let cfg = Config::for_mode(mode);
    let signal = preprocess::prepare(samples, sample_rate, &cfg);
    if signal.silent {
        return blank(
            signal.duration_secs,
            mode,
            UnknownReason::Silent,
            "rms below the silence floor",
        );
    }
    if signal.duration_secs < cfg.min_duration_secs {
        return blank(
            signal.duration_secs,
            mode,
            UnknownReason::InsufficientDuration,
            format!(
                "duration {:.2}s is shorter than {:.2}s",
                signal.duration_secs, cfg.min_duration_secs
            ),
        );
    }

    let onset = onset::extract(&signal.samples, signal.sample_rate, &cfg);
    if onset.envelope.len() < 12 {
        return blank(
            signal.duration_secs,
            mode,
            UnknownReason::InsufficientDuration,
            "not enough analysis frames",
        );
    }

    let predicted = estimator.map(|est| est.predict(onset.frame_rate, onset.envelope.len()));
    let neural = predicted
        .as_ref()
        .and_then(|pred| (pred.beat.len() == onset.envelope.len()).then_some(pred.beat.as_slice()));
    let evidence = beat::mix_evidence(&onset.envelope, neural);

    let (periodicity, windows) = periodicity::analyze_envelope(&evidence, onset.frame_rate, &cfg);
    let local_tempo = local_tempos(&windows, &onset, signal.duration_secs);
    let peak_count = count_pulses(&evidence, onset.frame_rate, cfg.max_bpm);

    if peak_count < 3 {
        return finish_unknown(
            signal.duration_secs,
            mode,
            UnknownReason::InsufficientPeriodicity,
            &onset,
            &evidence,
            &periodicity,
            local_tempo,
            periodicity.rhythmicity,
            "fewer than three rhythmic events",
        );
    }
    if periodicity.rhythmicity < cfg.rhythmicity_min {
        return finish_unknown(
            signal.duration_secs,
            mode,
            UnknownReason::LowRhythmicity,
            &onset,
            &evidence,
            &periodicity,
            local_tempo,
            periodicity.rhythmicity,
            format!(
                "rhythmicity {:.2} below {:.2}",
                periodicity.rhythmicity, cfg.rhythmicity_min
            ),
        );
    }

    let peaks = periodicity::with_octaves(&periodicity.peaks, &periodicity, &cfg);
    if peaks.is_empty() || peaks[0].score < cfg.periodicity_min {
        return finish_unknown(
            signal.duration_secs,
            mode,
            UnknownReason::InsufficientPeriodicity,
            &onset,
            &evidence,
            &periodicity,
            local_tempo,
            periodicity.rhythmicity,
            "no periodicity peak inside the tempo range",
        );
    }

    let mut grids: Vec<GridScore> = peaks
        .iter()
        .filter_map(|peak| {
            beat::score_period(
                &evidence,
                &onset.low,
                peak.bpm,
                onset.frame_rate,
                peak.score,
            )
        })
        .collect();
    grids.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    dedupe_grids(&mut grids);

    let Some(best) = grids.first().cloned() else {
        return finish_unknown(
            signal.duration_secs,
            mode,
            UnknownReason::InsufficientPeriodicity,
            &onset,
            &evidence,
            &periodicity,
            local_tempo,
            periodicity.rhythmicity,
            "tempo hypotheses did not produce a beat grid",
        );
    };
    if best.score < cfg.beat_score_min {
        return finish_unknown(
            signal.duration_secs,
            mode,
            UnknownReason::InsufficientPeriodicity,
            &onset,
            &evidence,
            &periodicity,
            local_tempo,
            periodicity.rhythmicity,
            format!(
                "best beat score {:.2} below {:.2}",
                best.score, cfg.beat_score_min
            ),
        );
    }

    let second = grids.get(1).cloned();
    let gap = match &second {
        Some(other) => ((best.score - other.score) / best.score.max(1e-3)).clamp(0.0, 1.0),
        None => 1.0,
    };
    let octave = second
        .as_ref()
        .is_some_and(|other| util::octave_related(best.bpm, other.bpm));
    let ambiguous = second.is_some() && !octave && gap < 0.01 && best.score > 0.5;

    let tracked = track_winner(&evidence, &onset, &best, cfg.tightness);
    let (bpm, stability, curve) = tempo_from_beats(&tracked, best.bpm);
    let beats = beats_from(&tracked, &onset.envelope, &onset);
    let candidates = candidate_list(&grids);

    let separation = if octave { 0.55 + 0.45 * gap } else { gap };
    let confidence = confidence_set(
        periodicity.rhythmicity,
        &best,
        separation,
        stability,
        signal.duration_secs,
        beats.len(),
    );

    let (meter_est, downbeats, bars) = if beats.len() >= 8 {
        let onset_env = onset.envelope.clone();
        let low = onset.low.clone();
        let hop = onset.hop_secs;
        let offset = onset.time_offset;
        meter::estimate(&beats, &onset_env, &low, |secs| {
            if hop <= 0.0 {
                0.0
            } else {
                (secs - offset) / hop
            }
        })
    } else {
        (MeterEstimate::default(), Vec::new(), Vec::new())
    };
    let mut confidence = confidence;
    confidence.downbeat = meter_est.confidence;
    confidence.meter = meter_est.confidence;

    let reason = if ambiguous {
        Some(UnknownReason::AmbiguousTempo)
    } else {
        None
    };
    let chosen = if ambiguous { None } else { Some(bpm) };
    let alternatives = alternatives_of(&candidates, chosen);
    let first = first_sounded(&beats);
    let phase = phase_of(first, chosen.or(Some(bpm)));

    let mut notes = vec![
        format!("rhythmicity {:.2}", periodicity.rhythmicity),
        format!(
            "hypothesis {:.2} bpm, beat score {:.2}",
            best.bpm, best.score
        ),
        format!("pulses {peak_count}"),
    ];
    if ambiguous {
        notes.push("top hypotheses are close and not octave-related".to_string());
    }
    if let Some(other) = &second {
        notes.push(format!(
            "runner-up {:.2} bpm score {:.2}",
            other.bpm, other.score
        ));
    }

    let mut analysis = assemble(
        signal.duration_secs,
        mode,
        &onset,
        &periodicity,
        local_tempo,
        TempoEstimate {
            bpm: chosen,
            confidence: confidence.tempo,
            alternatives,
            stability,
            first_beat_secs: first,
            phase_secs: phase,
            unknown_reason: reason,
        },
        beats,
        downbeats,
        bars,
        curve,
        confidence,
        meter_est,
        candidates,
        notes,
    );
    analysis.loops = super::loops::detect_loops(&analysis);
    analysis
}

fn track_winner(
    evidence: &[f32],
    onset: &OnsetExtraction,
    best: &GridScore,
    tightness: f32,
) -> Vec<f32> {
    let period = onset.frame_rate * 60.0 / best.bpm.max(1.0);
    let mut frames = beat::track_beats(evidence, period, tightness);
    if frames.len() < 3 {
        frames = beat::steady_grid(best.phase_frames, period, evidence.len());
    }
    frames
        .into_iter()
        .map(|frame| onset.time_of(frame))
        .filter(|t| t.is_finite() && *t >= -0.02)
        .collect()
}

fn tempo_from_beats(times: &[f32], hypothesis: f32) -> (f32, f32, Vec<TempoPoint>) {
    let intervals = beat::interval_bpms(times);
    let stability = beat::stability_of(&intervals, times.len());
    let smoothed = beat::smooth_tempo(&intervals);
    let median = if intervals.len() >= 3 {
        let mut copy = intervals.clone();
        Some(util::median(&mut copy))
    } else {
        None
    };
    let bpm = match median {
        Some(value) if hypothesis > 1.0 && (value / hypothesis).max(hypothesis / value) < 1.12 => {
            value
        }
        _ => hypothesis,
    };
    let bpm = (bpm * 10.0).round() / 10.0;
    let curve = smoothed
        .into_iter()
        .enumerate()
        .filter_map(|(i, value)| {
            times.get(i).map(|time| TempoPoint {
                time_secs: *time,
                bpm: (value * 10.0).round() / 10.0,
            })
        })
        .collect();
    (bpm, stability, curve)
}

fn beats_from(times: &[f32], envelope: &[f32], onset: &OnsetExtraction) -> Vec<Beat> {
    times
        .iter()
        .enumerate()
        .map(|(i, time)| {
            let frame = if onset.hop_secs > 0.0 {
                (time - onset.time_offset) / onset.hop_secs
            } else {
                0.0
            };
            Beat {
                time_secs: *time,
                position: i as f32,
                strength: onset::onset_near(envelope, frame, 1.5).clamp(0.0, 1.0),
            }
        })
        .collect()
}

fn first_sounded(beats: &[Beat]) -> Option<f32> {
    beats
        .iter()
        .find(|beat| beat.strength >= 0.28)
        .or_else(|| beats.first())
        .map(|beat| beat.time_secs)
}

fn phase_of(first: Option<f32>, bpm: Option<f32>) -> Option<f32> {
    let first = first?;
    let bpm = bpm.filter(|b| *b > 1.0)?;
    let period = 60.0 / bpm;
    let mut phase = first % period;
    if phase < 0.0 {
        phase += period;
    }
    Some(phase)
}

fn confidence_set(
    rhythmicity: f32,
    best: &GridScore,
    separation: f32,
    stability: f32,
    duration: f32,
    n_beats: usize,
) -> ConfidenceSet {
    let dur = (duration / 6.0).clamp(0.45, 1.0);
    let count = (n_beats as f32 / 8.0).clamp(0.5, 1.0);
    let stab = 0.72 + 0.28 * stability.clamp(0.0, 1.0);
    let sep = 0.62 + 0.38 * separation.clamp(0.0, 1.0);
    let tempo =
        (rhythmicity.clamp(0.0, 1.0) * best.score.clamp(0.0, 1.0) * sep * dur * count * stab)
            .sqrt()
            .clamp(0.0, 1.0);
    let beat_grid =
        (0.55 * best.hit_rate + 0.25 * best.coverage + 0.20 * stability).clamp(0.0, 1.0);
    ConfidenceSet {
        tempo,
        beat_grid,
        phase: best.phase_confidence.clamp(0.0, 1.0),
        downbeat: 0.0,
        meter: 0.0,
    }
}

fn candidate_list(grids: &[GridScore]) -> Vec<TempoCandidate> {
    grids
        .iter()
        .map(|grid| TempoCandidate {
            bpm: (grid.bpm * 10.0).round() / 10.0,
            confidence: grid.score,
            periodicity_score: grid.periodicity,
            onset_score: grid.align,
            beat_score: grid.score,
        })
        .collect()
}

fn alternatives_of(candidates: &[TempoCandidate], chosen: Option<f32>) -> Vec<TempoCandidate> {
    let mut out = Vec::new();
    for candidate in candidates {
        if chosen.is_some_and(|bpm| util::rel_close(bpm, candidate.bpm, 0.012)) {
            continue;
        }
        if out
            .iter()
            .any(|existing: &TempoCandidate| util::rel_close(existing.bpm, candidate.bpm, 0.02))
        {
            continue;
        }
        let octave = chosen.is_some_and(|bpm| util::octave_related(bpm, candidate.bpm));
        if chosen.is_some() && candidate.beat_score < if octave { 0.2 } else { 0.3 } {
            continue;
        }
        out.push(candidate.clone());
        if out.len() == 4 {
            break;
        }
    }
    out
}

fn dedupe_grids(grids: &mut Vec<GridScore>) {
    let sorted = std::mem::take(grids);
    for grid in sorted {
        if let Some(existing) = grids
            .iter_mut()
            .find(|g| util::rel_close(g.bpm, grid.bpm, 0.03))
        {
            if grid.score > existing.score {
                *existing = grid;
            }
        } else {
            grids.push(grid);
        }
    }
    grids.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn count_pulses(envelope: &[f32], frame_rate: f32, max_bpm: f32) -> usize {
    let min_dist = (frame_rate * 60.0 / max_bpm.max(1.0) * 0.65)
        .round()
        .max(2.0) as usize;
    onset::strong_peaks(envelope, min_dist, 0.4).len()
}

fn local_tempos(
    windows: &[periodicity::WindowPeriod],
    onset: &OnsetExtraction,
    duration: f32,
) -> Vec<LocalTempo> {
    windows
        .iter()
        .map(|window| {
            let mut end = onset.time_of(window.end_frame as f32);
            if end > duration {
                end = duration;
            }
            LocalTempo {
                start_secs: onset.time_of(window.start_frame as f32).max(0.0),
                end_secs: end.max(0.0),
                bpm: window.bpm.map(|bpm| (bpm * 10.0).round() / 10.0),
                rhythmicity: window.rhythmicity,
                confidence: window.confidence,
            }
        })
        .collect()
}

fn finish_unknown(
    duration: f32,
    mode: AnalysisMode,
    reason: UnknownReason,
    onset: &OnsetExtraction,
    _evidence: &[f32],
    periodicity: &Periodicity,
    local_tempo: Vec<LocalTempo>,
    rhythmicity: f32,
    note: impl Into<String>,
) -> MusicalTimeAnalysis {
    let candidates = periodicity
        .peaks
        .iter()
        .map(|peak| TempoCandidate {
            bpm: (peak.bpm * 10.0).round() / 10.0,
            confidence: peak.score * rhythmicity,
            periodicity_score: peak.score,
            onset_score: 0.0,
            beat_score: 0.0,
        })
        .collect();
    assemble(
        duration,
        mode,
        onset,
        periodicity,
        local_tempo,
        TempoEstimate {
            bpm: None,
            confidence: rhythmicity * 0.25,
            alternatives: Vec::new(),
            stability: 0.0,
            first_beat_secs: None,
            phase_secs: None,
            unknown_reason: Some(reason),
        },
        Vec::new(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
        ConfidenceSet {
            tempo: rhythmicity * 0.25,
            ..ConfidenceSet::default()
        },
        MeterEstimate::default(),
        candidates,
        vec![note.into()],
    )
}

fn blank(
    duration: f32,
    mode: AnalysisMode,
    reason: UnknownReason,
    note: impl Into<String>,
) -> MusicalTimeAnalysis {
    MusicalTimeAnalysis {
        duration_secs: duration,
        tempo: TempoEstimate {
            bpm: None,
            confidence: 0.0,
            alternatives: Vec::new(),
            stability: 0.0,
            first_beat_secs: None,
            phase_secs: None,
            unknown_reason: Some(reason),
        },
        beats: Vec::new(),
        downbeats: Vec::new(),
        bars: Vec::new(),
        tempo_curve: Vec::new(),
        local_tempo: Vec::new(),
        rhythmicity: 0.0,
        meter: MeterEstimate::default(),
        warp: WarpMap::default(),
        loops: Vec::new(),
        confidence: ConfidenceSet::default(),
        diagnostics: Diagnostics {
            mode,
            frame_rate: 0.0,
            onset_times: Vec::new(),
            onset_envelope: Vec::new(),
            tempogram_bpm: Vec::new(),
            tempogram: Vec::new(),
            tempo_candidates: Vec::new(),
            beat_times: Vec::new(),
            tempo_curve: Vec::new(),
            rhythmicity: 0.0,
            confidence: ConfidenceSet::default(),
            notes: vec![note.into()],
        },
    }
}

fn assemble(
    duration: f32,
    mode: AnalysisMode,
    onset: &OnsetExtraction,
    periodicity: &Periodicity,
    local_tempo: Vec<LocalTempo>,
    tempo: TempoEstimate,
    beats: Vec<Beat>,
    downbeats: Vec<Beat>,
    bars: Vec<Bar>,
    curve: Vec<TempoPoint>,
    confidence: ConfidenceSet,
    meter: MeterEstimate,
    candidates: Vec<TempoCandidate>,
    notes: Vec<String>,
) -> MusicalTimeAnalysis {
    let times = onset_times(onset);
    let (onset_times, onset_envelope) = util::decimate(&times, &onset.envelope, 1500);
    let tempogram_bpm: Vec<f32> = (0..periodicity.scores.len())
        .map(|i| periodicity.bpm_min + i as f32 * periodicity.bpm_step)
        .collect();
    let beat_times: Vec<f32> = beats.iter().map(|beat| beat.time_secs).collect();
    let warp_map = warp::from_beats(&beats);
    MusicalTimeAnalysis {
        duration_secs: duration,
        rhythmicity: periodicity.rhythmicity,
        local_tempo,
        meter,
        warp: warp_map,
        loops: Vec::new(),
        confidence,
        diagnostics: Diagnostics {
            mode,
            frame_rate: onset.frame_rate,
            onset_times,
            onset_envelope,
            tempogram_bpm,
            tempogram: periodicity.scores.clone(),
            tempo_candidates: candidates,
            beat_times,
            tempo_curve: curve.clone(),
            rhythmicity: periodicity.rhythmicity,
            confidence,
            notes,
        },
        tempo_curve: curve,
        beats,
        downbeats,
        bars,
        tempo,
    }
}

fn onset_times(onset: &OnsetExtraction) -> Vec<f32> {
    (0..onset.envelope.len())
        .map(|i| onset.time_of(i as f32))
        .collect()
}
