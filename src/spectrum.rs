use std::sync::Arc;
use std::time::Instant;

use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};

pub const NUM_BARS: usize = 100;
pub const FFT_SIZE: usize = 1024;
pub const ANALYSIS_RATE: u32 = 44_100;
pub const MAX_FREQ: f32 = 8_000.0;
pub const MAX_DB_VALUE: f32 = 90.0;
const UI_STEP: f32 = 0.025;
const VISUAL_ALPHA: f32 = 0.28;
const LEVEL_GAIN: f32 = 2.4;
const PEAK_DECAY: f32 = 0.995;

#[derive(Clone, Copy, Default, Debug)]
pub struct VisualState {
    pub energy: f32,
    pub bass: f32,
    pub mid: f32,
    pub treble: f32,
    pub kick: f32,
    pub phase: f32,
}

pub struct SpectrumEngine {
    fft: Arc<dyn Fft<f32>>,
    window: Vec<f32>,
    chunk: Vec<f32>,
    chunk_len: usize,
    frame: Vec<Complex<f32>>,
    mag: Vec<f32>,
    bar_start: Vec<usize>,
    bar_end: Vec<usize>,
    bars: Vec<f32>,
    chunk_db: Vec<f32>,
    display: Vec<f32>,
    last_db: Vec<f32>,
    has_last: bool,
    beat: BeatDetector,
    visual: VisualFeatures,
    phase: f32,
    visual_accum: f32,
    last_tick: Instant,
    cached: VisualState,
    has_new: bool,
    audio_time: f64,
    beat_state: (u64, f32),
    chunk_count: u32,
    beat_block_until: u32,
}

impl SpectrumEngine {
    pub fn new() -> Self {
        let mut planner = FftPlanner::new();
        let fft = planner.plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / (FFT_SIZE - 1) as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect::<Vec<_>>();

        let freqs = (0..=FFT_SIZE / 2)
            .map(|i| i as f32 * ANALYSIS_RATE as f32 / FFT_SIZE as f32)
            .collect::<Vec<_>>();
        let max_edge = MAX_FREQ.min(*freqs.last().unwrap_or(&MAX_FREQ));
        let mut edges = Vec::with_capacity(NUM_BARS + 1);
        for i in 0..=NUM_BARS {
            let t = i as f32 / NUM_BARS as f32;
            let edge = max_edge * t.powf(1.2);
            let idx = freqs
                .partition_point(|freq| *freq < edge)
                .min(freqs.len() - 1);
            edges.push(idx);
        }

        Self {
            fft,
            window,
            chunk: vec![0.0; FFT_SIZE],
            chunk_len: 0,
            frame: vec![Complex::new(0.0, 0.0); FFT_SIZE],
            mag: vec![0.0; FFT_SIZE / 2 + 1],
            bar_start: edges[..NUM_BARS].to_vec(),
            bar_end: edges[1..].to_vec(),
            bars: vec![0.0; NUM_BARS],
            chunk_db: vec![0.0; NUM_BARS],
            display: vec![0.0; NUM_BARS],
            last_db: vec![0.0; NUM_BARS],
            has_last: false,
            beat: BeatDetector::new(),
            visual: VisualFeatures::new(),
            phase: 0.0,
            visual_accum: 0.0,
            last_tick: Instant::now(),
            cached: VisualState::default(),
            has_new: false,
            audio_time: 0.0,
            beat_state: (0, 0.0),
            chunk_count: 0,
            beat_block_until: 32,
        }
    }

    pub fn reset(&mut self) {
        self.chunk_len = 0;
        self.bars.fill(0.0);
        self.last_db.fill(0.0);
        self.has_last = false;
        self.beat.reset();
        self.visual.reset();
        self.has_new = false;
        self.audio_time = 0.0;
        self.beat_state = (0, 0.0);
        self.chunk_count = 0;
        self.beat_block_until = 32;
        self.visual_accum = 0.0;
        self.cached = VisualState {
            phase: self.phase,
            ..VisualState::default()
        };
    }

    pub fn reset_beat(&mut self) {
        self.beat.reset();
        self.beat_block_until = self.chunk_count.saturating_add(8);
        self.visual.kick = 0.0;
        self.visual.last_beat_sequence = 0;
    }

    pub fn clear_display(&mut self) {
        self.chunk_len = 0;
        self.bars.fill(0.0);
        self.last_db.fill(0.0);
        self.has_last = false;
        self.has_new = false;
        self.visual.reset();
        self.beat.reset();
        self.beat_state = (0, 0.0);
        self.chunk_count = 0;
        self.beat_block_until = 16;
    }

    /// Fade the currently displayed spectrum towards zero without touching
    /// the analyser state, so stopping playback lets the bars fall naturally.
    pub fn decay_display(&mut self, dt: f32) {
        let factor = (-(dt.max(0.0)) / 0.18).exp();
        for bar in &mut self.bars {
            *bar *= factor;
            if *bar < 0.2 {
                *bar = 0.0;
            }
        }
        self.last_db.fill(0.0);
        self.has_last = false;
        self.has_new = false;
    }

    pub fn has_signal(&self) -> bool {
        self.bars.iter().any(|bar| *bar > 0.0)
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &sample in samples {
            self.chunk[self.chunk_len] = sample;
            self.chunk_len += 1;
            if self.chunk_len == FFT_SIZE {
                self.process_chunk();
                self.chunk_len = 0;
            }
        }
    }

    fn process_chunk(&mut self) {
        self.audio_time += FFT_SIZE as f64 / ANALYSIS_RATE as f64;

        for i in 0..FFT_SIZE {
            self.frame[i] = Complex::new(self.chunk[i] * self.window[i], 0.0);
        }
        self.fft.process(&mut self.frame);

        for (i, slot) in self.mag.iter_mut().enumerate() {
            let c = self.frame[i];
            *slot = (c.re * c.re + c.im * c.im).sqrt() / FFT_SIZE as f32;
        }

        self.chunk_count = self.chunk_count.saturating_add(1);
        let allow_beats = self.chunk_count >= self.beat_block_until;
        let beat_state = self.beat.process(&self.mag, self.audio_time, allow_beats);

        for i in 0..NUM_BARS {
            let start = self.bar_start[i];
            let end = self.bar_end[i];
            let peak = if end > start {
                self.mag[start..end].iter().copied().fold(0.0_f32, f32::max)
            } else {
                self.mag.get(start).copied().unwrap_or(0.0)
            };
            self.chunk_db[i] = 400.0 * (1.0 + peak.sqrt()).ln();
        }

        if self.has_last {
            for i in 0..NUM_BARS {
                self.display[i] = self.chunk_db[i] * 0.6 + self.last_db[i] * 0.4;
            }
        } else {
            self.display.copy_from_slice(&self.chunk_db);
            self.has_last = true;
        }
        self.last_db.copy_from_slice(&self.display);
        self.bars.copy_from_slice(&self.display);
        self.has_new = true;
        self.beat_state = beat_state;
    }

    pub fn tick(&mut self, playing: bool) -> VisualState {
        let now = Instant::now();
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.001, 0.2);
        self.last_tick = now;
        self.phase = (self.phase + 0.18 * dt) % std::f32::consts::TAU;

        self.visual_accum += dt;
        if self.visual_accum >= UI_STEP {
            self.visual_accum %= UI_STEP;
            let heights = if self.has_new {
                self.has_new = false;
                Some(self.bars.as_slice())
            } else {
                None
            };
            self.visual.update(heights, playing, self.beat_state);
            self.cached = VisualState {
                energy: self.visual.energy,
                bass: self.visual.bass,
                mid: self.visual.mid,
                treble: self.visual.treble,
                kick: self.visual.kick,
                phase: self.phase,
            };
        }
        self.cached.phase = self.phase;
        self.cached
    }

    pub fn bars(&self) -> &[f32] {
        &self.bars
    }

    pub fn visual(&self) -> VisualState {
        self.cached
    }
}

struct BeatDetector {
    low_mask: Vec<usize>,
    wide_mask: Vec<usize>,
    mid_mask: Vec<usize>,
    sensitivity: f32,
    min_interval: f64,
    output_delay: f64,
    soft_pulses: bool,
    low: BandTracker,
    wide: BandTracker,
    mid: BandTracker,
    prev_score: f32,
    prev_score2: f32,
    prev_low: f32,
    lock_until: f64,
    score_floor: Option<f32>,
    score_spread: f32,
    prev_soft: f32,
    prev_soft2: f32,
    soft_lock_until: f64,
    soft_floor: Option<f32>,
    soft_spread: f32,
    pending: Vec<(f64, f32)>,
    sequence: u64,
    strength: f32,
}

impl BeatDetector {
    fn new() -> Self {
        let freqs = (0..=FFT_SIZE / 2)
            .map(|i| i as f32 * ANALYSIS_RATE as f32 / FFT_SIZE as f32)
            .collect::<Vec<_>>();
        let low_mask = freqs
            .iter()
            .enumerate()
            .filter(|(_, freq)| **freq >= 28.0 && **freq <= 210.0)
            .map(|(i, _)| i)
            .collect();
        let wide_mask = freqs
            .iter()
            .enumerate()
            .filter(|(_, freq)| **freq >= 28.0 && **freq <= 8_000.0)
            .map(|(i, _)| i)
            .collect();
        let mid_mask = freqs
            .iter()
            .enumerate()
            .filter(|(_, freq)| **freq > 210.0 && **freq <= 2_000.0)
            .map(|(i, _)| i)
            .collect();

        Self {
            low_mask,
            wide_mask,
            mid_mask,
            sensitivity: 0.42,
            min_interval: 0.130,
            output_delay: 0.0,
            soft_pulses: true,
            low: BandTracker::new(),
            wide: BandTracker::new(),
            mid: BandTracker::new(),
            prev_score: 0.0,
            prev_score2: 0.0,
            prev_low: 0.0,
            lock_until: 0.0,
            score_floor: None,
            score_spread: 0.1,
            prev_soft: 0.0,
            prev_soft2: 0.0,
            soft_lock_until: 0.0,
            soft_floor: None,
            soft_spread: 0.1,
            pending: Vec::with_capacity(8),
            sequence: 0,
            strength: 0.0,
        }
    }

    fn reset(&mut self) {
        self.low.reset();
        self.wide.reset();
        self.mid.reset();
        self.prev_score = 0.0;
        self.prev_score2 = 0.0;
        self.prev_low = 0.0;
        self.lock_until = 0.0;
        self.score_floor = None;
        self.score_spread = 0.1;
        self.prev_soft = 0.0;
        self.prev_soft2 = 0.0;
        self.soft_lock_until = 0.0;
        self.soft_floor = None;
        self.soft_spread = 0.1;
        self.pending.clear();
        self.sequence = 0;
        self.strength = 0.0;
    }

    fn process(&mut self, mag: &[f32], now: f64, allow_beats: bool) -> (u64, f32) {
        let (low_flux, low_relative, low_scale) = self.low.update(mag, &self.low_mask);
        let (wide_flux, _, wide_scale) = self.wide.update(mag, &self.wide_mask);
        let Some(low_scale) = low_scale else {
            return self.snapshot(now);
        };
        let wide_scale = wide_scale.unwrap_or(0.0);

        let low_norm = low_flux / low_scale;
        let wide_norm = if wide_scale > 0.0 {
            wide_flux / wide_scale
        } else {
            0.0
        };
        let (score, low_driven) = if low_scale >= 0.30 * wide_scale.max(low_scale) {
            (low_norm + 0.25 * wide_norm, true)
        } else {
            (wide_norm + 0.5 * low_norm, false)
        };

        let strictness = 0.7 + 3.4 * self.sensitivity;
        let threshold = self.track_threshold(score, strictness).max(0.10);
        let is_peak = self.prev_score >= self.prev_score2 && self.prev_score > score;
        let loud_enough = low_relative >= 0.004;
        let has_low = !low_driven || self.prev_low >= 0.20;
        let mut hit = false;

        if allow_beats
            && is_peak
            && loud_enough
            && has_low
            && self.prev_score > threshold
            && now >= self.lock_until
        {
            let over = (self.prev_score - threshold) / threshold.max(0.1);
            let strength = (0.62 + 0.38 * over.min(1.0)).clamp(0.62, 1.0);
            self.lock_until = now + self.min_interval;
            self.enqueue(now + self.output_delay, strength);
            hit = true;
        }

        if self.soft_pulses {
            self.process_soft(mag, now, hit, allow_beats);
        }

        self.prev_score2 = self.prev_score;
        self.prev_score = score;
        self.prev_low = low_norm;
        self.snapshot(now)
    }

    fn process_soft(&mut self, mag: &[f32], now: f64, hard_hit: bool, allow_beats: bool) {
        let (flux, relative, scale) = self.mid.update(mag, &self.mid_mask);
        let Some(scale) = scale else {
            return;
        };
        let score = flux / scale;
        let strictness = 0.7 + 3.4 * self.sensitivity;
        if self.soft_floor.is_none() {
            self.soft_floor = Some(score);
        }
        let floor = self.soft_floor.unwrap_or(score);
        let floor = floor + (score - floor) * 0.05;
        self.soft_floor = Some(floor);
        self.soft_spread += (score - floor).abs().max(0.0) * 0.05 - self.soft_spread * 0.05;
        let threshold = (floor + strictness * self.soft_spread.max(0.02)).max(0.10);

        let is_peak = self.prev_soft >= self.prev_soft2 && self.prev_soft > score;
        if allow_beats
            && !hard_hit
            && is_peak
            && relative >= 0.004
            && self.prev_soft > threshold
            && now >= self.soft_lock_until
            && now >= self.lock_until - self.min_interval * 0.5
        {
            let over = (self.prev_soft - threshold) / threshold.max(0.1);
            let strength = (0.18 + 0.16 * over.min(1.0)).clamp(0.18, 0.34);
            self.soft_lock_until = now + 0.15;
            self.enqueue(now + self.output_delay, strength);
        }

        self.prev_soft2 = self.prev_soft;
        self.prev_soft = score;
    }

    fn track_threshold(&mut self, score: f32, strictness: f32) -> f32 {
        if self.score_floor.is_none() {
            self.score_floor = Some(score);
        }
        let floor = self.score_floor.unwrap_or(score);
        let floor = floor + (score - floor) * 0.05;
        self.score_floor = Some(floor);
        self.score_spread += (score - floor).abs() * 0.05 - self.score_spread * 0.05;
        floor + strictness * self.score_spread.max(0.02)
    }

    fn enqueue(&mut self, due: f64, strength: f32) {
        self.pending.push((due, strength));
        if self.pending.len() > 8 {
            let cut = self.pending.len() - 8;
            self.pending.drain(0..cut);
        }
    }

    fn snapshot(&mut self, now: f64) -> (u64, f32) {
        let mut due = 0_u64;
        let mut strength = 0.0_f32;
        self.pending.retain(|(due_time, value)| {
            if *due_time <= now {
                due += 1;
                strength = strength.max(*value);
                false
            } else {
                true
            }
        });
        if due > 0 {
            self.sequence += due;
            self.strength = strength;
        }
        (self.sequence, self.strength)
    }
}

struct BandTracker {
    reference: Option<f32>,
    prev: Option<f32>,
    prev2: Option<f32>,
    scale: Option<f32>,
}

impl BandTracker {
    fn new() -> Self {
        Self {
            reference: None,
            prev: None,
            prev2: None,
            scale: None,
        }
    }

    fn reset(&mut self) {
        *self = Self::new();
    }

    fn update(&mut self, mag: &[f32], mask: &[usize]) -> (f32, f32, Option<f32>) {
        if mask.is_empty() {
            return (0.0, 0.0, None);
        }
        let energy = mask
            .iter()
            .map(|&index| mag[index] * mag[index])
            .sum::<f32>();
        let reference = self.reference.unwrap_or_else(|| energy.max(1e-12));
        let alpha = if energy > reference { 0.10 } else { 0.02 };
        let reference = reference + (energy - reference) * alpha;
        self.reference = Some(reference);
        let relative = energy / reference.max(1e-12);
        let curve = (relative * 40.0).ln_1p();

        let flux = if let Some(previous) = self.prev {
            let baseline = self.prev2.map_or(previous, |prev2| previous.max(prev2));
            (curve - baseline).max(0.0)
        } else {
            0.0
        };
        self.prev2 = self.prev;
        self.prev = Some(curve);

        let scale = if let Some(scale) = self.scale {
            let target = if flux > scale { 0.25 } else { 0.01 };
            let next = (scale + (flux - scale) * target).max(0.02);
            self.scale = Some(next);
            next
        } else {
            let initial = flux.max(0.05);
            self.scale = Some(initial);
            initial
        };

        (flux, relative, Some(scale))
    }
}

struct VisualFeatures {
    energy: f32,
    bass: f32,
    mid: f32,
    treble: f32,
    kick: f32,
    peak_hold: f32,
    last_beat_sequence: u64,
    raw: Vec<f32>,
    norm: Vec<f32>,
    sort_buf: Vec<f32>,
}

impl VisualFeatures {
    fn new() -> Self {
        Self {
            energy: 0.0,
            bass: 0.0,
            mid: 0.0,
            treble: 0.0,
            kick: 0.0,
            peak_hold: 0.25,
            last_beat_sequence: 0,
            raw: vec![0.0; NUM_BARS],
            norm: vec![0.0; NUM_BARS],
            sort_buf: Vec::with_capacity(NUM_BARS),
        }
    }

    fn reset(&mut self) {
        self.energy = 0.0;
        self.bass = 0.0;
        self.mid = 0.0;
        self.treble = 0.0;
        self.kick = 0.0;
        self.peak_hold = 0.25;
        self.last_beat_sequence = 0;
    }

    fn update(&mut self, heights: Option<&[f32]>, playing: bool, beat_state: (u64, f32)) {
        if playing {
            self.update_external_beat(beat_state);
        }

        if playing && heights.is_none() {
            return;
        }

        if !playing || heights.is_none() {
            let decay = 0.82;
            self.energy *= decay;
            self.bass *= decay;
            self.mid *= decay;
            self.treble *= decay;
            self.kick *= 0.78;
            if self.kick < 0.02 {
                self.kick = 0.0;
            }
            if self.energy < 0.01 {
                self.reset();
            }
            return;
        }

        let heights = heights.unwrap();
        self.raw.resize(heights.len(), 0.0);
        for (slot, value) in self.raw.iter_mut().zip(heights.iter()) {
            *slot = (*value / MAX_DB_VALUE).clamp(0.0, 1.0);
        }
        let frame_peak = percentile(&self.raw, 0.92, &mut self.sort_buf);
        self.peak_hold = frame_peak.max(self.peak_hold * PEAK_DECAY).max(0.12);
        let scale = LEVEL_GAIN / self.peak_hold.max(0.12);
        self.norm.resize(self.raw.len(), 0.0);
        for (slot, value) in self.norm.iter_mut().zip(self.raw.iter()) {
            *slot = (*value * scale).clamp(0.0, 1.0);
        }

        let n = self.norm.len();
        let i0 = ((n as f32 * 0.12) as usize).max(1).min(n);
        let i1 = ((n as f32 * 0.45) as usize).max(i0 + 1).min(n);
        let bass = mean(&self.norm[..i0]);
        let mid = mean(&self.norm[i0..i1]);
        let treble = mean(&self.norm[i1..]);
        let energy = (0.50 * bass + 0.35 * mid + 0.15 * treble).min(1.0);

        let smooth = VISUAL_ALPHA;
        self.bass = self.bass * (1.0 - smooth) + bass * smooth;
        self.mid = self.mid * (1.0 - smooth) + mid * smooth;
        self.treble = self.treble * (1.0 - smooth) + treble * smooth;
        self.energy = self.energy * (1.0 - smooth) + energy * smooth;
    }

    fn update_external_beat(&mut self, beat_state: (u64, f32)) {
        let (sequence, strength) = beat_state;
        if sequence < self.last_beat_sequence {
            self.last_beat_sequence = sequence;
        } else if sequence > self.last_beat_sequence {
            self.kick = self.kick.max(strength.clamp(0.0, 1.0));
            self.last_beat_sequence = sequence;
            return;
        }
        self.kick *= 0.78;
        if self.kick < 0.02 {
            self.kick = 0.0;
        }
    }
}

fn mean(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    values.iter().sum::<f32>() / values.len() as f32
}

fn percentile(values: &[f32], q: f32, buffer: &mut Vec<f32>) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    buffer.clear();
    buffer.extend_from_slice(values);
    buffer.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let pos = (buffer.len() - 1) as f32 * q.clamp(0.0, 1.0);
    let lower = pos.floor() as usize;
    let upper = pos.ceil() as usize;
    let frac = pos - lower as f32;
    buffer[lower] + (buffer[upper] - buffer[lower]) * frac
}
