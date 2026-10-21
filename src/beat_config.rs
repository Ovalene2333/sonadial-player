use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Deserializer, Serialize};

use crate::storage;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShortVisual {
    Flash,
    Ring,
}

impl Default for ShortVisual {
    fn default() -> Self {
        Self::Flash
    }
}

impl<'de> Deserialize<'de> for ShortVisual {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.to_ascii_lowercase().as_str() {
            "ring" => Self::Ring,
            _ => Self::Flash,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct BeatConfig {
    pub heavy_min_interval_ms: f32,
    pub heavy_pair_window_ms: f32,
    pub heavy_short_interval_ms: f32,
    pub heavy_burst_cooldown_ms: f32,
    pub heavy_burst_max: u32,
    pub quick_strength_min: f32,
    pub quick_strength_max: f32,
    pub quick_min_ratio: f32,
    pub soft_min_interval_ms: f32,
    pub soft_merge_ms: f32,
    pub soft_strength_min: f32,
    pub soft_strength_max: f32,
    pub heavy_strength_min: f32,
    pub heavy_strength_max: f32,
    pub low_share_min: f32,
    pub sensitivity: f32,
    pub heavy_ring_threshold: f32,
    pub short_visual: ShortVisual,
    pub short_ring_strength: f32,
}

impl Default for BeatConfig {
    fn default() -> Self {
        Self {
            heavy_min_interval_ms: 225.0,
            heavy_pair_window_ms: 240.0,
            heavy_short_interval_ms: 100.0,
            heavy_burst_cooldown_ms: 350.0,
            heavy_burst_max: 2,
            quick_strength_min: 0.36,
            quick_strength_max: 0.46,
            quick_min_ratio: 0.5,
            soft_min_interval_ms: 260.0,
            soft_merge_ms: 120.0,
            soft_strength_min: 0.18,
            soft_strength_max: 0.34,
            heavy_strength_min: 0.62,
            heavy_strength_max: 1.0,
            low_share_min: 0.45,
            sensitivity: 0.42,
            heavy_ring_threshold: 0.5,
            short_visual: ShortVisual::Flash,
            short_ring_strength: 0.55,
        }
    }
}

impl BeatConfig {
    pub fn path() -> PathBuf {
        storage::data_dir().join("beat.json")
    }

    pub fn load() -> Self {
        let path = Self::path();
        if !path.is_file() {
            let config = Self::default();
            config.write_default(&path);
            return config;
        }
        let mut config = fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str::<Self>(text.trim_start_matches('\u{feff}')).ok())
            .unwrap_or_default();
        config.validate();
        config
    }

    pub fn analysis_fingerprint(&self) -> u64 {
        let mut hash = 0xcbf2_9ce4_8422_2325_u64;
        let mut mix = |value: u64| {
            hash ^= value;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        };
        for value in [
            self.heavy_min_interval_ms,
            self.heavy_pair_window_ms,
            self.heavy_short_interval_ms,
            self.heavy_burst_cooldown_ms,
            self.quick_strength_min,
            self.quick_strength_max,
            self.quick_min_ratio,
            self.soft_min_interval_ms,
            self.soft_merge_ms,
            self.soft_strength_min,
            self.soft_strength_max,
            self.heavy_strength_min,
            self.heavy_strength_max,
            self.low_share_min,
            self.sensitivity,
        ] {
            mix(value.to_bits() as u64);
        }
        mix(self.heavy_burst_max as u64);
        hash
    }

    fn write_default(&self, path: &PathBuf) {
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = fs::write(path, text);
        }
    }

    fn validate(&mut self) {
        self.heavy_min_interval_ms = clamp(self.heavy_min_interval_ms, 20.0, 5000.0, 225.0);
        self.heavy_short_interval_ms = clamp(self.heavy_short_interval_ms, 20.0, 5000.0, 100.0);
        self.heavy_pair_window_ms =
            clamp(self.heavy_pair_window_ms, 20.0, 5000.0, 240.0).max(self.heavy_short_interval_ms);
        self.heavy_burst_cooldown_ms = clamp(self.heavy_burst_cooldown_ms, 20.0, 5000.0, 350.0);
        self.heavy_burst_max = self.heavy_burst_max.min(8);
        self.quick_strength_min = clamp(self.quick_strength_min, 0.0, 1.0, 0.36);
        self.quick_strength_max =
            clamp(self.quick_strength_max, 0.0, 1.0, 0.46).max(self.quick_strength_min);
        self.quick_min_ratio = clamp(self.quick_min_ratio, 0.0, 1.0, 0.5);
        self.soft_min_interval_ms = clamp(self.soft_min_interval_ms, 20.0, 5000.0, 260.0);
        self.soft_merge_ms = clamp(self.soft_merge_ms, 0.0, 2000.0, 120.0);
        self.soft_strength_min = clamp(self.soft_strength_min, 0.0, 1.0, 0.18);
        self.soft_strength_max =
            clamp(self.soft_strength_max, 0.0, 1.0, 0.34).max(self.soft_strength_min);
        self.heavy_strength_min = clamp(self.heavy_strength_min, 0.0, 1.0, 0.62);
        self.heavy_strength_max =
            clamp(self.heavy_strength_max, 0.0, 1.0, 1.0).max(self.heavy_strength_min);
        self.low_share_min = clamp(self.low_share_min, 0.0, 1.0, 0.45);
        self.sensitivity = clamp(self.sensitivity, 0.05, 0.95, 0.42);
        self.heavy_ring_threshold = clamp(self.heavy_ring_threshold, 0.0, 1.0, 0.5);
        self.short_ring_strength = clamp(self.short_ring_strength, 0.0, 1.0, 0.55);
    }
}

fn clamp(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_partial_config_and_visual_names() {
        let config: BeatConfig = serde_json::from_str(
            r#"{"_help": "ignored", "short_visual": "Ring", "heavy_burst_max": 1}"#,
        )
        .expect("partial config should parse");
        assert_eq!(config.short_visual, ShortVisual::Ring);
        assert_eq!(config.heavy_burst_max, 1);
        assert_eq!(config.heavy_min_interval_ms, 225.0);
    }

    #[test]
    fn visual_only_changes_keep_analysis_fingerprint() {
        let mut config = BeatConfig::default();
        let fingerprint = config.analysis_fingerprint();
        config.short_visual = ShortVisual::Ring;
        config.short_ring_strength = 0.9;
        config.heavy_ring_threshold = 0.4;
        assert_eq!(config.analysis_fingerprint(), fingerprint);

        config.heavy_min_interval_ms = 230.0;
        assert_ne!(config.analysis_fingerprint(), fingerprint);
    }
}
