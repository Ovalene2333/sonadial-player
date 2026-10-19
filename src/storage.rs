use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn default_volume() -> f32 {
    0.78
}

fn default_true() -> bool {
    true
}

fn default_radial_scale() -> f32 {
    1.0
}

fn default_eq_gains() -> Vec<f32> {
    vec![0.0; 10]
}

fn default_eq_preset() -> usize {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default)]
    pub playlist: Vec<String>,
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default = "default_volume")]
    pub volume: f32,
    #[serde(default)]
    pub light: bool,
    #[serde(default)]
    pub theme: Option<usize>,
    #[serde(default = "default_true")]
    pub show_bg_spectrum: bool,
    #[serde(default)]
    pub char_stream: bool,
    #[serde(default = "default_true")]
    pub show_radial: bool,
    #[serde(default = "default_true")]
    pub show_core_pulse: bool,
    #[serde(default = "default_radial_scale")]
    pub radial_scale: f32,
    #[serde(default)]
    pub track_volumes: HashMap<String, f32>,
    #[serde(default = "default_true")]
    pub eq_enabled: bool,
    #[serde(default = "default_eq_gains")]
    pub eq_gains: Vec<f32>,
    #[serde(default = "default_eq_preset")]
    pub eq_preset: usize,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            playlist: Vec::new(),
            current: None,
            volume: default_volume(),
            light: false,
            theme: None,
            show_bg_spectrum: true,
            char_stream: false,
            show_radial: true,
            show_core_pulse: true,
            radial_scale: default_radial_scale(),
            track_volumes: HashMap::new(),
            eq_enabled: true,
            eq_gains: default_eq_gains(),
            eq_preset: default_eq_preset(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SonadialPlayer")
}

fn legacy_data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("NeonPlayerRS")
}

pub fn state_path() -> PathBuf {
    data_dir().join("state.json")
}

pub fn download_dir() -> PathBuf {
    dirs::audio_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join("Music")))
        .unwrap_or_else(data_dir)
        .join("SonadialPlayer")
}

pub fn load() -> PersistedState {
    let text = fs::read_to_string(state_path())
        .ok()
        .or_else(|| fs::read_to_string(legacy_data_dir().join("state.json")).ok());
    text.and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

pub fn save(state: &PersistedState) {
    let dir = data_dir();
    let _ = fs::create_dir_all(&dir);
    if let Ok(text) = serde_json::to_string_pretty(state) {
        let _ = fs::write(dir.join("state.json"), text);
    }
}
