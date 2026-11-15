use std::sync::{PoisonError, RwLock};

pub type Rgb = [u8; 3];

const BLACK: Rgb = [0, 0, 0];
const WHITE: Rgb = [255, 255, 255];

#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub dark: bool,
    pub bg: Rgb,
    pub bg_deep: Rgb,
    pub surface: Rgb,
    pub surface_hi: Rgb,
    pub surface_sunken: Rgb,
    pub text_hi: Rgb,
    pub text: Rgb,
    pub text_dim: Rgb,
    pub line: Rgb,
    pub danger: Rgb,
    pub ink_tint: Rgb,
    pub ink_mix: f32,
}

const PALETTES: [Palette; BUILTIN_COUNT] = [
    Palette {
        dark: true,
        bg: [0, 0, 0],
        bg_deep: [0, 0, 0],
        surface: [5, 5, 5],
        surface_hi: [12, 12, 12],
        surface_sunken: [0, 0, 0],
        text_hi: [238, 238, 238],
        text: [184, 184, 184],
        text_dim: [110, 110, 110],
        line: [42, 42, 42],
        danger: [238, 238, 238],
        ink_tint: [238, 238, 238],
        ink_mix: 0.0,
    },
    Palette {
        dark: false,
        bg: [244, 244, 244],
        bg_deep: [255, 255, 255],
        surface: [250, 250, 250],
        surface_hi: [230, 230, 230],
        surface_sunken: [255, 255, 255],
        text_hi: [16, 16, 16],
        text: [72, 72, 72],
        text_dim: [128, 128, 128],
        line: [205, 205, 205],
        danger: [16, 16, 16],
        ink_tint: [128, 128, 128],
        ink_mix: 0.0,
    },
    Palette {
        dark: true,
        bg: [10, 12, 15],
        bg_deep: [7, 9, 12],
        surface: [14, 17, 21],
        surface_hi: [22, 26, 31],
        surface_sunken: [7, 9, 12],
        text_hi: [218, 224, 230],
        text: [165, 174, 184],
        text_dim: [105, 113, 122],
        line: [38, 44, 51],
        danger: [205, 155, 155],
        ink_tint: [120, 150, 180],
        ink_mix: 0.22,
    },
    Palette {
        dark: true,
        bg: [9, 13, 10],
        bg_deep: [6, 9, 7],
        surface: [13, 18, 14],
        surface_hi: [20, 27, 22],
        surface_sunken: [6, 9, 7],
        text_hi: [218, 226, 216],
        text: [164, 174, 163],
        text_dim: [104, 114, 103],
        line: [38, 46, 40],
        danger: [205, 160, 150],
        ink_tint: [130, 170, 140],
        ink_mix: 0.22,
    },
    Palette {
        dark: true,
        bg: [13, 10, 16],
        bg_deep: [8, 6, 10],
        surface: [18, 14, 22],
        surface_hi: [26, 21, 32],
        surface_sunken: [8, 6, 10],
        text_hi: [226, 218, 230],
        text: [174, 164, 178],
        text_dim: [112, 102, 116],
        line: [46, 40, 52],
        danger: [205, 155, 175],
        ink_tint: [160, 130, 180],
        ink_mix: 0.22,
    },
    Palette {
        dark: false,
        bg: [242, 238, 230],
        bg_deep: [252, 250, 245],
        surface: [248, 245, 238],
        surface_hi: [228, 222, 211],
        surface_sunken: [252, 250, 245],
        text_hi: [42, 38, 31],
        text: [92, 86, 74],
        text_dim: [140, 132, 118],
        line: [205, 197, 182],
        danger: [130, 65, 60],
        ink_tint: [190, 165, 120],
        ink_mix: 0.20,
    },
    Palette {
        dark: false,
        bg: [237, 241, 243],
        bg_deep: [250, 253, 255],
        surface: [244, 248, 250],
        surface_hi: [223, 230, 234],
        surface_sunken: [250, 253, 255],
        text_hi: [36, 43, 48],
        text: [82, 91, 97],
        text_dim: [130, 139, 145],
        line: [197, 206, 211],
        danger: [120, 60, 60],
        ink_tint: [130, 165, 180],
        ink_mix: 0.20,
    },
    Palette {
        dark: false,
        bg: [243, 237, 238],
        bg_deep: [253, 249, 250],
        surface: [249, 244, 245],
        surface_hi: [228, 217, 219],
        surface_sunken: [253, 249, 250],
        text_hi: [50, 35, 38],
        text: [98, 80, 84],
        text_dim: [144, 126, 130],
        line: [207, 193, 196],
        danger: [120, 55, 65],
        ink_tint: [190, 145, 155],
        ink_mix: 0.20,
    },
    Palette {
        dark: false,
        bg: [249, 222, 230],
        bg_deep: [255, 239, 244],
        surface: [252, 232, 238],
        surface_hi: [239, 202, 214],
        surface_sunken: [255, 239, 244],
        text_hi: [54, 20, 32],
        text: [108, 58, 72],
        text_dim: [154, 104, 118],
        line: [231, 178, 193],
        danger: [150, 42, 66],
        ink_tint: [228, 92, 140],
        ink_mix: 0.42,
    },
    Palette {
        dark: false,
        bg: [219, 243, 238],
        bg_deep: [238, 253, 250],
        surface: [228, 248, 244],
        surface_hi: [198, 231, 224],
        surface_sunken: [238, 253, 250],
        text_hi: [14, 46, 42],
        text: [50, 90, 84],
        text_dim: [102, 140, 134],
        line: [172, 214, 205],
        danger: [148, 60, 58],
        ink_tint: [62, 188, 166],
        ink_mix: 0.42,
    },
    Palette {
        dark: false,
        bg: [250, 236, 210],
        bg_deep: [255, 246, 228],
        surface: [252, 242, 222],
        surface_hi: [240, 220, 184],
        surface_sunken: [255, 246, 228],
        text_hi: [54, 38, 14],
        text: [106, 82, 44],
        text_dim: [150, 124, 82],
        line: [226, 204, 164],
        danger: [150, 60, 40],
        ink_tint: [230, 150, 40],
        ink_mix: 0.45,
    },
    Palette {
        dark: true,
        bg: [18, 16, 28],
        bg_deep: [10, 9, 18],
        surface: [24, 21, 36],
        surface_hi: [34, 29, 50],
        surface_sunken: [10, 9, 18],
        text_hi: [238, 232, 246],
        text: [188, 178, 204],
        text_dim: [124, 116, 146],
        line: [50, 43, 70],
        danger: [225, 130, 170],
        ink_tint: [170, 140, 220],
        ink_mix: 0.34,
    },
    Palette {
        dark: true,
        bg: [20, 18, 40],
        bg_deep: [11, 10, 24],
        surface: [26, 23, 50],
        surface_hi: [36, 32, 66],
        surface_sunken: [11, 10, 24],
        text_hi: [236, 232, 250],
        text: [184, 180, 214],
        text_dim: [122, 118, 160],
        line: [48, 44, 84],
        danger: [235, 130, 180],
        ink_tint: [226, 146, 236],
        ink_mix: 0.36,
    },
    Palette {
        dark: true,
        bg: [34, 18, 30],
        bg_deep: [20, 10, 18],
        surface: [42, 23, 36],
        surface_hi: [56, 32, 46],
        surface_sunken: [20, 10, 18],
        text_hi: [248, 236, 228],
        text: [204, 184, 176],
        text_dim: [146, 122, 118],
        line: [72, 44, 52],
        danger: [245, 120, 120],
        ink_tint: [255, 170, 110],
        ink_mix: 0.36,
    },
    Palette {
        dark: true,
        bg: [10, 24, 38],
        bg_deep: [5, 13, 22],
        surface: [14, 30, 46],
        surface_hi: [20, 40, 60],
        surface_sunken: [5, 13, 22],
        text_hi: [226, 242, 248],
        text: [168, 196, 206],
        text_dim: [104, 136, 150],
        line: [32, 56, 74],
        danger: [235, 140, 140],
        ink_tint: [110, 210, 235],
        ink_mix: 0.36,
    },
    Palette {
        dark: false,
        bg: [250, 230, 224],
        bg_deep: [255, 244, 240],
        surface: [252, 237, 232],
        surface_hi: [240, 210, 206],
        surface_sunken: [255, 244, 240],
        text_hi: [62, 30, 38],
        text: [114, 72, 78],
        text_dim: [158, 118, 122],
        line: [232, 196, 196],
        danger: [160, 44, 60],
        ink_tint: [238, 104, 120],
        ink_mix: 0.42,
    },
    Palette {
        dark: false,
        bg: [232, 232, 246],
        bg_deep: [246, 246, 255],
        surface: [238, 238, 250],
        surface_hi: [214, 214, 236],
        surface_sunken: [246, 246, 255],
        text_hi: [30, 32, 60],
        text: [72, 76, 108],
        text_dim: [120, 124, 156],
        line: [196, 198, 226],
        danger: [150, 50, 80],
        ink_tint: [110, 110, 230],
        ink_mix: 0.42,
    },
    Palette {
        dark: false,
        bg: [230, 242, 222],
        bg_deep: [246, 252, 240],
        surface: [238, 248, 232],
        surface_hi: [210, 228, 202],
        surface_sunken: [246, 252, 240],
        text_hi: [24, 46, 30],
        text: [64, 92, 68],
        text_dim: [112, 140, 114],
        line: [184, 210, 182],
        danger: [150, 60, 50],
        ink_tint: [70, 170, 100],
        ink_mix: 0.42,
    },
    Palette {
        dark: true,
        bg: [34, 10, 24],
        bg_deep: [20, 6, 14],
        surface: [39, 15, 29],
        surface_hi: [47, 23, 37],
        surface_sunken: [20, 6, 14],
        text_hi: [244, 226, 233],
        text: [194, 174, 183],
        text_dim: [135, 114, 124],
        line: [63, 33, 49],
        danger: [237, 122, 140],
        ink_tint: [240, 96, 156],
        ink_mix: 0.42,
    },
    Palette {
        dark: true,
        bg: [6, 28, 24],
        bg_deep: [4, 17, 14],
        surface: [11, 33, 29],
        surface_hi: [19, 41, 37],
        surface_sunken: [4, 17, 14],
        text_hi: [223, 239, 234],
        text: [171, 188, 184],
        text_dim: [110, 129, 125],
        line: [28, 56, 49],
        danger: [206, 142, 141],
        ink_tint: [70, 206, 164],
        ink_mix: 0.42,
    },
    Palette {
        dark: true,
        bg: [8, 16, 42],
        bg_deep: [5, 10, 25],
        surface: [13, 21, 47],
        surface_hi: [21, 29, 54],
        surface_sunken: [5, 10, 25],
        text_hi: [226, 232, 245],
        text: [174, 180, 196],
        text_dim: [113, 120, 139],
        line: [31, 41, 71],
        danger: [211, 131, 157],
        ink_tint: [96, 146, 255],
        ink_mix: 0.42,
    },
];

// Corner colours: top-left, top-right, bottom-right, bottom-left.
const GRADIENTS: [Option<[Rgb; 4]>; BUILTIN_COUNT] = [
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    None,
    Some([[8, 26, 32], [25, 24, 37], [60, 20, 48], [43, 22, 43]]),
    Some([[12, 18, 54], [36, 16, 64], [70, 18, 60], [14, 34, 66]]),
    Some([[34, 16, 48], [20, 18, 50], [76, 36, 18], [70, 22, 34]]),
    Some([[8, 16, 42], [10, 30, 60], [6, 52, 56], [12, 44, 34]]),
    Some([[254, 230, 208], [250, 216, 228], [234, 218, 246], [252, 242, 206]]),
    Some([[216, 240, 236], [218, 228, 250], [236, 222, 248], [248, 228, 236]]),
    Some([[250, 244, 206], [214, 242, 222], [208, 232, 246], [232, 246, 212]]),
    None,
    None,
    None,
];

// Order doubles as the legacy `theme` index in state.json: append only.
pub const BUILTIN_NAMES: [&str; 21] = [
    "MONO", "PAPER", "SLATE", "MOSS", "DUSK", "SAND", "FOG", "ROSE", "BLUSH", "MINT", "AMBER",
    "AURORA", "NEBULA", "SUNSET", "OCEAN", "SHERBET", "OPAL", "MEADOW", "BERRY", "JADE", "COBALT",
];
pub const BUILTIN_COUNT: usize = BUILTIN_NAMES.len();

const CATEGORIES: [Category; BUILTIN_COUNT] = {
    use Category::*;
    [
        Mono, Mono, Tint, Tint, Tint, Tint, Tint, Tint, Color, Color, Color, Gradient, Gradient,
        Gradient, Gradient, Gradient, Gradient, Gradient, Color, Color, Color,
    ]
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    /// Pure greyscale.
    Mono,
    /// One low-saturation hue.
    Tint,
    /// One saturated hue.
    Color,
    /// Multi-colour corner gradient.
    Gradient,
    Custom,
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Mono,
        Self::Tint,
        Self::Color,
        Self::Gradient,
        Self::Custom,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Mono => "MONO",
            Self::Tint => "TINT",
            Self::Color => "COLOR",
            Self::Gradient => "GRADIENT",
            Self::Custom => "CUSTOM",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Look {
    pub palette: Palette,
    /// Corner colours: top-left, top-right, bottom-right, bottom-left.
    pub gradient: Option<[Rgb; 4]>,
}

impl Look {
    pub fn text_hi(&self) -> Rgb {
        self.palette.text_hi
    }

    pub fn text_dim(&self) -> Rgb {
        self.palette.text_dim
    }

    /// Straight RGB blend for cross-fades; `dark` flips at the midpoint.
    pub fn blend(&self, other: &Look, t: f32) -> Look {
        let t = t.clamp(0.0, 1.0);
        let (a, b) = (&self.palette, &other.palette);
        let mix = |x: Rgb, y: Rgb| mix_rgb(x, y, t);
        let corners = |look: &Look| look.gradient.unwrap_or([look.palette.bg; 4]);
        let (from, to) = (corners(self), corners(other));
        Look {
            palette: Palette {
                dark: if t < 0.5 { a.dark } else { b.dark },
                bg: mix(a.bg, b.bg),
                bg_deep: mix(a.bg_deep, b.bg_deep),
                surface: mix(a.surface, b.surface),
                surface_hi: mix(a.surface_hi, b.surface_hi),
                surface_sunken: mix(a.surface_sunken, b.surface_sunken),
                text_hi: mix(a.text_hi, b.text_hi),
                text: mix(a.text, b.text),
                text_dim: mix(a.text_dim, b.text_dim),
                line: mix(a.line, b.line),
                danger: mix(a.danger, b.danger),
                ink_tint: mix(a.ink_tint, b.ink_tint),
                ink_mix: a.ink_mix + (b.ink_mix - a.ink_mix) * t,
            },
            gradient: (self.gradient.is_some() || other.gradient.is_some())
                .then(|| std::array::from_fn(|i| mix(from[i], to[i]))),
        }
    }
}

pub fn builtin_category(index: usize) -> Category {
    CATEGORIES[index]
}

pub fn builtin_look(index: usize) -> Look {
    Look {
        palette: PALETTES[index],
        gradient: GRADIENTS[index],
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Background {
    Solid(Rgb),
    /// Top-left to bottom-right blend.
    Diagonal(Rgb, Rgb),
    Corners([Rgb; 4]),
}

impl Background {
    pub fn corners(self) -> [Rgb; 4] {
        match self {
            Self::Solid(rgb) => [rgb; 4],
            Self::Diagonal(from, to) => {
                let mid = mix_rgb(from, to, 0.5);
                [from, mid, to, mid]
            }
            Self::Corners(corners) => corners,
        }
    }
}

/// The few colours a custom theme is built from; every other slot is derived.
#[derive(Clone, Copy, Debug)]
pub struct Seeds {
    pub background: Background,
    pub accent: Rgb,
    pub tint: f32,
    /// `None` picks dark or light from the background brightness.
    pub dark: Option<bool>,
}

pub fn derive(seeds: &Seeds) -> Look {
    let corners = seeds.background.corners();
    let gradient = match seeds.background {
        Background::Solid(_) => None,
        _ => Some(corners),
    };
    let base = average(&corners);
    // 0.179 is the luminance where black and white text reach equal contrast.
    let dark = seeds.dark.unwrap_or(luminance(base) < 0.179);
    // Gradient themes keep windows a shade off the backdrop, like AURORA.
    let bg = match (gradient, dark) {
        (None, _) => base,
        (Some(_), true) => mix_rgb(base, BLACK, 0.35),
        (Some(_), false) => mix_rgb(base, WHITE, 0.1),
    };
    let accent = seeds.accent;
    let mut palette = if dark {
        let text_hi = mix_rgb([244; 3], accent, 0.12);
        let bg_deep = mix_rgb(bg, BLACK, 0.4);
        Palette {
            dark,
            bg,
            bg_deep,
            surface: mix_rgb(bg, text_hi, 0.025),
            surface_hi: mix_rgb(bg, text_hi, 0.06),
            surface_sunken: bg_deep,
            text_hi,
            text: mix_rgb(text_hi, bg, 0.24),
            text_dim: mix_rgb(text_hi, bg, 0.52),
            line: mix_rgb(bg, mix_rgb(text_hi, accent, 0.4), 0.14),
            danger: mix_rgb([236, 128, 136], accent, 0.18),
            ink_tint: accent,
            ink_mix: seeds.tint,
        }
    } else {
        let text_hi = mix_rgb([16; 3], accent, 0.16);
        let bg_deep = mix_rgb(bg, WHITE, 0.6);
        Palette {
            dark,
            bg,
            bg_deep,
            surface: mix_rgb(bg, WHITE, 0.35),
            surface_hi: mix_rgb(bg, text_hi, 0.08),
            surface_sunken: bg_deep,
            text_hi,
            text: mix_rgb(text_hi, bg, 0.25),
            text_dim: mix_rgb(text_hi, bg, 0.49),
            line: mix_rgb(bg, mix_rgb(text_hi, accent, 0.4), 0.16),
            danger: mix_rgb([150, 44, 56], accent, 0.12),
            ink_tint: accent,
            ink_mix: seeds.tint,
        }
    };

    // Text has to stay readable on every part of the backdrop, not only on
    // the window fill.
    let backs = [bg, corners[0], corners[1], corners[2], corners[3], base];
    let toward = if dark { WHITE } else { BLACK };
    palette.text_hi = legible(palette.text_hi, &backs, 7.0, toward);
    palette.text = legible(palette.text, &backs, 4.5, toward);
    palette.text_dim = legible(palette.text_dim, &backs, 3.0, toward);
    Look { palette, gradient }
}

fn legible(rgb: Rgb, backs: &[Rgb], target: f32, toward: Rgb) -> Rgb {
    (0..=20)
        .map(|step| mix_rgb(rgb, toward, step as f32 / 20.0))
        .find(|candidate| backs.iter().all(|back| contrast(*candidate, *back) >= target))
        .unwrap_or(toward)
}

fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

fn luminance(rgb: Rgb) -> f32 {
    let channel = |value: u8| {
        let value = value as f32 / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(rgb[0]) + 0.7152 * channel(rgb[1]) + 0.0722 * channel(rgb[2])
}

pub fn mix_rgb(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    std::array::from_fn(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

pub fn average(colors: &[Rgb; 4]) -> Rgb {
    std::array::from_fn(|i| {
        (colors.iter().map(|rgb| rgb[i] as f32).sum::<f32>() / 4.0).round() as u8
    })
}

static ACTIVE: RwLock<Look> = RwLock::new(Look {
    palette: PALETTES[0],
    gradient: None,
});

pub fn set_look(look: Look) {
    *ACTIVE.write().unwrap_or_else(PoisonError::into_inner) = look;
}

pub fn current_look() -> Look {
    *ACTIVE.read().unwrap_or_else(PoisonError::into_inner)
}

pub fn is_light() -> bool {
    !palette().dark
}

fn palette() -> Palette {
    current_look().palette
}

pub fn bg() -> Rgb {
    palette().bg
}

pub fn bg_deep() -> Rgb {
    palette().bg_deep
}

pub fn surface() -> Rgb {
    palette().surface
}

pub fn surface_hi() -> Rgb {
    palette().surface_hi
}

pub fn surface_sunken() -> Rgb {
    palette().surface_sunken
}

pub fn text_hi() -> Rgb {
    palette().text_hi
}

pub fn text() -> Rgb {
    palette().text
}

pub fn text_dim() -> Rgb {
    palette().text_dim
}

pub fn line() -> Rgb {
    palette().line
}

pub fn danger() -> Rgb {
    palette().danger
}

/// The greyscale instrument ink: a grey value pulled toward the palette's
/// ink tint, matching the egui build's `ink()` behaviour.
pub fn ink(value: u8) -> Rgb {
    let palette = palette();
    let value = if palette.dark { value } else { 255 - value };
    let gray = [value; 3];
    mix_rgb(gray, palette.ink_tint, palette.ink_mix)
}
