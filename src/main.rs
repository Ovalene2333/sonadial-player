#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod app;
mod audio;
mod beat;
mod beat_config;
mod decode;
mod design;
mod download;
mod eq;
mod lyrics;
mod notes;
mod pitch;
mod spectrum;
mod storage;
mod theme;
mod viz;

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use eframe::egui::ViewportBuilder;

fn main() -> eframe::Result {
    install_panic_log();

    let initial = std::env::args().nth(1).map(PathBuf::from);
    let options = eframe::NativeOptions {
        viewport: ViewportBuilder::default()
            .with_inner_size([1180.0, 760.0])
            .with_min_inner_size([880.0, 560.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_title("Sonadial Player"),
        renderer: eframe::Renderer::Glow,
        ..Default::default()
    };

    eframe::run_native(
        "Sonadial Player",
        options,
        Box::new(move |cc| Ok(Box::new(app::SonadialApp::new(cc, initial.clone())))),
    )
}

fn install_panic_log() {
    let fallback = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let dir = dirs::config_dir()
        .map(|dir| dir.join("SonadialPlayer"))
        .unwrap_or(fallback);
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::fs::create_dir_all(&dir);
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("crash.log"))
        {
            let _ = writeln!(file, "[{stamp}] {info}");
        }
    }));
}
