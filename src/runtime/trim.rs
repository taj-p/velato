// Copyright 2026 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use kurbo::{
    Affine, CubicBez, Line, ParamCurve, ParamCurveArclen, PathEl, PathSeg, Point, QuadBez, segments,
};
use std::ops::RangeInclusive;

const ACCURACY: f64 = 0.1;
// A segment is measured to `ACCURACY`, or to this fraction of the length of its control polygon
// if that is larger. Measuring to a fixed accuracy takes ever more subdivisions as a curve gets
// longer, so the cost of a trim would grow with the coordinates. Segments shorter than 1e8 units
// are measured to `ACCURACY`.
const RELATIVE_ACCURACY: f64 = 1e-9;
// kurbo squares coordinates when it measures a curve, which overflows above about 1e154 and
// keeps it from converging. Paths with larger coordinates than this are measured scaled down.
const MAX_MAGNITUDE: f64 = 1e100;

// kurbo also squares the differences between the points of a curve, and halves a curve up to
// twenty times while it measures it. Below this range the squares underflow to zero, with the
// same result as above it: kurbo subdivides every curve as far as it can. A segment whose
// extent is outside this range is measured scaled into it.
const MEASURABLE_EXTENT: RangeInclusive<f64> = 1e-100..=1e100;

// kurbo halves a curve until the error it estimates for each part is below the accuracy asked
// for. The points of the parts are rounded to the floats at their coordinates, and once that
// rounding is of the size of the accuracy the estimate no longer falls, so that kurbo halves
// every part as far as it can: a million parts for one curve, and again for each step of
// finding the parameter at a length. That is the case for a segment that lies far from the
// origin compared to its length, where floats are spaced widely for a segment of its size.
//
// A segment is therefore measured where it lies only if floats at its coordinates are spaced
// this many times finer than the accuracy, which is 2^24. Otherwise a copy of it that starts at the origin is
// measured, whose coordinates are no larger than the segment is long.
const RESOLUTION: f64 = 16_777_216.0;

/// Returns a copy of the segment that kurbo can measure, and the factor that it was scaled by.
///
/// The copy is the segment itself unless its extent is outside of what kurbo can measure. Then
/// it is the segment moved to start at the origin and scaled by a power of two. Its curve
/// parameters are those of the segment, and its lengths are those of the segment times the
/// factor.
pub(crate) fn measurable(segment: PathSeg) -> (PathSeg, f64) {
    let origin = segment.start();
    // Halved, so that the difference between two coordinates of opposite sign stays finite.
    let half = |point: Point| {
        (point.x * 0.5 - origin.x * 0.5)
            .abs()
            .max((point.y * 0.5 - origin.y * 0.5).abs())
    };
    let half_extent = match segment {
        PathSeg::Line(line) => half(line.p1),
        PathSeg::Quad(quad) => half(quad.p1).max(half(quad.p2)),
        PathSeg::Cubic(cubic) => half(cubic.p1).max(half(cubic.p2)).max(half(cubic.p3)),
    };
    let extent = 2.0 * half_extent;
    if half_extent == 0.0 || !half_extent.is_finite() || MEASURABLE_EXTENT.contains(&extent) {
        return (segment, 1.0);
    }
    // Powers of two up to 2^1000 and down to 2^-1000 are normal floats.
    let exponent = (-half_extent.log2().ceil()).clamp(-1000.0, 1000.0);
    let scale = 2.0_f64.powi(exponent as i32);
    // Scaling down comes before moving, and scaling up after it, so that neither overflows.
    let (before, after) = if scale < 1.0 {
        (scale, 1.0)
    } else {
        (1.0, scale)
    };
    let fit = |point: Point| {
        Point::new(
            (point.x * before - origin.x * before) * after,
            (point.y * before - origin.y * before) * after,
        )
    };
    let fitted = match segment {
        PathSeg::Line(line) => PathSeg::Line(Line::new(fit(line.p0), fit(line.p1))),
        PathSeg::Quad(quad) => {
            PathSeg::Quad(QuadBez::new(fit(quad.p0), fit(quad.p1), fit(quad.p2)))
        }
        PathSeg::Cubic(cubic) => PathSeg::Cubic(CubicBez::new(
            fit(cubic.p0),
            fit(cubic.p1),
            fit(cubic.p2),
            fit(cubic.p3),
        )),
    };
    (fitted, scale)
}

/// Returns the largest size of a coordinate of the segment.
fn magnitude(segment: &PathSeg) -> f64 {
    let size = |point: Point| point.x.abs().max(point.y.abs());
    match segment {
        PathSeg::Line(line) => size(line.p0).max(size(line.p1)),
        PathSeg::Quad(quad) => size(quad.p0).max(size(quad.p1)).max(size(quad.p2)),
        PathSeg::Cubic(cubic) => size(cubic.p0)
            .max(size(cubic.p1))
            .max(size(cubic.p2))
            .max(size(cubic.p3)),
    }
}

/// Returns an upper bound of the distance between neighbouring floats at the coordinates of
/// the segment.
fn spacing(segment: &PathSeg) -> f64 {
    magnitude(segment) * f64::EPSILON
}

/// Returns a copy of the segment that starts at the origin.
///
/// The differences between the coordinates of a segment are rounded to a part in 2^53 of
/// their own size, however large the coordinates are, so the copy has the shape of the
/// segment as precisely as floats can hold it.
fn at_origin(segment: PathSeg) -> PathSeg {
    let origin = segment.start().to_vec2();
    match segment {
        PathSeg::Line(line) => PathSeg::Line(Line::new(line.p0 - origin, line.p1 - origin)),
        PathSeg::Quad(quad) => PathSeg::Quad(QuadBez::new(
            quad.p0 - origin,
            quad.p1 - origin,
            quad.p2 - origin,
        )),
        PathSeg::Cubic(cubic) => PathSeg::Cubic(CubicBez::new(
            cubic.p0 - origin,
            cubic.p1 - origin,
            cubic.p2 - origin,
            cubic.p3 - origin,
        )),
    }
}

/// A copy of a segment to measure in its place.
struct Fitted {
    /// The copy. Its curve parameters are those of the segment.
    segment: PathSeg,
    /// The factor that the lengths of the copy are larger by than those of the segment.
    fit: f64,
    /// The accuracy to measure the copy with.
    accuracy: f64,
}

impl Fitted {
    /// Returns the copy to measure of a segment of a path that was scaled by `scale`.
    ///
    /// The path has no coordinate above `MAX_MAGNITUDE`, which keeps the differences between
    /// the coordinates of its segments finite.
    fn new(segment: PathSeg, scale: f64) -> Self {
        let (fitted, fit) = measurable(segment);
        let scale = scale * fit;
        let in_place = accuracy(&fitted, scale);
        let (segment, accuracy) = if spacing(&fitted) * RESOLUTION <= in_place {
            (fitted, in_place)
        } else {
            // The accuracy is no finer than `RELATIVE_ACCURACY` of the length of the control
            // polygon, and the copy has no coordinate larger than that length, so its floats
            // are spaced four million times finer than the accuracy.
            let moved = at_origin(fitted);
            (moved, accuracy(&moved, scale))
        };
        #[cfg(test)]
        RESOLUTION_MEASURED.with(|least| {
            least.set(least.get().min(accuracy / spacing(&segment)));
        });
        Self {
            segment,
            fit,
            accuracy,
        }
    }
}

/// Returns the length of a segment of a path that was scaled by `scale`.
fn segment_length(segment: PathSeg, scale: f64) -> f64 {
    let fitted = Fitted::new(segment, scale);
    fitted.segment.arclen(fitted.accuracy) / fitted.fit
}

/// Returns the parameter at which a segment of a path that was scaled by `scale` reaches the
/// given length.
fn segment_parameter(segment: PathSeg, length: f64, scale: f64) -> f64 {
    let fitted = Fitted::new(segment, scale);
    fitted
        .segment
        .inv_arclen(length * fitted.fit, fitted.accuracy)
}

/// Returns the accuracy to measure a segment with, for a path that was scaled by `scale`.
fn accuracy(segment: &PathSeg, scale: f64) -> f64 {
    let control_length = match segment {
        PathSeg::Line(line) => (line.p1 - line.p0).hypot(),
        PathSeg::Quad(quad) => (quad.p1 - quad.p0).hypot() + (quad.p2 - quad.p1).hypot(),
        PathSeg::Cubic(cubic) => {
            (cubic.p1 - cubic.p0).hypot()
                + (cubic.p2 - cubic.p1).hypot()
                + (cubic.p3 - cubic.p2).hypot()
        }
    };
    // An accuracy of zero cannot be met, and makes kurbo subdivide as far as it can.
    (ACCURACY * scale)
        .max(control_length * RELATIVE_ACCURACY)
        .max(f64::MIN_POSITIVE)
}

/// Returns the factor to scale a path by before measuring it: 1 for all but extreme coordinates,
/// and a power of two that brings them near 1 otherwise. Returns `None` if a coordinate is not
/// finite, in which case the path has no length to measure.
fn measuring_scale(elements: &[PathEl]) -> Option<f64> {
    let mut magnitude: f64 = 0.0;
    let mut point =
        |point: kurbo::Point| magnitude = magnitude.max(point.x.abs()).max(point.y.abs());
    let mut finite = true;
    for element in elements {
        finite &= match *element {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => {
                point(p);
                p.is_finite()
            }
            PathEl::QuadTo(p1, p2) => {
                point(p1);
                point(p2);
                p1.is_finite() && p2.is_finite()
            }
            PathEl::CurveTo(p1, p2, p3) => {
                point(p1);
                point(p2);
                point(p3);
                p1.is_finite() && p2.is_finite() && p3.is_finite()
            }
            PathEl::ClosePath => true,
        };
    }
    if !finite {
        None
    } else if magnitude > MAX_MAGNITUDE {
        // A power of two below 2^-1022 is not a normal float, and comes out as zero.
        let exponent = (magnitude.log2().ceil() as i32).min(1022);
        Some(2.0_f64.powi(-exponent))
    } else {
        Some(1.0)
    }
}

/// Splits a path into its contours. A contour ends before a move, and after a close.
fn contours(elements: &[PathEl]) -> impl Iterator<Item = &[PathEl]> {
    let mut rest = elements;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = (1..rest.len())
            .find(|&end| {
                matches!(rest[end], PathEl::MoveTo(_)) || matches!(rest[end - 1], PathEl::ClosePath)
            })
            .unwrap_or(rest.len());
        let (contour, tail) = rest.split_at(end);
        rest = tail;
        Some(contour)
    })
}

/// Returns the segments of a contour.
fn contour_segments(contour: &[PathEl]) -> impl Iterator<Item = PathSeg> + '_ {
    // A contour cannot start with a close, and has no segments if it does.
    let contour = match contour.first() {
        Some(PathEl::ClosePath) => &[],
        _ => contour,
    };
    segments(contour.iter().copied())
}

/// Returns a copy of a path scaled by `scale`, or the path itself if that is 1.
fn scaled(elements: &[PathEl], scale: f64) -> std::borrow::Cow<'_, [PathEl]> {
    if scale == 1.0 {
        elements.into()
    } else {
        elements
            .iter()
            .map(|element| Affine::scale(scale) * *element)
            .collect()
    }
}

#[cfg(test)]
thread_local! {
    /// Number of segments measured on this thread.
    pub(crate) static SEGMENTS_MEASURED: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// The least number of times that the accuracy of a measurement on this thread was larger
    /// than the spacing of floats at the coordinates of what was measured.
    pub(crate) static RESOLUTION_MEASURED: std::cell::Cell<f64> =
        const { std::cell::Cell::new(f64::INFINITY) };
}

/// The lengths of the segments of a path.
///
/// Measuring is what takes the time in trimming, so a path is measured once, and trimmed with
/// what was measured.
#[derive(Clone, Debug)]
pub(crate) struct Measured {
    /// The factor that the path is scaled by for measuring and trimming it: 1 for all but
    /// extreme coordinates. `None` if the path has a coordinate that is not finite, and so has
    /// no length to measure.
    scale: Option<f64>,
    /// The length of each segment of the scaled path, contour by contour.
    lengths: Vec<f64>,
}

impl Measured {
    pub(crate) fn new(elements: &[PathEl]) -> Self {
        let scale = measuring_scale(elements);
        let mut lengths = Vec::new();
        if let Some(scale) = scale {
            let elements = scaled(elements, scale);
            for contour in contours(&elements) {
                lengths.extend(contour_segments(contour).map(|segment| {
                    #[cfg(test)]
                    SEGMENTS_MEASURED.with(|count| count.set(count.get() + 1));
                    segment_length(segment, scale)
                }));
            }
        }
        Self { scale, lengths }
    }

    /// Returns the length of the path, or zero if it has coordinates that are not finite.
    pub(crate) fn length(&self) -> f64 {
        self.scale
            .map_or(0.0, |scale| self.lengths.iter().sum::<f64>() / scale)
    }
}

/// Returns the length of a path, or zero if it has coordinates that are not finite.
#[cfg(test)]
pub(crate) fn length(elements: &[PathEl]) -> f64 {
    Measured::new(elements).length()
}

/// Appends the parts of a path that lie within `ranges` to `output`. `measured` holds the
/// lengths of the path.
///
/// A path with coordinates that are not finite cannot be measured and is appended untrimmed.
pub(crate) fn trim(
    elements: &[PathEl],
    measured: &Measured,
    ranges: &[(f64, f64)],
    sequential: Option<(f64, f64)>,
    output: &mut Vec<PathEl>,
) {
    let Some(scale) = measured.scale else {
        output.extend_from_slice(elements);
        return;
    };
    let sequential = sequential.map(|(offset, total)| (offset * scale, total * scale));
    let start = output.len();
    trim_scaled(
        &scaled(elements, scale),
        scale,
        &measured.lengths,
        ranges,
        sequential,
        output,
    );
    if scale != 1.0 {
        for element in &mut output[start..] {
            *element = Affine::scale(1.0 / scale) * *element;
        }
    }
}

fn trim_scaled(
    elements: &[PathEl],
    scale: f64,
    lengths: &[f64],
    ranges: &[(f64, f64)],
    sequential: Option<(f64, f64)>,
    output: &mut Vec<PathEl>,
) {
    let mut offset = sequential.map_or(0.0, |(offset, _)| offset);
    let mut lengths = lengths;
    for contour in contours(elements) {
        let segment_count = contour_segments(contour).count().min(lengths.len());
        let (contour_lengths, rest) = lengths.split_at(segment_count);
        lengths = rest;
        let contour_length: f64 = contour_lengths.iter().sum();
        let (base, total) = sequential.map_or((0.0, contour_length), |(_, total)| (offset, total));
        let intervals: Vec<_> = ranges
            .iter()
            .map(|(a, b)| {
                (
                    (a * total - base).max(0.0),
                    (b * total - base).min(contour_length),
                )
            })
            .filter(|(a, b)| b > a)
            .collect();
        if intervals
            .iter()
            .any(|(a, b)| *a == 0.0 && *b == contour_length)
        {
            output.extend_from_slice(contour);
        } else if !intervals.is_empty() {
            trim_contour(
                contour,
                scale,
                contour_lengths,
                &intervals,
                contour_length,
                output,
            );
        }
        offset += contour_length;
    }
}

fn trim_contour(
    elements: &[PathEl],
    scale: f64,
    lengths: &[f64],
    intervals: &[(f64, f64)],
    length: f64,
    output: &mut Vec<PathEl>,
) {
    let closed = matches!(elements.last(), Some(PathEl::ClosePath));
    let mut previous_end = None;
    for &(start, end) in intervals {
        let mut need_move = !(closed && previous_end == Some(length) && start == 0.0);
        let mut position = 0.0;
        for (segment, &segment_length) in contour_segments(elements).zip(lengths) {
            if position >= end {
                break;
            }
            let next = position + segment_length;
            if segment_length > 0.0 && next > start && position < end {
                let t0 = if start <= position {
                    0.0
                } else {
                    segment_parameter(segment, start - position, scale).clamp(0.0, 1.0)
                };
                let t1 = if end >= next {
                    1.0
                } else {
                    segment_parameter(segment, end - position, scale).clamp(0.0, 1.0)
                };
                if t1 > t0 {
                    let sub = segment.subsegment(t0..t1);
                    if need_move {
                        output.push(PathEl::MoveTo(sub.start()));
                        need_move = false;
                    }
                    output.push(sub.as_path_el());
                }
            }
            position = next;
        }
        previous_end = Some(end);
    }
}

#[cfg(test)]
mod tests {
    use super::{Fitted, Measured, RESOLUTION_MEASURED, length, magnitude, trim};
    use kurbo::{
        CubicBez, Line, ParamCurve as _, PathEl, PathSeg, Point, QuadBez, Shape as _, Vec2,
    };
    use std::time::{Duration, Instant};

    /// Floats at the coordinates of everything that is measured have to be spaced this many
    /// times finer than the accuracy it is measured to. Then rounding cannot keep kurbo from
    /// reaching the accuracy, and measuring a curve takes what it takes at the origin.
    const RESOLVED: f64 = 4e6;

    /// Trims a path in a few ways, and returns the least number of times that floats were
    /// spaced finer than the accuracy of a measurement while it did, along with the pieces.
    fn trimmed_resolution(path: &[PathEl]) -> (f64, Vec<Vec<PathEl>>) {
        RESOLUTION_MEASURED.set(f64::INFINITY);
        let measured = Measured::new(path);
        let total = measured.length();
        let mut pieces = Vec::new();
        let mut cut = |ranges: &[(f64, f64)], sequential: Option<(f64, f64)>| {
            let mut output = Vec::new();
            trim(path, &measured, ranges, sequential, &mut output);
            pieces.push(output);
        };
        cut(&[(0.1, 0.9)], None);
        // A range that an offset wraps around the end of the path.
        cut(&[(0.0, 0.25), (0.75, 1.0)], None);
        // The path as the second of three of its length that a trim takes in turn.
        cut(&[(0.2, 0.6)], Some((total, 3.0 * total)));
        cut(&[(0.0, 0.4), (0.9, 1.0)], Some((total, 3.0 * total)));
        (RESOLUTION_MEASURED.get(), pieces)
    }

    fn is_finite(path: &[PathEl]) -> bool {
        path.iter().all(|element| match *element {
            PathEl::MoveTo(p) | PathEl::LineTo(p) => p.is_finite(),
            PathEl::QuadTo(p1, p2) => p1.is_finite() && p2.is_finite(),
            PathEl::CurveTo(p1, p2, p3) => p1.is_finite() && p2.is_finite() && p3.is_finite(),
            PathEl::ClosePath => true,
        })
    }

    /// Cubics that take the most subdivisions to measure, in a unit square: an S, a cusp, one
    /// that runs out and back along a line, a loop, and one with handles far longer than it.
    const CUBICS: [[(f64, f64); 4]; 5] = [
        [(0.0, 0.0), (0.5, 1.0), (0.5, -1.0), (1.0, 0.0)],
        [(0.0, 0.0), (1.0, 1.0), (0.0, 1.0), (1.0, 0.0)],
        [(0.0, 0.0), (1.0, 0.0), (-1.0, 0.0), (0.0, 0.0)],
        [(0.0, 0.0), (1.0, 1.0), (-1.0, 1.0), (0.0, 0.0)],
        [(0.0, 0.0), (1000.0, 1.0), (-1000.0, 1.0), (1.0, 0.0)],
    ];

    /// An open path of one cubic of the given size that starts at `origin`.
    fn cubic_at(shape: &[(f64, f64); 4], origin: Point, size: f64) -> Vec<PathEl> {
        let [p0, p1, p2, p3] = shape.map(|(x, y)| origin + Vec2::new(x, y) * size);
        vec![PathEl::MoveTo(p0), PathEl::CurveTo(p1, p2, p3)]
    }

    /// An open path of `count` S-shaped curves, each `size` wide, that runs along the X axis.
    fn curvy(count: usize, size: f64) -> Vec<PathEl> {
        let mut path = vec![PathEl::MoveTo(Point::new(0.0, 0.0))];
        for index in 0..count {
            let x = index as f64 * size;
            path.push(PathEl::CurveTo(
                Point::new(x + size * 0.5, size),
                Point::new(x + size * 0.5, -size),
                Point::new(x + size, 0.0),
            ));
        }
        path
    }

    fn trimmed_half(path: &[PathEl]) -> Vec<PathEl> {
        let mut output = Vec::new();
        trim(path, &Measured::new(path), &[(0.0, 0.5)], None, &mut output);
        output
    }

    #[test]
    fn measuring_takes_the_same_effort_at_any_scale() {
        // The reference is only measured to a tenth of a unit per curve.
        let reference = length(&curvy(8, 100.0)) / 100.0;
        // Measured to a fixed accuracy, curves this long need every subdivision kurbo allows,
        // which takes seconds for a handful of them.
        for size in [1e30, 1e150] {
            let path = curvy(8, size);
            let start = Instant::now();
            let relative = length(&path) / size;
            let half = trimmed_half(&path);
            assert!(start.elapsed() < Duration::from_secs(1), "size {size}");
            assert!(
                (relative - reference).abs() < 0.05,
                "size {size}: {relative}"
            );
            let end = half.as_slice().bounding_box().x1 / size;
            assert!((end - 4.0).abs() < 0.01, "size {size}: half ends at {end}");
        }
    }

    #[test]
    fn coordinates_whose_squares_overflow_are_measured_scaled_down() {
        let reference = length(&curvy(8, 100.0)) / 100.0;
        for size in [1e160, 1e300] {
            let path = curvy(8, size);
            let relative = length(&path) / size;
            assert!(
                (relative - reference).abs() < 0.05,
                "size {size}: {relative}"
            );
            let half = trimmed_half(&path);
            let end = half.as_slice().bounding_box().x1 / size;
            assert!((end - 4.0).abs() < 0.01, "size {size}: half ends at {end}");
        }
    }

    #[test]
    fn path_with_coordinates_that_are_far_apart_in_size_is_measured() {
        // The first curve reaches the largest coordinates a float can hold, and the others are
        // a hundred and fifty orders of magnitude smaller. Scaled so that the first can be
        // measured, the others are too small to measure without scaling them back up.
        let mut path = curvy(40, 1e150);
        path[1] = PathEl::CurveTo(
            Point::new(1e308, 1e150),
            Point::new(0.5e150, -1e150),
            Point::new(1e150, 0.0),
        );
        let start = Instant::now();
        let total = length(&path);
        let half = trimmed_half(&path);
        assert!(start.elapsed() < Duration::from_secs(1));
        // The first curve goes out to its far control point and back, and outweighs the rest.
        assert!(total > 0.5e308 && total < 1e308, "{total}");
        assert!(!half.is_empty() && half.len() <= 2, "{half:?}");

        // The same for a path that is small as a whole, and for one with a few such curves.
        // A length far below the accuracy of a tenth of a unit is only estimated.
        for size in [1e-150, 1e-300] {
            let reference = length(&curvy(8, 100.0)) / 100.0;
            let small = curvy(8, size);
            let start = Instant::now();
            let relative = length(&small) / size;
            let half = trimmed_half(&small);
            assert!(start.elapsed() < Duration::from_secs(1), "size {size}");
            assert!(
                (relative / reference - 1.0).abs() < 0.05,
                "size {size}: {relative} for {reference}"
            );
            assert!((5..=6).contains(&half.len()), "size {size}: {half:?}");

            // The small curves end next to the origin, where the others start.
            let mut mixed = curvy(8, size);
            mixed.extend(curvy(8, 100.0).into_iter().skip(1));
            let start = Instant::now();
            let mixed_length = length(&mixed);
            assert!(start.elapsed() < Duration::from_secs(1), "size {size}");
            assert!(
                (mixed_length / 100.0 - reference).abs() < 0.05,
                "size {size}"
            );
        }
    }

    #[test]
    fn path_with_a_coordinate_that_is_not_finite_is_left_untrimmed() {
        for bad in [f64::NAN, f64::INFINITY] {
            let mut path = curvy(4, 100.0);
            path[2] = PathEl::LineTo(Point::new(bad, 0.0));
            assert_eq!(length(&path), 0.0);
            // `PathEl` holds floats, so compare the debug forms to treat NaN as equal to itself.
            assert_eq!(format!("{:?}", trimmed_half(&path)), format!("{path:?}"));
        }
    }

    #[test]
    fn ordinary_paths_are_trimmed_to_the_same_accuracy_as_before() {
        let path = curvy(8, 100.0);
        let half = trimmed_half(&path);
        // Four of the eight curves, each measured to a tenth of a unit.
        assert_eq!(half.len(), 5);
        let end = half.as_slice().bounding_box().x1;
        assert!((end - 400.0).abs() < 0.5, "half ends at {end}");
    }

    #[test]
    fn segment_is_measured_where_it_lies_unless_floats_are_too_coarse_there() {
        let s_curve = |x: f64, size: f64| {
            PathSeg::Cubic(CubicBez::new(
                (x, 0.0),
                (x + size * 0.5, size),
                (x + size * 0.5, -size),
                (x + size, 0.0),
            ))
        };
        // At the coordinates of authored animations a segment is measured as it is, so its
        // trims come out as they always did.
        for x in [0.0, 1.0, 500.0, 1e5, 1e7] {
            for size in [1e-3, 1.0, 100.0, 1e5] {
                let segment = s_curve(x, size);
                let fitted = Fitted::new(segment, 1.0);
                assert_eq!(fitted.segment, segment, "at {x}, {size} long");
                assert_eq!(fitted.fit, 1.0);
                assert_eq!(fitted.accuracy, 0.1);
            }
        }
        // Far from the origin the copy starts at the origin and is no larger than the segment.
        for (x, size) in [(1e9, 1.0), (1e18, 8388608.0), (1e30, 2.0_f64.powi(47))] {
            let segment = s_curve(x, size);
            let fitted = Fitted::new(segment, 1.0);
            assert_eq!(fitted.segment.start(), Point::ORIGIN, "at {x}");
            assert!(magnitude(&fitted.segment) <= size, "at {x}");
            // It has the shape of the segment to the last bit that the segment has it to.
            let PathSeg::Cubic(cubic) = segment else {
                unreachable!()
            };
            let origin = cubic.p0.to_vec2();
            let expected = CubicBez::new(
                cubic.p0 - origin,
                cubic.p1 - origin,
                cubic.p2 - origin,
                cubic.p3 - origin,
            );
            assert_eq!(fitted.segment, PathSeg::Cubic(expected));
        }
    }

    #[test]
    fn curve_far_from_the_origin_is_trimmed_like_the_same_curve_at_the_origin() {
        // One float step wide at its coordinates. Measured where it lies, kurbo splits it into
        // a million parts, and again for each step of finding where the trim cuts it.
        let step = 2.0_f64.powi(47);
        let far = cubic_at(&CUBICS[0], Point::new(1e30, 0.0), step);
        // What floats hold of the curve: both handles round to the nearest end.
        let near = [
            PathEl::MoveTo(Point::new(0.0, 0.0)),
            PathEl::CurveTo(
                Point::new(0.0, step),
                Point::new(0.0, -step),
                Point::new(step, 0.0),
            ),
        ];
        assert_eq!(length(&far), length(&near));
        let (resolution, pieces) = trimmed_resolution(&far);
        assert!(resolution >= RESOLVED, "{resolution}");
        let (_, near_pieces) = trimmed_resolution(&near);
        for (piece, near_piece) in pieces.iter().zip(&near_pieces) {
            assert!(!piece.is_empty());
            assert_eq!(piece.len(), near_piece.len());
            let (bounds, near_bounds) = (
                piece.as_slice().bounding_box(),
                near_piece.as_slice().bounding_box(),
            );
            // Along Y the curve is as precise far away as it is at the origin.
            assert_eq!((bounds.y0, bounds.y1), (near_bounds.y0, near_bounds.y1));
            // Along X it is no more precise than floats are there.
            assert!(
                bounds.x0 >= 1e30 - 4.0 * step && bounds.x1 <= 1e30 + 4.0 * step,
                "{bounds:?}"
            );
        }
    }

    #[test]
    fn curves_that_are_short_for_their_coordinates_are_measured_at_the_origin() {
        let start = Instant::now();
        let mut trimmed = 0;
        // From where floats are a hundredth of a unit apart to where they are 1e14 apart, and
        // one place beyond what a path is measured unscaled at.
        for exponent in [14, 15, 16, 17, 18, 20, 25, 30, 60, 99, 120] {
            let far = 10.0_f64.powi(exponent);
            let step = far * f64::EPSILON;
            // From one float step long to as long as the curve is far away.
            for steps in [0, 1, 2, 4, 8, 12, 16, 20, 24, 32, 40, 52] {
                let size = step * 2.0_f64.powi(steps);
                for shape in &CUBICS {
                    for origin in [
                        Point::new(far, 0.0),
                        Point::new(-far * 0.7, far),
                        Point::new(0.0, -far),
                    ] {
                        let path = cubic_at(shape, origin, size);
                        let (resolution, pieces) = trimmed_resolution(&path);
                        assert!(
                            resolution >= RESOLVED,
                            "{shape:?} of {size} at {origin:?}: {resolution}"
                        );
                        assert!(pieces.iter().all(|piece| is_finite(piece)));
                        trimmed += 1;
                    }
                }
            }
        }
        assert_eq!(trimmed, 11 * 12 * 5 * 3);
        // Measured where they lie, a single one of the worst of these took half a second
        // with optimizations. All of them take a few milliseconds without, so this holds on
        // a slow and busy machine.
        assert!(start.elapsed() < Duration::from_secs(60));
    }

    #[test]
    fn lines_quadratics_and_segments_without_length_far_from_the_origin_are_trimmed() {
        let far = 1e30;
        let step = far * f64::EPSILON;
        let at = |x: f64, y: f64| Point::new(far + x * step, y * step);
        let paths = [
            // Lines, of which one has no length.
            vec![
                PathEl::MoveTo(at(0.0, 0.0)),
                PathEl::LineTo(at(3.0, 2.0)),
                PathEl::LineTo(at(3.0, 2.0)),
                PathEl::LineTo(at(0.0, 4.0)),
                PathEl::ClosePath,
            ],
            // Quadratics: an arc, and one that runs out and back along a line.
            vec![
                PathEl::MoveTo(at(0.0, 0.0)),
                PathEl::QuadTo(at(4.0, 4.0), at(8.0, 0.0)),
                PathEl::QuadTo(at(16.0, 0.0), at(8.0, 0.0)),
            ],
            // Cubics that are a point, and that only have handles at one end.
            vec![
                PathEl::MoveTo(at(0.0, 0.0)),
                PathEl::CurveTo(at(0.0, 0.0), at(0.0, 0.0), at(0.0, 0.0)),
                PathEl::CurveTo(at(0.0, 0.0), at(4.0, 1.0), at(4.0, 1.0)),
                PathEl::CurveTo(at(4.0, 1.0), at(4.0, 1.0), at(0.0, 0.0)),
            ],
        ];
        for path in &paths {
            let (resolution, pieces) = trimmed_resolution(path);
            assert!(resolution >= RESOLVED, "{path:?}: {resolution}");
            let total = length(path);
            assert!(total > 0.0 && total.is_finite());
            for piece in &pieces {
                assert!(!piece.is_empty() && is_finite(piece), "{path:?}: {piece:?}");
                // A piece lies within the path, give or take the rounding of its points.
                let bounds = piece.as_slice().bounding_box();
                let within = path.as_slice().bounding_box().inflate(step, step);
                assert!(within.union(bounds) == within, "{piece:?}");
            }
        }
        // The same far away along one axis only, and at the largest coordinates there are.
        for origin in [Point::new(0.0, -1e30), Point::new(1.5e308, -1.5e308)] {
            let step = 1.5e308 * f64::EPSILON;
            let line = Line::new(origin, origin + Vec2::new(step, step * 3.0));
            let quad = QuadBez::new(
                origin,
                origin + Vec2::new(step * 2.0, 0.0),
                origin + Vec2::new(step * 2.0, step * 2.0),
            );
            for path in [
                vec![PathEl::MoveTo(line.p0), PathEl::LineTo(line.p1)],
                vec![PathEl::MoveTo(quad.p0), PathEl::QuadTo(quad.p1, quad.p2)],
            ] {
                let (resolution, pieces) = trimmed_resolution(&path);
                assert!(resolution >= RESOLVED, "{path:?}: {resolution}");
                assert!(pieces.iter().all(|piece| is_finite(piece)), "{pieces:?}");
            }
        }
    }
}
