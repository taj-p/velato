# Changelog

<!-- Instructions

This changelog follows the patterns described here: <https://keepachangelog.com/en/1.0.0/>.

Subheadings to categorize changes are `added, changed, deprecated, removed, fixed, security`.

-->

The latest published Velato release is [0.12.0](#0120-2026-09-09) which was released on 2026-09-09.
You can find its changes [documented below](#0120-2026-09-09).

## [Unreleased]

This release has an [MSRV][] of 1.88.

### Added

- Added `TransformComponents` and `Transform::components()` for accessing authored anchor, position, scale, rotation, and skew values without matrix decomposition. ([#124][] by [@RobertBrewitz][])
- Added precomposition time remapping (`tm`) support. ([#121][] by [@RobertBrewitz][])
- Added plumbing to support animated effects through `RenderSink::push_filter`; Requires a supporting render sink. ([#123][] by [@RobertBrewitz][])
- Added `Composition::evaluate()` to query layers, transforms, anchors, and authored paths without rendering. ([#125][] by [@RobertBrewitz][])
- Added `Renderer::try_append()` for fallible rendering. ([#125][] by [@RobertBrewitz][])
- Added opt-in `lottie-shape-paths` for Lottie-compatible ellipse and rectangle start points and drawing directions. ([#127][] by [@RobertBrewitz][])
- Added sequential trimming across repeater copies without changing paint order. ([#128][] by [@RobertBrewitz][])
- Added `EvaluatedLayer::evaluated_paths()` to query shape paths after transforms, trims, and repeaters without rendering. ([#129][] by [@RobertBrewitz][])
- Added `Renderer::append_evaluated()` to render saved evaluations without repeating layer evaluation. ([#130][] by [@RobertBrewitz][])
- Solid color layers (`ty` 1) are now rendered, as a rectangle of the layer's `sw` by `sh` filled with its color `sc`. They were imported without content and drew nothing. The model holds a solid layer as `Content::Shape` with that rectangle and fill.
- Added `Composition::unsupported_layers`, which lists layers of an unsupported type and 3D layers that are rotated or oriented about the X or Y axis, and `Composition::unsupported_assets`, which lists sound and data source assets. Other unsupported features are not reported. `UnsupportedLayer` and `UnsupportedLayerFeature` are non-exhaustive, so that more can be reported later.
- Added limits on what an animation may ask for, in the new `limits` module, so that the time and memory a frame takes are bounded and the stack cannot overflow, whatever the file. A repeater may make 2,000 copies, a star or polygon may have 2,000 points and a gradient 256 stops; one frame may hold 100,000 shapes, 2,000,000 path elements, of which 30,000 under trim paths, and 20,000 layers, nested 32 deep through precompositions and track mattes, with shape groups nested 64 deep. Import fails with the new `Error::LimitExceeded` for what is over a limit at every frame. `Composition::evaluate` fails with the new `EvaluationError::LimitExceeded` for what is over a limit at the frame, for example through animated repeater copies, so `Renderer::try_append` leaves the sink untouched. It fails with the new `EvaluationError::InvalidShape` for a rectangle or ellipse whose animated size is not finite at the frame, and for a path that is not finite in a layer that is rendered at it. Both also fail for a composition that is over a limit and was not imported.
- Layer blend modes (`bm`) are now rendered: a layer with a blend mode is composited with `RenderSink::push_layer`, and blends only with the animation or precomposition that contains it. `Add` maps to `Compose::PlusLighter`; `HardMix` has no equivalent and renders as normal.

### Changed

- Position keyframes now retain spatial tangents (`ti`, `to`) and follow curved motion paths using temporally eased arc length. ([#120][] by [@RobertBrewitz])
- Renamed `Layer::mask_layer` to `matte` and `Layer::is_mask` to `is_matte_source` to distinguish track mattes from path masks in accordance with lottie spec layers docs. ([#125][] by [@RobertBrewitz][])
- Parent and matte references now use `LayerReference`. ([#125][] by [@RobertBrewitz][])
- `model::Time` has a new field `dimension_easings` for the easing of the second to fourth dimension of a value, and `model::Tween` a new method `tween_each` that eases each dimension with an easing of its own. Code that builds a `Time` literally has to set the field; `[None; 3]` eases every dimension like the first.
- `EvaluationError` is now non-exhaustive.
- `Composition::from_json` fails for a value in which arrays and objects nest deeper than 128 levels, which is what reading a file takes.
- `model::Layer` has private fields for what import found out about its shapes and paths, so it can no longer be built with a struct literal that names every field. End the literal with `..Layer::default()`.
- `animated::Star::into_model` turns a fixed star or polygon into a path only if it has at most 64 points. One with more points stays a `Geometry::Star` and is evaluated when it is drawn, so that the memory an imported animation takes stays in proportion to the size of its file.
- `RenderSink::draw` takes a new `fill_rule` argument after `stroke`, and `model::Draw` has a new `fill_rule` field. Sinks should fill with that rule when `stroke` is `None`.
- Layers of an unsupported type, such as text, and layers without a type no longer fail the import. They are imported as empty layers that keep their index, parent, transform and matte settings, and are listed in `Composition::unsupported_layers`. Sound and data source assets no longer fail the import either and are listed in `Composition::unsupported_assets`.
- The schema enums `AnyLayer` and `AnyAsset` have a new `Unknown` variant that holds such a layer or asset as raw JSON.
- The transform schema, `schema::helpers::transform::Transform`, has four new public fields for the rotation keys of 3D layers: `x_rotation` (`rx`), `y_rotation` (`ry`), `z_rotation` (`rz`) and `orientation` (`or`). Code that builds the struct literally has to set them.
- Layers with zero opacity are skipped, along with their `begin_layer_group` and `end_layer_group` calls, and rendering with an `alpha` of zero emits nothing.
- When the `alpha` passed to the renderer needs compositing, the composition clip is pushed with `RenderSink::push_layer` instead of `push_clip_layer`.

### Fixed

- Fixed trim-path modes and stray lines between disconnected paths. ([#128][] by [@RobertBrewitz][])
- Implicit mattes now use the preceding layer; matte sources no longer render separately without `td`. ([#126][] by [@RobertBrewitz][])
- Hidden layers retain content and effects for matte use, per Lottie's hidden-layer rules. ([#126][] by [@RobertBrewitz][])
- Fixed reversed transform skew direction for nonzero `sk` values. ([#119][] by [@RobertBrewitz])
- Reject invalid animation frame rates duriing import ([#121][] by [@RobertBrewitz][])
- Fixed omitted layer rotation causing panick during import. ([#122][] by [@RobertBrewitz][])
- Opacity now applies to a layer, precomposition, shape group or repeater copy as a whole, and the `alpha` passed to the renderer to the whole animation. Content with more than one draw call is rendered into a layer first, so overlapping shapes no longer show through each other; a single draw call still takes the opacity in its brush.
- Import no longer panics on split rotations, split positions in group transforms, easing handles with several or no values, or the `Add` and `HardMix` blend modes. A split rotation uses its Z component and an easing handle its first value.
- Import and rendering no longer panic on a polystar point count or repeater copy count too large to hold in memory, which are now errors, or on trim paths and spatial position keyframes over extremely long curves.
- Trim paths that follow each other can no longer multiply the pieces of a path without bound. Each trim that wraps around the end of a path cuts it in two, so thirty of them asked for a thousand million pieces; the paths that a trim applies to now count twice toward the limits on path elements.
- Trim paths measure curves to an accuracy relative to their length once they are longer than 1e8 units, and scale down coordinates above 1e100 first, so that the time a trim takes no longer grows with the coordinates. A path with a coordinate that is not finite is left untrimmed. Paths of ordinary size are trimmed as before.
- Position keyframes with spatial tangents and coordinates above 1e100 are measured scaled down. Importing them took tens of milliseconds per keyframe, and they evaluated to a position that was not finite.
- Trim paths and position keyframes with spatial tangents measure every curve at a size that kurbo can measure: a curve smaller than 1e-100 units is scaled up first, as larger ones already were scaled down, and a path that holds a coordinate near the largest a float can represent is no longer scaled down to nothing. Such curves took tens of milliseconds each to measure.
- Position keyframes with spatial tangents that are barely longer than the spacing of floats at the position, as when a keyframe returns to the point it left by way of a tangent of 1e-12, are measured as a copy at the origin. Evaluating such a position took milliseconds, because the curve lost its shape to rounding while it was measured.
- Trim paths measure a curve that is short for the coordinates it lies at as a copy that starts at the origin, as position keyframes do. A curve at 1e30 that is a few float steps long took half a second to trim, because it lost its shape to rounding while it was measured, and two thousand copies of it, which is within every limit, a quarter of an hour. Curves at 1e17 and beyond were affected. A curve is measured where it lies as long as floats there are spaced 2^24 times finer than the accuracy it is measured to, which is the case for every path whose coordinates stay below 2.6e7, so those are trimmed to the same bits as before.
- The copies that a repeater makes of a fill or stroke share its brush and stroke settings. Each copy held its own, so a gradient with 30,000 colour stops under a repeater with 250 copies took 190 MB to evaluate.
- A fill or stroke under a transform that flattens it to a line, exactly or but for rounding, draws nothing, as it shows nothing in lottie-web. A layer without width in a precomposition that is turned by a quarter is flat but for rounding, and drawing the path of an inner group with it handed the sink coordinates of 1e30 and more. A skew of 90° is treated the same, where lottie-web stretches the shape across the whole animation. A stroke outside a flattened group still draws the line that is left of its path.
- A trim path measures each curve that it applies to once, and leaves the paths that it does not apply to where they are. It measured each curve up to three times and copied every path of the layer, so several trim paths in one layer took time in proportion to their number times the size of the layer.
- The arcs of ellipses and rounded rectangles are approximated with a tolerance that grows with shapes larger than 100,000 units, so that the number of curves no longer grows without limit with the size of the shape. Shapes of ordinary size are unchanged.
- A trim that covers hidden geometry now gives it its share of the range when rendering too, as lottie-web does: a sequential trim over a hidden and a visible path spreads over both. `EvaluatedLayer::evaluated_paths()` already did, so its visible paths could differ from the ones drawn.
- A fill or stroke that comes after a repeater also draws the repeater's source geometry at its unrepeated position, as lottie-web does. This shows when the repeater has an offset or no copies. An even-odd fill leaves the source out when there are copies, because lottie-web puts it into the path twice, and a sequential trim after the repeater reaches the copies before the source. `EvaluatedLayer::evaluated_paths()` returns the source geometry as well.
- A fill or stroke that comes after a repeater no longer takes the opacity of each copy. The start and end opacity of a repeater apply to the styles it repeats; a style after it draws the paths of all copies at once, at its own opacity, as lottie-web does.
- Finding out whether a layer with a blend mode draws anything evaluates its shapes once per frame. A layer inside nested precompositions with blend modes was evaluated again for each of them.
- A keyframe that eases the dimensions of its value separately, with easing handles that hold a value for each dimension, is now evaluated that way, as lottie-web does: the width and height of a size, the two axes of a scale, anchor or position without spatial tangents, and the channels of a color each follow their own easing curve. Such a keyframe was imported as two keyframes at the same time, of which the second applied, so every dimension took the easing of the second. A dimension that a handle has no value for takes the first, as a value of one dimension always did.
- The start and end opacity of a repeater whose copies are stacked the other way round (`m: 2`) now run along the copies in that order, as in lottie-web: the copy that is moved furthest takes the start opacity. They ran from the unmoved copy, as they do for `m: 1`.
- A trim path measures each path in the path's own space, as lottie-web does. A trim that applies to each path separately (`m: 1`) measured a path of an inner group after the transform of that group, so that outside a group with different scales along its two axes it ended at another point of the path than in lottie-web. Trims that take the paths one after another (`m: 2`) already measured them this way.
- A trim path that takes the paths one after another (`m: 2`) and follows a repeater whose copies are stacked the other way round (`m: 2`) now starts at the copy that is moved furthest, as in lottie-web. It started at the unmoved copy.
- The sink is no longer handed a path, a clip or a transform that is not finite. `Composition::evaluate`, and so `Renderer::try_append`, fails with `EvaluationError::InvalidShape` for a layer that is rendered at the frame and has a shape or a mask whose path is not finite there: an animated path or mask whose easing overshoots by more than a float can hold, which eased path keyframes made possible, or a path whose vertex and tangent add up to more than that. What is finite in the animation and is not once a transform is applied to it is left out: a fill or stroke whose path overflows in the transform of a group or of a repeater copy is not drawn, nor is a layer whose transform overflows in the transform passed to the renderer, and nothing is drawn with a transform that is not finite. The paths of layers that are not rendered at the frame are not checked, and `EvaluatedLayer::evaluated_paths()` can still return them.
- An ellipse of any finite size has a finite path without the `lottie-shape-paths` feature too. kurbo holds an ellipse as a matrix and derives the radii of its arc from the squares and fourth powers of the coefficients, which overflow for a circle with a radius above 1e154 and for other ellipses above 1e77, so that the path was not a number. With the feature the path already was finite.
- Path keyframes are now eased. An animated path (`sh`) or mask path moves from one keyframe to the next along the easing curve of the keyframe, as lottie-web does; it moved at a constant speed, whatever the easing. Paths with linear easing are unchanged.
- Fills and gradient fills with the even-odd fill rule (`r: 2`) are now filled with it. They were always filled with the non-zero rule.
- 3D layers are now rotated by their rotation about the Z axis (`rz`). The rotation keys of a 3D layer (`rx`, `ry`, `rz`, `or`) were not read at all, so such a layer was not rotated. The orientation (`or`) adds its rotation about the Z axis to it, as in lottie-web, where an animated orientation turns the short way round at a constant speed. Rotation about the X and Y axes, and orientation about them, are still ignored.
- Hidden shapes (`hd`) are no longer rendered: hidden geometry and hidden styles draw nothing, a hidden group is left out with its content, and a hidden trim path is not applied, as hidden repeaters already were not. For groups, trim paths and repeaters this follows the Lottie specification; lottie-web 5.7.14 ignores `hd` on them. Hidden geometry, styles and groups stay in the model as `Shape::Hidden`, so `EvaluatedLayer::evaluated_paths()` and `authored_paths()` still include them.
- Gradient fills and gradient strokes now apply their opacity (`o`).
- Precomposition layers are now clipped to their width and height. A precomposition that covers the composition needs no clip and pushes none.
- Keyframe lookup no longer depends on which of several keyframes at the same time a binary search returns. The last one applies from that time on, and a span without length is never interpolated.
- Import now accepts these deviations from the schema that occur in real files: a keyframe value written as a bare number, a static scalar written as an array of one number, a fractional animation width or height (rounded up), and a shape direction (`d`) other than 1 or 3 (read as normal).
- Keyframes without a start value (`s`) now import. Older exporters end each animated property with a keyframe that holds only a time; it takes the previous keyframe's end value (`e`).

## [0.12.0][] (2026-09-09)

This release has an [MSRV][] of 1.88.

### Fixed

- Fixed omitted transform scales defaulting to 1% instead of 100%. ([#113][] by [@RobertBrewitz])
- Fixed `Composite` values to `Below = 1`, `Above = 2` and defaults to `Below`. ([#116][] by [@RobertBrewitz][])

### Added

- Retain unmodeled layer effects (`ef`) as raw JSON. ([#114][] by [@RobertBrewitz][])
- Added image asset metadata and image layer rendering. ([#115][] by [@RobertBrewitz])
- Added repeater shape import and rendering, including animated properties, split position, per-copy opacity, below/above compositing, hidden repeaters, and mirrored scales at fractional offsets. ([#116][] by [@RobertBrewitz][])

### Changed

- `RenderSink` implementations must now provide `draw_image`, passing a bare `vello::Scene` to `Renderer::append` panics when drawing an image. Use `Renderer::render_to_vello_scene` with a caller-owned image map, or use a custom sink instead. ([#115][] by [@RobertBrewitz])
- Vello has been upgraded to v0.10 ([#117][] by [@RobertBrewitz])

## [0.11.0]

This release has an [MSRV][] of 1.88.

### Changed

- Vello has been upgraded to v0.9, and `wgpu` (in the `with_winit` example) to v29. ([#110][] by [@nixonyh][])

## [0.10.0]

This release has an [MSRV][] of 1.88.

### Added

- Added `markers`, `meta` and `metadata` fields to the Animation schema. ([#101][] by [@RobertBrewitz][])
- Added PolyStar shape support (star and polygon geometry), including animated properties for position, radii, roundness, rotation, and point count. ([#102][] by [@RobertBrewitz][])
- Added `begin_layer_group`/`end_layer_group` hooks to `RenderSink` for per-layer rendering callbacks. ([#104][] by [@RobertBrewitz][])

### Fixed

- Fixed `with_winit` example failing to start due to `wgpu` version mismatch (v26 vs v27) left behind during the Vello 0.7 upgrade. ([#100][] by [@RobertBrewitz][])
- Fixed precomp `start_time` always being `None` because `#[serde(flatten)]` shadowed the outer `Layer.start_time` for the "st" key. ([#103][] by [@RobertBrewitz][])
- Fixed hidden layers breaking parent transform chains, hidden layers now remain in the layer set with their transforms intact but render no content. ([#103][] by [@RobertBrewitz][])
- Fixed `AnyAsset` deserialization errors not naming the failing asset. Errors now include the asset id, and also forward the actual inner failure (e.g. an unknown shape variant inside a precomposition's layers) instead of the generic "did not match any variant of untagged enum" message. ([#105][] by [@RobertBrewitz][])

## [0.9.0]

This release has an [MSRV][] of 1.88.

### Changed

- The `RenderSink` trait has been reintroduced, making it possible to use `velato` with any rendering backend. The `vello` dependency is now optional. ([#95][] by [@nicoburns][])
- Vello has been upgraded to v0.7 ([#96][] by [@nicoburns][])

### Removed

- The `thiserror` dependency has been removed ([#94][] by [@nicoburns][])
- The `once_cell` dependency has been removed ([#93][] by [@nicoburns][])

### Changed

## [0.8.1]

This release has an [MSRV][] of 1.86.

### Fixed

- A panic on WASM when finding roots.

## [0.8.0]

This release has an [MSRV][] of 1.86.

### Added

- Added image schema layer. ([#78][] by [@RobertBrewitz][])
- Added deserializer to deserialize into the correct Layer type based on the "ty" field. ([#78][] by [@RobertBrewitz][])
- Added twist, stroke_dash, modifier and rounded_corners schema shapes. ([#78][] by [@RobertBrewitz][])
- Added support for trim paths in animations. ([#83][] by [@RobertBrewitz][])

### Changed

- Changed the rust schema to better match the lottie spec. ([#78][] by [@RobertBrewitz][])

### Fixed

- Fixed tweening issue by replacing keyframe with kurbo cubic bezier implementation. ([#80][] & [#81][] by [@RobertBrewitz][])
- Fixed flickering of hold frames in splines. ([#82][] by [@RobertBrewitz][])
- Fixed matte layer clipping. ([#84][] by [@RobertBrewitz][])
- Fixed easing handles, only use the first two components ([#85][] by [@RobertBrewitz][])

### Removed

- Removed custom LayerId type in favor for new deserializer. ([#78][] by [@RobertBrewitz][])
- Removed unused layer enumeration. ([#78][] by [@RobertBrewitz][])
- Removed SKEW_LIMIT from Transform::evaluate method as it can cause rendering errors. ([#79][] by [@RobertBrewitz][])

## [0.7.0]  - 2025-10-11

This release has an [MSRV][] of 1.86.

### Changed

- Updated to vello 0.6

## [0.6.0]  - 2025-05-26

This release has an [MSRV][] of 1.85.

### Added

- Keyframes in an animated Spline may now specify an `"e"` key, dictating an end value. When specified, this value is used instead of the next keyframe's start value, allowing for discontinuous animations. ([#60][] by [@RishiChalla][])
- The last keyframe in an animated Spline may now specify only the timestamp, omitting all other fields. In this scenario, the previous keyframe's end/start values will be used. ([#60][] by [@RishiChalla][])

### Changed

- Updated to `vello` 0.5 ([#63][] by [@RobertBrewitz][])

## [0.5.0]  - 2025-02-02

This release has an [MSRV][] of 1.82.

### Changed

- Updated to `vello` 0.4 ([#49][] by [@simbleau][]).

## [0.4.0]  - 2024-11-21

This release has an [MSRV][] of 1.75.

### Changed

- Updated to `vello` 0.3
- Updated `thiserror` to 2.0

## [0.3.1]  - 2024-11-11

This release has an [MSRV][] of 1.75.

### Fixed

- Non-linear easing is now correctly interpolated ([#42] by [@atoktoto])

## [0.3.0]  - 2024-07-04

This release has an [MSRV][] of 1.75.

### Added

- Added `velato::Renderer::render`, which now returns a new vello scene.

### Changed

- Updated to vello 0.2
- Renamed `VelatoError` to `velato::Error`
- Renamed the existing `velato::Renderer::render` to `velato::Renderer::append`

### Removed

- All code and related profiling (`wgpu_profiler`) used in examples.

## [0.2.0] - 2024-05-26

This release has an [MSRV][] of 1.75.

### Changed

- Disable `vello`'s default `wgpu` feature, and provide a `wgpu` passthrough feature to turn it back on. ([#17] by [@MarijnS95])

### Fixed

- Image viewBox clipping is now applied to the animation ([#16] by [@luke1188])
- Errors that may occur on parsing a lottie composition are now public as `VelatoError`. ([#19] by [@simbleau])

## [0.1.0] - 2024-03-26

This release has an [MSRV][] of 1.75.

- Initial release

[@luke1188]: https://github.com/luke1188
[@MarijnS95]: https://github.com/MarijnS95
[@simbleau]: https://github.com/simbleau
[@atoktoto]: https://github.com/atoktoto
[@RishiChalla]: https://github.com/RishiChalla
[@RobertBrewitz]: https://github.com/RobertBrewitz
[@nicoburns]: https://github.com/nicoburns
[@nixonyh]: https://github.com/nixonyh

[#16]: https://github.com/linebender/velato/pull/16
[#17]: https://github.com/linebender/velato/pull/17
[#19]: https://github.com/linebender/velato/pull/19
[#42]: https://github.com/linebender/velato/pull/42
[#49]: https://github.com/linebender/velato/pull/49
[#60]: https://github.com/linebender/velato/pull/60
[#63]: https://github.com/linebender/velato/pull/63
[#78]: https://github.com/linebender/velato/pull/78
[#79]: https://github.com/linebender/velato/pull/79
[#80]: https://github.com/linebender/velato/pull/80
[#81]: https://github.com/linebender/velato/pull/81
[#82]: https://github.com/linebender/velato/pull/82
[#83]: https://github.com/linebender/velato/pull/83
[#84]: https://github.com/linebender/velato/pull/82
[#85]: https://github.com/linebender/velato/pull/85
[#93]: https://github.com/linebender/velato/pull/93
[#94]: https://github.com/linebender/velato/pull/94
[#95]: https://github.com/linebender/velato/pull/95
[#96]: https://github.com/linebender/velato/pull/96
[#100]: https://github.com/linebender/velato/pull/100
[#101]: https://github.com/linebender/velato/pull/101
[#102]: https://github.com/linebender/velato/pull/102
[#103]: https://github.com/linebender/velato/pull/103
[#104]: https://github.com/linebender/velato/pull/104
[#105]: https://github.com/linebender/velato/pull/105
[#110]: https://github.com/linebender/velato/pull/110
[#113]: https://github.com/linebender/velato/pull/113
[#114]: https://github.com/linebender/velato/pull/114
[#115]: https://github.com/linebender/velato/pull/115
[#116]: https://github.com/linebender/velato/pull/116
[#117]: https://github.com/linebender/velato/pull/117
[#119]: https://github.com/linebender/velato/pull/119
[#120]: https://github.com/linebender/velato/pull/120
[#121]: https://github.com/linebender/velato/pull/121
[#122]: https://github.com/linebender/velato/pull/122
[#123]: https://github.com/linebender/velato/pull/123
[#124]: https://github.com/linebender/velato/pull/124
[#125]: https://github.com/linebender/velato/pull/125
[#126]: https://github.com/linebender/velato/pull/126
[#127]: https://github.com/linebender/velato/pull/127
[#128]: https://github.com/linebender/velato/pull/128
[#129]: https://github.com/linebender/velato/pull/129
[#130]: https://github.com/linebender/velato/pull/130

[Unreleased]: https://github.com/linebender/velato/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/linebender/velato/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/linebender/velato/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/linebender/velato/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/linebender/velato/compare/v0.8.1...v0.9.0
[0.8.1]: https://github.com/linebender/velato/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/linebender/velato/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/linebender/velato/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/linebender/velato/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/linebender/velato/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/linebender/velato/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/linebender/velato/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/linebender/velato/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/linebender/velato/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/linebender/velato/releases/tag/v0.1.0

[MSRV]: README.md#minimum-supported-rust-version-msrv
