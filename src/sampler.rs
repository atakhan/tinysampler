//! Modal sampling instrument: preview, pitch, tempo, overview map, main waveform, pads.

use egui::{
    Align2, Color32, CursorIcon, Event, FontId, Key, PointerButton, Pos2, Rect, Sense, Shape, Stroke, Vec2,
};

use crate::model::{PadEdge, PadMarker, SampleSlice};
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
    pub slices: &'a [SampleSlice],
    pub selected_pad: Option<u8>,
    pub active_pad: Option<u8>,
}

pub enum SamplerAction {
    Close,
    TogglePreview,
    PitchDelta(i32),
    TempoDelta(f32),
    LoadFile,
    AddSounds,
    SeekCursor { sample_index: usize },
    MovePad {
        slot: u8,
        edge: PadEdge,
        sample_index: usize,
    },
    DeletePad { slot: u8 },
    SelectPad { slot: Option<u8> },
    TriggerPad { slot: u8 },
    /// Move file `from` so it occupies slot `to`. Files stay contiguous.
    ReorderSlice { from: usize, to: usize },
    DeleteSlice { index: usize },
}

pub fn pad_slot_from_keys(i: &egui::InputState) -> Option<u8> {
    pad_slots_pressed(i).next()
}

pub fn pad_slots_pressed(i: &egui::InputState) -> impl Iterator<Item = u8> + '_ {
    pad_slots_where(i, key_pressed_no_repeat)
}

fn key_pressed_no_repeat(i: &egui::InputState, k: Key) -> bool {
    i.events.iter().any(|e| {
        matches!(
            e,
            Event::Key {
                key,
                pressed: true,
                repeat: false,
                ..
            } if *key == k
        )
    })
}

pub fn pad_key_down(i: &egui::InputState, slot: u8) -> bool {
    if i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt {
        return false;
    }
    PAD_KEYS
        .get(slot as usize)
        .is_some_and(|k| i.key_down(*k))
}

fn pad_slots_where<'a>(
    i: &'a egui::InputState,
    pred: impl Fn(&egui::InputState, Key) -> bool + 'a,
) -> impl Iterator<Item = u8> + 'a {
    let blocked = i.modifiers.ctrl || i.modifiers.command || i.modifiers.alt;
    PAD_KEYS.into_iter().enumerate().filter_map(move |(idx, k)| {
        if blocked || !pred(i, k) {
            None
        } else {
            Some(idx as u8)
        }
    })
}

pub fn show(
    ctx: &egui::Context,
    model: SamplerModel<'_>,
    view_start: &mut f32,
    view_len: &mut f32,
    pad_drag: &mut Option<(u8, PadEdge)>,
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
                    if ui
                        .button("Добавить звуки")
                        .on_hover_text("WAV или MP3 встанут в конец текущего сэмпла. Можно выбрать несколько.")
                        .clicked()
                    {
                        action = Some(SamplerAction::AddSounds);
                    }
                    if model.sample.is_some()
                        && ui
                            .button("Заменить")
                            .on_hover_text("Заменить весь сэмпл одним файлом")
                            .clicked()
                    {
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

            if !model.slices.is_empty() {
                ui.add_space(6.0);
                if let Some(slice_action) = show_slice_badges(ui, ctx, model.slices) {
                    action = Some(slice_action);
                }
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
                    egui::RichText::new("Перетащите WAV или MP3 сюда — или нажмите «Добавить звуки». Можно несколько файлов: каждый встанет после предыдущего.")
                        .weak()
                        .italics(),
                );
            } else {
                ui.label(
                    egui::RichText::new(
                        "Клик по waveform — базовая позиция. Стоп возвращает курсор к ней. «Добавить звуки» ставит файлы друг за другом и вешает каждый на свободный пэд. Бейджи между картой и дорожкой двигают файлы встык, × удаляет файл. Alt+скролл — горизонтально. Полоски — начало и конец сэмпла, тяни чтобы изменить длину.",
                    )
                    .weak()
                    .size(12.0),
                );
                if !model.status.is_empty() {
                    ui.label(egui::RichText::new(model.status).weak().size(12.0));
                }
            }
        });

    if !open {
        action = Some(SamplerAction::Close);
    }
    action
}

const SLICE_BADGE_H: f32 = 28.0;
const SLICE_BADGE_MIN_W: f32 = 108.0;

fn slice_drag_id() -> egui::Id {
    egui::Id::new("sampler_slice_drag")
}

fn slice_hover_id() -> egui::Id {
    egui::Id::new("sampler_slice_hover")
}

fn show_slice_badges(
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    slices: &[SampleSlice],
) -> Option<SamplerAction> {
    let n = slices.len();
    if n == 0 {
        return None;
    }
    let mut action = None;
    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), SLICE_BADGE_H), Sense::hover());
    let rects = badge_rects(row, slices);
    let pointer = ctx.input(|i| i.pointer.interact_pos());
    let dragging: Option<usize> = ctx.data(|d| d.get_temp(slice_drag_id()));
    let released = ctx.input(|i| i.pointer.any_released());
    let sticky: Option<usize> = ctx.data(|d| d.get_temp(slice_hover_id()));
    let hover = if let Some(from) = dragging.filter(|_| !released) {
        Some(from)
    } else {
        badge_under_pointer(&rects, pointer, sticky)
    };
    if let Some(index) = hover {
        ctx.data_mut(|d| d.insert_temp(slice_hover_id(), index));
    }
    let to = pointer
        .map(|pos| slice_index_at_x(slices, row, pos.x))
        .unwrap_or(0);
    let mut order: Vec<usize> = (0..n).filter(|&i| hover != Some(i)).collect();
    if let Some(index) = hover {
        order.push(index);
    }
    let mut deleted = false;
    for slice_i in order {
        let Some(slice) = slices.get(slice_i) else {
            continue;
        };
        let Some(badge) = rects.get(slice_i).copied() else {
            continue;
        };
        let on_top = hover == Some(slice_i);
        let dragging_this = dragging == Some(slice_i) && !released;
        let close_rect = Rect::from_min_size(
            Pos2::new(badge.right() - 22.0, badge.top()),
            Vec2::new(22.0, badge.height()),
        );
        let drag_rect = Rect::from_min_max(badge.min, Pos2::new(close_rect.left(), badge.bottom()));
        let drag_resp = ui.interact(drag_rect, egui::Id::new(("slice_drag", slice_i)), Sense::drag());
        let fill = if dragging_this || on_top {
            Color32::from_rgb(70, 110, 86)
        } else {
            Color32::from_rgb(48, 52, 64)
        };
        let stroke = if dragging_this || on_top {
            Stroke::new(1.5_f32, Color32::from_rgb(130, 220, 160))
        } else {
            Stroke::new(1.0_f32, Color32::from_gray(80))
        };
        ui.painter().rect(badge, 6.0, fill, stroke);
        ui.painter().text(
            Pos2::new(drag_rect.left() + 8.0, drag_rect.center().y),
            Align2::LEFT_CENTER,
            badge_label(&slice.label),
            FontId::proportional(12.0),
            Color32::from_gray(230),
        );
        if drag_resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if drag_resp.drag_started() && !released {
            ui.ctx()
                .data_mut(|d| d.insert_temp(slice_drag_id(), slice_i));
        }
        let close = ui
            .new_child(egui::UiBuilder::new().max_rect(close_rect))
            .small_button("×")
            .on_hover_text("Удалить этот файл");
        if close.clicked() {
            action = Some(SamplerAction::DeleteSlice { index: slice_i });
            deleted = true;
            ui.ctx().data_mut(|d| d.remove_temp::<usize>(slice_drag_id()));
        }
    }
    if released && !deleted {
        if let Some(from) = dragging {
            ctx.data_mut(|d| d.remove_temp::<usize>(slice_drag_id()));
            if to != from && from < n {
                action = Some(SamplerAction::ReorderSlice { from, to });
            }
        }
    }
    action
}

fn badge_rects(row: Rect, slices: &[SampleSlice]) -> Vec<Rect> {
    let total = slices
        .last()
        .map(|s| s.end_index.max(1))
        .unwrap_or(1) as f32;
    let width = row.width().max(1.0);
    slices
        .iter()
        .map(|slice| {
            let x0 = row.left() + slice.start_index as f32 / total * width;
            let x1 = row.left() + slice.end_index as f32 / total * width;
            let w = (x1 - x0).max(SLICE_BADGE_MIN_W).min(width);
            let left = x0.min(row.right() - w).max(row.left());
            Rect::from_min_size(Pos2::new(left, row.top()), Vec2::new(w, row.height()))
        })
        .collect()
}

fn badge_under_pointer(rects: &[Rect], pointer: Option<Pos2>, sticky: Option<usize>) -> Option<usize> {
    let pos = pointer?;
    if let Some(index) = sticky {
        if rects.get(index).is_some_and(|rect| rect.contains(pos)) {
            return Some(index);
        }
    }
    rects
        .iter()
        .enumerate()
        .rev()
        .find(|(_, rect)| rect.contains(pos))
        .map(|(index, _)| index)
}

fn slice_index_at_x(slices: &[SampleSlice], row: Rect, x: f32) -> usize {
    let total = slices
        .last()
        .map(|s| s.end_index.max(1))
        .unwrap_or(1) as f32;
    let idx = ((x - row.left()) / row.width().max(1.0) * total).floor();
    let idx = idx.clamp(0.0, total - 1.0) as usize;
    slices
        .iter()
        .position(|slice| idx < slice.end_index)
        .unwrap_or(slices.len().saturating_sub(1))
}

fn badge_label(label: &str) -> String {
    let mut chars = label.chars();
    let short: String = chars.by_ref().take(16).collect();
    if chars.next().is_some() {
        format!("{short}…")
    } else {
        short
    }
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

fn sample_at_x(x: f32, rect: Rect, view_start: f32, view_len: f32, n: f32, allow_end: bool) -> usize {
    let u = ((x - rect.left()) / rect.width().max(1.0)).clamp(0.0, 1.0);
    let idx = view_start + u * view_len.max(1.0);
    let max = if allow_end {
        n.max(0.0)
    } else {
        (n - 1.0).max(0.0)
    };
    idx.round().clamp(0.0, max) as usize
}

fn sample_to_x(index: usize, rect: Rect, view_start: f32, view_len: f32) -> f32 {
    let span = view_len.max(1.0);
    rect.left() + (index as f32 - view_start) / span * rect.width()
}

const MARKER_CHIP_W: f32 = 22.0;
const MARKER_CHIP_H: f32 = 18.0;
const MARKER_DELETE_W: f32 = 14.0;
const PAD_TRI: f32 = 24.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum PadHit {
    Start,
    End,
    Delete,
    Body,
}

fn start_triangle(x: f32, wave: Rect) -> [Pos2; 3] {
    let top = wave.top();
    [
        Pos2::new(x, top),
        Pos2::new(x - PAD_TRI, top),
        Pos2::new(x, top + PAD_TRI),
    ]
}

fn end_triangle(x: f32, wave: Rect) -> [Pos2; 3] {
    let bot = wave.bottom();
    [
        Pos2::new(x, bot),
        Pos2::new(x + PAD_TRI, bot),
        Pos2::new(x, bot - PAD_TRI),
    ]
}

fn pos_in_start_tri(pos: Pos2, x: f32, wave: Rect) -> bool {
    let dx = x - pos.x;
    let dy = pos.y - wave.top();
    dx >= -2.0 && dy >= -2.0 && dx + dy <= PAD_TRI + 2.0
}

fn pos_in_end_tri(pos: Pos2, x: f32, wave: Rect) -> bool {
    let dx = pos.x - x;
    let dy = wave.bottom() - pos.y;
    dx >= -2.0 && dy >= -2.0 && dx + dy <= PAD_TRI + 2.0
}

fn marker_chip_rect(x0: f32, x1: f32, wave: Rect, selected: bool) -> Rect {
    let w = if selected {
        MARKER_CHIP_W + MARKER_DELETE_W
    } else {
        MARKER_CHIP_W
    };
    let mid = (x0 + x1) * 0.5;
    let x = (mid - w * 0.5).clamp(wave.left() + 2.0, (wave.right() - w - 2.0).max(wave.left() + 2.0));
    Rect::from_min_size(
        Pos2::new(x, wave.top() + 4.0),
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
) -> Option<(u8, PadHit)> {
    let mut hits: Vec<(u8, PadHit, u8, f32)> = Vec::new();
    for m in markers {
        if (m.slot as usize) >= PAD_COUNT {
            continue;
        }
        let is_sel = selected == Some(m.slot);
        let x0 = sample_to_x(m.start_index, wave, view_start, view_len);
        let x1 = sample_to_x(m.end_index, wave, view_start, view_len);
        let left = x0.min(x1);
        let right = x0.max(x1);
        let chip = marker_chip_rect(x0, x1, wave, is_sel);
        let on_chip = chip.contains(pos);
        let on_delete = is_sel && on_chip && marker_delete_rect(chip).contains(pos);
        if on_delete {
            hits.push((m.slot, PadHit::Delete, 0, 0.0));
        }
        let on_start = pos_in_start_tri(pos, x0, wave);
        let on_end = pos_in_end_tri(pos, x1, wave);
        if on_start {
            hits.push((m.slot, PadHit::Start, 1, (x0 - pos.x).abs()));
        }
        if on_end {
            hits.push((m.slot, PadHit::End, 1, (x1 - pos.x).abs()));
        }
        let on_body = wave.contains(pos) && pos.x >= left && pos.x <= right;
        if on_body || (on_chip && !on_delete) {
            let center = (left + right) * 0.5;
            hits.push((m.slot, PadHit::Body, 2, (center - pos.x).abs()));
        }
    }
    hits.sort_by(|a, b| {
        a.2.cmp(&b.2)
            .then_with(|| a.3.partial_cmp(&b.3).unwrap_or(std::cmp::Ordering::Equal))
    });
    hits.first().map(|&(slot, hit, _, _)| (slot, hit))
}

fn handle_wave_markers(
    resp: &egui::Response,
    wave: Rect,
    model: &SamplerModel<'_>,
    view_start: f32,
    view_len: f32,
    n: f32,
    pad_drag: &mut Option<(u8, PadEdge)>,
) -> Option<SamplerAction> {
    let ctrl = resp.ctx.input(|i| i.modifiers.ctrl || i.modifiers.command);
    let pos = resp.interact_pointer_pos();

    if resp.hovered() {
        if let Some(hover) = resp.hover_pos() {
            if let Some((_, hit)) =
                hit_pad_marker(model.pad_markers, hover, wave, view_start, view_len, model.selected_pad)
            {
                if matches!(hit, PadHit::Start | PadHit::End) {
                    resp.ctx.set_cursor_icon(CursorIcon::ResizeHorizontal);
                }
            }
        }
    }

    if resp.drag_started_by(PointerButton::Primary) {
        if let Some(pos) = pos {
            if let Some((slot, hit)) =
                hit_pad_marker(model.pad_markers, pos, wave, view_start, view_len, model.selected_pad)
            {
                match hit {
                    PadHit::Start => {
                        *pad_drag = Some((slot, PadEdge::Start));
                        return Some(SamplerAction::SelectPad { slot: Some(slot) });
                    }
                    PadHit::End => {
                        *pad_drag = Some((slot, PadEdge::End));
                        return Some(SamplerAction::SelectPad { slot: Some(slot) });
                    }
                    PadHit::Body => {
                        return Some(SamplerAction::SelectPad { slot: Some(slot) });
                    }
                    PadHit::Delete => {}
                }
            }
        }
    }

    if let Some((slot, edge)) = *pad_drag {
        if resp.dragged_by(PointerButton::Primary) {
            if let Some(pos) = pos {
                let idx = sample_at_x(pos.x, wave, view_start, view_len, n, edge == PadEdge::End);
                return Some(SamplerAction::MovePad {
                    slot,
                    edge,
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
        if let Some((slot, hit)) =
            hit_pad_marker(model.pad_markers, pos, wave, view_start, view_len, model.selected_pad)
        {
            if hit == PadHit::Delete {
                *pad_drag = None;
                return Some(SamplerAction::DeletePad { slot });
            }
            return Some(SamplerAction::SelectPad { slot: Some(slot) });
        }
        let idx = sample_at_x(pos.x, wave, view_start, view_len, n, false);
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
            let fill = Color32::from_rgba_unmultiplied(230, 180, 64, 40);
            for m in model.pad_markers {
                let x0 = rect.left() + (m.start_index as f32 / n) * rect.width();
                let x1 = rect.left() + (m.end_index as f32 / n) * rect.width();
                let band = Rect::from_min_max(
                    Pos2::new(x0.min(x1), rect.top() + 2.0),
                    Pos2::new(x0.max(x1), rect.bottom() - 2.0),
                );
                if band.width() > 0.5 {
                    painter.rect_filled(band, 0.0, fill);
                }
                painter.line_segment(
                    [Pos2::new(x0, rect.top() + 2.0), Pos2::new(x0, rect.bottom() - 2.0)],
                    Stroke::new(1.0_f32, col),
                );
                painter.line_segment(
                    [Pos2::new(x1, rect.top() + 2.0), Pos2::new(x1, rect.bottom() - 2.0)],
                    Stroke::new(1.0_f32, col),
                );
            }
        }
            paint_cursors(&painter, rect, 0.0, n, model);
            paint_slice_boundaries(&painter, rect, model.slices, 0.0, n, false);
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
        paint_slice_boundaries(&painter, rect, model.slices, view_start, view_len, true);
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

fn paint_slice_boundaries(
    painter: &egui::Painter,
    rect: Rect,
    slices: &[SampleSlice],
    view_start: f32,
    view_len: f32,
    labels: bool,
) {
    if slices.len() < 2 || view_len <= 0.0 {
        return;
    }
    let col = Color32::from_gray(170);
    let font = FontId::proportional(11.0);
    let span_end = view_start + view_len;
    for slice in slices {
        let start_f = slice.start_index as f32;
        if start_f <= view_start || start_f > span_end {
            continue;
        }
        let x = rect.left() + (start_f - view_start) / view_len * rect.width();
        painter.line_segment(
            [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
            Stroke::new(1.0_f32, col),
        );
        if labels {
            painter.text(
                Pos2::new(x + 4.0, rect.top() + 4.0),
                Align2::LEFT_TOP,
                &slice.label,
                font.clone(),
                col,
            );
        }
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
        let start_f = m.start_index as f32;
        let end_f = m.end_index as f32;
        if end_f < view_start - 1.0 || start_f > span_end + 1.0 {
            continue;
        }
        let selected = model.selected_pad == Some(m.slot);
        let active = model.active_pad == Some(m.slot) && model.preview_playing;
        let x0 = sample_to_x(m.start_index, rect, view_start, view_len);
        let x1 = sample_to_x(m.end_index, rect, view_start, view_len);
        let fill = if active {
            Color32::from_rgba_unmultiplied(52, 140, 92, 55)
        } else if selected {
            Color32::from_rgba_unmultiplied(230, 180, 64, 55)
        } else {
            Color32::from_rgba_unmultiplied(230, 180, 64, 28)
        };
        let fill_rect = Rect::from_min_max(
            Pos2::new(x0.min(x1).max(rect.left()), rect.top()),
            Pos2::new(x0.max(x1).min(rect.right()), rect.bottom()),
        );
        if fill_rect.width() > 0.5 {
            painter.rect_filled(fill_rect, 0.0, fill);
        }
        let stroke = Stroke::new(if selected { 2.5_f32 } else { 1.5_f32 }, col);
        painter.line_segment(
            [Pos2::new(x0, rect.top()), Pos2::new(x0, rect.bottom())],
            stroke,
        );
        painter.line_segment(
            [Pos2::new(x1, rect.top()), Pos2::new(x1, rect.bottom())],
            stroke,
        );
        painter.add(Shape::convex_polygon(
            start_triangle(x0, rect).to_vec(),
            col,
            Stroke::NONE,
        ));
        painter.add(Shape::convex_polygon(
            end_triangle(x1, rect).to_vec(),
            col,
            Stroke::NONE,
        ));
        let chip = marker_chip_rect(x0, x1, rect, selected);
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
                        "Пэд {} — воспроизвести сэмпл",
                        PAD_LABELS[slot]
                    ));
                } else {
                    resp.clone().on_hover_text(format!(
                        "Пэд {} — сэмпл от курсора",
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
