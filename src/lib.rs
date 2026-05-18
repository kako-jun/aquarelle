//! **aquarelle** — watercolor-style soft-bleed orb rendering on a
//! [`tiny_skia::Pixmap`].
//!
//! Originally written as the cel-anime night-scene texture set for the
//! [`orber`](https://crates.io/crates/orber) abstract-mood-image generator,
//! the engine is independent of orber and only depends on
//! `tiny-skia` + `palette` + `rand` + `rand_chacha`. It takes a
//! center, radius, RGB color, and a `u64` seed, and draws four
//! compositable elements onto a pixel buffer you already own.
//!
//! # The four elements
//!
//! 1. **bleed** — small same-color radial gradients scattered around
//!    the orb to fake a film grain / paper-bleed feel.
//! 2. **bloom** — a near-white core inside the inner ~30 % of the
//!    radius so the orb reads as a light source rather than a flat dot.
//! 3. **offset** — the gradient center is decoupled from the geometric
//!    center by up to 25 % of the radius. A perfectly concentric
//!    light source looks artificial; a slightly off-center one feels
//!    natural. The direction is seed-derived so calls are deterministic.
//! 4. **halo** — saturation of the outer falloff is boosted so the
//!    bleed reads as a film halation instead of a flat alpha fade.
//!
//! Each is tunable in `0.0..=1.0` and they compose with source-over.
//!
//! # Renderer-agnostic on purpose
//!
//! aquarelle does **not** know what background is on the pixmap, what
//! the rest of your scene looks like, or how you intend to encode the
//! result. It just composites four watercolor layers onto the buffer
//! you hand it. The caller decides the background fill, layout, and
//! output format (PNG / WebP / SVG / animation frames / WebCodecs).
//!
//! # Example
//!
//! ```
//! use aquarelle::{render_aquarelle_orb, AquarelleParams};
//! use tiny_skia::{Color, Pixmap};
//!
//! let mut pix = Pixmap::new(128, 128).unwrap();
//! pix.fill(Color::from_rgba8(0, 0, 0, 255));
//!
//! render_aquarelle_orb(
//!     &mut pix,
//!     (64.0, 64.0),       // center
//!     40.0,               // radius
//!     [200, 100, 50],     // sRGB color
//!     42,                 // seed (determinism)
//!     AquarelleParams::default(),
//! );
//! ```
//!
//! # Determinism
//!
//! Identical `(center, radius, color, seed, params)` produce
//! byte-identical pixels. RNG state is seeded per call via
//! `ChaCha8Rng::seed_from_u64(seed)` and never touches `thread_rng`.
//!
//! # Bleed pass (v0.2)
//!
//! For callers that already have a rasterized image (e.g. ink lines on a
//! white background from `blueprinter`) and want to bleed the existing
//! pixels themselves, [`render_aquarelle_bleed_pass`] applies a
//! Gaussian-approximating box blur (3 passes) to the whole pixmap with a
//! halo saturation boost and a faint seed-derived paper-grain noise. The
//! original picture is then re-composited on top, so the bleed reads as
//! a halo underneath the existing strokes.

use palette::{FromColor, Hsl, IntoColor, Srgb};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::f32::consts::TAU;
use tiny_skia::{
    Color, FillRule, GradientStop, Paint, PathBuilder, Pixmap, Point, RadialGradient, SpreadMode,
    Transform,
};

/// Intensities of the four aquarelle elements. Each value is interpreted
/// in `0.0..=1.0`; out-of-range inputs are clamped internally so the
/// caller can pass raw slider values without pre-validation.
#[derive(Debug, Clone, Copy)]
pub struct AquarelleParams {
    /// Number and strength of the small same-color satellite gradients
    /// scattered around the orb. `0.0` = none, `1.0` = three full
    /// satellites at maximum radius.
    pub bleed: f32,
    /// White-flare core in the inner ~30 % of the radius. `0.0` = the
    /// color stays at its source value, `1.0` = the core is mixed 70 %
    /// toward white.
    pub bloom: f32,
    /// How far the gradient center is offset from the geometric center.
    /// `0.0` = perfectly concentric, `1.0` = up to 25 % of the radius
    /// along a seed-derived angle.
    pub offset: f32,
    /// Saturation boost applied to the outer halo color. `0.0` = no
    /// boost (matches source), `1.0` = saturation × 1.6 (film
    /// halation feel).
    pub halo: f32,
}

impl Default for AquarelleParams {
    /// Mid-strength preset (every element at `0.5`). A calm cel-anime
    /// night-scene feel suitable as a starting point for callers that
    /// want one knob to tune later.
    fn default() -> Self {
        Self {
            bleed: 0.5,
            bloom: 0.5,
            offset: 0.5,
            halo: 0.5,
        }
    }
}

impl AquarelleParams {
    /// Return a copy with every field clamped to `0.0..=1.0`. Called
    /// internally by [`render_aquarelle_orb`] — exposed publicly so
    /// callers that want to mirror the clamp (e.g. a UI showing the
    /// effective values) can stay in sync without duplicating limits.
    pub fn clamped(self) -> Self {
        Self {
            bleed: self.bleed.clamp(0.0, 1.0),
            bloom: self.bloom.clamp(0.0, 1.0),
            offset: self.offset.clamp(0.0, 1.0),
            halo: self.halo.clamp(0.0, 1.0),
        }
    }
}

/// Composite one aquarelle orb onto `pixmap` with source-over blending.
///
/// `seed` drives the deterministic angle of the gradient offset and the
/// placement of bleed satellites. Identical `(center, radius, color,
/// seed, params)` produce byte-identical pixels. The background of
/// `pixmap` is the caller's responsibility (typically `pixmap.fill(...)`
/// before any orb).
pub fn render_aquarelle_orb(
    pixmap: &mut Pixmap,
    center: (f32, f32),
    radius: f32,
    color: [u8; 3],
    seed: u64,
    params: AquarelleParams,
) {
    if radius <= 0.0 {
        return;
    }
    let p = params.clamped();
    let mut rng = ChaCha8Rng::seed_from_u64(seed);

    // 1. offset: shift the gradient center by up to 25 % of the radius.
    let offset_dist = radius * 0.25 * p.offset;
    let theta: f32 = rng.gen_range(0.0..TAU);
    let cx = center.0 + offset_dist * theta.cos();
    let cy = center.1 + offset_dist * theta.sin();

    // 2. main radial gradient with halo-boosted outer saturation.
    let halo_color = boost_saturation(color, 1.0 + 0.6 * p.halo);
    draw_radial(
        pixmap,
        cx,
        cy,
        radius,
        color_with_alpha(color, 255),
        color_with_alpha(halo_color, 128),
        color_with_alpha(halo_color, 0),
        0.55,
    );

    // 3. bleed: scatter 0..3 small same-color gradients nearby.
    let bleed_count = (3.0 * p.bleed).round() as u32;
    for _ in 0..bleed_count {
        let bleed_theta: f32 = rng.gen_range(0.0..TAU);
        let bleed_dist = radius * rng.gen_range(0.4..0.9);
        let bx = center.0 + bleed_dist * bleed_theta.cos();
        let by = center.1 + bleed_dist * bleed_theta.sin();
        let bleed_radius = radius * rng.gen_range(0.2..0.4) * (0.5 + 0.5 * p.bleed);
        let bleed_color = boost_saturation(color, 1.0 + 0.4 * p.halo);
        draw_radial(
            pixmap,
            bx,
            by,
            bleed_radius,
            color_with_alpha(bleed_color, 100),
            color_with_alpha(bleed_color, 50),
            color_with_alpha(bleed_color, 0),
            0.5,
        );
    }

    // 4. bloom: near-white core in the inner ~30 % of the radius.
    if p.bloom > 0.0 {
        let core_radius = radius * 0.3 * p.bloom;
        if core_radius > 0.0 {
            let mix_amount = 0.7;
            let bloom_color = mix_with_white(color, mix_amount);
            draw_radial(
                pixmap,
                cx,
                cy,
                core_radius,
                color_with_alpha(bloom_color, 255),
                color_with_alpha(bloom_color, 128),
                color_with_alpha(bloom_color, 0),
                0.55,
            );
        }
    }
}

#[inline]
fn color_with_alpha(rgb: [u8; 3], a: u8) -> [u8; 4] {
    [rgb[0], rgb[1], rgb[2], a]
}

#[allow(clippy::too_many_arguments)]
fn draw_radial(
    pixmap: &mut Pixmap,
    cx: f32,
    cy: f32,
    radius: f32,
    inner_rgba: [u8; 4],
    mid_rgba: [u8; 4],
    edge_rgba: [u8; 4],
    mid_stop: f32,
) {
    let center_color =
        Color::from_rgba8(inner_rgba[0], inner_rgba[1], inner_rgba[2], inner_rgba[3]);
    let mid_color = Color::from_rgba8(mid_rgba[0], mid_rgba[1], mid_rgba[2], mid_rgba[3]);
    let edge_color = Color::from_rgba8(edge_rgba[0], edge_rgba[1], edge_rgba[2], edge_rgba[3]);
    let stops = vec![
        GradientStop::new(0.0, center_color),
        GradientStop::new(mid_stop.clamp(0.05, 0.95), mid_color),
        GradientStop::new(1.0, edge_color),
    ];
    let Some(shader) = RadialGradient::new(
        Point::from_xy(cx, cy),
        Point::from_xy(cx, cy),
        radius,
        stops,
        SpreadMode::Pad,
        Transform::identity(),
    ) else {
        return;
    };
    let paint = Paint {
        shader,
        anti_alias: true,
        ..Default::default()
    };
    let mut pb = PathBuilder::new();
    pb.push_circle(cx, cy, radius * 1.5);
    if let Some(path) = pb.finish() {
        pixmap.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }
}

fn boost_saturation(rgb: [u8; 3], factor: f32) -> [u8; 3] {
    if (factor - 1.0).abs() < f32::EPSILON {
        return rgb;
    }
    let srgb = Srgb::new(
        rgb[0] as f32 / 255.0,
        rgb[1] as f32 / 255.0,
        rgb[2] as f32 / 255.0,
    );
    let mut hsl: Hsl = Hsl::from_color(srgb);
    hsl.saturation = (hsl.saturation * factor).clamp(0.0, 1.0);
    let out: Srgb = hsl.into_color();
    [
        (out.red.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.green.clamp(0.0, 1.0) * 255.0).round() as u8,
        (out.blue.clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

/// Knobs for [`render_aquarelle_bleed_pass`]. All values are interpreted
/// in their natural range and clamped internally; callers can pass raw
/// slider values without pre-validation.
#[derive(Debug, Clone, Copy)]
pub struct AquarelleBleedParams {
    /// Radius of the Gaussian-approximating spread in pixels. The
    /// implementation uses a 3-pass box blur whose box width is derived
    /// from this radius (≈ `radius * 1.15`). `0.0` disables the pass.
    pub radius: f32,
    /// Intensity of the bleed in `0.0..=1.0`. `0.0` leaves the pixmap
    /// untouched; `1.0` lets the blurred layer show through at full
    /// alpha underneath the original. Out-of-range values are clamped.
    pub intensity: f32,
    /// Halo saturation boost applied to the blurred bleed layer in
    /// `0.0..=1.0`. `0.0` keeps the layer at source saturation; `1.0`
    /// multiplies saturation by `1.6` (film halation feel, matching the
    /// orb halo knob).
    pub halo: f32,
}

impl Default for AquarelleBleedParams {
    /// Mild paper-bleed preset: `radius = 3.0`, `intensity = 0.5`,
    /// `halo = 0.3`. Suitable as a starting point for ink-on-paper
    /// looks where the original picture is the foreground.
    fn default() -> Self {
        Self {
            radius: 3.0,
            intensity: 0.5,
            halo: 0.3,
        }
    }
}

impl AquarelleBleedParams {
    /// Return a copy with every field clamped to its valid range
    /// (`radius >= 0.0`, `intensity` and `halo` in `0.0..=1.0`). Called
    /// internally by [`render_aquarelle_bleed_pass`] — exposed publicly
    /// so callers that want to mirror the clamp can stay in sync.
    pub fn clamped(self) -> Self {
        Self {
            radius: self.radius.max(0.0),
            intensity: self.intensity.clamp(0.0, 1.0),
            halo: self.halo.clamp(0.0, 1.0),
        }
    }
}

/// Apply a watercolor bleed pass to an entire `pixmap`.
///
/// Unlike [`render_aquarelle_orb`], which draws a new orb from
/// `(center, radius, color)`, this pass bleeds the pixels already
/// rasterized in `pixmap`. Internally it:
///
/// 1. Snapshots the current pixmap.
/// 2. Applies a 3-pass box blur to the snapshot (Gaussian approximation
///    via the central limit theorem). The blur runs on pre-multiplied
///    RGBA so alpha bleeds naturally.
/// 3. Boosts saturation on the blurred layer by `params.halo`.
/// 4. Multiplies the blurred layer by a faint seed-derived paper-grain
///    noise (`±10 % * intensity`) so repeated frames stay watercolor-y
///    instead of flat-blurry.
/// 5. Linearly blends the blurred layer into the original by
///    `intensity` (`out = original * (1 - intensity) + blurred * intensity`).
///    For the `blueprinter` use case of "black ink on white paper" the
///    dark ink bleeds outward, mixing into the surrounding white to
///    produce a soft gray halo while distant white pixels stay white.
///
/// Identical `(pixmap, params, seed)` produce byte-identical output.
///
/// # Example
///
/// ```
/// use aquarelle::{render_aquarelle_bleed_pass, AquarelleBleedParams};
/// use tiny_skia::{Color, Paint, PathBuilder, Pixmap, Transform, FillRule};
///
/// // Start from a white page with a single black dot.
/// let mut pix = Pixmap::new(64, 64).unwrap();
/// pix.fill(Color::from_rgba8(255, 255, 255, 255));
/// let mut paint = Paint::default();
/// paint.set_color_rgba8(0, 0, 0, 255);
/// let mut pb = PathBuilder::new();
/// pb.push_circle(32.0, 32.0, 4.0);
/// let path = pb.finish().unwrap();
/// pix.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
///
/// render_aquarelle_bleed_pass(
///     &mut pix,
///     AquarelleBleedParams::default(),
///     42,
/// );
/// ```
pub fn render_aquarelle_bleed_pass(pixmap: &mut Pixmap, params: AquarelleBleedParams, seed: u64) {
    let p = params.clamped();
    if p.radius <= 0.0 || p.intensity <= 0.0 {
        return;
    }

    let width = pixmap.width() as usize;
    let height = pixmap.height() as usize;
    if width == 0 || height == 0 {
        return;
    }

    // Snapshot the current pixmap. `original` is the foreground that
    // will be re-composited on top after blurring.
    let original: Vec<u8> = pixmap.data().to_vec();

    // Work buffer for the blur. Operates on pre-multiplied RGBA bytes.
    let mut blurred = original.clone();
    let box_width = (p.radius * 1.15).round().max(1.0) as usize;
    let box_radius = box_width.max(1);
    let mut scratch = vec![0u8; blurred.len()];
    for _ in 0..3 {
        box_blur_horizontal(&blurred, &mut scratch, width, height, box_radius);
        box_blur_vertical(&scratch, &mut blurred, width, height, box_radius);
    }

    // Halo: saturation boost on the blurred layer.
    if p.halo > 0.0 {
        boost_saturation_buffer(&mut blurred, 1.0 + 0.6 * p.halo);
    }

    // Paper-grain noise: faint seed-derived multiplicative jitter so the
    // bleed reads as paper texture and not as a flat Gaussian. Amplitude
    // is capped at 0.1 × intensity to stay subtle on edge cases (e.g.
    // fully transparent inputs at intensity = 1.0).
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let noise_amp = 0.1 * p.intensity;
    if noise_amp > 0.0 {
        for px in blurred.chunks_exact_mut(4) {
            let n = 1.0 + (rng.gen_range(-1.0..=1.0_f32)) * noise_amp;
            for c in &mut px[..4] {
                *c = ((*c as f32) * n).clamp(0.0, 255.0) as u8;
            }
        }
    }

    // Compose: linearly blend the blurred layer into the original by
    // `intensity`. With intensity=0 the pixmap is unchanged; with
    // intensity=1 the pixmap is replaced by the blurred layer. For the
    // ink-on-paper use case (black dot on white page) this produces a
    // soft gray halo around the dot — the blur spreads the dark ink
    // outward, and the intensity-weighted mix darkens nearby white
    // pixels without touching distant ones.
    let t = p.intensity;
    let inv = 1.0 - t;
    let dst = pixmap.data_mut();
    for (d, (o, b)) in dst
        .chunks_exact_mut(4)
        .zip(original.chunks_exact(4).zip(blurred.chunks_exact(4)))
    {
        for i in 0..4 {
            let v = (o[i] as f32) * inv + (b[i] as f32) * t;
            d[i] = v.clamp(0.0, 255.0) as u8;
        }
    }
}

/// 1D box blur along the horizontal axis. `src` and `dst` are
/// pre-multiplied RGBA buffers of `width * height * 4` bytes. The blur
/// uses clamp-to-edge sampling so edge pixels are not darkened.
fn box_blur_horizontal(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize) {
    // Clamp the effective radius so the initialisation window never
    // indexes past the rightmost pixel. With this cap a `radius` larger
    // than `width - 1` simply converges towards the row's mean, which is
    // the natural clamp-to-edge limit. Caller guarantees width >= 1.
    let radius = radius.min(width - 1);
    let window = (radius * 2 + 1) as f32;
    for y in 0..height {
        let row = y * width * 4;
        // Initialise running sum over the first window centred at x=0.
        let mut sum = [0f32; 4];
        for k in 0..=radius {
            let i = row + k * 4;
            for c in 0..4 {
                sum[c] += src[i + c] as f32;
            }
        }
        // The remaining radius samples to the left of x=0 are clamped
        // to the leftmost pixel.
        for c in 0..4 {
            sum[c] += src[row + c] as f32 * radius as f32;
        }
        for x in 0..width {
            let oi = row + x * 4;
            for c in 0..4 {
                dst[oi + c] = (sum[c] / window).clamp(0.0, 255.0) as u8;
            }
            // Slide window: subtract leftmost, add new rightmost.
            let left_idx = x.saturating_sub(radius);
            let right_idx = if x + radius + 1 < width {
                x + radius + 1
            } else {
                width - 1
            };
            let lp = row + left_idx * 4;
            let rp = row + right_idx * 4;
            for c in 0..4 {
                sum[c] += src[rp + c] as f32 - src[lp + c] as f32;
            }
        }
    }
}

/// 1D box blur along the vertical axis. Mirror of
/// [`box_blur_horizontal`] but stepping by `width * 4` bytes per row.
fn box_blur_vertical(src: &[u8], dst: &mut [u8], width: usize, height: usize, radius: usize) {
    // Mirror of the horizontal clamp: cap radius at `height - 1` so the
    // first-window initialisation cannot step past the bottom row.
    // Caller guarantees height >= 1.
    let radius = radius.min(height - 1);
    let window = (radius * 2 + 1) as f32;
    let stride = width * 4;
    for x in 0..width {
        let col = x * 4;
        let mut sum = [0f32; 4];
        for k in 0..=radius {
            let i = col + k * stride;
            for c in 0..4 {
                sum[c] += src[i + c] as f32;
            }
        }
        for c in 0..4 {
            sum[c] += src[col + c] as f32 * radius as f32;
        }
        for y in 0..height {
            let oi = col + y * stride;
            for c in 0..4 {
                dst[oi + c] = (sum[c] / window).clamp(0.0, 255.0) as u8;
            }
            let top_idx = y.saturating_sub(radius);
            let bot_idx = if y + radius + 1 < height {
                y + radius + 1
            } else {
                height - 1
            };
            let tp = col + top_idx * stride;
            let bp = col + bot_idx * stride;
            for c in 0..4 {
                sum[c] += src[bp + c] as f32 - src[tp + c] as f32;
            }
        }
    }
}

/// Apply [`boost_saturation`] over an entire pre-multiplied RGBA buffer.
/// Pixels with zero alpha are skipped (no defined hue). For non-zero
/// alpha the saturation operation is performed on the un-premultiplied
/// color, then re-premultiplied.
fn boost_saturation_buffer(buf: &mut [u8], factor: f32) {
    if (factor - 1.0).abs() < f32::EPSILON {
        return;
    }
    for px in buf.chunks_exact_mut(4) {
        let a = px[3];
        if a == 0 {
            continue;
        }
        let af = a as f32 / 255.0;
        // Un-premultiply.
        let r = (px[0] as f32 / 255.0) / af;
        let g = (px[1] as f32 / 255.0) / af;
        let b = (px[2] as f32 / 255.0) / af;
        let srgb = Srgb::new(r.clamp(0.0, 1.0), g.clamp(0.0, 1.0), b.clamp(0.0, 1.0));
        let mut hsl: Hsl = Hsl::from_color(srgb);
        hsl.saturation = (hsl.saturation * factor).clamp(0.0, 1.0);
        let out: Srgb = hsl.into_color();
        // Re-premultiply.
        px[0] = (out.red.clamp(0.0, 1.0) * af * 255.0).round() as u8;
        px[1] = (out.green.clamp(0.0, 1.0) * af * 255.0).round() as u8;
        px[2] = (out.blue.clamp(0.0, 1.0) * af * 255.0).round() as u8;
    }
}

fn mix_with_white(rgb: [u8; 3], amount: f32) -> [u8; 3] {
    let a = amount.clamp(0.0, 1.0);
    [
        (rgb[0] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
        (rgb[1] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
        (rgb[2] as f32 * (1.0 - a) + 255.0 * a).round() as u8,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_skia::Pixmap;

    fn fresh_pixmap(w: u32, h: u32) -> Pixmap {
        let mut p = Pixmap::new(w, h).expect("pixmap");
        p.fill(Color::from_rgba8(0, 0, 0, 255));
        p
    }

    fn count_non_black(pix: &Pixmap) -> u64 {
        pix.data()
            .chunks_exact(4)
            .filter(|px| px[0] > 0 || px[1] > 0 || px[2] > 0)
            .count() as u64
    }

    #[test]
    fn aquarelle_renders_visible_orb() {
        let mut pix = fresh_pixmap(64, 64);
        render_aquarelle_orb(
            &mut pix,
            (32.0, 32.0),
            16.0,
            [200, 100, 50],
            42,
            AquarelleParams::default(),
        );
        assert!(
            count_non_black(&pix) > 0,
            "aquarelle orb should produce visible pixels"
        );
    }

    #[test]
    fn aquarelle_zero_radius_is_noop() {
        let mut pix = fresh_pixmap(32, 32);
        render_aquarelle_orb(
            &mut pix,
            (16.0, 16.0),
            0.0,
            [200, 100, 50],
            1,
            AquarelleParams::default(),
        );
        assert_eq!(count_non_black(&pix), 0);
    }

    #[test]
    fn bloom_brightens_center() {
        let mut a = fresh_pixmap(64, 64);
        let mut b = fresh_pixmap(64, 64);
        let zero_bloom = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        };
        let full_bloom = AquarelleParams {
            bleed: 0.0,
            bloom: 1.0,
            offset: 0.0,
            halo: 0.0,
        };
        render_aquarelle_orb(&mut a, (32.0, 32.0), 24.0, [200, 100, 50], 1, zero_bloom);
        render_aquarelle_orb(&mut b, (32.0, 32.0), 24.0, [200, 100, 50], 1, full_bloom);
        let pa = a.pixel(32, 32).expect("center pixel exists");
        let pb = b.pixel(32, 32).expect("center pixel exists");
        assert!(
            pb.blue() > pa.blue(),
            "bloom should raise blue at center: zero={} full={}",
            pa.blue(),
            pb.blue()
        );
    }

    #[test]
    fn params_individually_change_output() {
        let base = AquarelleParams {
            bleed: 0.0,
            bloom: 0.0,
            offset: 0.0,
            halo: 0.0,
        };
        let mut p_base = fresh_pixmap(64, 64);
        render_aquarelle_orb(&mut p_base, (32.0, 32.0), 20.0, [200, 100, 50], 7, base);
        let base_data: Vec<u8> = p_base.data().to_vec();

        for (name, modified) in [
            ("bleed", AquarelleParams { bleed: 1.0, ..base }),
            ("bloom", AquarelleParams { bloom: 1.0, ..base }),
            (
                "offset",
                AquarelleParams {
                    offset: 1.0,
                    ..base
                },
            ),
            ("halo", AquarelleParams { halo: 1.0, ..base }),
        ] {
            let mut p = fresh_pixmap(64, 64);
            render_aquarelle_orb(&mut p, (32.0, 32.0), 20.0, [200, 100, 50], 7, modified);
            assert_ne!(
                p.data(),
                &base_data[..],
                "{name}=1.0 should change rendered orb"
            );
        }
    }

    #[test]
    fn deterministic_with_seed() {
        let mut a = fresh_pixmap(64, 64);
        let mut b = fresh_pixmap(64, 64);
        let params = AquarelleParams::default();
        render_aquarelle_orb(&mut a, (32.0, 32.0), 20.0, [200, 100, 50], 12345, params);
        render_aquarelle_orb(&mut b, (32.0, 32.0), 20.0, [200, 100, 50], 12345, params);
        assert_eq!(
            a.data(),
            b.data(),
            "same seed + inputs must produce identical output"
        );
    }

    fn fresh_white(w: u32, h: u32) -> Pixmap {
        let mut p = Pixmap::new(w, h).expect("pixmap");
        p.fill(Color::from_rgba8(255, 255, 255, 255));
        p
    }

    fn draw_black_dot(pix: &mut Pixmap, cx: f32, cy: f32, r: f32) {
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(0, 0, 0, 255);
        let mut pb = PathBuilder::new();
        pb.push_circle(cx, cy, r);
        let path = pb.finish().expect("path");
        pix.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
    }

    #[test]
    fn bleed_pass_zero_radius_is_noop() {
        let mut pix = fresh_white(32, 32);
        draw_black_dot(&mut pix, 16.0, 16.0, 4.0);
        let snapshot = pix.data().to_vec();
        render_aquarelle_bleed_pass(
            &mut pix,
            AquarelleBleedParams {
                radius: 0.0,
                ..AquarelleBleedParams::default()
            },
            42,
        );
        assert_eq!(pix.data(), &snapshot[..]);
    }

    #[test]
    fn bleed_pass_zero_intensity_is_noop() {
        let mut pix = fresh_white(32, 32);
        draw_black_dot(&mut pix, 16.0, 16.0, 4.0);
        let snapshot = pix.data().to_vec();
        render_aquarelle_bleed_pass(
            &mut pix,
            AquarelleBleedParams {
                intensity: 0.0,
                ..AquarelleBleedParams::default()
            },
            42,
        );
        assert_eq!(pix.data(), &snapshot[..]);
    }

    #[test]
    fn bleed_pass_changes_surrounding_pixels() {
        // White page + small black dot in the middle. After the bleed
        // pass, pixels just outside the dot should darken (gray halo).
        let mut pix = fresh_white(64, 64);
        draw_black_dot(&mut pix, 32.0, 32.0, 3.0);
        let before = pix.pixel(32, 38).expect("pixel");
        render_aquarelle_bleed_pass(&mut pix, AquarelleBleedParams::default(), 42);
        let after = pix.pixel(32, 38).expect("pixel");
        assert!(
            after.red() < before.red(),
            "halo should darken nearby pixel: before={} after={}",
            before.red(),
            after.red()
        );
    }

    #[test]
    fn bleed_pass_is_deterministic() {
        let mut a = fresh_white(48, 48);
        let mut b = fresh_white(48, 48);
        draw_black_dot(&mut a, 24.0, 24.0, 3.0);
        draw_black_dot(&mut b, 24.0, 24.0, 3.0);
        render_aquarelle_bleed_pass(&mut a, AquarelleBleedParams::default(), 12345);
        render_aquarelle_bleed_pass(&mut b, AquarelleBleedParams::default(), 12345);
        assert_eq!(a.data(), b.data());
    }

    #[test]
    fn bleed_params_clamped_caps_out_of_range() {
        let p = AquarelleBleedParams {
            radius: -1.0,
            intensity: 2.0,
            halo: -0.5,
        }
        .clamped();
        assert_eq!(p.radius, 0.0);
        assert_eq!(p.intensity, 1.0);
        assert_eq!(p.halo, 0.0);
    }

    #[test]
    fn clamped_caps_out_of_range() {
        let p = AquarelleParams {
            bleed: 2.0,
            bloom: -0.5,
            offset: 10.0,
            halo: -10.0,
        }
        .clamped();
        assert_eq!(p.bleed, 1.0);
        assert_eq!(p.bloom, 0.0);
        assert_eq!(p.offset, 1.0);
        assert_eq!(p.halo, 0.0);
    }
}
