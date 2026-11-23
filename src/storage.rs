use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::auto_theme::Mood;

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

fn default_peak_hold() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedState {
    #[serde(default)]
    pub playlist: Vec<String>,
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// 0 sequence, 1 repeat one, 2 shuffle.
    #[serde(default)]
    pub play_mode: u8,
    #[serde(default)]
    pub light: bool,
    /// Built-in index from before themes were saved by name; still written
    /// for built-ins so older builds keep the choice.
    #[serde(default)]
    pub theme: Option<usize>,
    #[serde(default)]
    pub theme_name: Option<String>,
    /// AUTO theme: `Some(true)` dark, `Some(false)` light, `None` off. The
    /// manual `theme_name` is kept so leaving AUTO restores it.
    #[serde(default)]
    pub auto_theme: Option<bool>,
    /// Mood the AUTO look was last generated from, shown at startup until
    /// the current track is analysed.
    #[serde(default)]
    pub auto_mood: Option<Mood>,
    #[serde(default = "default_true")]
    pub show_bg_spectrum: bool,
    #[serde(default)]
    pub braille_stream: bool,
    #[serde(default)]
    pub led_bars: bool,
    #[serde(default = "default_true")]
    pub show_radial: bool,
    #[serde(default = "default_true")]
    pub show_core_pulse: bool,
    #[serde(default = "default_radial_scale")]
    pub radial_scale: f32,
    #[serde(default = "default_true")]
    pub eq_enabled: bool,
    #[serde(default = "default_eq_gains")]
    pub eq_gains: Vec<f32>,
    #[serde(default = "default_eq_preset")]
    pub eq_preset: usize,
    #[serde(default)]
    pub lrc_map: HashMap<String, String>,
    #[serde(default = "default_peak_hold")]
    pub peak_hold: bool,
    /// Peak hold for the pixel (dot matrix) style; separate so it can default
    /// to off there while the smooth style keeps its markers.
    #[serde(default)]
    pub peak_hold_dots: bool,
    #[serde(default = "default_true")]
    pub show_lyrics: bool,
    #[serde(default)]
    pub piano_mode: bool,
    /// HTTP/SOCKS proxy for the Bilibili downloader, e.g.
    /// `http://127.0.0.1:7890`. Empty means direct. A proxy also takes over
    /// DNS, which is the point: the CONNECT carries the host name.
    #[serde(default)]
    pub proxy: String,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            playlist: Vec::new(),
            current: None,
            volume: default_volume(),
            play_mode: 0,
            light: false,
            theme: None,
            theme_name: None,
            auto_theme: None,
            auto_mood: None,
            show_bg_spectrum: true,
            braille_stream: false,
            led_bars: false,
            show_radial: true,
            show_core_pulse: true,
            radial_scale: default_radial_scale(),
            eq_enabled: true,
            eq_gains: default_eq_gains(),
            eq_preset: default_eq_preset(),
            lrc_map: HashMap::new(),
            peak_hold: default_peak_hold(),
            peak_hold_dots: false,
            show_lyrics: true,
            piano_mode: false,
            proxy: String::new(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("SonadialPlayer")
}

fn legacy_data_dir() -> PathBuf {
    // Read-only fallback for playlists created before the product was renamed
    // to Sonadial Player. New state is always written to SonadialPlayer.
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
