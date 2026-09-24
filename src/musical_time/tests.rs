//! Synthetic regression corpus. Real genre recordings are not in the repo;
//! each test builds the rhythmic situation the spec calls out.

use super::{analyze, AnalysisMode, MusicalTimeAnalysis, UnknownReason};

const SR: u32 = 22_050;

fn assert_bpm(analysis: &MusicalTimeAnalysis, expected: f32, tol: f32) {
    let got = analysis.tempo.bpm;
    assert!(
        got.is_some(),
        "expected {expected}, got unknown {:?}\n{}",
        analysis.tempo.unknown_reason,
        dump(analysis)
    );
    let got = got.unwrap();
    assert!(
        (got - expected).abs() <= tol,
        "expected {expected} ± {tol}, got {got}\n{}",
        dump(analysis)
    );
}

fn assert_unknown(analysis: &MusicalTimeAnalysis, reason: UnknownReason) {
    assert!(
        analysis.tempo.bpm.is_none(),
        "invented {} bpm\n{}",
        analysis.tempo.bpm.unwrap_or(0.0),
        dump(analysis)
    );
    assert_eq!(
        analysis.tempo.unknown_reason,
        Some(reason),
        "reason mismatch\n{}",
        dump(analysis)
    );
}

fn listed(analysis: &MusicalTimeAnalysis, bpm: f32, tol: f32) -> bool {
    analysis
        .tempo
        .bpm
        .is_some_and(|value| (value - bpm).abs() <= tol)
        || analysis
            .tempo
            .alternatives
            .iter()
            .any(|candidate| (candidate.bpm - bpm).abs() <= tol)
}

fn dump(analysis: &MusicalTimeAnalysis) -> String {
    let alts: Vec<_> = analysis
        .tempo
        .alternatives
        .iter()
        .map(|candidate| format!("{:.1}@{:.2}", candidate.bpm, candidate.beat_score))
        .collect();
    format!(
        "bpm={:?} conf={:.2} rhy={:.2} stab={:.2} reason={:?} alts={alts:?} beats={} notes={:?}",
        analysis.tempo.bpm,
        analysis.tempo.confidence,
        analysis.rhythmicity,
        analysis.tempo.stability,
        analysis.tempo.unknown_reason,
        analysis.beats.len(),
        analysis.diagnostics.notes
    )
}

fn empty(sr: u32, dur: f32) -> Vec<f32> {
    vec![0.0; (dur * sr as f32) as usize]
}

fn at(sr: u32, time: f32) -> usize {
    (time * sr as f32).round() as usize
}

fn add_tone(buf: &mut [f32], sr: u32, start: usize, freq: f32, decay: f32, amp: f32, dur: f32) {
    let n = ((dur * sr as f32) as usize).min(buf.len().saturating_sub(start));
    for i in 0..n {
        let t = i as f32 / sr as f32;
        buf[start + i] += (2.0 * std::f32::consts::PI * freq * t).sin() * (-decay * t).exp() * amp;
    }
}

fn add_hat(buf: &mut [f32], sr: u32, start: usize, amp: f32, seed: &mut u32) {
    let n = ((0.02 * sr as f32) as usize).min(buf.len().saturating_sub(start));
    for i in 0..n {
        *seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let unit = ((*seed >> 8) & 0x00ff_ffff) as f32 / 16_777_215.0;
        let t = i as f32 / sr as f32;
        buf[start + i] += (unit * 2.0 - 1.0) * (-t * 220.0).exp() * amp;
    }
}

fn metronome(sr: u32, bpm: f32, dur: f32) -> (Vec<f32>, Vec<f32>) {
    let mut audio = empty(sr, dur);
    let mut times = Vec::new();
    let step = 60.0 / bpm;
    let mut t = 0.12;
    while t < dur - 0.06 {
        let i = at(sr, t);
        if i < audio.len() {
            add_tone(&mut audio, sr, i, 72.0, 42.0, 0.95, 0.07);
            times.push(t);
        }
        t += step;
    }
    (audio, times)
}

fn beat_hit_ratio(analysis: &MusicalTimeAnalysis, times: &[f32], tol: f32) -> f32 {
    if times.is_empty() || analysis.beats.is_empty() {
        return 0.0;
    }
    let hits = times
        .iter()
        .filter(|time| {
            analysis
                .beats
                .iter()
                .any(|beat| (beat.time_secs - **time).abs() <= tol)
        })
        .count();
    hits as f32 / times.len() as f32
}

#[test]
fn straight_quarters_at_common_tempos() {
    for bpm in [72.0, 96.0, 120.0, 128.0, 140.0] {
        let (audio, times) = metronome(SR, bpm, 6.0);
        let analysis = analyze(&audio, SR, AnalysisMode::Deep);
        assert_bpm(&analysis, bpm, 1.5);
        assert!(analysis.tempo.confidence > 0.4, "{}", dump(&analysis));
        let hits = beat_hit_ratio(&analysis, &times, 0.05);
        assert!(hits >= 0.75, "hits {hits} at {bpm}\n{}", dump(&analysis));
        assert!(analysis.beats.len() >= 4, "{}", dump(&analysis));
        assert!(analysis.warp.points.len() == analysis.beats.len());
    }
}

#[test]
fn half_tempo_clicks_stay_slow() {
    let (audio, _) = metronome(SR, 60.0, 8.0);
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, 60.0, 2.0);
}

#[test]
fn double_tempo_clicks_stay_fast() {
    let (audio, _) = metronome(SR, 160.0, 6.0);
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, 160.0, 2.5);
    let (audio, _) = metronome(SR, 200.0, 6.0);
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, 200.0, 3.0);
}

#[test]
fn octave_alternative_is_kept_for_a_clear_pulse() {
    let (audio, _) = metronome(SR, 120.0, 6.0);
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, 120.0, 1.5);
    assert!(
        listed(&analysis, 60.0, 3.0) || listed(&analysis, 240.0, 4.0),
        "expected an octave alternative\n{}",
        dump(&analysis)
    );
}

#[test]
fn house_backbeat_is_the_quarter_pulse() {
    let bpm = 124.0;
    let step = 60.0 / bpm;
    let mut audio = empty(SR, 6.5);
    let mut seed = 3u32;
    let mut t = 0.1;
    let mut beat = 0;
    while t < 6.3 {
        let i = at(SR, t);
        add_tone(&mut audio, SR, i, 52.0, 28.0, 1.0, 0.09);
        if beat % 2 == 1 {
            add_tone(&mut audio, SR, i, 180.0, 60.0, 0.45, 0.05);
            add_hat(&mut audio, SR, i, 0.35, &mut seed);
        }
        add_hat(&mut audio, SR, i, 0.1, &mut seed);
        add_hat(&mut audio, SR, at(SR, t + step * 0.5), 0.07, &mut seed);
        t += step;
        beat += 1;
    }
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, bpm, 2.0);
}

#[test]
fn half_time_keeps_both_octaves() {
    let fast = 140.0;
    let step = 60.0 / fast;
    let mut audio = empty(SR, 8.0);
    let mut seed = 11u32;
    let mut t = 0.1;
    let mut beat = 0;
    while t < 7.7 {
        let i = at(SR, t);
        if beat % 4 == 0 {
            add_tone(&mut audio, SR, i, 50.0, 24.0, 1.0, 0.12);
        }
        if beat % 4 == 2 {
            add_tone(&mut audio, SR, i, 170.0, 40.0, 0.85, 0.07);
            add_hat(&mut audio, SR, i, 0.4, &mut seed);
        }
        add_hat(&mut audio, SR, i, 0.08, &mut seed);
        t += step;
        beat += 1;
    }
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert!(
        analysis.tempo.bpm.is_some(),
        "withheld half-time\n{}",
        dump(&analysis)
    );
    assert!(
        listed(&analysis, 70.0, 4.0),
        "missing 70\n{}",
        dump(&analysis)
    );
    assert!(
        listed(&analysis, 140.0, 4.0),
        "missing 140\n{}",
        dump(&analysis)
    );
}

#[test]
fn swing_follows_the_quarter() {
    let bpm = 100.0;
    let quarter = 60.0 / bpm;
    let mut audio = empty(SR, 8.0);
    let mut seed = 5u32;
    let mut t = 0.15;
    while t < 7.6 {
        add_tone(&mut audio, SR, at(SR, t), 64.0, 36.0, 1.0, 0.08);
        add_hat(&mut audio, SR, at(SR, t + quarter * 0.66), 0.22, &mut seed);
        t += quarter;
    }
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, bpm, 3.0);
}

#[test]
fn backbeat_without_kick_still_has_a_pulse() {
    let bpm = 110.0;
    let step = 60.0 / bpm;
    let mut audio = empty(SR, 6.5);
    let mut seed = 9u32;
    let mut t = 0.12;
    let mut beat = 0;
    while t < 6.2 {
        let i = at(SR, t);
        add_hat(&mut audio, SR, i, 0.35, &mut seed);
        if beat % 2 == 1 {
            add_tone(&mut audio, SR, i, 200.0, 50.0, 0.8, 0.05);
        }
        t += step;
        beat += 1;
    }
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, bpm, 3.0);
}

#[test]
fn noise_silence_and_one_shot_are_not_given_a_tempo() {
    let mut noise = empty(SR, 4.0);
    let mut seed = 99u32;
    for sample in &mut noise {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let unit = ((seed >> 8) & 0x00ff_ffff) as f32 / 16_777_215.0;
        *sample = (unit * 2.0 - 1.0) * 0.3;
    }
    let noise_analysis = analyze(&noise, SR, AnalysisMode::Deep);
    assert!(
        noise_analysis.tempo.bpm.is_none(),
        "noise got a tempo\n{}",
        dump(&noise_analysis)
    );
    assert!(
        noise_analysis.tempo.confidence < 0.45,
        "{}",
        dump(&noise_analysis)
    );

    let silence = analyze(&empty(SR, 3.0), SR, AnalysisMode::Deep);
    assert_unknown(&silence, UnknownReason::Silent);

    let mut hit = empty(SR, 3.0);
    add_tone(&mut hit, SR, at(SR, 0.4), 60.0, 20.0, 1.0, 0.2);
    let hit_analysis = analyze(&hit, SR, AnalysisMode::Deep);
    assert!(
        hit_analysis.tempo.bpm.is_none(),
        "one-shot got a tempo\n{}",
        dump(&hit_analysis)
    );

    let (short, _) = metronome(SR, 120.0, 0.4);
    let short_analysis = analyze(&short, SR, AnalysisMode::Fast);
    assert_unknown(&short_analysis, UnknownReason::InsufficientDuration);
}

#[test]
fn sustained_pad_is_not_a_confident_tempo() {
    let mut audio = empty(SR, 5.0);
    for (i, sample) in audio.iter_mut().enumerate() {
        let t = i as f32 / SR as f32;
        let env = (1.0 - (-t * 1.5).exp()) * 0.2;
        *sample = env * (2.0 * std::f32::consts::PI * 220.0 * t).sin();
    }
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    if let Some(bpm) = analysis.tempo.bpm {
        assert!(
            analysis.tempo.confidence < 0.45,
            "pad assigned {bpm} confidently\n{}",
            dump(&analysis)
        );
    }
}

#[test]
fn sample_rate_does_not_change_the_reading() {
    let mut readings = Vec::new();
    for sr in [22_050, 44_100, 48_000] {
        let (audio, _) = metronome(sr, 120.0, 6.0);
        let original = audio.clone();
        let analysis = analyze(&audio, sr, AnalysisMode::Deep);
        assert_eq!(audio, original);
        assert_bpm(&analysis, 120.0, 1.5);
        readings.push(analysis.tempo.bpm.unwrap());
    }
    for pair in readings.windows(2) {
        assert!(
            (pair[0] - pair[1]).abs() <= 1.5,
            "rates diverged {readings:?}"
        );
    }
}

#[test]
fn fast_mode_matches_a_steady_pulse() {
    let (audio, _) = metronome(SR, 120.0, 6.0);
    let analysis = analyze(&audio, SR, AnalysisMode::Fast);
    assert_bpm(&analysis, 120.0, 2.0);
}

#[test]
fn long_intro_does_not_pin_the_phase_to_zero() {
    let mut audio = empty(SR, 10.0);
    let step = 0.5;
    let mut times = Vec::new();
    let mut t = 4.0;
    while t < 9.7 {
        add_tone(&mut audio, SR, at(SR, t), 70.0, 40.0, 0.95, 0.07);
        times.push(t);
        t += step;
    }
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, 120.0, 2.0);
    let hits = beat_hit_ratio(&analysis, &times, 0.06);
    assert!(hits >= 0.7, "hits {hits}\n{}", dump(&analysis));
}

#[test]
fn tempo_drift_lowers_stability_and_moves_the_curve() {
    let steady = analyze(&metronome(SR, 120.0, 12.0).0, SR, AnalysisMode::Deep);
    let mut audio = empty(SR, 12.0);
    let mut t = 0.15;
    let start = 110.0;
    let end = 132.0;
    while t < 11.6 {
        add_tone(&mut audio, SR, at(SR, t), 68.0, 40.0, 0.95, 0.07);
        let along = (t / 12.0).clamp(0.0, 1.0);
        let bpm = start + (end - start) * along;
        t += 60.0 / bpm;
    }
    let drifted = analyze(&audio, SR, AnalysisMode::Deep);
    assert!(
        drifted.tempo.bpm.is_some(),
        "drift unknown\n{}",
        dump(&drifted)
    );
    let bpm = drifted.tempo.bpm.unwrap();
    assert!(
        (105.0..=140.0).contains(&bpm),
        "drift bpm {bpm}\n{}",
        dump(&drifted)
    );
    assert!(
        drifted.tempo.stability < steady.tempo.stability,
        "drift {:.2} not less stable than steady {:.2}",
        drifted.tempo.stability,
        steady.tempo.stability
    );
    if drifted.tempo_curve.len() >= 4 {
        let first = drifted.tempo_curve[1].bpm;
        let last = drifted.tempo_curve[drifted.tempo_curve.len() - 2].bpm;
        assert!(
            last > first + 6.0,
            "curve did not rise ({first} → {last})\n{}",
            dump(&drifted)
        );
    }
}

#[test]
fn accented_bars_can_mark_downbeats() {
    let bpm = 120.0;
    let step = 60.0 / bpm;
    let mut audio = empty(SR, 8.0);
    let mut loud = Vec::new();
    let mut t = 0.1;
    let mut beat = 0;
    while t < 7.7 {
        let amp = if beat % 4 == 0 { 1.0 } else { 0.38 };
        if beat % 4 == 0 {
            loud.push(t);
            add_tone(&mut audio, SR, at(SR, t), 48.0, 22.0, amp, 0.1);
        } else {
            add_tone(&mut audio, SR, at(SR, t), 90.0, 48.0, amp, 0.04);
        }
        t += step;
        beat += 1;
    }
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert_bpm(&analysis, bpm, 2.0);
    if let Some(meter) = analysis.meter.beats_per_bar {
        assert_eq!(meter, 4, "{}", dump(&analysis));
    }
    if !analysis.downbeats.is_empty() {
        let hits = analysis
            .downbeats
            .iter()
            .filter(|down| loud.iter().any(|time| (down.time_secs - time).abs() < 0.06))
            .count();
        assert!(
            hits * 2 >= analysis.downbeats.len(),
            "downbeats missed the accents\n{}",
            dump(&analysis)
        );
    }
}

#[test]
fn warp_maps_audio_time_to_beats() {
    let (audio, _) = metronome(SR, 120.0, 6.0);
    let analysis = analyze(&audio, SR, AnalysisMode::Deep);
    assert!(analysis.beats.len() >= 6, "{}", dump(&analysis));
    let beat = &analysis.beats[4];
    let mapped = analysis.warp.beat_at(beat.time_secs).unwrap();
    assert!((mapped - 4.0).abs() < 0.05, "mapped {mapped}");
    let back = analysis.warp.audio_at(4.0).unwrap();
    assert!((back - beat.time_secs).abs() < 0.02, "back {back}");
}

#[test]
fn analyzer_leaves_the_caller_buffer_alone() {
    let (mut audio, _) = metronome(SR, 100.0, 4.0);
    audio[10] = 0.25;
    let original = audio.clone();
    let _ = analyze(&audio, SR, AnalysisMode::Fast);
    assert_eq!(audio, original);
}
