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
                let (measured, scale) = measurable(PathSeg::Cubic(curve));
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
