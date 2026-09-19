//! Tiny sampler desktop prototype: WAV/MP3 → timeline → cpal (no rodio).

mod app;
mod audio;
mod library;
mod mix;
mod model;
mod persist;
mod pianoroll;
mod project_actions;
mod sampler;
mod theme;
mod timeline;
mod waveform;
mod wav_loader;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--test-tone") {
        println!("Playing 440 Hz test tone for 1.5 s (step 0)…");
        if let Err(e) = audio::play_test_tone_blocking(1.5) {
            eprintln!("{e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    // Do not call `with_maximized(true)` here. On Windows eframe always applies
    // `inner_size` *after* window creation (`apply_viewport_builder_to_window`).
    // That `SetWindowPos` shrinks the HWND while leaving WS_MAXIMIZE / winit's
    // MAXIMIZED flag set, so the caption button shows "restore" but the window
    // stays at 960×640. Later `Maximized(true)` is a no-op because the flag is
    // already true. Maximize is requested from the first UI frames instead.
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_maximized(false)
            .with_title("tinysampler")
            .with_drag_and_drop(true),
        ..Default::default()
    };

    eframe::run_native(
        "tinysampler",
        native_options,
        Box::new(|cc| Ok(Box::new(app::TinySamplerApp::new(cc)?))),
    )
}
