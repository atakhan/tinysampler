//! Tiny sampler desktop prototype: WAV/MP3 → timeline → cpal (no rodio).

mod app;
mod audio;
mod mix;
mod model;
mod project_actions;
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

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
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
