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
