// Minimal CPU rasterizer for the stage texture (slint-ui branch). The Slint
// software renderer cannot rotate plain rectangles, so the radial spectrum,
// core dial and LED wall are painted here with the same algorithms viz.rs
// used through egui, and handed to the window as an opaque image.
//
// The canvas is always fully opaque (alpha = 255): the theme gradient is
// copied in first, then everything blends over it, so the window compositor
// treats the stage as a plain memcpy-able layer.
//
// Performance notes: every primitive resolves per-scanline spans instead of
// scanning full bounding boxes, and radial gradients are evaluated per pixel
// (the egui build approximated them with stacks of circle strokes, which is
// far too expensive on the CPU).

#[derive(Clone, Copy, Debug)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const TRANSPARENT: Rgba = Rgba { r: 0, g: 0, b: 0, a: 0 };

    pub fn new(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }

    pub fn from_rgb(rgb: crate::theme::Rgb) -> Self {
        Self { r: rgb[0], g: rgb[1], b: rgb[2], a: 255 }
    }

    /// Blend toward `other` by `t` in [0, 1], like egui's theme::mix.
    pub fn mix(self, other: Rgba, t: f32) -> Rgba {
        let t = t.clamp(0.0, 1.0);
        let lerp = |x: u8, y: u8| {
            (x as f32 + (y as f32 - x as f32) * t).round().clamp(0.0, 255.0) as u8
        };
        Rgba {
            r: lerp(self.r, other.r),
            g: lerp(self.g, other.g),
            b: lerp(self.b, other.b),
            a: lerp(self.a, other.a),
        }
    }
}

/// Piecewise-linear colour ramp, shared by the stage gradients.
pub fn gradient_color(stops: &[(f32, Rgba)], t: f32) -> Rgba {
    let t = t.clamp(0.0, 1.0);
    for pair in stops.windows(2) {
        if t >= pair[0].0 && t <= pair[1].0 {
            let span = (pair[1].0 - pair[0].0).max(1e-6);
            return pair[0].1.mix(pair[1].1, (t - pair[0].0) / span);
        }
    }
    if t < stops[0].0 {
        stops[0].1
    } else {
        stops[stops.len() - 1].1
    }
}

pub struct Canvas<'a> {
    buf: &'a mut [u8],
    pub width: usize,
    pub height: usize,
}

impl<'a> Canvas<'a> {
    pub fn new(buf: &'a mut [u8], width: u32, height: u32) -> Self {
        Self { buf, width: width as usize, height: height as usize }
    }

    #[inline]
    fn blend_span(&mut self, y: usize, x0: usize, x1: usize, cov: impl Fn(f32) -> f32, color: Rgba) {
        let src_a_base = color.a as f32 / 255.0;
        if src_a_base <= 0.0 {
            return;
        }
        let row = y * self.width;
        for x in x0..x1.min(self.width) {
            let c = cov(x as f32 + 0.5).clamp(0.0, 1.0);
            if c <= 0.0 {
                continue;
            }
            let src_a = src_a_base * c;
            let index = (row + x) * 4;
            let dst = &mut self.buf[index..index + 4];
            let inv = 1.0 - src_a;
            dst[0] = (color.r as f32 * src_a + dst[0] as f32 * inv).round() as u8;
            dst[1] = (color.g as f32 * src_a + dst[1] as f32 * inv).round() as u8;
            dst[2] = (color.b as f32 * src_a + dst[2] as f32 * inv).round() as u8;
            dst[3] = 255;
        }
    }

    /// Filled circle with a soft (feathered) edge.
    pub fn circle_filled(&mut self, cx: f32, cy: f32, r: f32, color: Rgba) {
        if color.a == 0 || r <= 0.0 {
            return;
        }
        let min_y = (cy - r - 1.0).floor().max(0.0) as usize;
        let max_y = (cy + r + 1.0).ceil().min(self.height as f32) as usize;
        for y in min_y..max_y {
            let dy = y as f32 + 0.5 - cy;
            let span = (r * r - dy * dy).sqrt().max(0.0);
            let x0 = ((cx - span - 1.0).floor().max(0.0)) as usize;
            let x1 = ((cx + span + 1.0).ceil().min(self.width as f32)) as usize;
            self.blend_span(y, x0, x1, |px| r - ((px - cx) * (px - cx) + dy * dy).sqrt() + 0.5, color);
        }
    }

    /// Stroked circle ring centred on (cx, cy).
    pub fn circle_stroke(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: Rgba) {
        if color.a == 0 || r <= 0.0 || width <= 0.0 {
            return;
        }
        let half = width * 0.5;
        let outer = r + half;
        let inner = (r - half).max(0.0);
        let min_y = (cy - outer - 1.0).floor().max(0.0) as usize;
        let max_y = (cy + outer + 1.0).ceil().min(self.height as f32) as usize;
        for y in min_y..max_y {
            let dy = y as f32 + 0.5 - cy;
            let outer_span = (outer * outer - dy * dy).sqrt().max(0.0);
            let inner_span = if dy < inner {
                (inner * inner - dy * dy).sqrt()
            } else {
                0.0
            };
            let x_lo = ((cx - outer_span - 1.0).floor().max(0.0)) as usize;
            let x_hi = ((cx + outer_span + 1.0).ceil().min(self.width as f32)) as usize;
            // Coverage: distance from the ideal radius, feathered 1 px.
            self.blend_span(y, x_lo, x_hi, |px| {
                let d = ((px - cx) * (px - cx) + dy * dy).sqrt();
                half - (d - r).abs() + 0.5
            }, color);
            // Skip the interior for efficiency: pixels with d < inner cannot
            // be covered, but blend_span's per-pixel check already rejects
            // them (cov <= 0), so no extra work is needed here.
            let _ = inner_span;
        }
    }

    /// Thick line segment with round caps, scanning only the per-row spans
    /// where the capsule actually covers pixels.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, width: f32, color: Rgba) {
        if color.a == 0 || width <= 0.0 {
            return;
        }
        let half = width * 0.5;
        let dx = x1 - x0;
        let dy = y1 - y0;
        let len_sq = dx * dx + dy * dy;
        let len = len_sq.sqrt().max(1e-6);
        let min_y = (y0.min(y1) - half - 1.0).floor().max(0.0) as usize;
        let max_y = (y0.max(y1) + half + 1.0).ceil().min(self.height as f32) as usize;
        for y in min_y..max_y {
            let py = y as f32 + 0.5 - y0;
            // The capsule's cross-section on this row is a single interval.
            // Collect boundary candidates from the two end caps and the slab.
            let mut lo = f32::INFINITY;
            let mut hi = f32::NEG_INFINITY;
            let add = |a: f32, b: f32, lo: &mut f32, hi: &mut f32| {
                *lo = lo.min(a);
                *hi = hi.max(b);
            };
            // Cap at A.
            if py.abs() <= half {
                let s = (half * half - py * py).sqrt();
                add(x0 - s, x0 + s, &mut lo, &mut hi);
            }
            // Cap at B.
            let py_b = py - dy;
            if py_b.abs() <= half {
                let s = (half * half - py_b * py_b).sqrt();
                add(x1 - s, x1 + s, &mut lo, &mut hi);
            }
            // Slab: perpendicular distance to the segment line within `half`,
            // with the projection parameter inside [0, 1].
            if len_sq > 1e-6 {
                if dy.abs() > 1e-6 {
                    let q_lo = (py * dx - half * len) / dy;
                    let q_hi = (py * dx + half * len) / dy;
                    let (q_lo, q_hi) = if dy > 0.0 { (q_lo, q_hi) } else { (q_hi, q_lo) };
                    // t bounds: 0 <= (qx*dx + py*dy) <= len_sq
                    if dx.abs() > 1e-6 {
                        let mut t0 = (-py * dy) / dx;
                        let mut t1 = (len_sq - py * dy) / dx;
                        if dx < 0.0 {
                            std::mem::swap(&mut t0, &mut t1);
                        }
                        let s_lo = q_lo.max(t0);
                        let s_hi = q_hi.min(t1);
                        if s_lo <= s_hi {
                            add(x0 + s_lo, x0 + s_hi, &mut lo, &mut hi);
                        }
                    } else {
                        // Vertical segment: t is fixed for the whole row.
                        let t = py * dy / len_sq;
                        if (0.0..=1.0).contains(&t) {
                            add(x0 + q_lo, x0 + q_hi, &mut lo, &mut hi);
                        }
                    }
                } else if py.abs() <= half {
                    // Horizontal segment: slab spans between the endpoints.
                    let (t0, t1) = if dx >= 0.0 { (0.0, len_sq / dx) } else { (len_sq / dx, 0.0) };
                    add(x0 + t0, x0 + t1, &mut lo, &mut hi);
                }
            }
            if !lo.is_finite() || !hi.is_finite() {
                continue;
            }
            let x_start = ((lo - 1.0).floor().max(0.0)) as usize;
            let x_end = ((hi + 1.0).ceil().min(self.width as f32)) as usize;
            if x_start >= x_end {
                continue;
            }
            self.blend_span(y, x_start, x_end, |px| {
                let qx = px - x0;
                let d = if len_sq <= 1e-6 {
                    (qx * qx + py * py).sqrt()
                } else {
                    let t = ((qx * dx + py * dy) / len_sq).clamp(0.0, 1.0);
                    ((qx - t * dx) * (qx - t * dx) + (py - t * dy) * (py - t * dy)).sqrt()
                };
                half - d + 0.5
            }, color);
        }
    }

    /// Filled axis-aligned rectangle (grid cells).
    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: Rgba) {
        if color.a == 0 {
            return;
        }
        let min_x = x.floor().max(0.0) as usize;
        let max_x = (x + w).ceil().min(self.width as f32) as usize;
        let min_y = y.floor().max(0.0) as usize;
        let max_y = (y + h).ceil().min(self.height as f32) as usize;
        let cov_row = |px: f32| {
            if px >= x && px <= x + w {
                1.0
            } else {
                0.0
            }
        };
        for py in min_y..max_y {
            self.blend_span(py, min_x, max_x, cov_row, color);
        }
    }

    /// Radial gradient disc: per-pixel distance ramp, `t = d / r`.
    /// Replaces the egui-era stacks of circle strokes with one pass.
    pub fn radial_gradient(&mut self, cx: f32, cy: f32, r: f32, stops: &[(f32, Rgba)]) {
        if r <= 0.0 {
            return;
        }
        let min_y = (cy - r - 1.0).floor().max(0.0) as usize;
        let max_y = (cy + r + 1.0).ceil().min(self.height as f32) as usize;
        for y in min_y..max_y {
            let dy = y as f32 + 0.5 - cy;
            let span = (r * r - dy * dy).sqrt().max(0.0);
            let x0 = ((cx - span - 1.0).floor().max(0.0)) as usize;
            let x1 = ((cx + span + 1.0).ceil().min(self.width as f32)) as usize;
            let row = y * self.width;
            let _ = row;
            for x in x0..x1.min(self.width) {
                let ddx = x as f32 + 0.5 - cx;
                let d = (ddx * ddx + dy * dy).sqrt();
                if d > r {
                    continue;
                }
                let color = gradient_color(stops, d / r);
                if color.a == 0 {
                    continue;
                }
                let src_a = color.a as f32 / 255.0;
                let index = (row + x) * 4;
                let dst = &mut self.buf[index..index + 4];
                let inv = 1.0 - src_a;
                dst[0] = (color.r as f32 * src_a + dst[0] as f32 * inv).round() as u8;
                dst[1] = (color.g as f32 * src_a + dst[1] as f32 * inv).round() as u8;
                dst[2] = (color.b as f32 * src_a + dst[2] as f32 * inv).round() as u8;
                dst[3] = 255;
            }
        }
    }

    /// Overwrite the whole canvas with `src` (the cached background layer).
    pub fn copy_from(&mut self, src: &[u8]) {
        debug_assert_eq!(src.len(), self.buf.len());
        self.buf.copy_from_slice(src);
    }

    /// Paint a bilinear four-corner gradient over the whole canvas.
    pub fn fill_gradient4(&mut self, corners: [crate::theme::Rgb; 4]) {
        let [tl, tr, br, bl] = corners;
        let (w, h) = (self.width, self.height);
        for y in 0..h {
            let v = y as f32 / (h.max(2) - 1) as f32;
            for x in 0..w {
                let u = x as f32 / (w.max(2) - 1) as f32;
                let index = (y * w + x) * 4;
                let dst = &mut self.buf[index..index + 4];
                for ch in 0..3 {
                    let top = tl[ch] as f32 + (tr[ch] as f32 - tl[ch] as f32) * u;
                    let bottom = bl[ch] as f32 + (br[ch] as f32 - bl[ch] as f32) * u;
                    dst[ch] = (top + (bottom - top) * v).round() as u8;
                }
                dst[3] = 255;
            }
        }
    }
}
