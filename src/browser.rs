//! In-app loader: browse folders and audition files before adding them.

use std::path::{Path, PathBuf};

use egui::{Color32, RichText, ScrollArea, Vec2};

pub struct LoadBrowser {
    pub append: bool,
    pub dir: PathBuf,
    pub rows: Vec<BrowserRow>,
    pub selected: Option<usize>,
    /// Path of the file currently auditioned, if any.
    pub playing: Option<PathBuf>,
}

pub enum BrowserRow {
    Dir { name: String, path: PathBuf },
    Sound { name: String, path: PathBuf },
}

pub enum BrowserAction {
    Close,
    PlayFile(PathBuf),
    Stop,
    AddFile(PathBuf),
}

impl LoadBrowser {
    pub fn open(append: bool, library_dir: Option<&Path>) -> Self {
        let dir = library_dir
            .filter(|p| p.is_dir())
            .map(Path::to_path_buf)
            .unwrap_or_else(default_audio_dir);
        let mut browser = Self {
            append,
            dir: dir.clone(),
            rows: Vec::new(),
            selected: None,
            playing: None,
        };
        browser.reload();
        browser
    }

    pub fn reload(&mut self) {
        self.rows = read_dir_rows(&self.dir);
        self.selected = None;
    }

    pub fn go_up(&mut self) {
        if let Some(parent) = self.dir.parent() {
            if parent != self.dir {
                self.dir = parent.to_path_buf();
                self.reload();
            }
        }
    }

    pub fn enter(&mut self, path: PathBuf) {
        if path.is_dir() {
            self.dir = path;
            self.reload();
        }
    }
}

pub fn show(ctx: &egui::Context, browser: &mut LoadBrowser) -> Option<BrowserAction> {
    let mut action = None;
    let append = browser.append;
    let mut open = true;
    let title = if append {
        "Загрузка · добавить звуки"
    } else {
        "Загрузка · заменить сэмпл"
    };
    egui::Window::new(title)
        .id(egui::Id::new("load_browser"))
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .fixed_size([640.0, 460.0])
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Вверх").clicked() {
                    browser.go_up();
                }
                ui.label(
                    RichText::new(browser.dir.display().to_string())
                        .monospace()
                        .size(12.0),
                );
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(if append {
                    "Звуки встанут в конец текущего сэмпла. ▶ — прослушать."
                } else {
                    "Выбранный файл заменит сэмпл целиком. ▶ — прослушать."
                })
                .weak()
                .size(12.0),
            );
            ui.add_space(6.0);
            ScrollArea::vertical()
                .id_salt("browser_files")
                .max_height(360.0)
                .show(ui, |ui| {
                    if browser.rows.is_empty() {
                        ui.label(RichText::new("В этой папке нет WAV и MP3").weak());
                    }
                    let rows: Vec<(bool, String, PathBuf)> = browser
                        .rows
                        .iter()
                        .map(|row| match row {
                            BrowserRow::Dir { name, path } => (true, name.clone(), path.clone()),
                            BrowserRow::Sound { name, path } => (false, name.clone(), path.clone()),
                        })
                        .collect();
                    let mut enter = None;
                    for (i, (is_dir, name, path)) in rows.into_iter().enumerate() {
                        ui.horizontal(|ui| {
                            let selected = browser.selected == Some(i);
                            if is_dir {
                                if ui.selectable_label(selected, format!("📁 {name}")).clicked() {
                                    browser.selected = Some(i);
                                    enter = Some(path);
                                }
                            } else {
                                let playing = browser.playing.as_ref() == Some(&path);
                                let mut label = RichText::new(&name);
                                if playing {
                                    label = label.color(Color32::from_rgb(130, 220, 160)).strong();
                                }
                                if ui.selectable_label(selected, label).clicked() {
                                    browser.selected = Some(i);
                                }
                                if ui
                                    .add_sized(
                                        [28.0, 20.0],
                                        egui::Button::new(if playing { "⏹" } else { "▶" }),
                                    )
                                    .on_hover_text(if playing {
                                        "Стоп"
                                    } else {
                                        "Проиграть звук"
                                    })
                                    .clicked()
                                {
                                    action = Some(if playing {
                                        BrowserAction::Stop
                                    } else {
                                        BrowserAction::PlayFile(path.clone())
                                    });
                                }
                                let add = if append { "Добавить" } else { "Заменить" };
                                if ui.small_button(add).clicked() {
                                    action = Some(BrowserAction::AddFile(path));
                                }
                            }
                        });
                    }
                    if let Some(path) = enter {
                        browser.enter(path);
                    }
                });
        });

    if !open {
        action = Some(BrowserAction::Close);
    }
    action
}

pub(crate) fn default_audio_dir() -> PathBuf {
    if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        let home = PathBuf::from(home);
        let music = home.join("Music");
        if music.is_dir() {
            return music;
        }
        if home.is_dir() {
            return home;
        }
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn read_dir_rows(dir: &Path) -> Vec<BrowserRow> {
    let mut dirs = Vec::new();
    let mut sounds = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    for entry in rd.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        if path.is_dir() {
            dirs.push(name);
        } else if is_audio(&path) {
            sounds.push(name);
        }
    }
    dirs.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    sounds.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
    let mut rows = Vec::new();
    for name in dirs {
        let path = dir.join(&name);
        rows.push(BrowserRow::Dir { name, path });
    }
    for name in sounds {
        let path = dir.join(&name);
        rows.push(BrowserRow::Sound { name, path });
    }
    rows
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            e == "wav" || e == "mp3"
        })
        .unwrap_or(false)
}
