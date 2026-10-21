use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, FontId, FontTweak, Stroke, TextStyle,
    Visuals,
};

use crate::design as tokens;

struct Palette {
    dark: bool,
    bg: [u8; 3],
    bg_deep: [u8; 3],
    surface: [u8; 3],
    surface_hi: [u8; 3],
    surface_sunken: [u8; 3],
    text_hi: [u8; 3],
    text: [u8; 3],
    text_dim: [u8; 3],
    line: [u8; 3],
    danger: [u8; 3],
    ink_tint: [u8; 3],
    ink_mix: f32,
}

const PALETTES: [Palette; 12] = [
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
];

const GRADIENTS: [Option<([u8; 3], [u8; 3])>; 12] = [
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
    Some(([8, 26, 32], [60, 20, 48])),
];

pub const THEME_NAMES: [&str; 12] = [
    "MONO", "PAPER", "SLATE", "MOSS", "DUSK", "SAND", "FOG", "ROSE", "BLUSH", "MINT", "AMBER",
    "AURORA",
];
pub const THEME_COUNT: usize = THEME_NAMES.len();

static THEME_INDEX: AtomicUsize = AtomicUsize::new(0);

pub fn set_theme(index: usize) {
    THEME_INDEX.store(index.min(THEME_COUNT - 1), Ordering::Relaxed);
}

pub fn theme_index() -> usize {
    THEME_INDEX.load(Ordering::Relaxed).min(THEME_COUNT - 1)
}

pub fn theme_name() -> &'static str {
    THEME_NAMES[theme_index()]
}

pub fn is_light() -> bool {
    !palette().dark
}

fn palette() -> &'static Palette {
    &PALETTES[theme_index()]
}

fn color(rgb: [u8; 3]) -> Color32 {
    Color32::from_rgb(rgb[0], rgb[1], rgb[2])
}

pub fn theme_colors(index: usize) -> (Color32, Color32, Color32) {
    let palette = &PALETTES[index.min(THEME_COUNT - 1)];
    (
        color(palette.bg),
        color(palette.text_hi),
        color(palette.text_dim),
    )
}

pub fn paint_background(painter: &egui::Painter, rect: egui::Rect) {
    match GRADIENTS[theme_index()] {
        Some((top, bottom)) => paint_gradient(painter, rect, top, bottom),
        None => {
            painter.rect_filled(rect, tokens::RADIUS, bg());
        }
    }
}

pub fn paint_theme_preview(painter: &egui::Painter, rect: egui::Rect, index: usize) {
    let index = index.min(THEME_COUNT - 1);
    match GRADIENTS[index] {
        Some((top, bottom)) => paint_gradient(painter, rect, top, bottom),
        None => {
            painter.rect_filled(rect, tokens::RADIUS, color(PALETTES[index].bg));
        }
    }
}

fn paint_gradient(painter: &egui::Painter, rect: egui::Rect, top: [u8; 3], bottom: [u8; 3]) {
    let top = color(top);
    let bottom = color(bottom);
    let mut mesh = egui::Mesh::default();
    mesh.colored_vertex(rect.left_top(), top);
    mesh.colored_vertex(rect.right_top(), mix(top, bottom, 0.32));
    mesh.colored_vertex(rect.right_bottom(), bottom);
    mesh.colored_vertex(rect.left_bottom(), mix(top, bottom, 0.68));
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

pub fn bg() -> Color32 {
    color(palette().bg)
}

pub fn bg_deep() -> Color32 {
    color(palette().bg_deep)
}

pub fn surface() -> Color32 {
    color(palette().surface)
}

pub fn surface_hi() -> Color32 {
    color(palette().surface_hi)
}

pub fn surface_sunken() -> Color32 {
    color(palette().surface_sunken)
}

pub fn text_hi() -> Color32 {
    color(palette().text_hi)
}

pub fn text() -> Color32 {
    color(palette().text)
}

pub fn text_dim() -> Color32 {
    color(palette().text_dim)
}

pub fn line() -> Color32 {
    color(palette().line)
}

pub fn danger() -> Color32 {
    color(palette().danger)
}

pub fn ink(value: u8) -> Color32 {
    let palette = palette();
    let value = if palette.dark { value } else { 255 - value };
    let gray = Color32::from_gray(value);
    mix(gray, color(palette.ink_tint), palette.ink_mix)
}

pub fn alpha(color: Color32, a: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

pub fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_unmultiplied(
        lerp(a.r(), b.r()),
        lerp(a.g(), b.g()),
        lerp(a.b(), b.b()),
        lerp(a.a(), b.a()),
    )
}

pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    apply(ctx);
}

pub fn apply(ctx: &egui::Context) {
    let dark = palette().dark;
    ctx.set_theme(if dark {
        egui::Theme::Dark
    } else {
        egui::Theme::Light
    });

    ctx.tessellation_options_mut(|options| {
        options.feathering = true;
        options.feathering_size_in_pixels = 1.0;
        options.round_line_segments_to_pixels = true;
    });

    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.panel_fill = Color32::TRANSPARENT;
    visuals.window_fill = bg();
    visuals.extreme_bg_color = bg();
    visuals.faint_bg_color = surface_hi();
    visuals.selection.bg_fill = text_hi();
    visuals.selection.stroke = Stroke::new(1.0, bg());
    visuals.hyperlink_color = text_hi();
    visuals.text_cursor.stroke = Stroke::new(6.0, text_hi());
    visuals.widgets.noninteractive.bg_fill = bg();
    visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, line());
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, text());
    visuals.widgets.inactive.bg_fill = Color32::TRANSPARENT;
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, line());
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, text());
    visuals.widgets.hovered.bg_fill = text_hi();
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, text_hi());
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, bg());
    visuals.widgets.active.bg_fill = text_hi();
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, text_hi());
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, bg());
    // Square widgets everywhere: egui's default rounded scrollbar handle
    // reads as a modern overlay and breaks the terminal look.
    for widget in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widget.corner_radius = tokens::RADIUS.into();
    }
    visuals.window_shadow = egui::epaint::Shadow {
        offset: [0, 0],
        blur: 0,
        spread: 0,
        color: Color32::TRANSPARENT,
    };
    visuals.popup_shadow = visuals.window_shadow;
    visuals.window_corner_radius = tokens::RADIUS.into();
    visuals.menu_corner_radius = tokens::RADIUS.into();

    ctx.all_styles_mut(|style| {
        style.text_styles = std::collections::BTreeMap::from([
            (TextStyle::Heading, FontId::new(20.0, FontFamily::Monospace)),
            (TextStyle::Body, FontId::new(14.0, FontFamily::Monospace)),
            (TextStyle::Button, FontId::new(13.0, FontFamily::Monospace)),
            (TextStyle::Small, FontId::new(12.0, FontFamily::Monospace)),
            (
                TextStyle::Monospace,
                FontId::new(13.0, FontFamily::Monospace),
            ),
        ]);
        style.spacing.item_spacing = egui::vec2(tokens::SPACE_TIGHT, tokens::SPACE_1);
        style.spacing.button_padding = egui::vec2(tokens::SPACE_TIGHT, tokens::HAIRLINE);
        style.spacing.interact_size.y = tokens::CONTROL_H;
        style.spacing.scroll = egui::style::ScrollStyle::solid();
        style.spacing.scroll.bar_width = tokens::SPACE_TIGHT;
        style.spacing.scroll.bar_inner_margin = tokens::HAIRLINE;
        style.spacing.scroll.bar_outer_margin = tokens::SPACE_1;
        style.visuals = visuals.clone();
    });
}

fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // Hack and Ubuntu are unused: the embedded faces cover every glyph this
    // UI draws. Dropping them keeps only the typefaces the design needs.
    fonts.font_data.remove("Hack");
    fonts.font_data.remove("Ubuntu-Light");

    // Sarasa Mono SC supplies the narrower TUI/Latin face. Only Latin,
    // punctuation, symbol and Braille ranges are embedded; CJK lives in the
    // Fusion pixel faces below.
    let sarasa = include_bytes!("../assets/fonts/SarasaMonoSC-TUI.ttf");
    fonts
        .font_data
        .insert("tui".to_owned(), Arc::new(FontData::from_static(sarasa)));

    // Dot-matrix Latin/narrow glyphs for the lyric readout. The main UI keeps
    // Sarasa's TUI face; this one is reserved for lyrics and their brackets.
    let lyric_tui = include_bytes!("../assets/fonts/FusionPixel12-Mono-TUI.ttf");
    fonts.font_data.insert(
        "lyric-tui".to_owned(),
        Arc::new(FontData::from_static(lyric_tui)),
    );

    // epaint positions each fallback run from its own ascent. Fusion's CJK
    // box already extends below the Latin baseline, so a small upward optical
    // correction keeps Chinese from sitting low in mixed lines.
    let fusion_tweak = FontTweak {
        y_offset_factor: -0.16,
        ..Default::default()
    };

    // Fusion Pixel 12px handles CJK.
    let compact = include_bytes!("../assets/fonts/FusionPixel12-Mono-CJK.ttf");
    fonts.font_data.insert(
        "ui".to_owned(),
        Arc::new(FontData::from_static(compact).tweak(fusion_tweak.clone())),
    );

    // Japanese and Korean live in separate pixel subsets: the Chinese face
    // above only carries simplified forms and has no kana or hangul.
    let japanese = include_bytes!("../assets/fonts/FusionPixel12-Mono-JA.ttf");
    fonts.font_data.insert(
        "ja".to_owned(),
        Arc::new(FontData::from_static(japanese).tweak(fusion_tweak.clone())),
    );
    let korean = include_bytes!("../assets/fonts/FusionPixel12-Mono-KO.ttf");
    fonts.font_data.insert(
        "ko".to_owned(),
        Arc::new(FontData::from_static(korean).tweak(fusion_tweak)),
    );

    // Fusion first so CJK punctuation stays pixel-shaped; Sarasa supplies the
    // narrow Latin face and Braille when Fusion has no glyph.
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        fonts.families.insert(
            family,
            vec![
                "ui".to_owned(),
                "ja".to_owned(),
                "ko".to_owned(),
                "tui".to_owned(),
                "egui-icons".to_owned(),
            ],
        );
    }
    fonts.families.insert(
        FontFamily::Name("ui".into()),
        vec![
            "ui".to_owned(),
            "ja".to_owned(),
            "ko".to_owned(),
            "tui".to_owned(),
        ],
    );
    fonts.families.insert(
        FontFamily::Name("lyric".into()),
        vec![
            "lyric-tui".to_owned(),
            "ui".to_owned(),
            "ja".to_owned(),
            "ko".to_owned(),
            "tui".to_owned(),
        ],
    );

    ctx.set_fonts(fonts);
}
