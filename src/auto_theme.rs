//! AUTO theme: a gradient generated from the playing track's song type.
//!
//! Each type (see `song_type.rs`) has its own four-corner palette. Within a
//! type, four things about the track vary the look: brighter or duller sound
//! turns the hues, harder or softer hitting scales chroma and glow, a more or
//! less regular rhythm turns the gradient, and clearer or noisier treble
//! spreads the hues of the two diagonals. All types share one lightness
//! layout, so a type change on the next track mostly moves hue.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::song_type::{self, SongType, TYPES};
use crate::sound_profile::SoundProfile;
use crate::theme::{self, Background, Look, Rgb, Seeds};

const FADE_SECONDS: f32 = 1.5;
// Hue turn at tone ±1, degrees, and chroma change at drive ±1.
const TONE_HUE: f32 = 14.0;
const DRIVE_CHROMA: f32 = 0.22;
// How far the corner colours move round the screen at pulse ±1, in corners;
// 0.4 turns the gradient by 36 degrees.
const GRADIENT_TURN: f32 = 0.4;
// Hue added to one diagonal and taken from the other at clarity ±1, degrees.
const CLARITY_HUE: f32 = 12.0;
// Corner lightness, top-left, top-right, bottom-right, bottom-left, in the
// range of the built-in GRADIENT themes. Dark looks glow towards the bottom
// right and light ones darken a little there, before the pulse turn.
const DARK_LIGHTNESS: [f32; 4] = [0.195, 0.215, 0.285, 0.250];
const LIGHT_LIGHTNESS: [f32; 4] = [0.955, 0.930, 0.912, 0.950];
// Light looks keep the palette hues at lower chroma and a darker accent.
const LIGHT_CHROMA: f32 = 0.55;
const LIGHT_ACCENT_LIGHTNESS: f32 = 0.64;
const LIGHT_ACCENT_CHROMA: f32 = 1.1;
const LIGHT_TINT: f32 = 0.04;

struct Palette {
    /// OKLCH corner hues, in the corner order of `DARK_LIGHTNESS`.
    hues: [f32; 4],
    /// Corner chroma on dark backgrounds.
    chroma: [f32; 4],
    accent_hue: f32,
    accent_chroma: f32,
    /// Accent lightness on dark backgrounds.
    accent_lightness: f32,
    tint: f32,
    /// Added to the dark corner lightness.
    dark_lift: f32,
}

// Rows follow `SongType::ALL`. Types the classifier tends to mix up keep
// neighbouring hues, so a miss looks close to the right answer.
const PALETTES: [Palette; TYPES] = [
    // PIANO: moonlit indigo and steel blue, the calmest.
    Palette {
        hues: [262.0, 238.0, 300.0, 222.0],
        chroma: [0.035, 0.030, 0.060, 0.042],
        accent_hue: 285.0,
        accent_chroma: 0.09,
        accent_lightness: 0.78,
        tint: 0.28,
        dark_lift: 0.0,
    },
    // EPIC: navy and teal under an ember glow.
    Palette {
        hues: [255.0, 218.0, 58.0, 32.0],
        chroma: [0.050, 0.040, 0.085, 0.065],
        accent_hue: 75.0,
        accent_chroma: 0.13,
        accent_lightness: 0.80,
        tint: 0.36,
        dark_lift: 0.0,
    },
    // BALLAD: plum, mauve, rose and lilac.
    Palette {
        hues: [308.0, 342.0, 12.0, 285.0],
        chroma: [0.045, 0.045, 0.080, 0.050],
        accent_hue: 2.0,
        accent_chroma: 0.11,
        accent_lightness: 0.78,
        tint: 0.32,
        dark_lift: 0.0,
    },
    // POP: candy violet, aqua, pink and peach.
    Palette {
        hues: [290.0, 228.0, 352.0, 42.0],
        chroma: [0.060, 0.050, 0.100, 0.075],
        accent_hue: 345.0,
        accent_chroma: 0.15,
        accent_lightness: 0.76,
        tint: 0.40,
        dark_lift: 0.0,
    },
    // ROCK: charcoal with crimson and burnt orange.
    Palette {
        hues: [18.0, 300.0, 28.0, 52.0],
        chroma: [0.030, 0.022, 0.105, 0.065],
        accent_hue: 32.0,
        accent_chroma: 0.16,
        accent_lightness: 0.70,
        tint: 0.42,
        dark_lift: 0.0,
    },
    // ELEC: neon blue, cyan, magenta and violet.
    Palette {
        hues: [266.0, 205.0, 326.0, 292.0],
        chroma: [0.075, 0.060, 0.105, 0.080],
        accent_hue: 205.0,
        accent_chroma: 0.13,
        accent_lightness: 0.82,
        tint: 0.42,
        dark_lift: 0.0,
    },
    // PHONK: near-black purple and blood red, a shade darker than the rest.
    Palette {
        hues: [292.0, 262.0, 6.0, 332.0],
        chroma: [0.055, 0.035, 0.100, 0.070],
        accent_hue: 352.0,
        accent_chroma: 0.17,
        accent_lightness: 0.68,
        tint: 0.44,
        dark_lift: -0.02,
    },
];

/// The type, then four -1..1 places next to what is usual for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mood {
    pub kind: SongType,
    /// Duller to brighter; turns all the hues.
    pub tone: f32,
    /// Softer to harder-hitting; scales chroma and glow.
    pub drive: f32,
    /// Looser to more regular rhythm; turns the gradient.
    pub pulse: f32,
    /// Noisier to clearer treble; pulls the diagonals apart in hue.
    pub clarity: f32,
}

impl Mood {
    pub fn from_profile(profile: &SoundProfile) -> Self {
        let (kind, [tone, drive, pulse, clarity]) = song_type::classify(profile);
        Self {
            kind,
            tone,
            drive,
            pulse,
            clarity,
        }
        .sanitized()
    }

    /// Persisted values come from a hand-editable file.
    pub fn sanitized(self) -> Self {
        let clean = |value: f32| {
            if value.is_finite() {
                value.clamp(-1.0, 1.0)
            } else {
                0.0
            }
        };
        Self {
            kind: self.kind,
            tone: clean(self.tone),
            drive: clean(self.drive),
            pulse: clean(self.pulse),
            clarity: clean(self.clarity),
        }
    }

    /// Name for a saved copy of the look.
    pub fn name(self) -> String {
        format!("AUTO {}", self.kind.label())
    }
}

pub fn look(mood: Mood, dark: bool) -> Look {
    theme::derive(&seeds(mood.sanitized(), dark))
}

fn seeds(mood: Mood, dark: bool) -> Seeds {
    let palette = &PALETTES[mood.kind as usize];
    let turn = TONE_HUE * mood.tone;
    let drive = mood.drive;
    let vivid = 1.0 + DRIVE_CHROMA * drive;
    let (lightness, chroma, accent_lightness, accent_chroma, tint) = if dark {
        let mut lightness = DARK_LIGHTNESS.map(|value| value + palette.dark_lift);
        lightness[0] -= 0.005 * drive;
        lightness[2] += 0.012 * drive;
        lightness[3] += 0.012 * drive;
        (
            lightness,
            vivid,
            palette.accent_lightness,
            palette.accent_chroma * vivid,
            palette.tint,
        )
    } else {
        let mut lightness = LIGHT_LIGHTNESS;
        lightness[2] -= 0.008 * drive;
        (
            lightness,
            LIGHT_CHROMA * vivid,
            LIGHT_ACCENT_LIGHTNESS,
            LIGHT_ACCENT_CHROMA * palette.accent_chroma * vivid,
            palette.tint + LIGHT_TINT,
        )
    };
    let shift = GRADIENT_TURN * mood.pulse;
    let twist = CLARITY_HUE * mood.clarity;
    let corners = std::array::from_fn(|index| {
        // The screen corner lands between two palette corners and blends them.
        let position = index as f32 - shift;
        let from = (position.floor() as i32).rem_euclid(4) as usize;
        let to = (from + 1) % 4;
        let t = position - position.floor();
        let mix = |values: [f32; 4]| values[from] + (values[to] - values[from]) * t;
        let hue_step = (palette.hues[to] - palette.hues[from] + 180.0).rem_euclid(360.0) - 180.0;
        let diagonal = if index % 2 == 0 { twist } else { -twist };
        oklch(
            mix(lightness),
            mix(palette.chroma) * chroma,
            palette.hues[from] + hue_step * t + turn + diagonal,
        )
    });
    Seeds {
        background: Background::Corners(corners),
        accent: oklch(accent_lightness, accent_chroma, palette.accent_hue + turn),
        tint,
        dark: Some(dark),
    }
}

/// OKLCH to sRGB, lowering chroma until the colour fits the gamut.
fn oklch(lightness: f32, chroma: f32, hue: f32) -> Rgb {
    let (sin, cos) = (hue as f64).to_radians().sin_cos();
    let lightness = lightness as f64;
    let linear = |chroma: f64| oklab_to_linear(lightness, chroma * cos, chroma * sin);
    let fits = |rgb: [f64; 3]| rgb.iter().all(|channel| (-1e-4..=1.0 + 1e-4).contains(channel));
    let mut chroma = chroma as f64;
    if !fits(linear(chroma)) {
        let (mut low, mut high) = (0.0, chroma);
        for _ in 0..16 {
            let mid = 0.5 * (low + high);
            if fits(linear(mid)) {
                low = mid;
            } else {
                high = mid;
            }
        }
        chroma = low;
    }
    linear(chroma).map(encode_srgb)
}

fn oklab_to_linear(l: f64, a: f64, b: f64) -> [f64; 3] {
    let l_ = (l + 0.396_337_777_4 * a + 0.215_803_757_3 * b).powi(3);
    let m_ = (l - 0.105_561_345_8 * a - 0.063_854_172_8 * b).powi(3);
    let s_ = (l - 0.089_484_177_5 * a - 1.291_485_548_0 * b).powi(3);
    [
        4.076_741_662_1 * l_ - 3.307_711_591_3 * m_ + 0.230_969_929_2 * s_,
        -1.268_438_004_6 * l_ + 2.609_757_401_1 * m_ - 0.341_319_396_5 * s_,
        -0.004_196_086_3 * l_ - 0.703_418_614_7 * m_ + 1.707_614_701_0 * s_,
    ]
}

fn encode_srgb(linear: f64) -> u8 {
    let linear = linear.clamp(0.0, 1.0);
    let encoded = if linear <= 0.003_130_8 {
        12.92 * linear
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (encoded * 255.0).round() as u8
}

/// Cross-fade from whatever is on screen to a new look.
#[derive(Clone, Copy)]
pub struct Fade {
    from: Look,
    to: Look,
    started: Instant,
}

impl Fade {
    pub fn new(from: Look, to: Look) -> Self {
        Self {
            from,
            to,
            started: Instant::now(),
        }
    }

    /// The look to show now, and whether the fade has finished.
    pub fn sample(&self, now: Instant) -> (Look, bool) {
        let t = now.duration_since(self.started).as_secs_f32() / FADE_SECONDS;
        if t >= 1.0 {
            return (self.to, true);
        }
        let eased = t * t * (3.0 - 2.0 * t);
        (self.from.blend(&self.to, eased), false)
    }
}
