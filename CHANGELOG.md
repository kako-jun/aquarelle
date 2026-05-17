# Changelog

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
