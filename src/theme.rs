//! Layout and color constants shared by UI modules.

use egui::Color32;

/// Ctrl+wheel zoom. Wide range so you can see a whole hour or individual samples.
/// Finite only to keep `f32` math and the ruler from blowing up.
pub const TIMELINE_PPS_MIN: f32 = 0.01;
pub const TIMELINE_PPS_MAX: f32 = 2_000_000.0;

/// While playing, nudge horizontal scroll if the playhead gets closer than this to a viewport edge.
pub const PLAYHEAD_EDGE_MARGIN_PX: f32 = 48.0;

/// Time scale bar height at the top of the timeline stack.
pub const TIME_RULER_HEIGHT: f32 = 30.0;

/// Tempo / ruler-mode strip above the time ruler.
pub const TEMPO_BAR_HEIGHT: f32 = 36.0;

/// Left track-select column.
pub const TRACK_GUTTER_WIDTH: f32 = 40.0;

/// Horizontal hit width for trim handles on a selected clip.
pub const TRIM_HANDLE_WIDTH_PX: f32 = 10.0;

/// Minimum visible clip length after trim (seconds); used by trim drag and split.
pub const MIN_TRIM_DURATION_SECS: f32 = 0.08;

// Transport bar
pub const TRANSPORT_BTN_DIAMETER: f32 = 56.0;
pub const TRANSPORT_BTN_GAP: f32 = 24.0;
/// Reserve under the buttons for a possible error line.
pub const TRANSPORT_RESERVE_H: f32 = 40.0;

pub const PIANO_KEY_COUNT: usize = 16;
pub const PIANO_KEY_W: f32 = 32.0;
pub const PIANO_KEY_MIN_H: f32 = 14.0;
pub const TIMELINE_TRACK_HEIGHT: f32 = 160.0;
/// Alt+wheel over the studio sidebar. Auto-fit still caps at [`TIMELINE_TRACK_HEIGHT`].
pub const STUDIO_LANE_H_MIN: f32 = 72.0;
pub const STUDIO_LANE_H_MAX: f32 = 280.0;

pub const STUDIO_TOP_BAR_H: f32 = 52.0;
pub const STUDIO_SIDEBAR_W: f32 = 200.0;
pub const SAMPLER_MAP_H: f32 = 60.0;
pub const SAMPLER_WAVE_H: f32 = 320.0;
pub const SAMPLER_PAD_H: f32 = 48.0;
pub const SAMPLER_PAD_GAP: f32 = 6.0;
pub const SAMPLER_PAD_ROWS: usize = 2;
pub const SAMPLER_PAD_COLS: usize = 8;

pub fn color_transport_load() -> Color32 {
    Color32::from_rgb(64, 108, 168)
}

pub fn color_transport_bar() -> Color32 {
    Color32::from_rgb(52, 52, 62)
}

pub fn color_transport_play() -> Color32 {
    Color32::from_rgb(52, 140, 92)
}

pub fn color_transport_stop() -> Color32 {
    Color32::from_rgb(138, 56, 56)
}

pub fn color_track_mute() -> Color32 {
    Color32::from_rgb(168, 72, 56)
}

pub fn color_track_solo() -> Color32 {
    Color32::from_rgb(168, 132, 48)
}

pub fn color_timeline_bg() -> Color32 {
    Color32::from_rgb(30, 30, 36)
}

pub fn color_timeline_bg_alt() -> Color32 {
    Color32::from_rgb(38, 38, 46)
}

pub fn color_timeline_border() -> Color32 {
    Color32::from_gray(80)
}

pub fn color_clip_bg() -> Color32 {
    Color32::from_rgb(186, 190, 236)
}

pub fn color_clip_waveform() -> Color32 {
    Color32::from_rgb(42, 42, 56)
}

pub fn color_clip_zero_line() -> Color32 {
    Color32::from_rgb(120, 124, 180)
}

pub fn color_playhead() -> Color32 {
    Color32::from_rgb(200, 80, 80)
}

pub fn color_sampler_base() -> Color32 {
    Color32::from_rgb(220, 220, 236)
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

pub fn color_marker() -> Color32 {
    Color32::from_rgb(230, 180, 64)
}

pub fn color_marker_lane_bg() -> Color32 {
    Color32::from_rgb(20, 20, 26)
}

pub fn color_marker_delete() -> Color32 {
    Color32::from_rgb(40, 36, 36)
}

pub fn color_track_gutter() -> Color32 {
    Color32::from_rgb(22, 22, 28)
}

pub fn color_track_gutter_selected() -> Color32 {
    Color32::from_rgb(48, 70, 118)
}

pub fn color_sampler_viewport() -> Color32 {
    Color32::from_rgba_unmultiplied(220, 230, 255, 55)
}

pub fn color_sampler_viewport_stroke() -> Color32 {
    Color32::from_rgba_unmultiplied(230, 240, 255, 180)
}

pub fn color_pad_empty() -> Color32 {
    Color32::from_rgb(42, 42, 50)
}

pub fn color_pad_assigned() -> Color32 {
    Color32::from_rgb(72, 92, 148)
}

pub fn color_pad_selected() -> Color32 {
    Color32::from_rgb(96, 122, 186)
}

pub fn color_pad_held() -> Color32 {
    Color32::from_rgb(52, 140, 92)
}

pub fn color_piano_key() -> Color32 {
    Color32::from_rgb(48, 48, 56)
}

pub fn color_piano_key_alt() -> Color32 {
    Color32::from_rgb(38, 38, 46)
}

pub fn color_piano_key_bound() -> Color32 {
    Color32::from_rgb(62, 78, 118)
}

pub fn color_piano_grid_beat() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 28)
}

pub fn color_piano_grid_step() -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, 12)
}

pub fn color_piano_note() -> Color32 {
    Color32::from_rgb(186, 190, 236)
}

pub fn color_piano_note_selected() -> Color32 {
    Color32::from_rgb(220, 224, 255)
}
