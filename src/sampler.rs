//! Modal sampling instrument: preview, pitch, tempo, overview map, main waveform.

use egui::{Align2, Color32, PointerButton, Pos2, Rect, Sense, Stroke, Vec2};

use crate::theme;
use crate::waveform::{self, PeakPyramid};

pub struct SamplerModel<'a> {
    pub track_name: &'a str,
    pub pitch_semitones: i32,
    pub tempo_bpm: f32,
    pub sample: Option<(&'a [f32], &'a PeakPyramid)>,
    pub preview_playing: bool,
    pub preview_secs: f32,
    pub sample_rate: u32,
    pub speed: f32,
    pub status: &'a str,
}

pub enum SamplerAction {
    Close,
    TogglePreview,
    PitchDelta(i32),
    TempoDelta(f32),
    LoadFile,
}

pub fn show(
    ctx: &egui::Context,
    model: SamplerModel<'_>,
    view_start: &mut f32,
    view_len: &mut f32,
) -> Option<SamplerAction> {
    let mut action = None;
    let screen = ctx.screen_rect();
    egui::Area::new(egui::Id::new("sampler_dim"))
        .order(egui::Order::Foreground)
        .fixed_pos(screen.min)
        .interactable(true)
        .show(ctx, |ui| {
            ui.allocate_rect(screen, Sense::click());
            ui.painter()
                .rect_filled(screen, 0.0, Color32::from_black_alpha(170));
        });

    let mut open = true;
    egui::Window::new("Инструмент сэмплинга")
        .id(egui::Id::new("sampling_instrument"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size([960.0, 500.0])
        .min_width(760.0)
        .min_height(460.0)
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
                handle_map_nav(&map_resp, map_rect, view_start, view_len, sample_n);
            }

            ui.add_space(8.0);
            let wave_h = theme::SAMPLER_WAVE_H.min(ui.available_height().max(120.0));
            let (wave_rect, wave_resp) =
                ui.allocate_exact_size(Vec2::new(ui.available_width(), wave_h), Sense::hover());
            paint_main_wave(ui, wave_rect, &model, *view_start, *view_len, sample_n);
            if sample_n > 1.0 {
                handle_wave_zoom(ctx, &wave_resp, wave_rect, view_start, view_len, sample_n);
            }

            ui.add_space(6.0);
            if model.sample.is_none() {
                ui.label(
                    egui::RichText::new("Перетащите WAV или MP3 сюда — или нажмите «Загрузить».")
                        .weak()
                        .italics(),
                );
            } else if !model.status.is_empty() {
                ui.label(egui::RichText::new(model.status).weak().size(12.0));
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
}

fn handle_wave_zoom(
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
    let (ctrl, dy) = ctx.input(|i| {
        (
            i.modifiers.ctrl || i.modifiers.command,
            i.smooth_scroll_delta.y + i.raw_scroll_delta.y,
        )
    });
    if !ctrl || dy.abs() < 0.01 {
        return;
    }
    let pos = ctx.pointer_hover_pos().unwrap_or(wave.center());
    let u = ((pos.x - wave.left()) / wave.width().max(1.0)).clamp(0.0, 1.0);
    let anchor = *start + u * *len;
    let new_len = (*len * (1.0 - dy * 0.004)).clamp(8.0, n);
    *len = new_len;
    *start = (anchor - u * new_len).clamp(0.0, (n - new_len).max(0.0));
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
        }
        paint_preview_line(&painter, rect, 0.0, n, model);
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "карта сэмпла",
            egui::FontId::proportional(13.0),
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
        paint_preview_line(&painter, rect, view_start, view_len.max(1.0), model);
        let _ = n;
    } else {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "waveform сэмпла",
            egui::FontId::proportional(16.0),
            Color32::from_gray(120),
        );
    }
}

fn paint_preview_line(
    painter: &egui::Painter,
    rect: Rect,
    view_start: f32,
    view_span: f32,
    model: &SamplerModel<'_>,
) {
    if !model.preview_playing || view_span <= 0.0 {
        return;
    }
    let idx = model.preview_secs * model.sample_rate as f32 * model.speed.max(0.05);
    if idx < view_start || idx > view_start + view_span {
        return;
    }
    let x = rect.left() + (idx - view_start) / view_span * rect.width();
    painter.line_segment(
        [Pos2::new(x, rect.top()), Pos2::new(x, rect.bottom())],
        Stroke::new(2.0_f32, theme::color_playhead()),
    );
}
