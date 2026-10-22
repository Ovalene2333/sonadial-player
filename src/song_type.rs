//! Song type for the AUTO theme, read from a track's sound profile.
//!
//! A shrunk linear discriminant over standardised profile features, fitted
//! offline on 112 hand-labelled tracks. It gets about 63% of them right in
//! leave-one-out tests, and most misses land on a neighbouring type such as
//! POP for ELEC or BALLAD for EPIC.

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::sound_profile::{SoundProfile, FEATURES};

pub const TYPES: usize = 7;

/// The order matches the model rows below and the AUTO palettes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SongType {
    Piano,
    Epic,
    Ballad,
    #[default]
    Pop,
    Rock,
    Elec,
    /// Also takes most hip-hop and trap.
    Phonk,
}

impl SongType {
    pub const ALL: [SongType; TYPES] = [
        SongType::Piano,
        SongType::Epic,
        SongType::Ballad,
        SongType::Pop,
        SongType::Rock,
        SongType::Elec,
        SongType::Phonk,
    ];

    pub fn label(self) -> &'static str {
        match self {
            SongType::Piano => "PIANO",
            SongType::Epic => "EPIC",
            SongType::Ballad => "BALLAD",
            SongType::Pop => "POP",
            SongType::Rock => "ROCK",
            SongType::Elec => "ELEC",
            SongType::Phonk => "PHONK",
        }
    }
}

impl Serialize for SongType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

impl<'de> Deserialize<'de> for SongType {
    /// Unknown names fall back to POP, so a hand-edited state file still loads.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let label = String::deserialize(deserializer)?;
        Ok(Self::ALL
            .into_iter()
            .find(|kind| kind.label().eq_ignore_ascii_case(label.trim()))
            .unwrap_or_default())
    }
}

// Positions in `SoundProfile::vector`.
const LOUDNESS: usize = 0;
const FLUX: usize = 1;
const TILT: usize = 5;
const PULSE: usize = 18;
const LOW_PULSE: usize = 19;
const CONTRAST_HIGH: usize = 26;
const CONTRAST_TOP: usize = 27;
// Standardised features are clamped so one odd value cannot decide alone.
const Z_LIMIT: f32 = 6.0;

/// A direction a track can differ in from typical tracks of its type: the
/// mean of some standardised features, measured from the type's centre.
struct Axis {
    features: &'static [usize],
    centre: [f32; TYPES],
    /// Distance from the centre that reads as -1 or 1.
    spread: f32,
}

pub const AXES: usize = 4;

/// The type, then where the track sits next to typical tracks of that type
/// on each of the `WITHIN` axes (tone, drive, pulse, clarity), in -1..1.
pub fn classify(profile: &SoundProfile) -> (SongType, [f32; AXES]) {
    let values = profile.vector();
    let z: [f32; FEATURES] =
        std::array::from_fn(|index| (values[index] - MEAN[index]) / SCALE[index]);
    let kind = (0..TYPES)
        .map(|row| {
            let score = WEIGHTS[row]
                .iter()
                .zip(&z)
                .map(|(weight, z)| weight * z.clamp(-Z_LIMIT, Z_LIMIT))
                .sum::<f32>();
            (row, BIAS[row] + score)
        })
        .max_by(|left, right| left.1.total_cmp(&right.1))
        .map_or(SongType::default(), |(row, _)| SongType::ALL[row]);
    let place = WITHIN.each_ref().map(|axis| {
        let value = axis.features.iter().map(|&index| z[index]).sum::<f32>()
            / axis.features.len() as f32;
        ((value - axis.centre[kind as usize]) / axis.spread).clamp(-1.0, 1.0)
    });
    (kind, place)
}

// Fitted on the labelled tracks with 0.3 shrinkage towards a diagonal
// covariance and equal priors. Rows follow `SongType::ALL`.
const MEAN: [f32; FEATURES] = [
    -12.833981, 0.06708842, 0.07862608, 0.07512402, 0.078279585, -10.651014, 3.8956368,
    -4.8412375, -0.13091932, -6.6996064, -4.3879085, -4.670755, -2.241294, 629.86285,
    8.97001, 8.722371, 10.437137, 0.9676244, 0.4000059, 0.42880392, 3.1919246,
    2.1323316, 1.9301281, 2.3375893, 2.536541, 2.329063, 2.454544, 1.900873,
];
const SCALE: [f32; FEATURES] = [
    2.4317286, 0.015887015, 0.021715852, 0.017441815, 0.022741081, 3.202071, 3.5764267,
    1.7553097, 2.05154, 3.1046453, 2.7926562, 1.1909941, 0.7727714, 170.78625,
    4.037626, 0.4802793, 2.7550967, 0.024481788, 0.18299003, 0.2092065, 0.31034696,
    0.35275403, 0.20009609, 0.226884, 0.27113605, 0.28170595, 0.36092418, 0.21005733,
];
const WEIGHTS: [[f32; FEATURES]; TYPES] = [
    // PIANO
    [
        -4.288507, -1.3161408, -0.35305172, -0.005438082, 0.70413613, -1.5642716, -2.8849423,
        -1.0031718, 2.2493224, -2.6906104, -2.6990595, -5.293476, -9.185454, 3.1400871,
        0.85696304, -1.404161, 0.51273954, -1.3420415, -0.9747431, -0.8208276, 2.0091543,
        -1.2036674, -0.82972133, -0.9512261, -0.331284, 2.5157802, -3.9386601, -0.30480656,
    ],
    // EPIC
    [
        0.16374971, 0.5878184, 1.0811067, -0.41104287, -0.6425885, 0.25782576, -0.35619676,
        0.38026813, -0.1116617, -0.0064654755, -0.72252643, -0.6683987, -0.32633218, -0.9107817,
        0.49131358, -0.25584835, 0.27346906, 0.080592506, -0.7128514, -1.0610651, 0.6940977,
        -1.0077026, -0.83711106, 0.74962497, 0.073070735, -1.0159891, -0.11811478, -0.05429625,
    ],
    // BALLAD
    [
        0.06813343, -0.18008336, -0.3660067, -0.16733113, -0.5813036, 0.050748825, -0.34598857,
        -0.079462044, -0.24331185, 0.24831365, 0.6100316, -0.79976267, -0.22359993, -0.18606918,
        0.12567693, -0.0643225, -0.950269, -0.4178588, -0.40958738, -0.9947528, -0.11136496,
        -0.42583686, 0.019588087, 1.2819388, 0.40572575, 0.05052388, 0.06853595, -0.26105076,
    ],
    // POP
    [
        0.003272106, 0.31651562, 0.4963549, 0.0040331916, 0.051378366, -0.012066139, 0.07125713,
        0.028308887, -0.043246787, 0.2363372, -0.2474459, 0.46645677, 0.7224408, -0.30844307,
        -0.049175743, 0.20201348, 0.34857538, 0.13179605, -0.08563317, -0.09628053, -0.1372247,
        0.35329095, 0.53531766, 0.29930335, -0.06900568, -0.042377166, 0.13456774, 0.18236697,
    ],
    // ROCK
    [
        -0.086542726, -0.028719146, 0.37894583, -0.20381682, 0.0659749, 0.22412495, -0.72638196,
        -0.27478987, -0.21002132, 0.6361466, -0.34663177, 1.2568052, 0.8801507, 0.09222561,
        -0.11861757, 0.5127085, -0.34880516, -0.30600032, -0.574528, -0.46594957, 0.16479531,
        -0.21600237, 0.29605463, -0.19525799, -0.18487097, -0.45491537, 0.16307217, -0.0087624,
    ],
    // ELEC
    [
        0.37766004, -0.5522378, -0.67023194, 0.03504368, 0.0863852, 0.031512506, 0.5660152,
        -0.03854047, 0.1260098, 0.03952695, 0.97609675, 0.24055755, 0.4551942, 0.04003042,
        -0.19642687, 0.50145066, -0.21857288, 0.31877324, 0.6114576, 0.99365145, 0.046920195,
        0.673549, -0.25676826, -0.7699224, -0.009851002, 0.3411619, 0.3579404, -0.27153555,
    ],
    // PHONK
    [
        0.5452493, 0.9684987, -0.98673546, 1.1039318, 1.0413219, -0.0059606256, 1.1456971,
        0.54674274, -0.64083445, -1.242606, -0.9578493, 0.44939235, -0.7147614, 1.292,
        -0.24892433, -2.587371, 0.8201675, -0.13088283, 1.1369867, 1.028231, -1.7468646,
        -1.2719723, -0.32515135, -1.1589688, -0.07576547, -0.53638583, -0.1598532, 1.0199536,
    ],
];
const BIAS: [f32; TYPES] = [
    -52.576122, -3.5390096, -2.3335474, -0.53229207, -2.356471, -1.6795454, -8.978676,
];
// Within a type the axes barely correlate with each other, so each one
// changes the look in its own way. Centres and spreads are measured on the
// whole 142-track library.
const WITHIN: [Axis; AXES] = [
    // Tone: treble against bass.
    Axis {
        features: &[TILT],
        centre: [
            -1.4650544, -0.051023975, 0.3434133, -0.13437656, 1.1048945, 0.123731464, -0.45785943,
        ],
        spread: 1.3922799,
    },
    // Drive: loudness and how often the level jumps.
    Axis {
        features: &[LOUDNESS, FLUX],
        centre: [
            -2.4576483, 0.062497724, -0.46232036, 0.34929124, -0.025349181, 0.16405593, 0.5041428,
        ],
        spread: 0.8615277,
    },
    // Pulse: how regular the rhythm is.
    Axis {
        features: &[PULSE, LOW_PULSE],
        centre: [-0.963292, -0.7981, -0.792485, 0.0420503, -0.354271, 0.756226, 0.990888],
        spread: 1.04755,
    },
    // Clarity: clear peaks against noise in the 3–8 kHz spectrum.
    Axis {
        features: &[CONTRAST_HIGH, CONTRAST_TOP],
        centre: [3.47396, 0.451967, 0.549595, -0.127022, -0.51419, -0.485625, -0.525339],
        spread: 1.25427,
    },
];
