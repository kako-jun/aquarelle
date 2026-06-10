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
///    The blend is performed in sRGB byte space without gamma correction;
///    this matches typical watercolor compositing in practice and keeps
///    the per-pixel cost minimal.
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
            // Noise is a paper-texture concept: only modulate RGB and
            // leave alpha untouched so opaque pixels stay opaque. Then
            // clamp each channel by alpha to preserve the premultiplied
            // invariant `RGB <= A`.
            for c in &mut px[..3] {
                *c = ((*c as f32) * n).clamp(0.0, 255.0).round() as u8;
            }
            let a = px[3];
            px[0] = px[0].min(a);
            px[1] = px[1].min(a);
            px[2] = px[2].min(a);
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
            d[i] = v.clamp(0.0, 255.0).round() as u8;
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
                dst[oi + c] = (sum[c] / window).clamp(0.0, 255.0).round() as u8;
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
                dst[oi + c] = (sum[c] / window).clamp(0.0, 255.0).round() as u8;
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

// ============================================================================
// 新「にじみ」= 48 タップ黄金角螺旋（orber #239 / #250 Phase 1）
// ----------------------------------------------------------------------------
// 旧 `render_aquarelle_bleed_pass`（CPU 3-pass box blur・全画面）とは別系統の、
// **per-primitive** な本物の空間ブラー。orber で kako-jun が承認したルックを共有
// エンジン化したもの。GPU は `AQUA_BLEED_WGSL`（orber/additive が include）、CPU は
// 下の `*_cpu` リファレンス（blueprinter / parity オラクル用）。
//
// **このエンジンは「にじみ」だけを担う**。「ぼやけ」（orb 縁の柔らかさ / falloff）は
// 各アプリに残す。にじみは形を formless 化する空間ブラーであり、読みやすさは各アプリ
// 側の「前面に元を重ねる」合成で担保する。box は温存（blueprinter が現役採用中）。
// ============================================================================

/// 共有「にじみ」WGSL フラグメント（48 タップ黄金角螺旋 + bloom/halo character）。
///
/// orber / additive（どちらも wgpu）がシェーダに **結合** して使う。結合前にホスト側で
/// `TAU` / `hash21` / `clampf` / `coverage_at` を定義しておくこと（必要シンボルと署名は
/// フラグメント冒頭のコメント参照）。中身は orber `orb.wgsl` から byte 等価に移植。
///
/// box blur 経路（[`render_aquarelle_bleed_pass`]）とは別物。
pub const AQUA_BLEED_WGSL: &str = include_str!("aqua_bleed.wgsl");

/// 新「にじみ」（螺旋ブラー）の 4 軸パラメータ。各 `0.0..=1.0`。
///
/// orber の製品 UI は 3 段ボタン（weak/mid/strong）でこれを駆動する。量の目安は
/// `weak ≈ 0.33` / `mid ≈ 0.66` / `strong ≈ 1.0`（実マッピングは消費側が持つ）。
///
/// **注意**: box blur 用の [`AquarelleBleedParams`]（`radius`/`intensity`/`halo`）とは
/// 別の型。こちらは per-primitive 螺旋ブラーの 4 軸で、orber の `aqua_bleed` /
/// `aqua_bloom` / `aqua_halo` / `aqua_offset` に対応する。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpiralBleedParams {
    /// にじみ（空間ブラー）量。`0.0` = 素の形（にじみオフ）→ `1.0` = 強く formless 化。
    pub bleed: f32,
    /// 中心の柔らかい明るいコア（控えめに白へ）。`0.0` = 無し。`bleed > 0` が前提。
    pub bloom: f32,
    /// 外周の彩度ブースト（枠リングは作らない）。`0.0` = 無し。`bleed > 0` が前提。
    pub halo: f32,
    /// にじみの左右非対称（disk 原点を seed 方向へずらす）。`0.0` = 対称。`bleed > 0` が前提。
    pub offset: f32,
}

impl Default for SpiralBleedParams {
    /// 全軸 `0.0` = にじみ無し（素の形）。
    fn default() -> Self {
        Self {
            bleed: 0.0,
            bloom: 0.0,
            halo: 0.0,
            offset: 0.0,
        }
    }
}

// --- CPU リファレンス（WGSL と同一の式・定数。blueprinter / parity オラクル用）-------
// GPU と bit-exact ではない（CPU/GPU の sin 実装差）が、式と定数は WGSL に一致させる。
// 不変条件（`blur_px=0` で単一タップ恒等・定数被覆で恒等・character の各軸オフで恒等）は
// sin に依存せず厳密に成り立つので、それをテストの土台にする。

/// 螺旋ブラーのタップ数（WGSL `AQUA_BLUR_TAPS` と一致）。
pub const AQUA_BLUR_TAPS: u32 = 48;
/// 黄金角（rad、WGSL `AQUA_GOLDEN_ANGLE` と一致）。値は orber `orb.wgsl` と逐字一致させる
/// （f32 の表現桁を超えるが WGSL/naga も同じ f32 に丸めるので値は一致＝parity）。
#[allow(clippy::excessive_precision)]
pub const AQUA_GOLDEN_ANGLE: f32 = 2.39996323;
/// bloom が白へ寄せる上限比（WGSL `BLOOM_MAX`）。
pub const BLOOM_MAX: f32 = 0.45;
/// halo の彩度ゲイン係数（WGSL `HALO_SAT_GAIN`）。
pub const HALO_SAT_GAIN: f32 = 0.6;
/// offset の disk 原点バイアス比（WGSL `AQUA_OFFSET_BIAS`）。
pub const AQUA_OFFSET_BIAS: f32 = 0.6;
/// 2π（WGSL がホスト供給を前提とする `TAU` = `6.28318530718` と同一 f32）。
pub const AQUA_TAU: f32 = std::f32::consts::TAU;

/// WGSL `smoothstep(edge0, edge1, x)` と同式（`edge0 > edge1` の逆向きも許容）。
#[inline]
fn aqua_smoothstep(edge0: f32, edge1: f32, x: f32) -> f32 {
    let t = ((x - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// WGSL `hash21` の CPU port。**`fract` は `v - v.floor()`**（`f32::fract` は符号付きで
/// WGSL `fract` と異なるため使わない）。GPU の per-pixel ディザと同じ値を返す。
#[inline]
pub fn aqua_hash21(x: f32, y: f32) -> f32 {
    let h = x * 127.1 + y * 311.7;
    // 43758.5453123 は WGSL hash21 と逐字一致（f32 表現桁超だが同一 f32 に丸まる）。
    #[allow(clippy::excessive_precision)]
    let v = h.sin() * 43758.5453123;
    v - v.floor()
}

/// WGSL `aqua_seed_dir` の CPU port。seed から決定論的な単位方向ベクトルを返す。
#[inline]
pub fn aqua_seed_dir_cpu(seed: f32) -> (f32, f32) {
    let a = aqua_hash21(seed * 12.9898, seed * 78.233 + 4.1) * AQUA_TAU;
    (a.cos(), a.sin())
}

/// WGSL `blurred_coverage` の CPU リファレンス。
///
/// `coverage_at` は「タップ点 `(x, y)` でのシルエット被覆 `(straight_alpha, rgb_scale)`」
/// を返すクロージャ（消費側固有の `coverage_at` を呼び出し側がここへ束ねる）。`sample_px`
/// は評価画素、`blur_px` は disk 半径、`seed` は per-primitive seed、`bias_px` は offset 軸の
/// disk 原点ずれ（`offset = 0` で `(0.0, 0.0)`）。
///
/// `blur_px = 0` かつ `bias_px = (0, 0)` のとき全タップが `sample_px` に潰れ、結果は
/// `coverage_at(sample_px)` そのものになる（sin に依存しない厳密恒等）。
pub fn aqua_blurred_coverage_cpu(
    coverage_at: impl Fn(f32, f32) -> (f32, f32),
    sample_px: (f32, f32),
    blur_px: f32,
    seed: f32,
    bias_px: (f32, f32),
) -> (f32, f32) {
    let n = AQUA_BLUR_TAPS;
    let nf = n as f32;
    let ang0 = seed * AQUA_GOLDEN_ANGLE * 7.0 + aqua_hash21(sample_px.0, sample_px.1) * AQUA_TAU;
    let cx = sample_px.0 + bias_px.0;
    let cy = sample_px.1 + bias_px.1;
    let mut sum_a = 0.0f32;
    let mut sum_scaled = 0.0f32;
    for k in 0..n {
        let kf = k as f32;
        let rr = blur_px * ((kf + 0.5) / nf).sqrt();
        let th = ang0 + kf * AQUA_GOLDEN_ANGLE;
        let sp_x = cx + th.cos() * rr;
        let sp_y = cy + th.sin() * rr;
        let (a, s) = coverage_at(sp_x, sp_y);
        sum_a += a;
        sum_scaled += a * s;
    }
    let avg_a = sum_a / nf;
    let avg_scale = if sum_a > 0.0 { sum_scaled / sum_a } else { 1.0 };
    (avg_a, avg_scale)
}

/// WGSL `aqua_character` の CPU リファレンス（ブラー後の色味補正。各軸 `0.0` で恒等）。
///
/// `color` は straight RGB（各 `0.0..=1.0`）、`cov_a` はブラー後の被覆 alpha。
/// `halo` は外周の彩度ブースト、`bloom` は中心の白寄せ。`bloom = halo = 0.0` で `color`
/// をそのまま返す（厳密恒等）。
pub fn aqua_character_cpu(color: [f32; 3], cov_a: f32, bloom: f32, halo: f32) -> [f32; 3] {
    let mut rgb = color;
    if halo > 0.0 {
        let edgeness = aqua_smoothstep(0.45, 0.05, cov_a);
        let luma = rgb[0] * 0.299 + rgb[1] * 0.587 + rgb[2] * 0.114;
        let sat_gain = 1.0 + halo * HALO_SAT_GAIN * edgeness;
        for c in rgb.iter_mut() {
            *c = (luma + (*c - luma) * sat_gain).clamp(0.0, 1.0);
        }
    }
    if bloom > 0.0 {
        let centerness = aqua_smoothstep(0.18, 0.5, cov_a);
        let t = bloom * BLOOM_MAX * centerness;
        for c in rgb.iter_mut() {
            *c = *c * (1.0 - t) + t; // mix(rgb, white=1.0, t)
        }
    }
    rgb
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

    #[test]
    fn bleed_pass_default_values_match_spec() {
        let d = AquarelleBleedParams::default();
        assert_eq!(d.radius, 3.0);
        assert_eq!(d.intensity, 0.5);
        assert_eq!(d.halo, 0.3);
    }

    #[test]
    fn bleed_pass_uniform_input_is_invariant() {
        // A uniform-color pixmap should remain (approximately) the same
        // color after the bleed pass: the box blur is a no-op on uniform
        // input (clamp-to-edge), halo=0 disables saturation boost, and
        // only the seed-derived multiplicative noise (±10% × intensity)
        // can perturb individual pixels.
        let mut pix = Pixmap::new(32, 32).expect("pixmap");
        pix.fill(Color::from_rgba8(128, 64, 32, 255));
        let before = pix.data().to_vec();
        render_aquarelle_bleed_pass(
            &mut pix,
            AquarelleBleedParams {
                radius: 3.0,
                intensity: 1.0,
                halo: 0.0,
            },
            0,
        );
        let after = pix.data();
        // Tolerance: noise amplitude is 0.1 × intensity = 0.1, applied
        // multiplicatively. For the largest channel (red=128) the max
        // perturbation is ~12.8, with some additional slack for blur
        // rounding at the edges. 26 is well above the worst case.
        for (b, a) in before.chunks_exact(4).zip(after.chunks_exact(4)) {
            for i in 0..4 {
                let diff = (b[i] as i32 - a[i] as i32).abs();
                assert!(
                    diff <= 26,
                    "uniform input should stay close: channel {i} before={} after={} diff={}",
                    b[i],
                    a[i],
                    diff
                );
            }
        }
    }

    #[test]
    fn bleed_pass_intensity_monotonic() {
        // White background + central black dot. As intensity increases,
        // the blurred dark layer mixes more strongly into nearby pixels,
        // so red at a point a few pixels off-center must decrease
        // monotonically (0.0 ≥ 0.5 ≥ 1.0).
        fn render_at_intensity(intensity: f32) -> u8 {
            let mut pix = fresh_white(64, 64);
            // 1-pixel "dot": tiny circle so the bleed has plenty of
            // contrast to spread.
            draw_black_dot(&mut pix, 32.0, 32.0, 0.5);
            render_aquarelle_bleed_pass(
                &mut pix,
                AquarelleBleedParams {
                    radius: 3.0,
                    intensity,
                    halo: 0.5,
                },
                42,
            );
            pix.pixel(34, 32).expect("pixel").red()
        }
        let r0 = render_at_intensity(0.0);
        let r05 = render_at_intensity(0.5);
        let r1 = render_at_intensity(1.0);
        assert!(
            r0 >= r05 && r05 >= r1,
            "red should decrease monotonically: r0={r0} r05={r05} r1={r1}"
        );
    }

    #[test]
    fn bleed_pass_halo_changes_output() {
        let mut a = fresh_white(64, 64);
        draw_black_dot(&mut a, 32.0, 32.0, 3.0);
        // Replace the dot with a saturated red so the halo saturation
        // boost has color to work with.
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(200, 50, 50, 255);
        let mut pb = PathBuilder::new();
        pb.push_circle(32.0, 32.0, 3.0);
        let path = pb.finish().expect("path");
        a.fill_path(
            &path,
            &paint,
            FillRule::Winding,
            Transform::identity(),
            None,
        );
        let mut b = a.clone();
        render_aquarelle_bleed_pass(
            &mut a,
            AquarelleBleedParams {
                radius: 3.0,
                intensity: 0.5,
                halo: 0.0,
            },
            42,
        );
        render_aquarelle_bleed_pass(
            &mut b,
            AquarelleBleedParams {
                radius: 3.0,
                intensity: 0.5,
                halo: 1.0,
            },
            42,
        );
        assert_ne!(
            a.data(),
            b.data(),
            "halo=0 and halo=1 should produce different output"
        );
    }

    #[test]
    fn bleed_pass_different_seeds_differ() {
        let mut a = fresh_white(64, 64);
        let mut b = fresh_white(64, 64);
        draw_black_dot(&mut a, 32.0, 32.0, 3.0);
        draw_black_dot(&mut b, 32.0, 32.0, 3.0);
        render_aquarelle_bleed_pass(&mut a, AquarelleBleedParams::default(), 1);
        render_aquarelle_bleed_pass(&mut b, AquarelleBleedParams::default(), 2);
        assert_ne!(a.data(), b.data(), "different seeds should perturb noise");
    }

    #[test]
    fn bleed_pass_seed_irrelevant_when_intensity_zero() {
        let mut a = fresh_white(48, 48);
        let mut b = fresh_white(48, 48);
        draw_black_dot(&mut a, 24.0, 24.0, 3.0);
        draw_black_dot(&mut b, 24.0, 24.0, 3.0);
        let params = AquarelleBleedParams {
            intensity: 0.0,
            ..AquarelleBleedParams::default()
        };
        render_aquarelle_bleed_pass(&mut a, params, 1);
        render_aquarelle_bleed_pass(&mut b, params, 99999);
        assert_eq!(
            a.data(),
            b.data(),
            "intensity=0 must short-circuit before seed is consumed"
        );
    }

    #[test]
    fn bleed_pass_tall_pixmap_does_not_panic() {
        let mut pix = Pixmap::new(1, 100).expect("pixmap");
        pix.fill(Color::from_rgba8(255, 255, 255, 255));
        render_aquarelle_bleed_pass(&mut pix, AquarelleBleedParams::default(), 42);
    }

    #[test]
    fn bleed_pass_wide_pixmap_does_not_panic() {
        let mut pix = Pixmap::new(100, 1).expect("pixmap");
        pix.fill(Color::from_rgba8(255, 255, 255, 255));
        render_aquarelle_bleed_pass(&mut pix, AquarelleBleedParams::default(), 42);
    }

    #[test]
    fn bleed_pass_1x1_pixmap_does_not_panic() {
        let mut pix = Pixmap::new(1, 1).expect("pixmap");
        pix.fill(Color::from_rgba8(255, 255, 255, 255));
        render_aquarelle_bleed_pass(&mut pix, AquarelleBleedParams::default(), 42);
    }

    #[test]
    fn bleed_pass_huge_radius_does_not_panic() {
        // Radius far larger than the pixmap. After a 3-pass box blur of
        // an enormous window the entire image should converge to roughly
        // the mean color, so the max pairwise channel difference must
        // remain small.
        let mut pix = fresh_white(16, 16);
        draw_black_dot(&mut pix, 8.0, 8.0, 2.0);
        render_aquarelle_bleed_pass(
            &mut pix,
            AquarelleBleedParams {
                radius: 1000.0,
                intensity: 1.0,
                halo: 0.0,
            },
            42,
        );
        let data = pix.data();
        let mut max_r = 0u8;
        let mut min_r = 255u8;
        for px in data.chunks_exact(4) {
            max_r = max_r.max(px[0]);
            min_r = min_r.min(px[0]);
        }
        let spread = max_r as i32 - min_r as i32;
        assert!(
            spread <= 30,
            "huge radius should converge to near-uniform color, but red spread={spread}"
        );
    }

    #[test]
    fn bleed_pass_fully_transparent_pixmap_does_not_panic() {
        // `Pixmap::new` returns an all-zero buffer (alpha=0). The
        // `boost_saturation_buffer` loop must `continue` on alpha==0 and
        // the compose step must keep alpha at zero.
        let mut pix = Pixmap::new(32, 32).expect("pixmap");
        render_aquarelle_bleed_pass(
            &mut pix,
            AquarelleBleedParams {
                radius: 3.0,
                intensity: 0.5,
                halo: 1.0,
            },
            42,
        );
        for px in pix.data().chunks_exact(4) {
            assert_eq!(px[3], 0, "alpha must remain 0 on transparent input");
        }
    }

    #[test]
    fn bleed_pass_repeated_application_is_stable() {
        // Apply the pass twice. The far corners should still be near
        // white — bleed shouldn't propagate dramatically across the
        // entire image after two applications with default radius.
        let mut pix = fresh_white(64, 64);
        draw_black_dot(&mut pix, 32.0, 32.0, 3.0);
        render_aquarelle_bleed_pass(&mut pix, AquarelleBleedParams::default(), 42);
        render_aquarelle_bleed_pass(&mut pix, AquarelleBleedParams::default(), 42);
        let corner = pix.pixel(0, 0).expect("pixel");
        assert!(
            corner.red() > 200 && corner.green() > 200 && corner.blue() > 200,
            "far corner should stay near white after 2x bleed: r={} g={} b={}",
            corner.red(),
            corner.green(),
            corner.blue()
        );
    }

    #[test]
    fn bleed_pass_negative_radius_is_noop() {
        let mut pix = fresh_white(32, 32);
        draw_black_dot(&mut pix, 16.0, 16.0, 4.0);
        let snapshot = pix.data().to_vec();
        render_aquarelle_bleed_pass(
            &mut pix,
            AquarelleBleedParams {
                radius: -5.0,
                intensity: 0.5,
                halo: 0.3,
            },
            42,
        );
        assert_eq!(
            pix.data(),
            &snapshot[..],
            "negative radius should clamp to 0 and short-circuit"
        );
    }

    #[test]
    fn bleed_pass_intensity_one_replaces_with_blurred() {
        // intensity=1.0 fully replaces the pixmap with the blurred
        // layer. The center pixel of a black dot should no longer be
        // pure black because the surrounding white pixels mix in via
        // the blur.
        let mut pix = fresh_white(64, 64);
        draw_black_dot(&mut pix, 32.0, 32.0, 3.0);
        render_aquarelle_bleed_pass(
            &mut pix,
            AquarelleBleedParams {
                radius: 3.0,
                intensity: 1.0,
                halo: 0.0,
            },
            42,
        );
        let center = pix.pixel(32, 32).expect("pixel");
        assert!(
            center.red() > 0,
            "center red should be lifted off zero by blurred white neighbors, got {}",
            center.red()
        );
    }
}

#[cfg(test)]
mod spiral_bleed_tests {
    use super::*;

    #[test]
    fn wgsl_fragment_exposes_expected_symbols() {
        assert!(AQUA_BLEED_WGSL.contains("fn blurred_coverage"));
        assert!(AQUA_BLEED_WGSL.contains("fn aqua_character"));
        assert!(AQUA_BLEED_WGSL.contains("fn aqua_seed_dir"));
        assert!(AQUA_BLEED_WGSL.contains("AQUA_BLUR_TAPS"));
    }

    #[test]
    fn cpu_consts_match_documented_values() {
        assert_eq!(AQUA_BLUR_TAPS, 48);
        assert_eq!(BLOOM_MAX, 0.45);
        assert_eq!(HALO_SAT_GAIN, 0.6);
        assert_eq!(AQUA_OFFSET_BIAS, 0.6);
        // 黄金角 = π(3−√5)。リテラル比較（トートロジー）でなく独立計算で検証する。
        let golden = std::f32::consts::PI * (3.0 - 5.0_f32.sqrt());
        assert!(
            (AQUA_GOLDEN_ANGLE - golden).abs() < 1e-5,
            "golden angle {} vs {}",
            AQUA_GOLDEN_ANGLE,
            golden
        );
        assert_eq!(AQUA_TAU, std::f32::consts::TAU);
    }

    #[test]
    fn blur_zero_is_single_tap_identity() {
        // blur_px=0 & bias=0 → 全タップが sample_px に潰れる → sampler の値そのもの。
        // 位置依存の sampler（定数でない）を使い、潰れていることを保証する。
        let sampler = |x: f32, y: f32| ((x * 0.01 + y * 0.02).rem_euclid(1.0), 0.7);
        let expected = sampler(10.0, 20.0);
        let got = aqua_blurred_coverage_cpu(sampler, (10.0, 20.0), 0.0, 3.0, (0.0, 0.0));
        assert!(
            (got.0 - expected.0).abs() < 1e-6,
            "alpha {} vs {}",
            got.0,
            expected.0
        );
        assert!(
            (got.1 - expected.1).abs() < 1e-6,
            "scale {} vs {}",
            got.1,
            expected.1
        );
    }

    #[test]
    fn constant_coverage_averages_to_constant() {
        // どの位置でも (0.4, 0.6) を返す被覆 → 平均も (0.4, 0.6)。
        let got = aqua_blurred_coverage_cpu(|_, _| (0.4, 0.6), (5.0, 5.0), 8.0, 1.0, (0.0, 0.0));
        assert!((got.0 - 0.4).abs() < 1e-6, "alpha {}", got.0);
        assert!((got.1 - 0.6).abs() < 1e-6, "scale {}", got.1);
    }

    #[test]
    fn zero_alpha_coverage_keeps_unit_scale() {
        // 被覆 alpha が全タップ 0 → avg_scale は 1.0 にフォールバック（0 除算回避）。
        let got = aqua_blurred_coverage_cpu(|_, _| (0.0, 0.5), (5.0, 5.0), 8.0, 1.0, (0.0, 0.0));
        assert_eq!(got.0, 0.0);
        assert_eq!(got.1, 1.0);
    }

    #[test]
    fn character_identity_when_axes_off() {
        let c = [0.2, 0.6, 0.9];
        assert_eq!(aqua_character_cpu(c, 0.3, 0.0, 0.0), c);
    }

    #[test]
    fn halo_increases_saturation_at_edge() {
        let c = [0.8, 0.2, 0.2];
        let cov_a = 0.2; // 縁レンジ → edgeness > 0
        let luma = c[0] * 0.299 + c[1] * 0.587 + c[2] * 0.114;
        let sat = |v: [f32; 3]| (v[0] - luma).abs() + (v[1] - luma).abs() + (v[2] - luma).abs();
        let base = aqua_character_cpu(c, cov_a, 0.0, 0.0);
        let haloed = aqua_character_cpu(c, cov_a, 0.0, 1.0);
        assert!(
            sat(haloed) > sat(base),
            "halo should boost saturation: {} vs {}",
            sat(haloed),
            sat(base)
        );
    }

    #[test]
    fn bloom_moves_toward_white_in_interior() {
        let c = [0.2, 0.3, 0.4];
        let cov_a = 0.5; // 内部 → centerness 高
        let base = aqua_character_cpu(c, cov_a, 0.0, 0.0);
        let bloomed = aqua_character_cpu(c, cov_a, 1.0, 0.0);
        for i in 0..3 {
            assert!(
                bloomed[i] > base[i],
                "bloom channel {} should rise toward white: {} vs {}",
                i,
                bloomed[i],
                base[i]
            );
        }
    }

    #[test]
    fn seed_dir_is_unit_vector() {
        for &seed in &[0.0f32, 0.3, 1.0, 7.5, 42.0] {
            let (dx, dy) = aqua_seed_dir_cpu(seed);
            let len = (dx * dx + dy * dy).sqrt();
            assert!((len - 1.0).abs() < 1e-5, "seed {} → len {}", seed, len);
        }
    }
}
