use std::f32::consts::PI;

pub const NUM_BANDS: usize = 10;
pub const MIN_GAIN_DB: f32 = -12.0;
pub const MAX_GAIN_DB: f32 = 12.0;
pub const BAND_FREQS: [f32; NUM_BANDS] = [
    31.0, 62.0, 125.0, 250.0, 500.0, 1_000.0, 2_000.0, 4_000.0, 8_000.0, 16_000.0,
];

pub const PRESETS: &[(&str, [f32; NUM_BANDS])] = &[
    ("FLAT", [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]),
    ("POP", [1.0, 3.0, 5.0, 3.0, 0.0, -1.0, -1.0, 1.0, 2.0, 3.0]),
    ("ROCK", [4.0, 3.0, 2.0, 1.0, -1.0, -2.0, 0.0, 2.0, 3.0, 4.0]),
    (
        "CLASS",
        [3.0, 2.0, -1.0, -2.0, -2.0, -1.0, 1.0, 2.0, 3.0, 4.0],
    ),
    ("JAZZ", [2.0, 1.0, 0.0, 1.0, 3.0, 2.0, 0.0, 1.0, 2.0, 3.0]),
    ("ELEC", [3.0, 2.0, 0.0, -1.0, 2.0, 0.0, 1.0, 3.0, 4.0, 5.0]),
    (
        "VOCAL",
        [-2.0, -1.0, 1.0, 3.0, 4.0, 4.0, 3.0, 1.0, 0.0, -1.0],
    ),
    (
        "BASS",
        [6.0, 4.0, 2.0, 0.0, -1.0, -2.0, -1.0, 0.0, 1.0, 2.0],
    ),
    (
        "TREBLE",
        [-1.0, 0.0, 1.0, 2.0, 1.0, 0.0, 2.0, 4.0, 6.0, 8.0],
    ),
    ("DANCE", [4.0, 3.0, 1.0, 0.0, 0.0, -1.0, 0.0, 2.0, 3.0, 4.0]),
];

#[derive(Clone, Copy, Debug)]
pub struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
}

impl Biquad {
    const fn identity() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
        }
    }

    fn peaking(freq: f32, gain_db: f32, q: f32, sample_rate: f32) -> Self {
        let a = 10.0_f32.powf(gain_db / 40.0);
        let w0 = 2.0 * PI * freq / sample_rate.max(1.0);
        let cos_w0 = w0.cos();
        let alpha = w0.sin() / (2.0 * q.max(0.1));
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: (-2.0 * cos_w0) / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: (-2.0 * cos_w0) / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct EqParams {
    pub enabled: bool,
    pub preamp: f32,
    pub bands: [Biquad; NUM_BANDS],
}

impl EqParams {
    pub fn new(gains: &[f32; NUM_BANDS], enabled: bool, sample_rate: u32) -> Self {
        let sample_rate = sample_rate.max(8_000) as f32;
        let mut bands = [Biquad::identity(); NUM_BANDS];
        let mut max_boost = 0.0_f32;

        for (index, &gain) in gains.iter().enumerate() {
            let gain = if gain.is_finite() {
                gain.clamp(MIN_GAIN_DB, MAX_GAIN_DB)
            } else {
                0.0
            };
            max_boost = max_boost.max(gain);
            if gain.abs() > 0.01 {
                bands[index] = Biquad::peaking(BAND_FREQS[index], gain, 1.0, sample_rate);
            }
        }

        let preamp = if enabled && max_boost > 0.0 {
            10.0_f32.powf(-max_boost / 20.0)
        } else {
            1.0
        };

        Self {
            enabled,
            preamp,
            bands,
        }
    }

    pub fn flat(sample_rate: u32) -> Self {
        Self::new(&[0.0; NUM_BANDS], false, sample_rate)
    }
}

#[derive(Clone, Copy, Default)]
struct FilterState {
    z1: f32,
    z2: f32,
}

impl FilterState {
    #[inline]
    fn process(&mut self, band: &Biquad, input: f32) -> f32 {
        let output = band.b0 * input + self.z1;
        self.z1 = band.b1 * input - band.a1 * output + self.z2;
        self.z2 = band.b2 * input - band.a2 * output;
        output
    }
}

pub struct EqProcessor {
    states: [[FilterState; 2]; NUM_BANDS],
    was_enabled: bool,
}

impl Default for EqProcessor {
    fn default() -> Self {
        Self::new()
    }
}

impl EqProcessor {
    pub fn new() -> Self {
        Self {
            states: [[FilterState::default(); 2]; NUM_BANDS],
            was_enabled: false,
        }
    }

    #[inline]
    pub fn process(&mut self, left: &mut f32, right: &mut f32, params: &EqParams) {
        if !params.enabled {
            self.was_enabled = false;
            return;
        }
        if !self.was_enabled {
            self.states = [[FilterState::default(); 2]; NUM_BANDS];
            self.was_enabled = true;
        }

        for (band_index, band) in params.bands.iter().enumerate() {
            *left = self.states[band_index][0].process(band, *left);
            *right = self.states[band_index][1].process(band, *right);
        }
        *left *= params.preamp;
        *right *= params.preamp;
    }
}
