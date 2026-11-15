#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

mod audio;
mod auto_theme;
mod beat;
mod beat_config;
mod cache_store;
mod decode;
mod design;
mod download;
mod eq;
mod lyrics;
mod notes;
mod pitch;
mod search;
mod song_type;
mod sound_profile;
mod spectrum;
mod storage;
mod theme;
mod theme_library;
mod ui;

use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() {
    install_panic_log();

    // Software rendering is the default on this branch (the whole point: the
    // GPU driver stack is ~100 MB of the old build's footprint). `--gpu`
    // switches to Slint's femtovg renderer for comparison.
    if !std::env::args().any(|arg| arg == "--gpu") {
        std::env::set_var("SLINT_BACKEND", "winit-software");
    }

    let initial = std::env::args()
        .nth(1)
        .filter(|arg| !arg.starts_with("--"))
        .map(PathBuf::from);
    ui::run(initial);
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
