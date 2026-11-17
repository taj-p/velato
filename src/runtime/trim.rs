// Copyright 2026 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use kurbo::{Affine, ParamCurve, ParamCurveArclen, PathEl, PathSeg, segments};

const ACCURACY: f64 = 0.1;
// A segment is measured to `ACCURACY`, or to this fraction of the length of its control polygon
// if that is larger. Measuring to a fixed accuracy takes ever more subdivisions as a curve gets
// longer, so the cost of a trim would grow with the coordinates. Segments shorter than 1e8 units
// are measured to `ACCURACY`.
const RELATIVE_ACCURACY: f64 = 1e-9;
// kurbo squares coordinates when it measures a curve, which overflows above about 1e154 and
// keeps it from converging. Paths with larger coordinates than this are measured scaled down.
const MAX_MAGNITUDE: f64 = 1e100;

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
    (ACCURACY * scale).max(control_length * RELATIVE_ACCURACY)
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
        Some(2.0_f64.powi(-(magnitude.log2().ceil() as i32)))
    } else {
        Some(1.0)
    }
}

fn scaled_length(elements: &[PathEl], scale: f64) -> f64 {
    segments(elements.iter().copied())
        .map(|segment| segment.arclen(accuracy(&segment, scale)))
        .sum()
}

/// Returns the length of a path, or zero if it has coordinates that are not finite.
pub(crate) fn length(elements: &[PathEl]) -> f64 {
    match measuring_scale(elements) {
        None => 0.0,
        Some(1.0) => scaled_length(elements, 1.0),
        Some(scale) => {
            let scaled: Vec<_> = elements
                .iter()
                .map(|element| Affine::scale(scale) * *element)
                .collect();
            scaled_length(&scaled, scale) / scale
        }
    }
}

/// Appends the parts of a path that lie within `ranges` to `output`.
///
/// A path with coordinates that are not finite cannot be measured and is appended untrimmed.
pub(crate) fn trim(
    elements: &[PathEl],
    ranges: &[(f64, f64)],
    sequential: Option<(f64, f64)>,
    output: &mut Vec<PathEl>,
) {
    match measuring_scale(elements) {
        None => output.extend_from_slice(elements),
        Some(1.0) => trim_scaled(elements, 1.0, ranges, sequential, output),
        Some(scale) => {
            let scaled: Vec<_> = elements
                .iter()
                .map(|element| Affine::scale(scale) * *element)
                .collect();
            let sequential = sequential.map(|(offset, total)| (offset * scale, total * scale));
            let start = output.len();
            trim_scaled(&scaled, scale, ranges, sequential, output);
            for element in &mut output[start..] {
                *element = Affine::scale(1.0 / scale) * *element;
            }
        }
    }
}

fn trim_scaled(
    elements: &[PathEl],
    scale: f64,
    ranges: &[(f64, f64)],
    sequential: Option<(f64, f64)>,
    output: &mut Vec<PathEl>,
) {
    let mut offset = sequential.map_or(0.0, |(offset, _)| offset);
    let mut start = 0;
    for end in 1..=elements.len() {
        if end != elements.len()
            && !matches!(elements[end], PathEl::MoveTo(_))
            && !matches!(elements[end - 1], PathEl::ClosePath)
        {
            continue;
        }
        let contour = &elements[start..end];
        let contour_length = scaled_length(contour, scale);
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
            trim_contour(contour, scale, &intervals, contour_length, output);
        }
        offset += contour_length;
        start = end;
    }
}

fn trim_contour(
    elements: &[PathEl],
    scale: f64,
    intervals: &[(f64, f64)],
    length: f64,
    output: &mut Vec<PathEl>,
) {
    let segments: Vec<_> = segments(elements.iter().copied())
        .map(|seg| (seg, seg.arclen(accuracy(&seg, scale))))
        .collect();
    let closed = matches!(elements.last(), Some(PathEl::ClosePath));
    let mut previous_end = None;
    for &(start, end) in intervals {
        let mut need_move = !(closed && previous_end == Some(length) && start == 0.0);
        let mut position = 0.0;
        for &(segment, segment_length) in &segments {
            if position >= end {
                break;
            }
            let next = position + segment_length;
            if segment_length > 0.0 && next > start && position < end {
                let accuracy = accuracy(&segment, scale);
                let t0 = if start <= position {
                    0.0
                } else {
                    segment
                        .inv_arclen(start - position, accuracy)
                        .clamp(0.0, 1.0)
                };
                let t1 = if end >= next {
                    1.0
                } else {
                    segment.inv_arclen(end - position, accuracy).clamp(0.0, 1.0)
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
    use super::{length, trim};
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
        trim(path, &[(0.0, 0.5)], None, &mut output);
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
