//! Modal sampling instrument: preview, pitch, tempo, overview map, main waveform, pads.

use egui::{Align2, Color32, FontId, Key, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};

use crate::model::PadMarker;
use crate::theme;
use crate::waveform::{self, PeakPyramid};

pub const PAD_COUNT: usize = 16;
pub const PAD_LABELS: [&str; PAD_COUNT] = [
    "Q", "W", "E", "R", "T", "Y", "U", "I", "A", "S", "D", "F", "G", "H", "J", "K",
];
const PAD_KEYS: [Key; PAD_COUNT] = [
    Key::Q,
    Key::W,
    Key::E,
    Key::R,
    Key::T,
    Key::Y,
    Key::U,
    Key::I,
    Key::A,
    Key::S,
    Key::D,
    Key::F,
    Key::G,
    Key::H,
    Key::J,
    Key::K,
];

pub struct SamplerModel<'a> {
    pub track_name: &'a str,
    pub pitch_semitones: i32,
    pub tempo_bpm: f32,
    pub sample: Option<(&'a [f32], &'a PeakPyramid)>,
    pub preview_playing: bool,
    pub preview_secs: f32,
    pub base_secs: f32,
    pub sample_rate: u32,
    pub speed: f32,
    pub status: &'a str,
    pub pad_markers: &'a [PadMarker],
    pub selected_pad: Option<u8>,
    pub active_pad: Option<u8>,
}

pub enum SamplerAction {
    Close,
    TogglePreview,
    PitchDelta(i32),
    TempoDelta(f32),
    LoadFile,
    SeekCursor { sample_index: usize },
    MovePad { slot: u8, sample_index: usize },
    DeletePad { slot: u8 },
    SelectPad { slot: Option<u8> },
    TriggerPad { slot: u8 },
}

pub fn pad_slot_from_keys(i: &egui::InputState) -> Option<u8> {
    if i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt {
        return None;
    }
    PAD_KEYS
        .into_iter()
        .enumerate()
        .find(|(_, k)| i.key_pressed(*k))
        .map(|(i, _)| i as u8)
}

pub fn show(
    ctx: &egui::Context,
    model: SamplerModel<'_>,
    view_start: &mut f32,
    view_len: &mut f32,
    pad_drag: &mut Option<u8>,
) -> Option<SamplerAction> {
    let mut action = None;
    let screen = ctx.screen_rect();
    // Dim lives on Middle so a click cannot promote it above the Foreground window.
    egui::Area::new(egui::Id::new("sampler_dim"))
        .order(egui::Order::Middle)
        .fixed_pos(screen.min)
        .interactable(false)
        .show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(170));
        });

    let mut open = true;
    egui::Window::new("Инструмент сэмплинга")
        .id(egui::Id::new("sampling_instrument"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([960.0, 640.0])
        .min_width(760.0)
        .min_height(560.0)
        .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(model.track_name).strong().size(16.0));
                ui.add_space(12.0);
                let play_label = if model.preview_playing { "⏹ Стоп" } else { "▶ Плей" };
                let play_col = if model.preview_playing {
                    theme::color_transport_stop()
                } else {
                    theme::color_transport_play()
                };
                if ui
                    .add(
                        egui::Button::new(egui::RichText::new(play_label).color(Color32::WHITE))
                            .fill(play_col)
                            .min_size(Vec2::new(88.0, 28.0)),
                    )
                    .clicked()
                {
                    action = Some(SamplerAction::TogglePreview);
                }
                ui.separator();
                ui.label(egui::RichText::new("Нота").weak());
                if ui.button("−").on_hover_text("Ниже на полутон").clicked() {
                    action = Some(SamplerAction::PitchDelta(-1));
                }
                ui.label(
                    egui::RichText::new(format_pitch(model.pitch_semitones))
                        .monospace()
                        .size(14.0),
                );
                if ui.button("+").on_hover_text("Выше на полутон").clicked() {
                    action = Some(SamplerAction::PitchDelta(1));
                }
                ui.separator();
                ui.label(egui::RichText::new("Темп").weak());
                if ui.button("−").on_hover_text("Медленнее на 1 BPM").clicked() {
                    action = Some(SamplerAction::TempoDelta(-1.0));
                }
                ui.label(
                    egui::RichText::new(format!("{:.0} BPM", model.tempo_bpm))
                        .monospace()
                        .size(14.0),
                );
                if ui.button("+").on_hover_text("Быстрее на 1 BPM").clicked() {
                    action = Some(SamplerAction::TempoDelta(1.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Загрузить WAV / MP3").clicked() {
                        action = Some(SamplerAction::LoadFile);
                    }
                });
            });
            ui.add_space(8.0);

            let sample_n = model.sample.map(|(d, _)| d.len() as f32).unwrap_or(0.0);
            clamp_view(view_start, view_len, sample_n);

            let map_h = theme::SAMPLER_MAP_H;
            let (map_rect, map_resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), map_h), Sense::click_and_drag());
            paint_map(ui, map_rect, &model, *view_start, *view_len, sample_n);
            if sample_n > 1.0 {
                handle_map_nav(ctx, &map_resp, map_rect, view_start, view_len, sample_n);
            }

            ui.add_space(8.0);
            let pad_block_h = theme::SAMPLER_PAD_H * theme::SAMPLER_PAD_ROWS as f32
                + theme::SAMPLER_PAD_GAP
                + 22.0;
            let wave_budget = (ui.available_height() - pad_block_h - 8.0).max(120.0);
            let wave_h = theme::SAMPLER_WAVE_H.min(wave_budget);
            let (wave_rect, wave_resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), wave_h),
                Sense::click_and_drag(),
            );
            paint_main_wave(ui, wave_rect, &model, *view_start, *view_len, sample_n);
            if sample_n > 1.0 {
                handle_wave_scroll(ctx, &wave_resp, wave_rect, view_start, view_len, sample_n);
                if let Some(wave_action) = handle_wave_markers(
                    &wave_resp,
                    wave_rect,
                    &model,
                    *view_start,
                    *view_len,
                    sample_n,
                    pad_drag,
                ) {
                    action = Some(wave_action);
                }
            } else {
                *pad_drag = None;
            }

            ui.add_space(8.0);
            if let Some(pad_action) = show_pads(ui, ctx, &model) {
                action = Some(pad_action);
            }

            ui.add_space(6.0);
            if model.sample.is_none() {
                ui.label(
                    egui::RichText::new("Перетащите WAV или MP3 сюда — или нажмите «Загрузить».")
                        .weak()
                        .italics(),
                );
            } else {
                ui.label(
                    egui::RichText::new(
                        "Клик по waveform — базовая позиция. Стоп возвращает курсор к ней. Alt+скролл — горизонтально. Свободный пэд (Q–I / A–K) — метка на курсор.",
                    )
                    .weak()
                    .size(12.0),
                );
                if !model.status.is_empty() {
                    ui.label(egui::RichText::new(model.status).weak().size(12.0));
                }
            }
        });

    if !open || ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        action = Some(SamplerAction::Close);
    }
    action
}

fn format_pitch(semitones: i32) -> String {
    if semitones > 0 {
        format!("+{semitones} st")
    } else {
        format!("{semitones} st")
    }
}

fn clamp_view(start: &mut f32, len: &mut f32, n: f32) {
    if n <= 1.0 {
        *start = 0.0;
        *len = 1.0;
        return;
    }
    *len = len.clamp(8.0, n);
    *start = start.clamp(0.0, (n - *len).max(0.0));
}

fn handle_map_nav(
    ctx: &egui::Context,
    resp: &egui::Response,
    map: Rect,
    start: &mut f32,
    len: &mut f32,
    n: f32,
) {
    let w = map.width().max(1.0);
    if resp.dragged_by(PointerButton::Primary) {
        let dx = resp.drag_delta().x;
        *start = (*start + dx / w * n).clamp(0.0, (n - *len).max(0.0));
    } else if resp.clicked() {
        if let Some(pos) = resp.interact_pointer_pos() {
            let u = ((pos.x - map.left()) / w).clamp(0.0, 1.0);
            let center = u * n;
            *start = (center - *len * 0.5).clamp(0.0, (n - *len).max(0.0));
        }
    }
    if resp.hovered() {
        pan_view_alt_scroll(ctx, w, start, *len, n);
    }
}

fn handle_wave_scroll(
    ctx: &egui::Context,
    resp: &egui::Response,
    wave: Rect,
    start: &mut f32,
    len: &mut f32,
    n: f32,
) {
    if !resp.hovered() {
        return;
    }
    let (ctrl, alt, dy) = scroll_input(ctx);
    if dy.abs() < 0.01 {
        return;
    }
    if ctrl && !alt {
        let pos = ctx.pointer_hover_pos().unwrap_or(wave.center());
        let u = ((pos.x - wave.left()) / wave.width().max(1.0)).clamp(0.0, 1.0);
        let anchor = *start + u * *len;
        let new_len = (*len * (1.0 - dy * 0.004)).clamp(8.0, n);
        *len = new_len;
        *start = (anchor - u * new_len).clamp(0.0, (n - new_len).max(0.0));
    } else if alt && !ctrl {
        pan_view_alt_scroll(ctx, wave.width().max(1.0), start, *len, n);
    }
}

fn scroll_input(ctx: &egui::Context) -> (bool, bool, f32) {
    ctx.input(|i| {
        (
            i.modifiers.ctrl || i.modifiers.command,
            i.modifiers.alt,
            i.smooth_scroll_delta.y + i.raw_scroll_delta.y,
        )
    })
}

fn pan_view_alt_scroll(ctx: &egui::Context, width: f32, start: &mut f32, len: f32, n: f32) {
    let (ctrl, alt, dy) = scroll_input(ctx);
    if !alt || ctrl || dy.abs() < 0.01 {
        return;
    }
    *start = (*start - dy / width.max(1.0) * len).clamp(0.0, (n - len).max(0.0));
}

fn sample_at_x(x: f32, rect: Rect, view_start: f32, view_len: f32, n: f32) -> usize {
    let u = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    let idx = view_start + u * view_len.max(1.0);
    idx.round().clamp(0.0, (n - 1.0).max(0.0)) as usize
}

fn sample_to_x(index: usize, rect: Rect, view_start: f32, view_len: f32) -> f32 {
    let span = view_len.max(1.0);
    rect.left() + (index as f32 - view_start) / span * rect.width()
}

const MARKER_CHIP_W: f32 = 22.0;
const MARKER_CHIP_H: f32 = 18.0;
const MARKER_DELETE_W: f32 = 14.0;
const MARKER_HIT_PX: f32 = 8.0;

fn marker_chip_rect(index: usize, wave: Rect, view_start: f32, view_len: f32, selected: bool) -> Rect {
    let x = sample_to_x(index, wave, view_start, view_len);
    let w = if selected {
        MARKER_CHIP_W + MARKER_DELETE_W
    } else {
        MARKER_CHIP_W
    };
    Rect::from_min_size(
        Pos2::new(x - MARKER_CHIP_W * 0.5, wave.top() + 4.0),
        Vec2::new(w, MARKER_CHIP_H),
    )
}

fn marker_delete_rect(chip: Rect) -> Rect {
    Rect::from_min_max(
        Pos2::new(chip.right() - MARKER_DELETE_W, chip.top()),
        chip.max,
    )
}

fn hit_pad_marker(
    markers: &[PadMarker],
    pos: Pos2,
    wave: Rect,
    view_start: f32,
    view_len: f32,
    selected: Option<u8>,
) -> Option<(u8, bool)> {
    let mut hits: Vec<(u8, bool, f32)> = Vec::new();
    for m in markers {
        if (m.slot as usize) >= PAD_COUNT {
            continue;
        }
        let is_sel = selected == Some(m.slot);
        let chip = marker_chip_rect(m.sample_index, wave, view_start, view_len, is_sel);
        let x = sample_to_x(m.sample_index, wave, view_start, view_len);
        let on_chip = chip.contains(pos);
        let on_line = (pos.x - x).abs() <= MARKER_HIT_PX && wave.contains(pos);
        if !on_chip && !on_line {
            continue;
        }
        let on_delete = is_sel && on_chip && marker_delete_rect(chip).contains(pos);
        hits.push((m.slot, on_delete, (x - pos.x).abs()));
    }
    hits.sort_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));
    hits.first().map(|&(slot, del, _)| (slot, del))
}

fn handle_wave_markers(
    resp: &egui::Response,
    wave: Rect,
    model: &SamplerModel<'_>,
    view_start: f32,
    view_len: f32,
    n: f32,
    pad_drag: &mut Option<u8>,
) -> Option<SamplerAction> {
    let ctrl = resp.ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
    let pos = resp.interact_pointer_pos();

    if resp.drag_started_by(PointerButton::Primary) {
        if let Some(pos) = pos {
            if let Some((slot, on_delete)) =
                hit_pad_marker(model.pad_markers, pos, wave, view_start, view_len, model.selected_pad)
            {
                if !on_delete {
                    *pad_drag = Some(slot);
                    return Some(SamplerAction::SelectPad { slot: Some(slot) });
                }
            }
        }
    }

    if let Some(slot) = *pad_drag {
        if resp.dragged_by(PointerButton::Primary) {
            if let Some(pos) = pos {
                let idx = sample_at_x(pos.x, wave, view_start, view_len, n);
                return Some(SamplerAction::MovePad {
                    slot,
                    sample_index: idx,
                });
            }
        }
        if resp.drag_stopped() {
            *pad_drag = None;
        }
    }

    if resp.clicked() && !ctrl {
        let pos = pos?;
        if let Some((slot, on_delete)) =
            hit_pad_marker(model.pad_markers, pos, wave, view_start, view_len, model.selected_pad)
        {
            if on_delete {
                *pad_drag = None;
                return Some(SamplerAction::DeletePad { slot });
            }
            return Some(SamplerAction::SelectPad { slot: Some(slot) });
        }
        let idx = sample_at_x(pos.x, wave, view_start, view_len, n);
        Some(SamplerAction::SeekCursor { sample_index: idx })
    } else {
        None
    }
}

fn paint_map(
    ui: &egui::Ui,
    rect: Rect,
    model: &SamplerModel<'_>,
    view_start: f32,
    view_len: f32,
    n: f32,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::color_ruler_bg());
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, theme::color_timeline_border()));
    if let Some((data, peaks)) = model.sample {
        waveform::paint_waveform_overlay(
            &painter,
            rect,
            rect,
            data,
            peaks,
            0,
            data.len(),
            theme::color_clip_waveform(),
            theme::color_clip_zero_line(),
        );
        if n > 1.0 {
            let x0 = rect.left() + (view_start / n) * rect.width();
            let x1 = rect.left() + ((view_start + view_len) / n) * rect.width();
            let vp = Rect::from_min_max(
                Pos2::new(x0, rect.top() + 1.0),
                Pos2::new(x1, rect.bottom() - 1.0),
            );
            painter.rect_filled(vp, 2.0, theme::color_sampler_viewport());
            painter.rect_stroke(vp, 2.0, Stroke::new(1.0_f32, theme::color_sampler_viewport_stroke()));
            let col = theme::color_marker();
            for m in model.pad_markers {
                let x = rect.left() + (m.sample_index as f32 / n) * rect.width();
                painter.line_segment(
                    [Pos2::new(x, rect.top() + 2.0), Pos2::new(x, rect.bottom() - 2.0)],
                    Stroke::new(1.0_f32, col),
                );
            }
        }
        paint_cursors(&painter, rect, 0.0, n, model);
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "карта сэмпла",
            FontId::proportional(13.0),
            Color32::from_gray(120),
        );
    }
}

fn paint_main_wave(
    ui: &egui::Ui,
    rect: Rect,
    model: &SamplerModel<'_>,
    view_start: f32,
    view_len: f32,
    n: f32,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, theme::color_timeline_bg());
    painter.rect_stroke(rect, 4.0, Stroke::new(1.0_f32, theme::color_timeline_border()));
    if let Some((data, peaks)) = model.sample {
        let s0 = view_start.floor().max(0.0) as usize;
        let s1 = (view_start + view_len).ceil().min(data.len() as f32) as usize;
        waveform::paint_waveform_overlay(
            &painter,
            rect,
            rect,
            data,
            peaks,
            s0,
            s1.max(s0 + 1),
            theme::color_clip_waveform(),
            theme::color_clip_zero_line(),
        );
        paint_pad_markers(&painter, rect, model, view_start, view_len);
        paint_cursors(&painter, rect, view_start, view_len.max(1.0), model);
        let _ = n;
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "waveform сэмпла",
            FontId::proportional(16.0),
            Color32::from_gray(120),
        );
    }
}

fn paint_pad_markers(
    painter: &egui::Painter,
    rect: Rect,
    model: &SamplerModel<'_>,
    view_start: f32,
    view_len: f32,
) {
    let col = theme::color_marker();
    let font = FontId::proportional(11.0);
    let span_end = view_start + view_len;
    for m in model.pad_markers {
        let slot = m.slot as usize;
        if slot >= PAD_COUNT {
            continue;
        }
        let idx = m.sample_index as f32;
        if idx < view_start - 1.0 || idx > span_end + 1.0 {
            continue;
        }
        let x = sample_to_x(m.sample_index, rect, view_start, view_len);
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.5_f32, col),
        );
        let selected = model.selected_pad == Some(m.slot);
        let chip = marker_chip_rect(m.sample_index, rect, view_start, view_len, selected);
        painter.rect_filled(chip, 3.0, col);
        if selected {
            painter.rect_stroke(chip, 3.0, Stroke::new(1.5_f32, Color32::WHITE));
            let del = marker_delete_rect(chip);
            painter.rect_filled(del, 3.0, theme::color_marker_delete());
            painter.text(
                del.center(),
                Align2::CENTER_CENTER,
                "×",
                font.clone(),
                Color32::from_gray(210),
            );
        }
        painter.text(
            Pos2::new(
                chip.left() + MARKER_CHIP_W * 0.5,
                chip.center().y,
            ),
            Align2::CENTER_CENTER,
            PAD_LABELS[slot],
            font.clone(),
            Color32::BLACK,
        );
    }
}

fn sample_idx_to_x(idx: f32, rect: Rect, view_start: f32, view_span: f32) -> Option<f32> {
    if view_span <= 0.0 || idx < view_start || idx > view_start + view_span {
        return None;
    }
    Some(rect.left() + (idx - view_start) / view_span * rect.width())
}

fn paint_cursors(
    painter: &egui::Painter,
    rect: Rect,
    view_start: f32,
    view_span: f32,
    model: &SamplerModel<'_>,
) {
    let rate_speed = model.sample_rate as f32 * model.speed.max(0.05);
    let base_idx = model.base_secs * rate_speed;
    if let Some(x) = sample_idx_to_x(base_idx, rect, view_start, view_span) {
        let col = theme::color_sampler_base();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.5_f32, col),
        );
        let top = rect.top();
        painter.add(egui::Shape::convex_polygon(
            vec![
                Pos2::new(x - 6.0, top),
                Pos2::new(x + 6.0, top),
                Pos2::new(x, top + 9.0),
            ],
            col,
            Stroke::NONE,
        ));
    }
    let active_idx = model.preview_secs * rate_speed;
    if let Some(x) = sample_idx_to_x(active_idx, rect, view_start, view_span) {
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(2.0_f32, theme::color_playhead()),
        );
    }
}

fn show_pads(ui: &mut egui::Ui, ctx: &egui::Context, model: &SamplerModel<'_>) -> Option<SamplerAction> {
    let mut action = None;
    let gap = theme::SAMPLER_PAD_GAP;
    let cols = theme::SAMPLER_PAD_COLS;
    let avail = ui.available_width();
    let cell_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).max(36.0);
    let cell_h = theme::SAMPLER_PAD_H;
    let held = ctx.input(|i| {
        PAD_KEYS
            .into_iter()
            .enumerate()
            .find(|(_, k)| i.key_down(*k) && !i.modifiers.ctrl && !i.modifiers.command && !i.modifiers.alt)
            .map(|(i, _)| i as u8)
    });

    egui::Grid::new("sampler_pads")
        .num_columns(cols)
        .spacing([gap, gap])
        .min_col_width(cell_w)
        .show(ui, |ui| {
            for slot in 0..PAD_COUNT {
                let slot_u8 = slot as u8;
                let assigned = model.pad_markers.iter().any(|m| m.slot == slot_u8);
                let selected = model.selected_pad == Some(slot_u8);
                let active = model.active_pad == Some(slot_u8) && model.preview_playing;
                let key_held = held == Some(slot_u8);
                let fill = if key_held || active {
                    theme::color_pad_held()
                } else if selected {
                    theme::color_pad_selected()
                } else if assigned {
                    theme::color_pad_assigned()
                } else {
                    theme::color_pad_empty()
                };
                let text_col = if assigned || key_held || active {
                    Color32::WHITE
                } else {
                    Color32::from_gray(140)
                };
                let btn = egui::Button::new(
                    egui::RichText::new(PAD_LABELS[slot])
                        .size(18.0)
                        .strong()
                        .color(text_col),
                )
                .min_size(Vec2::new(cell_w, cell_h))
                .fill(fill)
                .stroke(Stroke::new(
                    if selected { 1.5_f32 } else { 1.0_f32 },
                    if selected {
                        Color32::WHITE
                    } else {
                        Color32::from_gray(70)
                    },
                ));
                let resp = ui.add(btn);
                if assigned {
                    resp.clone().on_hover_text(format!(
                        "Пэд {} — воспроизвести с метки",
                        PAD_LABELS[slot]
                    ));
                } else {
                    resp.clone().on_hover_text(format!(
                        "Пэд {} — метка на курсор",
                        PAD_LABELS[slot]
                    ));
                }
                if resp.clicked() {
                    action = Some(SamplerAction::TriggerPad { slot: slot_u8 });
                }
                if (slot + 1) % cols == 0 {
                    ui.end_row();
                }
            }
        });
    action
}
