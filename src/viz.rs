use eframe::egui::{Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Stroke, Vec2};

use crate::spectrum::{VisualState, MAX_DB_VALUE, NUM_BARS};
use crate::theme;

const TAU: f32 = std::f32::consts::TAU;

// One LED cell matches the 12 px TUI body text size.
const LED_CELL: f32 = 12.0;
// Character cells are slightly larger to keep the glyph field readable.
const CHAR_CELL: f32 = 18.0;

// Monochrome luminance ramp. The old ember ramp is gone; only brightness remains.
const SPECTRUM_GREYS: [(f32, u8); 4] = [(0.00, 52), (0.22, 104), (0.52, 178), (0.85, 246)];

pub fn spectrum_color(norm: f32) -> Color32 {
    let value = norm.clamp(0.0, 1.0);
    let mut lower = SPECTRUM_GREYS[0];
    let mut upper = SPECTRUM_GREYS[SPECTRUM_GREYS.len() - 1];
    for pair in SPECTRUM_GREYS.windows(2) {
        if value >= pair[0].0 && value <= pair[1].0 {
            lower = pair[0];
            upper = pair[1];
            break;
        }
    }
    let span = (upper.0 - lower.0).max(1e-6);
    let t = ((value - lower.0) / span).clamp(0.0, 1.0);
    let grey = (lower.1 as f32 + (upper.1 as f32 - lower.1 as f32) * t)
        .round()
        .clamp(0.0, 255.0) as u8;
    theme::ink(grey)
}

pub fn paint_spectrum(
    painter: &Painter,
    rect: Rect,
    bars: &[f32],
    _playing: bool,
    rotation: f32,
    size_scale: f32,
) {
    let usable = (rect.width().min(rect.height()) * 0.5 - 8.0).max(1.0);
    let scale = (usable / 258.0).clamp(0.45, 2.4) * size_scale.clamp(0.7, 1.2);
    let center = rect.center();

    let inner_r = (120.0 * scale).min(usable * 0.75);
    let max_amp = (100.0 * scale).min((usable - inner_r - 2.0).max(10.0));
    let wide_w = (12.0 * scale).max(3.0);
    let glow_w = (6.0 * scale).max(2.0);
    let bar_w = (2.8 * scale).max(1.0);

    // A quiet baseline; no instrument bezel, ticks or labels.
    painter.circle_stroke(
        center,
        inner_r,
        Stroke::new((1.5 * scale).max(1.0), theme::alpha(theme::ink(210), 20)),
    );
    radial_rings(
        painter,
        center,
        inner_r * 1.05,
        &[
            (0.0, Color32::TRANSPARENT),
            (0.85, Color32::TRANSPARENT),
            (0.95, theme::alpha(theme::ink(210), 9)),
            (1.0, Color32::TRANSPARENT),
        ],
        48,
    );

    if !bars.iter().any(|bar| *bar > 0.01) {
        return;
    }

    let mut p1 = Vec::with_capacity(NUM_BARS);
    let mut p2 = Vec::with_capacity(NUM_BARS);
    let mut colors = Vec::with_capacity(NUM_BARS);
    let mut glows = Vec::with_capacity(NUM_BARS);
    let mut wide_glows = Vec::with_capacity(NUM_BARS);

    for i in 0..NUM_BARS {
        let height = bars.get(i).copied().unwrap_or(0.0);
        let norm = (height / MAX_DB_VALUE).clamp(0.0, 1.0);
        let color = spectrum_color(norm);
        let outer = inner_r + (norm * max_amp).max(inner_r * 0.02);
        let angle = std::f32::consts::FRAC_PI_2 + i as f32 / NUM_BARS as f32 * TAU + rotation;
        let dir = Vec2::new(angle.cos(), angle.sin());
        p1.push(center + dir * inner_r);
        p2.push(center + dir * outer);
        colors.push(color);
        glows.push(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            26,
        ));
        wide_glows.push(Color32::from_rgba_unmultiplied(
            color.r(),
            color.g(),
            color.b(),
            8,
        ));
    }

    // Very wide, very faint halo.
    for i in 0..NUM_BARS {
        painter.line_segment([p1[i], p2[i]], Stroke::new(wide_w, wide_glows[i]));
        painter.circle_filled(p1[i], wide_w * 0.5, wide_glows[i]);
        painter.circle_filled(p2[i], wide_w * 0.5, wide_glows[i]);
    }

    // Tight glow.
    for i in 0..NUM_BARS {
        painter.line_segment([p1[i], p2[i]], Stroke::new(glow_w, glows[i]));
        painter.circle_filled(p1[i], glow_w * 0.5, glows[i]);
        painter.circle_filled(p2[i], glow_w * 0.5, glows[i]);
    }

    // Bright core line.
    for i in 0..NUM_BARS {
        painter.line_segment([p1[i], p2[i]], Stroke::new(bar_w, colors[i]));
        painter.circle_filled(p1[i], bar_w * 0.5, colors[i]);
        painter.circle_filled(p2[i], bar_w * 0.5, colors[i]);
    }
}

struct SpectrumGrid {
    rect: Rect,
    cols: usize,
    rows: usize,
    cell_w: f32,
    cell_h: f32,
    /// Rows at the bottom that the spectrum may light up (40% of the stage).
    signal_rows: usize,
}

fn spectrum_grid(rect: Rect, cell: f32) -> Option<SpectrumGrid> {
    let side_margin = 12.0;
    let bottom_margin = 24.0;
    let max_w = (rect.width() - side_margin * 2.0).max(0.0);
    let max_h = (rect.height() - bottom_margin).max(0.0);
    if max_w < cell * 8.0 || max_h < cell * 4.0 {
        return None;
    }

    let cols = ((max_w / cell).round() as usize).max(8);
    let rows = ((max_h / cell).floor() as usize).max(4);
    let cell_w = max_w / cols as f32;
    // Stretch the cells over the full stage height so the lattice reaches
    // the top, while the lit spectrum is limited to the bottom 40%.
    let cell_h = max_h / rows as f32;
    let signal_rows = (((rect.height() * 0.40) / cell_h).round() as usize).clamp(1, rows);
    let panel = Rect::from_min_max(
        Pos2::new(rect.left() + side_margin, rect.top()),
        Pos2::new(rect.right() - side_margin, rect.bottom() - bottom_margin),
    );
    Some(SpectrumGrid {
        rect: panel,
        cols,
        rows,
        cell_w,
        cell_h,
        signal_rows,
    })
}

fn painted_grid(painter: &Painter, grid: &SpectrumGrid) {
    // A faint matrix stays visible in silence; the lit cells carry the data.
    let grid_color = theme::alpha(theme::ink(210), 8);
    for i in 1..grid.cols {
        let x = grid.rect.left() + i as f32 * grid.cell_w;
        painter.line_segment(
            [
                Pos2::new(x, grid.rect.top()),
                Pos2::new(x, grid.rect.bottom()),
            ],
            Stroke::new(1.0, grid_color),
        );
    }
    for j in 1..grid.rows {
        let y = grid.rect.bottom() - j as f32 * grid.cell_h;
        painter.line_segment(
            [
                Pos2::new(grid.rect.left(), y),
                Pos2::new(grid.rect.right(), y),
            ],
            Stroke::new(1.0, grid_color),
        );
    }
    painter.line_segment(
        [grid.rect.left_bottom(), grid.rect.right_bottom()],
        Stroke::new(1.0, theme::alpha(theme::ink(210), 18)),
    );

    // A thin lattice keeps the cell texture without physical gaps.
    let lattice = if theme::is_light() {
        Color32::from_rgba_unmultiplied(255, 255, 255, 18)
    } else {
        Color32::from_rgba_unmultiplied(0, 0, 0, 14)
    };
    for i in 1..grid.cols {
        let x = grid.rect.left() + i as f32 * grid.cell_w;
        painter.line_segment(
            [
                Pos2::new(x, grid.rect.top()),
                Pos2::new(x, grid.rect.bottom()),
            ],
            Stroke::new(1.0, lattice),
        );
    }
    for j in 1..grid.rows {
        let y = grid.rect.bottom() - j as f32 * grid.cell_h;
        painter.line_segment(
            [
                Pos2::new(grid.rect.left(), y),
                Pos2::new(grid.rect.right(), y),
            ],
            Stroke::new(1.0, lattice),
        );
    }
}

fn painted_labels(painter: &Painter, rect: Rect, grid: &SpectrumGrid) {
    let label = theme::alpha(theme::ink(160), 130);
    let label_y = rect.bottom() - 9.0;
    painter.text(
        Pos2::new(grid.rect.left(), label_y),
        Align2::LEFT_CENTER,
        "20HZ",
        FontId::new(9.0, FontFamily::Monospace),
        label,
    );
    painter.text(
        Pos2::new(grid.rect.center().x, label_y),
        Align2::CENTER_CENTER,
        "8KHZ",
        FontId::new(9.0, FontFamily::Monospace),
        label,
    );
    painter.text(
        Pos2::new(grid.rect.right(), label_y),
        Align2::RIGHT_CENTER,
        "20HZ",
        FontId::new(9.0, FontFamily::Monospace),
        label,
    );
}

fn mapped_bar(bars: &[f32], cols: usize, col: usize) -> f32 {
    // V-shaped layout: low frequencies at both edges, high in the centre.
    // The two halves take alternate bands so the right side is not a clone.
    let half = (cols - 1) / 2;
    let pair_span = (NUM_BARS / 2 - 1).max(1) as f32;
    let edge_distance = (col.min(cols - 1 - col) as f32 / half.max(1) as f32).min(1.0);
    let pair_pos = edge_distance * pair_span;
    let pair_index = pair_pos.floor() as usize;
    let t = pair_pos - pair_index as f32;
    let parity = usize::from(col > half);
    let i0 = (pair_index * 2).min(NUM_BARS - 2) + parity;
    let i1 = ((pair_index + 1) * 2).min(NUM_BARS - 2) + parity;
    let a = bars.get(i0).copied().unwrap_or(0.0);
    let b = bars.get(i1).copied().unwrap_or(a);
    ((a + (b - a) * t) / MAX_DB_VALUE).clamp(0.0, 1.0)
}

// Pixel LED wall behind the radial spectrum: frequency left-to-right,
// amplitude bottom-to-top, quantised into discrete cells.
pub fn paint_bar_spectrum(painter: &Painter, rect: Rect, bars: &[f32], _playing: bool) {
    let cell = LED_CELL;
    let Some(grid) = spectrum_grid(rect, cell) else {
        return;
    };
    painted_grid(painter, &grid);
    painted_labels(painter, rect, &grid);

    if !bars.iter().any(|bar| *bar > 0.01) {
        return;
    }

    for col in 0..grid.cols {
        let norm = mapped_bar(bars, grid.cols, col);
        if norm <= 0.01 {
            continue;
        }
        let lit = ((norm * grid.signal_rows as f32).ceil() as usize).clamp(1, grid.signal_rows);
        for row in 0..lit {
            let t = (row as f32 + 1.0) / grid.signal_rows as f32;
            let grey = (56.0 + 168.0 * t.powf(0.85)).round() as u8;
            let alpha = (34.0 + 66.0 * t).round() as u8;
            let x0 = grid.rect.left() + col as f32 * grid.cell_w;
            let x1 = x0 + grid.cell_w;
            let y0 = grid.rect.bottom() - (row + 1) as f32 * grid.cell_h;
            let y1 = y0 + grid.cell_h;
            painter.rect_filled(
                Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1)),
                0.0,
                theme::alpha(theme::ink(grey), alpha),
            );
        }
    }
}

// Character-stream rendition of the same spectrum. The classic ASCII density
// ramp `" .:-=+*#%@"` maps dark to bright; phase drift keeps it moving.
pub fn paint_char_stream(painter: &Painter, rect: Rect, bars: &[f32], phase: f32) {
    const RAMP: [&str; 10] = [" ", ".", ":", "-", "=", "+", "*", "#", "%", "@"];
    let cell = CHAR_CELL;
    let Some(grid) = spectrum_grid(rect, cell) else {
        return;
    };
    painted_grid(painter, &grid);
    painted_labels(painter, rect, &grid);

    if !bars.iter().any(|bar| *bar > 0.01) {
        return;
    }

    let font = FontId::new(12.0, FontFamily::Monospace);
    for col in 0..grid.cols {
        let norm = mapped_bar(bars, grid.cols, col);
        if norm <= 0.01 {
            continue;
        }
        let lit = ((norm * grid.signal_rows as f32).ceil() as usize).clamp(1, grid.signal_rows);
        for row in 0..lit {
            let t = (row as f32 + 1.0) / grid.signal_rows as f32;
            let drift =
                ((phase * 2.2 + col as f32 * 0.37 - row as f32 * 0.61).sin() * 0.5 + 0.5) * 0.08;
            let level = (t.powf(0.9) * (0.45 + 0.55 * norm) + drift).clamp(0.0, 1.0);
            let glyph = RAMP[(level * (RAMP.len() - 1) as f32).round() as usize];
            let alpha = (30.0 + 90.0 * t).round() as u8;
            let x = grid.rect.left() + (col as f32 + 0.5) * grid.cell_w;
            let y = grid.rect.bottom() - (row as f32 + 0.5) * grid.cell_h;
            painter.text(
                Pos2::new(x, y),
                Align2::CENTER_CENTER,
                glyph,
                font.clone(),
                theme::alpha(theme::ink((80.0 + 150.0 * t).round() as u8), alpha),
            );
        }
    }
}

pub fn paint_core(
    painter: &Painter,
    rect: Rect,
    visual: VisualState,
    playing: bool,
    paused: bool,
    pulse_enabled: bool,
    elapsed: &str,
    total: &str,
) -> Rect {
    let usable = (rect.width().min(rect.height()) * 0.5 - 8.0).max(1.0);
    let scale = (usable / 258.0).clamp(0.45, 2.4);
    let core_size = 160.48 * scale;
    let radius = core_size * 0.5;
    let center = rect.center();

    let raw_pulse = 0.5 + 0.5 * (visual.phase * 2.0).sin();
    let pulse = if playing {
        if pulse_enabled {
            0.35 + 0.25 * raw_pulse
        } else {
            0.5
        }
    } else {
        0.0
    };
    let energy = if pulse_enabled { visual.energy } else { 0.0 };
    let kick = if pulse_enabled { visual.kick } else { 0.0 };
    let bass = if pulse_enabled { visual.bass } else { 0.0 };

    let (intensity, disc_scale) = if playing {
        if pulse_enabled {
            (
                (0.34 + 0.04 * pulse + 0.18 * energy + 0.10 * bass + 0.48 * kick).clamp(0.0, 1.0),
                1.0 + 0.03 * kick + 0.012 * bass,
            )
        } else {
            (0.36, 1.0)
        }
    } else {
        ((0.28 + 0.14 * pulse).clamp(0.0, 1.0), 1.0)
    };

    let ring = theme::ink(214);
    let hi = theme::ink(255);
    let deep = theme::ink(74);
    let cr = theme::ink(214);
    let cr2 = theme::ink(92);

    // Subtle monochrome bloom around the disc.
    let glow_r = (radius * (0.92 + 0.10 * kick + 0.04 * bass)).min(radius * 0.985);
    radial_rings(
        painter,
        center,
        glow_r,
        &[
            (
                0.0,
                Color32::from_rgba_unmultiplied(
                    cr.r(),
                    cr.g(),
                    cr.b(),
                    (10.0 + 20.0 * intensity + 22.0 * kick) as u8,
                ),
            ),
            (
                0.55,
                Color32::from_rgba_unmultiplied(
                    cr2.r(),
                    cr2.g(),
                    cr2.b(),
                    (5.0 + 9.0 * intensity + 10.0 * kick) as u8,
                ),
            ),
            (1.0, Color32::TRANSPARENT),
        ],
        56,
    );

    let base_disc_r = radius * 0.70;
    let disc_r = base_disc_r * disc_scale;
    radial_rings(
        painter,
        center,
        disc_r,
        &[
            (0.00, theme::alpha(theme::ink(30), 246)),
            (0.45, theme::alpha(theme::ink(19), 238)),
            (0.82, theme::alpha(theme::ink(10), 232)),
            (1.00, theme::alpha(theme::ink(5), 224)),
        ],
        64,
    );

    painter.circle_stroke(
        center,
        disc_r,
        Stroke::new(
            (radius * 0.018).max(1.6),
            Color32::from_rgba_unmultiplied(
                ring.r(),
                ring.g(),
                ring.b(),
                (42.0 + 42.0 * intensity) as u8,
            ),
        ),
    );

    if playing {
        draw_beat_ring(
            painter, center, disc_r, radius, kick, bass, energy, ring, hi, deep,
        );
    } else {
        let beat_rr = disc_r * 1.05;
        painter.circle_stroke(
            center,
            beat_rr,
            Stroke::new(
                (radius * 0.022).max(2.0),
                Color32::from_rgba_unmultiplied(ring.r(), ring.g(), ring.b(), 72),
            ),
        );
    }

    let core_r = disc_r * (0.26 + 0.05 * energy + 0.012 * pulse + 0.14 * kick + 0.03 * bass);
    radial_rings(
        painter,
        center,
        core_r,
        &[
            (
                0.0,
                theme::alpha(
                    theme::ink(255),
                    (14.0 + 26.0 * intensity + 45.0 * kick) as u8,
                ),
            ),
            (
                1.0,
                Color32::from_rgba_unmultiplied(ring.r(), ring.g(), ring.b(), 0),
            ),
        ],
        28,
    );

    let time_size = (base_disc_r * 0.22).clamp(9.0, 17.0);
    let status_size = (base_disc_r * 0.17).clamp(10.0, 16.0);

    painter.text(
        Pos2::new(center.x, center.y - base_disc_r * 0.04),
        Align2::CENTER_CENTER,
        format!("{}/{}", elapsed, total),
        FontId::new(time_size, FontFamily::Monospace),
        theme::alpha(theme::ink(230), 225),
    );

    let (status, status_color) = if playing && !paused {
        ("[PLAYING]", theme::alpha(theme::ink(205), 190))
    } else if paused {
        ("[PAUSED]", theme::alpha(theme::ink(145), 160))
    } else {
        ("[STANDBY]", theme::alpha(theme::ink(100), 130))
    };
    painter.text(
        Pos2::new(center.x, center.y + base_disc_r * 0.30),
        Align2::CENTER_CENTER,
        status,
        FontId::new(status_size, FontFamily::Monospace),
        status_color,
    )
}

fn draw_beat_ring(
    painter: &Painter,
    center: Pos2,
    disc_r: f32,
    radius: f32,
    kick: f32,
    bass: f32,
    energy: f32,
    ring: Color32,
    hi: Color32,
    deep: Color32,
) {
    let base = disc_r * 1.045;
    let expand = 1.0 + 0.16 * kick + 0.035 * bass + 0.02 * energy;
    let rr = (base * expand).min(radius * 0.98);

    if kick > 0.08 {
        let shock = (rr * (1.0 + 0.08 * kick)).min(radius * 0.995);
        let shock_w = (radius * 0.02 * kick).max(2.0);
        let trail_r = rr.max(shock - shock_w * 1.5);
        painter.circle_stroke(
            center,
            trail_r,
            Stroke::new(
                shock_w * 3.0,
                Color32::from_rgba_unmultiplied(
                    ring.r(),
                    ring.g(),
                    ring.b(),
                    (8.0 + 22.0 * kick) as u8,
                ),
            ),
        );
        let visible_w = shock_w.min(((radius - shock) * 1.7).max(1.0));
        painter.circle_stroke(
            center,
            shock,
            Stroke::new(
                visible_w,
                Color32::from_rgba_unmultiplied(hi.r(), hi.g(), hi.b(), (18.0 + 55.0 * kick) as u8),
            ),
        );
    }

    let halo_w = (radius * (0.055 + 0.10 * kick + 0.02 * bass)).max(6.0);
    let halo_a = (16.0 + 28.0 * bass + 64.0 * kick) as u8;
    let halo_outer = (rr + halo_w * 0.58).min(radius * 0.995);
    let halo_inner = (rr - halo_w * 0.72).max(0.0);
    radial_rings(
        painter,
        center,
        halo_outer,
        &[
            (0.0, Color32::TRANSPARENT),
            (
                (halo_inner / halo_outer).clamp(0.0, 1.0),
                Color32::TRANSPARENT,
            ),
            (
                (rr / halo_outer).clamp(0.0, 0.998),
                Color32::from_rgba_unmultiplied(ring.r(), ring.g(), ring.b(), halo_a.min(160)),
            ),
            (1.0, Color32::TRANSPARENT),
        ],
        64,
    );

    let main_w = (radius * (0.028 + 0.035 * kick + 0.01 * bass)).max(2.4);
    let main_a = (108.0 + 42.0 * energy + 70.0 * kick).min(220.0) as u8;
    let energy_w = (main_w * 1.65).max(3.8);
    let energy_a = (26.0 + 24.0 * energy + 48.0 * kick + 12.0 * bass).min(130.0) as u8;
    painter.circle_stroke(
        center,
        rr,
        Stroke::new(
            energy_w,
            Color32::from_rgba_unmultiplied(deep.r(), deep.g(), deep.b(), energy_a),
        ),
    );

    let segments = 96;
    for i in 0..segments {
        let t0 = i as f32 / segments as f32;
        let t1 = (i + 1) as f32 / segments as f32;
        let color = conical_color(t0, ring, hi, deep, main_a);
        let a0 = t0 * TAU;
        let a1 = t1 * TAU;
        painter.line_segment(
            [
                center + Vec2::new(a0.cos(), a0.sin()) * rr,
                center + Vec2::new(a1.cos(), a1.sin()) * rr,
            ],
            Stroke::new(main_w, color),
        );
    }

    painter.circle_stroke(
        center,
        rr * 0.985,
        Stroke::new(
            (radius * (0.012 + 0.02 * kick)).max(1.2),
            Color32::from_rgba_unmultiplied(hi.r(), hi.g(), hi.b(), (58.0 + 92.0 * kick) as u8),
        ),
    );
}

fn conical_color(t: f32, ring: Color32, hi: Color32, deep: Color32, main_a: u8) -> Color32 {
    let stops = [
        (
            0.00,
            Color32::from_rgba_unmultiplied(ring.r(), ring.g(), ring.b(), main_a),
        ),
        (
            0.18,
            Color32::from_rgba_unmultiplied(
                hi.r(),
                hi.g(),
                hi.b(),
                (main_a as u16 + 12).min(255) as u8,
            ),
        ),
        (
            0.42,
            Color32::from_rgba_unmultiplied(ring.r(), ring.g(), ring.b(), main_a),
        ),
        (
            0.70,
            Color32::from_rgba_unmultiplied(
                deep.r(),
                deep.g(),
                deep.b(),
                main_a.saturating_sub(24).max(80),
            ),
        ),
        (
            1.00,
            Color32::from_rgba_unmultiplied(ring.r(), ring.g(), ring.b(), main_a),
        ),
    ];
    let t = t.rem_euclid(1.0);
    for pair in stops.windows(2) {
        if t >= pair[0].0 && t <= pair[1].0 {
            let span = (pair[1].0 - pair[0].0).max(1e-6);
            return theme::mix(pair[0].1, pair[1].1, (t - pair[0].0) / span);
        }
    }
    stops[0].1
}

fn radial_rings(
    painter: &Painter,
    center: Pos2,
    radius: f32,
    stops: &[(f32, Color32)],
    rings: usize,
) {
    if radius <= 0.0 || rings == 0 {
        return;
    }
    let rings = rings.max(8);
    let step = radius / rings as f32;
    for i in (0..rings).rev() {
        let t = (i as f32 + 0.5) / rings as f32;
        let color = gradient_color(stops, t);
        if color.a() == 0 {
            continue;
        }
        painter.circle_stroke(center, radius * t, Stroke::new(step + 0.9, color));
    }
    let center_color = gradient_color(stops, 0.0);
    if center_color.a() > 0 {
        painter.circle_filled(center, step * 0.55, center_color);
    }
}

fn gradient_color(stops: &[(f32, Color32)], t: f32) -> Color32 {
    let t = t.clamp(0.0, 1.0);
    for pair in stops.windows(2) {
        if t >= pair[0].0 && t <= pair[1].0 {
            let span = (pair[1].0 - pair[0].0).max(1e-6);
            return theme::mix(pair[0].1, pair[1].1, (t - pair[0].0) / span);
        }
    }
    if t < stops[0].0 {
        stops[0].1
    } else {
        stops[stops.len() - 1].1
    }
}
