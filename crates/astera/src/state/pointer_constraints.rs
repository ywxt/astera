use smithay::{
    input::pointer::PointerHandle,
    reexports::wayland_server::protocol::wl_surface::WlSurface,
    utils::{Logical, Point},
    wayland::{
        compositor::RegionAttributes,
        pointer_constraints::{
            PointerConstraint, PointerConstraintsHandler, with_pointer_constraint,
        },
    },
};

use super::Astera;

impl Astera {
    /// Returns the active constraint for the pointer's current focus.  Activation regions are
    /// deliberately not checked here: they decide when a constraint starts, not whether an
    /// already-active constraint continues to apply.
    pub(super) fn active_pointer_constraint(&self) -> Option<ActivePointerConstraint> {
        let pointer = self.pointer.clone();
        let surface = pointer.current_focus()?;
        self.pointer_focus_origin
            .as_ref()
            .filter(|(focused, _, _)| *focused == surface)?;
        let mut result = None;
        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint.filter(|constraint| constraint.is_active()) else {
                return;
            };
            result = Some(match &*constraint {
                PointerConstraint::Locked(_) => ActivePointerConstraint::Locked,
                PointerConstraint::Confined(constraint) => ActivePointerConstraint::Confined {
                    surface: surface.clone(),
                    region: constraint.region().cloned(),
                },
            });
        });
        result
    }

    pub(super) fn constrain_pointer_target(
        &mut self,
        target: Point<f64, Logical>,
    ) -> ConstrainedPointerTarget {
        match self.active_pointer_constraint() {
            Some(ActivePointerConstraint::Locked) => ConstrainedPointerTarget::Locked,
            Some(ActivePointerConstraint::Confined { surface, region }) => {
                // A confined surface can move beneath a stationary pointer.  Resolve its current
                // transform instead of using the focus-enter snapshot; if it is no longer under
                // the pointer, explicitly end confinement rather than leaving an invalid active
                // constraint that can never move again.
                let current_placement = self
                    .surface_under(self.pointer_location)
                    .filter(|(under, _, _)| *under == surface);
                let Some((_, origin, window)) = current_placement else {
                    self.deactivate_pointer_constraint(&surface);
                    return ConstrainedPointerTarget::Motion(target);
                };
                let scale = window
                    .and_then(|window| self.visual_geometry(window).map(|geometry| geometry.2))
                    .unwrap_or(1.0);
                let current_local = surface_local(self.pointer_location, origin, scale);
                if region
                    .as_ref()
                    .is_some_and(|region| !region.contains(current_local.to_i32_round()))
                {
                    // A committed region can invalidate the current position.  Leaving an active
                    // constraint in that state would freeze it forever, so explicitly unconfine.
                    self.deactivate_pointer_constraint(&surface);
                    return ConstrainedPointerTarget::Motion(target);
                }
                let valid = |point| {
                    self.surface_under(point)
                        .is_some_and(|(under, _, _)| under == surface)
                        && region.as_ref().is_none_or(|region| {
                            region.contains(surface_local(point, origin, scale).to_i32_round())
                        })
                };
                ConstrainedPointerTarget::Motion(clip_motion_segment(
                    self.pointer_location,
                    target,
                    valid,
                ))
            }
            None => ConstrainedPointerTarget::Motion(target),
        }
    }

    pub(super) fn deactivate_pointer_constraint(&mut self, surface: &WlSurface) {
        let pointer = self.pointer.clone();
        with_pointer_constraint(surface, &pointer, |constraint| {
            if let Some(constraint) = constraint.filter(|constraint| constraint.is_active()) {
                constraint.deactivate();
            }
        });
    }

    pub(super) fn maybe_activate_pointer_constraint(&mut self) {
        if self.session_is_locked() {
            return;
        }
        let Some((surface, origin, window)) = self.surface_under(self.pointer_location) else {
            return;
        };
        let pointer = self.pointer.clone();
        if pointer.current_focus().as_ref() != Some(&surface) {
            return;
        }
        let scale = window
            .and_then(|window| self.visual_geometry(window).map(|geometry| geometry.2))
            .unwrap_or(1.0);
        let local = surface_local(self.pointer_location, origin, scale);
        with_pointer_constraint(&surface, &pointer, |constraint| {
            let Some(constraint) = constraint else {
                return;
            };
            if constraint.is_active()
                || constraint
                    .region()
                    .is_some_and(|region| !region.contains(local.to_i32_round()))
            {
                return;
            }
            constraint.activate();
        });
    }
}

impl PointerConstraintsHandler for Astera {
    fn new_constraint(&mut self, _surface: &WlSurface, _pointer: &PointerHandle<Self>) {
        self.maybe_activate_pointer_constraint();
    }

    fn cursor_position_hint(
        &mut self,
        surface: &WlSurface,
        pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
        // Smithay stores this value on the locked-pointer object.  The protocol specifies that a
        // hint takes effect only when the lock is deactivated, so committing one must not warp an
        // active pointer or change constraint enforcement.
        let _ = (surface, pointer);
    }
}

pub(super) enum ConstrainedPointerTarget {
    Locked,
    Motion(Point<f64, Logical>),
}

pub(super) enum ActivePointerConstraint {
    Locked,
    Confined {
        surface: WlSurface,
        region: Option<RegionAttributes>,
    },
}

fn surface_local(
    point: Point<f64, Logical>,
    origin: Point<f64, Logical>,
    scale: f64,
) -> Point<f64, Logical> {
    ((point.x - origin.x) / scale, (point.y - origin.y) / scale).into()
}

/// Find the furthest valid point on a motion segment.  This prevents a large delta from jumping
/// across the surface edge or a subtracted constraint-region rectangle.
fn clip_motion_segment(
    from: Point<f64, Logical>,
    to: Point<f64, Logical>,
    valid: impl Fn(Point<f64, Logical>) -> bool,
) -> Point<f64, Logical> {
    let extent = (to.x - from.x).abs().max((to.y - from.y).abs());
    // Backend coordinates are untrusted. Refuse pathological samples rather than allowing one
    // event to trigger billions of scene hit-tests. Keeping the last valid position is safe for
    // confinement and preserves responsiveness.
    if !extent.is_finite() || extent > 16_384.0 {
        return from;
    }
    if !valid(from) {
        return from;
    }
    // Locate the first invalid interval as well as an invalid endpoint.  Half-logical-pixel
    // sampling cannot skip an integer-coordinate protocol region cell, while ordinary small
    // input deltas require much less work than a fixed high sample count.
    let mut low = 0.0;
    let mut high = None;
    let steps = (extent * 2.0).ceil().max(1.0) as u32;
    for step in 1..=steps {
        let fraction = f64::from(step) / f64::from(steps);
        let point = Point::from((
            from.x + (to.x - from.x) * fraction,
            from.y + (to.y - from.y) * fraction,
        ));
        if valid(point) {
            low = fraction;
        } else {
            high = Some(fraction);
            break;
        }
    }
    let Some(mut high) = high else {
        return to;
    };
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        let point = Point::from((
            from.x + (to.x - from.x) * middle,
            from.y + (to.y - from.y) * middle,
        ));
        if valid(point) {
            low = middle;
        } else {
            high = middle;
        }
    }
    Point::from((
        from.x + (to.x - from.x) * low,
        from.y + (to.y - from.y) * low,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clips_motion_at_first_invalid_boundary() {
        let result = clip_motion_segment((5.0, 5.0).into(), (20.0, 5.0).into(), |point| {
            point.x < 10.0
        });
        assert!((result.x - 10.0).abs() < 0.001);
        assert_eq!(result.y, 5.0);
    }

    #[test]
    fn keeps_start_when_constraint_was_invalidated() {
        let from = Point::from((5.0, 5.0));
        assert_eq!(
            clip_motion_segment(from, (8.0, 5.0).into(), |_| false),
            from
        );
    }

    #[test]
    fn cannot_jump_across_an_invalid_gap() {
        let result = clip_motion_segment((0.0, 0.0).into(), (1000.0, 0.0).into(), |point| {
            !(400.0..401.0).contains(&point.x)
        });
        assert!((result.x - 400.0).abs() < 0.001);
    }

    #[test]
    fn pathological_delta_is_rejected_in_bounded_time() {
        let from = Point::from((1.0, 1.0));
        assert_eq!(
            clip_motion_segment(from, (f64::INFINITY, 1.0).into(), |_| true),
            from
        );
        assert_eq!(
            clip_motion_segment(from, (100_000.0, 1.0).into(), |_| true),
            from
        );
    }
}
