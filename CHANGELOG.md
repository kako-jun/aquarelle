# Changelog

## [0.3.0] — 2026-06-11

Third release. Adds the **spiral bleed** (にじみ) — a second, per-primitive
bleed algorithm distinct from the v0.2 whole-pixmap box-blur pass. This is
the watercolor bleed developed and approved in `orber` (#239), now shared so
`orber` and `additive` (both wgpu) can `include` the WGSL and `blueprinter`
(CPU) can use the Rust reference. The shared engine carries **にじみ only**
(the formless spreading bleed); **ぼやけ** (plain edge softness) stays in each
consumer. Readability is the consumer's job (composite the sharp original on
top of the bleed).

### Added

- `AQUA_BLEED_WGSL` — the shared WGSL fragment (48-tap golden-angle spiral
  spatial blur + `bloom`/`halo` color character). Byte-equivalent to orber's
  in-tree `orb.wgsl`. The consuming shader must define `TAU`, `hash21`,
  `clampf`, and `coverage_at` before concatenation (signatures documented in
  the fragment header).
- `SpiralBleedParams { bleed, bloom, halo, offset }` — the 4-axis params for
  the spiral bleed (each `0.0..=1.0`), mapping to orber's
  `aqua_bleed`/`aqua_bloom`/`aqua_halo`/`aqua_offset`. Distinct from the
  box-blur `AquarelleBleedParams`.
- CPU reference (mirrors the WGSL math exactly, for `blueprinter` + as a
  parity oracle): `aqua_blurred_coverage_cpu`, `aqua_character_cpu`,
  `aqua_seed_dir_cpu`, `aqua_hash21`, plus the constants `AQUA_BLUR_TAPS`,
  `AQUA_GOLDEN_ANGLE`, `BLOOM_MAX`, `HALO_SAT_GAIN`, `AQUA_OFFSET_BIAS`,
  `AQUA_TAU`. (Not bit-exact with the GPU across the `sin` boundary; the
  invariant tests — single-tap at `blur=0`, constant-coverage identity,
  character axes-off identity — hold regardless.)

### Unchanged

- `render_aquarelle_orb` / `AquarelleParams` and the v0.2 box-blur pass
  `render_aquarelle_bleed_pass` / `AquarelleBleedParams` are untouched
  (`blueprinter` still depends on the box pass).

## [0.2.0] — 2026-05-18

Second public release. Adds a whole-pixmap bleed pass alongside the
existing orb renderer.

### Added

- `render_aquarelle_bleed_pass(pixmap, params, seed)` — apply a soft
  watercolor bleed to an already-rasterized `tiny_skia::Pixmap`. Uses a
  3-pass box blur as a Gaussian approximation, boosts saturation on the
  blurred layer by `params.halo`, multiplies a faint seed-derived
  paper-grain noise into the blur, and re-composites the original
  picture on top so the bleed reads as a halo underneath existing
  strokes (the `blueprinter` use case).
- `AquarelleBleedParams { radius, intensity, halo }` with
  `Default = { 3.0, 0.5, 0.3 }` and `clamped()` mirroring the internal
  clamp.

### Unchanged

- `render_aquarelle_orb` and `AquarelleParams` are byte-compatible with
  v0.1.0.

## [0.1.0] — 2026-05-17

Initial release. Extracted from
[`orber-core`](https://github.com/kako-jun/orber/tree/main/crates/core)'s
in-tree `aquarelle` module (in production since orber v0.2.x;
designed to be liftable since orber PR #30).

### Added

- `render_aquarelle_orb(pixmap, center, radius, color, seed, params)` —
  composite one watercolor orb onto a `tiny_skia::Pixmap` with
  source-over blending and per-seed determinism.
- `AquarelleParams { bleed, bloom, offset, halo }` — four orthogonal
  tunables (`0.0..=1.0`) for the satellite-bleed, white-flare core,
  off-center gradient, and halation-saturation boost.
- `AquarelleParams::clamped()` exposed publicly so callers can mirror
  the internal clamp without duplicating limits.
- `AquarelleParams::default()` = every knob at `0.5` (calm
  cel-anime mid-strength preset).
