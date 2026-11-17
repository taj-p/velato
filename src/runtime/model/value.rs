// Copyright 2024 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

use kurbo::{
    ParamCurve, Point,
    common::{solve_cubic, solve_itp},
};

/// Fixed or animated value.
#[derive(Clone, Debug)]
pub enum Value<T: Tween> {
    /// Fixed value.
    Fixed(T),
    /// Animated value.
    Animated(Animated<T>),
}

impl<T: Tween + Default> Value<T> {
    /// Returns the value at a specified frame.
    pub fn evaluate(&self, frame: f64) -> T {
        match self {
            Self::Fixed(fixed) => fixed.clone(),
            Self::Animated(animated) => animated.evaluate_or(frame, T::default()),
        }
    }
}

impl<T: Tween> Value<T> {
    /// Returns true if the value is fixed.
    pub fn is_fixed(&self) -> bool {
        matches!(self, Self::Fixed(_))
    }

    /// Returns the value at a specified frame, with a fallback default.
    pub fn evaluate_or(&self, frame: f64, default: T) -> T {
        match self {
            Self::Fixed(fixed) => fixed.clone(),
            Self::Animated(animated) => animated.evaluate_or(frame, default),
        }
    }
}

impl<T: Tween + Default> Default for Value<T> {
    fn default() -> Self {
        Self::Fixed(T::default())
    }
}

/// Borrowed or owned value.
#[derive(Clone, Debug)]
pub enum ValueRef<'a, T> {
    Borrowed(&'a T),
    Owned(T),
}

impl<T> AsRef<T> for ValueRef<'_, T> {
    fn as_ref(&self) -> &T {
        match self {
            Self::Borrowed(value) => value,
            Self::Owned(value) => value,
        }
    }
}

impl<T: Clone> ValueRef<'_, T> {
    pub fn into_owned(self) -> T {
        match self {
            Self::Borrowed(value) => value.clone(),
            Self::Owned(value) => value,
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub struct Easing {
    pub o: EasingHandle,
    pub i: EasingHandle,
}

impl Easing {
    pub const LERP: Easing = Easing {
        o: EasingHandle { x: 0.0, y: 0.0 },
        i: EasingHandle { x: 1.0, y: 1.0 },
    };

    /// Whether the easing curve is the straight line from start to end.
    pub(crate) fn is_linear(&self) -> bool {
        self.o.x == self.o.y && self.i.x == self.i.y
    }
}

#[derive(Copy, Clone, Debug)]
pub struct EasingHandle {
    pub x: f64,
    pub y: f64,
}

/// Time for a particular keyframe, represented as a frame number.
#[derive(Copy, Clone, Debug)]
pub struct Time {
    /// Frame number.
    pub frame: f64,
    /// Easing tangent going into the next keyframe
    pub in_tangent: Option<EasingHandle>,
    /// Easing tangent leaving the current keyframe
    pub out_tangent: Option<EasingHandle>,
    /// Whether it's a hold frame.
    pub hold: bool,
    /// Easing of the second, third and fourth dimension of the value, for a keyframe that
    /// eases the dimensions of its value separately. A dimension without easing of its own is
    /// eased like the first, with `in_tangent` and `out_tangent`.
    pub dimension_easings: [Option<Easing>; 3],
}

impl Time {
    /// Returns the easing from this keyframe to the next. Without tangents it is linear.
    fn easing(&self) -> Easing {
        Easing {
            o: self.out_tangent.unwrap_or(Easing::LERP.o),
            i: self.in_tangent.unwrap_or(Easing::LERP.i),
        }
    }

    /// Returns the easing of each of the first four dimensions of the value.
    fn easings(&self) -> [Easing; 4] {
        let first = self.easing();
        let [second, third, fourth] = self.dimension_easings.map(|easing| easing.unwrap_or(first));
        [first, second, third, fourth]
    }

    /// Returns the frame indices and interpolation weight for the given frame,
    /// and whether to hold the frame
    pub(crate) fn frames_and_weight(
        times: &[Time],
        frame: f64,
    ) -> Option<([usize; 2], f64, Easing, bool)> {
        if times.is_empty() {
            return None;
        }
        // When several keyframes share a time, the last of them applies from that time on. A
        // binary search may return any of them.
        let ix = times
            .partition_point(|x| x.frame <= frame)
            .saturating_sub(1);
        let ix0 = ix.min(times.len() - 1);
        let ix1 = (ix0 + 1).min(times.len() - 1);

        let t0 = times[ix0];
        let t1 = times[ix1];
        let easing = t0.easing();
        let hold = t0.hold;
        // Keyframes that are out of order can still leave a span without length, which has no
        // position to interpolate at.
        let span = t1.frame - t0.frame;
        let t = if span > 0.0 {
            (frame - t0.frame) / span
        } else {
            0.0
        };
        Some(([ix0, ix1], t.clamp(0.0, 1.0), easing, hold))
    }
}

#[derive(Clone, Debug)]
pub struct Animated<T: Tween> {
    pub times: Vec<Time>,
    pub values: Vec<T>,
}

impl<T: Tween> Animated<T> {
    /// Returns the value at the specified frame.
    pub fn evaluate_or(&self, frame: f64, default: T) -> T {
        self.evaluate_inner(frame).unwrap_or(default)
    }

    fn evaluate_inner(&self, frame: f64) -> Option<T> {
        let ([ix0, ix1], t, _, hold) = Time::frames_and_weight(&self.times, frame)?;
        let t = if hold { 0.0 } else { t };

        let v1 = self.values.get(ix0)?;
        let v2 = self.values.get(ix1)?;

        Some(v1.tween_each(v2, t, &self.times[ix0].easings()))
    }
}

/// Something that can be interpolated with an easing function.
pub trait Tween: Clone {
    fn tween(&self, other: &Self, t: f64, easing: &Easing) -> Self;

    /// Tweens each dimension of the value with an easing of its own. A value of one dimension
    /// takes the first.
    fn tween_each(&self, other: &Self, t: f64, easings: &[Easing; 4]) -> Self {
        self.tween(other, t, &easings[0])
    }
}

impl Tween for f64 {
    /// Tween between two f64 values using a cubic bezier easing curve.
    ///
    /// Unfortunately, kurbo doesn't expose a way to get the eased value of t directly, so we have
    /// to re-implement it here.
    ///
    /// t - normalized time 0.0 to 1.0
    fn tween(&self, other: &Self, t: f64, easing: &Easing) -> Self {
        // avoid unnecessary calculations
        if t <= 0.0 {
            return *self;
        }
        if t >= 1.0 {
            return *other;
        }

        // Created for [kurbo::ParamCurve::eval]
        let curve = kurbo::CubicBez::new(
            Point::new(0.0, 0.0),
            Point::new(easing.o.x, easing.o.y),
            Point::new(easing.i.x, easing.i.y),
            Point::new(1.0, 1.0),
        );

        // Manual copy-paste of [kurbo::bezpath::cubic_bez_coefs]
        // in preparation for [kurbo::common::solve_cubic]
        let (x0, x1, x2, x3) = (curve.p0.x, curve.p1.x, curve.p2.x, curve.p3.x);
        let c0 = x0;
        let c1 = 3.0 * (x1 - x0);
        let c2 = 3.0 * (x2 - 2.0 * x1 + x0);
        let c3 = x3 - 3.0 * x2 + 3.0 * x1 - x0;

        // Find where along the curve we are at normalized time `t`
        // See: https://github.com/jneem/linesweeper/blob/7d620ed004605141519037c681edac93e22f5855/src/curve/mod.rs#L355
        let max_coeff = c0.abs().max(c1.abs()).max(c2.abs()).max(c3.abs());
        let eps = 1e-12 * max_coeff.max(1.0);

        let roots = kurbo::common::solve_cubic(c0 - t, c1, c2, c3);
        let u = roots
            .iter()
            .copied()
            .find(|&r| (-eps..=1.0 + eps).contains(&r))
            .unwrap_or_else(|| {
                // Fallback when no valid root found due to precision issues
                // The cubic evaluates to (c0 - t) at u=0 and (c0 + c1 + c2 + c3 - t) at u=1
                let val_at_0 = (c0 - t).abs();
                let val_at_1 = (c0 + c1 + c2 + c3 - t).abs();

                if val_at_0 <= val_at_1 { 0.0 } else { 1.0 }
            });

        // Evaluate the curve at that point to get the y value
        let eased_y = curve.eval(u).y;

        self + (other - self) * eased_y
    }
}

impl Tween for kurbo::Point {
    fn tween(&self, other: &Self, t: f64, easing: &Easing) -> Self {
        Self::new(
            self.x.tween(&other.x, t, easing),
            self.y.tween(&other.y, t, easing),
        )
    }

    fn tween_each(&self, other: &Self, t: f64, easings: &[Easing; 4]) -> Self {
        Self::new(
            self.x.tween(&other.x, t, &easings[0]),
            self.y.tween(&other.y, t, &easings[1]),
        )
    }
}

impl Tween for kurbo::Vec2 {
    fn tween(&self, other: &Self, t: f64, easing: &Easing) -> Self {
        Self::new(
            self.x.tween(&other.x, t, easing),
            self.y.tween(&other.y, t, easing),
        )
    }

    fn tween_each(&self, other: &Self, t: f64, easings: &[Easing; 4]) -> Self {
        Self::new(
            self.x.tween(&other.x, t, &easings[0]),
            self.y.tween(&other.y, t, &easings[1]),
        )
    }
}

impl Tween for kurbo::Size {
    fn tween(&self, other: &Self, t: f64, easing: &Easing) -> Self {
        Self::new(
            self.width.tween(&other.width, t, easing),
            self.height.tween(&other.height, t, easing),
        )
    }

    fn tween_each(&self, other: &Self, t: f64, easings: &[Easing; 4]) -> Self {
        Self::new(
            self.width.tween(&other.width, t, &easings[0]),
            self.height.tween(&other.height, t, &easings[1]),
        )
    }
}

impl Tween for peniko::Color {
    fn tween(&self, other: &Self, t: f64, easing: &Easing) -> Self {
        let [r1, g1, b1, a1] = self.components;
        let [r2, g2, b2, a2] = other.components;
        let r = (r1 as f64).tween(&(r2 as f64), t, easing);
        let g = (g1 as f64).tween(&(g2 as f64), t, easing);
        let b = (b1 as f64).tween(&(b2 as f64), t, easing);
        let a = (a1 as f64).tween(&(a2 as f64), t, easing);
        peniko::Color::new([r as f32, g as f32, b as f32, a as f32])
    }

    fn tween_each(&self, other: &Self, t: f64, easings: &[Easing; 4]) -> Self {
        let mut components = self.components;
        for ((component, to), easing) in components.iter_mut().zip(other.components).zip(easings) {
            *component = f64::from(*component).tween(&f64::from(to), t, easing) as f32;
        }
        peniko::Color::new(components)
    }
}
