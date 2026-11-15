//! Offline beat analysis for the core pulse.
//!
//! The track is streamed through a 22.05kHz mono decoder in small chunks, so
//! analysis never holds a decoded copy of the whole song. Heavy beats are
//! picked from the low band first, and mid-band transients provide the sparse
//! light pulses. Results are cached by path, size and mtime so replaying a
//! track is instant.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use crate::beat_config::BeatConfig;
use crate::cache_store;
use crate::sound_profile::{BandCurves, ProfileFrames, SoundProfile};
use crate::storage;

const ANALYSIS_RATE: u32 = 22_050;
const FRAME_SIZE: usize = 1024;
const HOP_SIZE: usize = 256;
const LOW_CUT_HZ: f32 = 28.0;
const LOW_HIGH_HZ: f32 = 210.0;
const WIDE_HIGH_HZ: f32 = 8_000.0;
const SOFT_HIGH_HZ: f32 = 2_000.0;
// Bump when the offline detector changes; old cache entries are ignored.
// 13 added the sound profile, 14 widened it for the song type.
const ANALYSIS_VERSION: u32 = 14;
const FRAME_CENTER_SECONDS: f64 = 0.5 * FRAME_SIZE as f64 / ANALYSIS_RATE as f64;

#[derive(Clone, Debug, Default)]
pub struct BeatAnalysis {
    pub beats: Vec<(f64, f32)>,
    pub bpm: f32,
    pub profile: Option<SoundProfile>,
}

struct BandCurve {
    relative: Vec<f32>,
    curve: Vec<f32>,
}

#[derive(Clone, Copy)]
struct PickedPeak {
    frame: usize,
    quick: bool,
}

#[derive(Clone, Copy)]
struct FileSignature {
    size: u64,
    mtime_ms: u64,
}

/// Legacy single-file cache layout; only parsed once during migration, never
/// written anymore.
#[derive(Serialize, Deserialize, Default)]
struct BeatCacheFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    tracks: HashMap<String, BeatCacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct BeatCacheEntry {
    #[serde(default)]
    version: u32,
    size: u64,
    mtime_ms: u64,
    #[serde(default)]
    saved_at: u64,
    #[serde(default)]
    config_hash: u64,
    beats: Vec<(f64, f32)>,
    #[serde(default)]
    bpm: f32,
    #[serde(default)]
    profile: Option<SoundProfile>,
}

static CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

/// Analyze a track in the background. Results are cached by path, size and
/// mtime so replaying a track does not decode it again.
pub fn analyze_file(path: &Path, cancel: &AtomicBool, config: &BeatConfig) -> Result<BeatAnalysis> {
    let config_hash = config.analysis_fingerprint();
    let signature = file_signature(path);
    if let Some(entry) = signature.and_then(|sig| load_cache_entry(path, sig, config_hash)) {
        return Ok(BeatAnalysis {
            beats: entry.beats,
            bpm: entry.bpm,
            profile: entry.profile,
        });
    }

    // The track is streamed through the frame pass; no full decoded copy of
    // the song is ever held in memory, so the peak stays flat.
    let analysis = analyze_streamed(path, cancel, config)?;
    if let Some(signature) = signature {
        store_cache_entry(path, signature, &analysis, config_hash);
    }
    Ok(analysis)
}

/// Runs the frame pass while the track is decoded in small chunks. No full
/// decoded copy of the song is ever held in memory, so the peak stays flat.
fn analyze_streamed(
    path: &Path,
    cancel: &AtomicBool,
    config: &BeatConfig,
) -> Result<BeatAnalysis> {
    let mut pass = FramePass::new();
    if !pass.usable() {
        return Ok(BeatAnalysis::default());
    }
    // The sink returns false to stop early, which only happens on cancel.
    let ran_to_end = crate::decode::decode_mono_streamed(
        path,
        ANALYSIS_RATE,
        cancel,
        &mut |chunk: &[f32]| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            pass.push(chunk);
            true
        },
    )?;
    if !ran_to_end || cancel.load(Ordering::Relaxed) {
        return Err(anyhow!("beat analysis cancelled"));
    }
    finish_pass(pass, config)
}

/// The same frame pass over an in-memory buffer, fed in blocks that are not
/// hop-aligned, so the sliding-window realignment is exercised too.
fn analyze_pcm(samples: &[f32], cancel: &AtomicBool, config: &BeatConfig) -> Result<BeatAnalysis> {
    let mut pass = FramePass::new();
    if !pass.usable() {
        return Ok(BeatAnalysis::default());
    }
    for chunk in samples.chunks(1000) {
        if cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("beat analysis cancelled"));
        }
        pass.push(chunk);
    }
    finish_pass(pass, config)
}

/// Turns streamed mono samples into per-frame band energies and the sound
/// profile accumulator. Chunks may arrive in any size; a sliding carry buffer
/// realigns them into the overlapping analysis frames.
struct FramePass {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    buffer: Vec<Complex<f32>>,
    bin_hz: f32,
    low_start: usize,
    low_end: usize,
    wide_start: usize,
    wide_end: usize,
    mid_start: usize,
    mid_end: usize,
    high_start: usize,
    low_energy: Vec<f32>,
    wide_energy: Vec<f32>,
    mid_energy: Vec<f32>,
    high_energy: Vec<f32>,
    frame_power: Vec<f32>,
    profile_frames: ProfileFrames,
    carry: Vec<f32>,
    frames_done: usize,
    total_samples: usize,
}

impl FramePass {
    fn new() -> Self {
        let bin_hz = ANALYSIS_RATE as f32 / FRAME_SIZE as f32;
        let bins = FRAME_SIZE / 2 + 1;
        let low_start = (LOW_CUT_HZ / bin_hz).ceil() as usize;
        let low_end = ((LOW_HIGH_HZ / bin_hz).floor() as usize).min(bins - 1);
        let wide_start = low_start;
        let wide_end = ((WIDE_HIGH_HZ / bin_hz).floor() as usize).min(bins - 1);
        let mid_start = (low_end + 1).min(bins - 1);
        let mid_end = ((SOFT_HIGH_HZ / bin_hz).floor() as usize).min(bins - 1);
        let high_start = ((SOFT_HIGH_HZ / bin_hz).ceil() as usize).min(wide_end);
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(FRAME_SIZE);
        let window = (0..FRAME_SIZE)
            .map(|index| {
                let x = std::f32::consts::TAU * index as f32 / (FRAME_SIZE - 1) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect::<Vec<f32>>();
        Self {
            fft,
            window,
            buffer: vec![Complex::new(0.0_f32, 0.0_f32); FRAME_SIZE],
            bin_hz,
            low_start,
            low_end,
            wide_start,
            wide_end,
            mid_start,
            mid_end,
            high_start,
            low_energy: Vec::new(),
            wide_energy: Vec::new(),
            mid_energy: Vec::new(),
            high_energy: Vec::new(),
            frame_power: Vec::new(),
            profile_frames: ProfileFrames::with_capacity(0),
            carry: Vec::new(),
            frames_done: 0,
            total_samples: 0,
        }
    }

    fn usable(&self) -> bool {
        self.low_start <= self.low_end && self.mid_start <= self.mid_end
    }

    fn push(&mut self, chunk: &[f32]) {
        self.carry.extend_from_slice(chunk);
        self.total_samples += chunk.len();
        let frames_here = if self.carry.len() >= FRAME_SIZE {
            1 + (self.carry.len() - FRAME_SIZE) / HOP_SIZE
        } else {
            0
        };
        for frame in 0..frames_here {
            let start = frame * HOP_SIZE;
            let frame_samples = &self.carry[start..start + FRAME_SIZE];
            let mut square_sum = 0.0;
            for index in 0..FRAME_SIZE {
                let sample = frame_samples[index];
                square_sum += sample * sample;
                self.buffer[index] = Complex::new(sample * self.window[index], 0.0);
            }
            self.frame_power.push(square_sum / FRAME_SIZE as f32);
            self.fft.process(&mut self.buffer);
            self.profile_frames
                .push(frame_samples, &self.buffer, self.bin_hz);

            let mut low = 0.0;
            let mut wide = 0.0;
            let mut mid = 0.0;
            let mut high = 0.0;
            for bin in self.low_start..=self.low_end {
                let value = self.buffer[bin];
                low += value.re * value.re + value.im * value.im;
            }
            for bin in self.wide_start..=self.wide_end {
                let value = self.buffer[bin];
                let power = value.re * value.re + value.im * value.im;
                wide += power;
                if bin >= self.high_start {
                    high += power;
                }
            }
            for bin in self.mid_start..=self.mid_end {
                let value = self.buffer[bin];
                mid += value.re * value.re + value.im * value.im;
            }
            self.low_energy.push(low);
            self.wide_energy.push(wide);
            self.mid_energy.push(mid);
            self.high_energy.push(high);
        }
        if frames_here > 0 {
            self.carry.drain(0..frames_here * HOP_SIZE);
            self.frames_done += frames_here;
        }
    }
}

/// The band curves, frame power and sound profile accumulator are consumed
/// here; everything is per-frame data, so the raw samples are long gone.
fn finish_pass(pass: FramePass, config: &BeatConfig) -> Result<BeatAnalysis> {
    let FramePass {
        low_energy,
        wide_energy,
        mid_energy,
        high_energy,
        frame_power,
        profile_frames,
        frames_done,
        total_samples,
        ..
    } = pass;
    let frame_count = frames_done;
    if frame_count == 0 || total_samples < FRAME_SIZE * 4 {
        return Ok(BeatAnalysis::default());
    }
    let frame_rate = ANALYSIS_RATE as f32 / HOP_SIZE as f32;
    let Some(low) = band_curve(&low_energy) else {
        return Ok(BeatAnalysis::default());
    };
    let wide = band_curve(&wide_energy).unwrap_or_else(|| zero_band(frame_count));
    let mid = band_curve(&mid_energy).unwrap_or_else(|| zero_band(frame_count));
    let high = band_curve(&high_energy).unwrap_or_else(|| zero_band(frame_count));

    let low_flux = rectified_flux(&low.curve);
    let wide_flux = rectified_flux(&wide.curve);
    let mid_flux = rectified_flux(&mid.curve);
    let profile = profile_frames.finish(
        &BandCurves {
            power: &frame_power,
            low: &low_energy,
            mid: &mid_energy,
            wide: &wide_energy,
            high: &high_energy,
            low_flux: &low_flux,
            mid_flux: &mid_flux,
            wide_flux: &wide_flux,
            high_flux: &rectified_flux(&high.curve),
        },
        frame_rate,
    );
    let low_span = span(&low_flux);
    let wide_span = span(&wide_flux);
    let mid_span = span(&mid_flux);
    let low_norm = normalize(&low_flux, low_span);
    let wide_norm = normalize(&wide_flux, wide_span);
    let mid_norm = normalize(&mid_flux, mid_span);
    let low_driven = low_span >= 0.30 * wide_span.max(low_span);

    // Heavy beats are picked from the low band only when it dominates the
    // track. The wide band is used for strength/accent, not for triggering.
    let smooth_low = smooth3(&low_norm);
    let smooth_wide = smooth3(&wide_norm);
    let heavy_onset = if low_driven {
        smooth_low.clone()
    } else {
        smooth_wide
            .iter()
            .zip(&smooth_low)
            .map(|(wide, low)| wide + 0.5 * low)
            .collect()
    };
    // Low must dominate the wide band. Mid-band percussion is deliberately
    // ignored here: a bass-driven groove can still be the obvious beat even
    // when the mix has a lot of mid content.
    let share = (0..frame_count)
        .map(|index| smooth_low[index] / (smooth_low[index] + smooth_wide[index] + 1e-9))
        .collect::<Vec<f32>>();
    let low_peak = percentile(&smooth_low, 0.99);
    let wide_peak = percentile(&smooth_wide, 0.99);
    let mut heavy_gate = vec![false; frame_count];
    for index in 0..frame_count {
        heavy_gate[index] = if low_driven {
            low.relative[index] >= 0.004
                && share[index] >= config.low_share_min
                && smooth_low[index] >= 0.20 * low_peak
        } else {
            wide.relative[index] >= 0.004 && smooth_wide[index] >= 0.20 * wide_peak
        };
    }
    let heavy_peaks = pick_peaks(
        &heavy_onset,
        frame_rate,
        config.sensitivity,
        config.heavy_min_interval_ms,
        &heavy_gate,
        Some(config),
    );
    let heavy_frames = heavy_peaks
        .iter()
        .map(|peak| peak.frame)
        .collect::<Vec<usize>>();
    let normal_frames = heavy_peaks
        .iter()
        .filter(|peak| !peak.quick)
        .map(|peak| peak.frame)
        .collect::<Vec<usize>>();
    let heavy_scale = percentile(
        &normal_frames
            .iter()
            .map(|&frame| heavy_onset[frame])
            .collect::<Vec<f32>>(),
        0.90,
    )
    .max(1e-6);
    let quick_span = config.quick_strength_max - config.quick_strength_min;
    let heavy_span = config.heavy_strength_max - config.heavy_strength_min;
    let heavy = heavy_peaks
        .iter()
        .map(|peak| {
            let ratio = (heavy_onset[peak.frame] / heavy_scale).clamp(0.0, 1.0);
            let strength = if peak.quick {
                config.quick_strength_min + quick_span * ratio
            } else {
                (config.heavy_strength_min + heavy_span * ratio)
                    .clamp(config.heavy_strength_min, config.heavy_strength_max)
            };
            (peak.frame, strength)
        })
        .collect::<Vec<(usize, f32)>>();

    let soft_onset = smooth3(&mid_norm);
    let mid_peak = percentile(&mid_norm, 0.99);
    let mut soft_gate = vec![false; frame_count];
    for index in 0..frame_count {
        soft_gate[index] = mid.relative[index] >= 0.004
            && mid_norm[index] >= 0.6 * low_norm[index]
            && mid_norm[index] >= 0.20 * mid_peak;
    }
    let soft_peaks = pick_peaks(
        &soft_onset,
        frame_rate,
        config.sensitivity,
        config.soft_min_interval_ms,
        &soft_gate,
        None,
    );
    let soft_scale = percentile(
        &soft_peaks
            .iter()
            .map(|peak| soft_onset[peak.frame])
            .collect::<Vec<f32>>(),
        0.90,
    )
    .max(1e-6);
    let merge = (frame_rate * config.soft_merge_ms / 1000.0).round() as usize;
    let mut blocked = vec![false; frame_count];
    for &frame in &heavy_frames {
        let start = frame.saturating_sub(merge);
        let end = (frame + merge).min(frame_count - 1);
        for slot in &mut blocked[start..=end] {
            *slot = true;
        }
    }
    let soft_span_strength = config.soft_strength_max - config.soft_strength_min;
    let soft = soft_peaks
        .iter()
        .filter(|peak| !blocked[peak.frame])
        .map(|peak| {
            let strength = (config.soft_strength_min
                + soft_span_strength * soft_onset[peak.frame] / soft_scale)
                .clamp(config.soft_strength_min, config.soft_strength_max);
            (peak.frame, strength)
        })
        .collect::<Vec<(usize, f32)>>();

    let mut beats = heavy.into_iter().chain(soft).collect::<Vec<(usize, f32)>>();
    beats.sort_by_key(|(frame, _)| *frame);
    let beats = beats
        .into_iter()
        .map(|(frame, strength)| {
            (
                frame as f64 / frame_rate as f64 + FRAME_CENTER_SECONDS,
                strength,
            )
        })
        .collect::<Vec<(f64, f32)>>();
    let bpm = estimate_bpm(&normal_frames, frame_rate);
    Ok(BeatAnalysis {
        beats,
        bpm,
        profile,
    })
}

fn zero_band(frame_count: usize) -> BandCurve {
    BandCurve {
        relative: vec![0.0; frame_count],
        curve: vec![0.0; frame_count],
    }
}

fn band_curve(energy: &[f32]) -> Option<BandCurve> {
    let reference = percentile(energy, 0.96);
    if reference <= 1e-16 {
        return None;
    }
    let relative = energy
        .iter()
        .map(|value| *value / reference)
        .collect::<Vec<f32>>();
    let curve = relative
        .iter()
        .map(|value| (value * 40.0).ln_1p())
        .collect::<Vec<f32>>();
    Some(BandCurve { relative, curve })
}

fn rectified_flux(curve: &[f32]) -> Vec<f32> {
    let mut flux = vec![0.0; curve.len()];
    for index in 1..curve.len() {
        let baseline = if index >= 2 {
            curve[index - 1].max(curve[index - 2])
        } else {
            curve[index - 1]
        };
        flux[index] = (curve[index] - baseline).max(0.0);
    }
    flux
}

fn span(values: &[f32]) -> f32 {
    (percentile(values, 0.99) - percentile(values, 0.50)).max(1e-6)
}

fn normalize(values: &[f32], span: f32) -> Vec<f32> {
    let span = span.max(1e-6);
    values.iter().map(|value| *value / span).collect()
}

fn smooth3(values: &[f32]) -> Vec<f32> {
    let mut out = vec![0.0; values.len()];
    for index in 0..values.len() {
        let previous = if index > 0 { values[index - 1] } else { 0.0 };
        let next = if index + 1 < values.len() {
            values[index + 1]
        } else {
            0.0
        };
        out[index] = 0.25 * previous + 0.5 * values[index] + 0.25 * next;
    }
    out
}

fn pick_peaks(
    onset: &[f32],
    frame_rate: f32,
    sensitivity: f32,
    min_interval_ms: f32,
    gate: &[bool],
    burst: Option<&BeatConfig>,
) -> Vec<PickedPeak> {
    let count = onset.len();
    if count == 0 {
        return Vec::new();
    }
    let window_radius = ((frame_rate * 0.7).round() as usize).max(2);
    let peak_radius = ((frame_rate * 0.035).round() as usize).max(1);
    let strictness = 0.7 + 3.4 * sensitivity.clamp(0.10, 0.85);
    let peak_reference = percentile(onset, 0.99);
    if peak_reference <= 1e-6 {
        return Vec::new();
    }

    let mut candidates = Vec::new();
    for index in 0..count {
        let window_start = index.saturating_sub(window_radius);
        let window_end = (index + window_radius).min(count - 1);
        let mut sum = 0.0;
        for value in &onset[window_start..=window_end] {
            sum += *value;
        }
        let mean = sum / (window_end - window_start + 1) as f32;
        let mut spread = 0.0;
        for value in &onset[window_start..=window_end] {
            spread += (*value - mean).abs();
        }
        spread /= (window_end - window_start + 1) as f32;
        let threshold = (mean + strictness * spread.max(1e-4)).max(0.10 * peak_reference);
        let peak_start = index.saturating_sub(peak_radius);
        let peak_end = (index + peak_radius).min(count - 1);
        let gate_start = index.saturating_sub(1);
        let gate_end = (index + 1).min(count - 1);
        let gate_ok = gate[gate_start..=gate_end].iter().any(|value| *value);
        if !gate_ok || onset[index] <= threshold {
            continue;
        }
        let local_max = onset[peak_start..=peak_end]
            .iter()
            .copied()
            .fold(f32::NEG_INFINITY, f32::max);
        if onset[index] >= local_max - 1e-9 {
            candidates.push(index);
        }
    }

    let gap = ((frame_rate * min_interval_ms / 1000.0).round() as usize).max(1);
    let replace_gap = ((frame_rate * 0.060).round() as usize).max(1);
    let short_gap = burst
        .map(|config| {
            ((frame_rate * config.heavy_short_interval_ms / 1000.0).round() as usize).max(1)
        })
        .unwrap_or(1);
    let pair_window = burst
        .map(|config| {
            ((frame_rate * config.heavy_pair_window_ms / 1000.0).round() as usize).max(short_gap)
        })
        .unwrap_or(gap);
    let cooldown = burst
        .map(|config| {
            ((frame_rate * config.heavy_burst_cooldown_ms / 1000.0).round() as usize).max(1)
        })
        .unwrap_or(gap);
    let mut picked: Vec<PickedPeak> = Vec::new();
    let mut quick_count = 0usize;
    let mut blocked_until = 0usize;

    for index in candidates {
        let Some(last) = picked.last().copied() else {
            picked.push(PickedPeak {
                frame: index,
                quick: false,
            });
            continue;
        };
        if burst.is_some() && index < blocked_until {
            continue;
        }
        let dt = index - last.frame;
        if let Some(config) = burst {
            let burst_max = config.heavy_burst_max as usize;
            if quick_count < burst_max
                && dt >= short_gap
                && dt < pair_window
                && onset[index] >= config.quick_min_ratio * onset[last.frame]
            {
                picked.push(PickedPeak {
                    frame: index,
                    quick: true,
                });
                quick_count += 1;
                if quick_count >= burst_max {
                    blocked_until = index + cooldown;
                }
                continue;
            }
        }
        if dt >= gap {
            picked.push(PickedPeak {
                frame: index,
                quick: false,
            });
            quick_count = 0;
            blocked_until = 0;
            continue;
        }
        if !last.quick && dt <= replace_gap && onset[index] > onset[last.frame] {
            if let Some(slot) = picked.last_mut() {
                slot.frame = index;
            }
        }
    }
    picked
}

pub(crate) fn percentile(values: &[f32], q: f32) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let position = (sorted.len() - 1) as f32 * q.clamp(0.0, 1.0);
    let lower = position.floor() as usize;
    let upper = position.ceil() as usize;
    let fraction = position - lower as f32;
    sorted[lower] + (sorted[upper] - sorted[lower]) * fraction
}

fn estimate_bpm(frames: &[usize], frame_rate: f32) -> f32 {
    if frames.len() < 4 {
        return 0.0;
    }
    let mut intervals = frames
        .windows(2)
        .map(|pair| (pair[1] - pair[0]) as f32 / frame_rate)
        .filter(|value| *value > 0.2 && *value < 2.0)
        .collect::<Vec<f32>>();
    if intervals.is_empty() {
        return 0.0;
    }
    intervals.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let middle = intervals.len() / 2;
    let median = if intervals.len() % 2 == 0 {
        (intervals[middle - 1] + intervals[middle]) * 0.5
    } else {
        intervals[middle]
    };
    if median <= 0.0 {
        0.0
    } else {
        (60.0 / median * 100.0).round() / 100.0
    }
}

fn file_signature(path: &Path) -> Option<FileSignature> {
    let metadata = fs::metadata(path).ok()?;
    let mtime_ms = metadata
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_millis() as u64;
    Some(FileSignature {
        size: metadata.len(),
        mtime_ms,
    })
}

/// Analysis cache kind: one JSON file per track under `analysis/beat`.
const CACHE_KIND: &str = "beat";

fn cache_lock() -> &'static Mutex<()> {
    CACHE_LOCK.get_or_init(|| Mutex::new(()))
}

fn cache_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// One-time split of the legacy single-file cache: each track goes into its
/// own file, then the big file is dropped. Unreadable or outdated caches are
/// simply removed — those entries re-analyze on demand.
fn migrate_legacy_cache() {
    let legacy = storage::data_dir().join("beat_cache.json");
    let Some(cache) = cache_store::read_json_file::<BeatCacheFile>(&legacy) else {
        return;
    };
    if cache.version == ANALYSIS_VERSION {
        for (key, entry) in &cache.tracks {
            cache_store::write_entry(CACHE_KIND, key, entry);
        }
    }
    let _ = fs::remove_file(&legacy);
}

fn load_cache_entry(
    path: &Path,
    signature: FileSignature,
    config_hash: u64,
) -> Option<BeatCacheEntry> {
    let _guard = cache_lock().lock().ok()?;
    migrate_legacy_cache();
    let key = cache_key(path);
    let entry: BeatCacheEntry = cache_store::read_entry(CACHE_KIND, &key)?;
    if entry.version == ANALYSIS_VERSION
        && entry.config_hash == config_hash
        && entry.size == signature.size
        && entry.mtime_ms == signature.mtime_ms
    {
        Some(entry)
    } else {
        None
    }
}

fn store_cache_entry(
    path: &Path,
    signature: FileSignature,
    analysis: &BeatAnalysis,
    config_hash: u64,
) {
    let Ok(_guard) = cache_lock().lock() else {
        return;
    };
    migrate_legacy_cache();
    let saved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let entry = BeatCacheEntry {
        version: ANALYSIS_VERSION,
        size: signature.size,
        mtime_ms: signature.mtime_ms,
        saved_at,
        config_hash,
        beats: analysis.beats.clone(),
        bpm: analysis.bpm,
        profile: analysis.profile,
    };
    cache_store::write_entry(CACHE_KIND, &cache_key(path), &entry);
    cache_store::prune_entries(CACHE_KIND, 160, 120);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "requires SONADIAL_BEAT_TEST_TRACK"]
    fn analyze_real_track() {
        let path = std::env::var("SONADIAL_BEAT_TEST_TRACK")
            .expect("set SONADIAL_BEAT_TEST_TRACK to an audio file");
        let cancel = AtomicBool::new(false);
        let analysis = analyze_file(Path::new(&path), &cancel, &BeatConfig::default())
            .expect("analysis should finish");
        let heavy = analysis
            .beats
            .iter()
            .filter(|(_, strength)| *strength >= 0.5)
            .count();
        let last = analysis.beats.last().map(|(time, _)| *time).unwrap_or(0.0);
        eprintln!(
            "beat analysis: total={} heavy={} bpm={} last={last:.1}s",
            analysis.beats.len(),
            heavy,
            analysis.bpm
        );
        assert!(!analysis.beats.is_empty());
    }

    #[test]
    fn keeps_short_pairs() {
        let rate = ANALYSIS_RATE as f32;
        let duration = 8.0;
        let mut samples = vec![0.0_f32; (rate * duration) as usize];
        let mut expected = Vec::new();
        for group in 0..4 {
            let base = group as f32 * 2.0;
            for offset in [0.0_f32, 0.18, 0.64] {
                let start = ((base + offset) * rate) as usize;
                let length = (0.12 * rate) as usize;
                for index in 0..length {
                    let time = index as f32 / rate;
                    let envelope = (-time * 28.0).exp();
                    samples[start + index] +=
                        (std::f32::consts::TAU * 60.0 * time).sin() * envelope * 0.8;
                }
                expected.push((base + offset) as f64 + FRAME_CENTER_SECONDS);
            }
        }

        let cancel = AtomicBool::new(false);
        let analysis =
            analyze_pcm(&samples, &cancel, &BeatConfig::default()).expect("analysis should finish");
        let found = expected
            .iter()
            .filter(|time| {
                analysis
                    .beats
                    .iter()
                    .any(|(beat_time, _)| (beat_time - **time).abs() < 0.05)
            })
            .count();
        assert!(
            found >= 9,
            "found {found}/{} short-pair hits",
            expected.len()
        );
    }

    #[test]
    fn detects_synthetic_kicks() {
        let rate = ANALYSIS_RATE as f32;
        let duration = 8.0;
        let mut samples = vec![0.0_f32; (rate * duration) as usize];
        for beat in 0..16 {
            let start = (beat as f32 * 0.5 * rate) as usize;
            let length = (0.12 * rate) as usize;
            for offset in 0..length {
                let time = offset as f32 / rate;
                let envelope = (-time * 28.0).exp();
                samples[start + offset] +=
                    (std::f32::consts::TAU * 60.0 * time).sin() * envelope * 0.8;
            }
        }

        let cancel = AtomicBool::new(false);
        let analysis =
            analyze_pcm(&samples, &cancel, &BeatConfig::default()).expect("analysis should finish");
        let heavy = analysis
            .beats
            .iter()
            .filter(|(_, strength)| *strength >= 0.5)
            .count();
        assert!(heavy >= 10, "heavy beats: {heavy}");
        assert!((analysis.bpm - 120.0).abs() < 20.0, "bpm: {}", analysis.bpm);
    }
}
