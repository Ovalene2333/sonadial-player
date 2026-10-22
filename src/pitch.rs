//! Real-time note tracking for clear material such as solo piano.
//!
//! A dedicated 4096-point STFT runs alongside the 1024-point visual spectrum.
//! Pitch is not read from the FFT peak directly: low piano strings have
//! neighbouring semitones only a couple of hertz apart, so each candidate note
//! is scored by summing its harmonic series. Strong notes have their harmonics
//! cancelled before the next candidate is picked, which keeps octaves and
//! fifths from lighting up as phantom notes.

use std::sync::Arc;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub const SAMPLE_RATE: f32 = 44_100.0;
pub const FFT_SIZE: usize = 4096;
pub const HOP_SIZE: usize = 1024;
pub const MIN_MIDI: u8 = 21;
pub const MAX_MIDI: u8 = 108;
pub const NUM_NOTES: usize = (MAX_MIDI - MIN_MIDI + 1) as usize;

const NUM_HARMONICS: usize = 14;
const MAX_HARMONIC_HZ: f32 = 5_200.0;
const MAX_POLYPHONY: usize = 6;
/// A candidate must reach this fraction of the strongest salience to count.
const RELATIVE_GATE: f32 = 0.10;
const ABSOLUTE_GATE: f32 = 0.0015;
/// Smoothed level above which a key is considered pressed.
const ACTIVE_LEVEL: f32 = 0.34;
const ATTACK_SECS: f32 = 0.035;
const RELEASE_SECS: f32 = 0.16;

#[inline]
pub fn midi_hz(midi: u8) -> f32 {
    440.0 * 2.0_f32.powf((midi as f32 - 69.0) / 12.0)
}

#[inline]
#[allow(dead_code)]
pub fn hz_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

pub fn midi_name(midi: u8) -> String {
    let name = NOTE_NAMES[midi as usize % 12];
    let octave = midi as i32 / 12 - 1;
    format!("{name}{octave}")
}

pub fn pitch_class_name(pitch_class: u8) -> &'static str {
    NOTE_NAMES[pitch_class as usize % 12]
}

#[derive(Clone, Copy, Default, Debug)]
#[allow(dead_code)] // kept for note-level analysis; the UI no longer prints it
pub struct NoteInfo {
    pub midi: u8,
    /// Deviation from equal temperament in cents, clamped to +/-50.
    pub cents: f32,
    pub level: f32,
}

#[derive(Clone)]
pub struct PitchState {
    /// Smoothed activity for every key, MIDI 21..=108.
    pub levels: [f32; NUM_NOTES],
    pub top: Option<NoteInfo>,
    pub polyphony: u8,
}

/// One sampled slice of keyboard activity, used by the piano-roll style.
#[derive(Clone)]
pub struct NoteFrame {
    /// Audio position the activity belongs to, already offset for the STFT
    /// window so the trail lines up with what the ear hears.
    pub time: f64,
    pub levels: [f32; NUM_NOTES],
}

impl Default for PitchState {
    fn default() -> Self {
        Self {
            levels: [0.0; NUM_NOTES],
            top: None,
            polyphony: 0,
        }
    }
}

pub struct PitchTracker {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    /// Rolling analysis window, oldest sample first.
    buffer: Vec<f32>,
    pending: Vec<f32>,
    pending_len: usize,
    frame: Vec<Complex<f32>>,
    mag: Vec<f32>,
    work_mag: Vec<f32>,
    scratch: [f32; NUM_NOTES],
    target: [f32; NUM_NOTES],
    cents: [f32; NUM_NOTES],
    chosen: [bool; NUM_NOTES],
    levels: [f32; NUM_NOTES],
    state: PitchState,
}

impl PitchTracker {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Self {
            fft,
            window,
            buffer: vec![0.0; FFT_SIZE],
            pending: vec![0.0; HOP_SIZE],
            pending_len: 0,
            frame: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            mag: vec![0.0; FFT_SIZE / 2 + 1],
            work_mag: vec![0.0; FFT_SIZE / 2 + 1],
            scratch: [0.0; NUM_NOTES],
            target: [0.0; NUM_NOTES],
            cents: [0.0; NUM_NOTES],
            chosen: [false; NUM_NOTES],
            levels: [0.0; NUM_NOTES],
            state: PitchState::default(),
        }
    }

    pub fn reset(&mut self) {
        self.buffer.fill(0.0);
        self.pending.fill(0.0);
        self.pending_len = 0;
        self.target.fill(0.0);
        self.cents.fill(0.0);
        self.levels.fill(0.0);
        self.state = PitchState::default();
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.pending[self.pending_len] = sample;
            self.pending_len += 1;
            if self.pending_len == HOP_SIZE {
                self.pending_len = 0;
                self.advance();
                self.analyze();
            }
        }
    }

    pub fn tick(&mut self, dt: f32) -> &PitchState {
        let dt = dt.clamp(0.001, 0.2);
        let attack = 1.0 - (-dt / ATTACK_SECS).exp();
        let release = (-dt / RELEASE_SECS).exp();
        for (level, target) in self.levels.iter_mut().zip(self.target.iter()) {
            if *target > *level {
                *level += (*target - *level) * attack;
            } else {
                *level *= release;
                if *level < 0.02 {
                    *level = 0.0;
                }
            }
        }

        let mut top_index = None;
        let mut top_level = 0.0;
        let mut polyphony = 0_u8;
        for (index, level) in self.levels.iter().enumerate() {
            if *level >= ACTIVE_LEVEL {
                polyphony = polyphony.saturating_add(1);
                if *level > top_level {
                    top_level = *level;
                    top_index = Some(index);
                }
            }
        }

        self.state.levels = self.levels;
        self.state.polyphony = polyphony;
        self.state.top = top_index.map(|index| NoteInfo {
            midi: MIN_MIDI + index as u8,
            cents: self.cents[index],
            level: self.levels[index],
        });
        &self.state
    }

    pub fn state(&self) -> &PitchState {
        &self.state
    }

    fn advance(&mut self) {
        self.buffer.copy_within(HOP_SIZE.., 0);
        let tail = FFT_SIZE - HOP_SIZE;
        self.buffer[tail..].copy_from_slice(&self.pending);
    }

    fn analyze(&mut self) {
        let mut energy = 0.0_f32;
        for i in 0..FFT_SIZE {
            let sample = self.buffer[i];
            energy += sample * sample;
            self.frame[i] = Complex::new(sample * self.window[i], 0.0);
        }
        let rms = (energy / FFT_SIZE as f32).sqrt();

        self.target.fill(0.0);
        self.cents.fill(0.0);
        self.chosen.fill(false);

        if rms < 1e-5 {
            return;
        }

        self.fft.process(&mut self.frame);
        let norm = 2.0 / FFT_SIZE as f32;
        for (i, slot) in self.mag.iter_mut().enumerate() {
            let value = self.frame[i];
            *slot = (value.re * value.re + value.im * value.im).sqrt() * norm;
        }
        self.work_mag.copy_from_slice(&self.mag);

        let mut peak = 0.0_f32;

        for pass in 0..MAX_POLYPHONY {
            self.score_harmonics();
            let Some(best_index) = self.best_candidate() else {
                break;
            };
            let best = self.scratch[best_index];
            if pass == 0 {
                peak = best;
            }
            if best < peak * RELATIVE_GATE || best < ABSOLUTE_GATE {
                break;
            }

            self.chosen[best_index] = true;
            let midi = MIN_MIDI + best_index as u8;
            self.cents[best_index] = self.estimate_cents(midi);
            self.target[best_index] = (best / peak.max(1e-9)).sqrt().clamp(0.0, 1.0);
            self.suppress_harmonics(midi);
        }
    }

    fn score_harmonics(&mut self) {
        let bin_hz = SAMPLE_RATE / FFT_SIZE as f32;
        for (index, slot) in self.scratch.iter_mut().enumerate() {
            let midi = MIN_MIDI + index as u8;
            let f0 = midi_hz(midi);
            let mut sum = 0.0;
            for k in 1..=NUM_HARMONICS {
                let freq = f0 * k as f32;
                if freq >= MAX_HARMONIC_HZ {
                    break;
                }
                // Higher harmonics are common to several candidates; weighting
                // them down keeps the fundamental in charge.
                let weight = 1.0 / (k as f32).sqrt();
                sum += sample_mag(&self.work_mag, freq / bin_hz) * weight;
            }
            *slot = sum;
        }
    }

    fn best_candidate(&self) -> Option<usize> {
        let mut best_index = None;
        let mut best = 0.0;
        for (index, value) in self.scratch.iter().enumerate() {
            if self.chosen[index] {
                continue;
            }
            if *value > best {
                best = *value;
                best_index = Some(index);
            }
        }
        best_index
    }

    fn suppress_harmonics(&mut self, midi: u8) {
        let bin_hz = SAMPLE_RATE / FFT_SIZE as f32;
        let f0 = midi_hz(midi);
        for k in 1..=NUM_HARMONICS {
            let freq = f0 * k as f32;
            if freq >= MAX_HARMONIC_HZ {
                break;
            }
            let center = (freq / bin_hz).round() as isize;
            for bin in (center - 2)..=(center + 2) {
                if bin >= 0 && (bin as usize) < self.work_mag.len() {
                    self.work_mag[bin as usize] *= 0.02;
                }
            }
        }
    }

    fn estimate_cents(&self, midi: u8) -> f32 {
        let bin_hz = SAMPLE_RATE / FFT_SIZE as f32;
        let f0 = midi_hz(midi);
        let mut weighted = 0.0;
        let mut weights = 0.0;
        for k in 1..=NUM_HARMONICS {
            let freq = f0 * k as f32;
            // Peak interpolation gets more precise as k grows, but very high
            // harmonics are noisy. Stay below 3kHz.
            if freq > 3_000.0 {
                break;
            }
            let Some((bin, magnitude)) = peak_near(&self.mag, freq / bin_hz) else {
                continue;
            };
            if magnitude < 1e-5 {
                continue;
            }
            let weight = magnitude * (k as f32).sqrt();
            weighted += (bin * bin_hz / k as f32) * weight;
            weights += weight;
        }
        if weights <= 0.0 {
            return 0.0;
        }
        let estimate = weighted / weights;
        (1200.0 * (estimate / f0).log2()).clamp(-50.0, 50.0)
    }
}

fn sample_mag(mag: &[f32], bin: f32) -> f32 {
    if bin <= 0.0 {
        return 0.0;
    }
    let index = bin.floor() as usize;
    if index + 1 >= mag.len() {
        return 0.0;
    }
    let fraction = bin - index as f32;
    mag[index] * (1.0 - fraction) + mag[index + 1] * fraction
}

/// Parabolic interpolation around the local maximum nearest `center`.
fn peak_near(mag: &[f32], center: f32) -> Option<(f32, f32)> {
    if mag.len() < 3 {
        return None;
    }
    let center = center.round() as isize;
    let low = (center - 2).max(1);
    let high = (center + 2).min(mag.len() as isize - 2);
    if high < low {
        return None;
    }
    let mut best = low;
    for bin in low..=high {
        if mag[bin as usize] > mag[best as usize] {
            best = bin;
        }
    }
    let left = mag[best as usize - 1];
    let middle = mag[best as usize];
    let right = mag[best as usize + 1];
    let denominator = left - 2.0 * middle + right;
    let offset = if denominator.abs() > 1e-12 {
        (0.5 * (left - right) / denominator).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    Some((best as f32 + offset, middle))
}
