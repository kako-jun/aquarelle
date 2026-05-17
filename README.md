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
aquarelle = "0.1"
```

## Example

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

- **v0.1.0** — extracted from `orber-core v0.3.x`'s in-tree `aquarelle`
  module, where it has been in production since 2026-04 (see
  [`orber` PR #30 / Issue #8](https://github.com/kako-jun/orber/issues/8)).
  API is stable; the breakout closes orber Issue #10 and unblocks
  `blueprinter` adopting the same texture set.

## License

MIT
