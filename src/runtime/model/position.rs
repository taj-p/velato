// Copyright 2026 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use super::{Easing, Time, Tween};
use crate::runtime::trim::measurable;
use kurbo::{
    CubicBez, ParamCurve, ParamCurveArclen, ParamCurveDeriv, ParamCurveExtrema, PathSeg, Point,
    Vec2,
};

#[derive(Clone, Debug)]
pub struct SpatialKeyframe {
    pub time: Time,
    pub value: Point,
    /// Offset from this keyframe's position.
    pub out_tangent: Option<Vec2>,
    /// Offset from the next position, stored on this keyframe.
    pub in_tangent: Option<Vec2>,
}

#[derive(Clone, Debug)]
struct MotionPath {
    curve: CubicBez,
    /// The curve to measure: `curve`, or a scaled copy of it if its extent is extreme.
    measured: PathSeg,
    /// Length of `measured`.
    arc_length: f64,
    accuracy: f64,
}

#[derive(Clone, Debug)]
pub struct SpatialPosition {
    keyframes: Vec<SpatialKeyframe>,
    paths: Vec<Option<MotionPath>>,
}

impl SpatialPosition {
    const ACCURACY: f64 = 0.01;
    const RELATIVE_ACCURACY: f64 = 1e-5;
    // Keeps the root finding that inverts an arc length within its iteration budget when a path
    // is extremely long. It is below `ACCURACY` for any length that occurs in practice.
    const MIN_RELATIVE_ACCURACY: f64 = 1e-12;

    fn is_linear(curve: &CubicBez) -> bool {
        let chord = curve.p3 - curve.p0;
        let out = curve.p1 - curve.p0;
        let incoming = curve.p3 - curve.p2;
        if chord == Vec2::ZERO {
            return out == Vec2::ZERO && incoming == Vec2::ZERO;
        }
        if !curve.is_finite()
            || !chord.is_finite()
            || chord.cross(out) != 0.0
            || chord.cross(incoming) != 0.0
        {
            return false;
        }
        let derivative = curve.deriv();
        if !derivative.is_finite() {
            return false;
        }
        let bounds = derivative.bounding_box();
        let (direction, min, max) = if chord.x.abs() >= chord.y.abs() {
            (chord.x, bounds.x0, bounds.x1)
        } else {
            (chord.y, bounds.y0, bounds.y1)
        };
        if direction > 0.0 {
            min >= 0.0
        } else {
            max <= 0.0
        }
    }

    /// Returns the curve, or a copy of it that starts at the origin if the curve is very short
    /// for the coordinates that it lies at.
    ///
    /// kurbo halves a curve until its halves are accurate enough. The halves of a curve that
    /// is not much longer than the spacing of floats at its coordinates are rounding errors,
    /// which never are, so that kurbo halves the curve as far as it can. At the origin the
    /// points of the copy are as precise as the curve is short.
    fn resolvable(curve: CubicBez) -> CubicBez {
        // A curve that is longer than this fraction of its coordinates is accurate enough
        // well before its halves come near the spacing of floats.
        const SHORT: f64 = 1.0 / 65536.0;
        let points = [curve.p0, curve.p1, curve.p2, curve.p3];
        let largest = |values: [Vec2; 4]| {
            values.iter().fold(0.0_f64, |largest, value| {
                largest.max(value.x.abs()).max(value.y.abs())
            })
        };
        let magnitude = largest(points.map(Point::to_vec2));
        let offsets = points.map(|point| point - curve.p0);
        if largest(offsets) < magnitude * SHORT {
            let [p0, p1, p2, p3] = offsets.map(Vec2::to_point);
            CubicBez::new(p0, p1, p2, p3)
        } else {
            curve
        }
    }

    pub fn new(keyframes: Vec<SpatialKeyframe>) -> Self {
        let paths = keyframes
            .windows(2)
            .map(|pair| {
                let start = &pair[0];
                let end = &pair[1];
                let out_tangent = start.out_tangent.unwrap_or(Vec2::ZERO);
                let in_tangent = start.in_tangent.unwrap_or(Vec2::ZERO);
                if start.time.hold {
                    return None;
                }
                let curve = CubicBez::new(
                    start.value,
                    start.value + out_tangent,
                    end.value + in_tangent,
                    end.value,
                );
                if Self::is_linear(&curve) {
                    return None;
                }
                // kurbo squares the differences between the points of a curve while it
                // measures it. For a very large curve they overflow, and for a very small one
                // they underflow, and either keeps it from converging. Such curves are
                // measured scaled.
                let (measured, scale) = measurable(PathSeg::Cubic(Self::resolvable(curve)));
                let control_length = match measured {
                    PathSeg::Cubic(fitted) => {
                        (fitted.p1 - fitted.p0).hypot()
                            + (fitted.p2 - fitted.p1).hypot()
                            + (fitted.p3 - fitted.p2).hypot()
                    }
                    _ => unreachable!("a cubic stays a cubic"),
                };
                let accuracy = (Self::ACCURACY * scale)
                    .min(control_length * Self::RELATIVE_ACCURACY)
                    .max(control_length * Self::MIN_RELATIVE_ACCURACY)
                    .max(f64::MIN_POSITIVE);
                let arc_length = if curve.is_finite() {
                    measured.arclen(accuracy)
                } else {
                    f64::NAN
                };
                Some(MotionPath {
                    curve,
                    measured,
                    arc_length,
                    accuracy,
                })
            })
            .collect();
        Self { keyframes, paths }
    }

    pub fn evaluate(&self, frame: f64) -> Point {
        let index = self
            .keyframes
            .partition_point(|k| k.time.frame <= frame)
            .saturating_sub(1);
        let Some(start) = self.keyframes.get(index) else {
            return Point::ORIGIN;
        };
        let Some(end) = self.keyframes.get(index + 1) else {
            return start.value;
        };
        if frame <= start.time.frame || start.time.hold || end.time.frame <= start.time.frame {
            return start.value;
        }
        let t = ((frame - start.time.frame) / (end.time.frame - start.time.frame)).clamp(0.0, 1.0);
        let easing = Easing {
            o: start.time.out_tangent.unwrap_or(Easing::LERP.o),
            i: start.time.in_tangent.unwrap_or(Easing::LERP.i),
        };
        let progress = 0.0_f64.tween(&1.0, t, &easing);
        let Some(path) = &self.paths[index] else {
            return start.value.lerp(end.value, progress);
        };
        if !path.arc_length.is_finite() || !progress.is_finite() {
            return Point::new(f64::NAN, f64::NAN);
        }
        if path.arc_length == 0.0 || progress <= 0.0 {
            return start.value;
        }
        if progress >= 1.0 {
            return end.value;
        }
        let parameter = path
            .measured
            .inv_arclen(progress * path.arc_length, path.accuracy);
        path.curve.eval(parameter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn keyframe(frame: f64, value: Point, tangent: Vec2) -> SpatialKeyframe {
        SpatialKeyframe {
            time: Time {
                frame,
                in_tangent: None,
                out_tangent: None,
                hold: false,
            },
            value,
            out_tangent: Some(tangent),
            in_tangent: Some(tangent),
        }
    }

    #[test]
    fn motion_paths_with_extreme_coordinates_are_measured_scaled_down() {
        // Measured as they are, the squares of these coordinates overflow, and kurbo then
        // subdivides each path as far as it can.
        let size = 1e200;
        let keyframes: Vec<_> = (0..40)
            .map(|index| {
                let x = f64::from(index) * size;
                keyframe(f64::from(index), Point::new(x, 0.0), Vec2::new(0.0, size))
            })
            .collect();
        let start = Instant::now();
        let position = SpatialPosition::new(keyframes);
        let middle = position.evaluate(0.5);
        assert!(start.elapsed() < Duration::from_secs(1));
        // Halfway along the first arc, which bulges away from the X axis.
        assert!((middle.x / size - 0.5).abs() < 1e-6, "{middle:?}");
        assert!(middle.y / size > 0.5, "{middle:?}");
    }

    #[test]
    fn motion_path_shorter_than_its_coordinates_resolve_is_measured() {
        // Keyframes that return to the point they left, by way of tangents that are barely
        // longer than the spacing of floats at that point. Halved where it lies, such a path
        // turns into rounding errors, which are never accurate enough for kurbo, so that it
        // halves the path as far as it can. Each of these took milliseconds to evaluate.
        let cases = [
            ((100.0, 250.0), (0.0, 0.0), (1e-12, 0.0)),
            ((-37.25, 0.001), (1e-13, 0.0), (0.0, 1e-13)),
            ((512.0, 384.5), (1e-12, 1e-12), (1e-12, -1e-12)),
            ((512.0, 384.5), (1e-9, 0.0), (0.0, 1e-9)),
        ];
        let start = Instant::now();
        for (point, out_tangent, in_tangent) in cases {
            let point = Point::from(point);
            let mut first = keyframe(0.0, point, Vec2::from(out_tangent));
            first.in_tangent = Some(Vec2::from(in_tangent));
            let last = keyframe(1000.0, point, Vec2::ZERO);
            let position = SpatialPosition::new(vec![first, last]);
            let reach = Vec2::from(out_tangent).hypot() + Vec2::from(in_tangent).hypot();
            for frame in 1..1000 {
                let value = position.evaluate(f64::from(frame));
                assert!((value - point).hypot() <= reach, "{point:?}: {value:?}");
            }
        }
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn motion_paths_of_any_extent_are_measured() {
        // The squares of differences this small are zero, and of ones this large not finite.
        for size in [1e-160, 1e-120, 1.0, 1e160, 1e307] {
            let keyframes: Vec<_> = (0..40)
                .map(|index| {
                    let x = f64::from(index) * size;
                    keyframe(f64::from(index), Point::new(x, 0.0), Vec2::new(0.0, size))
                })
                .collect();
            let start = Instant::now();
            let position = SpatialPosition::new(keyframes);
            let middle = position.evaluate(0.5);
            assert!(start.elapsed() < Duration::from_secs(1), "size {size}");
            assert!(
                (middle.x / size - 0.5).abs() < 1e-3,
                "size {size}: {middle:?}"
            );
            assert!(middle.y / size > 0.5, "size {size}: {middle:?}");
        }
    }
}
