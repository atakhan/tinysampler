//! Home screen: list of projects and “new project”.

use egui::{Color32, RichText, Sense, Vec2};

use crate::persist::LibraryMeta;
use crate::theme;

pub enum LibraryAction {
    Create,
    Open(u64),
}

pub fn show(
    ctx: &egui::Context,
    items: &[LibraryMeta],
    data_dir: &str,
    status: &str,
) -> Option<LibraryAction> {
    let mut action = None;
    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(theme::color_timeline_bg()).inner_margin(28.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading(RichText::new("Проекты").size(28.0).color(Color32::from_gray(235)));
                ui.add_space(16.0);
                let create = ui.add(
                    egui::Button::new(RichText::new("+ Новый проект").size(16.0).color(Color32::WHITE))
                        .min_size(Vec2::new(160.0, 36.0))
                        .fill(theme::color_transport_play()),
                );
                if create.clicked() {
                    action = Some(LibraryAction::Create);
                }
            });
            ui.add_space(8.0);
            ui.label(
                RichText::new("Откройте проект, чтобы попасть в студию.")
                    .weak()
                    .size(14.0),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!("Папка: {data_dir}"))
                    .weak()
                    .size(12.0)
                    .italics(),
            );
            if !status.is_empty() {
                ui.add_space(4.0);
                ui.label(RichText::new(status).size(13.0).color(Color32::from_rgb(210, 180, 90)));
            }
            ui.add_space(20.0);

            if items.is_empty() {
                ui.add_space(48.0);
                ui.vertical_centered(|ui| {
                    ui.label(
                        RichText::new("Пока нет проектов")
                            .size(18.0)
                            .color(Color32::from_gray(160)),
                    );
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("Создайте первый, чтобы начать сэмплировать.")
                            .weak()
                            .size(14.0),
                    );
                });
                return;
            }

            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = Vec2::splat(16.0);
                    for item in items {
                        let card = egui::Frame::none()
                            .fill(theme::color_timeline_bg_alt())
                            .stroke(egui::Stroke::new(1.0_f32, theme::color_timeline_border()))
                            .rounding(8.0)
                            .inner_margin(16.0)
                            .show(ui, |ui| {
                                ui.set_min_size(Vec2::new(220.0, 96.0));
                                ui.allocate_ui_with_layout(
                                    Vec2::new(220.0, 96.0),
                                    egui::Layout::top_down(egui::Align::LEFT),
                                    |ui| {
                                        ui.label(
                                            RichText::new(&item.name)
                                                .size(18.0)
                                                .color(Color32::WHITE),
                                        );
                                        ui.add_space(8.0);
                                        let tracks = if item.track_count == 0 {
                                            "Нет дорожек".to_string()
                                        } else {
                                            format!("Дорожек: {}", item.track_count)
                                        };
                                        ui.label(RichText::new(tracks).weak().size(13.0));
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::BOTTOM),
                                            |ui| {
                                                ui.label(
                                                    RichText::new("Открыть →")
                                                        .size(12.0)
                                                        .color(theme::color_ruler_text()),
                                                );
                                            },
                                        );
                                    },
                                );
                            });
                        let resp = ui.interact(
                            card.response.rect,
                            ui.id().with(("lib_card", item.id)),
                            Sense::click(),
                        );
                        if resp.hovered() {
                            ui.painter().rect_stroke(
                                card.response.rect,
                                8.0,
                                egui::Stroke::new(1.5_f32, theme::color_track_gutter_selected()),
                            );
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            action = Some(LibraryAction::Open(item.id));
                        }
                    }
                });
            });
        });

    action
}
