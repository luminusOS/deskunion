# DeskUnion app icon

## Artwork

The two interlocking links retain the existing DeskUnion D/U identity: two
desktops connected through shared input. The arrangement, rather than color
alone, distinguishes the links. No lettering, computer screenshot, stock app
identity, raster image, or external font is embedded.

- Editable source: `io.github.luminusos.DeskUnion.Source.svg`, a 192×152 board
  with named full-color, symbolic, and hidden guide layers.
- Runtime full-color: `../../crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion.svg`,
  nominal 128×128. Flat GNOME Blue 3 / Green 2 surfaces, Blue 5 / Green 5
  lower profiles, 4px nominal depth. Artwork spans x=14…114 and y=12…116.
- Runtime symbolic: `../../crates/deskunion-gtk/resources/io.github.luminusos.DeskUnion-symbolic.svg`,
  nominal 16×16, redrawn without perspective. The small interruption at the
  upper crossing separates the two links. Holes are transparent geometry.
- Windows: `../../crates/deskunion-gtk/resources/deskunion.ico`, containing
  native-size renders at 16, 24, 32, 48, 64, 128, and 256px.
- Preview: `../../screenshots/deskunion-icon-preview.png`; full-color at
  128/64/32px and symbolic at 16/32/64/128px on light and dark backgrounds.

Keep source and runtime path geometry and fills synchronized. When exporting
the source, use the full-color region **(0, 0, 128, 128)** or symbolic region
**(160, 16, 16, 16)**, preserving transparent margins. Do not export the whole
board or crop to the artwork bounding box. Guides and authoring metadata belong
only in the source.

## References and provenance

Artwork was written as explicit editable SVG by an AI agent, adapting the
previous D/U link identity already present in this repository. No GNOME artwork
or template paths were copied. Assets remain under this project's existing
licensing; see `../../LICENSE` and the GTK crate's `GPL-3.0-or-later` declaration.
This is project artwork, not a human-made upstream GNOME submission.

Design references:

- [GNOME HIG app icons](https://developer.gnome.org/hig/guidelines/app-icons.html):
  simple metaphor, 128px nominal artwork, small-size readability, flat planar
  surfaces, lower profile, and no external drop shadow.
- [GNOME palette](https://developer.gnome.org/hig/reference/palette.html):
  `#3584e4`, `#1a5fb4`, `#57e389`, and `#26a269`.
- [GNOME HIG UI icons](https://developer.gnome.org/hig/guidelines/ui-icons.html)
  and [GTK symbolic format](https://docs.gtk.org/gtk4/icon-format.html):
  orthogonal monochrome geometry and runtime recoloring.
- HIG template proportions recorded by the workspace `gnome-icons` skill at
  revision `abdeb8ed577dbece1f4c959c9ff5d796307c8286`: 2px detail grid,
  square keyline approximately 103×103, baseline y≈117. Used as construction
  references, not copied into the artwork.

## Verification — 2026-10-06

- Static `gnome-icons` SVG inspector: both runtime exports passed with no
  errors or warnings. Source passed; its warnings concern Inkscape layer
  metadata, intentionally absent from runtime exports.
- Full-color rasterized with librsvg 2.63.2 at every ICO size. Reviewed the
  128/64/32px contact sheet at native size on light and dark backgrounds.
- Symbolic lookup and recoloring tested with GTK 4.24.1 through the compiled
  GResource, at 16/32/64/128px. The preview uses actual GTK renders, not an
  alpha-mask tint simulation. GTK 4.14, the crate's minimum, was not tested.
- GResource compilation and source/export geometry comparison passed.
- ICO decoded at all seven sizes and matched the corresponding PNG pixels.
- Application ID, desktop `Icon=`, resource aliases, Flatpak installation
  commands, and Nix installation paths agree. Flatpak/Nix package builds and
  Windows/macOS runtime installation were not executed.
- `cargo fmt --all --check` passed. GTK crate tests and workspace Clippy could
  not run offline because `autocfg v1.5.0` and `aead v0.5.2`, respectively,
  were missing from the dependency cache.

Independent human review of metaphor and small-size recognition remains
pending. Technical checks and the author's visual inspection are not GNOME
design approval or independent user testing.
