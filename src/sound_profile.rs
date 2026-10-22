//! Whole-track descriptors the AUTO theme reads a song type from.
//!
//! The values are gathered during the beat analysis FFT pass. Each one is an
//! input of the classifier in `song_type.rs`, which was fitted offline on
//! exactly these definitions, so changing one means refitting it.

use rustfft::num_complex::Complex;
use serde::{Deserialize, Serialize};

use crate::beat::percentile;

pub const FEATURES: usize = 28;

// FFT bin ranges for 22.05 kHz audio in 1024-point frames (21.5 Hz per bin).
const WIDE: (usize, usize) = (2, 371); // 43 Hz to 8 kHz
const HIGH: (usize, usize) = (93, 371); // 2 to 8 kHz
const SUB: (usize, usize) = (2, 4); // 43 to 86 Hz
const BASS: (usize, usize) = (5, 9); // 108 to 194 Hz
const LOW_MID: (usize, usize) = (10, 23); // 215 to 495 Hz
const UPPER_MID: (usize, usize) = (24, 92); // 517 Hz to 2 kHz
const PRESENCE: (usize, usize) = (93, 185); // 2 to 4 kHz
const BRILLIANCE: (usize, usize) = (186, 371); // 4 to 8 kHz
const BANDS: [(usize, usize); 6] = [SUB, BASS, LOW_MID, UPPER_MID, PRESENCE, BRILLIANCE];
// Spectral contrast bands: 43 to 194 Hz, then octaves from 215 Hz to 8 kHz.
const CONTRAST_BANDS: [(usize, usize); 7] = [
    (2, 9),
    (10, 18),
    (19, 37),
    (38, 74),
    (75, 148),
    (149, 297),
    (298, 371),
];
// Share of a contrast band's bins averaged as its peaks, and as its valleys.
const CONTRAST_SHARE: f32 = 0.2;
const EPSILON: f32 = 1e-12;

/// Means are over the frames within 30 dB of the loud parts, so intros,
/// fades and gaps between movements do not drag the numbers down.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct SoundProfile {
    /// Mean power, dBFS.
    pub loudness_db: f32,
    /// Mean rectified flux of the wide band: how often the level jumps up.
    pub flux: f32,
    pub low_flux: f32,
    pub mid_flux: f32,
    pub high_flux: f32,
    /// 2–8 kHz power against 28–210 Hz power, dB; low means bass-heavy.
    pub tilt_db: f32,
    /// 43–86 Hz against 108–194 Hz.
    pub sub_db: f32,
    /// 210 Hz–2 kHz against 28 Hz–8 kHz.
    pub mid_share_db: f32,
    /// 215–495 Hz against 517 Hz–2 kHz.
    pub low_mid_db: f32,
    /// 2–4 kHz against 517 Hz–2 kHz.
    pub presence_db: f32,
    /// 4–8 kHz against 2–4 kHz.
    pub brilliance_db: f32,
    /// Spectral flatness, ln of geometric over arithmetic mean, 43 Hz–8 kHz.
    pub flatness: f32,
    /// The same over 2–8 kHz.
    pub high_flatness: f32,
    pub centroid_hz: f32,
    /// One-second loudness, 90th minus 10th percentile, dB.
    pub dynamics_db: f32,
    /// Frame peak over frame power, dB.
    pub crest_db: f32,
    /// Standard deviation of the 28–210 Hz level, dB.
    pub low_spread_db: f32,
    /// Share of frames above the silence gate.
    pub active: f32,
    /// Strongest autocorrelation of the wide-band flux at 0.25–1.5 s lags.
    pub pulse: f32,
    pub low_pulse: f32,
    /// 90th percentile of the wide-band flux over its mean.
    pub onset_peaks: f32,
    /// Peak-to-valley spread per contrast band, log10 power.
    pub contrast: [f32; 7],
}

impl SoundProfile {
    /// The classifier inputs, in the order its weights were fitted.
    pub fn vector(&self) -> [f32; FEATURES] {
        let head = [
            self.loudness_db,
            self.flux,
            self.low_flux,
            self.mid_flux,
            self.high_flux,
            self.tilt_db,
            self.sub_db,
            self.mid_share_db,
            self.low_mid_db,
            self.presence_db,
            self.brilliance_db,
            self.flatness,
            self.high_flatness,
            self.centroid_hz,
            self.dynamics_db,
            self.crest_db,
            self.low_spread_db,
            self.active,
            self.pulse,
            self.low_pulse,
            self.onset_peaks,
        ];
        std::array::from_fn(|index| match head.get(index) {
            Some(value) => *value,
            None => self.contrast[index - head.len()],
        })
    }
}

/// Per-frame curves the beat analysis already has.
pub struct BandCurves<'a> {
    pub power: &'a [f32],
    pub low: &'a [f32],
    pub mid: &'a [f32],
    pub wide: &'a [f32],
    pub high: &'a [f32],
    pub low_flux: &'a [f32],
    pub mid_flux: &'a [f32],
    pub wide_flux: &'a [f32],
    pub high_flux: &'a [f32],
}

/// Collects what the profile needs from each analysis frame.
pub struct ProfileFrames {
    peak: Vec<f32>,
    bands: Vec<[f32; 6]>,
    flatness: Vec<[f32; 2]>,
    centroid: Vec<f32>,
    contrast: Vec<[f32; 7]>,
    log_power: Vec<f32>,
    sorted: Vec<f32>,
}

impl ProfileFrames {
    pub fn with_capacity(frames: usize) -> Self {
        Self {
            peak: Vec::with_capacity(frames),
            bands: Vec::with_capacity(frames),
            flatness: Vec::with_capacity(frames),
            centroid: Vec::with_capacity(frames),
            contrast: Vec::with_capacity(frames),
            log_power: Vec::with_capacity(WIDE.1 - WIDE.0 + 1),
            sorted: Vec::new(),
        }
    }

    /// `samples` is the frame before windowing, `spectrum` its FFT.
    pub fn push(&mut self, samples: &[f32], spectrum: &[Complex<f32>], bin_hz: f32) {
        self.peak
            .push(samples.iter().fold(0.0_f32, |peak, sample| peak.max(sample * sample)));

        let mut bands = [0.0_f32; 6];
        let (mut sum, mut weighted, mut log_sum) = (0.0_f64, 0.0_f64, 0.0_f64);
        let (mut high_sum, mut high_log_sum) = (0.0_f64, 0.0_f64);
        self.log_power.clear();
        for bin in WIDE.0..=WIDE.1 {
            let value = spectrum[bin];
            let power = value.re * value.re + value.im * value.im;
            for (band, (start, end)) in bands.iter_mut().zip(BANDS) {
                if (start..=end).contains(&bin) {
                    *band += power;
                }
            }
            let shifted = power + EPSILON;
            let log = shifted.ln();
            self.log_power.push(log);
            sum += shifted as f64;
            weighted += shifted as f64 * (bin as f32 * bin_hz) as f64;
            log_sum += log as f64;
            if bin >= HIGH.0 {
                high_sum += shifted as f64;
                high_log_sum += log as f64;
            }
        }
        let flatness = |log_sum: f64, sum: f64, count: usize| {
            (log_sum / count as f64 - (sum / count as f64).ln()) as f32
        };
        self.bands.push(bands);
        self.flatness.push([
            flatness(log_sum, sum, WIDE.1 - WIDE.0 + 1),
            flatness(high_log_sum, high_sum, HIGH.1 - HIGH.0 + 1),
        ]);
        self.centroid.push((weighted / sum) as f32);

        let mut contrast = [0.0_f32; 7];
        for (slot, (start, end)) in contrast.iter_mut().zip(CONTRAST_BANDS) {
            self.sorted.clear();
            self.sorted
                .extend_from_slice(&self.log_power[start - WIDE.0..=end - WIDE.0]);
            self.sorted.sort_unstable_by(f32::total_cmp);
            let count = self.sorted.len();
            let take = ((CONTRAST_SHARE * count as f32).round() as usize).clamp(1, count);
            let valley = self.sorted[..take].iter().sum::<f32>();
            let peak = self.sorted[count - take..].iter().sum::<f32>();
            *slot = (peak - valley) / take as f32 / std::f32::consts::LN_10;
        }
        self.contrast.push(contrast);
    }

    pub fn finish(self, curves: &BandCurves, frame_rate: f32) -> Option<SoundProfile> {
        let frames = curves.power.len();
        let db = curves
            .power
            .iter()
            .map(|value| 10.0 * (value + EPSILON).log10())
            .collect::<Vec<f32>>();
        let gate = (percentile(&db, 0.95) - 30.0).max(-60.0);
        let active = (0..frames)
            .filter(|&index| db[index] >= gate)
            .collect::<Vec<usize>>();
        if active.len() < 16 || self.peak.len() != frames {
            return None;
        }
        let mean = |value: &dyn Fn(usize) -> f64| {
            active.iter().map(|&index| value(index)).sum::<f64>() / active.len() as f64
        };
        let level = |values: &[f32]| mean(&|index| values[index] as f64);
        let ratio_db =
            |top: f64, bottom: f64| (10.0 * (top.max(1e-12) / bottom.max(1e-12)).log10()) as f32;
        let band = |slot: usize| mean(&|index| self.bands[index][slot] as f64);
        let [sub, bass, low_mid, upper_mid, presence, brilliance] = std::array::from_fn(band);

        let low_db = active
            .iter()
            .map(|&index| 10.0 * (curves.low[index] as f64 + 1e-12).log10())
            .collect::<Vec<f64>>();
        let low_db_mean = low_db.iter().sum::<f64>() / low_db.len() as f64;
        let low_spread = (low_db
            .iter()
            .map(|value| (value - low_db_mean).powi(2))
            .sum::<f64>()
            / low_db.len() as f64)
            .sqrt();

        let active_flux = active
            .iter()
            .map(|&index| curves.wide_flux[index])
            .collect::<Vec<f32>>();
        let flux = level(curves.wide_flux);

        let profile = SoundProfile {
            loudness_db: (10.0 * level(curves.power).max(1e-12).log10()) as f32,
            flux: flux as f32,
            low_flux: level(curves.low_flux) as f32,
            mid_flux: level(curves.mid_flux) as f32,
            high_flux: level(curves.high_flux) as f32,
            tilt_db: ratio_db(level(curves.high), level(curves.low)),
            sub_db: ratio_db(sub, bass),
            mid_share_db: ratio_db(level(curves.mid), level(curves.wide)),
            low_mid_db: ratio_db(low_mid, upper_mid),
            presence_db: ratio_db(presence, upper_mid),
            brilliance_db: ratio_db(brilliance, presence),
            flatness: mean(&|index| self.flatness[index][0] as f64) as f32,
            high_flatness: mean(&|index| self.flatness[index][1] as f64) as f32,
            centroid_hz: level(self.centroid.as_slice()) as f32,
            dynamics_db: dynamics(curves.power, gate, frame_rate),
            crest_db: mean(&|index| {
                10.0 * (self.peak[index] as f64 / curves.power[index] as f64).log10()
            }) as f32,
            low_spread_db: low_spread as f32,
            active: active.len() as f32 / frames as f32,
            pulse: pulse(curves.wide_flux, frame_rate),
            low_pulse: pulse(curves.low_flux, frame_rate),
            onset_peaks: (percentile(&active_flux, 0.90) as f64 / (flux + 1e-9)) as f32,
            contrast: std::array::from_fn(|slot| {
                mean(&|index| self.contrast[index][slot] as f64) as f32
            }),
        };
        // serde_json writes non-finite floats as null, which would break the cache.
        profile
            .vector()
            .iter()
            .all(|value| value.is_finite())
            .then_some(profile)
    }
}

/// Spread of the loudness of one-second blocks that clear the gate.
fn dynamics(power: &[f32], gate: f32, frame_rate: f32) -> f32 {
    let block = (frame_rate.round() as usize).max(1);
    let levels = power
        .chunks_exact(block)
        .map(|chunk| {
            let mean = chunk.iter().map(|value| *value as f64).sum::<f64>() / block as f64;
            (10.0 * (mean + 1e-12).log10()) as f32
        })
        .filter(|level| *level >= gate)
        .collect::<Vec<f32>>();
    if levels.len() > 4 {
        percentile(&levels, 0.90) - percentile(&levels, 0.10)
    } else {
        0.0
    }
}

/// How regular an onset curve is: its strongest normalised autocorrelation
/// at lags from 0.25 to 1.5 seconds.
fn pulse(onset: &[f32], frame_rate: f32) -> f32 {
    let count = onset.len();
    if count == 0 {
        return 0.0;
    }
    let mean = onset.iter().map(|value| *value as f64).sum::<f64>() / count as f64;
    let centred = onset
        .iter()
        .map(|value| *value as f64 - mean)
        .collect::<Vec<f64>>();
    let correlation = |lag: usize| {
        centred[..count - lag]
            .iter()
            .zip(&centred[lag..])
            .map(|(left, right)| left * right)
            .sum::<f64>()
    };
    let energy = correlation(0);
    let shortest = (frame_rate * 0.25) as usize;
    let longest = ((frame_rate * 1.5) as usize).min(count - 1);
    if energy <= 0.0 || shortest > longest {
        return 0.0;
    }
    (shortest..=longest)
        .map(|lag| correlation(lag) / energy)
        .fold(f64::MIN, f64::max) as f32
}
