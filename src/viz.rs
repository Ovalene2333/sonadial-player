use std::collections::VecDeque;

use eframe::egui::{
    Align2, Color32, FontFamily, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Vec2,
};

use crate::notes::NoteEvent;
use crate::pitch::NoteFrame;
use crate::spectrum::{VisualState, MAX_DB_VALUE, NUM_BARS};
use crate::theme;

const TAU: f32 = std::f32::consts::TAU;

// One LED cell matches the 12 px TUI body text size.
const LED_CELL: f32 = 12.0;
// DOT MATRIX layout: one cell is two dot columns wide and four dot rows tall.
// The actual dots are drawn as pixel-snapped squares, so the cell ratio only
// drives the frequency-column layout; the faint backdrop lattice stays square
// like the default style.
const DOT_CELL_H: f32 = 18.0;
const DOT_CELL_W: f32 = 9.0;

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
    peaks: &[f32],
    show_peaks: bool,
    _playing: bool,
    rotation: f32,
    size_scale: f32,
    led_bars: bool,
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

    if led_bars {
        paint_radial_led(
            painter, center, inner_r, max_amp, bars, peaks, show_peaks, rotation, scale,
        );
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

        if show_peaks {
            let peak = peaks.get(i).copied().unwrap_or(0.0);
            let peak_norm = (peak / MAX_DB_VALUE).clamp(0.0, 1.0);
            if peak_norm > norm + 0.012 {
                let point = center + dir * (inner_r + peak_norm * max_amp);
                let peak_color = spectrum_color(peak_norm);
                painter.circle_filled(point, (bar_w * 0.9).max(1.0), theme::alpha(peak_color, 190));
            }
        }
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

// TUI-style radial LED matrix. Each spoke is split into discrete square cells;
// no glow, no rounded caps, and only four grey levels, matching the background
// LED wall and the rest of the terminal-inspired surface.
fn paint_radial_led(
    painter: &Painter,
    center: Pos2,
    inner_r: f32,
    max_amp: f32,
    bars: &[f32],
    peaks: &[f32],
    show_peaks: bool,
    rotation: f32,
    scale: f32,
) {
    let cell_step = (5.0 * scale).clamp(4.0, 7.0);
    let cell_len = (cell_step * 0.66).max(2.0);
    let cell_w = (cell_step * 0.62).max(1.0);
    let steps = ((max_amp / cell_step).floor() as usize).max(1);
    let half_w = cell_w * 0.5;

    for i in 0..NUM_BARS {
        let norm = (bars.get(i).copied().unwrap_or(0.0) / MAX_DB_VALUE).clamp(0.0, 1.0);
        let peak_norm = (peaks.get(i).copied().unwrap_or(0.0) / MAX_DB_VALUE).clamp(0.0, 1.0);
        if norm <= 0.01 && peak_norm <= 0.01 {
            continue;
        }
        let lit = if norm <= 0.01 {
            0
        } else {
            ((norm * steps as f32).ceil() as usize).clamp(1, steps)
        };
        let angle = std::f32::consts::FRAC_PI_2 + i as f32 / NUM_BARS as f32 * TAU + rotation;
        let dir = Vec2::new(angle.cos(), angle.sin());
        let tangent = Vec2::new(-dir.y, dir.x) * half_w;

        for cell in 0..lit {
            let r0 = inner_r + cell as f32 * cell_step;
            let r1 = (r0 + cell_len).min(inner_r + max_amp);
            let t = ((cell + 1) as f32 / steps as f32).clamp(0.0, 1.0);
            let level = (t * 3.0).round() / 3.0;
            let color = spectrum_color(level);
            let p0 = center + dir * r0 - tangent;
            let p1 = center + dir * r1 - tangent;
            let p2 = center + dir * r1 + tangent;
            let p3 = center + dir * r0 + tangent;
            painter.add(Shape::convex_polygon(
                vec![p0, p1, p2, p3],
                color,
                Stroke::NONE,
            ));
        }

        if show_peaks && peak_norm > norm + 0.012 {
            let peak_r = inner_r + peak_norm * max_amp;
            let r0 = (peak_r - cell_len * 0.5).max(inner_r);
            let r1 = (peak_r + cell_len * 0.5).min(inner_r + max_amp);
            let p0 = center + dir * r0 - tangent;
            let p1 = center + dir * r1 - tangent;
            let p2 = center + dir * r1 + tangent;
            let p3 = center + dir * r0 + tangent;
            painter.add(Shape::convex_polygon(
                vec![p0, p1, p2, p3],
                spectrum_color(1.0),
                Stroke::NONE,
            ));
        }
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
    spectrum_grid_cells(rect, cell, cell)
}

fn spectrum_grid_cells(rect: Rect, cell_width: f32, cell_height: f32) -> Option<SpectrumGrid> {
    let side_margin = 12.0;
    let top_margin = 24.0;
    let bottom_margin = 24.0;
    let max_w = (rect.width() - side_margin * 2.0).max(0.0);
    let max_h = (rect.height() - top_margin - bottom_margin).max(0.0);
    if max_w < cell_width * 8.0 || max_h < cell_height * 4.0 {
        return None;
    }

    let rows = ((max_h / cell_height).floor() as usize).max(4);
    let cell_h = max_h / rows as f32;
    // Preserve the requested cell aspect so the dot lattice keeps the same
    // proportions across different window sizes.
    let cell_w = cell_h * (cell_width / cell_height);
    let cols = (max_w / cell_w).floor() as usize;
    if cols < 8 {
        return None;
    }
    let panel_w = cols as f32 * cell_w;
    let panel_left = rect.left() + side_margin + (max_w - panel_w) * 0.5;
    let signal_rows = (((max_h * 0.40) / cell_h).round() as usize).clamp(1, rows);
    let panel = Rect::from_min_max(
        Pos2::new(panel_left, rect.top() + top_margin),
        Pos2::new(panel_left + panel_w, rect.bottom() - bottom_margin),
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
        let x0 = grid.rect.left() + col as f32 * grid.cell_w;
        let x1 = x0 + grid.cell_w;
        for row in 0..lit {
            let t = (row as f32 + 1.0) / grid.signal_rows as f32;
            let grey = (56.0 + 168.0 * t.powf(0.85)).round() as u8;
            let alpha = (34.0 + 66.0 * t).round() as u8;
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

// DOT MATRIX spectrum: frequency left-to-right, amplitude bottom-to-top,
// rendered as a uniform field of small square dots. Direct dots avoid the
// sub-pixel seams and moiré produced by rasterizing tiny glyphs.
pub fn paint_dot_stream(painter: &Painter, rect: Rect, bars: &[f32]) {
    let Some(grid) = spectrum_grid_cells(rect, DOT_CELL_W, DOT_CELL_H) else {
        return;
    };
    // The faint lattice and frequency labels follow the default style so both
    // modes share the same backdrop; only the lit dots use the dot cell grid.
    let backdrop = spectrum_grid(rect, LED_CELL);
    let bg_grid = backdrop.as_ref().unwrap_or(&grid);
    painted_grid(painter, bg_grid);
    painted_labels(painter, rect, bg_grid);

    if !bars.iter().any(|bar| *bar > 0.01) {
        return;
    }

    let ppp = painter.ctx().pixels_per_point().max(1.0);
    let snap = |value: f32| (value * ppp).round() / ppp;
    let step_px = ((grid.cell_h * 0.25) * ppp).round().clamp(2.0, 24.0);
    let step = step_px / ppp;
    let dot_px = (step_px * 0.58).round().clamp(1.0, step_px - 1.0);
    let dot = dot_px / ppp;

    let cols = (grid.rect.width() / step).floor().max(1.0) as usize;
    let rows = ((grid.rect.height() * 0.40) / step).floor().max(1.0) as usize;
    let left = snap(grid.rect.center().x - cols as f32 * step * 0.5);
    let bottom = snap(grid.rect.bottom());
    let center = rect.center();
    let core_r = core_radius(rect);
    let square = Vec2::splat(dot);

    for col in 0..cols {
        let norm = mapped_bar(bars, cols, col);
        if norm <= 0.01 {
            continue;
        }
        let lit = ((norm * rows as f32).ceil() as usize).clamp(1, rows);
        for row in 0..lit {
            let x = left + (col as f32 + 0.5) * step;
            let y = bottom - (row as f32 + 0.5) * step;
            let dist = ((x - center.x).powi(2) + (y - center.y).powi(2)).sqrt();
            if dist <= core_r {
                continue;
            }
            let t = (row as f32 + 1.0) / rows as f32;
            let grey = (92.0 + 148.0 * t).round() as u8;
            let alpha = (42.0 + 104.0 * t).round() as u8;
            painter.rect_filled(
                Rect::from_center_size(Pos2::new(x, y), square),
                0.0,
                theme::alpha(theme::ink(grey), alpha),
            );
        }
    }
}

fn core_radius(rect: Rect) -> f32 {
    let usable = (rect.width().min(rect.height()) * 0.5 - 8.0).max(1.0);
    let scale = (usable / 258.0).clamp(0.45, 2.4);
    80.24 * scale * 0.70
}

pub fn paint_core(
    painter: &Painter,
    rect: Rect,
    visual: VisualState,
    playing: bool,
    paused: bool,
    dot_style: bool,
    pulse_enabled: bool,
    beat_age: f32,
    ring_strength: f32,
    elapsed: &str,
    total: &str,
) -> Rect {
    let base_disc_r = core_radius(rect);
    let center = rect.center();

    if dot_style {
        // Beat values only exist while CORE PULSE is enabled. The dot circle
        // never rotates or scales; light beats change its brightness and heavy
        // beats also send a dotted ring outwards.
        let kick = if pulse_enabled { visual.kick } else { 0.0 };
        if let Some(grid) = spectrum_grid_cells(rect, DOT_CELL_W, DOT_CELL_H) {
            let age = if pulse_enabled { beat_age } else { 1.0 };
            let strength = if pulse_enabled { ring_strength } else { 0.0 };
            paint_core_dots(
                painter,
                &grid,
                center,
                base_disc_r,
                playing,
                kick,
                age,
                strength,
            );
        }
    } else {
        paint_core_classic(painter, center, visual, playing, pulse_enabled, base_disc_r);
    }

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

/// The original smooth vector core used by the default style. It keeps the
/// soft bloom, the shaded disc and the expanding beat ring.
fn paint_core_classic(
    painter: &Painter,
    center: Pos2,
    visual: VisualState,
    playing: bool,
    pulse_enabled: bool,
    base_disc_r: f32,
) {
    let radius = base_disc_r / 0.70;

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

/// Draw the core as individual square dots on a pixel-snapped lattice. Font
/// glyphs are not used here: rasterizing hundreds of tiny glyphs at fractional
/// positions produces alternating gaps and a visible moiré across the disc.
/// Direct dots keep the same matrix look with an exact uniform pitch.
fn paint_core_dots(
    painter: &Painter,
    grid: &SpectrumGrid,
    center: Pos2,
    disc_r: f32,
    playing: bool,
    kick: f32,
    beat_age: f32,
    ring_strength: f32,
) {
    const PULSE_LIFE: f32 = 0.42;

    let ppp = painter.ctx().pixels_per_point().max(1.0);
    let snap = |value: f32| (value * ppp).round() / ppp;
    let step_px = ((grid.cell_h * 0.25) * ppp).round().clamp(2.0, 24.0);
    let step = step_px / ppp;
    let dot_px = (step_px * 0.58).round().clamp(1.0, step_px - 1.0);
    let dot = dot_px / ppp;
    let center = Pos2::new(snap(center.x), snap(center.y));

    let t = (beat_age / PULSE_LIFE).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - t).powi(2);
    let ring_on = playing && ring_strength > 0.0 && t < 1.0;
    let ring_r = disc_r + step * 1.2 + step * 6.8 * ease;
    let ring_half = step * 0.95;
    let ring_alpha = ((1.0 - t).powi(2) * (120.0 + 100.0 * ring_strength))
        .round()
        .clamp(0.0, 230.0) as u8;

    let circle_level = if playing {
        (0.28 + 1.15 * kick).min(1.0)
    } else {
        0.28
    };
    let circle_alpha = (circle_level * 180.0).round() as u8;

    let reach = (disc_r + step * 2.0).max(ring_r + ring_half) + step;
    let n = (reach / step).ceil() as i32 + 1;
    let square = Vec2::splat(dot);
    for iy in -n..=n {
        for ix in -n..=n {
            let x = center.x + ix as f32 * step;
            let y = center.y + iy as f32 * step;
            let dist = ((x - center.x).powi(2) + (y - center.y).powi(2)).sqrt();
            let in_circle = dist <= disc_r;
            let in_ring = ring_on && (dist - ring_r).abs() <= ring_half;
            if !in_circle && !in_ring {
                continue;
            }
            let pos = Pos2::new(x, y);
            let rect = Rect::from_center_size(pos, square);
            if in_circle {
                let edge = (dist / disc_r).clamp(0.0, 1.0);
                let alpha = (circle_alpha as f32 * (0.55 + 0.45 * edge)).round() as u8;
                painter.rect_filled(rect, 0.0, theme::alpha(theme::ink(236), alpha));
            } else {
                painter.rect_filled(rect, 0.0, theme::alpha(theme::ink(255), ring_alpha));
            }
        }
    }
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

fn is_black_key(midi: u8) -> bool {
    matches!(midi % 12, 1 | 3 | 6 | 8 | 10)
}

const PIANO_KEY_H: f32 = 84.0;
const PIANO_HISTORY_SECS: f64 = 8.0;

#[derive(Clone, Copy, Default)]
struct KeyGeom {
    center_x: f32,
    width: f32,
    black: bool,
}

fn piano_key_layout(inner: Rect) -> [KeyGeom; crate::pitch::NUM_NOTES] {
    let mut keys = [KeyGeom::default(); crate::pitch::NUM_NOTES];
    let white_w = inner.width() / 52.0;
    let black_w = white_w * 0.62;

    let mut white_index = 0usize;
    for midi in crate::pitch::MIN_MIDI..=crate::pitch::MAX_MIDI {
        if is_black_key(midi) {
            continue;
        }
        keys[(midi - crate::pitch::MIN_MIDI) as usize] = KeyGeom {
            center_x: inner.left() + (white_index as f32 + 0.5) * white_w,
            width: white_w - 1.0,
            black: false,
        };
        white_index += 1;
    }

    white_index = 0;
    for midi in crate::pitch::MIN_MIDI..=crate::pitch::MAX_MIDI {
        if !is_black_key(midi) {
            white_index += 1;
            continue;
        }
        keys[(midi - crate::pitch::MIN_MIDI) as usize] = KeyGeom {
            center_x: inner.left() + white_index as f32 * white_w,
            width: black_w,
            black: true,
        };
    }
    keys
}

/// Exclusive PIANO style: a quiet terminal panel with a solid keyboard and
/// note bars drifting upward as the track advances.
pub fn paint_piano_style(
    painter: &Painter,
    rect: Rect,
    frames: &VecDeque<NoteFrame>,
    events: Option<&[NoteEvent]>,
    levels: &[f32],
    position: f64,
    playing: bool,
) {
    if rect.width() < 180.0 || rect.height() < 180.0 {
        return;
    }

    let margin = 24.0;
    let key_h = (rect.height() * 0.26).clamp(58.0, PIANO_KEY_H);
    let inner = Rect::from_min_max(
        Pos2::new(rect.left() + margin, rect.bottom() - margin - key_h),
        Pos2::new(rect.right() - margin, rect.bottom() - margin),
    );
    let roll = Rect::from_min_max(
        Pos2::new(inner.left(), rect.top() + 46.0),
        Pos2::new(inner.right(), inner.top() - 2.0),
    );
    if roll.height() < 50.0 || roll.width() < 90.0 {
        return;
    }

    let ppp = painter.ctx().pixels_per_point().max(1.0);
    let snap = |value: f32| (value * ppp).round() / ppp;
    let keys = piano_key_layout(inner);
    let px_per_sec = (roll.height() as f64 / PIANO_HISTORY_SECS) as f32;

    paint_piano_frame(
        painter,
        rect.left() + margin,
        rect.top() + 40.0,
        rect.right() - margin,
        rect.bottom() - margin,
    );

    painter.line_segment(
        [inner.left_top(), inner.right_top()],
        Stroke::new(1.0, theme::alpha(theme::ink(130), 190)),
    );

    // Once offline transcription is ready, draw clean note events. Until then
    // fall back to the real-time activity trail.
    if let Some(events) = events {
        let horizon = position - PIANO_HISTORY_SECS;
        for event in events {
            if event.end < horizon || event.start > position {
                continue;
            }
            let start = event.start.max(horizon);
            let end = event.end.min(position);
            let mut y_top = snap(inner.top() - ((position - start) as f32 * px_per_sec));
            let mut y_bottom = snap(inner.top() - ((position - end) as f32 * px_per_sec));
            if y_bottom < roll.top() {
                continue;
            }
            y_top = y_top.max(roll.top());
            y_bottom = y_bottom.min(inner.top());
            if y_bottom - y_top < 2.0 {
                y_bottom = (y_top + 2.0).min(inner.top());
            }
            let active = event.start <= position && event.end >= position;
            let age = (position - event.end).max(0.0);
            let fade = if active {
                1.0
            } else {
                (1.0 - (age / PIANO_HISTORY_SECS)).clamp(0.0, 1.0).powf(0.8)
            } as f32;
            let geom = keys[(event.midi - crate::pitch::MIN_MIDI) as usize];
            let lane = geom.width * 0.60;
            let grey = (96.0 + 132.0 * event.velocity).round().clamp(0.0, 255.0) as u8;
            let alpha = ((72.0 + 150.0 * event.velocity) * (0.45 + 0.55 * event.confidence) * fade)
                .round()
                .clamp(0.0, 255.0) as u8;
            let bar = Rect::from_min_max(
                Pos2::new(geom.center_x - lane * 0.5, y_top),
                Pos2::new(geom.center_x + lane * 0.5, y_bottom),
            );
            painter.rect_filled(bar, 0.0, theme::alpha(theme::ink(grey), alpha));
            if active {
                painter.rect_filled(
                    Rect::from_min_max(
                        Pos2::new(bar.left(), y_bottom - 2.0),
                        Pos2::new(bar.right(), y_bottom),
                    ),
                    0.0,
                    theme::alpha(theme::ink(246), 230),
                );
            }
        }
    } else {
        // Real-time fallback: solid bars, one per sample row, fading with age.
        let mut previous = [0.0_f32; crate::pitch::NUM_NOTES];
        let row_h = (px_per_sec * 0.045).max(2.0);
        let fade_floor = if playing { 0.0 } else { 0.28 };
        for frame in frames {
            let age = position - frame.time;
            if age < 0.0 || age > PIANO_HISTORY_SECS {
                previous.copy_from_slice(&frame.levels);
                continue;
            }
            let y = snap(inner.top() - (age as f32 * px_per_sec));
            if y < roll.top() || y > roll.bottom() {
                previous.copy_from_slice(&frame.levels);
                continue;
            }
            let fade = (1.0 - (age / PIANO_HISTORY_SECS) as f32)
                .clamp(0.0, 1.0)
                .powf(0.8)
                .max(fade_floor);
            for (index, level) in frame.levels.iter().enumerate() {
                if *level < 0.26 {
                    continue;
                }
                let geom = keys[index];
                let onset = previous[index] < 0.26;
                let lane = geom.width * 0.60;
                let grey = (82.0 + 146.0 * level).round().clamp(0.0, 255.0) as u8;
                let alpha = ((54.0 + 186.0 * level) * fade).round().clamp(0.0, 255.0) as u8;
                painter.rect_filled(
                    Rect::from_center_size(
                        Pos2::new(geom.center_x, y),
                        Vec2::new(lane, row_h + 0.5),
                    ),
                    0.0,
                    theme::alpha(theme::ink(grey), alpha),
                );
                if onset {
                    painter.rect_filled(
                        Rect::from_center_size(
                            Pos2::new(geom.center_x, y),
                            Vec2::new(lane, (2.0 * ppp).round() / ppp),
                        ),
                        0.0,
                        theme::alpha(theme::ink(248), (240.0 * fade) as u8),
                    );
                }
            }
            previous.copy_from_slice(&frame.levels);
        }
    }

    painter.rect_filled(inner, 0.0, theme::ink(6));
    painter.rect_stroke(
        inner,
        0.0,
        Stroke::new(1.0, theme::alpha(theme::ink(80), 210)),
        StrokeKind::Inside,
    );

    let black_h = inner.height() * 0.62;
    for midi in crate::pitch::MIN_MIDI..=crate::pitch::MAX_MIDI {
        let index = (midi - crate::pitch::MIN_MIDI) as usize;
        let geom = keys[index];
        if geom.black {
            continue;
        }
        let level = levels.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        paint_solid_key(painter, inner, geom, inner.height(), level);
    }
    for midi in crate::pitch::MIN_MIDI..=crate::pitch::MAX_MIDI {
        let index = (midi - crate::pitch::MIN_MIDI) as usize;
        let geom = keys[index];
        if !geom.black {
            continue;
        }
        let level = levels.get(index).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        paint_solid_key(painter, inner, geom, black_h, level);
    }

    for midi in crate::pitch::MIN_MIDI..=crate::pitch::MAX_MIDI {
        if midi % 12 != 0 {
            continue;
        }
        let geom = keys[(midi - crate::pitch::MIN_MIDI) as usize];
        painter.text(
            Pos2::new(geom.center_x, inner.bottom() - 2.0),
            Align2::CENTER_BOTTOM,
            crate::pitch::midi_name(midi),
            FontId::new(8.0, FontFamily::Monospace),
            theme::alpha(theme::ink(135), 230),
        );
    }
}

fn paint_piano_frame(painter: &Painter, left: f32, top: f32, right: f32, bottom: f32) {
    let stroke = Stroke::new(1.0, theme::alpha(theme::ink(90), 95));
    let len = 10.0;
    for (x, y, dx, dy) in [
        (left, top, 1.0, 1.0),
        (right, top, -1.0, 1.0),
        (left, bottom, 1.0, -1.0),
        (right, bottom, -1.0, -1.0),
    ] {
        painter.line_segment([Pos2::new(x, y), Pos2::new(x + len * dx, y)], stroke);
        painter.line_segment([Pos2::new(x, y), Pos2::new(x, y + len * dy)], stroke);
    }
}

fn paint_solid_key(painter: &Painter, inner: Rect, geom: KeyGeom, height: f32, level: f32) {
    let left = geom.center_x - geom.width * 0.5;
    let key = Rect::from_min_max(
        Pos2::new(left, inner.top()),
        Pos2::new(left + geom.width, inner.top() + height),
    );
    let level = level.powf(0.7);
    let base = if geom.black { 3 } else { 20 };
    painter.rect_filled(
        key,
        0.0,
        theme::mix(theme::ink(base), theme::ink(234), level),
    );
    painter.rect_stroke(
        key,
        0.0,
        Stroke::new(1.0, theme::alpha(theme::ink(4), 200)),
        StrokeKind::Inside,
    );
    if level > 0.05 {
        painter.rect_filled(
            Rect::from_min_max(
                Pos2::new(key.left() + 1.0, key.top() + 1.0),
                Pos2::new(key.right() - 1.0, key.top() + 3.0),
            ),
            0.0,
            theme::ink(240),
        );
    }
}
