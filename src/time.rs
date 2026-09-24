//! Musical time helpers (no UI). 4/4, tempo clamped to 20–400 BPM.

pub const BEATS_PER_BAR: f32 = 4.0;

pub fn format_bpm(tempo_bpm: f32) -> String {
    let rounded = (tempo_bpm * 10.0).round() / 10.0;
    if (rounded - rounded.round()).abs() < 0.05 {
        format!("{rounded:.0} BPM")
    } else {
        format!("{rounded:.1} BPM")
    }
}

pub fn beat_secs(tempo_bpm: f32) -> f32 {
    60.0 / tempo_bpm.clamp(20.0, 400.0)
}

pub fn grid_step_secs(tempo_bpm: f32) -> f32 {
    beat_secs(tempo_bpm) / 4.0
}

pub fn default_seq_duration_secs(tempo_bpm: f32) -> f32 {
    beat_secs(tempo_bpm) * BEATS_PER_BAR * 4.0
}

pub fn snap_time_floor(time_secs: f32, step: f32) -> f32 {
    if step <= 1e-6 {
        return time_secs.max(0.0);
    }
    (time_secs.max(0.0) / step).floor() * step
}

pub fn snap_time_round(time_secs: f32, step: f32) -> f32 {
    if step <= 1e-6 {
        return time_secs.max(0.0);
    }
    (time_secs.max(0.0) / step).round() * step
}
