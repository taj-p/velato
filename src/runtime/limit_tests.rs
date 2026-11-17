// Copyright 2026 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Tests of the [limits](crate::limits) on what an animation may ask for.

use super::model::{Content, Geometry, Layer};
use super::render::count_shapes;
use crate::limits::{
    Limit, Limits, MAX_GRADIENT_STOPS, MAX_GROUP_DEPTH, MAX_LAYER_INSTANCES, MAX_NESTING_DEPTH,
    MAX_PATH_ELEMENTS, MAX_POLYSTAR_POINTS, MAX_REPEATER_COPIES, MAX_SHAPE_INSTANCES,
    MAX_TRIMMED_ELEMENTS, ShapeCheck, ShapeCost, ShapeError,
};
use crate::test_support::*;
use crate::{Composition, Error, EvaluationError, OccurrencePath, Renderer};
use kurbo::Affine;
use serde_json::{Value, json};

/// Imports a 100×100 animation that runs for frames 0 to 60.
fn import(assets: Value, layers: Value) -> Result<Composition, Error> {
    Composition::from_json(json!({
        "v": "5.7.14",
        "fr": 60,
        "ip": 0,
        "op": 60,
        "w": 100,
        "h": 100,
        "assets": assets,
        "layers": layers,
    }))
}

/// Imports an animation of one shape layer.
fn import_shapes(shapes: Value) -> Result<Composition, Error> {
    import(json!([]), json!([shape_layer(1, shapes)]))
}

/// Returns the limit that an import failed for.
fn import_limit(result: Result<Composition, Error>) -> Limit {
    match result {
        Err(Error::LimitExceeded { limit, maximum }) => {
            assert_eq!(maximum, Limits::DEFAULT.maximum(limit));
            limit
        }
        Err(error) => panic!("import failed with {error}"),
        Ok(_) => panic!("import should have failed"),
    }
}

/// Returns the limit that an evaluation failed for.
fn evaluation_limit(composition: &Composition, frame: f64, limits: &Limits) -> Option<Limit> {
    match composition.evaluate_within(frame, limits) {
        Ok(_) => None,
        Err(EvaluationError::LimitExceeded { limit, maximum }) => {
            assert_eq!(maximum, limits.maximum(limit));
            Some(limit)
        }
        Err(error) => panic!("evaluation failed with {error}"),
    }
}

/// Renders a frame and returns the error, having checked that the sink was left untouched.
fn render_error(composition: &Composition, frame: f64) -> EvaluationError {
    let mut sink = RecordingSink::default();
    let error = Renderer::new()
        .try_append(composition, frame, Affine::IDENTITY, 1.0, &mut sink)
        .expect_err("rendering should have failed");
    assert!(sink.events.is_empty(), "{:?}", sink.events);
    error
}

fn exceeded(limit: Limit) -> EvaluationError {
    EvaluationError::LimitExceeded {
        limit,
        maximum: Limits::DEFAULT.maximum(limit),
    }
}

fn shapes_of(layer: &Layer) -> &[super::model::Shape] {
    match &layer.content {
        Content::Shape(shapes) => shapes,
        content => panic!("not a shape layer: {content:?}"),
    }
}

/// Counts the shapes of the first layer at a frame, within the given limits.
fn count(composition: &Composition, frame: f64, limits: &Limits) -> Result<ShapeCost, ShapeError> {
    count_shapes(shapes_of(&composition.layers[0]), frame, limits)
}

fn fixed(value: impl Into<Value>) -> Value {
    json!({ "a": 0, "k": value.into() })
}

/// A value that goes from `from` at frame 0 to `to` at frame 60.
fn animated(from: f64, to: f64) -> Value {
    json!({ "a": 1, "k": [{ "t": 0, "s": [from] }, { "t": 60, "s": [to] }] })
}

/// An open path of straight lines with the given number of vertices, which is also its number
/// of elements.
fn path(vertices: usize) -> Value {
    let points: Vec<_> = (0..vertices)
        .map(|index| json!([index, index % 2]))
        .collect();
    let tangents = vec![json!([0, 0]); vertices];
    json!({
        "ty": "sh",
        "ks": fixed(json!({ "c": false, "v": points, "i": tangents, "o": tangents })),
    })
}

fn copies(count: impl Into<Value>) -> Value {
    with(repeater(1, 1.0, 100.0, 100.0), json!({ "c": fixed(count) }))
}

fn polystar(star_type: u8, points: Value) -> Value {
    json!({
        "ty": "sr",
        "sy": star_type,
        "p": fixed(json!([50, 50])),
        "or": fixed(20),
        "os": fixed(0),
        "ir": fixed(10),
        "is": fixed(0),
        "r": fixed(0),
        "pt": points,
    })
}

fn ellipse(size: Value) -> Value {
    json!({ "ty": "el", "p": fixed(json!([0, 0])), "s": size })
}

/// A trim path that keeps half of each path and wraps around its end.
fn wrapping_trim() -> Value {
    json!({ "ty": "tm", "s": fixed(0), "e": fixed(50), "o": fixed(270), "m": 1 })
}

fn null_layers(count: usize) -> Vec<Value> {
    (0..count).map(|index| layer(3, index as u32 + 1)).collect()
}

/// A layer of the main composition that shows precomposition `c1`, which shows `c2`, and so
/// on, down to a shape layer in the last of `precompositions`.
fn nested_precompositions(precompositions: usize) -> Result<Composition, Error> {
    let shape = shape_layer(1, json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0)]));
    let assets: Vec<_> = (1..=precompositions)
        .map(|level| {
            let content = if level == precompositions {
                shape.clone()
            } else {
                precomp_layer(1, &format!("c{}", level + 1))
            };
            precomp(&format!("c{level}"), json!([content]))
        })
        .collect();
    import(json!(assets), json!([precomp_layer(1, "c1")]))
}

/// Shape layers that are each matted by the layer before them.
fn matte_chain(layers: usize) -> Result<Composition, Error> {
    let layers: Vec<_> = (0..layers)
        .map(|index| {
            let layer = shape_layer(
                index as u32 + 1,
                json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0)]),
            );
            if index == 0 {
                layer
            } else {
                with(layer, json!({ "tt": 1 }))
            }
        })
        .collect();
    import(json!([]), json!(layers))
}

#[test]
fn repeater_copies_are_limited_at_import() {
    let shapes = |count: f64| json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0), copies(count)]);
    let at_limit = import_shapes(shapes(MAX_REPEATER_COPIES as f64)).unwrap();
    assert!(at_limit.evaluate(0.0).is_ok());
    let draws = render(&at_limit, 0.0, 1.0)
        .iter()
        .filter(|event| matches!(event, Event::Draw { .. }))
        .count();
    assert_eq!(draws, MAX_REPEATER_COPIES);
    for count in [MAX_REPEATER_COPIES as f64 + 1.0, 1e5, 1e6, 3e6, 1e30] {
        assert_eq!(
            import_limit(import_shapes(shapes(count))),
            Limit::RepeaterCopies,
            "{count} copies"
        );
    }
    // A number of copies that is not whole makes the next whole number of copies.
    let most = MAX_REPEATER_COPIES as f64;
    assert!(import_shapes(shapes(most - 0.6)).is_ok());
    for count in [most + 0.01, most + 0.4, most + 0.5] {
        assert_eq!(
            import_limit(import_shapes(shapes(count))),
            Limit::RepeaterCopies,
            "{count} copies"
        );
    }
}

#[test]
fn animated_repeater_copies_are_limited_at_the_frame() {
    let animated_copies = with(
        repeater(1, 1.0, 100.0, 100.0),
        json!({ "c": animated(1.0, 4001.0) }),
    );
    let shapes = json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0), animated_copies]);
    let composition = import_shapes(shapes).unwrap();
    // 1,934 and a third copies, which makes 1,935.
    assert!(composition.evaluate(29.0).is_ok());
    // 1,999 and two thirds, which makes 2,000.
    assert!(composition.evaluate(29.98).is_ok());
    // A little over 2,000, which makes 2,001.
    assert_eq!(
        composition.evaluate(29.99).unwrap_err(),
        exceeded(Limit::RepeaterCopies)
    );
    // 2,001 copies.
    assert_eq!(
        composition.evaluate(30.0).unwrap_err(),
        exceeded(Limit::RepeaterCopies)
    );
    assert_eq!(
        render_error(&composition, 30.0),
        exceeded(Limit::RepeaterCopies)
    );
}

#[test]
fn easing_that_overshoots_cannot_exceed_the_copies() {
    // Between two keyframes of one and two copies, the easing curve leaves the range of the
    // keyframes by far.
    let overshooting = json!({
        "a": 1,
        "k": [
            { "t": 0, "s": [1], "o": { "x": [0.3], "y": [1e5] }, "i": { "x": [0.7], "y": [1e5] } },
            { "t": 60, "s": [2] },
        ],
    });
    let repeater = with(repeater(1, 1.0, 100.0, 100.0), json!({ "c": overshooting }));
    let shapes = json!([rect(0.0, 0.0, 10.0, 10.0), fill(100.0), repeater]);
    let composition = import_shapes(shapes).unwrap();
    assert!(composition.evaluate(0.0).is_ok());
    assert_eq!(
        render_error(&composition, 30.0),
        exceeded(Limit::RepeaterCopies)
    );
}

#[test]
fn nested_repeaters_are_limited_by_what_they_make_together() {
    // Three repeaters of 150 copies each make more than three million copies.
    let shapes = json!([
        rect(0.0, 0.0, 10.0, 10.0),
        fill(100.0),
        copies(150),
        copies(150),
        copies(150),
    ]);
    assert_eq!(import_limit(import_shapes(shapes)), Limit::ShapeInstances);
}

#[test]
fn shapes_are_counted_with_their_copies() {
    // One group and two repeaters, 4 × 3 paths, and 3 × 2 fills.
    let inner = group(vec![path(2), fill(100.0), copies(3)], 100.0);
    let composition = import_shapes(json!([inner, copies(2)])).unwrap();
    let cost = count(&composition, 0.0, &Limits::DEFAULT).unwrap();
    assert_eq!(cost.instances, 3 + 12 + 6);
    // Two elements in each path, and two drawn by each fill.
    assert_eq!(cost.elements, 12 * 2 + 6 * 2);

    let limits = |shape_instances| Limits {
        shape_instances,
        ..Limits::DEFAULT
    };
    assert!(count(&composition, 0.0, &limits(21)).is_ok());
    assert_eq!(
        count(&composition, 0.0, &limits(20)),
        Err(ShapeError::Limit(Limit::ShapeInstances))
    );
    assert_eq!(evaluation_limit(&composition, 0.0, &limits(21)), None);
    assert_eq!(
        evaluation_limit(&composition, 0.0, &limits(20)),
        Some(Limit::ShapeInstances)
    );
}

#[test]
fn shapes_at_the_limit_import_and_one_more_does_not() {
    // 223 × 223 paths, 222 × 222 fills, a group and two repeaters: 99,016 shapes.
    let shapes = |more: usize| {
        let inner = group(vec![path(2), fill(100.0), copies(222)], 100.0);
        let mut shapes = vec![inner, copies(222)];
        shapes.extend(std::iter::repeat_n(path(2), more));
        json!(shapes)
    };
    let room = MAX_SHAPE_INSTANCES - 99_016;
    let at_limit = import_shapes(shapes(room)).unwrap();
    assert_eq!(
        count(&at_limit, 0.0, &Limits::DEFAULT).unwrap().instances,
        MAX_SHAPE_INSTANCES
    );
    assert!(at_limit.evaluate(0.0).is_ok());
    assert_eq!(
        import_limit(import_shapes(shapes(room + 1))),
        Limit::ShapeInstances
    );
}

#[test]
fn path_elements_at_the_limit_import_and_one_more_does_not() {
    // 2,000 paths of 500 elements, which the fill after the repeater draws as one path.
    let shapes = |vertices| json!([path(vertices), copies(1999), fill(100.0)]);
    let at_limit = import_shapes(shapes(500)).unwrap();
    assert_eq!(
        count(&at_limit, 0.0, &Limits::DEFAULT).unwrap().elements,
        MAX_PATH_ELEMENTS
    );
    let evaluated = at_limit.evaluate(0.0).unwrap();
    assert_eq!(evaluated.layers[0].evaluated_paths().len(), 2000);
    assert_eq!(
        import_limit(import_shapes(shapes(501))),
        Limit::PathElements
    );
}

#[test]
fn path_elements_count_paths_styles_trims_and_gradient_stops() {
    let limits = &Limits::DEFAULT;
    let elements = |shapes: Value| {
        let composition = import_shapes(shapes).unwrap();
        count(&composition, 0.0, limits).unwrap().elements
    };
    assert_eq!(elements(json!([path(10)])), 10);
    // An empty path counts as one element.
    assert_eq!(elements(json!([path(0)])), 1);
    // Each style draws the path.
    assert_eq!(elements(json!([path(10), fill(100.0), stroke(100.0)])), 30);
    // A gradient of two colour stops.
    assert_eq!(elements(json!([path(10), gradient("gf", 100.0)])), 22);
    // A trim may cut the path in two.
    assert_eq!(elements(json!([path(10), wrapping_trim()])), 20);
    // The style before the trim draws both parts as well.
    assert_eq!(
        elements(json!([path(10), fill(100.0), wrapping_trim()])),
        40
    );
    // The copies of a repeater, and the source path, which stays in place.
    assert_eq!(elements(json!([path(10), copies(4)])), 50);
    // A style before the repeater draws each copy, and one after it draws them all and the
    // source.
    assert_eq!(elements(json!([path(10), fill(100.0), copies(4)])), 90);
    assert_eq!(elements(json!([path(10), copies(4), fill(100.0)])), 100);
    // No copies leave the source path, and nothing for the style before the repeater to draw.
    assert_eq!(elements(json!([path(10), fill(100.0), copies(0)])), 10);
}

#[test]
fn trims_in_a_row_cannot_multiply_a_path_without_limit() {
    // Each of these trims cuts every path it gets in two, so thirty of them would turn one
    // line into a thousand million pieces.
    let shapes = |trims: usize| {
        let mut shapes = vec![path(2)];
        shapes.extend(std::iter::repeat_n(wrapping_trim(), trims));
        shapes.push(stroke(100.0));
        json!(shapes)
    };
    assert_eq!(
        import_limit(import_shapes(shapes(30))),
        Limit::TrimmedElements
    );
    let composition = import_shapes(shapes(8)).unwrap();
    let evaluated = composition.evaluate(0.0).unwrap();
    let pieces: usize = evaluated.layers[0]
        .evaluated_paths()
        .iter()
        .map(|path| path.elements().len())
        .sum();
    assert_eq!(pieces, 2 * 256);
    assert!(pieces <= count(&composition, 0.0, &Limits::DEFAULT).unwrap().elements);
}

#[test]
fn polystar_points_are_limited_at_import() {
    for star_type in [1, 2] {
        let shapes = |points: f64| json!([polystar(star_type, fixed(points)), fill(100.0)]);
        let at_limit = import_shapes(shapes(MAX_POLYSTAR_POINTS as f64)).unwrap();
        let evaluated = at_limit.evaluate(0.0).unwrap();
        let paths = evaluated.layers[0].evaluated_paths();
        let vertices = MAX_POLYSTAR_POINTS * if star_type == 1 { 2 } else { 1 };
        // A move, a line to each further vertex, and a close.
        assert_eq!(paths[0].elements().len(), vertices + 1);
        for points in [MAX_POLYSTAR_POINTS as f64 + 1.0, 1e6, 1e7, 1e30] {
            assert_eq!(
                import_limit(import_shapes(shapes(points))),
                Limit::PolystarPoints,
                "{points} points"
            );
        }
    }
}

#[test]
fn animated_polystar_points_are_limited_at_the_frame() {
    let shapes = json!([polystar(1, animated(5.0, 4001.0)), fill(100.0)]);
    let composition = import_shapes(shapes).unwrap();
    assert!(composition.evaluate(29.0).is_ok());
    assert_eq!(
        render_error(&composition, 30.0),
        exceeded(Limit::PolystarPoints)
    );
}

#[test]
fn fixed_polystar_with_many_points_is_evaluated_when_drawn() {
    let geometry = |points: f64| {
        let shapes = json!([polystar(1, fixed(points)), fill(100.0)]);
        let composition = import_shapes(shapes).unwrap();
        let paths = composition.evaluate(0.0).unwrap().layers[0].evaluated_paths();
        (shapes_of(&composition.layers[0])[0].clone(), paths)
    };
    let (few, few_paths) = geometry(64.0);
    assert!(
        matches!(
            few,
            super::model::Shape::Geometry(Geometry::Fixed(ref path)) if path.len() == 129
        ),
        "{few:?}"
    );
    assert_eq!(few_paths[0].elements().len(), 129);
    let (many, many_paths) = geometry(65.0);
    assert!(
        matches!(many, super::model::Shape::Geometry(Geometry::Star(_))),
        "{many:?}"
    );
    assert_eq!(many_paths[0].elements().len(), 131);
}

#[test]
fn layers_at_the_limit_import_and_one_more_does_not() {
    // 100 layers that each show a precomposition of 198 layers, and other layers next to them.
    let layers = |others: usize| {
        let mut layers: Vec<_> = (0..100).map(|index| precomp_layer(index, "comp")).collect();
        layers.extend(null_layers(others));
        import(
            json!([precomp("comp", json!(null_layers(198)))]),
            json!(layers),
        )
    };
    let room = MAX_LAYER_INSTANCES - 100 * 199;
    let mut at_limit = layers(room).unwrap();
    assert!(at_limit.evaluate(0.0).is_ok());
    assert_eq!(import_limit(layers(room + 1)), Limit::LayerInstances);

    // A composition that is changed after import is checked when it is evaluated.
    let extra = at_limit.layers[100].clone();
    at_limit.layers.push(extra);
    assert_eq!(
        at_limit.evaluate(0.0).unwrap_err(),
        exceeded(Limit::LayerInstances)
    );
}

#[test]
fn precompositions_that_multiply_layers_are_limited() {
    // Each of six, and of seven, levels of precompositions shows the next ten times.
    for levels in [6, 7] {
        let assets: Vec<_> = (0..levels)
            .map(|level| {
                let layers: Vec<_> = if level + 1 == levels {
                    null_layers(1)
                } else {
                    (0..10)
                        .map(|index| precomp_layer(index, &format!("c{}", level + 1)))
                        .collect()
                };
                precomp(&format!("c{level}"), json!(layers))
            })
            .collect();
        let result = import(json!(assets), json!([precomp_layer(1, "c0")]));
        assert_eq!(import_limit(result), Limit::LayerInstances);
    }
}

#[test]
fn matte_source_counts_for_each_layer_that_it_mattes() {
    // A source of 30 path elements and a fill that draws them.
    let source = with(
        shape_layer(1, json!([path(30), fill(100.0)])),
        json!({ "td": 1 }),
    );
    let matted = |ind| with(layer(3, ind), json!({ "tt": 1, "tp": 1 }));
    let composition = import(json!([]), json!([source, matted(2), matted(3), matted(4)])).unwrap();
    let limits = |layer_instances, path_elements| Limits {
        layer_instances,
        path_elements,
        ..Limits::DEFAULT
    };
    // Three layers that each render the source as well.
    assert_eq!(evaluation_limit(&composition, 0.0, &limits(6, 180)), None);
    assert_eq!(
        evaluation_limit(&composition, 0.0, &limits(5, 180)),
        Some(Limit::LayerInstances)
    );
    assert_eq!(
        evaluation_limit(&composition, 0.0, &limits(6, 179)),
        Some(Limit::PathElements)
    );
}

#[test]
fn layers_count_at_the_frames_at_which_they_are_rendered() {
    // Two layers of 20 elements each, one after the other in time.
    let first = with(
        shape_layer(1, json!([path(10), fill(100.0)])),
        json!({ "op": 30 }),
    );
    let second = with(
        shape_layer(2, json!([path(10), fill(100.0)])),
        json!({ "ip": 30 }),
    );
    let hidden = with(
        shape_layer(3, json!([path(10), fill(100.0)])),
        json!({ "hd": true }),
    );
    let composition = import(json!([]), json!([first, second, hidden])).unwrap();
    let limits = Limits {
        path_elements: 20,
        ..Limits::DEFAULT
    };
    for frame in [0.0, 29.0, 30.0, 59.0] {
        assert_eq!(evaluation_limit(&composition, frame, &limits), None);
    }
}

#[test]
fn layer_with_animated_copies_counts_when_it_is_not_rendered() {
    let animated_copies = with(
        repeater(1, 1.0, 100.0, 100.0),
        json!({ "c": animated(3.6, 4.0) }),
    );
    // 50 elements at every frame: the path and its four copies.
    let dynamic = with(
        shape_layer(1, json!([path(10), animated_copies])),
        json!({ "op": 30 }),
    );
    let fixed = with(
        shape_layer(2, json!([path(10), copies(4)])),
        json!({ "op": 30 }),
    );
    let composition = import(json!([]), json!([dynamic, fixed])).unwrap();
    assert_eq!(composition.layers[0].shape_check, ShapeCheck::EveryFrame);
    assert!(matches!(
        composition.layers[1].shape_check,
        ShapeCheck::Never(ShapeCost {
            instances: 6,
            elements: 50,
            trimmed: 0,
        })
    ));
    let limits = |path_elements| Limits {
        path_elements,
        ..Limits::DEFAULT
    };
    // Both layers are rendered.
    assert_eq!(evaluation_limit(&composition, 0.0, &limits(100)), None);
    assert_eq!(
        evaluation_limit(&composition, 0.0, &limits(99)),
        Some(Limit::PathElements)
    );
    // Neither is, and the shapes of the first are counted all the same.
    assert_eq!(evaluation_limit(&composition, 40.0, &limits(50)), None);
    assert_eq!(
        evaluation_limit(&composition, 40.0, &limits(49)),
        Some(Limit::PathElements)
    );
}

#[test]
fn masks_and_effects_count_as_shapes() {
    let mask = |vertices: usize| {
        let Value::Object(mut path) = path(vertices) else {
            unreachable!()
        };
        json!({ "mode": "a", "pt": path.remove("ks").unwrap(), "o": fixed(100) })
    };
    let blur = json!({ "ty": 29, "nm": "blur", "ef": [{ "v": fixed(4) }] });
    let masked = with(
        layer(3, 1),
        json!({ "masksProperties": [mask(10), mask(15)], "ef": [blur] }),
    );
    let composition = import(json!([]), json!([masked])).unwrap();
    assert_eq!(composition.layers[0].masks.len(), 2);
    assert_eq!(composition.layers[0].effects.len(), 1);
    let limits = |shape_instances, path_elements| Limits {
        shape_instances,
        path_elements,
        ..Limits::DEFAULT
    };
    assert_eq!(evaluation_limit(&composition, 0.0, &limits(3, 25)), None);
    assert_eq!(
        evaluation_limit(&composition, 0.0, &limits(2, 25)),
        Some(Limit::ShapeInstances)
    );
    assert_eq!(
        evaluation_limit(&composition, 0.0, &limits(3, 24)),
        Some(Limit::PathElements)
    );
}

#[test]
fn nesting_at_the_limit_imports_and_one_level_more_does_not() {
    // The layer of the main composition is at depth one.
    let at_limit = nested_precompositions(MAX_NESTING_DEPTH - 1).unwrap();
    assert_eq!(
        trace(&at_limit, 0.0)
            .iter()
            .filter(|call| *call == "fill(1)")
            .count(),
        1
    );
    for precompositions in [MAX_NESTING_DEPTH, 1000, 20_000] {
        assert_eq!(
            import_limit(nested_precompositions(precompositions)),
            Limit::NestingDepth,
            "{precompositions} precompositions"
        );
    }
}

#[test]
fn matte_chain_at_the_limit_imports_and_one_layer_more_does_not() {
    let at_limit = matte_chain(MAX_NESTING_DEPTH).unwrap();
    assert!(
        trace(&at_limit, 0.0)
            .iter()
            .any(|call| call == "layer(1, SrcIn)")
    );
    for layers in [MAX_NESTING_DEPTH + 1, 1000, 20_000] {
        assert_eq!(
            import_limit(matte_chain(layers)),
            Limit::NestingDepth,
            "{layers} layers"
        );
    }
}

#[test]
fn mattes_and_precompositions_nest_together() {
    // A matte source that shows a precomposition is two levels below the layer it mattes.
    let source = with(precomp_layer(1, "comp"), json!({ "td": 1 }));
    let matted = with(layer(3, 2), json!({ "tt": 1 }));
    let composition = import(
        json!([precomp("comp", json!(null_layers(1)))]),
        json!([source, matted]),
    )
    .unwrap();
    let limits = |nesting_depth| Limits {
        nesting_depth,
        ..Limits::DEFAULT
    };
    assert_eq!(evaluation_limit(&composition, 0.0, &limits(3)), None);
    assert_eq!(
        evaluation_limit(&composition, 0.0, &limits(2)),
        Some(Limit::NestingDepth)
    );
    let mut checked = composition.clone();
    assert!(checked.check_limits(&limits(3)).is_ok());
    assert!(matches!(
        checked.check_limits(&limits(2)),
        Err(Error::LimitExceeded {
            limit: Limit::NestingDepth,
            maximum: 2
        })
    ));
}

#[test]
fn composition_nested_too_deep_after_import_fails_to_evaluate() {
    let mut composition = nested_precompositions(MAX_NESTING_DEPTH - 1).unwrap();
    assert!(composition.evaluate(0.0).is_ok());
    // Put the layer of the main composition into one more precomposition.
    let layer = composition.layers[0].clone();
    composition.assets.insert("outer".to_owned(), vec![layer]);
    composition.layers[0].content = Content::Instance {
        name: "outer".to_owned(),
        time_remap: None,
    };
    assert_eq!(
        composition.evaluate(0.0).unwrap_err(),
        exceeded(Limit::NestingDepth)
    );
    assert_eq!(
        render_error(&composition, 0.0),
        exceeded(Limit::NestingDepth)
    );
}

#[test]
fn nesting_at_the_limit_fits_a_small_stack() {
    // Evaluating and rendering recurse once for each level. Every precomposition holds a layer
    // that is matted by the layer that shows the next precomposition, which makes two levels
    // of each: one through the matte and one through the precomposition.
    //
    // The shapes are in as many groups as the JSON parser lets a precomposition nest, which is
    // the other thing that evaluating and rendering recurse for.
    let mut shapes = vec![
        rect(0.0, 0.0, 10.0, 10.0),
        wrapping_trim(),
        fill(100.0),
        stroke(100.0),
    ];
    for _ in 0..NESTED_GROUPS {
        shapes = vec![group(shapes, 50.0), stroke(100.0)];
    }
    let shape = shape_layer(1, json!(shapes));
    let levels = (MAX_NESTING_DEPTH - 1) / 2;
    let assets: Vec<_> = (1..=levels)
        .map(|level| {
            let content = if level == levels {
                shape.clone()
            } else {
                precomp_layer(1, &format!("c{}", level + 1))
            };
            let matted = with(layer(3, 2), json!({ "tt": 1 }));
            precomp(&format!("c{level}"), json!([content, matted]))
        })
        .collect();
    let nested = import(json!(assets), json!([precomp_layer(1, "c1")])).unwrap();
    let composition = nested.clone();
    let (calls, paths) = std::thread::Builder::new()
        .stack_size(STACK_FOR_NESTING)
        .spawn(move || {
            let calls = trace(&composition, 0.0).len();
            let evaluated = composition.evaluate(0.0).unwrap();
            let mut layer = &evaluated.layers[0];
            while let Some(child) = layer.children.first() {
                layer = child;
            }
            (calls, layer.evaluated_paths().len())
        })
        .unwrap()
        .join()
        .unwrap();
    assert!(calls > 3 * levels);
    assert_eq!(paths, 1);
    assert_eq!(evaluation_limit(&nested, 0.0, &Limits::DEFAULT), None);
    let shallower = Limits {
        nesting_depth: 2 * levels,
        ..Limits::DEFAULT
    };
    assert_eq!(
        evaluation_limit(&nested, 0.0, &shallower),
        Some(Limit::NestingDepth)
    );
}

#[test]
fn shape_groups_nest_no_deeper_than_the_parser_allows() {
    let nested = |groups: usize| {
        let mut shapes = vec![rect(0.0, 0.0, 10.0, 10.0), fill(100.0)];
        for _ in 0..groups {
            shapes = vec![group(shapes, 100.0)];
        }
        json!({
            "v": "5.7.14", "fr": 60, "ip": 0, "op": 60, "w": 100, "h": 100,
            "assets": [precomp("comp", json!([shape_layer(1, json!(shapes))]))],
            "layers": [precomp_layer(1, "comp")],
        })
    };
    // As text and as a value alike.
    let text = |groups| serde_json::to_string(&nested(groups)).unwrap();
    assert!(Composition::from_slice(text(NESTED_GROUPS)).is_ok());
    assert!(Composition::from_json(nested(NESTED_GROUPS)).is_ok());
    for groups in [NESTED_GROUPS + 1, MAX_GROUP_DEPTH + 1, 100] {
        assert!(
            matches!(Composition::from_slice(text(groups)), Err(Error::Json(_))),
            "{groups} groups"
        );
        assert!(
            matches!(Composition::from_json(nested(groups)), Err(Error::Json(_))),
            "{groups} groups"
        );
    }
}

#[test]
fn shape_groups_made_in_code_are_limited_in_depth() {
    use super::model::Shape;
    let composition = import_shapes(json!([path(4), fill(100.0)])).unwrap();
    let nested = |groups: usize| {
        let mut shapes = shapes_of(&composition.layers[0]).to_vec();
        for _ in 0..groups {
            shapes = vec![Shape::Group(shapes, None)];
        }
        let mut made = composition.clone();
        made.layers[0].content = Content::Shape(shapes);
        made.layers[0].shape_check = ShapeCheck::EveryFrame;
        made
    };
    let at_limit = nested(MAX_GROUP_DEPTH);
    assert_eq!(trace(&at_limit, 0.0), ["clip", "fill(1)", "pop"]);
    let mut deeper = nested(MAX_GROUP_DEPTH + 1);
    assert_eq!(render_error(&deeper, 0.0), exceeded(Limit::GroupDepth));
    assert!(matches!(
        deeper.check_limits(&Limits::DEFAULT),
        Err(Error::LimitExceeded {
            limit: Limit::GroupDepth,
            maximum: MAX_GROUP_DEPTH
        })
    ));
}

/// A stack that nesting at the limit has to fit, in a build without optimization.
const STACK_FOR_NESTING: usize = 256 * 1024;

/// The most shape groups that a shape layer of a precomposition can nest. The JSON parser
/// takes 127 levels of arrays and objects, and a group is two of them.
const NESTED_GROUPS: usize = 59;

#[test]
fn size_that_is_not_finite_is_an_error_at_the_frame() {
    // Halfway between these keyframes the difference between them is not finite.
    let size = json!({
        "a": 1,
        "k": [{ "t": 0, "s": [1e308, 1e308] }, { "t": 60, "s": [-1e308, -1e308] }],
    });
    let size = &size;
    let rounded = with(
        rect(0.0, 0.0, 10.0, 10.0),
        json!({ "r": animated(1e308, -1e308) }),
    );
    let resized = with(rect(0.0, 0.0, 10.0, 10.0), json!({ "s": size }));
    for shape in [ellipse(size.clone()), resized, rounded] {
        let only = with(
            shape_layer(1, json!([shape, fill(100.0)])),
            json!({ "op": 40 }),
        );
        let composition = import(json!([]), json!([only])).unwrap();
        assert_eq!(composition.layers[0].shape_check, ShapeCheck::EveryFrame);
        assert!(composition.evaluate(0.0).is_ok());
        let invalid = EvaluationError::InvalidShape(OccurrencePath(vec![0]));
        assert_eq!(composition.evaluate(30.0).unwrap_err(), invalid);
        assert_eq!(render_error(&composition, 30.0), invalid);
        // The paths of a layer can be queried at a frame where the layer is not rendered, so
        // its shapes have to be valid there too.
        assert_eq!(composition.evaluate(50.0).unwrap_err(), invalid);
        assert!(composition.evaluate(60.0).is_ok());
    }
}

#[test]
fn ellipses_and_rectangles_of_any_finite_size_render() {
    for size in [1e30, 1e60, 1e300] {
        let size = fixed(json!([size, size]));
        let rounded = with(
            rect(0.0, 0.0, 10.0, 10.0),
            json!({ "s": size, "r": fixed(1e300) }),
        );
        for (shape, most) in [(ellipse(size.clone()), 16), (rounded, 24)] {
            let visible = shape_layer(1, json!([shape.clone(), fill(100.0)]));
            let transparent = shape_layer(2, json!([shape, fill(0.0)]));
            let inactive = with(visible.clone(), json!({ "ip": 30 }));
            let composition = import(json!([]), json!([visible, transparent, inactive])).unwrap();
            assert_eq!(trace(&composition, 0.0), ["clip", "fill(1)", "pop"]);
            let evaluated = composition.evaluate(0.0).unwrap();
            for layer in &evaluated.layers {
                let paths = layer.evaluated_paths();
                assert_eq!(paths.len(), 1);
                assert!(paths[0].elements().len() <= most, "{paths:?}");
            }
        }
    }
}

#[test]
fn trim_of_paths_that_repeaters_scale_out_of_range_renders() {
    // Each copy is ten times the size of the one before, so the last are far beyond any
    // number a curve can be measured with, and beyond what a float can hold.
    let scaling = |count: u32, scale: f64| {
        with(
            repeater(count, 1.0, 100.0, 100.0),
            json!({ "tr": {
                "ty": "tr",
                "a": fixed(json!([0, 0])),
                "p": fixed(json!([1, 0])),
                "s": fixed(json!([scale, scale])),
                "r": fixed(0),
                "so": fixed(100),
                "eo": fixed(100),
            } }),
        )
    };
    for (mode, count, scale) in [(1, 100, 1000.0), (2, 100, 1000.0), (2, 60, 300.0)] {
        let trim = json!({ "ty": "tm", "s": fixed(10), "e": fixed(60), "o": fixed(0), "m": mode });
        let shapes = json!([
            ellipse(fixed(json!([20, 20]))),
            stroke(100.0),
            scaling(count, scale),
            trim,
        ]);
        let composition = import_shapes(shapes).unwrap();
        let start = std::time::Instant::now();
        let calls = trace(&composition, 0.0);
        assert!(calls.len() > 2, "{calls:?}");
        let paths = composition.evaluate(0.0).unwrap().layers[0].evaluated_paths();
        assert!(!paths.is_empty());
        // Measured to a fixed accuracy this took tens of milliseconds to minutes. A debug
        // build on a slow machine stays far below this.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }
}

#[test]
fn trim_of_long_curves_takes_no_longer_than_of_short_ones() {
    // 400 curves that are each 1e150 units wide.
    let size = 1e150;
    let count = 400;
    let vertices: Vec<_> = (0..=count)
        .map(|index| json!([index as f64 * size, 0.0]))
        .collect();
    let out_tangents = vec![json!([size / 2.0, size]); count + 1];
    let in_tangents = vec![json!([-size / 2.0, -size]); count + 1];
    let curvy = json!({
        "ty": "sh",
        "ks": fixed(json!({ "c": false, "v": vertices, "i": in_tangents, "o": out_tangents })),
    });
    let trim = json!({ "ty": "tm", "s": fixed(0), "e": fixed(50), "o": fixed(0), "m": 1 });
    let composition = import_shapes(json!([curvy, stroke(100.0), trim])).unwrap();
    let start = std::time::Instant::now();
    let paths = composition.evaluate(0.0).unwrap().layers[0].evaluated_paths();
    assert_eq!(paths.len(), 1);
    // Half of the curves, give or take the one that the trim ends in.
    let elements = paths[0].elements().len();
    assert!(
        (count / 2..=count / 2 + 2).contains(&elements),
        "{elements}"
    );
    assert!(start.elapsed() < std::time::Duration::from_secs(5));
}

/// Runs `work` on a thread of its own and returns what it returns.
///
/// Panics if the work takes longer than a minute. This is for work that takes milliseconds
/// when it is bounded, and minutes to hours when it is not. A minute is far more than a slow
/// and busy machine needs for the first, and it keeps the second from holding up the tests.
fn within_a_minute<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    use std::sync::mpsc::{RecvTimeoutError, channel};
    let (sender, receiver) = channel();
    std::thread::spawn(move || sender.send(work()));
    match receiver.recv_timeout(std::time::Duration::from_secs(60)) {
        Ok(result) => result,
        Err(RecvTimeoutError::Timeout) => panic!("the work took longer than a minute"),
        Err(RecvTimeoutError::Disconnected) => panic!("the work panicked"),
    }
}

/// What rendering and querying a frame of a trimmed layer came to.
struct Trimmed {
    /// Bounds of the paths that were drawn.
    drawn: Vec<kurbo::Rect>,
    /// Number of paths that the layer has.
    paths: usize,
    /// Number of curves that were measured to render the frame.
    measured: usize,
    /// The least number of times that floats were spaced finer than the accuracy of a
    /// measurement, at the coordinates of what was measured.
    resolution: f64,
}

/// Renders the first frame of a layer with the given shapes, and queries its paths.
fn trimmed(shapes: Value) -> Trimmed {
    use super::trim::{RESOLUTION_MEASURED, SEGMENTS_MEASURED};
    let composition = import_shapes(shapes).unwrap();
    within_a_minute(move || {
        RESOLUTION_MEASURED.set(f64::INFINITY);
        SEGMENTS_MEASURED.set(0);
        let drawn = render(&composition, 0.0, 1.0)
            .into_iter()
            .filter_map(|event| match event {
                Event::Draw { bounds, .. } => Some(bounds),
                _ => None,
            })
            .collect();
        let measured = SEGMENTS_MEASURED.get();
        let paths = composition.evaluate(0.0).unwrap().layers[0]
            .evaluated_paths()
            .len();
        Trimmed {
            drawn,
            paths,
            measured,
            resolution: RESOLUTION_MEASURED.get(),
        }
    })
}

/// An open path of one curve. The control points and the end are relative to the start.
fn curve(start: [f64; 2], p1: [f64; 2], p2: [f64; 2], end: [f64; 2]) -> Value {
    let vertices = [start, [start[0] + end[0], start[1] + end[1]]];
    let out_tangents = [p1, [0.0, 0.0]];
    let in_tangents = [[0.0, 0.0], [p2[0] - end[0], p2[1] - end[1]]];
    json!({
        "ty": "sh",
        "ks": fixed(json!({ "c": false, "v": vertices, "i": in_tangents, "o": out_tangents })),
    })
}

/// A trim path that keeps the part of each path from a tenth to nine tenths of its length.
fn inner_trim() -> Value {
    json!({ "ty": "tm", "s": fixed(10), "e": fixed(90), "o": fixed(0), "m": 1 })
}

/// Floats at the coordinates of a curve that is measured have to be spaced this many times
/// finer than the accuracy it is measured to. Rounding then cannot keep the measuring from
/// reaching its accuracy, so it takes what it takes for the same curve at the origin.
const RESOLVED: f64 = 4e6;

#[test]
fn trim_of_a_curve_far_from_the_origin_is_measured_at_the_origin() {
    // An S-shaped curve at 1e30, where floats are 2^47 apart, that is one such step wide and
    // two high. Measured where it lies it took half a second in an optimized build: split in
    // two, its halves are rounding errors, which never get accurate enough to stop splitting.
    let step = 2.0_f64.powi(47);
    let s_curve = |x: f64| curve([x, 0.0], [0.0, step], [0.0, -step], [step, 0.0]);
    let far = trimmed(json!([s_curve(1e30), inner_trim(), stroke(100.0)]));
    assert!(far.resolution >= RESOLVED, "{}", far.resolution);
    assert_eq!((far.drawn.len(), far.paths, far.measured), (1, 1, 1));
    // It is trimmed to the part that the same curve at the origin is trimmed to. Along Y,
    // where its coordinates are small, that is exact.
    let near = trimmed(json!([s_curve(0.0), inner_trim(), stroke(100.0)]));
    assert_eq!(near.drawn.len(), 1);
    let (far, near) = (far.drawn[0], near.drawn[0]);
    assert_eq!((far.y0, far.y1), (near.y0, near.y1));
    assert!(near.y0 < 0.0 && near.y1 > 0.0 && near.height() < 2.0 * step);
    assert!(far.x0.is_finite() && far.x1.is_finite());
}

#[test]
fn trim_of_many_curves_far_from_the_origin_renders() {
    // The same curve and two thousand copies of it, which is within every limit. At half a
    // second each this frame took a quarter of an hour.
    let step = 2.0_f64.powi(47);
    let far = trimmed(json!([
        curve([1e30, 0.0], [0.0, step], [0.0, -step], [step, 0.0]),
        copies(2000),
        inner_trim(),
        stroke(100.0),
    ]));
    assert!(far.resolution >= RESOLVED, "{}", far.resolution);
    // One stroke draws the path and its copies.
    assert_eq!((far.drawn.len(), far.paths, far.measured), (1, 2001, 2001));
}

#[test]
fn trim_of_shapes_that_are_small_for_where_they_lie_renders() {
    let step = 2.0_f64.powi(47);
    let rounded_rectangle = json!({
        "ty": "rc",
        "p": fixed(json!([1e30, 0])),
        "s": fixed(json!([8.0 * step, 8.0 * step])),
        "r": fixed(2.0 * step),
    });
    let far_ellipse = json!({
        "ty": "el",
        "p": fixed(json!([1e30, 0])),
        "s": fixed(json!([2.0 * step, 2.0 * step])),
    });
    // Curves that are long in units and still short for their coordinates: tens of
    // thousands of float steps at 1e17, 1e18 and 1e20, where a curve took tens of
    // milliseconds to measure.
    let looped = |x: f64, size: f64| curve([x, 0.0], [0.0, size], [-size, size], [0.0, 0.0]);
    let s_curve = |x: f64, size: f64| {
        curve(
            [x, 0.0],
            [size / 2.0, size],
            [size / 2.0, -size],
            [size, 0.0],
        )
    };
    let shapes = [
        vec![rounded_rectangle],
        vec![far_ellipse],
        vec![looped(1e17, 16777216.0)],
        vec![s_curve(1e18, 8388608.0); 4],
        vec![s_curve(1e20, 4194304.0); 4],
        vec![looped(1e20, 67108864.0), s_curve(-1e25, 1e12)],
    ];
    for mut shapes in shapes {
        let paths = shapes.len();
        shapes.extend([inner_trim(), stroke(100.0)]);
        let far = trimmed(Value::Array(shapes.clone()));
        assert!(far.resolution >= RESOLVED, "{shapes:?}: {}", far.resolution);
        assert_eq!((far.drawn.len(), far.paths), (1, paths), "{shapes:?}");
        let bounds = far.drawn[0];
        assert!(bounds.is_finite() && bounds.area() > 0.0, "{bounds:?}");
    }
}

#[test]
fn shape_checks_follow_what_is_animated() {
    let check = |shapes: Value| import_shapes(shapes).unwrap().layers[0].shape_check;
    let never = |check| matches!(check, ShapeCheck::Never(_));
    assert!(never(check(json!([
        rect(0.0, 0.0, 10.0, 10.0),
        fill(100.0)
    ]))));
    assert!(never(check(json!([path(4), copies(3), wrapping_trim()]))));
    assert!(never(check(json!([polystar(1, fixed(5)), fill(100.0)]))));
    // An animated position or colour changes nothing about the count.
    let moving = with(
        rect(0.0, 0.0, 10.0, 10.0),
        json!({ "p": { "a": 1, "k": [{ "t": 0, "s": [0, 0] }, { "t": 60, "s": [50, 50] }] } }),
    );
    assert!(never(check(json!([moving, fill(100.0)]))));
    let size = json!({ "a": 1, "k": [{ "t": 0, "s": [10, 10] }, { "t": 60, "s": [50, 50] }] });
    let every_frame = |shapes| assert_eq!(check(shapes), ShapeCheck::EveryFrame);
    // An animated size may not be finite at some frame.
    every_frame(json!([ellipse(size.clone())]));
    let resized = with(rect(0.0, 0.0, 10.0, 10.0), json!({ "s": size }));
    every_frame(json!([group(vec![resized], 100.0)]));
    every_frame(json!([polystar(1, animated(5.0, 8.0))]));
    every_frame(json!([
        path(2),
        with(
            repeater(1, 1.0, 100.0, 100.0),
            json!({ "c": animated(1.0, 3.0) })
        ),
    ]));
    // A path whose keyframes differ in their number of vertices.
    let vertices = |count: usize| {
        let Value::Object(mut path) = path(count) else {
            unreachable!()
        };
        path.remove("ks").unwrap()["k"].take()
    };
    let morphing = json!({
        "ty": "sh",
        "ks": { "a": 1, "k": [{ "t": 0, "s": [vertices(3)] }, { "t": 60, "s": [vertices(5)] }] },
    });
    every_frame(json!([morphing]));
    let steady = json!({
        "ty": "sh",
        "ks": { "a": 1, "k": [{ "t": 0, "s": [vertices(4)] }, { "t": 60, "s": [vertices(4)] }] },
    });
    assert!(never(check(json!([steady]))));
}

#[test]
fn layer_made_in_code_is_counted_at_each_frame() {
    let composition = import_shapes(json!([path(4), fill(100.0)])).unwrap();
    let shapes = shapes_of(&composition.layers[0]).to_vec();
    let mut made = composition.clone();
    made.layers[0] = Layer {
        frames: 0.0..60.0,
        content: Content::Shape(shapes),
        ..Layer::default()
    };
    assert_eq!(made.layers[0].shape_check, ShapeCheck::EveryFrame);
    let limits = |path_elements| Limits {
        path_elements,
        ..Limits::DEFAULT
    };
    assert_eq!(evaluation_limit(&made, 0.0, &limits(8)), None);
    assert_eq!(
        evaluation_limit(&made, 0.0, &limits(7)),
        Some(Limit::PathElements)
    );
}

#[test]
fn precompositions_and_images_are_looked_up_once_for_each_layer() {
    use super::evaluate::LOOKUPS;
    // Thirty layers that each show a precomposition of twenty layers, which each show a
    // precomposition of an image layer and a precomposition layer: 1,800 layers at a frame.
    // Finding a precomposition or an image by its identifier takes time in proportion to the
    // length of the identifier, so it is done once for each layer that is written in the
    // file, and not once for each time that the layer is shown.
    let image = with(layer(2, 1), json!({ "refId": "image" }));
    let assets = json!([
        precomp("outer", (1..=20).map(|ind| precomp_layer(ind, "inner")).collect()),
        precomp("inner", json!([image, precomp_layer(2, "empty")])),
        precomp("empty", json!([])),
        { "id": "image", "w": 10, "h": 10, "u": "", "p": "image.png" },
    ]);
    let layers: Value = (1..=30).map(|ind| precomp_layer(ind, "outer")).collect();
    LOOKUPS.set(0);
    let composition = import(assets, layers).unwrap();
    // Import goes through the precompositions as rendering does, and finds no images.
    let written = 30 + 20 + 1;
    assert_eq!(LOOKUPS.get(), written);
    LOOKUPS.set(0);
    let evaluated = composition.evaluate(0.0).unwrap();
    assert_eq!(LOOKUPS.get(), written + 1);
    assert_eq!(evaluated.layers.len(), 30);
    assert_eq!(evaluated.layers[29].children[19].children.len(), 2);
    // Rendering draws the image six hundred times, and looks nothing up.
    LOOKUPS.set(0);
    let mut sink = RecordingSink::default();
    Renderer::new().append_evaluated(&evaluated, Affine::IDENTITY, 1.0, &mut sink);
    let images = sink
        .events
        .iter()
        .filter(|event| matches!(event, Event::Image { .. }))
        .count();
    assert_eq!((images, LOOKUPS.get()), (600, 0));

    // A precomposition that holds itself and one that is missing are still told apart.
    let cyclic = import(
        json!([
            precomp("a", json!([precomp_layer(1, "b")])),
            precomp("b", json!([precomp_layer(1, "a")]))
        ]),
        json!([precomp_layer(1, "a")]),
    )
    .unwrap();
    assert_eq!(
        cyclic.evaluate(0.0).unwrap_err(),
        EvaluationError::PrecompCycle("a".to_owned())
    );
    let missing = import(json!([]), json!([precomp_layer(1, "nowhere")])).unwrap();
    assert_eq!(
        missing.evaluate(0.0).unwrap_err(),
        EvaluationError::MissingAsset("nowhere".to_owned())
    );
}

#[test]
fn composition_made_in_code_is_held_to_the_limits_on_single_shapes() {
    use super::model::{Brush, ColorStops, Repeater, Shape, animated, fixed};
    // Import checks the gradients of a file as they are written, before it converts them. A
    // composition that is put together in code has none of that, so the same limits are
    // checked on the model: by the evaluation of a frame, and by the check that import ends
    // with.
    let over_limit = |change: &dyn Fn(&mut Shape), shapes: Value| {
        let mut made = import_shapes(shapes).unwrap();
        assert!(made.evaluate(0.0).is_ok());
        let Content::Shape(shapes) = &mut made.layers[0].content else {
            panic!("a shape layer has shapes");
        };
        shapes.iter_mut().for_each(change);
        // Nothing is known about a layer that code has made.
        made.layers[0].shape_check = ShapeCheck::EveryFrame;
        let error = render_error(&made, 0.0);
        assert_eq!(made.evaluate(0.0).unwrap_err(), error);
        let EvaluationError::LimitExceeded { limit, .. } = error else {
            panic!("{error}");
        };
        assert_eq!(
            import_limit(made.check_limits(&Limits::DEFAULT).map(|()| made)),
            limit
        );
        limit
    };
    let most = MAX_GRADIENT_STOPS;
    let gradient = |shapes: Value| json!([rect(0.0, 0.0, 10.0, 10.0), shapes]);

    // A gradient of the model has colour stops only: import has folded the transparency
    // stops of the file into them. One stop more than the most, on a gradient at the limit.
    let one_more_stop = |shape: &mut Shape| {
        if let Shape::Draw(draw) = shape {
            let Brush::Fixed(fixed::Brush::Gradient(gradient)) = &mut draw.brush else {
                panic!("the gradient is fixed");
            };
            assert_eq!(gradient.stops.len(), most);
            let last = gradient.stops[most - 1];
            gradient.stops.push(last);
        }
    };
    let at_limit = gradient(gradient_with_stops(most, most, most));
    assert_eq!(over_limit(&one_more_stop, at_limit), Limit::GradientStops);
    // Next to a number that is animated, the shapes are not counted when they are checked
    // without a frame, and the gradient is still found.
    let next_to_animated_copies = json!([
        rect(0.0, 0.0, 10.0, 10.0),
        gradient_with_stops(most, most, most),
        with(
            repeater(1, 1.0, 100.0, 100.0),
            json!({ "c": animated(1.0, 3.0) })
        ),
    ]);
    assert_eq!(
        over_limit(&one_more_stop, next_to_animated_copies),
        Limit::GradientStops
    );

    // The same for a gradient with keyframes, which holds five numbers for each stop.
    let animated_stops = |shape: &mut Shape| {
        if let Shape::Draw(draw) = shape {
            let Brush::Animated(animated::Brush::Gradient(gradient)) = &mut draw.brush else {
                panic!("the gradient is animated");
            };
            let ColorStops::Animated(stops) = &mut gradient.stops else {
                panic!("the stops are animated");
            };
            assert_eq!(stops.count, most);
            stops.count += 1;
            for values in &mut stops.values {
                values.extend([1.0, 0.0, 0.0, 0.0, 1.0]);
            }
        }
    };
    let mut keyframed = gradient_with_stops(most, most, most);
    let values = keyframed["g"]["k"]["k"].clone();
    keyframed["g"]["k"] =
        json!({ "a": 1, "k": [{ "t": 0, "s": values }, { "t": 60, "s": values }] });
    assert_eq!(
        over_limit(&animated_stops, gradient(keyframed)),
        Limit::GradientStops
    );

    // A file with as many transparency stops as there may be, and two colour stops, makes a
    // gradient of two stops.
    let transparent = import_shapes(gradient(gradient_with_stops(2, most, 2))).unwrap();
    let stops: Vec<_> = shapes_of(&transparent.layers[0])
        .iter()
        .filter_map(|shape| match shape {
            Shape::Draw(draw) => Some(draw.brush.max_color_stops()),
            _ => None,
        })
        .collect();
    assert_eq!(stops, [2]);

    // Copies of a repeater and points of a star.
    let more_copies = |shape: &mut Shape| {
        if let Shape::Repeater(Repeater::Fixed(repeater)) = shape {
            repeater.copies = MAX_REPEATER_COPIES + 1;
        }
    };
    let repeated = json!([path(2), stroke(100.0), copies(MAX_REPEATER_COPIES)]);
    assert_eq!(over_limit(&more_copies, repeated), Limit::RepeaterCopies);
    let more_points = |shape: &mut Shape| {
        if let Shape::Geometry(Geometry::Star(star)) = shape {
            star.points = super::model::Value::Fixed(MAX_POLYSTAR_POINTS as f64 + 1.0);
        }
    };
    let star = json!([polystar(1, fixed(MAX_POLYSTAR_POINTS)), fill(100.0)]);
    assert_eq!(over_limit(&more_points, star), Limit::PolystarPoints);
}

#[test]
fn trimmed_elements_count_for_each_trim_that_applies_to_them() {
    let trimmed = |shapes: Value| {
        let composition = import_shapes(shapes).unwrap();
        count(&composition, 0.0, &Limits::DEFAULT).unwrap().trimmed
    };
    assert_eq!(trimmed(json!([path(10), fill(100.0)])), 0);
    assert_eq!(trimmed(json!([path(10), wrapping_trim()])), 10);
    // The second trim applies to both parts that the first may have cut the path into.
    assert_eq!(
        trimmed(json!([path(10), wrapping_trim(), wrapping_trim()])),
        30
    );
    // A trim applies to the paths of its group, and to their copies.
    let inner = || group(vec![path(4), wrapping_trim()], 100.0);
    assert_eq!(trimmed(json!([path(10), inner()])), 4);
    assert_eq!(
        trimmed(json!([inner(), path(10), wrapping_trim()])),
        4 + 8 + 10
    );
    assert_eq!(trimmed(json!([path(10), copies(4), wrapping_trim()])), 50);
    assert_eq!(trimmed(json!([path(10), wrapping_trim(), copies(4)])), 10);
}

#[test]
fn trimmed_elements_at_the_limit_import_and_one_more_does_not() {
    // 2,000 paths under one trim.
    let shapes = |vertices| json!([path(vertices), copies(1999), wrapping_trim(), stroke(100.0)]);
    let at_limit = import_shapes(shapes(15)).unwrap();
    assert_eq!(
        count(&at_limit, 0.0, &Limits::DEFAULT).unwrap().trimmed,
        MAX_TRIMMED_ELEMENTS
    );
    assert_eq!(trace(&at_limit, 0.0), ["clip", "stroke(1)", "pop"]);
    assert_eq!(
        import_limit(import_shapes(shapes(16))),
        Limit::TrimmedElements
    );
}

#[test]
fn trimmed_elements_of_animated_copies_are_limited_at_the_frame() {
    let animated_copies = with(
        repeater(1, 1.0, 100.0, 100.0),
        json!({ "c": animated(1.0, 2001.0) }),
    );
    let shapes = json!([path(30), animated_copies, wrapping_trim(), stroke(100.0)]);
    let composition = import_shapes(shapes).unwrap();
    // The path and 968 copies of it.
    assert!(composition.evaluate(29.0).is_ok());
    // The path and 1,001 copies of it.
    assert_eq!(
        render_error(&composition, 30.0),
        exceeded(Limit::TrimmedElements)
    );
}

#[test]
fn many_trims_of_one_path_are_refused() {
    // Each trim measures what the trims before it left of the path, and each of them may
    // have cut every piece in two.
    let shapes = |trims: usize| {
        let mut shapes = vec![path(500)];
        shapes.extend(std::iter::repeat_n(
            json!({ "ty": "tm", "s": fixed(1), "e": fixed(99), "o": fixed(0), "m": 1 }),
            trims,
        ));
        shapes.push(stroke(100.0));
        json!(shapes)
    };
    for trims in [100, 2700] {
        assert_eq!(
            import_limit(import_shapes(shapes(trims))),
            Limit::TrimmedElements,
            "{trims} trims"
        );
    }
    let few = import_shapes(shapes(5)).unwrap();
    assert_eq!(
        count(&few, 0.0, &Limits::DEFAULT).unwrap().trimmed,
        500 * 31
    );
    assert_eq!(trace(&few, 0.0), ["clip", "stroke(1)", "pop"]);
}

/// A gradient fill with the given numbers of colour stops and transparency stops, which
/// declares `declared` colour stops.
fn gradient_with_stops(color: usize, transparency: usize, declared: usize) -> Value {
    let mut values = Vec::new();
    for stop in 0..color {
        values.extend([stop as f64 / color.max(2) as f64, 1.0, 0.0, 0.0]);
    }
    for stop in 0..transparency {
        values.extend([stop as f64 / transparency.max(2) as f64, 1.0]);
    }
    with(
        gradient("gf", 100.0),
        json!({ "g": { "p": declared, "k": fixed(values) } }),
    )
}

#[test]
fn gradient_stops_are_limited_at_import() {
    let shapes = |gradient: Value| json!([rect(0.0, 0.0, 10.0, 10.0), gradient]);
    let most = MAX_GRADIENT_STOPS;
    let at_limit = import_shapes(shapes(gradient_with_stops(most, most, most))).unwrap();
    assert_eq!(trace(&at_limit, 0.0), ["clip", "fill(1)", "pop"]);
    // 256 stops of the brush and four sides of the rectangle, which the fill draws.
    let cost = count(&at_limit, 0.0, &Limits::DEFAULT).unwrap();
    let sides = cost.elements - most;
    assert!(sides > 0 && sides.is_multiple_of(2), "{cost:?}");

    for (color, transparency) in [(most + 1, 0), (2, most + 1), (30_000, 30_000)] {
        let gradient = gradient_with_stops(color, transparency, color);
        assert_eq!(
            import_limit(import_shapes(shapes(gradient.clone()))),
            Limit::GradientStops,
            "{color} colour stops and {transparency} transparency stops"
        );
        // In a group of a layer of a precomposition, and in a keyframe.
        let layer = shape_layer(
            1,
            json!([group(
                vec![rect(0.0, 0.0, 10.0, 10.0), gradient.clone()],
                100.0
            )]),
        );
        let nested = import(
            json!([precomp("comp", json!([layer]))]),
            json!([precomp_layer(1, "comp")]),
        );
        assert_eq!(import_limit(nested), Limit::GradientStops);
        let few = gradient_with_stops(2, 0, 2);
        let mut animated = gradient.clone();
        animated["g"]["k"] = json!({
            "a": 1,
            "k": [
                { "t": 0, "s": few["g"]["k"]["k"] },
                { "t": 60, "s": gradient["g"]["k"]["k"] },
            ],
        });
        assert_eq!(
            import_limit(import_shapes(shapes(animated))),
            Limit::GradientStops
        );
    }

    // A gradient that declares more colour stops than it has is counted by the ones it has.
    let overstated = gradient_with_stops(2, 0, 1_000_000_000);
    assert!(import_shapes(shapes(overstated)).is_ok());
}

#[test]
fn gradient_stops_count_for_each_copy_of_a_style() {
    let composition = import_shapes(json!([
        path(2),
        gradient_with_stops(100, 0, 100),
        copies(50),
    ]))
    .unwrap();
    let cost = count(&composition, 0.0, &Limits::DEFAULT).unwrap();
    // 51 paths of two elements, and 50 fills that each draw one with a hundred stops.
    assert_eq!(cost.elements, 51 * 2 + 50 * (2 + 100));
}

#[test]
fn path_without_keyframes_counts_for_each_copy() {
    // A path that is animated and has no keyframes has no elements. A repeater copies it all
    // the same, so it counts like a path of one element.
    let empty = || json!({ "ty": "sh", "ks": { "a": 1, "k": [] } });
    let composition = import_shapes(json!([empty(), empty(), copies(256)])).unwrap();
    let cost = count(&composition, 0.0, &Limits::DEFAULT).unwrap();
    assert_eq!(cost.instances, 1 + 2 * 257);
    assert_eq!(cost.elements, 2 * 257);
    assert_eq!(trace(&composition, 0.0), ["clip", "pop"]);

    // 200 layers that each make 257 copies of 2 such paths, and 100 that do not make one more.
    let layer = |ind| shape_layer(ind, json!([empty(), empty(), copies(256)]));
    let layers = |count: u32| import(json!([]), json!((0..count).map(layer).collect::<Vec<_>>()));
    let within = layers(MAX_SHAPE_INSTANCES as u32 / 515).unwrap();
    assert!(within.evaluate(0.0).is_ok());
    let over = layers(MAX_SHAPE_INSTANCES as u32 / 515 + 1).unwrap();
    assert_eq!(render_error(&over, 0.0), exceeded(Limit::ShapeInstances));
}
