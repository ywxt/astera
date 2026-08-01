//! Shared construction of a render-element snapshot.
//!
//! Frame callbacks are captured beside the exact surface that produced an
//! element. Backends complete this immutable list only after presentation.

use std::collections::{HashMap, HashSet};

use smithay::{
    backend::renderer::{
        ImportAll, Renderer,
        element::{Kind, surface::WaylandSurfaceRenderElement},
        utils::RendererSurfaceStateUserData,
    },
    reexports::wayland_server::{
        Resource,
        protocol::{wl_callback::WlCallback, wl_surface::WlSurface},
    },
    utils::{Physical, Point, Scale},
    wayland::compositor::{
        SurfaceAttributes, SurfaceData, TraversalAction, with_states, with_surface_tree_downward,
    },
};

/// A presentation callback captured in the same surface-tree snapshot as its
/// render element.
#[derive(Clone)]
pub struct FrameCallback {
    surface: WlSurface,
    callback: WlCallback,
}

fn frame_callbacks(surface: &WlSurface, states: &SurfaceData) -> Vec<FrameCallback> {
    // Tree traversal already owns the surface-state lock. Never call
    // with_states() from this function: that recursively locks the same state.
    states
        .cached_state
        .get::<SurfaceAttributes>()
        .current()
        .frame_callbacks
        .iter()
        .cloned()
        .map(|callback| FrameCallback {
            surface: surface.clone(),
            callback,
        })
        .collect()
}

pub fn surface_tree_snapshot<R>(
    renderer: &mut R,
    surface: &WlSurface,
    location: Point<i32, Physical>,
    scale: f64,
    alpha: f32,
    kind: Kind,
    callbacks: &mut Vec<FrameCallback>,
) -> Vec<WaylandSurfaceRenderElement<R>>
where
    R: Renderer + ImportAll,
    R::TextureId: Clone + 'static,
{
    let location = location.to_f64();
    let scale = Scale::from(scale);
    let mut elements = Vec::new();
    with_surface_tree_downward(
        surface,
        location,
        |_, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return TraversalAction::SkipChildren;
            };
            let Some(view) = data.lock().expect("renderer surface state poisoned").view() else {
                return TraversalAction::SkipChildren;
            };
            TraversalAction::DoChildren(*location + view.offset.to_f64().to_physical(scale))
        },
        |surface, states, location| {
            let Some(data) = states.data_map.get::<RendererSurfaceStateUserData>() else {
                return;
            };
            let Some(view) = data.lock().expect("renderer surface state poisoned").view() else {
                return;
            };
            let element_location = *location + view.offset.to_f64().to_physical(scale);
            match WaylandSurfaceRenderElement::from_surface(
                renderer,
                surface,
                states,
                element_location,
                alpha,
                kind,
            ) {
                Ok(Some(element)) => {
                    callbacks.extend(frame_callbacks(surface, states));
                    elements.push(element);
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "could not import surface for frame"),
            }
        },
        |_, _, _| true,
    );
    elements
}

/// Complete only callbacks captured by a successfully submitted frame.
pub fn complete_frame_callbacks(callbacks: &[FrameCallback], time: u32) {
    let mut callbacks_by_surface = HashMap::<WlSurface, HashSet<_>>::new();
    for item in callbacks {
        item.callback.done(time);
        callbacks_by_surface
            .entry(item.surface.clone())
            .or_default()
            .insert(item.callback.id());
    }
    for (surface, callback_ids) in callbacks_by_surface {
        with_states(&surface, |states| {
            states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .retain(|callback| !callback_ids.contains(&callback.id()));
        });
    }
}

#[cfg(test)]
pub(crate) fn frame_callbacks_for_tree(root: &WlSurface) -> Vec<FrameCallback> {
    let mut callbacks = Vec::new();
    with_surface_tree_downward(
        root,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |surface, states, &()| callbacks.extend(frame_callbacks(surface, states)),
        |_, _, &()| true,
    );
    callbacks
}
