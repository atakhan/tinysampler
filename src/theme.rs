//! Layout and color constants shared by UI modules.

use egui::Color32;

/// Same range as the "px / sec" slider; Ctrl+wheel on the timeline uses these bounds.
pub const TIMELINE_PPS_MIN: f32 = 40.0;
pub const TIMELINE_PPS_MAX: f32 = 300.0;

/// While playing, nudge horizontal scroll if the playhead gets closer than this to a viewport edge.
pub const PLAYHEAD_EDGE_MARGIN_PX: f32 = 48.0;

/// Time scale bar height at the top of the timeline stack.
pub const TIME_RULER_HEIGHT: f32 = 30.0;

/// Horizontal hit width for trim handles on a selected clip.
pub const TRIM_HANDLE_WIDTH_PX: f32 = 10.0;

/// Minimum visible clip length after trim (seconds); used by trim drag and split.
pub const MIN_TRIM_DURATION_SECS: f32 = 0.08;

// Transport bar
pub const TRANSPORT_BTN_DIAMETER: f32 = 56.0;
pub const TRANSPORT_BTN_GAP: f32 = 24.0;
/// Reserve under the buttons for a possible error line.
pub const TRANSPORT_RESERVE_H: f32 = 40.0;

pub const TIMELINE_TRACK_HEIGHT: f32 = 160.0;

pub fn color_transport_load() -> Color32 {
    Color32::from_rgb(64, 108, 168)
}

pub fn color_transport_play() -> Color32 {
    Color32::from_rgb(52, 140, 92)
}

pub fn color_transport_pause() -> Color32 {
    Color32::from_rgb(118, 98, 52)
}

pub fn color_transport_stop() -> Color32 {
    Color32::from_rgb(138, 56, 56)
}

pub fn color_timeline_bg() -> Color32 {
    Color32::from_rgb(30, 30, 36)
}

pub fn color_timeline_border() -> Color32 {
    Color32::from_gray(80)
}

pub fn color_clip_fallback() -> Color32 {
    Color32::from_rgb(50, 90, 160)
}

pub fn color_playhead() -> Color32 {
    Color32::from_rgb(200, 80, 80)
}

pub fn color_playhead_cross() -> Color32 {
    Color32::from_rgba_unmultiplied(240, 120, 120, 110)
}

pub fn color_ruler_bg() -> Color32 {
    Color32::from_rgb(24, 24, 30)
}

pub fn color_ruler_line_major() -> Color32 {
    Color32::from_gray(95)
}

pub fn color_ruler_line_minor() -> Color32 {
    Color32::from_gray(62)
}

pub fn color_ruler_text() -> Color32 {
    Color32::from_gray(200)
}

pub fn color_ruler_bottom_line() -> Color32 {
    Color32::from_gray(55)
}
