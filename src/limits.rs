// Copyright 2026 the Velato Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Limits on what an animation may ask for.
//!
//! A Lottie file is a small program. Repeaters multiply shapes, precompositions multiply layers,
//! and a keyframe with easing can turn a count into a number far larger than any in the file.
//! Without limits, a file of a few hundred bytes can ask for more memory or time than a machine
//! has, or nest deeper than the stack allows.
//!
//! Velato therefore limits what one frame of an animation may take. An animation that is over
//! a limit is rejected with an error. It is never drawn in part.
//!
//! - What is the same at every frame is checked when the animation is imported, which fails
//!   with [`Error::LimitExceeded`](crate::Error::LimitExceeded).
//! - What depends on the frame is checked by
//!   [`Composition::evaluate`](crate::Composition::evaluate), which fails with
//!   [`EvaluationError::LimitExceeded`](crate::EvaluationError::LimitExceeded).
//!   [`Renderer::try_append`](crate::Renderer::try_append) evaluates before it draws, so it
//!   leaves the sink untouched.
//!
//! With these limits, the time and memory that importing takes grow in proportion to the size
//! of the file, and the time and memory that evaluating and rendering a frame take, and that
//! the queries of [`EvaluatedLayer`](crate::EvaluatedLayer) take, have a fixed bound.
//!
//! Evaluating and rendering recurse once for each level of nested layers and of nested shape
//! groups, and nowhere else, so [`MAX_NESTING_DEPTH`] and [`MAX_GROUP_DEPTH`] bound the stack
//! that they use. An animation that nests as deep as both allow takes less than 256 kilobytes of
//! stack in a build without optimization. Importing recurses as deep as the JSON parser does,
//! which is 128 levels of arrays and objects.
//!
//! # What counts
//!
//! The limits on shapes and layers apply to one frame, and count what is rendered at it:
//!
//! - A layer counts if the frame is within its in and out points. A layer that only serves as
//!   a track matte counts once for each layer that it mattes, since it is rendered that often.
//! - A layer of a precomposition counts once for each layer that shows the precomposition.
//! - The shapes of a layer are counted at the frame itself when their number is animated, and
//!   then they count whether or not the layer is rendered. That is the case for a layer in
//!   which the copies of a repeater, the points of a star or the vertices of a path change over
//!   time.
//!
//! The limits are far above what authored animations use.
//!
//! # Compositions that are not imported
//!
//! [`Composition::evaluate`](crate::Composition::evaluate) checks every limit for a
//! composition that was put together in code as well. A layer that was imported keeps a record
//! of the shapes that import counted for it. Code that changes the shapes of such a layer
//! should make a new [`Layer`](crate::model::Layer) for them, which is counted afresh.

use std::fmt;

/// Most copies a single repeater may make.
pub const MAX_REPEATER_COPIES: usize = 2_000;

/// Most points a single star or polygon may have.
pub const MAX_POLYSTAR_POINTS: usize = 2_000;

/// Most colour stops a gradient may have, and most transparency stops.
pub const MAX_GRADIENT_STOPS: usize = 256;

/// Most shapes in one frame.
///
/// Every item of a shape layer counts as one: a group, a path, a fill or stroke, a trim path
/// and a repeater. A path or a fill or stroke counts once more for each copy that a repeater
/// makes of it, and nested repeaters multiply their copies. Each mask and each effect of a
/// layer counts as one as well.
pub const MAX_SHAPE_INSTANCES: usize = 100_000;

/// Most path elements in one frame.
///
/// The elements of a path are its move, line, curve and close commands. This counts:
///
/// - the elements of every path, once for each copy that a repeater makes of it, where an
///   empty path counts as one element,
/// - the same elements again for each fill or stroke that draws the path, along with the
///   colour stops of a gradient and the dashes of a stroke that it draws with,
/// - the elements of the masks of a layer.
///
/// A trim path can cut each path that it applies to in two, so it counts their elements a
/// second time.
pub const MAX_PATH_ELEMENTS: usize = 2_000_000;

/// Most path elements that trim paths apply to in one frame.
///
/// A trim path measures the paths that it applies to, which takes far longer than drawing them.
/// The elements of each of these paths count once for every trim path that applies to it, and
/// once more for each copy that a repeater before the trim path makes of it. Like the paths
/// themselves, they count twice from the second trim path on, because the first can have cut
/// each path in two.
pub const MAX_TRIMMED_ELEMENTS: usize = 30_000;

/// Most layers in one frame.
///
/// This holds for the layers that are rendered at a frame, and also for all the layers that an
/// animation has when every layer of a precomposition is counted once for each layer that shows
/// the precomposition, and every track matte source once for each layer that it mattes.
pub const MAX_LAYER_INSTANCES: usize = 20_000;

/// Deepest nesting of layers.
///
/// A layer of the animation itself is at depth one. A layer of a precomposition is one level
/// deeper than the layer that shows the precomposition, and a track matte source is one level
/// deeper than the layer that it mattes. Evaluating and rendering recurse once per level, so
/// this limits the stack that they use.
pub const MAX_NESTING_DEPTH: usize = 32;

/// Deepest nesting of shape groups in a layer.
///
/// A group that holds no other group is at depth one. Evaluating the shapes of a layer recurses
/// once per level, so this limits the stack that it uses, together with [`MAX_NESTING_DEPTH`].
///
/// An animation that is read from JSON cannot reach this limit: the parser takes no more than
/// 128 levels of arrays and objects, and a group is two of them. The limit is there for a
/// composition that is put together in code.
pub const MAX_GROUP_DEPTH: usize = 64;

/// A limit that an animation can exceed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Limit {
    /// [`MAX_REPEATER_COPIES`]
    RepeaterCopies,
    /// [`MAX_POLYSTAR_POINTS`]
    PolystarPoints,
    /// [`MAX_GRADIENT_STOPS`]
    GradientStops,
    /// [`MAX_SHAPE_INSTANCES`]
    ShapeInstances,
    /// [`MAX_PATH_ELEMENTS`]
    PathElements,
    /// [`MAX_TRIMMED_ELEMENTS`]
    TrimmedElements,
    /// [`MAX_LAYER_INSTANCES`]
    LayerInstances,
    /// [`MAX_NESTING_DEPTH`]
    NestingDepth,
    /// [`MAX_GROUP_DEPTH`]
    GroupDepth,
}

impl fmt::Display for Limit {
    /// Writes what the limit counts, to follow a number.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RepeaterCopies => "copies of a repeater",
            Self::PolystarPoints => "points of a star or polygon",
            Self::GradientStops => "stops of a gradient",
            Self::ShapeInstances => "shapes in one frame",
            Self::PathElements => "path elements in one frame",
            Self::TrimmedElements => "path elements under trim paths in one frame",
            Self::LayerInstances => "layers in one frame",
            Self::NestingDepth => "levels of nested layers",
            Self::GroupDepth => "levels of nested shape groups",
        })
    }
}

/// The limits in force for an import or an evaluation.
///
/// Everything that enforces a limit takes this value rather than reading the constants, so that
/// callers can be given a way to choose their own limits later.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub(crate) repeater_copies: usize,
    pub(crate) polystar_points: usize,
    pub(crate) gradient_stops: usize,
    pub(crate) shape_instances: usize,
    pub(crate) path_elements: usize,
    pub(crate) trimmed_elements: usize,
    pub(crate) layer_instances: usize,
    pub(crate) nesting_depth: usize,
    pub(crate) group_depth: usize,
}

impl Limits {
    pub(crate) const DEFAULT: Self = Self {
        repeater_copies: MAX_REPEATER_COPIES,
        polystar_points: MAX_POLYSTAR_POINTS,
        gradient_stops: MAX_GRADIENT_STOPS,
        shape_instances: MAX_SHAPE_INSTANCES,
        path_elements: MAX_PATH_ELEMENTS,
        trimmed_elements: MAX_TRIMMED_ELEMENTS,
        layer_instances: MAX_LAYER_INSTANCES,
        nesting_depth: MAX_NESTING_DEPTH,
        group_depth: MAX_GROUP_DEPTH,
    };

    pub(crate) fn maximum(&self, limit: Limit) -> usize {
        match limit {
            Limit::RepeaterCopies => self.repeater_copies,
            Limit::PolystarPoints => self.polystar_points,
            Limit::GradientStops => self.gradient_stops,
            Limit::ShapeInstances => self.shape_instances,
            Limit::PathElements => self.path_elements,
            Limit::TrimmedElements => self.trimmed_elements,
            Limit::LayerInstances => self.layer_instances,
            Limit::NestingDepth => self.nesting_depth,
            Limit::GroupDepth => self.group_depth,
        }
    }
}

/// Why the shapes of a layer cannot be evaluated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ShapeError {
    /// The shapes ask for more than a limit allows.
    Limit(Limit),
    /// A rectangle or ellipse has a size that is not finite.
    NonFiniteSize,
}

impl From<Limit> for ShapeError {
    fn from(limit: Limit) -> Self {
        Self::Limit(limit)
    }
}

/// What the shapes of a layer take to evaluate and draw at one frame, in the units of
/// [`MAX_SHAPE_INSTANCES`], [`MAX_PATH_ELEMENTS`] and [`MAX_TRIMMED_ELEMENTS`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShapeCost {
    pub(crate) instances: usize,
    pub(crate) elements: usize,
    pub(crate) trimmed: usize,
}

impl ShapeCost {
    /// Adds the cost of other shapes that are evaluated in the same frame.
    pub(crate) fn add(&mut self, other: Self, limits: &Limits) -> Result<(), Limit> {
        self.instances = self.instances.saturating_add(other.instances);
        self.elements = self.elements.saturating_add(other.elements);
        self.trimmed = self.trimmed.saturating_add(other.trimmed);
        self.check(limits)
    }

    pub(crate) fn check(self, limits: &Limits) -> Result<(), Limit> {
        if self.instances > limits.shape_instances {
            Err(Limit::ShapeInstances)
        } else if self.elements > limits.path_elements {
            Err(Limit::PathElements)
        } else if self.trimmed > limits.trimmed_elements {
            Err(Limit::TrimmedElements)
        } else {
            Ok(())
        }
    }
}

/// When the shapes of a layer have to be counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ShapeCheck {
    /// Each time the composition is evaluated, because what the shapes take depends on the
    /// frame, a size in them is animated and may not be finite at some frame, or nothing is
    /// known about them.
    #[default]
    EveryFrame,
    /// Never again: import counted them, and nothing in them can change the count.
    Never(ShapeCost),
}

/// A running count of what the shapes of one layer take to evaluate and draw.
///
/// The count is kept by the code that evaluates the shapes, and is the same whether that code
/// stores what it evaluates or only counts. It is an upper bound: it is made from the sizes
/// that paths can have rather than the sizes they turn out to have, so that it can be known
/// before anything is allocated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShapeCount {
    /// Groups, trims and repeaters.
    items: usize,
    /// Paths, each counted once for every copy that repeaters make of it.
    geometries: usize,
    /// Styles, counted likewise.
    draws: usize,
    /// Elements of the paths counted by `geometries`. An empty path counts as one element.
    elements: usize,
    /// Elements of the paths that each style draws, colour stops of its brush and dashes of
    /// its stroke, summed over the styles.
    drawn: usize,
    /// Elements of the paths that trims applied to, summed over the trims.
    trimmed: usize,
}

impl ShapeCount {
    /// Returns the count of the elements of the paths, after repeaters and trims.
    pub(crate) fn elements(&self) -> usize {
        self.elements
    }

    pub(crate) fn cost(&self) -> ShapeCost {
        ShapeCost {
            instances: self
                .items
                .saturating_add(self.geometries)
                .saturating_add(self.draws),
            elements: self.elements.saturating_add(self.drawn),
            trimmed: self.trimmed,
        }
    }

    fn check(&self, limits: &Limits) -> Result<(), Limit> {
        self.cost().check(limits)
    }

    /// Counts a group, trim or repeater.
    pub(crate) fn item(&mut self, limits: &Limits) -> Result<(), Limit> {
        self.items = self.items.saturating_add(1);
        self.check(limits)
    }

    /// Counts a path that has at most `elements` elements.
    pub(crate) fn geometry(&mut self, elements: usize, limits: &Limits) -> Result<(), Limit> {
        self.geometries = self.geometries.saturating_add(1);
        self.elements = self.elements.saturating_add(elements.max(1));
        self.check(limits)
    }

    /// Counts a style that draws the paths of its group with a brush and stroke that hold
    /// `paint_elements` colour stops and dashes. `group` is the count from before the group
    /// started.
    pub(crate) fn draw(
        &mut self,
        group: &Self,
        paint_elements: usize,
        limits: &Limits,
    ) -> Result<(), Limit> {
        self.draws = self.draws.saturating_add(1);
        self.drawn = self
            .drawn
            .saturating_add(self.elements - group.elements)
            .saturating_add(paint_elements);
        self.check(limits)
    }

    /// Counts a repeater that makes `copies` copies of the paths and styles of its group.
    ///
    /// The paths stay in place in addition to their copies. The styles are replaced by their
    /// copies.
    pub(crate) fn repeat(
        &mut self,
        group: &Self,
        copies: usize,
        limits: &Limits,
    ) -> Result<(), Limit> {
        let repeated = |total: usize, before: usize| (total - before).saturating_mul(copies);
        self.geometries = self
            .geometries
            .saturating_add(repeated(self.geometries, group.geometries));
        self.elements = self
            .elements
            .saturating_add(repeated(self.elements, group.elements));
        self.draws = group
            .draws
            .saturating_add(repeated(self.draws, group.draws));
        self.drawn = group
            .drawn
            .saturating_add(repeated(self.drawn, group.drawn));
        self.check(limits)
    }

    /// Counts a trim of the paths of its group.
    ///
    /// A trim that wraps around the end of a path cuts it in two, which can double its
    /// elements. Every trim is counted as if it did, so that the count does not depend on the
    /// range of the trim.
    pub(crate) fn trim(&mut self, group: &Self, limits: &Limits) -> Result<(), Limit> {
        let applied_to = self.elements - group.elements;
        self.trimmed = self.trimmed.saturating_add(applied_to);
        self.elements = self.elements.saturating_add(applied_to);
        self.drawn = self.drawn.saturating_add(self.drawn - group.drawn);
        self.check(limits)
    }
}
