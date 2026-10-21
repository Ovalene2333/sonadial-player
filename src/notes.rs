//! Offline note transcription for the piano style.
//!
//! The track is decoded to 22.05kHz mono and every frame is decomposed with a
//! fixed piano harmonic dictionary (sparse NMF). Activations are tracked into
//! note events. Results are cached by path, size and mtime so replaying a track
//! does not decode it again.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Result};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use crate::pitch::{self, MAX_MIDI, MIN_MIDI, NUM_NOTES};
use crate::storage;

const SAMPLE_RATE: u32 = 22_050;
const FFT_SIZE: usize = 2_048;
const LOW_FFT_SIZE: usize = 8_192;
const HOP_SIZE: usize = 256;

/// Offline note decomposition: a fixed harmonic dictionary per MIDI note is
/// fitted to every frame with sparse non-negative matrix factorisation. Shared
/// harmonics are split by energy instead of being deleted, which is what makes
/// chord tones above a low bass note detectable.
const NMF_HARMONICS: usize = 12;
const NMF_ALPHA: f32 = 1.6;
const NMF_INHARMONICITY: f32 = 0.0002;
const NMF_KERNEL_RADIUS: i32 = 2;
const NMF_ITERATIONS: usize = 10;
const NMF_L1: f32 = 0.1;
const NMF_NOISE_L1: f32 = 3.0;
const NMF_THRESHOLD: f32 = 0.05;
const NMF_ASSIGN_MARGIN: f32 = 0.10;
const NMF_ABS_GATE: f32 = 0.004;

const HIGH_MAX_HZ: f32 = 5_200.0;
const HIGH_MAX_POLYPHONY: usize = 6;
const LOW_MAX_HZ: f32 = 1_200.0;
const LOW_MAX_POLYPHONY: usize = 10;
/// The low detector's 8192-point window reports a note about 0.28s before its
/// attack. Its release timing is already correct, so only the start moves.
const LOW_MAX_MIDI: u8 = 47;
const LOW_START_OFFSET: f64 = 0.28;

const MIN_NOTE_SECS: f64 = 0.18;
const MAX_NOTE_SECS: f64 = 10.0;
/// Frames a note may vanish before it is considered released.
const GAP_FRAMES: u32 = 2;
const MIN_NOTE_LEVEL: f32 = 0.14;
const MIN_NOTE_CONFIDENCE: f32 = 0.45;

/// Re-attacks are split at broadband onsets using the raw harmonic energy of
/// that pitch, which does not depend on the NMF sharing energy with new notes.
const ONSET_WINDOW: usize = 10;
const ONSET_RATIO: f32 = 1.4;
const ONSET_ADD: f32 = 0.002;
const ONSET_SPACING: usize = 4;
const ONSET_CLUSTER_SECS: f64 = 0.22;
const ONSET_MIN_SEGMENT: f64 = 0.12;

/// Bump when the transcription changes; old cache entries are ignored.
const ANALYSIS_VERSION: u32 = 6;

const MAJOR_PROFILE: [f32; 12] = [
    6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
];
const MINOR_PROFILE: [f32; 12] = [
    6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NoteEvent {
    pub start: f64,
    pub end: f64,
    pub midi: u8,
    /// 0..1, relative attack level.
    pub velocity: f32,
    /// 0..1, how consistently the note was seen.
    pub confidence: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyEstimate {
    /// Pitch class, 0 = C .. 11 = B.
    pub root: u8,
    pub minor: bool,
    /// Correlation with the Krumhansl-Kessler profile.
    pub score: f32,
}

impl KeyEstimate {
    pub fn label(&self) -> String {
        format!(
            "{} {}",
            pitch::pitch_class_name(self.root),
            if self.minor { "MIN" } else { "MAJ" }
        )
    }
}

#[derive(Clone, Debug, Default)]
pub struct NoteAnalysis {
    pub notes: Vec<NoteEvent>,
    pub key: Option<KeyEstimate>,
    pub avg_confidence: f32,
}

struct Pending {
    start_frame: usize,
    last_frame: usize,
    gap: u32,
    peak_level: f32,
    peak_absolute: f32,
    level_sum: f32,
    frames: u32,
}

impl Pending {
    fn new(frame: usize, level: f32, absolute: f32) -> Self {
        Self {
            start_frame: frame,
            last_frame: frame,
            gap: 0,
            peak_level: level,
            peak_absolute: absolute,
            level_sum: level,
            frames: 1,
        }
    }
}

struct DetectorConfig {
    fft_size: usize,
    min_midi: u8,
    max_midi: u8,
    output_min: u8,
    output_max: u8,
    max_hz: f32,
    max_poly: usize,
}

struct Atom {
    start: usize,
    end: usize,
}

struct FrameDetector {
    fft: std::sync::Arc<dyn Fft<f32>>,
    fft_size: usize,
    window: Vec<f32>,
    frame: Vec<Complex<f32>>,
    mag: Vec<f32>,
    num_bins: usize,
    min_midi: u8,
    output_min: u8,
    output_max: u8,
    max_poly: usize,
    bins: Vec<u16>,
    weights: Vec<f32>,
    atoms: Vec<Atom>,
    noise: Atom,
    /// Pre-normalisation harmonic mass per note, used to decide whether an
    /// upper note is already explained by a lower note's harmonic.
    z: Vec<f32>,
    h: Vec<f32>,
    work: Vec<f32>,
    ratio: Vec<f32>,
    order: Vec<usize>,
}

impl FrameDetector {
    fn new(config: DetectorConfig) -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(config.fft_size);
        let window: Vec<f32> = (0..config.fft_size)
            .map(|index| {
                let x = std::f32::consts::TAU * index as f32 / (config.fft_size - 1) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        let bin_hz = SAMPLE_RATE as f32 / config.fft_size as f32;
        let num_bins =
            ((config.max_hz / bin_hz) as usize + 3).min(config.fft_size / 2 + 1);
        let count = (config.max_midi - config.min_midi + 1) as usize;
        let mut bins: Vec<u16> = Vec::with_capacity(count * 96);
        let mut weights: Vec<f32> = Vec::with_capacity(count * 96);
        let mut atoms = Vec::with_capacity(count);
        let mut z = vec![0.0_f32; count];
        for (index, mass) in z.iter_mut().enumerate() {
            let start = bins.len();
            let f0 = pitch::midi_hz(config.min_midi + index as u8);
            for k in 1..=NMF_HARMONICS {
                let harmonic = k as f32;
                let stretch = (1.0 + NMF_INHARMONICITY * harmonic * harmonic).sqrt();
                let freq = f0 * harmonic * stretch;
                if freq >= config.max_hz {
                    break;
                }
                let position = freq / bin_hz;
                let base = position.floor() as i32;
                let fraction = position - base as f32;
                for offset in -NMF_KERNEL_RADIUS..=NMF_KERNEL_RADIUS {
                    let bin = base + offset;
                    if bin < 0 || bin as usize >= num_bins {
                        continue;
                    }
                    let distance = (offset as f32 - fraction).abs();
                    if distance > NMF_KERNEL_RADIUS as f32 {
                        continue;
                    }
                    let kernel = 0.5
                        * (1.0
                            + (std::f32::consts::PI * distance / NMF_KERNEL_RADIUS as f32).cos());
                    let weight = harmonic.powf(-NMF_ALPHA) * kernel;
                    bins.push(bin as u16);
                    weights.push(weight);
                    *mass += weight;
                }
            }
            atoms.push(Atom {
                start,
                end: bins.len(),
            });
        }
        for (index, atom) in atoms.iter().enumerate() {
            let inverse = 1.0 / z[index].max(1e-9);
            for weight in &mut weights[atom.start..atom.end] {
                *weight *= inverse;
            }
        }
        let noise_start = bins.len();
        for bin in 0..num_bins {
            bins.push(bin as u16);
            weights.push(1.0 / num_bins as f32);
        }
        let noise = Atom {
            start: noise_start,
            end: bins.len(),
        };
        Self {
            fft,
            fft_size: config.fft_size,
            window,
            frame: vec![Complex::new(0.0, 0.0); config.fft_size],
            mag: vec![0.0; config.fft_size / 2 + 1],
            num_bins,
            min_midi: config.min_midi,
            output_min: config.output_min,
            output_max: config.output_max,
            max_poly: config.max_poly,
            bins,
            weights,
            atoms,
            noise,
            z,
            h: vec![0.0; count + 1],
            work: vec![0.0; num_bins],
            ratio: vec![0.0; num_bins],
            order: (0..count).collect(),
        }
    }

    fn magnitude(&self) -> &[f32] {
        &self.mag
    }

    /// Fit the fixed harmonic dictionary to one frame. The activations say how
    /// much each note contributes, so a chord tone sharing a bass harmonic keeps
    /// its own energy instead of being cancelled.
    fn pick(&mut self, samples: &[f32], out: &mut Vec<(usize, f32, f32, f32)>) {
        out.clear();
        if samples.len() < self.fft_size {
            return;
        }
        for index in 0..self.fft_size {
            self.frame[index] = Complex::new(samples[index] * self.window[index], 0.0);
        }
        self.fft.process(&mut self.frame);
        let norm = 2.0 / self.fft_size as f32;
        for (index, slot) in self.mag.iter_mut().enumerate() {
            let value = self.frame[index];
            *slot = (value.re * value.re + value.im * value.im).sqrt() * norm;
        }

        let mut energy = 0.0_f32;
        for &value in &self.mag[..self.num_bins] {
            energy = energy.max(value);
        }
        if energy <= 1e-7 {
            return;
        }

        let count = self.atoms.len();
        self.h.fill(0.01 / energy.max(1e-9));
        for _ in 0..NMF_ITERATIONS {
            self.work.fill(0.0);
            for (index, atom) in self.atoms.iter().enumerate() {
                let activation = self.h[index];
                for entry in atom.start..atom.end {
                    let bin = self.bins[entry] as usize;
                    self.work[bin] += self.weights[entry] * activation;
                }
            }
            let noise_activation = self.h[count];
            for entry in self.noise.start..self.noise.end {
                let bin = self.bins[entry] as usize;
                self.work[bin] += self.weights[entry] * noise_activation;
            }

            for (ratio, (value, total)) in self
                .ratio
                .iter_mut()
                .zip(self.mag[..self.num_bins].iter().zip(self.work.iter()))
            {
                *ratio = *value / (*total + 1e-9);
            }

            for (index, atom) in self.atoms.iter().enumerate() {
                let mut sum = 0.0;
                for entry in atom.start..atom.end {
                    sum += self.weights[entry] * self.ratio[self.bins[entry] as usize];
                }
                self.h[index] *= sum / (1.0 + NMF_L1);
            }
            let mut noise_sum = 0.0;
            for entry in self.noise.start..self.noise.end {
                noise_sum += self.weights[entry] * self.ratio[self.bins[entry] as usize];
            }
            self.h[count] *= noise_sum / (1.0 + NMF_L1 * NMF_NOISE_L1);
        }

        // An upper note whose fundamental is a harmonic of a louder lower note
        // is only kept when it carries energy beyond that lower note's comb.
        for lower in 0..count {
            let value = self.h[lower];
            if value <= 0.0 {
                continue;
            }
            for (offset, gain) in [
                (12usize, 2.0f32),
                (19, 3.0),
                (24, 4.0),
                (28, 5.0),
                (31, 6.0),
            ] {
                let upper = lower + offset;
                if upper >= count {
                    break;
                }
                let expected =
                    value * gain.powf(-NMF_ALPHA) * self.z[upper] / self.z[lower].max(1e-9);
                if self.h[upper] <= expected * (1.0 + NMF_ASSIGN_MARGIN) {
                    self.h[upper] = 0.0;
                }
            }
        }

        let top = self.h[..count].iter().cloned().fold(0.0_f32, f32::max);
        if top <= 1e-9 {
            return;
        }
        let floor = (top * NMF_THRESHOLD).max(NMF_ABS_GATE);
        let levels = &self.h;
        self.order
            .sort_unstable_by(|&a, &b| levels[b].total_cmp(&levels[a]));
        for &index in self.order.iter().take(self.max_poly) {
            let absolute = self.h[index];
            if absolute < floor {
                break;
            }
            let midi = self.min_midi + index as u8;
            if midi < self.output_min || midi > self.output_max {
                continue;
            }
            let atom = &self.atoms[index];
            let mut raw = 0.0;
            for entry in atom.start..atom.end {
                raw += self.weights[entry] * self.mag[self.bins[entry] as usize];
            }
            raw *= self.z[index];
            out.push((
                (midi - MIN_MIDI) as usize,
                (absolute / top).clamp(0.0, 1.0),
                absolute,
                raw,
            ));
        }
    }
}

fn frame_time(frame: usize) -> f64 {
    (frame as f64 * HOP_SIZE as f64 / SAMPLE_RATE as f64
        + FFT_SIZE as f64 / (2.0 * SAMPLE_RATE as f64)
        - 0.02)
        .max(0.0)
}

fn analyze_pcm(samples: &[f32], cancel: &AtomicBool) -> Result<NoteAnalysis> {
    if samples.len() < FFT_SIZE * 2 {
        return Ok(NoteAnalysis::default());
    }

    let frame_count = 1 + (samples.len() - FFT_SIZE) / HOP_SIZE;
    let mut high = FrameDetector::new(DetectorConfig {
        fft_size: FFT_SIZE,
        min_midi: MIN_MIDI,
        max_midi: MAX_MIDI,
        output_min: 48,
        output_max: MAX_MIDI,
        max_hz: HIGH_MAX_HZ,
        max_poly: HIGH_MAX_POLYPHONY,
    });
    // The low band needs a longer window to separate semitones. Mid/high atoms
    // stay in its dictionary as explainers so a C4 fundamental is not blamed on
    // a C2 harmonic.
    let mut low = FrameDetector::new(DetectorConfig {
        fft_size: LOW_FFT_SIZE,
        min_midi: MIN_MIDI,
        max_midi: 84,
        output_min: MIN_MIDI,
        output_max: LOW_MAX_MIDI,
        max_hz: LOW_MAX_HZ,
        max_poly: LOW_MAX_POLYPHONY,
    });

    let mut states: [Option<Pending>; NUM_NOTES] = [const { None }; NUM_NOTES];
    let mut notes = Vec::new();
    let mut detections = Vec::new();
    let mut low_detections = Vec::new();
    let mut seen = [false; NUM_NOTES];
    let mut flux = vec![0.0_f32; frame_count];
    let mut raw = vec![0.0_f32; NUM_NOTES * frame_count];
    let mut previous_mag = vec![0.0_f32; high.num_bins];

    for frame in 0..frame_count {
        if frame % 256 == 0 && cancel.load(Ordering::Relaxed) {
            return Err(anyhow!("note analysis cancelled"));
        }
        let offset = frame * HOP_SIZE;
        high.pick(&samples[offset..offset + FFT_SIZE], &mut detections);

        if frame > 0 {
            let mag = high.magnitude();
            let mut rise_sum = 0.0;
            for bin in 0..high.num_bins {
                let rise = mag[bin] - previous_mag[bin];
                if rise > 0.0 {
                    rise_sum += rise;
                }
            }
            flux[frame] = rise_sum;
        }
        previous_mag.copy_from_slice(&high.magnitude()[..high.num_bins]);

        if offset + LOW_FFT_SIZE <= samples.len() {
            low.pick(&samples[offset..offset + LOW_FFT_SIZE], &mut low_detections);
            detections.extend_from_slice(&low_detections);
        }

        seen.fill(false);
        for &(index, level, absolute, attack) in &detections {
            seen[index] = true;
            raw[index * frame_count + frame] = attack;
            if let Some(state) = &mut states[index] {
                state.last_frame = frame;
                state.gap = 0;
                state.peak_level = state.peak_level.max(level);
                state.peak_absolute = state.peak_absolute.max(absolute);
                state.level_sum += level;
                state.frames += 1;
            } else {
                states[index] = Some(Pending::new(frame, level, absolute));
            }
        }

        for index in 0..NUM_NOTES {
            if seen[index] {
                continue;
            }
            let Some(state) = &mut states[index] else {
                continue;
            };
            state.gap += 1;
            if state.gap > GAP_FRAMES {
                if let Some(state) = states[index].take() {
                    close_note(index, state, &mut notes);
                }
            }
        }
    }

    for (index, state) in states.iter_mut().enumerate() {
        if let Some(state) = state.take() {
            close_note(index, state, &mut notes);
        }
    }

    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.midi.cmp(&b.midi)));
    split_onsets(&mut notes, &flux, &raw, frame_count);
    notes.sort_by(|a, b| a.start.total_cmp(&b.start).then(a.midi.cmp(&b.midi)));
    normalize_velocity(&mut notes);

    let key = detect_key(&notes);
    let avg_confidence = if notes.is_empty() {
        0.0
    } else {
        notes.iter().map(|note| note.confidence).sum::<f32>() / notes.len() as f32
    };

    Ok(NoteAnalysis {
        notes,
        key,
        avg_confidence,
    })
}

fn find_onsets(flux: &[f32]) -> Vec<usize> {
    if flux.len() < 3 {
        return Vec::new();
    }
    let count = flux.len() as f32;
    let mean = flux.iter().sum::<f32>() / count;
    let variance = flux
        .iter()
        .map(|value| (value - mean) * (value - mean))
        .sum::<f32>()
        / count;
    let mut sorted = flux.to_vec();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let p75 = sorted[(sorted.len() * 3 / 4).min(sorted.len() - 1)];
    let threshold = (mean + 0.5 * variance.sqrt()).max(p75 * 0.5);

    let mut onsets = Vec::new();
    for frame in 1..flux.len() - 1 {
        if flux[frame] >= flux[frame - 1]
            && flux[frame] >= flux[frame + 1]
            && flux[frame] > threshold
        {
            if onsets
                .last()
                .map_or(true, |last| frame - last >= ONSET_SPACING)
            {
                onsets.push(frame);
            }
        }
    }
    onsets
}

/// A merged note is split at broadband attacks when that pitch's raw harmonic
/// energy rises across the attack. The cut uses the first onset of the cluster,
/// because the raw magnitude can dip for a while after a re-attack.
fn split_onsets(notes: &mut Vec<NoteEvent>, flux: &[f32], raw: &[f32], frame_count: usize) {
    let onsets = find_onsets(flux);
    if onsets.is_empty() {
        return;
    }
    let mut result = Vec::with_capacity(notes.len());
    for note in notes.drain(..) {
        if note.midi < 48 {
            result.push(note);
            continue;
        }
        let base = (note.midi - MIN_MIDI) as usize * frame_count;
        let mut cuts: Vec<f64> = Vec::new();
        for (position, &frame) in onsets.iter().enumerate() {
            let moment = frame_time(frame);
            if moment <= note.start + 0.10 || moment >= note.end - 0.10 {
                continue;
            }
            let low = frame.saturating_sub(ONSET_WINDOW);
            let high = (frame + 1 + ONSET_WINDOW).min(frame_count);
            let before = if frame > low {
                raw[base + low..base + frame].iter().sum::<f32>() / (frame - low) as f32
            } else {
                0.0
            };
            let after = if high > frame + 1 {
                raw[base + frame + 1..base + high].iter().sum::<f32>()
                    / (high - frame - 1) as f32
            } else {
                0.0
            };
            if after <= before * ONSET_RATIO + ONSET_ADD {
                continue;
            }
            let mut cut_frame = frame;
            for &earlier in onsets[..position].iter().rev() {
                if frame_time(frame) - frame_time(earlier) > ONSET_CLUSTER_SECS {
                    break;
                }
                cut_frame = earlier;
            }
            let mut cut = frame_time(cut_frame);
            if cut <= note.start + 0.10 {
                cut = moment;
            }
            if cuts
                .last()
                .map_or(true, |last| cut - last >= ONSET_MIN_SEGMENT)
            {
                cuts.push(cut);
            }
        }
        if cuts.is_empty() {
            result.push(note);
            continue;
        }
        let mut points = Vec::with_capacity(cuts.len() + 2);
        points.push(note.start);
        points.extend(cuts);
        points.push(note.end);
        for pair in points.windows(2) {
            let start = pair[0];
            let end = pair[1];
            if end - start < ONSET_MIN_SEGMENT {
                continue;
            }
            let mut segment = note.clone();
            segment.start = start;
            segment.end = end;
            result.push(segment);
        }
    }
    *notes = result;
}

fn close_note(index: usize, state: Pending, notes: &mut Vec<NoteEvent>) {
    let midi = MIN_MIDI + index as u8;
    let end = frame_time(state.last_frame) + HOP_SIZE as f64 / SAMPLE_RATE as f64;
    let mut start = frame_time(state.start_frame);
    if midi <= LOW_MAX_MIDI {
        start = (start + LOW_START_OFFSET).min(end - 0.01);
    }
    let duration = end - start;
    if duration < MIN_NOTE_SECS
        || duration > MAX_NOTE_SECS
        || state.frames < 3
        || state.peak_level < MIN_NOTE_LEVEL
    {
        return;
    }
    let average = state.level_sum / state.frames as f32;
    if average < MIN_NOTE_CONFIDENCE {
        return;
    }
    notes.push(NoteEvent {
        start,
        end,
        midi,
        velocity: state.peak_absolute,
        confidence: average.clamp(0.0, 1.0),
    });
}

/// Absolute salience depends on the recording level, so scale note velocities
/// against the loud end of the track instead of per-frame peaks.
fn normalize_velocity(notes: &mut [NoteEvent]) {
    if notes.is_empty() {
        return;
    }
    let mut levels = notes.iter().map(|note| note.velocity).collect::<Vec<_>>();
    levels.sort_by(|a, b| a.total_cmp(b));
    let reference = levels[(levels.len() * 9 / 10).min(levels.len() - 1)].max(1e-6);
    for note in notes {
        note.velocity = ((note.velocity / reference).sqrt() * 0.9).clamp(0.15, 1.0);
    }
}

fn detect_key(notes: &[NoteEvent]) -> Option<KeyEstimate> {
    let mut histogram = [0.0_f32; 12];
    for note in notes {
        let duration = (note.end - note.start).clamp(0.1, 2.0) as f32;
        histogram[(note.midi % 12) as usize] += duration * (0.3 + note.velocity);
    }
    let total: f32 = histogram.iter().sum();
    if total <= 0.0 {
        return None;
    }
    for value in histogram.iter_mut() {
        *value /= total;
    }

    let mut best: Option<KeyEstimate> = None;
    for root in 0..12u8 {
        for (profile, minor) in [(&MAJOR_PROFILE, false), (&MINOR_PROFILE, true)] {
            let mut rotated = [0.0_f32; 12];
            for (index, value) in profile.iter().enumerate() {
                rotated[(root as usize + index) % 12] = *value;
            }
            let score = correlation(&histogram, &rotated);
            if best.as_ref().map_or(true, |current| score > current.score) {
                best = Some(KeyEstimate { root, minor, score });
            }
        }
    }
    best.filter(|estimate| estimate.score >= 0.35)
}

fn correlation(a: &[f32; 12], b: &[f32; 12]) -> f32 {
    let mean_a = a.iter().sum::<f32>() / 12.0;
    let mean_b = b.iter().sum::<f32>() / 12.0;
    let mut dot = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;
    for index in 0..12 {
        let x = a[index] - mean_a;
        let y = b[index] - mean_b;
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a <= 1e-9 || norm_b <= 1e-9 {
        return 0.0;
    }
    dot / (norm_a.sqrt() * norm_b.sqrt())
}

/// Analyze a track in the background. Results are cached by path, size and
/// mtime so replaying a track does not decode it again.
pub fn analyze_file(path: &Path, cancel: &AtomicBool) -> Result<NoteAnalysis> {
    let signature = file_signature(path);
    if let Some(entry) = signature.and_then(|signature| load_cache_entry(path, signature)) {
        return Ok(NoteAnalysis {
            notes: entry.notes,
            key: entry.key,
            avg_confidence: entry.avg_confidence,
        });
    }

    let samples = crate::decode::decode_mono(path, SAMPLE_RATE, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(anyhow!("note analysis cancelled"));
    }
    let analysis = analyze_pcm(&samples, cancel)?;
    if let Some(signature) = signature {
        store_cache_entry(path, signature, &analysis);
    }
    Ok(analysis)
}

#[derive(Clone, Copy)]
struct FileSignature {
    size: u64,
    mtime_ms: u64,
}

fn file_signature(path: &Path) -> Option<FileSignature> {
    let metadata = fs::metadata(path).ok()?;
    let mtime_ms = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0);
    Some(FileSignature {
        size: metadata.len(),
        mtime_ms,
    })
}

#[derive(Serialize, Deserialize, Default)]
struct NotesCacheFile {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    tracks: HashMap<String, NotesCacheEntry>,
}

#[derive(Serialize, Deserialize, Clone)]
struct NotesCacheEntry {
    #[serde(default)]
    version: u32,
    size: u64,
    mtime_ms: u64,
    #[serde(default)]
    saved_at: u64,
    notes: Vec<NoteEvent>,
    #[serde(default)]
    key: Option<KeyEstimate>,
    #[serde(default)]
    avg_confidence: f32,
}

static CACHE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn cache_path() -> PathBuf {
    storage::data_dir().join("notes_cache.json")
}

fn cache_lock() -> &'static Mutex<()> {
    CACHE_LOCK.get_or_init(|| Mutex::new(()))
}

fn cache_key(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

fn read_cache() -> NotesCacheFile {
    fs::read_to_string(cache_path())
        .ok()
        .and_then(|text| serde_json::from_str(text.trim_start_matches('\u{feff}')).ok())
        .unwrap_or_default()
}

fn write_cache(cache: &NotesCacheFile) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let Ok(data) = serde_json::to_vec(cache) else {
        return;
    };
    let temp = path.with_extension("json.tmp");
    if fs::write(&temp, data).is_ok() {
        let _ = fs::remove_file(&path);
        let _ = fs::rename(&temp, &path);
    }
}

fn load_cache_entry(path: &Path, signature: FileSignature) -> Option<NotesCacheEntry> {
    let _guard = cache_lock().lock().ok()?;
    let cache = read_cache();
    if cache.version != ANALYSIS_VERSION {
        return None;
    }
    let entry = cache.tracks.get(&cache_key(path))?;
    if entry.version == ANALYSIS_VERSION
        && entry.size == signature.size
        && entry.mtime_ms == signature.mtime_ms
    {
        Some(entry.clone())
    } else {
        None
    }
}

fn store_cache_entry(path: &Path, signature: FileSignature, analysis: &NoteAnalysis) {
    let Ok(_guard) = cache_lock().lock() else {
        return;
    };
    let mut cache = read_cache();
    if cache.version != ANALYSIS_VERSION {
        cache.tracks.clear();
    }
    cache.version = ANALYSIS_VERSION;
    let saved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    cache.tracks.insert(
        cache_key(path),
        NotesCacheEntry {
            version: ANALYSIS_VERSION,
            size: signature.size,
            mtime_ms: signature.mtime_ms,
            saved_at,
            notes: analysis.notes.clone(),
            key: analysis.key.clone(),
            avg_confidence: analysis.avg_confidence,
        },
    );
    prune_cache(&mut cache);
    write_cache(&cache);
}

fn prune_cache(cache: &mut NotesCacheFile) {
    if cache.tracks.len() <= 64 {
        return;
    }
    let mut entries = cache
        .tracks
        .iter()
        .map(|(key, entry)| (key.clone(), entry.saved_at))
        .collect::<Vec<(String, u64)>>();
    entries.sort_by_key(|(_, saved_at)| *saved_at);
    let remove = cache.tracks.len() - 48;
    for (key, _) in entries.into_iter().take(remove) {
        cache.tracks.remove(&key);
    }
}
