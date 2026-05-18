# Changelog

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
