# aquarelle

[![crates.io](https://img.shields.io/crates/v/aquarelle.svg)](https://crates.io/crates/aquarelle)
[![docs.rs](https://img.shields.io/docsrs/aquarelle)](https://docs.rs/aquarelle)
[![CI](https://github.com/kako-jun/aquarelle/actions/workflows/ci.yml/badge.svg)](https://github.com/kako-jun/aquarelle/actions/workflows/ci.yml)

Watercolor-style **soft-bleed orb rendering** on a `tiny_skia::Pixmap`.

Given a center, a radius, an RGB color, and a `u64` seed, `aquarelle`
composites a calm cel-anime night-scene orb onto a pixel buffer you
already own. Four orthogonal knobs — `bleed`, `bloom`, `offset`,
`halo` — let you tune from a flat soft orb to a hazy paper-bleed
light source.

Originally written as the texture set inside the
[`orber`](https://crates.io/crates/orber) abstract-mood-image
generator; lifted into its own crate so other watercolor / sumi
renderers (e.g. `blueprinter`) can share the same engine.

## Install

```toml
[dependencies]
aquarelle = "0.2"
```

## Example: orb rendering

```rust
use aquarelle::{render_aquarelle_orb, AquarelleParams};
use tiny_skia::{Color, Pixmap};

let mut pix = Pixmap::new(128, 128).unwrap();
pix.fill(Color::from_rgba8(0, 0, 0, 255));

render_aquarelle_orb(
    &mut pix,
    (64.0, 64.0),           // center
    40.0,                   // radius
    [200, 100, 50],         // sRGB color
    42,                     // seed (deterministic)
    AquarelleParams::default(),
);

// `pix.data()` is now BGRA bytes you can write to PNG, send to
// WebCodecs, copy to a GPU texture, etc.
```

## Example: bleed pass over an existing picture (v0.2)

Use `render_aquarelle_bleed_pass` when the pixmap already contains your
art (e.g. ink strokes from `blueprinter`) and you want a soft halo
underneath the existing pixels.

```rust
use aquarelle::{render_aquarelle_bleed_pass, AquarelleBleedParams};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Transform};

let mut pix = Pixmap::new(128, 128).unwrap();
pix.fill(Color::from_rgba8(255, 255, 255, 255));

// Draw a black dot to bleed.
let mut paint = Paint::default();
paint.set_color_rgba8(0, 0, 0, 255);
let mut pb = PathBuilder::new();
pb.push_circle(64.0, 64.0, 8.0);
let path = pb.finish().unwrap();
pix.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);

render_aquarelle_bleed_pass(
    &mut pix,
    AquarelleBleedParams::default(), // radius 3, intensity 0.5, halo 0.3
    42,
);
```

A 3-pass box blur approximates a Gaussian; a faint seed-derived paper
grain is multiplied onto the blurred layer; the original picture is
then re-composited on top.

## Spiral bleed — にじみ (v0.3)

The v0.2 pass above is a whole-pixmap box blur. v0.3 adds a second,
**per-primitive** bleed: the 48-tap golden-angle spiral (`AQUA_BLEED_WGSL`)
developed and approved in [`orber`](https://github.com/kako-jun/orber) (#239).
It spatially averages each primitive's coverage so a sharp shape spreads into
a formless, organic cloud — closer to pigment spreading on wet paper than a
uniform blur.

It ships in two matched forms so any renderer can use it:

- **GPU** — `AQUA_BLEED_WGSL` is a WGSL fragment for `orber` and `additive`
  (both wgpu) to concatenate into their shaders. (They still carry an in-tree
  copy today; wiring them onto this shared fragment is a later phase.) The host
  shader must define `TAU`, `hash21`, `clampf`, and `coverage_at` first
  (signatures are in the fragment header).
- **CPU** — `aqua_blurred_coverage_cpu` / `aqua_character_cpu` mirror the same
  math in Rust for `blueprinter` (no GPU) and as a parity oracle.

This engine carries **にじみ only** (the formless spread). **ぼやけ** (plain
edge softness) is intentionally *not* here — it stays in each consumer.
Because the spread dissolves shapes, readability is the consumer's job:
composite the sharp original on top of the bleed (ink line + watercolor wash).

```rust
use aquarelle::{aqua_blurred_coverage_cpu, aqua_character_cpu, SpiralBleedParams};

let p = SpiralBleedParams { bleed: 1.0, bloom: 0.0, halo: 0.5, offset: 0.3 };

let blur_px = 12.0 * p.bleed;
let (alpha, scale) = aqua_blurred_coverage_cpu(
    // your silhouette: return (straight_alpha, rgb_scale) at a tap point (x, y)
    |x, y| { let _ = (x, y); (0.4, 1.0) },
    (64.0, 64.0), // sample pixel
    blur_px,
    0.0,          // per-primitive seed
    (0.0, 0.0),   // offset bias (0 = symmetric)
);
let rgb = aqua_character_cpu([0.2, 0.5, 0.9], alpha, p.bloom, p.halo);
let _ = (scale, rgb);
```

## The four elements

| Knob | Range | What it does |
|---|---|---|
| `bleed` | `0.0 .. 1.0` | Number and size of same-color satellite gradients scattered around the orb (film grain / paper bleed). |
| `bloom` | `0.0 .. 1.0` | A near-white core inside the inner ~30 % of the radius so the orb reads as a light source. |
| `offset` | `0.0 .. 1.0` | Decouples the gradient center from the geometric center by up to 25 % of the radius along a seed-derived angle. |
| `halo` | `0.0 .. 1.0` | Saturation boost on the outer falloff (film halation feel). |

`AquarelleParams::default()` sets every knob to `0.5` for a calm
mid-strength orb.

## Renderer-agnostic on purpose

`aquarelle` does **not** know what's already on the pixmap, what the
rest of your scene looks like, or how you intend to encode the
result. It composites four watercolor layers onto the buffer you
hand it; you decide background fill, layout, output format. This
keeps the crate usable from CLIs (PNG via `image`), browser
visualizers (`OffscreenCanvas` + `wasm-bindgen`), and animation
pipelines (frame-by-frame `Pixmap` reuse) with the same code.

## Determinism

Identical `(center, radius, color, seed, params)` produce
byte-identical pixels. Internal RNG is seeded per call via
`ChaCha8Rng::seed_from_u64(seed)` and never touches `thread_rng`,
so animation pipelines that re-render the same orb every frame
get a stable visual.

## wasm friendliness

The crate's only dependencies are `tiny-skia`, `palette`, `rand`,
and `rand_chacha` — all wasm-compatible. It is used in production
from [`orber-wasm`](https://github.com/kako-jun/orber/tree/main/crates/wasm)
on Cloudflare Pages.

## Status

- **v0.2.0** — adds `render_aquarelle_bleed_pass` for bleeding an
  existing rasterized picture (3-pass box blur Gaussian approximation +
  halo saturation boost + seed-derived paper grain). The original
  `render_aquarelle_orb` API is unchanged. See
  [Issue #2](https://github.com/kako-jun/aquarelle/issues/2).
- **v0.1.0** — extracted from `orber-core v0.3.x`'s in-tree `aquarelle`
  module, where it has been in production since 2026-04 (see
  [`orber` PR #30 / Issue #8](https://github.com/kako-jun/orber/issues/8)).
  API is stable; the breakout closes orber Issue #10 and unblocks
  `blueprinter` adopting the same texture set.

## License

MIT
