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

/// Returns the length of a segment of a path that was scaled by `scale`.
fn segment_length(segment: PathSeg, scale: f64) -> f64 {
    let (fitted, fit) = measurable(segment);
    fitted.arclen(accuracy(&fitted, scale * fit)) / fit
}

/// Returns the parameter at which a segment of a path that was scaled by `scale` reaches the
/// given length.
fn segment_parameter(segment: PathSeg, length: f64, scale: f64) -> f64 {
    let (fitted, fit) = measurable(segment);
    fitted.inv_arclen(length * fit, accuracy(&fitted, scale * fit))
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
    use super::{Measured, length, trim};
    use kurbo::{PathEl, Point, Shape as _};
    use std::time::{Duration, Instant};

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
}
