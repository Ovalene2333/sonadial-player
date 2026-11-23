// Stage painting (slint-ui branch): a faithful port of the egui-era viz.rs
// drawing algorithms onto the CPU rasterizer (raster.rs). One opaque
// full-window texture per frame carries the theme gradient, the background
// LED lattice, the radial spectrum with its glow layers and the classic core
// dial with its beat ring — the exact visuals the egui build drew on the GPU.

use crate::raster::{Canvas, Rgba};
use crate::spectrum::{VisualState, MAX_DB_VALUE, NUM_BARS};
use crate::theme::{self, Look, Rgb};

const TAU: f32 = std::f32::consts::TAU;
const LED_CELL: f32 = 12.0;
const DOTS_PER_CELL: usize = 2;
const SPECTRUM_GREYS: [(f32, u8); 4] = [(0.00, 52), (0.22, 104), (0.52, 178), (0.85, 246)];
const PEAK_MARKER_GAP_DB: f32 = 1.0;

fn ink(value: u8) -> Rgba {
    Rgba::from_rgb(theme::ink(value))
}

fn alpha(rgb: Rgb, a: u8) -> Rgba {
    Rgba::new(rgb[0], rgb[1], rgb[2], a)
}

fn ink_alpha(value: u8, a: u8) -> Rgba {
    alpha(theme::ink(value), a)
}

fn with_alpha(color: Rgba, a: u8) -> Rgba {
    Rgba::new(color.r, color.g, color.b, a)
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    a.mix(b, t)
}

fn spectrum_color(norm: f32) -> Rgba {
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
    ink(grey)
}

// ---------- radial spectrum (viz.rs paint_spectrum) ----------

#[allow(clippy::too_many_arguments)]
fn paint_spectrum(
    canvas: &mut Canvas,
    rect: (f32, f32, f32, f32),
    ppp: f32,
    bars: &[f32],
    peaks: &[f32],
    show_peaks: bool,
    rotation: f32,
    size_scale: f32,
    led_bars: bool,
    dot_bars: bool,
    lattice: DotLattice,
    kick: f32,
) {
    let (rx, ry, rw, rh) = rect;
    let usable = (rw.min(rh) * 0.5 - 8.0).max(1.0);
    let scale = (usable / 258.0).clamp(0.45, 2.4) * size_scale.clamp(0.7, 1.2);
    let center = (rx + rw * 0.5, ry + rh * 0.5);

    let inner_r = (120.0 * scale).min(usable * 0.75);
    let max_amp = (100.0 * scale).min((usable - inner_r - 2.0).max(10.0));
    let wide_w = (12.0 * scale).max(3.0);
    let glow_w = (6.0 * scale).max(2.0);
    let bar_w = (2.8 * scale).max(1.0);
    let stroke_w = (1.5 * scale).max(1.0);

    // Pixel modes have only discrete cells, including the peak markers.
    // Keep the continuous baseline and glow exclusive to the smooth mode.
    if led_bars {
        if dot_bars {
            paint_radial_dots(
                canvas, ppp, center, inner_r, max_amp, bars, peaks, show_peaks, rotation, lattice, kick,
            );
        } else {
            paint_radial_led(canvas, center, ppp, inner_r, max_amp, bars, peaks, show_peaks, rotation, scale);
        }
        return;
    }

    // A quiet baseline; no instrument bezel, ticks or labels.
    canvas.circle_stroke(
        center.0 * ppp,
        center.1 * ppp,
        inner_r * ppp,
        stroke_w * ppp,
        ink_alpha(210, 20),
    );
    canvas.radial_gradient(
        center.0 * ppp,
        center.1 * ppp,
        inner_r * 1.05 * ppp,
        &[
            (0.0, Rgba::TRANSPARENT),
            (0.85, Rgba::TRANSPARENT),
            (0.95, ink_alpha(210, 9)),
            (1.0, Rgba::TRANSPARENT),
        ],
    );

    if !bars.iter().any(|bar| *bar > 0.01)
        && !(show_peaks && peaks.iter().any(|peak| *peak > 0.01))
    {
        return;
    }

    let mut p1 = [(0.0f32, 0.0f32); NUM_BARS];
    let mut p2 = [(0.0f32, 0.0f32); NUM_BARS];
    let mut colors = [Rgba::TRANSPARENT; NUM_BARS];
    for i in 0..NUM_BARS {
        let height = bars.get(i).copied().unwrap_or(0.0);
        let norm = (height / MAX_DB_VALUE).clamp(0.0, 1.0);
        let color = spectrum_color(norm);
        let outer = inner_r + (norm * max_amp).max(inner_r * 0.02);
        let angle = std::f32::consts::FRAC_PI_2 + i as f32 / NUM_BARS as f32 * TAU + rotation;
        let dir = (angle.cos(), angle.sin());
        p1[i] = (center.0 + dir.0 * inner_r, center.1 + dir.1 * inner_r);
        p2[i] = (center.0 + dir.0 * outer, center.1 + dir.1 * outer);
        colors[i] = color;
    }

    // Very wide, very faint halo.
    for i in 0..NUM_BARS {
        line_with_caps(canvas, p1[i], p2[i], wide_w, with_alpha(colors[i], 8), ppp);
    }
    // Tight glow.
    for i in 0..NUM_BARS {
        line_with_caps(canvas, p1[i], p2[i], glow_w, with_alpha(colors[i], 26), ppp);
    }
    // Bright core line.
    for i in 0..NUM_BARS {
        line_with_caps(canvas, p1[i], p2[i], bar_w, colors[i], ppp);
    }

    // Peak markers go on top of every bar layer.
    if show_peaks {
        let dot_r = (bar_w * 0.9).max(1.0);
        for i in 0..NUM_BARS {
            let height = bars.get(i).copied().unwrap_or(0.0);
            let peak = peaks.get(i).copied().unwrap_or(0.0);
            if peak <= height + PEAK_MARKER_GAP_DB {
                continue;
            }
            let peak_norm = (peak / MAX_DB_VALUE).clamp(0.0, 1.0);
            let peak_r = inner_r + peak_norm * max_amp;
            let angle =
                std::f32::consts::FRAC_PI_2 + i as f32 / NUM_BARS as f32 * TAU + rotation;
            let dir = (angle.cos(), angle.sin());
            canvas.circle_filled(
                (center.0 + dir.0 * peak_r) * ppp,
                (center.1 + dir.1 * peak_r) * ppp,
                dot_r * ppp,
                with_alpha(spectrum_color(peak_norm), 190),
            );
        }
    }
}

fn line_with_caps(
    canvas: &mut Canvas,
    p1: (f32, f32),
    p2: (f32, f32),
    width: f32,
    color: Rgba,
    ppp: f32,
) {
    canvas.line(
        p1.0 * ppp,
        p1.1 * ppp,
        p2.0 * ppp,
        p2.1 * ppp,
        width * ppp,
        color,
    );
    let half = (width * 0.5) * ppp;
    if half > 0.0 {
        canvas.circle_filled(p1.0 * ppp, p1.1 * ppp, half, color);
        canvas.circle_filled(p2.0 * ppp, p2.1 * ppp, half, color);
    }
}

// ---------- core dial (viz.rs paint_core_classic) ----------

fn core_radius(usable: f32) -> f32 {
    let scale = (usable / 258.0).clamp(0.45, 2.4);
    80.24 * scale * 0.70
}

#[allow(clippy::too_many_arguments)]
fn paint_core_classic(
    canvas: &mut Canvas,
    center: (f32, f32),
    visual: VisualState,
    playing: bool,
    pulse_enabled: bool,
    base_disc_r: f32,
    ppp: f32,
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

    let ring = ink(214);
    let hi = ink(255);
    let deep = ink(74);
    let cr = ink(214);
    let cr2 = ink(92);

    // Subtle monochrome bloom around the disc.
    let glow_r = (radius * (0.92 + 0.10 * kick + 0.04 * bass)).min(radius * 0.985);
    canvas.radial_gradient(
        center.0 * ppp,
        center.1 * ppp,
        glow_r * ppp,
        &[
            (
                0.0,
                Rgba::new(cr.r, cr.g, cr.b, (10.0 + 20.0 * intensity + 22.0 * kick) as u8),
            ),
            (
                0.55,
                Rgba::new(cr2.r, cr2.g, cr2.b, (5.0 + 9.0 * intensity + 10.0 * kick) as u8),
            ),
            (1.0, Rgba::TRANSPARENT),
        ],
    );

    let disc_r = base_disc_r * disc_scale;
    canvas.radial_gradient(
        center.0 * ppp,
        center.1 * ppp,
        disc_r * ppp,
        &[
            (0.00, ink_alpha(30, 246)),
            (0.45, ink_alpha(19, 238)),
            (0.82, ink_alpha(10, 232)),
            (1.00, ink_alpha(5, 224)),
        ],
    );

    canvas.circle_stroke(
        center.0 * ppp,
        center.1 * ppp,
        disc_r * ppp,
        ((radius * 0.018).max(1.6)) * ppp,
        Rgba::new(ring.r, ring.g, ring.b, (42.0 + 42.0 * intensity) as u8),
    );

    if playing {
        draw_beat_ring(canvas, center, disc_r, radius, kick, bass, energy, ring, hi, deep, ppp);
    } else {
        canvas.circle_stroke(
            center.0 * ppp,
            center.1 * ppp,
            (disc_r * 1.05) * ppp,
            ((radius * 0.022).max(2.0)) * ppp,
            Rgba::new(ring.r, ring.g, ring.b, 72),
        );
    }

    let core_r = disc_r * (0.26 + 0.05 * energy + 0.012 * pulse + 0.14 * kick + 0.03 * bass);
    canvas.radial_gradient(
        center.0 * ppp,
        center.1 * ppp,
        core_r * ppp,
        &[
            (
                0.0,
                ink_alpha(255, (14.0 + 26.0 * intensity + 45.0 * kick) as u8),
            ),
            (1.0, Rgba::new(ring.r, ring.g, ring.b, 0)),
        ],
    );
}

#[allow(clippy::too_many_arguments)]
fn draw_beat_ring(
    canvas: &mut Canvas,
    center: (f32, f32),
    disc_r: f32,
    radius: f32,
    kick: f32,
    bass: f32,
    energy: f32,
    ring: Rgba,
    hi: Rgba,
    deep: Rgba,
    ppp: f32,
) {
    let base = disc_r * 1.045;
    let expand = 1.0 + 0.16 * kick + 0.035 * bass + 0.02 * energy;
    let rr = (base * expand).min(radius * 0.98);

    if kick > 0.08 {
        let shock = (rr * (1.0 + 0.08 * kick)).min(radius * 0.995);
        let shock_w = (radius * 0.02 * kick).max(2.0);
        let trail_r = rr.max(shock - shock_w * 1.5);
        canvas.circle_stroke(
            center.0 * ppp,
            center.1 * ppp,
            trail_r * ppp,
            (shock_w * 3.0) * ppp,
            Rgba::new(ring.r, ring.g, ring.b, (8.0 + 22.0 * kick) as u8),
        );
        let visible_w = shock_w.min(((radius - shock) * 1.7).max(1.0));
        canvas.circle_stroke(
            center.0 * ppp,
            center.1 * ppp,
            shock * ppp,
            visible_w * ppp,
            Rgba::new(hi.r, hi.g, hi.b, (18.0 + 55.0 * kick) as u8),
        );
    }

    let halo_w = (radius * (0.055 + 0.10 * kick + 0.02 * bass)).max(6.0);
    let halo_a = (16.0 + 28.0 * bass + 64.0 * kick) as u8;
    let halo_outer = (rr + halo_w * 0.58).min(radius * 0.995);
    let halo_inner = (rr - halo_w * 0.72).max(0.0);
    canvas.radial_gradient(
        center.0 * ppp,
        center.1 * ppp,
        halo_outer * ppp,
        &[
            (0.0, Rgba::TRANSPARENT),
            ((halo_inner / halo_outer).clamp(0.0, 1.0), Rgba::TRANSPARENT),
            (
                (rr / halo_outer).clamp(0.0, 0.998),
                Rgba::new(ring.r, ring.g, ring.b, halo_a.min(160)),
            ),
            (1.0, Rgba::TRANSPARENT),
        ],
    );

    let main_w = (radius * (0.028 + 0.035 * kick + 0.01 * bass)).max(2.4);
    let main_a = (108.0 + 42.0 * energy + 70.0 * kick).min(220.0) as u8;
    let energy_w = (main_w * 1.65).max(3.8);
    let energy_a = (26.0 + 24.0 * energy + 48.0 * kick + 12.0 * bass).min(130.0) as u8;
    canvas.circle_stroke(
        center.0 * ppp,
        center.1 * ppp,
        rr * ppp,
        energy_w * ppp,
        Rgba::new(deep.r, deep.g, deep.b, energy_a),
    );

    let segments = 96;
    for i in 0..segments {
        let t0 = i as f32 / segments as f32;
        let t1 = (i + 1) as f32 / segments as f32;
        let color = conical_color(t0, ring, hi, deep, main_a);
        let a0 = t0 * TAU;
        let a1 = t1 * TAU;
        canvas.line(
            (center.0 + a0.cos() * rr) * ppp,
            (center.1 + a0.sin() * rr) * ppp,
            (center.0 + a1.cos() * rr) * ppp,
            (center.1 + a1.sin() * rr) * ppp,
            main_w * ppp,
            color,
        );
    }

    canvas.circle_stroke(
        center.0 * ppp,
        center.1 * ppp,
        (rr * 0.985) * ppp,
        ((radius * (0.012 + 0.02 * kick)).max(1.2)) * ppp,
        Rgba::new(hi.r, hi.g, hi.b, (58.0 + 92.0 * kick) as u8),
    );
}

fn conical_color(t: f32, ring: Rgba, hi: Rgba, deep: Rgba, main_a: u8) -> Rgba {
    let stops = [
        (0.00, Rgba::new(ring.r, ring.g, ring.b, main_a)),
        (
            0.18,
            Rgba::new(hi.r, hi.g, hi.b, (main_a as u16 + 12).min(255) as u8),
        ),
        (0.42, Rgba::new(ring.r, ring.g, ring.b, main_a)),
        (
            0.70,
            Rgba::new(deep.r, deep.g, deep.b, main_a.saturating_sub(24).max(80)),
        ),
        (1.00, Rgba::new(ring.r, ring.g, ring.b, main_a)),
    ];
    let t = t.rem_euclid(1.0);
    for pair in stops.windows(2) {
        if t >= pair[0].0 && t <= pair[1].0 {
            let span = (pair[1].0 - pair[0].0).max(1e-6);
            return mix(pair[0].1, pair[1].1, (t - pair[0].0) / span);
        }
    }
    stops[0].1
}

// TUI-style radial LED matrix: discrete cells along each spoke, four grey
// levels, no glow. Flat-ended rectangles preserve the egui quad geometry.
#[allow(clippy::too_many_arguments)]
fn paint_radial_led(
    canvas: &mut Canvas,
    center: (f32, f32),
    ppp: f32,
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

    for i in 0..NUM_BARS {
        let bar_value = bars.get(i).copied().unwrap_or(0.0);
        let peak_value = peaks.get(i).copied().unwrap_or(0.0);
        let norm = (bar_value / MAX_DB_VALUE).clamp(0.0, 1.0);
        let peak_norm = (peak_value / MAX_DB_VALUE).clamp(0.0, 1.0);
        if norm <= 0.01 && peak_norm <= 0.01 {
            continue;
        }
        let lit = if norm <= 0.01 {
            0
        } else {
            ((norm * steps as f32).ceil() as usize).clamp(1, steps)
        };
        let angle =
            std::f32::consts::FRAC_PI_2 + i as f32 / NUM_BARS as f32 * TAU + rotation;
        let dir = (angle.cos(), angle.sin());
        let radial = |r: f32| ((center.0 + dir.0 * r) * ppp, (center.1 + dir.1 * r) * ppp);
        for cell in 0..lit {
            let r0 = inner_r + cell as f32 * cell_step;
            let r1 = (r0 + cell_len).min(inner_r + max_amp);
            let t = ((cell + 1) as f32 / steps as f32).clamp(0.0, 1.0);
            let level = (t * 3.0).round() / 3.0;
            canvas.radial_cell(radial((r0 + r1) * 0.5), dir, (r1 - r0) * ppp, cell_w * ppp, spectrum_color(level));
        }
        if show_peaks && peak_value > bar_value + PEAK_MARKER_GAP_DB {
            let half = cell_len * 0.5;
            let outer = inner_r + max_amp;
            let low = inner_r + half;
            let high = (outer - half).max(low);
            let peak_r = (inner_r + peak_norm * max_amp).clamp(low, high);
            let r0 = peak_r - half;
            let r1 = (peak_r + half).min(outer);
            canvas.radial_cell(radial((r0 + r1) * 0.5), dir, (r1 - r0) * ppp, cell_w * ppp, spectrum_color(1.0));
        }
    }
}

/// DOT MATRIX radial spectrum as a filled wave on the background dot lattice.
/// Every lattice dot inside the ring samples the spectrum at its own angle
/// (smoothed, cosine-interpolated between bars) and lights up when it lies
/// under the wave, so the ring has no gaps between spokes and its dots sit
/// exactly on the same grid as the background stream and the core disc.
/// Brightness follows each band's energy, both edges fade with sub-dot
/// coverage, and the inner edge breathes on the kick. Peak-hold markers form
/// a dotted contour above the wave (off by default in pixel mode; see
/// `State::peak_hold_dots`).
#[allow(clippy::too_many_arguments)]
fn paint_radial_dots(
    canvas: &mut Canvas,
    ppp: f32,
    center: (f32, f32),
    inner_r: f32,
    max_amp: f32,
    bars: &[f32],
    peaks: &[f32],
    show_peaks: bool,
    rotation: f32,
    lattice: DotLattice,
    kick: f32,
) {
    // The inner edge breathes inward on each kick, in step with the core pulse.
    const BREATH_DOTS: f32 = 3.0;
    let DotLattice { step, dot, origin } = lattice;
    let wave = smoothed_ring(bars);
    let peak_wave = smoothed_ring(peaks);
    let quiet = |ring: &[f32; NUM_BARS]| ring.iter().all(|value| *value <= 0.01);
    if quiet(&wave) && !(show_peaks && !quiet(&peak_wave)) {
        return;
    }
    let outer = inner_r + max_amp;
    let base_r = inner_r - kick.clamp(0.0, 1.0) * BREATH_DOTS * step;
    let low = (base_r - step).max(0.0);
    let (inner2, outer2) = (low * low, (outer + step) * (outer + step));
    let gap = PEAK_MARKER_GAP_DB / MAX_DB_VALUE;
    let n = (outer / step).ceil() as i32 + 2;
    let base_i = ((center.0 - origin.0) / step).floor() as i32;
    let base_j = ((origin.1 - center.1) / step).floor() as i32;
    // How much of a dot (one lattice step wide) lies inside an edge `gap` away:
    // edges move with sub-dot precision by fading dots instead of popping them.
    let coverage = |gap: f32| (gap / step + 0.5).clamp(0.0, 1.0);
    for iy in -n..=n {
        let y = origin.1 - (base_j + iy) as f32 * step;
        let dy = y - center.1;
        for ix in -n..=n {
            let x = origin.0 + (base_i + ix) as f32 * step;
            let dx = x - center.0;
            let d2 = dx * dx + dy * dy;
            if d2 < inner2 || d2 > outer2 {
                continue;
            }
            let dist = d2.sqrt();
            let index = ((dy.atan2(dx) - std::f32::consts::FRAC_PI_2 - rotation) / TAU
                * NUM_BARS as f32)
                .rem_euclid(NUM_BARS as f32);
            let level = sample_ring(&wave, index);
            // Any audible band keeps at least one dot so the base ring stays closed.
            let reach = (inner_r + (level * max_amp).max(step)).min(outer);
            let cover = if level > 0.01 {
                coverage(reach - dist).min(coverage(dist - base_r))
            } else {
                0.0
            };
            if cover > 0.02 {
                // Brightness follows this band's energy as well as the radius, so
                // the inner dots rise and fall with the music instead of sitting
                // at a fixed dim shade.
                let t = ((dist - inner_r) / max_amp).clamp(0.0, 1.0);
                let shade = level.powf(0.7) * (0.45 + 0.55 * t);
                let alpha_v = (cover * 255.0).round() as u8;
                canvas.pixel_square(
                    x * ppp,
                    y * ppp,
                    dot * ppp,
                    with_alpha(spectrum_color(shade), alpha_v),
                );
                if cover >= 0.98 {
                    continue;
                }
            }
            if show_peaks {
                let peak = sample_ring(&peak_wave, index);
                let peak_r = (inner_r + peak * max_amp).min(outer);
                if peak > level + gap && (dist - peak_r).abs() <= step * 0.5 {
                    canvas.pixel_square(x * ppp, y * ppp, dot * ppp, spectrum_color(1.0));
                }
            }
        }
    }
}

/// Dot pitch, dot size and one lattice dot centre, shared by every painter
/// that must land on the background dot grid.
#[derive(Clone, Copy)]
struct DotLattice {
    step: f32,
    dot: f32,
    origin: (f32, f32),
}

/// Normalised bar heights around the ring with a circular [1, 2, 1] / 4 blur,
/// so the interpolated wave has no single-bar spikes.
fn smoothed_ring(values: &[f32]) -> [f32; NUM_BARS] {
    let norm = |i: usize| (values.get(i).copied().unwrap_or(0.0) / MAX_DB_VALUE).clamp(0.0, 1.0);
    let mut out = [0.0; NUM_BARS];
    for (i, slot) in out.iter_mut().enumerate() {
        let prev = norm((i + NUM_BARS - 1) % NUM_BARS);
        let next = norm((i + 1) % NUM_BARS);
        *slot = (prev + 2.0 * norm(i) + next) * 0.25;
    }
    out
}

/// Cosine interpolation between neighbouring bars, wrapping at the seam.
fn sample_ring(ring: &[f32; NUM_BARS], index: f32) -> f32 {
    let floor = index.floor();
    let i0 = (floor as usize) % NUM_BARS;
    let i1 = (i0 + 1) % NUM_BARS;
    let w = (1.0 - ((index - floor) * std::f32::consts::PI).cos()) * 0.5;
    ring[i0] * (1.0 - w) + ring[i1] * w
}

/// Dot-matrix background stream (the braille variant, drawn as real dots so
/// the CPU rasterizer needs no glyph atlas).
fn paint_dot_stream(
    canvas: &mut Canvas,
    rect: (f32, f32, f32, f32),
    ppp: f32,
    grid: &Grid,
    bars: &[f32],
) {
    let (step, dot) = dot_metrics_from_grid(grid, ppp);
    let cols = grid.cols * DOTS_PER_CELL;
    let rows = grid.signal_rows * DOTS_PER_CELL;
    let center = (rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5);
    let core_r = core_radius((rect.2.min(rect.3) * 0.5 - 8.0).max(1.0));
    for col in 0..cols {
        let norm = mapped_bar(bars, cols, col);
        if norm <= 0.01 {
            continue;
        }
        let lit = ((norm * rows as f32).ceil() as usize).clamp(1, rows);
        for row in 0..lit {
            let x = grid.left + (col as f32 + 0.5) * step;
            let y = grid.bottom - (row as f32 + 0.5) * step;
            let dist = ((x - center.0) * (x - center.0) + (y - center.1) * (y - center.1)).sqrt();
            if dist <= core_r {
                continue;
            }
            let t = (row as f32 + 1.0) / rows as f32;
            let grey = (92.0 + 148.0 * t).round() as u8;
            let alpha_v = (42.0 + 104.0 * t).round() as u8;
            canvas.pixel_square(
                x * ppp,
                y * ppp,
                dot * ppp,
                ink_alpha(grey, alpha_v),
            );
        }
    }
}

/// Dotted core dial: a dot circle with beat brightness plus an expanding
/// dotted ring on heavy beats.
#[allow(clippy::too_many_arguments)]
fn paint_core_dots(
    canvas: &mut Canvas,
    center: (f32, f32),
    origin: (f32, f32),
    disc_r: f32,
    playing: bool,
    kick: f32,
    beat_age: f32,
    ring_strength: f32,
    step: f32,
    dot: f32,
    ppp: f32,
) {
    const PULSE_LIFE: f32 = 0.42;

    let t = (beat_age / PULSE_LIFE).clamp(0.0, 1.0);
    let ease = 1.0 - (1.0 - t) * (1.0 - t);
    let ring_on = playing && ring_strength > 0.0 && t < 1.0;
    let ring_r = disc_r + step * 1.2 + step * 6.8 * ease;
    let ring_half = step * 0.95;
    let ring_alpha = (((1.0 - t) * (1.0 - t)) * (120.0 + 100.0 * ring_strength))
        .round()
        .clamp(0.0, 230.0) as u8;

    let circle_level = if playing { (0.28 + 1.15 * kick).min(1.0) } else { 0.28 };
    let circle_alpha = (circle_level * 180.0).round() as u8;

    let reach = (disc_r + step * 2.0).max(ring_r + ring_half) + step;
    let n = (reach / step).ceil() as i32 + 1;
    let base_i = ((center.0 - origin.0) / step).floor() as i32;
    let base_j = ((origin.1 - center.1) / step).floor() as i32;
    for iy in -n..=n {
        let y = origin.1 - (base_j + iy) as f32 * step;
        for ix in -n..=n {
            let x = origin.0 + (base_i + ix) as f32 * step;
            let dist = ((x - center.0) * (x - center.0) + (y - center.1) * (y - center.1)).sqrt();
            let in_circle = dist <= disc_r;
            let in_ring = ring_on && (dist - ring_r).abs() <= ring_half;
            if !in_circle && !in_ring {
                continue;
            }
            let color = if in_circle {
                let edge = (dist / disc_r).clamp(0.0, 1.0);
                let alpha = (circle_alpha as f32 * (0.55 + 0.45 * edge)).round() as u8;
                ink_alpha(236, alpha)
            } else {
                ink_alpha(255, ring_alpha)
            };
            canvas.pixel_square(
                x * ppp,
                y * ppp,
                dot * ppp,
                color,
            );
        }
    }
}

fn dot_metrics_from_grid(grid: &Grid, ppp: f32) -> (f32, f32) {
    let step = grid.cell / DOTS_PER_CELL as f32;
    let step_px = (step * ppp).round().max(1.0);
    let dot_px = (step_px * 0.58).round().clamp(1.0, step_px - 1.0);
    (step_px / ppp, dot_px / ppp)
}

fn dot_metrics_for_rect(rect: (f32, f32, f32, f32), ppp: f32) -> (f32, f32) {
    if let Some(grid) = pixel_grid(rect, ppp) {
        return dot_metrics_from_grid(&grid, ppp);
    }
    let step_px = (cell_dev(ppp) / DOTS_PER_CELL) as f32;
    let dot_px = (step_px * 0.58).round().clamp(1.0, step_px - 1.0);
    (step_px / ppp, dot_px / ppp)
}

// ---------- background LED wall (viz.rs paint_bar_spectrum) ----------

struct Grid {
    left: f32,
    top: f32,
    bottom: f32,
    cols: usize,
    rows: usize,
    cell: f32,
    signal_rows: usize,
}

fn cell_dev(ppp: f32) -> usize {
    let mut cell = (LED_CELL * ppp.max(1.0)).round().max(4.0) as usize;
    let rem = cell % (DOTS_PER_CELL * 2);
    if rem != 0 {
        cell += DOTS_PER_CELL * 2 - rem;
    }
    cell
}

fn pixel_grid(rect: (f32, f32, f32, f32), ppp: f32) -> Option<Grid> {
    let (rx, ry, rw, rh) = rect;
    let ppp = ppp.max(1.0);
    let cell_px = cell_dev(ppp);
    // The lattice sits one grid cell away from each edge of the stage box.
    let margin = cell_px as f32 / ppp;
    let max_w = (rw - margin * 2.0).max(0.0);
    let max_h = (rh - margin * 2.0).max(0.0);
    let cols = ((max_w * ppp) as usize) / cell_px;
    let rows = ((max_h * ppp) as usize) / cell_px;
    if cols < 8 || rows < 4 {
        return None;
    }
    let cell = cell_px as f32 / ppp;
    let panel_w = cols as f32 * cell;
    let panel_h = rows as f32 * cell;
    let panel_h_dev = (rows * cell_px) as i32;
    let mut left_dev = ((rx + margin + (max_w - panel_w) * 0.5) * ppp).round() as i32;
    // Centre the panel vertically between the margins; the leftover fraction
    // of one cell splits evenly between top and bottom.
    let box_top_dev = ((ry + margin) * ppp).round() as i32;
    let box_bottom_dev = ((ry + rh - margin) * ppp).round() as i32;
    let mut bottom_dev = box_bottom_dev - ((box_bottom_dev - box_top_dev - panel_h_dev) / 2);
    // Shift the lattice so the stage centre becomes a symmetry point.
    let symmetry = (cell_px / 4).max(1) as i32;
    let center_x = ((rx + rw * 0.5) * ppp).round() as i32;
    let center_y = ((ry + rh * 0.5) * ppp).round() as i32;
    let rem_x = (center_x - left_dev).rem_euclid(symmetry);
    let dx = if rem_x <= symmetry / 2 { rem_x } else { rem_x - symmetry };
    left_dev += dx;
    let rem_y = (center_y - bottom_dev).rem_euclid(symmetry);
    let dy = if rem_y <= symmetry / 2 { rem_y } else { rem_y - symmetry };
    bottom_dev += dy;
    let panel_left = left_dev as f32 / ppp;
    let panel_bottom = bottom_dev as f32 / ppp;
    let signal_rows = (((panel_h * 0.40) / cell).round() as usize).clamp(1, rows);
    Some(Grid {
        left: panel_left,
        top: panel_bottom - panel_h,
        bottom: panel_bottom,
        cols,
        rows,
        cell,
        signal_rows,
    })
}

/// The faint matrix stays visible in silence; it is baked into the cached
/// background layer together with the gradient.
fn painted_grid(canvas: &mut Canvas, grid: &Grid, ppp: f32, light: bool) {
    let grid_color = ink_alpha(210, 8);
    for i in 1..grid.cols {
        let x = (grid.left + i as f32 * grid.cell) * ppp;
        canvas.line(x, grid.top * ppp, x, grid.bottom * ppp, ppp, grid_color);
    }
    for j in 1..grid.rows {
        let y = (grid.bottom - j as f32 * grid.cell) * ppp;
        canvas.line(
            grid.left * ppp,
            y,
            (grid.left + grid.cols as f32 * grid.cell) * ppp,
            y,
            ppp,
            grid_color,
        );
    }
    canvas.line(
        grid.left * ppp,
        grid.bottom * ppp,
        (grid.left + grid.cols as f32 * grid.cell) * ppp,
        grid.bottom * ppp,
        ppp,
        ink_alpha(210, 18),
    );

    let lattice = if light {
        Rgba::new(255, 255, 255, 18)
    } else {
        Rgba::new(0, 0, 0, 14)
    };
    for i in 1..grid.cols {
        let x = (grid.left + i as f32 * grid.cell) * ppp;
        canvas.line(x, grid.top * ppp, x, grid.bottom * ppp, ppp, lattice);
    }
    for j in 1..grid.rows {
        let y = (grid.bottom - j as f32 * grid.cell) * ppp;
        canvas.line(
            grid.left * ppp,
            y,
            (grid.left + grid.cols as f32 * grid.cell) * ppp,
            y,
            ppp,
            lattice,
        );
    }
}

/// Use the same lattice as the lit dots, without a second 12px grid that
/// visually splits the matrix into groups of four.
fn painted_dot_grid(canvas: &mut Canvas, grid: &Grid, ppp: f32) {
    let (step, dot) = dot_metrics_from_grid(grid, ppp);
    for col in 0..grid.cols * DOTS_PER_CELL {
        let x = grid.left + (col as f32 + 0.5) * step;
        for row in 0..grid.rows * DOTS_PER_CELL {
            let y = grid.bottom - (row as f32 + 0.5) * step;
            canvas.pixel_square(x * ppp, y * ppp, dot * ppp, ink_alpha(210, 8));
        }
    }
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

/// Lit LED cells only; the lattice itself lives in the cached background.
fn paint_led_cells(canvas: &mut Canvas, grid: &Grid, ppp: f32, bars: &[f32]) {
    if !bars.iter().any(|bar| *bar > 0.01) {
        return;
    }
    for col in 0..grid.cols {
        let norm = mapped_bar(bars, grid.cols, col);
        if norm <= 0.01 {
            continue;
        }
        let lit = ((norm * grid.signal_rows as f32).ceil() as usize)
            .clamp(1, grid.signal_rows);
        let x0 = grid.left + col as f32 * grid.cell;
        for row in 0..lit {
            let t = (row as f32 + 1.0) / grid.signal_rows as f32;
            let grey = (56.0 + 168.0 * t.powf(0.85)).round() as u8;
            let alpha_v = (34.0 + 66.0 * t).round() as u8;
            let y0 = grid.bottom - (row + 1) as f32 * grid.cell;
            canvas.rect(
                x0 * ppp,
                y0 * ppp,
                grid.cell * ppp,
                grid.cell * ppp,
                ink_alpha(grey, alpha_v),
            );
        }
    }
}

// ---------- public entry ----------

#[derive(PartialEq)]
struct BackgroundKey {
    size: (u32, u32),
    scale: u32,
    corners: [Rgb; 4],
    dark: bool,
    rect: [u32; 4],
    dot_grid: bool,
    show_grid: bool,
}

pub struct Stage {
    bg: Vec<u8>,
    bg_key: Option<BackgroundKey>,
    grid: Option<Grid>,
    /// Double buffer: alternate between two textures so the frame handed to
    /// the renderer is never the one being repainted next tick.
    scratch: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
    last_rendered: Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>>,
}

pub struct StageInput<'a> {
    pub bars: &'a [f32],
    pub peaks: &'a [f32],
    pub visual: VisualState,
    pub playing: bool,
    pub pulse_enabled: bool,
    pub show_bg_spectrum: bool,
    pub show_radial: bool,
    pub peak_hold: bool,
    pub rotation: f32,
    pub radial_scale: f32,
    pub led_bars: bool,
    pub dot_style: bool,
    pub braille_stream: bool,
    pub beat_age: f32,
    pub ring_strength: f32,
}

pub struct StageOutput {
    pub buffer: slint::SharedPixelBuffer<slint::Rgba8Pixel>,
    pub disc_r: f32,
}

impl Stage {
    pub fn new() -> Self {
        Self {
            bg: Vec::new(),
            bg_key: None,
            grid: None,
            scratch: None,
            last_rendered: None,
        }
    }

    pub fn paint(
        &mut self,
        width: u32,
        height: u32,
        ppp: f32,
        look: &Look,
        stage_rect: (f32, f32, f32, f32),
        input: StageInput,
    ) -> StageOutput {
        let corners = look.gradient.unwrap_or([look.palette.bg; 4]);
        // The grid is baked into the cached background, so the cache key must
        // cover the stage rect too: collapsing the sidebar moves the lattice
        // even though the window size and theme stay the same.
        let key = BackgroundKey {
            size: (width, height),
            scale: ppp.to_bits(),
            corners,
            dark: look.palette.dark,
            rect: [
                stage_rect.0.to_bits(),
                stage_rect.1.to_bits(),
                stage_rect.2.to_bits(),
                stage_rect.3.to_bits(),
            ],
            dot_grid: input.braille_stream,
            show_grid: input.show_bg_spectrum,
        };
        if self.bg_key.as_ref() != Some(&key) {
            let mut bg = vec![0_u8; (width * height * 4) as usize];
            {
                let mut canvas = Canvas::new(&mut bg, width, height);
                canvas.fill_gradient4(corners);
                self.grid = pixel_grid(stage_rect, ppp);
                if let Some(grid) = &self.grid {
                    if input.show_bg_spectrum {
                        if input.braille_stream {
                            painted_dot_grid(&mut canvas, grid, ppp);
                        } else {
                            painted_grid(&mut canvas, grid, ppp, !look.palette.dark);
                        }
                    }
                }
            }
            self.bg = bg;
            self.bg_key = Some(key);
        }

        let mut buffer = self
            .scratch
            .take()
            .unwrap_or_else(|| slint::SharedPixelBuffer::new(width, height));
        if buffer.width() as u32 != width || buffer.height() as u32 != height {
            buffer = slint::SharedPixelBuffer::new(width, height);
        }
        let disc_r = {
            let mut canvas = Canvas::new(buffer.make_mut_bytes(), width, height);
            canvas.copy_from(&self.bg);

            let rect = stage_rect;
            let center = (rect.0 + rect.2 * 0.5, rect.1 + rect.3 * 0.5);
            // One dot lattice for the background stream, radial wave and core.
            let lattice = if let Some(grid) = &self.grid {
                let (step, dot) = dot_metrics_from_grid(grid, ppp);
                DotLattice { step, dot, origin: (grid.left + step * 0.5, grid.bottom - step * 0.5) }
            } else {
                let (step, dot) = dot_metrics_for_rect(rect, ppp);
                DotLattice { step, dot, origin: center }
            };
            if input.show_bg_spectrum {
                if let Some(grid) = &self.grid {
                    if input.braille_stream {
                        paint_dot_stream(&mut canvas, rect, ppp, grid, input.bars);
                    } else {
                        paint_led_cells(&mut canvas, grid, ppp, input.bars);
                    }
                }
            }
            if input.show_radial {
                paint_spectrum(
                    &mut canvas,
                    rect,
                    ppp,
                    input.bars,
                    input.peaks,
                    input.peak_hold,
                    input.rotation,
                    input.radial_scale,
                    input.led_bars,
                    input.dot_style,
                    lattice,
                    if input.pulse_enabled { input.visual.kick } else { 0.0 },
                );
            }
            let usable = (rect.2.min(rect.3) * 0.5 - 8.0).max(1.0);
            let base_disc_r = core_radius(usable);
            if input.dot_style {
                let kick = if input.pulse_enabled { input.visual.kick } else { 0.0 };
                let DotLattice { step, dot, origin } = lattice;
                let age = if input.pulse_enabled { input.beat_age } else { 1.0 };
                let strength = if input.pulse_enabled { input.ring_strength } else { 0.0 };
                paint_core_dots(
                    &mut canvas,
                    center,
                    origin,
                    base_disc_r,
                    input.playing,
                    kick,
                    age,
                    strength,
                    step,
                    dot,
                    ppp,
                );
            } else {
                paint_core_classic(
                    &mut canvas,
                    center,
                    input.visual,
                    input.playing,
                    input.pulse_enabled,
                    base_disc_r,
                    ppp,
                );
            }
            base_disc_r
        };
        // The buffer shown last frame becomes the next scratch surface.
        if let Some(prev) = self.last_rendered.take() {
            if prev.width() as u32 == width && prev.height() as u32 == height {
                self.scratch = Some(prev);
            }
        }
        self.last_rendered = Some(buffer.clone());
        StageOutput { buffer, disc_r }
    }
}
